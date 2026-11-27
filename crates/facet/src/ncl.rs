//! `facet ncl check|export`: the in-process Nickel embed (SPEC-nickel G1a).
//!
//! Facet is the only process on the platform that evaluates Nickel. It does so
//! by linking `hedron-ncl` (which pins `nickel-lang-core`); it never shells out
//! to a `nickel` binary. This module owns the two operations; `args.rs` and
//! `mcp.rs` wrap them for the CLI and MCP surfaces.
//!
//! - [`check`]: parse + typecheck + full evaluation so every contract fires.
//!   Nothing is returned beyond the module hash; nothing is persisted.
//! - [`export`]: `eval_full_for_export` (fields marked `| not_exported` are
//!   dropped) projected to the `World` shape `{ cluster, intent, calls }`, plus
//!   `moduleHash` (sha256 of the source bytes), `exportHash` (sha256 of the
//!   canonical frozen JSON) and `contractSet` (the release overlay id).
//! - [`apply`]: export without a separate ledger row, POST frozen `cluster`
//!   JSON, optional Hedron `Store::put` for `intent`, then one `ncl:apply` row.
//!
//! `--var path.to.field=value` maps to a Nickel `FieldOverride` at the
//! operator merge priority. Values are Nickel expressions; strings need quotes.
//! Secrets never pass through here: OpenCollection plaintext fields are
//! `| not_exported`, and hydration happens in Facet at run time, after export.

use std::path::{Path, PathBuf};

use hedron_ncl::eval::{self, Input};
use lattice::sha256_hex;
use serde_json::{Value, json};

use crate::ncl_apply::{self, NclApply};
use crate::{CONFIGURATION_EXIT_CODE, FacetError};

/// Contract set id of the release overlay compiled into this binary.
pub const CONTRACT_SET: &str = hedron_ncl::CONTRACT_SET;

/// Error category for a Nickel parse, type, import or contract failure.
pub const NCL_INVALID: &str = "ncl_invalid";

/// Result of a successful `check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NclCheck {
    /// The module that was checked.
    pub path: PathBuf,
    /// sha256 of the module's source bytes.
    pub module_hash: String,
    /// Release overlay id the check ran against.
    pub contract_set: &'static str,
}

/// Result of a successful `export`: the frozen `World` projections and hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NclExport {
    /// The module that was exported.
    pub path: PathBuf,
    /// sha256 of the module's source bytes.
    pub module_hash: String,
    /// sha256 of the canonical (sorted-key, compact) JSON of the frozen value.
    pub export_hash: String,
    /// Release overlay id the export ran against.
    pub contract_set: &'static str,
    /// `world.cluster` — frozen Kubernetes v1.34 objects for h3s (`null` if absent).
    pub cluster: Value,
    /// `world.intent` — frozen desired states for HedronDB (`null` if absent).
    pub intent: Value,
    /// `world.calls` — OpenCollection projection for Facet (`null` if absent).
    pub calls: Value,
}

impl NclCheck {
    /// JSON body (without `schemaVersion`; the caller versions it).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "moduleHash": self.module_hash,
            "contractSet": self.contract_set,
        })
    }
}

impl NclExport {
    /// JSON body (without `schemaVersion`; the caller versions it).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "moduleHash": self.module_hash,
            "exportHash": self.export_hash,
            "contractSet": self.contract_set,
            "cluster": self.cluster,
            "intent": self.intent,
            "calls": self.calls,
        })
    }
}

/// Parse, typecheck and evaluate the module at `path` with `vars` applied.
/// Persists nothing.
pub fn check(
    path: &Path,
    vars: &[String],
    import_paths: &[PathBuf],
) -> Result<NclCheck, FacetError> {
    let module_hash = module_hash(path)?;
    eval::check(Input::Path(path), vars, import_paths).map_err(ncl_error)?;
    Ok(NclCheck {
        path: path.to_path_buf(),
        module_hash,
        contract_set: CONTRACT_SET,
    })
}

/// Evaluate the module at `path` for export with `vars` applied and split the
/// frozen value into the three `World` projections.
pub fn export(
    path: &Path,
    vars: &[String],
    import_paths: &[PathBuf],
) -> Result<NclExport, FacetError> {
    let export = evaluate(path, vars, import_paths)?;
    crate::ncl_ledger::record_export(path, &export, vars);
    Ok(export)
}

/// Export, act on frozen projections, and record one `ncl:apply` Lattice row.
pub fn apply(
    path: &Path,
    vars: &[String],
    import_paths: &[PathBuf],
) -> Result<NclApply, FacetError> {
    let export = evaluate(path, vars, import_paths)?;
    let action = ncl_apply::execute(&export)?;
    crate::ncl_ledger::record_apply(path, &export, &action.to_json(), vars);
    Ok(ncl_apply::from_export(export, action))
}

/// Evaluate for export without persisting to Lattice.
pub(crate) fn evaluate(
    path: &Path,
    vars: &[String],
    import_paths: &[PathBuf],
) -> Result<NclExport, FacetError> {
    let module_hash = module_hash(path)?;
    let world = eval::export(Input::Path(path), vars, import_paths).map_err(ncl_error)?;
    let export_hash = sha256_hex(canonical(&world).as_bytes());
    let mut world = match world {
        Value::Object(map) => map,
        other => {
            return Err(FacetError {
                category: NCL_INVALID,
                message: format!(
                    "{}: export must be a record (World = {{ cluster, intent, calls }}), got {}",
                    path.display(),
                    kind(&other)
                ),
                exit_code: CONFIGURATION_EXIT_CODE,
                details: None,
            });
        }
    };
    let export = NclExport {
        path: path.to_path_buf(),
        module_hash,
        export_hash,
        contract_set: CONTRACT_SET,
        cluster: world.remove("cluster").unwrap_or(Value::Null),
        intent: world.remove("intent").unwrap_or(Value::Null),
        calls: world.remove("calls").unwrap_or(Value::Null),
    };
    Ok(export)
}

fn module_hash(path: &Path) -> Result<String, FacetError> {
    let bytes = std::fs::read(path).map_err(|error| {
        FacetError::invalid_workspace(format!("cannot read {}: {error}", path.display()))
    })?;
    Ok(sha256_hex(&bytes))
}

/// Compact JSON with sorted keys. `serde_json` maps are ordered without
/// `preserve_order`, so `to_string` is already canonical.
fn canonical(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON value serialization cannot fail")
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "a record",
    }
}

fn ncl_error(error: eval::Error) -> FacetError {
    match error {
        eval::Error::Override(message) => {
            FacetError::invalid_arguments(format!("--var: {message}"))
        }
        eval::Error::Build(message) => FacetError::invalid_workspace(message),
        eval::Error::Nickel(report) => FacetError {
            category: NCL_INVALID,
            message: report.trim_end().to_owned(),
            exit_code: CONFIGURATION_EXIT_CODE,
            details: None,
        },
        other @ (eval::Error::Export(_) | eval::Error::Json(_)) => FacetError {
            category: NCL_INVALID,
            message: other.to_string(),
            exit_code: CONFIGURATION_EXIT_CODE,
            details: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::INVALID_ARGUMENTS_EXIT_CODE;

    const WORLD: &str = r#"
let lib = import "lib.ncl" in
{
  replicas = 1,
  cluster = [ lib.pod { name = "web", n = replicas } ],
  intent = [ { name = "test-docs-eod", importance = 0.5, spec = { kind = "docs_eod", date = "2026-09-12", required_briefs = [] } }, ],
  calls = {
    opencollection = "1.0.0",
    environments = [ { name = "dev", variables = [ { name = "TOKEN", secret_ref = "kr:me" } ] } ],
    plaintext_token | not_exported = "hunter2",
  },
}
"#;

    const LIB: &str = r#"
{
  pod = fun args => {
    apiVersion = "v1",
    kind = "Pod",
    metadata.name = args.name,
    metadata.annotations.replicas = std.to_string args.n,
  },
}
"#;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.ncl"), LIB).unwrap();
        let world = dir.path().join("world.ncl");
        std::fs::write(&world, WORLD).unwrap();
        (dir, world)
    }

    #[test]
    fn export_projects_world_and_drops_not_exported() {
        let (_dir, world) = fixture();
        let out = export(&world, &[], &[]).unwrap();
        assert_eq!(out.contract_set, "k8s-1.34-h3s-0.9.1");
        assert_eq!(out.cluster[0]["kind"], "Pod");
        assert_eq!(out.cluster[0]["metadata"]["annotations"]["replicas"], "1");
        assert_eq!(out.intent[0]["spec"]["kind"], "docs_eod");
        assert_eq!(
            out.calls["environments"][0]["variables"][0]["secret_ref"],
            "kr:me"
        );
        assert!(
            out.calls.get("plaintext_token").is_none(),
            "not_exported must not leak"
        );
        assert!(!canonical(&out.to_json()).contains("hunter2"));
        assert_eq!(out.module_hash, sha256_hex(WORLD.as_bytes()));
        assert_eq!(out.export_hash.len(), 64);
        let json = out.to_json();
        for key in [
            "path",
            "moduleHash",
            "exportHash",
            "contractSet",
            "cluster",
            "intent",
            "calls",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn export_hash_is_stable_and_var_sensitive() {
        let (_dir, world) = fixture();
        let a = export(&world, &[], &[]).unwrap();
        let b = export(&world, &[], &[]).unwrap();
        assert_eq!(a.export_hash, b.export_hash);
        let c = export(&world, &["replicas=3".to_owned()], &[]).unwrap();
        assert_eq!(c.cluster[0]["metadata"]["annotations"]["replicas"], "3");
        assert_ne!(a.export_hash, c.export_hash);
        assert_eq!(
            a.module_hash, c.module_hash,
            "--var changes the export, not the module"
        );
    }

    #[test]
    fn check_passes_and_persists_nothing() {
        let (dir, world) = fixture();
        let out = check(&world, &[], &[]).unwrap();
        assert_eq!(out.module_hash, sha256_hex(WORLD.as_bytes()));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "check wrote files: {names:?}");
    }

    #[test]
    fn contract_blame_is_ncl_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.ncl");
        std::fs::write(&bad, r#"{ replicas | Number = "three" }"#).unwrap();
        let err = check(&bad, &[], &[]).unwrap_err();
        assert_eq!(err.category, NCL_INVALID);
        assert_eq!(err.exit_code, CONFIGURATION_EXIT_CODE);
        assert!(err.message.contains("contract broken"), "{}", err.message);
        let err = export(&bad, &[], &[]).unwrap_err();
        assert_eq!(err.category, NCL_INVALID);
    }

    #[test]
    fn non_record_export_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let scalar = dir.path().join("scalar.ncl");
        std::fs::write(&scalar, "[1, 2, 3]").unwrap();
        let err = export(&scalar, &[], &[]).unwrap_err();
        assert_eq!(err.category, NCL_INVALID);
        assert!(err.message.contains("an array"), "{}", err.message);
    }

    #[test]
    fn bad_var_and_missing_file() {
        let (_dir, world) = fixture();
        let err = export(&world, &["replicas".to_owned()], &[]).unwrap_err();
        assert_eq!(err.category, "invalid_arguments");
        assert_eq!(err.exit_code, INVALID_ARGUMENTS_EXIT_CODE);
        let err = check(Path::new("/nonexistent/world.ncl"), &[], &[]).unwrap_err();
        assert_eq!(err.category, "invalid_workspace");
    }

    /// The embed is in-process: no `nickel` binary is ever spawned.
    #[test]
    fn no_process_spawn_in_this_module() {
        let src = include_str!("ncl.rs");
        assert!(!src.contains(concat!("std::", "process")));
        assert!(!src.contains(concat!("Command::", "new")));
    }
}
