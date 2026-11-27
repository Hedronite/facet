//! Lattice eval ledger for `facet apply --frozen` (T3/T4).

use std::path::{Path, PathBuf};

use facet_record::{
    ConfigOverrides, actor_from_env, open_store, recording_disabled, session_from_env,
};
use lattice::{BodyInput, MachineStore, NewRun, RunRow, WorkspaceStore, now_ms};
use serde_json::{Value, json};

use crate::FacetError;

const METHOD: &str = "NCL";
const APPLY_SELECTOR: &str = "ncl:apply";

/// Best-effort Lattice row for a successful frozen apply. Never fails the apply.
pub(crate) fn record_frozen_apply(
    path: &Path,
    module_hash: &str,
    export_hash: &str,
    contract_set: &str,
    snapshot: &Value,
    artifact: &Value,
    var_names: &[String],
) {
    if recording_disabled() {
        return;
    }
    if let Err(error) = try_record_frozen(
        path,
        module_hash,
        export_hash,
        contract_set,
        snapshot,
        artifact,
        var_names,
    ) {
        eprintln!("facet: ncl apply ledger: {error:?}");
    }
}

pub(crate) fn workspace_root(path: &Path) -> Result<PathBuf, FacetError> {
    let absolute = std::path::absolute(path).map_err(|error| {
        FacetError::invalid_workspace(format!("cannot resolve {}: {error}", path.display()))
    })?;
    if let Some(root) = WorkspaceStore::discover(&absolute) {
        return Ok(root);
    }
    Ok(absolute.parent().map(Path::to_path_buf).unwrap_or(absolute))
}

pub(crate) fn module_path(workspace_root: &Path, path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute
        .strip_prefix(workspace_root)
        .unwrap_or(&absolute)
        .display()
        .to_string()
}

fn index_run(store: &WorkspaceStore, run: &RunRow) {
    let machine = match MachineStore::open(store.config()) {
        Ok(machine) => machine,
        Err(_) => return,
    };
    let _ = machine.touch_workspace(store.workspace_id(), store.root(), None, now_ms());
    if let Some(session_id) = run.session_id.as_deref() {
        let _ = machine.ensure_session(session_id, &run.actor, now_ms());
    }
    let _ = machine.index_run(run, store.workspace_id());
}

fn try_record_frozen(
    path: &Path,
    module_hash: &str,
    export_hash: &str,
    contract_set: &str,
    snapshot: &Value,
    artifact: &Value,
    var_names: &[String],
) -> Result<(), FacetError> {
    let workspace_root = workspace_root(path)?;
    let store = open_store(
        &workspace_root,
        &ConfigOverrides {
            inline_body_max: Some(0),
            history_retention: None,
        },
    )
    .map_err(FacetError::lattice)?;

    let source_blob = serde_json::to_vec(snapshot)
        .map_err(|error| FacetError::invalid_arguments(error.to_string()))?;
    let freeze_blob = serde_json::to_vec(artifact)
        .map_err(|error| FacetError::invalid_arguments(error.to_string()))?;

    let tags = json!([
        format!("ncl:module:{module_hash}"),
        format!("ncl:export:{export_hash}"),
        format!("ncl:contracts:{contract_set}"),
    ])
    .to_string();
    let var_names = json!(var_names).to_string();
    let request_path = module_path(&workspace_root, path);
    let actor = actor_from_env();
    let session = session_from_env();

    let new_run = NewRun {
        started_at: now_ms(),
        duration_ms: Some(0),
        request_path: APPLY_SELECTOR,
        request_hash: module_hash,
        environment: None,
        method: METHOD,
        url: &request_path,
        status: Some(200),
        error: None,
        req_headers: None,
        res_headers: Some(r#"{"content-type":"application/json"}"#),
        req_body: BodyInput::Bytes(&source_blob),
        res_body: BodyInput::Bytes(&freeze_blob),
        res_content_type: Some("application/json"),
        session_id: session.as_deref(),
        actor: &actor,
        tags: Some(&tags),
        replayed_from: None,
        var_names: Some(&var_names),
    };

    let run = store.record_run(&new_run).map_err(FacetError::lattice)?;
    index_run(&store, &run);
    Ok(())
}
