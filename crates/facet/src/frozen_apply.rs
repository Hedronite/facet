//! Execute a `schemaVersion: 1` frozen apply bundle without Nickel.
//!
//! This module must not import the Nickel VM crate, `ncl_shim`, `frozen_ledger`,
//! `args`, `run`, or `workspace`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use facet_record::{api_fallback, http_engine_from_env, kube_api_base_from_env};
use hedron_core::Store;
use probe_core::{
    Body, Header, HttpRequest, ItemMetadata, RawBody, RawBodyKind, RequestBody, RequestSettings,
    TransportLock,
};
use probe_http::{ExecutionOptions, HttpError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::docs_eod::check_shape;
use crate::{CommandOutput, FacetError};

const REQUIRED: &[&str] = &[
    "schemaVersion",
    "path",
    "moduleHash",
    "exportHash",
    "contractSet",
    "varNames",
    "sourceSnapshot",
    "cluster",
    "intent",
    "calls",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyAction {
    pub cluster: ClusterAction,
    pub intent: IntentAction,
    pub calls: CallsAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAction {
    pub skipped: bool,
    pub posted: usize,
    pub results: Vec<ClusterPostResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterPostResult {
    pub kind: String,
    pub name: String,
    pub status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAction {
    pub skipped: bool,
    pub put: usize,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallsAction {
    pub skipped: bool,
    pub executed: usize,
}

impl ApplyAction {
    pub fn to_json(&self) -> Value {
        json!({
            "cluster": self.cluster.to_json(),
            "intent": self.intent.to_json(),
            "calls": self.calls.to_json(),
        })
    }
}

impl ClusterAction {
    fn to_json(&self) -> Value {
        json!({
            "skipped": self.skipped,
            "posted": self.posted,
            "results": self.results.iter().map(|row| json!({
                "kind": row.kind,
                "name": row.name,
                "status": row.status,
            })).collect::<Vec<_>>(),
        })
    }
}

impl IntentAction {
    fn to_json(&self) -> Value {
        json!({
            "skipped": self.skipped,
            "put": self.put,
            "names": self.names,
        })
    }
}

impl CallsAction {
    fn to_json(&self) -> Value {
        json!({
            "skipped": self.skipped,
            "executed": self.executed,
        })
    }
}

pub struct FrozenRecord {
    pub path: PathBuf,
    pub module_hash: String,
    pub export_hash: String,
    pub contract_set: String,
    pub snapshot: Value,
    pub artifact: Value,
    pub var_names: Vec<String>,
}

struct Bundle {
    path: PathBuf,
    module_hash: String,
    export_hash: String,
    contract_set: String,
    var_names: Vec<String>,
    snapshot: Value,
    cluster: Value,
    intent: Value,
    calls: Value,
}

pub fn apply(
    frozen: &str,
    stdin: &mut impl Read,
) -> Result<(CommandOutput, FrozenRecord), FacetError> {
    let raw = if frozen == "-" {
        let mut buf = String::new();
        stdin
            .read_to_string(&mut buf)
            .map_err(|error| FacetError::stdin_bundle(&error))?;
        buf
    } else {
        std::fs::read_to_string(frozen).map_err(|error| {
            FacetError::invalid_workspace(format!("cannot read {frozen}: {error}"))
        })?
    };
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| frozen_err(format!("invalid JSON: {error}")))?;
    let bundle = parse_bundle(&value)?;
    let action = execute_projections(&bundle.cluster, &bundle.intent, &bundle.calls)?;
    let envelope = json!({
        "path": bundle.path.display().to_string(),
        "moduleHash": bundle.module_hash,
        "exportHash": bundle.export_hash,
        "contractSet": bundle.contract_set,
        "cluster": bundle.cluster,
        "intent": bundle.intent,
        "calls": bundle.calls,
        "action": action.to_json(),
    });
    let human = format!(
        "ok {} (moduleHash={} exportHash={} contractSet={} cluster.posted={} intent.put={})\n",
        bundle.path.display(),
        bundle.module_hash,
        bundle.export_hash,
        bundle.contract_set,
        action.cluster.posted,
        action.intent.put,
    );
    let record = FrozenRecord {
        path: bundle.path,
        module_hash: bundle.module_hash,
        export_hash: bundle.export_hash,
        contract_set: bundle.contract_set,
        snapshot: bundle.snapshot,
        artifact: envelope.clone(),
        var_names: bundle.var_names,
    };
    Ok((CommandOutput::new(human, envelope), record))
}

pub fn execute_projections(
    cluster: &Value,
    intent: &Value,
    calls: &Value,
) -> Result<ApplyAction, FacetError> {
    let cluster = apply_cluster(cluster)?;
    let intent = apply_intent(intent)?;
    let calls = apply_calls(calls)?;
    Ok(ApplyAction {
        cluster,
        intent,
        calls,
    })
}

fn frozen_err(message: impl Into<String>) -> FacetError {
    FacetError::invalid_arguments(format!("frozen bundle: {}", message.into()))
}

fn parse_bundle(value: &Value) -> Result<Bundle, FacetError> {
    let object = value
        .as_object()
        .ok_or_else(|| frozen_err("must be a JSON object"))?;
    for key in object.keys() {
        if !REQUIRED.contains(&key.as_str()) {
            return Err(frozen_err(format!("unknown key {key}")));
        }
    }
    for key in REQUIRED {
        if !object.contains_key(*key) {
            return Err(frozen_err(format!("missing key {key}")));
        }
    }
    if !is_exact_one(&object["schemaVersion"]) {
        return Err(frozen_err("schemaVersion must be the integer 1"));
    }
    let path = non_empty_str(&object["path"], "path")?;
    let module_hash = hex64(&object["moduleHash"], "moduleHash")?;
    let export_hash = hex64(&object["exportHash"], "exportHash")?;
    let contract_set = non_empty_str(&object["contractSet"], "contractSet")?;
    let var_names = parse_var_names(&object["varNames"])?;
    let snapshot = parse_snapshot(&object["sourceSnapshot"], &path)?;
    let entry = snapshot["entry"].as_str().unwrap();
    let source = snapshot["sources"][entry].as_str().unwrap();
    let computed = sha256_hex(source.as_bytes());
    if computed != module_hash {
        return Err(frozen_err(
            "moduleHash does not match sha256 of sourceSnapshot.sources[entry]",
        ));
    }
    let cluster = object["cluster"].clone();
    let intent = object["intent"].clone();
    let calls = object["calls"].clone();
    if !(cluster.is_array() || cluster.is_null()) {
        return Err(frozen_err("cluster must be an array or null"));
    }
    if !(intent.is_array() || intent.is_null()) {
        return Err(frozen_err("intent must be an array or null"));
    }
    if !(calls.is_object() || calls.is_null()) {
        return Err(frozen_err("calls must be an object or null"));
    }
    Ok(Bundle {
        path: PathBuf::from(path),
        module_hash,
        export_hash,
        contract_set,
        var_names,
        snapshot,
        cluster,
        intent,
        calls,
    })
}

fn is_exact_one(value: &Value) -> bool {
    matches!(value, Value::Number(n) if n.as_i64() == Some(1))
}

fn non_empty_str(value: &Value, key: &str) -> Result<String, FacetError> {
    let text = value
        .as_str()
        .ok_or_else(|| frozen_err(format!("{key} must be a string")))?;
    if text.is_empty() {
        return Err(frozen_err(format!("{key} must be non-empty")));
    }
    Ok(text.to_owned())
}

fn hex64(value: &Value, key: &str) -> Result<String, FacetError> {
    let text = non_empty_str(value, key)?;
    if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(frozen_err(format!(
            "{key} must be 64 lowercase hex characters"
        )));
    }
    Ok(text)
}

fn parse_var_names(value: &Value) -> Result<Vec<String>, FacetError> {
    let array = value
        .as_array()
        .ok_or_else(|| frozen_err("varNames must be an array"))?;
    let mut names = Vec::new();
    for item in array {
        let name = item
            .as_str()
            .ok_or_else(|| frozen_err("varNames entries must be strings"))?;
        if name.is_empty() {
            return Err(frozen_err("varNames entries must be non-empty"));
        }
        if name.contains('=') {
            return Err(frozen_err("varNames entries must not contain '='"));
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

fn parse_snapshot(value: &Value, path: &str) -> Result<Value, FacetError> {
    let object = value
        .as_object()
        .ok_or_else(|| frozen_err("sourceSnapshot must be an object"))?;
    if object.len() != 2 || !object.contains_key("entry") || !object.contains_key("sources") {
        return Err(frozen_err(
            "sourceSnapshot must be exactly {\"entry\", \"sources\"}",
        ));
    }
    let entry = object["entry"]
        .as_str()
        .ok_or_else(|| frozen_err("sourceSnapshot.entry must be a string"))?;
    if entry.is_empty() {
        return Err(frozen_err("sourceSnapshot.entry must be non-empty"));
    }
    let file_name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| frozen_err("path has no file name"))?;
    if entry != file_name {
        return Err(frozen_err(
            "sourceSnapshot.entry must equal the file name of path",
        ));
    }
    let sources = object["sources"]
        .as_object()
        .ok_or_else(|| frozen_err("sourceSnapshot.sources must be an object"))?;
    let mut map = BTreeMap::new();
    for (k, v) in sources {
        let text = v
            .as_str()
            .ok_or_else(|| frozen_err("sourceSnapshot.sources values must be strings"))?;
        map.insert(k.clone(), text.to_owned());
    }
    if !map.contains_key(entry) {
        return Err(frozen_err("sourceSnapshot.sources must contain entry"));
    }
    Ok(json!({"entry": entry, "sources": map}))
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn apply_cluster(cluster: &Value) -> Result<ClusterAction, FacetError> {
    let objects = cluster_objects(cluster)?;
    if objects.is_empty() {
        return Ok(ClusterAction {
            skipped: true,
            posted: 0,
            results: Vec::new(),
        });
    }
    let Some(api_base) = kube_api_base_from_env().map_err(http_config)? else {
        return Ok(ClusterAction {
            skipped: true,
            posted: 0,
            results: Vec::new(),
        });
    };
    let mut results = Vec::new();
    for object in objects {
        let url = post_url(&api_base, object)?;
        let status = http_post_json(&url, object)?;
        results.push(ClusterPostResult {
            kind: object["kind"].as_str().unwrap_or("").to_owned(),
            name: object["metadata"]["name"].as_str().unwrap_or("").to_owned(),
            status,
        });
    }
    Ok(ClusterAction {
        skipped: false,
        posted: results.len(),
        results,
    })
}

fn apply_intent(intent: &Value) -> Result<IntentAction, FacetError> {
    let rows = intent_rows(intent)?;
    if rows.is_empty() {
        return Ok(IntentAction {
            skipped: true,
            put: 0,
            names: Vec::new(),
        });
    }
    let Some(db_path) = std::env::var_os("FACET_HEDRON_DB") else {
        return Ok(IntentAction {
            skipped: true,
            put: 0,
            names: Vec::new(),
        });
    };
    if db_path.is_empty() {
        return Err(FacetError::invalid_arguments("FACET_HEDRON_DB is empty"));
    }
    let path = PathBuf::from(db_path);
    let (mut store, token) = open_hedron_store(&path)?;
    let mut names = Vec::new();
    for row in rows {
        let name = row["name"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| FacetError::invalid_arguments("intent rows require a non-empty name"))?;
        let importance = row["importance"].as_f64().unwrap_or(0.5);
        let spec = &row["spec"];
        check_shape(spec).map_err(|error| {
            FacetError::invalid_arguments(format!("intent spec shape: {error}"))
        })?;
        let yaml_spec = serde_yaml::from_str(&serde_json::to_string(spec).map_err(|error| {
            FacetError::invalid_arguments(format!("intent spec is not JSON-serializable: {error}"))
        })?)
        .map_err(|error| {
            FacetError::invalid_arguments(format!("intent spec is not YAML-serializable: {error}"))
        })?;
        store
            .put_desired_state(&token, name, yaml_spec, importance)
            .map_err(hedron_error)?;
        names.push(name.to_owned());
    }
    Ok(IntentAction {
        skipped: false,
        put: names.len(),
        names,
    })
}

fn apply_calls(calls: &Value) -> Result<CallsAction, FacetError> {
    let items = calls
        .get("items")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty());
    if items.is_none() {
        return Ok(CallsAction {
            skipped: true,
            executed: 0,
        });
    }
    Ok(CallsAction {
        skipped: true,
        executed: 0,
    })
}

fn cluster_objects(cluster: &Value) -> Result<Vec<&Value>, FacetError> {
    if cluster.is_null() {
        return Ok(Vec::new());
    }
    let array = cluster
        .as_array()
        .ok_or_else(|| FacetError::invalid_arguments("cluster projection must be an array"))?;
    Ok(array.iter().collect())
}

fn intent_rows(intent: &Value) -> Result<Vec<&Value>, FacetError> {
    if intent.is_null() {
        return Ok(Vec::new());
    }
    let array = intent
        .as_array()
        .ok_or_else(|| FacetError::invalid_arguments("intent projection must be an array"))?;
    Ok(array.iter().collect())
}

fn post_url(api_base: &str, object: &Value) -> Result<String, FacetError> {
    let api_version = object["apiVersion"]
        .as_str()
        .ok_or_else(|| FacetError::invalid_arguments("cluster object requires apiVersion"))?;
    let kind = object["kind"]
        .as_str()
        .ok_or_else(|| FacetError::invalid_arguments("cluster object requires kind"))?;
    if api_version != "v1" {
        return Err(FacetError::invalid_arguments(format!(
            "unsupported apiVersion {api_version} in frozen cluster object"
        )));
    }
    let namespace = object["metadata"]["namespace"]
        .as_str()
        .unwrap_or("default");
    let resource = kind_to_resource(kind)?;
    Ok(format!(
        "{}/{}/namespaces/{}/{}",
        api_base.trim_end_matches('/'),
        "api/v1",
        namespace,
        resource
    ))
}

fn kind_to_resource(kind: &str) -> Result<&'static str, FacetError> {
    match kind {
        "Pod" => Ok("pods"),
        "Service" => Ok("services"),
        other => Err(FacetError::invalid_arguments(format!(
            "unsupported cluster kind {other}"
        ))),
    }
}

fn json_post_request(url: &str, body: &Value) -> HttpRequest {
    HttpRequest {
        metadata: ItemMetadata::default(),
        method: Some("POST".to_owned()),
        url: Some(url.to_owned()),
        headers: vec![Header {
            name: "Content-Type".to_owned(),
            value: "application/json".to_owned(),
            disabled: false,
        }],
        query_parameters: Vec::new(),
        path_parameters: Vec::new(),
        body: Some(RequestBody::Single(Body::Raw(RawBody {
            kind: RawBodyKind::Json,
            data: serde_json::to_string(body).expect("cluster JSON serialization cannot fail"),
        }))),
        authentication: None,
        settings: RequestSettings::default(),
    }
}

fn http_post_json(url: &str, body: &Value) -> Result<u16, FacetError> {
    let request = json_post_request(url, body);
    let options = ExecutionOptions {
        base_directory: None,
        ..ExecutionOptions::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| FacetError::runtime(&error))?;
    let engine = http_engine_from_env();
    let mut result = runtime.block_on(async {
        let engine = engine?;
        engine
            .execute_cancellable(&request, &options, tokio::signal::ctrl_c())
            .await
    });
    if result.is_err()
        && let Some(Ok(response)) = api_fallback(&request, TransportLock::Open, &[])
    {
        result = Ok(response);
    }
    let status = result
        .as_ref()
        .map(|response| response.status)
        .map_err(FacetError::http)?;
    Ok(status)
}

fn http_config(error: HttpError) -> FacetError {
    match error {
        HttpError::ClientConfiguration(message) => FacetError::invalid_arguments(message),
        other => FacetError::http(&other),
    }
}

fn hedron_error(error: hedron_core::Error) -> FacetError {
    FacetError::invalid_arguments(error.to_string())
}

fn open_hedron_store(path: &Path) -> Result<(Store, String), FacetError> {
    let mut store = Store::open(path).map_err(hedron_error)?;
    if let Ok(token) = std::env::var("FACET_HEDRON_TOKEN")
        && !token.is_empty()
    {
        return Ok((store, token));
    }
    if hedrondb_empty(path)? {
        let boot = store
            .bootstrap(
                &hedron_env("FACET_HEDRON_VAULT", "facet"),
                &hedron_env("FACET_HEDRON_AGENT", "facet"),
                &hedron_env("FACET_HEDRON_HTEC", "facet"),
            )
            .map_err(hedron_error)?;
        return Ok((store, boot.token));
    }
    Err(FacetError::invalid_arguments(
        "FACET_HEDRON_TOKEN is required for a non-empty HedronDB store",
    ))
}

fn hedrondb_empty(path: &Path) -> Result<bool, FacetError> {
    let conn = rusqlite::Connection::open(path).map_err(|error| {
        FacetError::invalid_arguments(format!("cannot open HedronDB store: {error}"))
    })?;
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
        .map_err(|error| {
            FacetError::invalid_arguments(format!("cannot read HedronDB nodes: {error}"))
        })?;
    Ok(count == 0)
}

fn hedron_env(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;
    use serde_json::json;

    #[test]
    fn docs_eod_sha256_is_pinned() {
        let src = include_str!("docs_eod.rs");
        assert_eq!(
            sha256_hex(src.as_bytes()),
            "e013d039590950065a0006aaf8868e3eee97af154e59aad1ab8c67cda5b9b33d"
        );
        let needle = ["hedron", "ncl"].join("_");
        assert!(!src.contains(&format!("use {needle}")));
        assert!(!src.contains(&format!("{needle}::")));
    }

    #[test]
    fn frozen_apply_source_has_no_hedron_ncl() {
        let src = include_str!("frozen_apply.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        let needle = ["hedron", "ncl"].join("_");
        assert!(!prod.contains(&format!("use {needle}")));
        assert!(!prod.contains(&format!("{needle}::")));
    }

    #[test]
    fn docs_eod_check_shape_pinned_fixtures() {
        let valid = json!({"kind":"docs_eod","date":"2026-09-12","required_briefs":[]});
        let e4 = json!({"kind":"docs_eod","date":1,"required_briefs":[]});
        let e6 = json!({"kind":"nope","date":"2026-09-12","required_briefs":[]});
        assert_eq!(
            crate::docs_eod::check_shape(&valid)
                .map(|s| s.date)
                .map_err(|e| e.to_string()),
            Ok("2026-09-12".to_owned())
        );
        assert_eq!(
            crate::docs_eod::check_shape(&e4).map_err(|e| e.to_string()),
            Err("docs_eod spec: invalid type: integer `1`, expected a string".to_owned())
        );
        assert_eq!(
            crate::docs_eod::check_shape(&e6).map_err(|e| e.to_string()),
            Err("unknown desired state kind \"nope\"".to_owned())
        );
    }
}
