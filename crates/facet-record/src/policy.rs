//! The Facet-owned `facet:` key: recording policy read back from the
//! OpenCollection source, before anything is sent.
//!
//! Layouts the loader accepts, all covered here. A bundled collection is
//! one YAML file whose requests sit in a root `items` sequence, with
//! structural selectors like `items/0` or `items/0/items/1`. An unbundled
//! collection is a directory whose `opencollection.y*ml` collection file is
//! either a whole bundled collection (the loader then treats it as bundled)
//! or collection-level YAML above per-request and per-folder files, where
//! the selector is the workspace-relative path of the request file.
//!
//! The key is a mapping. At collection level `facet: { record: never }`
//! forbids recording for every request; at item level
//! `facet: { record: false }` forbids it for that request. Inside `facet:`
//! everything is strict: an unknown key, a wrong type or an unknown value
//! is a [`PolicyError`] that names the key path and never echoes a value.
//! Outside `facet:` nothing is inspected: unknown YAML stays accepted and
//! preserved by the loader, exactly as before.
//!
//! Whatever failed for a source that was given — unreadable, unparseable,
//! no collection file, a vanished request file — is drift that fails
//! closed: [`RecordPolicy::Forbidden`], so the send still happens but
//! nothing is recorded. Only `None` (an in-memory or stdin workspace)
//! has nothing to re-read and stays allowed.
use std::{fmt, fs, path::Path};

use serde_yaml_ng::{Mapping, Value};

/// Errors from reading the `facet:` key. Every message names the key path
/// and never echoes a value.
#[derive(Clone, Debug)]
pub struct PolicyError {
    /// Where the key went wrong: `facet`, `facet.<name>` or `facet.record`.
    path: String,
    /// Static reason; no values.
    reason: &'static str,
}

impl PolicyError {
    fn new(path: impl Into<String>, reason: &'static str) -> Self {
        Self {
            path: path.into(),
            reason,
        }
    }
    /// The key path, e.g. `facet`, `facet.<name>` or `facet.record`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
    /// The error category the CLI reports this under: `facet_key_invalid`.
    #[must_use]
    pub const fn category(&self) -> &'static str {
        "facet_key_invalid"
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid facet key `{}`: {}",
            self.path, self.reason
        )
    }
}

/// Whether recording is allowed for one request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecordPolicy {
    /// No `facet:` rule applies; flags, env and MCP `noRecord` may still
    /// turn recording off.
    #[default]
    Allowed,
    /// `facet: { record: never }` or `{ record: false }` forbade it. No run
    /// row and no blob, whatever the flags, `FACET_NO_RECORD` or MCP
    /// `noRecord` say.
    Forbidden,
}

impl RecordPolicy {
    /// False only for an explicit `facet` forbid: flags can turn recording
    /// off, never on.
    #[must_use]
    pub const fn allows_record(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Reads the recording policy for `selector` from the collection source.
///
/// `source` is the source path of the workspace: the bundled YAML file, or
/// the unbundled collection directory. `None` (an in-memory or stdin
/// workspace) has nothing to re-read, so the policy is
/// [`RecordPolicy::Allowed`]. The `selector` is the repository locator
/// `record` stores: a structural `items/…` path for a bundled collection,
/// the workspace-relative path of the request file otherwise. The request
/// is still sent when the policy forbids recording; only the row and its
/// blobs are withheld.
pub fn record_policy(source: Option<&Path>, selector: &str) -> Result<RecordPolicy, PolicyError> {
    let Some(source) = source else {
        return Ok(RecordPolicy::Allowed);
    };
    if source.is_file() {
        let Some(document) = document_at(source) else {
            return Ok(RecordPolicy::Forbidden);
        };
        return request_policy(&document, selector);
    }
    if source.is_dir() {
        let Some(config) = collection_file(source) else {
            // No collection file: the loader would have refused it too; a
            // `record` rule could be hiding in one.
            return Ok(RecordPolicy::Forbidden);
        };
        let Some(document) = document_at(&config) else {
            return Ok(RecordPolicy::Forbidden);
        };
        if is_bundled(&document) {
            return request_policy(&document, selector);
        }
        let Some(item) = document_at(&source.join(selector)) else {
            // The request file is gone: its rule is unreadable.
            return Ok(RecordPolicy::Forbidden);
        };
        let collection_rule = rule_of(&document)?;
        let item_rule = rule_of(&item)?;
        return combine(collection_rule, item_rule);
    }
    // The source vanished between load and now: nothing to re-read, and a
    // rule may have been there, so forbid.
    Ok(RecordPolicy::Forbidden)
}

/// A whole bundled collection: walk the structural `items/…` selector to
/// the request, then combine that item's rule with the collection's.
fn request_policy(document: &Value, selector: &str) -> Result<RecordPolicy, PolicyError> {
    let collection_rule = rule_of(document)?;
    let item_rule = walk_selector(document, selector).map_or(Ok(None), rule_of)?;
    combine(collection_rule, item_rule)
}

/// Follows a structural selector (`items/…`, index by index) into the
/// document, exactly the way the loader numbers its locators. `None` when
/// the selector does not name a request document the loader produced.
fn walk_selector<'a>(document: &'a Value, selector: &str) -> Option<&'a Value> {
    let mut node = document;
    let mut tokens = selector.split('/');
    while let (Some(key), Some(index)) = (tokens.next(), tokens.next()) {
        let sequence = node.get(key)?.as_sequence()?;
        node = sequence.get(index.parse::<usize>().ok()?)?;
    }
    Some(node)
}

/// One rule from the collection and one from the request: nothing forbids
/// while no rule says otherwise.
fn combine(collection: Option<bool>, item: Option<bool>) -> Result<RecordPolicy, PolicyError> {
    if collection.unwrap_or(true) && item.unwrap_or(true) {
        Ok(RecordPolicy::Allowed)
    } else {
        Ok(RecordPolicy::Forbidden)
    }
}

/// One rule from the `facet:` mapping of `document`: `None` without the
/// key. Only `record: never` and `record: false` forbid; every other shape
/// of the mapping, key, or value is refused.
fn rule_of(document: &Value) -> Result<Option<bool>, PolicyError> {
    let Some(facet) = facet_mapping(document)? else {
        return Ok(None);
    };
    validate_keys(facet)?;
    let Some(value) = facet.get("record") else {
        return Ok(None);
    };
    if let Some(forbids) = known_value(value) {
        Ok(Some(forbids))
    } else {
        Err(PolicyError::new(
            "facet.record",
            "must be `never` or `false`",
        ))
    }
}

/// Whether a `record:` value forbids recording, by its shape alone.
fn known_value(value: &Value) -> Option<bool> {
    match value {
        Value::String(text) if text == "never" => Some(false),
        Value::Bool(false) => Some(false),
        _ => None,
    }
}

/// Reads `facet` from `document`, refusing the wrong shape.
fn facet_mapping(document: &Value) -> Result<Option<&Mapping>, PolicyError> {
    match document.get("facet") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Mapping(mapping)) => Ok(Some(mapping)),
        Some(_) => Err(PolicyError::new("facet", "must be a mapping")),
    }
}

/// Unknown keys inside `facet:` are refused, wherever the mapping sits.
fn validate_keys(facet: &Mapping) -> Result<(), PolicyError> {
    for name in facet.keys() {
        let Some(name) = name.as_str() else {
            return Err(PolicyError::new("facet", "keys must be strings"));
        };
        if name != "record" {
            return Err(PolicyError::new(
                format!("facet.{name}"),
                "unknown facet key",
            ));
        }
    }
    Ok(())
}

/// Whether a directory's collection file holds the whole collection,
/// matching how the loader resolves the same directory.
fn is_bundled(document: &Value) -> bool {
    document.get("bundled").and_then(Value::as_bool) == Some(true)
}

/// `opencollection.y*ml` inside an unbundled collection directory; the
/// loader reserves that stem for the collection file.
fn collection_file(root: &Path) -> Option<std::path::PathBuf> {
    ["yml", "yaml"]
        .into_iter()
        .map(|extension| root.join(format!("opencollection.{extension}")))
        .find(|path| fs::metadata(path).is_ok_and(|meta| meta.is_file()))
}

/// Parses a source file that has to exist: `None` fails closed.
///
/// Only on-disk drift between load and send lands here, and a rule that
/// may have existed must not be lost: a source that cannot be read or
/// parsed counts as forbidden.
fn document_at(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|source| serde_yaml_ng::from_str(&source).ok())
}

#[cfg(test)]
mod tests {
    use super::{PolicyError, RecordPolicy, record_policy};
    use std::{fs, path::Path, path::PathBuf};

    const CANARY: &str = "g3-policy-canary";

    /// Asserts an `expect("no policy error")` result with the right shape.
    fn expect(policy: Result<RecordPolicy, PolicyError>) -> RecordPolicy {
        policy.expect("no policy error")
    }

    fn bundled(source: &str) -> (PathBuf, tempfile::TempDir) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("workspace.yml");
        fs::write(&file, source).unwrap();
        (file, temporary)
    }

    fn unbundled(collection: Option<&str>, items: &[(&str, &str)]) -> (PathBuf, tempfile::TempDir) {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().join("collection");
        fs::create_dir_all(&dir).unwrap();
        if let Some(collection) = collection {
            fs::write(dir.join("opencollection.yml"), collection).unwrap();
        }
        for (name, source) in items {
            fs::write(dir.join(name), source).unwrap();
        }
        (dir, temporary)
    }

    const BUNDLED_HEADER: &str = "opencollection: 1.0.0\nbundled: true\n";
    const BUNDLED_ITEMS: &str = concat!(
        "items:\n",
        "  - info:\n      type: http\n      name: One\n",
        "    facet:\n      record: false\n",
        "  - info:\n      type: http\n      name: Two\n",
    );

    #[test]
    fn nothing_given_means_allowed() {
        let policy = expect(record_policy(None, "items/0"));
        assert_eq!(policy, RecordPolicy::Allowed);
    }

    #[test]
    fn unreadable_unparseable_or_vanished_sources_fail_closed() {
        let a_gone_source = Path::new("/g3-never-created/workspace.yml");
        assert_eq!(
            expect(record_policy(Some(a_gone_source), "items/0")),
            RecordPolicy::Forbidden
        );
        // An unparseable bundled file cannot name a rule, so it forbids.
        let temporary = tempfile::tempdir().unwrap();
        let broken = temporary.path().join("workspace.yml");
        fs::write(
            &broken,
            "opencollection: 1.0.0\nbundled: true\nitems: [unclosed\n",
        )
        .unwrap();
        assert_eq!(
            expect(record_policy(Some(&broken), "items/0")),
            RecordPolicy::Forbidden
        );
    }

    #[test]
    fn collection_rule_covers_every_request() {
        let source = format!("{BUNDLED_HEADER}facet:\n  record: never\n{BUNDLED_ITEMS}");
        let (file, _temp) = bundled(&source);
        assert_eq!(
            expect(record_policy(Some(&file), "items/0")),
            RecordPolicy::Forbidden
        );
        assert_eq!(
            expect(record_policy(Some(&file), "items/1")),
            RecordPolicy::Forbidden
        );
    }

    #[test]
    fn item_rule_covers_its_own_request() {
        let source = format!("{BUNDLED_HEADER}{BUNDLED_ITEMS}");
        let (file, _temp) = bundled(&source);
        assert_eq!(
            expect(record_policy(Some(&file), "items/0")),
            RecordPolicy::Forbidden
        );
        assert_eq!(
            expect(record_policy(Some(&file), "items/1")),
            RecordPolicy::Allowed
        );
    }

    #[test]
    fn nested_folder_selectors_resolve() {
        let source = concat!(
            "opencollection: 1.0.0\n",
            "bundled: true\n",
            "items:\n",
            "  - info:\n      type: folder\n",
            "    items:\n",
            "      - info:\n          type: http\n          name: Deep\n",
            "        facet:\n          record: false\n",
            "      - info:\n          type: http\n          name: Near\n",
        );
        let (file, _temp) = bundled(source);
        assert_eq!(
            expect(record_policy(Some(&file), "items/0/items/0")),
            RecordPolicy::Forbidden
        );
        assert_eq!(
            expect(record_policy(Some(&file), "items/0/items/1")),
            RecordPolicy::Allowed
        );
    }

    #[test]
    fn absent_and_empty_rules_allow() {
        let (file, _temp) = bundled(
            "opencollection: 1.0.0\nbundled: true\nfacet:\nitems:\n  - info:\n      type: http\n      name: One\n    facet: null\n",
        );
        assert_eq!(
            expect(record_policy(Some(&file), "items/0")),
            RecordPolicy::Allowed
        );
    }

    #[test]
    fn everything_outside_facet_stays_lenient() {
        let source = format!(
            "opencollection: 1.0.0\nbundled: true\nx-vendor:\n  anything: {CANARY}\nitems:\n  - info:\n      type: http\n      name: One\n    x-note: {CANARY}\n"
        );
        let (file, _temp) = bundled(&source);
        assert_eq!(
            expect(record_policy(Some(&file), "items/0")),
            RecordPolicy::Allowed
        );
    }

    #[test]
    fn unknown_keys_are_named_without_echoing_a_value() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("workspace.yml");
        fs::write(
            &file,
            "opencollection: 1.0.0\nbundled: true\nfacet:\n  record: never\n  excluded: {CANARY}\n",
        )
        .unwrap();
        let error = record_policy(Some(&file), "items/0").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("facet.excluded"), "{message}");
        assert!(message.contains("unknown facet key"), "{message}");
        assert!(!message.contains(CANARY), "{message}");
    }

    #[test]
    fn wrong_types_and_unknown_values_are_named_without_echoing_a_value() {
        let wrong_type =
            format!("opencollection: 1.0.0\nbundled: true\nfacet:\n  record:\n    - {CANARY}\n");
        let unknown_value =
            String::from("opencollection: 1.0.0\nbundled: true\nfacet:\n  record: hever\n");
        let unknown_true =
            String::from("opencollection: 1.0.0\nbundled: true\nfacet:\n  record: true\n");
        let not_a_mapping =
            String::from("opencollection: 1.0.0\nbundled: true\nfacet: not-mapping\n");
        for (case, source) in [
            ("sequence", wrong_type.as_str()),
            ("string", unknown_value.as_str()),
            ("bool", unknown_true.as_str()),
            ("scalar", not_a_mapping.as_str()),
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let file = temporary.path().join("workspace.yml");
            fs::write(&file, source).unwrap();
            let error = record_policy(Some(&file), "items/0").unwrap_err();
            let message = error.to_string();
            assert!(
                !message.contains(CANARY),
                "{case} echoes a value: {message}"
            );
            if case == "scalar" {
                assert!(message.contains("`facet`"), "{message}");
                assert!(message.contains("must be a mapping"), "{message}");
            } else {
                assert!(message.contains("facet.record"), "{message}");
                assert!(message.contains("must be `never` or `false`"), "{message}");
            }
        }
    }

    #[test]
    fn unbundled_layout_and_its_fail_closed_cases() {
        let collection = "opencollection: 1.0.0\nbundled: false\nfacet:\n  record: never\n";
        let one = "info:\n  type: http\n  name: One\nfacet:\n  record: false\n";
        let (dir, _temp) = unbundled(
            Some(collection),
            &[("one.yml", one), ("two.yml", "info:\n  type: http\n")],
        );
        assert_eq!(
            expect(record_policy(Some(&dir), "one.yml")),
            RecordPolicy::Forbidden
        );
        assert_eq!(
            expect(record_policy(Some(&dir), "two.yml")),
            RecordPolicy::Forbidden
        );
        // A vanished request file cannot name a rule, so it forbids.
        assert_eq!(
            expect(record_policy(Some(&dir), "gone.yml")),
            RecordPolicy::Forbidden
        );

        // A directory the loader would refuse too: no collection file.
        let (bare, _temp) = unbundled(None, &[("one.yml", one)]);
        assert_eq!(
            expect(record_policy(Some(&bare), "one.yml")),
            RecordPolicy::Forbidden
        );
    }

    #[test]
    fn a_bundled_collection_inside_a_directory_resolves_selectors() {
        let whole = "opencollection: 1.0.0\nbundled: true\nitems:\n  - info:\n      type: http\n      name: One\n    facet:\n      record: false\n";
        let (dir, _temp) = unbundled(Some(whole), &[]);
        assert_eq!(
            expect(record_policy(Some(&dir), "items/0")),
            RecordPolicy::Forbidden
        );
    }
}
