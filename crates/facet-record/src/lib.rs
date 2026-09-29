//! Shared run-recording path for the Facet adapters (`facet` CLI and
//! `facet-tui`). Builds the Lattice row from a resolved request and its HTTP
//! outcome, redacts secrets, places bodies by threshold, and indexes the run
//! in the machine store. It lives here so that no frontend owns the logic
//! (Probe's invariant: business logic never in an adapter).

#![forbid(unsafe_code)]

mod diff;
mod github;
mod hydrate;
mod transport;

use std::path::Path;

pub use diff::{
    BodyDiff, BodySide, FieldChange, PROVENANCE_FIELDS, RunDiff, compare, diff_request_bodies,
    diff_response_bodies, unified_diff,
};
pub use github::{api_fallback, auth_token_for};
pub use hydrate::{HydrateError, Hydration, overlay_secrets};
pub use transport::{http_engine_from_env, http_engine_from_kubeconfig, kube_api_base_from_env};

use lattice::{
    BodyInput, LatticeConfig, LatticeError, MachineStore, NewRun, Retention, RunRow,
    WorkspaceStore, now_ms, sha256_hex,
};
use probe_core::{Body, FormField, Header, HttpRequest, RequestBody};
use probe_http::{HttpError, HttpResponse, ResponseHeader};
use serde_json::{Value, json};

/// Header names whose values are never written to Lattice.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
    "api-key",
    "x-amz-security-token",
];
const REDACTED: &str = "<redacted>";

/// Flag-level overrides applied over the file-based Lattice configuration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigOverrides {
    /// `--inline-body-max`.
    pub inline_body_max: Option<u64>,
    /// `--history-retention`.
    pub history_retention: Option<Retention>,
}

impl ConfigOverrides {
    /// Applies the overrides to a loaded configuration.
    pub fn apply(&self, config: &mut LatticeConfig) {
        if let Some(value) = self.inline_body_max {
            config.inline_body_max = value;
        }
        if let Some(value) = self.history_retention {
            config.history_retention = value;
        }
    }
}

/// Loads configuration for `root` (machine file, then workspace file, then
/// overrides) and opens or creates its workspace store.
pub fn open_store(
    root: &Path,
    overrides: &ConfigOverrides,
) -> Result<WorkspaceStore, LatticeError> {
    let mut config = LatticeConfig::load(root)?;
    overrides.apply(&mut config);
    WorkspaceStore::open(root, config)
}

/// `FACET_NO_RECORD` is set to something other than empty or `0`.
#[must_use]
pub fn recording_disabled() -> bool {
    std::env::var_os("FACET_NO_RECORD").is_some_and(|value| !value.is_empty() && value != "0")
}

/// `FACET_ACTOR`, or `human`.
#[must_use]
pub fn actor_from_env() -> String {
    std::env::var("FACET_ACTOR")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "human".to_owned())
}

/// `FACET_SESSION`, when set and non-empty.
#[must_use]
pub fn session_from_env() -> Option<String> {
    std::env::var("FACET_SESSION")
        .ok()
        .filter(|value| !value.is_empty())
}

/// What happened to the Lattice write for this run.
// Keep the existing owned RunRow API; boxing this variant would break callers.
#[allow(clippy::large_enum_variant)]
pub enum Recording {
    /// The run row landed in the workspace store.
    Recorded {
        /// The recorded row.
        run: RunRow,
        /// Workspace ULID the row belongs to.
        workspace_id: String,
        /// Whether the machine-store pointer row was written.
        indexed: Result<(), String>,
        /// Whether the run's session was minted in the machine store by the
        /// mint-if-missing path. `false` when no session was set, when the
        /// session already existed, or when the best-effort write failed.
        /// Not part of the `request run` envelope (it stays still; the
        /// session is visible through `facet session show`).
        session_created: bool,
    },
    /// Recording was not attempted (`disabled`, `stdin_workspace`).
    Skipped(&'static str),
    /// Recording was attempted and failed; the run itself still happened.
    Failed(String),
}

impl Recording {
    /// Machine-readable summary (the `lattice` field of `request run --json`).
    #[must_use]
    pub fn json(&self) -> Value {
        match self {
            Self::Recorded {
                run,
                workspace_id,
                indexed,
                ..
            } => json!({
                "recorded": true,
                "runId": run.id,
                "workspaceId": workspace_id,
                "indexed": indexed.is_ok(),
                "requestHash": run.request_hash,
                "responseBody": {
                    "sizeBytes": run.res_body.len,
                    "hash": run.res_body.hash,
                    "retention": run.res_body.retention(),
                },
            }),
            Self::Skipped(reason) => json!({ "recorded": false, "reason": reason }),
            Self::Failed(message) => json!({
                "recorded": false,
                "reason": "error",
                "message": message,
            }),
        }
    }

    /// One-line human summary.
    #[must_use]
    pub fn human(&self) -> String {
        match self {
            Self::Recorded {
                run, workspace_id, ..
            } => format!("run {} recorded (workspace {workspace_id})", run.id),
            Self::Skipped(reason) => format!("not recorded ({reason})"),
            Self::Failed(message) => format!("not recorded ({message})"),
        }
    }

    /// Stderr-worthy warning, if any.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Recorded {
                indexed: Err(message),
                ..
            } => Some(format!(
                "run recorded but not indexed in the machine store: {message}"
            )),
            Self::Failed(message) => Some(format!("run not recorded in Lattice: {message}")),
            _ => None,
        }
    }
}

/// Everything needed to record one executed request.
#[derive(Clone, Copy, Debug)]
pub struct RecordRequest<'a> {
    /// Workspace root (the directory beside the collection). `None` for a
    /// stdin workspace, which is never recorded.
    pub root: Option<&'a Path>,
    /// CLI-level configuration overrides.
    pub overrides: &'a ConfigOverrides,
    /// Repository selector of the request.
    pub selector: &'a str,
    /// Environment name used to resolve the request.
    pub environment: Option<&'a str>,
    /// The resolved request as executed.
    pub request: &'a HttpRequest,
    /// Unix milliseconds when execution started.
    pub started_at: i64,
    /// Wall-clock duration of the execution.
    pub elapsed_ms: i64,
    /// The HTTP outcome.
    pub result: &'a Result<HttpResponse, HttpError>,
    /// File the response body was streamed to, if any.
    pub output: Option<&'a Path>,
    /// Tags to store with the run.
    pub tags: &'a [String],
    /// `human` or an agent name.
    pub actor: &'a str,
    /// Machine-store session id.
    pub session: Option<&'a str>,
    /// `runs.id` this run replays (`facet replay`), if any.
    pub replayed_from: Option<&'a str>,
    /// Names of the `--var` overrides used at resolve time. Names only;
    /// values are never written anywhere in Lattice.
    pub var_names: &'a [String],
    /// Secret values to scrub from stored text (URL, header values, error
    /// text) before the row is written. See [`Hydration::redact`].
    pub redact: &'a [String],
}

/// Records one run in the workspace store beside the collection, then
/// indexes it in the machine store (best effort). Never panics on store
/// failure; the caller decides how to surface [`Recording::Failed`].
#[must_use]
pub fn record(req: &RecordRequest<'_>) -> Recording {
    let RecordRequest {
        root,
        overrides,
        selector,
        environment,
        request,
        started_at,
        elapsed_ms,
        result,
        output,
        tags,
        actor,
        session,
        replayed_from,
        var_names,
        redact,
    } = *req;
    let Some(root) = root else {
        return Recording::Skipped("stdin_workspace");
    };
    let store = match open_store(root, overrides) {
        Ok(store) => store,
        Err(error) => return Recording::Failed(error.to_string()),
    };

    let req_body_bytes = request_body_bytes(request);
    let request_hash = request_hash(request, req_body_bytes.as_deref());
    let req_headers = scrub(&request_headers_json(&request.headers).to_string(), redact);
    let error_text = result
        .as_ref()
        .err()
        .map(|error| scrub(&error.to_string(), redact));
    let res_headers = result.as_ref().ok().map(|response| {
        scrub(
            &response_headers_json(&response.headers).to_string(),
            redact,
        )
    });
    let content_type = result.as_ref().ok().and_then(|response| {
        response
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-type"))
            .map(|header| header.value.clone())
    });
    // Auto-tags ride beside the caller's tags: `git:<sha>[-dirty]` for the
    // workspace's repository, best effort, never a column.
    let mut all_tags: Vec<String> = tags.to_vec();
    if let Some(tag) = git_tag(root)
        && !all_tags.contains(&tag)
    {
        all_tags.push(tag);
    }
    let tags_json = if all_tags.is_empty() {
        None
    } else {
        Some(json!(all_tags).to_string())
    };
    let redacted_url = scrub(&redact_url(request.url.as_deref().unwrap_or("")), redact);
    // Always written post-0003 so `[]` (none) stays distinct from NULL (unknown).
    let var_names_json = json!(var_names).to_string();

    let res_body = match result {
        Ok(response) => {
            if let Some(output) = output {
                BodyInput::File(output)
            } else if response.body_complete {
                BodyInput::Bytes(&response.body)
            } else if let Some(file) = &response.body_file {
                BodyInput::File(file.path())
            } else {
                BodyInput::Unretained {
                    len: response.size as u64,
                }
            }
        }
        Err(_) => BodyInput::None,
    };
    let req_body = req_body_bytes
        .as_deref()
        .map_or(BodyInput::None, BodyInput::Bytes);

    let new_run = NewRun {
        started_at,
        duration_ms: Some(elapsed_ms),
        request_path: selector,
        request_hash: &request_hash,
        environment,
        method: request.method.as_deref().unwrap_or(""),
        url: &redacted_url,
        status: result
            .as_ref()
            .ok()
            .map(|response| i64::from(response.status)),
        error: error_text.as_deref(),
        req_headers: Some(&req_headers),
        res_headers: res_headers.as_deref(),
        req_body,
        res_body,
        res_content_type: content_type.as_deref(),
        session_id: session,
        actor,
        tags: tags_json.as_deref(),
        replayed_from,
        var_names: Some(&var_names_json),
    };

    match store.record_run(&new_run) {
        Ok(run) => {
            let (indexed, session_created) = index_run(&store, &run);
            Recording::Recorded {
                run,
                workspace_id: store.workspace_id().to_owned(),
                indexed,
                session_created,
            }
        }
        Err(error) => Recording::Failed(error.to_string()),
    }
}

/// Best-effort pointer row in the machine store, plus the mint-if-missing
/// session write. The workspace store is the record of truth; both the
/// index and the session mint are conveniences and never fail the run.
/// Returns `(indexed, session_created)` where `session_created` is `false`
/// when no session was set, when the session already existed, or when the
/// best-effort write failed.
fn index_run(store: &WorkspaceStore, run: &RunRow) -> (Result<(), String>, bool) {
    let machine = match MachineStore::open(store.config()) {
        Ok(machine) => machine,
        Err(error) => return (Err(error.to_string()), false),
    };
    if let Err(error) = machine.touch_workspace(store.workspace_id(), store.root(), None, now_ms())
    {
        return (Err(error.to_string()), false);
    }
    // Mint-if-missing: when the run carries a session id, ensure a parent
    // session row exists. Best effort, never fails the run.
    let session_created = match run.session_id.as_deref() {
        Some(session_id) => machine
            .ensure_session(session_id, &run.actor, now_ms())
            .unwrap_or(false),
        None => false,
    };
    let indexed = machine
        .index_run(run, store.workspace_id())
        .map_err(|error| error.to_string());
    (indexed, session_created)
}

fn request_body_bytes(request: &HttpRequest) -> Option<Vec<u8>> {
    match request.body.as_ref()? {
        RequestBody::Single(body) => single_body_bytes(body),
        RequestBody::Variants(variants) => variants
            .iter()
            .find(|variant| variant.selected)
            .and_then(|variant| single_body_bytes(&variant.body)),
    }
}

fn single_body_bytes(body: &Body) -> Option<Vec<u8>> {
    match body {
        Body::Raw(raw) => Some(raw.data.clone().into_bytes()),
        Body::FormUrlEncoded(fields) => Some(form_encode(fields).into_bytes()),
        Body::Multipart(_) | Body::File(_) => None,
    }
}

fn form_encode(fields: &[FormField]) -> String {
    fields
        .iter()
        .filter(|field| !field.disabled)
        .map(|field| format!("{}={}", url_encode(&field.name), url_encode(&field.value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// What `--dry-run` shows: the resolved request as it would be sent, with
/// the same redactions Lattice applies (sensitive header names, URL
/// userinfo, known secret values) so a preview is safe to paste. Bodies are
/// shown inline when UTF-8 and under `MAX_PREVIEW_BODY`, else by size only.
#[must_use]
pub fn request_preview_json(request: &HttpRequest, redact: &[String]) -> Value {
    const MAX_PREVIEW_BODY: usize = 64 * 1024;
    let body = request_body_bytes(request);
    let body_json = match body.as_deref() {
        None => {
            json!({ "content": Value::Null, "sizeBytes": Value::Null, "omitted": false, "omissionReason": Value::Null })
        }
        Some(bytes) if bytes.len() > MAX_PREVIEW_BODY => json!({
            "content": Value::Null, "sizeBytes": bytes.len(), "omitted": true, "omissionReason": "too_large",
        }),
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => json!({
                "content": scrub(text, redact), "sizeBytes": bytes.len(), "omitted": false, "omissionReason": Value::Null,
            }),
            Err(_) => json!({
                "content": Value::Null, "sizeBytes": bytes.len(), "omitted": true, "omissionReason": "binary",
            }),
        },
    };
    let headers: Value = serde_json::from_str(&scrub(
        &request_headers_json(&request.headers).to_string(),
        redact,
    ))
    .unwrap_or(Value::Null);
    let query: Vec<Value> = request
        .query_parameters
        .iter()
        .map(|parameter| {
            json!({
                "disabled": parameter.disabled,
                "name": parameter.name,
                "value": scrub(&parameter.value, redact),
            })
        })
        .collect();
    json!({
        "method": request.method,
        "url": scrub(&redact_url(request.url.as_deref().unwrap_or("")), redact),
        "query": query,
        "headers": headers,
        "body": body_json,
        "requestHash": request_hash(request, body.as_deref()),
    })
}

/// The `request_hash` Lattice would store for `request` as resolved now.
/// `facet replay` compares this against the recorded row before sending.
#[must_use]
pub fn request_hash_of(request: &HttpRequest) -> String {
    let body = request_body_bytes(request);
    request_hash(request, body.as_deref())
}

/// SHA-256 of a canonical JSON view of the resolved request. Bodies enter by
/// hash so the digest is small and stable; authentication enters by scheme
/// only so secrets never influence a value stored beside the run.
fn request_hash(request: &HttpRequest, body: Option<&[u8]>) -> String {
    let canonical = json!({
        "auth": request.authentication.as_ref().map(|auth| auth.kind.as_str()),
        "body": body.map(sha256_hex),
        "headers": request.headers.iter().map(|header| json!([header.name, header.value, header.disabled])).collect::<Vec<_>>(),
        "method": request.method,
        "path": request.path_parameters.iter().map(|parameter| json!([parameter.name, parameter.value, parameter.disabled])).collect::<Vec<_>>(),
        "query": request.query_parameters.iter().map(|parameter| json!([parameter.name, parameter.value, parameter.disabled])).collect::<Vec<_>>(),
        "url": request.url,
    });
    sha256_hex(canonical.to_string().as_bytes())
}

fn is_sensitive(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

fn request_headers_json(headers: &[Header]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|header| {
                json!({
                    "disabled": header.disabled,
                    "name": header.name,
                    "value": if is_sensitive(&header.name) { REDACTED } else { header.value.as_str() },
                })
            })
            .collect(),
    )
}

fn response_headers_json(headers: &[ResponseHeader]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|header| {
                json!({
                    "name": header.name,
                    "value": if is_sensitive(&header.name) { REDACTED } else { header.value.as_str() },
                })
            })
            .collect(),
    )
}

/// Prefixes of tags Facet writes itself (`git:<sha>[-dirty]`, `expect:fail`).
/// They describe one run and are never copied onto a replay.
pub const AUTO_TAG_PREFIXES: &[&str] = &["git:", "expect:"];

/// Whether `tag` is one Facet wrote itself.
#[must_use]
pub fn is_auto_tag(tag: &str) -> bool {
    AUTO_TAG_PREFIXES
        .iter()
        .any(|prefix| tag.starts_with(prefix))
}

/// `git:<sha>` or `git:<sha>-dirty` for the repository containing `dir`
/// (the workspace root, beside the collection), or `None` outside a
/// repository, without `git`, or on any failure. Local only: `rev-parse
/// HEAD` plus `status --porcelain --untracked-files=no`, so "dirty" means
/// tracked changes (staged or unstaged); untracked files do not count, or a
/// fresh `.facet/workspace.toml` would flag every first run.
#[must_use]
pub fn git_tag(dir: &Path) -> Option<String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let sha = git(&["rev-parse", "HEAD"])?;
    let sha = sha.trim();
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let dirty = !git(&["status", "--porcelain", "--untracked-files=no"])?
        .trim()
        .is_empty();
    Some(if dirty {
        format!("git:{sha}-dirty")
    } else {
        format!("git:{sha}")
    })
}

/// Shortest secret value that is scrubbed from stored text. Shorter values
/// would mangle unrelated text (and are not secrets in any useful sense).
pub const MIN_REDACT_LEN: usize = 4;

/// Replaces every occurrence of each known secret value with `<redacted>`.
/// Applied to stored URL, header JSON, and error text so a secret the YAML
/// interpolates into a query string or a custom header never lands in
/// Lattice. Bodies are content-addressed blobs and are not rewritten.
#[must_use]
pub fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        if secret.len() >= MIN_REDACT_LEN && out.contains(secret.as_str()) {
            out = out.replace(secret.as_str(), REDACTED);
        }
    }
    out
}

/// Strips `user:password@` from a URL's authority.
fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];
    match authority.rfind('@') {
        Some(at) => format!(
            "{}{REDACTED}@{}{}",
            &url[..authority_start],
            &authority[at + 1..],
            &url[authority_end..]
        ),
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use probe_core::{Header, HttpRequest};

    use super::{form_encode, redact_url, request_hash, request_headers_json, scrub, url_encode};

    fn request(url: &str) -> HttpRequest {
        HttpRequest {
            method: Some("GET".to_owned()),
            url: Some(url.to_owned()),
            headers: vec![
                Header {
                    name: "Authorization".to_owned(),
                    value: "Bearer secret".to_owned(),
                    disabled: false,
                },
                Header {
                    name: "Accept".to_owned(),
                    value: "application/json".to_owned(),
                    disabled: false,
                },
            ],
            ..HttpRequest::default()
        }
    }

    #[test]
    fn redacts_sensitive_headers_case_insensitively() {
        let headers = request_headers_json(&request("http://x/").headers);
        assert_eq!(headers[0]["value"], "<redacted>");
        assert_eq!(headers[1]["value"], "application/json");
    }

    #[test]
    fn redacts_url_userinfo_only() {
        assert_eq!(
            redact_url("https://user:pw@api.example.com/v1?x=1"),
            "https://<redacted>@api.example.com/v1?x=1"
        );
        assert_eq!(
            redact_url("https://api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(redact_url("not a url"), "not a url");
    }

    #[test]
    fn request_hash_is_deterministic_and_url_sensitive() {
        let first = request_hash(&request("http://a/"), Some(b"body"));
        let same = request_hash(&request("http://a/"), Some(b"body"));
        let other = request_hash(&request("http://b/"), Some(b"body"));
        assert_eq!(first, same);
        assert_ne!(first, other);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn auto_tags_are_recognized_and_git_tag_is_none_outside_a_repo() {
        assert!(super::is_auto_tag("git:0123abcd"));
        assert!(super::is_auto_tag("expect:fail"));
        assert!(!super::is_auto_tag("smoke"));
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(super::git_tag(dir.path()), None);
    }

    #[test]
    fn scrubs_known_secret_values_but_not_tiny_ones() {
        let secrets = vec!["hunter2".to_owned(), "ab".to_owned()];
        assert_eq!(
            scrub("http://x/?t=hunter2&u=ab", &secrets),
            "http://x/?t=<redacted>&u=ab"
        );
        assert_eq!(scrub("clean", &secrets), "clean");
    }

    #[test]
    fn encodes_forms() {
        assert_eq!(url_encode("a b&c"), "a+b%26c");
        let fields = vec![
            probe_core::FormField {
                name: "q".to_owned(),
                value: "x y".to_owned(),
                disabled: false,
            },
            probe_core::FormField {
                name: "skip".to_owned(),
                value: "me".to_owned(),
                disabled: true,
            },
        ];
        assert_eq!(form_encode(&fields), "q=x+y");
    }
}
