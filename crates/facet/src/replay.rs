//! `facet replay <runId> [<path>]`: re-resolve the **current** YAML at the
//! recorded environment and send it again, recording lineage.
//!
//! Replay truth is current YAML plus recorded environment. There is no
//! "send stored bytes" mode: stored headers are redacted, auth enters the
//! hash by scheme only, and request bodies are content-addressed blobs that
//! may hold secrets. `--frozen` is refuse-on-change only: when the resolved
//! request hashes differently from the recorded row, exit 1 with no network
//! and no Lattice row.
//!
//! Like `request run`, everything replay prints goes through one
//! [`Redactor`], whose known values include the token replay sends.

use std::io::Read;

use facet_record::{
    RecordRequest, Recording, Redactor, actor_from_env, record, recording_disabled,
    request_hash_of, session_from_env,
};
use lattice::{MachineStore, WorkspaceStore};
use serde_json::json;

use crate::{
    CommandOutput, FacetError, args,
    expect::{EXPECT_FAIL_TAG, Expectation},
    run::{
        add_auth_token, execute, execute_locked, hydrate, lookup_request, parse_vars, render,
        resolve_selected, var_names, with_secrets,
    },
    workspace::{WorkspaceInput, load, open_store, overrides_from_parsed},
};

const VALUE_FLAGS: &[&str] = &[
    "--environment",
    "--var",
    "--tag",
    "--expect",
    "--inline-body-max",
    "--history-retention",
];
const SWITCH_FLAGS: &[&str] = &["--frozen", "--strict-variables", "--no-record"];

pub(crate) fn replay(args: &[String], stdin: &mut impl Read) -> Result<CommandOutput, FacetError> {
    let mut redactor = Redactor::default();
    match replay_unredacted(args, stdin, &mut redactor) {
        Ok(output) => Ok(output.redact(&redactor)),
        Err(error) => Err(error.redact(&redactor)),
    }
}

/// `replay` before redaction. Fills `redactor` once the run knows its secret
/// values.
fn replay_unredacted(
    args: &[String],
    stdin: &mut impl Read,
    redactor: &mut Redactor,
) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(args, VALUE_FLAGS, SWITCH_FLAGS)?;
    let (run_id, path) = match parsed.positionals() {
        [run_id] => (run_id.as_str(), "."),
        [run_id, path] => (run_id.as_str(), path.as_str()),
        _ => {
            return Err(FacetError::invalid_arguments(
                "replay requires <runId> and an optional <path>",
            ));
        }
    };
    let input = WorkspaceInput::from_argument(path);
    let Some(base) = input.base_directory() else {
        return Err(FacetError::invalid_arguments(
            "replay needs a collection on disk (a stdin workspace has no Lattice store)",
        ));
    };
    let frozen = parsed.switch("--frozen");
    let strict_variables = parsed.switch("--strict-variables");
    let should_record = !parsed.switch("--no-record") && !recording_disabled();
    let overrides = overrides_from_parsed(&parsed)?;
    let variables = parse_vars(&parsed)?;
    let expect = parsed
        .value("--expect")?
        .map(Expectation::parse)
        .transpose()?;
    let extra_tags: Vec<String> = parsed
        .values("--tag")
        .into_iter()
        .map(str::to_owned)
        .collect();

    // 1. The run row, from the workspace store beside the collection.
    let root =
        WorkspaceStore::discover(&base).ok_or_else(|| FacetError::lattice_not_found(&base))?;
    let store = open_store(&root, &overrides)?;
    let Some(row) = store.run(run_id).map_err(FacetError::lattice)? else {
        return Err(run_not_found_with_breadcrumb(&store, run_id));
    };

    // 2. Selector and environment come from the row; `--environment` wins.
    let selector = row.request_path.as_str();
    let environment = parsed
        .value("--environment")?
        .map(str::to_owned)
        .or_else(|| row.environment.clone());

    // 3. Recorded `--var` names are a warning, never replayed (values are
    //    not stored). `None` means the row predates 0003.
    let mut warnings = Vec::new();
    if let Some(recorded) = row
        .var_names
        .as_deref()
        .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
    {
        let passed = var_names(&variables);
        let missing: Vec<&str> = recorded
            .iter()
            .filter(|name| !passed.contains(name))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "run {} was recorded with --var {}; pass them again",
                row.id,
                missing.join(", ")
            ));
        }
    }

    // 4. Resolve the current YAML and compare hashes before any network.
    let loaded = load(&input, stdin)?;
    let raw = lookup_request(&loaded, selector)
        .map_err(|error| error.with_details(json!({ "replayedFrom": row.id })))?;
    let mut hydration = hydrate(
        Some(&base),
        environment.as_deref(),
        raw,
        &loaded,
        &variables,
    )?;
    let mut merged_variables = hydration.merged(&variables);
    add_auth_token(raw, &mut hydration, &mut merged_variables);
    *redactor = Redactor::new(hydration.redact.iter().cloned());
    let request = resolve_selected(
        &loaded,
        raw,
        environment.as_deref(),
        &merged_variables,
        strict_variables,
    )
    .map_err(|error| error.with_details(json!({ "replayedFrom": row.id })))?;
    let current_hash = request_hash_of(&request);
    let hash_changed = current_hash != row.request_hash;
    if hash_changed && frozen {
        return Err(FacetError::replay_changed(
            &row.id,
            &row.request_hash,
            &current_hash,
        ));
    }
    if hash_changed {
        warnings.push(format!(
            "request changed since {}: recorded {}…, current {}…",
            row.id,
            &row.request_hash[..12.min(row.request_hash.len())],
            &current_hash[..12.min(current_hash.len())]
        ));
    }

    // 5. Send and record with lineage.
    let execution = if hydration.references_secret {
        execute_locked(&request, input.base_directory(), None)?
    } else {
        execute(&request, input.base_directory(), None)?
    };
    let status = execution
        .result
        .as_ref()
        .ok()
        .map(|response| response.status);
    let miss = expect.as_ref().and_then(|expect| expect.miss(status));
    let mut tags = merged_tags(row.tags.as_deref(), &extra_tags);
    if miss.is_some() && !tags.iter().any(|tag| tag == EXPECT_FAIL_TAG) {
        tags.push(EXPECT_FAIL_TAG.to_owned());
    }
    let recording = if should_record {
        let actor = actor_from_env();
        let session = session_from_env();
        record(&RecordRequest {
            root: Some(&base),
            overrides: &overrides,
            selector,
            environment: environment.as_deref(),
            request: &request,
            started_at: execution.started_at,
            elapsed_ms: execution.elapsed_ms,
            result: &execution.result,
            output: None,
            tags: &tags,
            actor: &actor,
            session: session.as_deref(),
            replayed_from: Some(&row.id),
            var_names: &var_names(&variables),
            redact: &hydration.redact,
        })
    } else {
        Recording::Skipped("disabled")
    };
    if let Some(warning) = recording.warning() {
        warnings.push(warning);
    }

    let (mut lattice_json, lattice_human) = with_secrets(
        recording.json(),
        recording.human(),
        &hydration,
        environment.is_some(),
    );
    lattice_json["replayedFrom"] = json!(row.id);
    lattice_json["hashChanged"] = json!(hash_changed);
    let lattice_human = format!(
        "{lattice_human} · replayed from {} ({})",
        row.id,
        if hash_changed {
            "request changed"
        } else {
            "request unchanged"
        }
    );
    let rendered = render(
        &request,
        execution,
        None,
        lattice_json,
        lattice_human,
        warnings,
        redactor,
    )?;
    Ok(match miss {
        Some(error) => rendered.fail(error),
        None => rendered,
    })
}

/// Recorded user tags first, then `--tag` additions, without duplicates.
fn merged_tags(recorded: Option<&str>, extra: &[String]) -> Vec<String> {
    // The source's own tags carry over; Facet's auto-tags (`git:`,
    // `expect:`) describe that run and are recomputed for this one.
    let mut tags: Vec<String> = recorded
        .and_then(|text| serde_json::from_str::<Vec<String>>(text).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|tag| !facet_record::is_auto_tag(tag))
        .collect();
    for tag in extra {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
    tags
}

/// `run_not_found`, with the workspace the machine index knows the run
/// under when it lives elsewhere (one query; best effort).
fn run_not_found_with_breadcrumb(store: &WorkspaceStore, run_id: &str) -> FacetError {
    let error = FacetError::run_not_found(run_id);
    let elsewhere = MachineStore::open(store.config())
        .ok()
        .and_then(|machine| machine.indexed_run(run_id).ok().flatten());
    match elsewhere {
        Some((workspace_id, path)) if workspace_id != store.workspace_id() => {
            error.with_details(json!({
                "workspaceId": workspace_id,
                "workspacePath": path,
            }))
        }
        _ => error,
    }
}
