//! Golden-file JSON contract for every Facet-owned command (Surface 6), plus
//! the behaviors an agent depends on: recording, the reader rule from the CLI
//! side, read-only SQL, and a dry-run-by-default gc.

#[allow(dead_code, unused_imports)]
mod common;

use common::*;

const ECHO_BODY: &[u8] = br#"{"users":[]}"#;

fn record_one_run(sandbox: &Sandbox, extra: &[&str]) -> Value {
    record_run_with(sandbox, &[], extra)
}

/// A run whose mock server answers with `body` instead of [`ECHO_BODY`].
fn record_run_body(sandbox: &Sandbox, body: &[u8]) -> Value {
    let (url, server) = serve_once(body.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let value = sandbox.run_json(&[
        "request",
        "run",
        workspace.to_str().unwrap(),
        "items/0",
        "--environment",
        "local",
    ]);
    server.join().unwrap();
    value
}

/// `request run … --json` against a one-shot echo server, with extra
/// environment variables (`FACET_SESSION`, `FACET_ACTOR`) as a harness sets them.
fn record_run_with(sandbox: &Sandbox, env: &[(&str, &str)], extra: &[&str]) -> Value {
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let mut arguments = vec![
        "request",
        "run",
        workspace.to_str().unwrap(),
        "items/0",
        "--environment",
        "local",
    ];
    arguments.extend_from_slice(extra);
    let mut command = sandbox.facet();
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.args(&arguments).arg("--json").output().unwrap();
    let sent = server.join().unwrap();
    assert_eq!(sent, br#"{"source":"cli"}"#);
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
}

fn run_count(sandbox: &Sandbox, arguments: &[&str]) -> usize {
    sandbox.run_json(arguments)["runs"]
        .as_array()
        .expect("runs array")
        .len()
}

/// A well-formed ULID no store has ever minted.
const UNKNOWN_ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

#[test]
fn version_json_is_golden_and_names_probe() {
    let sandbox = Sandbox::new();
    let value = sandbox.run_json(&["--version"]);
    assert_eq!(value["name"], "facet");
    assert_golden("version.json", &normalize(value));
}

#[test]
fn request_run_keeps_the_upstream_envelope_and_records() {
    let sandbox = Sandbox::new();
    let value = record_one_run(&sandbox, &[]);

    // Upstream fields, byte for byte in shape.
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["response"]["status"], 200);
    assert_eq!(value["response"]["body"]["content"], r#"{"users":[]}"#);
    assert_eq!(value["response"]["body"]["omitted"], false);
    // Facet's additive field.
    assert_eq!(value["lattice"]["recorded"], true);
    assert_eq!(value["lattice"]["indexed"], true);
    assert_eq!(value["lattice"]["responseBody"]["retention"], "inline");
    assert_golden("run.json", &normalize(value));

    let facet = sandbox.root().join(".facet");
    assert!(facet.join("lattice.db").is_file());
    assert!(facet.join("workspace.toml").is_file());
    assert!(facet.join(".gitignore").is_file());
    assert!(sandbox.root().join("machine/lattice.db").is_file());
}

#[test]
fn history_json_is_golden_and_carries_no_bodies_by_default() {
    let sandbox = Sandbox::new();
    let run = record_one_run(&sandbox, &[]);

    let history = sandbox.run_json(&["history", sandbox.root().to_str().unwrap()]);
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    let row = &history["runs"][0];
    assert_eq!(row["id"], run["lattice"]["runId"]);
    assert_eq!(row["requestPath"], "items/0");
    assert_eq!(row["environment"], "local");
    assert_eq!(row["status"], 200);
    assert_eq!(row["actor"], "human");
    assert_eq!(row["response"]["body"]["sizeBytes"], 12);
    assert!(row["response"]["body"].get("content").is_none());
    assert_golden("history.json", &normalize(history));

    // Payloads are explicit pulls. Response bodies under the inline
    // threshold hydrate via --bodies; request bodies are hash-only (v2), so
    // --bodies shows the hash and omits the content (hydrate via `facet blob`).
    let with_bodies = sandbox.run_json(&["history", sandbox.root().to_str().unwrap(), "--bodies"]);
    assert_eq!(
        with_bodies["runs"][0]["response"]["body"]["content"],
        r#"{"users":[]}"#
    );
    assert_eq!(
        with_bodies["runs"][0]["request"]["body"]["retention"],
        "blob"
    );
    assert!(with_bodies["runs"][0]["request"]["body"]["content"].is_null());
    assert_golden("history_bodies.json", &normalize(with_bodies));

    // Filters.
    let none = sandbox.run_json(&[
        "history",
        sandbox.root().to_str().unwrap(),
        "--status",
        "500",
    ]);
    assert_eq!(none["runs"].as_array().unwrap().len(), 0);
}

#[test]
fn history_without_a_store_is_empty_not_an_error() {
    let sandbox = Sandbox::new();
    let history = sandbox.run_json(&["history", sandbox.root().to_str().unwrap()]);
    assert!(history["workspace"].is_null());
    assert_eq!(history["runs"], json!([]));
}

#[test]
fn history_sql_is_golden_and_read_only() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &["--tag", "smoke"]);
    let root = sandbox.root().to_str().unwrap();

    let result = sandbox.run_json(&[
        "history",
        root,
        "--sql",
        "SELECT request_path, status, actor, tags FROM runs",
    ]);
    assert_eq!(
        result["columns"],
        json!(["request_path", "status", "actor", "tags"])
    );
    assert_eq!(
        result["rows"][0],
        json!(["items/0", 200, "human", "[\"smoke\"]"])
    );
    assert_golden("history_sql.json", &normalize(result));

    let (write_code, write_error) =
        sandbox.run_error_json(&["history", root, "--sql", "DELETE FROM runs"]);
    assert_eq!(write_code, 2);
    assert_eq!(write_error["error"]["category"], "sql_read_only");
    assert_golden("error_sql_read_only.json", &normalize_error(write_error));

    let (bad_code, bad_error) = sandbox.run_error_json(&["history", root, "--sql", "SELEC nope"]);
    assert_eq!(bad_code, 2);
    assert_eq!(bad_error["error"]["category"], "invalid_sql");
    assert_golden("error_invalid_sql.json", &normalize_error(bad_error));

    let still_there = sandbox.run_json(&["history", root, "--sql", "SELECT count(*) FROM runs"]);
    assert_eq!(still_there["rows"][0][0], 1);
}

#[test]
fn history_sql_without_a_store_is_lattice_not_found() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .facet()
        .args([
            "history",
            sandbox.root().to_str().unwrap(),
            "--sql",
            "SELECT 1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(9));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["category"], "lattice_not_found");
    assert_eq!(error["error"]["exitCode"], 9);
}

#[test]
fn blob_json_is_golden_and_raw_mode_streams_bytes() {
    let sandbox = Sandbox::new();
    // A zero threshold makes every body a blob.
    let run = record_one_run(&sandbox, &["--inline-body-max", "0"]);
    assert_eq!(run["lattice"]["responseBody"]["retention"], "blob");
    let hash = run["lattice"]["responseBody"]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(hash.len(), 64);
    let root = sandbox.root().to_str().unwrap();
    assert!(sandbox.root().join(".facet/blobs").join(&hash).is_file());

    let blob = sandbox.run_json(&["blob", &hash, root]);
    assert_eq!(blob["blob"]["sizeBytes"], 12);
    assert_eq!(blob["blob"]["contentType"], "application/json");
    assert_eq!(blob["blob"]["body"]["content"], r#"{"users":[]}"#);
    assert_golden("blob.json", &normalize(blob));

    let raw = sandbox
        .facet()
        .args(["blob", &hash, root])
        .output()
        .unwrap();
    assert!(raw.status.success());
    assert_eq!(raw.stdout, ECHO_BODY);

    let target = sandbox.root().join("body.json");
    let written = sandbox.run_json(&["blob", &hash, root, "--output", target.to_str().unwrap()]);
    assert_eq!(written["blob"]["body"]["content"], Value::Null);
    assert_eq!(fs::read(&target).unwrap(), ECHO_BODY);

    // History never carries a blob body inline, even when asked.
    let history = sandbox.run_json(&["history", root, "--bodies"]);
    let body = &history["runs"][0]["response"]["body"];
    assert_eq!(body["retention"], "blob");
    assert_eq!(body["hash"], hash);
    assert_eq!(body["omitted"], true);
    assert_eq!(body["omissionReason"], "blob");
}

#[test]
fn request_body_reader_rule_at_cli() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &["--inline-body-max", "0"]);
    let root = sandbox.root().to_str().unwrap();

    let history = sandbox.run_json(&["history", root]);
    let req_hash = history["runs"][0]["request"]["body"]["hash"]
        .as_str()
        .expect("request body hash when blobbed");
    assert_eq!(req_hash.len(), 64);
    assert!(sandbox.root().join(".facet/blobs").join(req_hash).is_file());

    let blob = sandbox.run_json(&["blob", req_hash, root]);
    assert_eq!(blob["blob"]["sizeBytes"], 16);
    assert_eq!(blob["blob"]["body"]["content"], r#"{"source":"cli"}"#);
    assert_golden("blob_request.json", &normalize(blob));

    let with_bodies = sandbox.run_json(&["history", root, "--bodies"]);
    let req = &with_bodies["runs"][0]["request"]["body"];
    assert_eq!(req["retention"], "blob");
    assert_eq!(req["hash"], req_hash);
    assert_eq!(req["omitted"], true);
    assert_eq!(req["omissionReason"], "blob");
    assert!(req["content"].is_null());
}

#[test]
fn blob_not_found_uses_exit_code_4() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &[]);
    let missing = "0".repeat(64);
    let (code, error) =
        sandbox.run_error_json(&["blob", &missing, sandbox.root().to_str().unwrap()]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "blob_not_found");
    assert_golden("error_blob_not_found.json", &normalize_error(error));
}

#[test]
fn gc_is_a_dry_run_unless_yes() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &["--inline-body-max", "0"]);
    let root = sandbox.root().to_str().unwrap();
    let blobs = sandbox.root().join(".facet/blobs");
    let orphan = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    fs::write(blobs.join(orphan), b"abc").unwrap();

    let dry = sandbox.run_json(&["gc", root]);
    assert_eq!(dry["applied"], false);
    assert_eq!(dry["orphans"][0]["hash"], orphan);
    assert_eq!(dry["bytesReclaimable"], 3);
    assert!(blobs.join(orphan).is_file());

    let applied = sandbox.run_json(&["gc", root, "--yes"]);
    assert_eq!(applied["applied"], true);
    assert!(!blobs.join(orphan).exists());
    // Threshold 0 blobbed both request and response; gc must keep both referenced files.
    assert_eq!(
        fs::read_dir(&blobs).unwrap().count(),
        2,
        "referenced req and res blobs survive"
    );

    let clean = sandbox.run_json(&["gc", root]);
    assert_eq!(clean["orphans"], json!([]));
    assert_golden("gc.json", &normalize(clean));
}

#[test]
fn gc_expires_runs_by_retention_at_cli() {
    let sandbox = Sandbox::new();
    let first = record_one_run(&sandbox, &["--inline-body-max", "0"]);
    let run_id = first["lattice"]["runId"].as_str().expect("recorded run id");
    sandbox.backdate_run(run_id, lattice::now_ms() - 3 * 86_400_000);
    record_one_run(&sandbox, &["--inline-body-max", "0"]);
    let root = sandbox.root().to_str().unwrap();

    let stamps = sandbox.run_json(&[
        "history",
        root,
        "--sql",
        "SELECT started_at FROM runs ORDER BY started_at ASC",
    ]);
    assert_eq!(stamps["rows"].as_array().unwrap().len(), 2);
    let gap = stamps["rows"][1][0].as_i64().unwrap() - stamps["rows"][0][0].as_i64().unwrap();
    assert!(
        gap > 86_400_000,
        "backdated run should be more than a day older"
    );

    let dry = sandbox.run_json(&["gc", root, "--history-retention", "1d"]);
    assert_eq!(dry["applied"], false);
    assert_eq!(dry["retention"], "1d");
    assert_eq!(dry["runsExpired"], 1);
    // Identical bodies share blob files; the fresh run keeps them referenced.
    assert_eq!(dry["orphans"].as_array().unwrap().len(), 0);
    assert_golden("gc_retention_expired.json", &normalize(dry));

    let applied = sandbox.run_json(&["gc", root, "--history-retention", "1d", "--yes"]);
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["runsExpired"], 1);

    let history = sandbox.run_json(&["history", root]);
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_dir(sandbox.root().join(".facet/blobs"))
            .unwrap()
            .count(),
        2,
        "fresh run keeps req and res blobs"
    );
}

#[test]
fn no_record_flag_skips_lattice() {
    let sandbox = Sandbox::new();
    let value = record_one_run(&sandbox, &["--no-record"]);
    assert_eq!(value["lattice"]["recorded"], false);
    assert_eq!(value["lattice"]["reason"], "disabled");
    assert!(!sandbox.root().join(".facet").exists());
}

#[test]
fn actor_and_tags_are_recorded() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let output = sandbox
        .facet()
        .env("FACET_ACTOR", "halo-qa")
        .args([
            "request",
            "run",
            workspace.to_str().unwrap(),
            "items/0",
            "--environment",
            "local",
            "--tag",
            "a",
            "--tag",
            "b",
            "--json",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(output.status.success());

    let history = sandbox.run_json(&[
        "history",
        sandbox.root().to_str().unwrap(),
        "--actor",
        "halo-qa",
    ]);
    assert_eq!(history["runs"][0]["actor"], "halo-qa");
    assert_eq!(history["runs"][0]["tags"], json!(["a", "b"]));
}

#[test]
fn missing_selector_matches_upstream_category_and_exit_code() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace("http://127.0.0.1:9");
    let output = sandbox
        .facet()
        .args([
            "request",
            "run",
            workspace.to_str().unwrap(),
            "items/99",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["category"], "request_not_found");
    assert!(!sandbox.root().join(".facet").exists());
}

#[test]
fn transport_failure_is_recorded_with_null_status() {
    let sandbox = Sandbox::new();
    // Port 9 (discard) is closed on a normal host; the connection is refused.
    let workspace = sandbox.workspace("http://127.0.0.1:9");
    let output = sandbox
        .facet()
        .args([
            "request",
            "run",
            workspace.to_str().unwrap(),
            "items/0",
            "--environment",
            "local",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["category"], "network_execution");
    assert_eq!(error["error"]["details"]["lattice"]["recorded"], true);

    let history = sandbox.run_json(&["history", sandbox.root().to_str().unwrap()]);
    let row = &history["runs"][0];
    assert!(row["status"].is_null());
    assert!(!row["error"].as_str().unwrap().is_empty());
    assert_eq!(row["response"]["body"]["retention"], "none");
}

#[test]
fn delegated_commands_are_untouched() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace("http://127.0.0.1:9");
    let value = sandbox.run_json(&["request", "list", workspace.to_str().unwrap()]);
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["requests"][0]["selector"], "items/0");
    assert!(!sandbox.root().join(".facet").exists());
}

#[test]
fn session_lifecycle_is_golden_and_start_prints_a_bare_ulid() {
    let sandbox = Sandbox::new();

    // Human mode prints the id alone so `$(facet session start)` needs no jq.
    let output = sandbox.facet().args(["session", "start"]).output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let bare = String::from_utf8(output.stdout).unwrap();
    assert!(bare.ends_with('\n'));
    let bare_id = bare.trim().to_owned();
    assert!(lattice::is_ulid(&bare_id), "not a ULID: {bare_id:?}");

    let started = sandbox.run_json(&[
        "session",
        "start",
        "--actor",
        "halo-qa",
        "--meta",
        r#"{"note":"golden"}"#,
    ]);
    let session = &started["session"];
    assert!(lattice::is_ulid(session["id"].as_str().unwrap()));
    assert_eq!(session["actor"], "halo-qa");
    assert!(session["startedAt"].is_number());
    assert!(session["endedAt"].is_null());
    assert_eq!(session["meta"]["note"], "golden");
    assert!(session["meta"]["cwd"].is_string());
    assert!(session["meta"].get("herdr").is_none(), "env is scrubbed");
    assert!(session.get("runs").is_none(), "no workspace store yet");
    assert_golden("session_start.json", &normalize(started.clone()));
    let id = session["id"].as_str().unwrap().to_owned();

    let shown = sandbox.run_json(&["session", "show", &id]);
    assert_eq!(shown["session"], started["session"]);
    let current = sandbox.run_json_in_session(&id, &["session", "show", "current"]);
    assert_eq!(current["session"]["id"], id);

    let open = sandbox.run_json(&["session", "list", "--open"]);
    let open_ids: Vec<&str> = open["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(open_ids.len(), 2);
    assert!(open_ids.contains(&id.as_str()) && open_ids.contains(&bare_id.as_str()));
    let by_actor = sandbox.run_json(&["session", "list", "--actor", "halo-qa"]);
    assert_eq!(by_actor["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(by_actor["sessions"][0]["id"], id);
    assert_golden("session_list.json", &normalize(by_actor));

    let ended = sandbox.run_json(&["session", "end", &id]);
    assert_eq!(ended["alreadyEnded"], false);
    assert!(ended["session"]["endedAt"].is_number());
    assert_golden("session_end.json", &normalize(ended.clone()));
    // Idempotent: the second end changes nothing and says so.
    let again = sandbox.run_json(&["session", "end", &id]);
    assert_eq!(again["alreadyEnded"], true);
    assert_eq!(again["session"]["endedAt"], ended["session"]["endedAt"]);
    // No id: FACET_SESSION.
    let from_env = sandbox.run_json_in_session(&bare_id, &["session", "end"]);
    assert_eq!(from_env["session"]["id"], bare_id);
    assert_eq!(from_env["alreadyEnded"], false);
    let none_open = sandbox.run_json(&["session", "list", "--open"]);
    assert_eq!(none_open["sessions"], json!([]));
    assert_eq!(
        sandbox.run_json(&["session", "list"])["sessions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let (code, error) = sandbox.run_error_json(&["session", "end"]);
    assert_eq!(code, 5);
    assert_eq!(error["error"]["category"], "session_not_set");
    let (code, error) = sandbox.run_error_json(&["session", "show", UNKNOWN_ULID]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "session_not_found");
    assert_golden("error_session_not_found.json", &normalize_error(error));
    let (code, error) = sandbox.run_error_json(&["session", "end", UNKNOWN_ULID]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "session_not_found");
    let (code, error) = sandbox.run_error_json(&["session", "start", "--meta", "[1]"]);
    assert_eq!(code, 2);
    assert_eq!(error["error"]["category"], "invalid_arguments");
    let (code, _) = sandbox.run_error_json(&["session", "frobnicate"]);
    assert_eq!(code, 2);
}

#[test]
fn history_id_is_golden_and_exclusive_with_filters() {
    let sandbox = Sandbox::new();
    let run = record_one_run(&sandbox, &[]);
    let run_id = run["lattice"]["runId"].as_str().unwrap();
    let root = sandbox.root().to_str().unwrap();

    let one = sandbox.run_json(&["history", root, "--id", run_id]);
    assert!(one.get("runs").is_none(), "--id is one object, not a list");
    assert_eq!(one["run"]["id"], run_id);
    assert_eq!(one["run"]["requestPath"], "items/0");
    assert_eq!(one["workspace"]["id"], run["lattice"]["workspaceId"]);
    assert!(one["run"]["response"]["body"].get("content").is_none());
    assert_golden("history_id.json", &normalize(one));

    // --bodies behaves exactly as on the list.
    let with_bodies = sandbox.run_json(&["history", root, "--id", run_id, "--bodies"]);
    assert_eq!(
        with_bodies["run"]["response"]["body"]["content"],
        r#"{"users":[]}"#
    );

    let human = sandbox
        .facet()
        .args(["history", root, "--id", run_id])
        .output()
        .unwrap();
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert_eq!(text.lines().count(), 2, "header plus one row");
    assert!(text.lines().nth(1).unwrap().starts_with(run_id));

    let (code, error) = sandbox.run_error_json(&["history", root, "--id", UNKNOWN_ULID]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
    assert_golden("error_run_not_found.json", &normalize_error(error));

    let (code, error) =
        sandbox.run_error_json(&["history", root, "--id", run_id, "--status", "200"]);
    assert_eq!(code, 2);
    assert_eq!(error["error"]["category"], "invalid_arguments");
    let (code, _) = sandbox.run_error_json(&["history", root, "--id", run_id, "--tag", "x"]);
    assert_eq!(code, 2);
    let (code, _) = sandbox.run_error_json(&["history", root, "--id", run_id, "--sql", "SELECT 1"]);
    assert_eq!(code, 2);

    // No store at all: the run cannot exist.
    let empty = Sandbox::new();
    let (code, error) =
        empty.run_error_json(&["history", empty.root().to_str().unwrap(), "--id", run_id]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
}

#[test]
fn history_session_filter_and_mint_if_missing() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().to_str().unwrap();
    let started = sandbox.run_json(&["session", "start", "--actor", "claude.halo-fullstack"]);
    let id = started["session"]["id"].as_str().unwrap().to_owned();

    let first = record_run_with(&sandbox, &[("FACET_SESSION", &id)], &[]);
    let second = record_run_with(&sandbox, &[("FACET_SESSION", &id)], &["--tag", "smoke"]);
    let outside = record_one_run(&sandbox, &[]);
    assert_eq!(run_count(&sandbox, &["history", root]), 3);

    let in_session = sandbox.run_json(&["history", root, "--session", &id]);
    let rows = in_session["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["sessionId"] == id));
    let ids: Vec<&Value> = rows.iter().map(|row| &row["id"]).collect();
    assert!(ids.contains(&&first["lattice"]["runId"]));
    assert!(ids.contains(&&second["lattice"]["runId"]));
    assert!(!ids.contains(&&outside["lattice"]["runId"]));
    assert_golden("history_session.json", &normalize(in_session));

    // `current` reads FACET_SESSION; without it the agent gets a clear exit 5.
    let current = sandbox.run_json_in_session(&id, &["history", root, "--session", "current"]);
    assert_eq!(current["runs"].as_array().unwrap().len(), 2);
    let (code, error) = sandbox.run_error_json(&["history", root, "--session", "current"]);
    assert_eq!(code, 5);
    assert_eq!(error["error"]["category"], "session_not_set");
    assert_golden("error_session_not_set.json", &normalize_error(error));

    // With a workspace store under the cwd, sessions report their run count.
    let shown = sandbox.run_json(&["session", "show", &id]);
    assert_eq!(shown["session"]["runs"], 2);
    let listed = sandbox.run_json(&["session", "list"]);
    assert_eq!(listed["sessions"][0]["runs"], 2);

    // Mint-if-missing: a harness that invents its own FACET_SESSION gets a
    // session row on the first recorded run, owned by that run's actor.
    let (code, error) = sandbox.run_error_json(&["session", "show", UNKNOWN_ULID]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "session_not_found");
    record_run_with(
        &sandbox,
        &[("FACET_SESSION", UNKNOWN_ULID), ("FACET_ACTOR", "halo-qa")],
        &[],
    );
    let minted = sandbox.run_json(&["session", "show", UNKNOWN_ULID]);
    assert_eq!(minted["session"]["id"], UNKNOWN_ULID);
    assert_eq!(minted["session"]["actor"], "halo-qa");
    assert!(minted["session"]["endedAt"].is_null());
    assert!(minted["session"]["meta"].is_null());
    assert_eq!(minted["session"]["runs"], 1);
    // A later run under the same id does not rewrite the row.
    record_run_with(
        &sandbox,
        &[
            ("FACET_SESSION", UNKNOWN_ULID),
            ("FACET_ACTOR", "someone-else"),
        ],
        &[],
    );
    let again = sandbox.run_json(&["session", "show", UNKNOWN_ULID]);
    assert_eq!(again["session"]["actor"], "halo-qa");
    assert_eq!(
        again["session"]["startedAt"],
        minted["session"]["startedAt"]
    );
    assert_eq!(again["session"]["runs"], 2);
    assert_golden("session_show.json", &normalize(again));
    assert_eq!(
        run_count(&sandbox, &["history", root, "--session", UNKNOWN_ULID]),
        2
    );
}

#[test]
fn history_environment_tag_and_hash_filters() {
    let sandbox = Sandbox::new();
    let tagged = record_one_run(
        &sandbox,
        &[
            "--tag",
            "smoke",
            "--tag",
            "golden",
            "--inline-body-max",
            "0",
        ],
    );
    record_one_run(&sandbox, &[]);
    let root = sandbox.root().to_str().unwrap();
    let tagged_id = tagged["lattice"]["runId"].as_str().unwrap();
    let response_hash = tagged["lattice"]["responseBody"]["hash"].as_str().unwrap();
    let request_hash = tagged["lattice"]["requestHash"].as_str().unwrap();

    assert_eq!(
        run_count(&sandbox, &["history", root, "--environment", "local"]),
        2
    );
    assert_eq!(
        run_count(&sandbox, &["history", root, "--environment", "prod"]),
        0
    );

    // --tag repeats and ANDs; untagged runs never match.
    assert_eq!(run_count(&sandbox, &["history", root, "--tag", "smoke"]), 1);
    assert_eq!(
        run_count(
            &sandbox,
            &["history", root, "--tag", "smoke", "--tag", "golden"]
        ),
        1
    );
    assert_eq!(
        run_count(
            &sandbox,
            &["history", root, "--tag", "smoke", "--tag", "nope"]
        ),
        0
    );

    // --hash is column-agnostic; the row says which column hit.
    let by_response =
        sandbox.run_json(&["history", root, "--tag", "smoke", "--hash", response_hash]);
    assert_eq!(by_response["runs"].as_array().unwrap().len(), 1);
    assert_eq!(by_response["runs"][0]["id"], tagged_id);
    assert_eq!(by_response["runs"][0]["matchedHash"], "responseBody");
    assert_golden("history_tag_hash.json", &normalize(by_response));

    // Each run hit a fresh mock server (different port, different URL), so
    // the resolved-request hash is unique per run; the request body bytes
    // are identical, so the request-body hash hits both.
    let by_request = sandbox.run_json(&["history", root, "--hash", request_hash]);
    let rows = by_request["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], tagged_id);
    assert_eq!(rows[0]["matchedHash"], "request");
    let request_body_hash = rows[0]["request"]["body"]["hash"].as_str().unwrap();
    let by_request_body = sandbox.run_json(&["history", root, "--hash", request_body_hash]);
    let rows = by_request_body["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row["matchedHash"] == "requestBody"));

    // Hashes compare case-insensitively, like `blob`.
    let upper = sandbox.run_json(&["history", root, "--hash", &response_hash.to_uppercase()]);
    assert_eq!(upper["runs"].as_array().unwrap().len(), 1);
    let (code, _) =
        sandbox.run_error_json(&["history", root, "--hash", "nope", "--sql", "SELECT 1"]);
    assert_eq!(code, 2);

    // Without --hash the row shape is unchanged.
    let plain = sandbox.run_json(&["history", root]);
    assert!(plain["runs"][0].get("matchedHash").is_none());
    assert_eq!(
        run_count(&sandbox, &["history", root, "--hash", &"0".repeat(64)]),
        0
    );
}

#[test]
fn replay_uses_current_yaml_and_records_lineage() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let original = sandbox.run_json(&[
        "request",
        "run",
        ws,
        "items/0",
        "--environment",
        "local",
        "--tag",
        "smoke",
    ]);
    server.join().unwrap();
    let run_id = original["lattice"]["runId"].as_str().unwrap().to_owned();

    // Same URL live again: the current YAML resolves to the recorded hash.
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let replayed = sandbox.run_json(&["replay", &run_id, ws, "--tag", "again"]);
    assert_eq!(again.join().unwrap(), br#"{"source":"cli"}"#);
    assert_eq!(replayed["response"]["status"], 200);
    assert_eq!(replayed["lattice"]["recorded"], true);
    assert_eq!(replayed["lattice"]["replayedFrom"], run_id);
    assert_eq!(replayed["lattice"]["hashChanged"], false);
    assert_eq!(
        replayed["lattice"]["requestHash"],
        original["lattice"]["requestHash"]
    );
    assert_golden("replay.json", &normalize(replayed.clone()));
    let replay_id = replayed["lattice"]["runId"].as_str().unwrap().to_owned();

    let row = sandbox.run_json(&["history", ws, "--id", &replay_id]);
    assert_eq!(row["run"]["replayedFrom"], run_id);
    assert_eq!(row["run"]["tags"], json!(["smoke", "again"]));
    assert_eq!(row["run"]["varNames"], json!([]));
    assert_eq!(row["run"]["environment"], "local");
    let source = sandbox.run_json(&["history", ws, "--id", &run_id]);
    assert!(source["run"]["replayedFrom"].is_null());
    let lineage = sandbox.run_json(&[
        "history",
        ws,
        "--sql",
        &format!("SELECT count(*) FROM runs WHERE replayed_from = '{run_id}'"),
    ]);
    assert_eq!(lineage["rows"][0][0], 1);

    // Human mode names the lineage.
    let human = sandbox
        .facet()
        .args(["replay", &run_id, ws, "--no-record"])
        .output();
    // (no server: transport failure is fine, we only care that it parsed)
    assert!(human.is_ok());

    // A changed request: --var moves the URL, so the hash differs. --frozen
    // refuses before any network and writes no row.
    let (moved_url, moved) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let var = format!("serverUrl={moved_url}");
    let before = run_count(&sandbox, &["history", ws]);
    let (code, error) = sandbox.run_error_json(&["replay", &run_id, ws, "--var", &var, "--frozen"]);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["category"], "replay_changed");
    assert_eq!(error["error"]["details"]["replayedFrom"], run_id);
    assert_ne!(
        error["error"]["details"]["recordedHash"],
        error["error"]["details"]["currentHash"]
    );
    assert_golden("error_replay_changed.json", &normalize_error(error));
    assert_eq!(
        run_count(&sandbox, &["history", ws]),
        before,
        "no row on refuse"
    );

    // Without --frozen it sends, warns, and records varNames (names only).
    let output = sandbox
        .facet()
        .args(["replay", &run_id, ws, "--var", &var, "--json"])
        .output()
        .unwrap();
    moved.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: request changed since"),
        "{stderr}"
    );
    let changed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(changed["lattice"]["hashChanged"], true);
    assert_eq!(changed["lattice"]["replayedFrom"], run_id);
    let changed_id = changed["lattice"]["runId"].as_str().unwrap().to_owned();
    let changed_row = sandbox.run_json(&["history", ws, "--id", &changed_id]);
    assert_eq!(changed_row["run"]["varNames"], json!(["serverUrl"]));
    let dump = sandbox.run_json(&["history", ws, "--sql", "SELECT var_names FROM runs"]);
    let text = dump.to_string();
    assert!(
        !text.contains(&moved_url),
        "--var values must never persist"
    );

    // Replaying the changed run without its --var warns about the names.
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let output = sandbox
        .facet()
        .args(["replay", &changed_id, ws, "--json"])
        .output()
        .unwrap();
    again.join().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("was recorded with --var serverUrl"),
        "{stderr}"
    );
    assert!(stderr.contains("request changed since"), "{stderr}");

    // Unknown run, and a run that lives in another workspace (breadcrumb).
    let (code, error) = sandbox.run_error_json(&["replay", UNKNOWN_ULID, ws]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
    assert!(error["error"].get("details").is_none());
    let other_dir = sandbox.root().join("other");
    fs::create_dir_all(&other_dir).unwrap();
    let source = fs::read_to_string(fixture("phase5-http.yml")).unwrap();
    let other_ws = other_dir.join("workspace.yml");
    let (other_url, other_server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    fs::write(&other_ws, source.replace("__SERVER_URL__", &other_url)).unwrap();
    let other_run = sandbox.run_json(&[
        "request",
        "run",
        other_ws.to_str().unwrap(),
        "items/0",
        "--environment",
        "local",
    ]);
    other_server.join().unwrap();
    let other_id = other_run["lattice"]["runId"].as_str().unwrap();
    let (code, error) = sandbox.run_error_json(&["replay", other_id, ws]);
    assert_eq!(code, 4);
    assert_eq!(
        error["error"]["details"]["workspaceId"],
        other_run["lattice"]["workspaceId"]
    );
    assert_golden("error_replay_run_elsewhere.json", &normalize_error(error));

    // No store beside the collection at all.
    let empty = Sandbox::new();
    let empty_ws = empty.workspace("http://127.0.0.1:9");
    let (code, error) = empty.run_error_json(&["replay", &run_id, empty_ws.to_str().unwrap()]);
    assert_eq!(code, 9);
    assert_eq!(error["error"]["category"], "lattice_not_found");
    let (code, _) = sandbox.run_error_json(&["replay"]);
    assert_eq!(code, 2);
}

#[test]
fn diff_is_hash_first_and_exit_1_when_different() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let a = sandbox.run_json(&["request", "run", ws, "items/0", "--environment", "local"]);
    server.join().unwrap();
    let id_a = a["lattice"]["runId"].as_str().unwrap().to_owned();
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let b = sandbox.run_json(&["replay", &id_a, ws, "--tag", "retry"]);
    again.join().unwrap();
    let id_b = b["lattice"]["runId"].as_str().unwrap().to_owned();
    sandbox.set_duration_ms(&id_a, 10);
    sandbox.set_duration_ms(&id_b, 20);

    // Same request, same bytes: equal, even though duration and tags
    // (provenance) differ; both are still reported.
    let equal = sandbox.run_json(&["diff", &id_a, &id_b, ws]);
    assert_eq!(equal["equal"], true);
    assert_eq!(
        equal["changes"],
        json!([
            { "field": "durationMs", "a": 10, "b": 20 },
            { "field": "tags", "a": [], "b": ["retry"] },
        ])
    );
    assert_eq!(equal["request"]["hash"]["equal"], true);
    assert_eq!(equal["request"]["body"]["equal"], true);
    assert_eq!(equal["response"]["body"]["equal"], true);
    assert_eq!(equal["response"]["body"]["a"]["retention"], "inline");
    assert!(
        equal["response"]["body"]["a"]["hash"].is_string(),
        "inline bodies are hashed"
    );
    assert_golden("diff_equal.json", &normalize(equal));
    let status = sandbox
        .facet()
        .args(["diff", &id_a, &id_b, ws])
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&status.stdout).contains("response body: equal"));

    // A different response (and a different port, so url + requestHash move).
    let c = record_run_body(&sandbox, br#"{"users":[1]}"#);
    let id_c = c["lattice"]["runId"].as_str().unwrap().to_owned();
    sandbox.set_duration_ms(&id_c, 30);
    let output = sandbox
        .facet()
        .args(["diff", &id_a, &id_c, ws, "--bodies", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let changed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(changed["equal"], false);
    let fields: Vec<&str> = changed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["field"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"url") && fields.contains(&"requestHash"),
        "{fields:?}"
    );
    assert!(
        fields.contains(&"response.headers"),
        "content-length moved: {fields:?}"
    );
    assert!(!fields.contains(&"status"));
    assert_eq!(changed["request"]["hash"]["equal"], false);
    assert_eq!(
        changed["request"]["body"]["equal"], true,
        "same request body blob"
    );
    assert_eq!(changed["response"]["body"]["equal"], false);
    let text = changed["response"]["body"]["text"]
        .as_str()
        .expect("unified diff");
    assert!(text.starts_with("--- a\n+++ b\n@@ "), "{text}");
    assert!(
        text.contains("-{\"users\":[]}\n+{\"users\":[1]}\n"),
        "{text}"
    );
    assert_golden("diff_changed.json", &normalize(changed));

    // Without --bodies there is no text; the human view still says differs.
    let terse = sandbox
        .facet()
        .args(["diff", &id_a, &id_c, ws, "--json"])
        .output()
        .unwrap();
    assert_eq!(terse.status.code(), Some(1));
    let terse: Value = serde_json::from_slice(&terse.stdout).unwrap();
    assert!(terse["response"]["body"]["text"].is_null());
    let human = sandbox
        .facet()
        .args(["diff", &id_a, &id_c, ws])
        .output()
        .unwrap();
    assert_eq!(human.status.code(), Some(1));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("response body: differs"), "{text}");
    let quiet = sandbox
        .facet()
        .args(["diff", &id_a, &id_c, ws, "--quiet"])
        .output()
        .unwrap();
    assert_eq!(quiet.status.code(), Some(1));
    assert!(quiet.stdout.is_empty());

    // Missing runs are exit 4 with the ids listed; no store is exit 9.
    let (code, error) = sandbox.run_error_json(&["diff", &id_a, UNKNOWN_ULID, ws]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
    assert_eq!(error["error"]["details"]["missing"], json!([UNKNOWN_ULID]));
    assert_golden("error_diff_run_not_found.json", &normalize_error(error));
    let empty = Sandbox::new();
    let (code, error) =
        empty.run_error_json(&["diff", &id_a, &id_b, empty.root().to_str().unwrap()]);
    assert_eq!(code, 9);
    assert_eq!(error["error"]["category"], "lattice_not_found");
    let (code, _) = sandbox.run_error_json(&["diff", &id_a]);
    assert_eq!(code, 2);
}

const SECRET_VALUE: &str = "hunter2-never-in-history";

/// Every read surface an agent has must be free of the secret value.
fn assert_no_secret_anywhere(sandbox: &Sandbox, ws: &str) {
    let surfaces = [
        sandbox.run_json(&["history", ws, "--bodies"]).to_string(),
        sandbox
            .run_json(&["history", ws, "--sql", "SELECT * FROM runs"])
            .to_string(),
        sandbox.run_json(&["env", "list", ws]).to_string(),
        sandbox.run_json(&["session", "list"]).to_string(),
    ];
    for surface in surfaces {
        assert!(!surface.contains(SECRET_VALUE), "secret leaked: {surface}");
        assert!(!surface.contains("enc:v1:"), "secret ref leaked: {surface}");
        assert!(
            !surface.contains("test-master-key"),
            "key leaked: {surface}"
        );
    }
}

#[test]
fn secret_hydration_overlays_lattice_values_before_resolve() {
    let sandbox = Sandbox::new();
    let (url, first) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.secret_workspace(&url);
    let ws = workspace.to_str().unwrap();
    let run = ["request", "run", ws, "items/0", "--environment", "local"];

    // Declared secret, no value anywhere: core refuses, never sends empty.
    let (code, error) = sandbox.run_error_json(&run);
    assert_eq!(code, 5);
    assert_eq!(error["error"]["category"], "secret_variable_unavailable");

    // `facet env set --secret` stores through the secrets layer; list is
    // metadata only.
    let set = sandbox.run_json(&[
        "env",
        "set",
        ws,
        "--environment",
        "local",
        "--name",
        "token",
        "--value",
        SECRET_VALUE,
        "--secret",
    ]);
    assert_eq!(set["entry"]["secret"], true);
    assert_eq!(set["entry"]["name"], "token");
    assert!(!set.to_string().contains(SECRET_VALUE));
    assert_golden("env_set.json", &normalize(set));
    let list = sandbox.run_json(&["env", "list", ws]);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    assert_eq!(list["entries"][0]["secret"], true);
    assert!(list["entries"][0].get("value").is_none());
    assert_golden("env_list.json", &normalize(list));
    let human = sandbox.facet().args(["env", "list", ws]).output().unwrap();
    assert!(!String::from_utf8_lossy(&human.stdout).contains(SECRET_VALUE));

    // The run now hydrates: the server sees the value; JSON carries names.
    let hydrated = sandbox.run_json(&run);
    let sent = String::from_utf8(first.join().unwrap()).unwrap();
    assert_eq!(sent, format!(r#"{{"token":"{SECRET_VALUE}"}}"#));
    assert_eq!(hydrated["lattice"]["secrets"]["hydrated"], json!(["token"]));
    assert_eq!(hydrated["lattice"]["secrets"]["source"], "encrypted");
    // The live document shows what was sent (upstream shape); Lattice's
    // part of it carries names only.
    assert!(!hydrated["lattice"].to_string().contains(SECRET_VALUE));
    assert_golden("run_hydrated.json", &normalize(hydrated.clone()));
    let run_id = hydrated["lattice"]["runId"].as_str().unwrap().to_owned();
    let row = sandbox.run_json(&["history", ws, "--id", &run_id]);
    assert_eq!(row["run"]["varNames"], json!([]), "hydration is not --var");
    // Scrubbed at record time: URL query and the custom header.
    assert!(row["run"]["url"].as_str().unwrap().contains("t=<redacted>"));
    let headers = row["run"]["request"]["headers"].as_array().unwrap();
    let x_token = headers.iter().find(|h| h["name"] == "X-Token").unwrap();
    assert_eq!(x_token["value"], "<redacted>");
    assert_no_secret_anywhere(&sandbox, ws);
    let human = sandbox.facet().args(run).output().unwrap();
    assert_eq!(
        human.status.code(),
        Some(6),
        "server gone: transport failure"
    );

    // replay hydrates too (same URL live again, so the hash is unchanged).
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let replayed = sandbox.run_json(&["replay", &run_id, ws]);
    assert_eq!(
        String::from_utf8(again.join().unwrap()).unwrap(),
        format!(r#"{{"token":"{SECRET_VALUE}"}}"#)
    );
    assert_eq!(replayed["lattice"]["secrets"]["hydrated"], json!(["token"]));
    assert_eq!(replayed["lattice"]["replayedFrom"], run_id);
    assert_no_secret_anywhere(&sandbox, ws);

    // --var wins and is not reported as hydrated.
    let (url2, second) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.secret_workspace(&url2);
    let ws = workspace.to_str().unwrap();
    let overridden = sandbox.run_json(&[
        "request",
        "run",
        ws,
        "items/0",
        "--environment",
        "local",
        "--var",
        "token=from-var",
    ]);
    assert_eq!(second.join().unwrap(), br#"{"token":"from-var"}"#);
    assert_eq!(overridden["lattice"]["secrets"]["hydrated"], json!([]));
    assert!(overridden["lattice"]["secrets"]["source"].is_null());
    let overridden_id = overridden["lattice"]["runId"].as_str().unwrap();
    let row = sandbox.run_json(&["history", ws, "--id", overridden_id]);
    assert!(
        row["run"]["url"].as_str().unwrap().contains("t=<redacted>"),
        "--var secret scrubbed"
    );
    assert_eq!(row["run"]["varNames"], json!(["token"]));
    let dump = sandbox.run_json(&[
        "history",
        ws,
        "--sql",
        "SELECT url, req_headers, error FROM runs",
    ]);
    assert!(!dump.to_string().contains("from-var"), "{dump}");

    // Backend down (empty FACET_SECRET_KEY) with a declared secret referenced:
    // one clear exit 5, before any network.
    let output = sandbox
        .facet()
        .env("FACET_SECRET_KEY", "")
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["category"], "secret_backend_unavailable");
    assert_eq!(error["error"]["details"]["variable"], "token");
    assert_golden(
        "error_secret_backend_unavailable.json",
        &normalize_error(error),
    );
    let output = sandbox
        .facet()
        .env("FACET_SECRET_KEY", "")
        .args([
            "env",
            "set",
            ws,
            "--environment",
            "local",
            "--name",
            "other",
            "--value",
            "x",
            "--secret",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(
        sandbox.run_json(&["env", "list", ws])["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Wrong key: also backend unavailable, never an empty substitution.
    let output = sandbox
        .facet()
        .env("FACET_SECRET_KEY", "another-key")
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));

    // Delete, then the declared secret is unavailable again.
    let deleted = sandbox.run_json(&[
        "env",
        "delete",
        ws,
        "--environment",
        "local",
        "--name",
        "token",
    ]);
    assert_eq!(deleted["deleted"], true);
    let (code, error) = sandbox.run_error_json(&run);
    assert_eq!(code, 5);
    assert_eq!(error["error"]["category"], "secret_variable_unavailable");
    let (code, _) = sandbox.run_error_json(&["env", "set", ws, "--name", "x", "--value", "y"]);
    assert_eq!(code, 2);
}

#[test]
fn plain_lattice_values_hydrate_silently_and_yaml_only_runs_are_unchanged() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let plain = sandbox.run_json(&["request", "run", ws, "items/0", "--environment", "local"]);
    server.join().unwrap();
    assert_eq!(
        plain["lattice"]["secrets"],
        json!({ "hydrated": [], "source": null })
    );
    // No environment selected: upstream sends the literal template (and
    // fails to build the URL); there is no hydration and no `secrets` field.
    let (code, bare) = sandbox.run_error_json(&["request", "run", ws, "items/0"]);
    assert_eq!(code, 5);
    assert_eq!(bare["error"]["category"], "request_configuration");
    assert!(bare["error"]["details"]["lattice"].get("secrets").is_none());

    // A plain Lattice value for a referenced, non-secret variable overlays
    // the YAML value, and works even with no secrets backend at all.
    let (moved_url, moved) = serve_once(ECHO_BODY.to_vec(), "application/json");
    sandbox.run_json(&[
        "env",
        "set",
        ws,
        "--environment",
        "local",
        "--name",
        "serverUrl",
        "--value",
        &moved_url,
    ]);
    let output = sandbox
        .facet()
        .env("FACET_SECRET_KEY", "")
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--json",
        ])
        .output()
        .unwrap();
    moved.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["lattice"]["secrets"]["hydrated"],
        json!(["serverUrl"])
    );
    assert_eq!(value["lattice"]["secrets"]["source"], "plain");
    assert!(
        value["request"]["url"]
            .as_str()
            .unwrap()
            .starts_with(&moved_url)
    );

    // A Lattice value for a name the request does not reference is ignored.
    sandbox.run_json(&[
        "env",
        "delete",
        ws,
        "--environment",
        "local",
        "--name",
        "serverUrl",
    ]);
    sandbox.run_json(&[
        "env",
        "set",
        ws,
        "--environment",
        "local",
        "--name",
        "unused",
        "--value",
        "z",
    ]);
    let (url3, third) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url3);
    let ws = workspace.to_str().unwrap();
    let value = sandbox.run_json(&["request", "run", ws, "items/0", "--environment", "local"]);
    third.join().unwrap();
    assert_eq!(value["lattice"]["secrets"]["hydrated"], json!([]));
    let list = sandbox.run_json(&["env", "list", ws, "--environment", "local"]);
    assert_eq!(list["entries"][0]["name"], "unused");
    let none = sandbox.run_json(&["env", "list", ws, "--environment", "prod"]);
    assert_eq!(none["entries"], json!([]));
}

#[test]
fn doctor_reports_stores_secrets_backend_and_env_presence() {
    let sandbox = Sandbox::new();
    // Recording a run lays down `.facet`, so the workspace store is found.
    record_one_run(&sandbox, &[]);

    let doctor = sandbox.run_json(&["doctor"]);
    assert_eq!(doctor["doctor"]["machine"]["opened"], true);
    assert!(
        doctor["doctor"]["machine"]["schemaVersion"]
            .as_i64()
            .is_some()
    );
    assert_eq!(doctor["doctor"]["workspace"]["found"], true);
    assert!(doctor["doctor"]["workspace"]["id"].as_str().is_some());
    assert_eq!(doctor["doctor"]["workspace"]["schemaVersion"], 3);
    // Sandbox sets FACET_SECRET_KEY (encrypted backend); no other FACET_*.
    assert_eq!(doctor["doctor"]["secrets"]["backend"], "encrypted");
    assert_eq!(doctor["doctor"]["secrets"]["usable"], Value::Null);
    assert_eq!(doctor["doctor"]["secrets"]["probe"], false);
    assert_eq!(doctor["doctor"]["env"]["FACET_ACTOR"], false);
    assert_eq!(doctor["doctor"]["env"]["FACET_SESSION"], false);
    assert_eq!(doctor["doctor"]["env"]["FACET_DATA_DIR"], true);
    assert_eq!(doctor["doctor"]["env"]["FACET_CONFIG_DIR"], true);
    assert_eq!(doctor["doctor"]["env"]["FACET_NO_RECORD"], false);
    assert_eq!(doctor["doctor"]["env"]["FACET_SECRET_KEY"], true);
    assert_golden("doctor.json", &normalize(doctor));

    // No warnings in the sandbox: healthy, exit 0.
    let human = sandbox.facet().args(["doctor"]).output().unwrap();
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).contains("healthy"),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );
    // No value or ref is ever printed.
    let human_text = String::from_utf8_lossy(&human.stdout);
    assert!(!human_text.contains("test-master-key"));
}

#[test]
fn doctor_probe_round_trips_a_secret_and_marks_the_backend_usable() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &[]);

    let doctor = sandbox.run_json(&["doctor", "--probe"]);
    assert_eq!(doctor["doctor"]["secrets"]["backend"], "encrypted");
    assert_eq!(doctor["doctor"]["secrets"]["usable"], true);
    assert_eq!(doctor["doctor"]["secrets"]["probe"], true);
    // The probe must not leave the probe value anywhere in the output.
    assert!(
        !doctor
            .to_string()
            .contains("facet-doctor-probe-not-a-real-secret")
    );
    assert_golden("doctor_probe.json", &normalize(doctor));
}

#[test]
fn history_sql_cannot_reach_the_machine_store_or_any_secret_ref() {
    let sandbox = Sandbox::new();
    record_one_run(&sandbox, &[]);
    let root = sandbox.root().to_str().unwrap();
    // Put a real secret in the machine store so `environments.secret_ref` exists.
    sandbox.run_json(&[
        "env",
        "set",
        root,
        "--environment",
        "local",
        "--name",
        "token",
        "--value",
        "probe-secret-value",
        "--secret",
    ]);

    // The machine store lives under FACET_DATA_DIR (sandbox/machine/lattice.db).
    // `--sql` must not be able to ATTACH it.
    let machine_db = sandbox.root().join("machine").join("lattice.db");
    assert!(
        machine_db.is_file(),
        "machine store exists at {}",
        machine_db.display()
    );
    let attach = format!("ATTACH 'file:{}' AS machine", machine_db.to_string_lossy());
    let (attach_code, attach_error) = sandbox.run_error_json(&["history", root, "--sql", &attach]);
    assert_eq!(attach_code, 2);
    let category = attach_error["error"]["category"].as_str().unwrap();
    assert!(
        category == "invalid_sql" || category == "sql_read_only",
        "ATTACH of the machine store must be refused, got {category}"
    );

    // The workspace store has no `environments` table (it lives in the
    // machine store); a query for it must fail.
    let (env_code, env_error) = sandbox.run_error_json(&[
        "history",
        root,
        "--sql",
        "SELECT count(*) FROM environments",
    ]);
    assert_eq!(env_code, 2);
    assert_eq!(env_error["error"]["category"], "invalid_sql");

    // The workspace `runs` table has no `secret_ref` column (that lives in
    // the machine store `environments`), so it cannot be selected.
    let (col_code, col_error) =
        sandbox.run_error_json(&["history", root, "--sql", "SELECT secret_ref FROM runs"]);
    assert_eq!(col_code, 2);
    assert_eq!(col_error["error"]["category"], "invalid_sql");

    // And a full dump of the workspace runs never exposes a secret_ref.
    let dump = sandbox.run_json(&["history", root, "--sql", "SELECT * FROM runs"]);
    assert!(
        !dump["columns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str() == Some("secret_ref")),
        "secret_ref must not be a column in the workspace store"
    );
    assert!(!dump.to_string().contains("probe-secret-value"));
    assert!(!dump.to_string().contains("enc:v1:"));
    assert!(!dump.to_string().contains("kr:"));
}

#[test]
fn expect_is_exit_1_on_a_miss_with_the_full_document() {
    let sandbox = Sandbox::new();
    let hit = record_one_run(&sandbox, &["--expect", "2xx"]);
    assert_eq!(hit["response"]["status"], 200);
    assert!(hit.get("error").is_none());
    let hit = record_one_run(&sandbox, &["--expect", "201, 200"]);
    assert!(hit.get("error").is_none());

    // Miss: full success document plus `error`, exit 1, recorded and tagged.
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let output = sandbox
        .facet()
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--expect",
            "201,204",
            "--tag",
            "smoke",
            "--json",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "json mode keeps stderr clean");
    let miss: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(miss["response"]["status"], 200);
    assert_eq!(miss["response"]["body"]["content"], r#"{"users":[]}"#);
    assert_eq!(miss["lattice"]["recorded"], true);
    assert_eq!(miss["error"]["category"], "expect_failed");
    assert_eq!(miss["error"]["exitCode"], 1);
    assert_eq!(miss["error"]["details"]["expected"], json!([201, 204]));
    assert_eq!(miss["error"]["details"]["actual"], 200);
    assert_golden("run_expect_failed.json", &normalize(miss.clone()));
    let run_id = miss["lattice"]["runId"].as_str().unwrap();
    let failed = sandbox.run_json(&["history", ws, "--tag", "expect:fail"]);
    assert_eq!(failed["runs"].as_array().unwrap().len(), 1);
    assert_eq!(failed["runs"][0]["id"], run_id);
    assert_eq!(failed["runs"][0]["tags"], json!(["smoke", "expect:fail"]));

    // Human: response on stdout, error line on stderr; --quiet: nothing, 1.
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let human = sandbox
        .facet()
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--expect",
            "3xx",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(human.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&human.stdout).contains("200 OK"));
    assert!(String::from_utf8_lossy(&human.stderr).starts_with("error[expect_failed]:"));
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let quiet = sandbox
        .facet()
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--expect",
            "500",
            "--quiet",
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(quiet.status.code(), Some(1));
    assert!(quiet.stdout.is_empty());
    assert!(quiet.stderr.is_empty());

    // Transport failure keeps 6 even with --expect; bad specs are 2.
    let dead = sandbox.workspace("http://127.0.0.1:9");
    let (code, error) = sandbox.run_error_json(&[
        "request",
        "run",
        dead.to_str().unwrap(),
        "items/0",
        "--environment",
        "local",
        "--expect",
        "2xx",
    ]);
    assert_eq!(code, 6);
    assert_eq!(error["error"]["category"], "network_execution");
    assert!(error.get("response").is_none());
    for bad in ["20x", "6xx", "ok", "200,"] {
        let (code, error) = sandbox.run_error_json(&[
            "request",
            "run",
            dead.to_str().unwrap(),
            "items/0",
            "--expect",
            bad,
        ]);
        assert_eq!(code, 2, "{bad}");
        assert_eq!(error["error"]["category"], "invalid_arguments");
    }

    // replay --expect: same contract, lineage kept. The workspace file was
    // rewritten for the dead-port case; point it back at a live server.
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let output = sandbox
        .facet()
        .args(["replay", run_id, ws, "--expect", "404", "--json"])
        .output()
        .unwrap();
    again.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let replayed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(replayed["error"]["category"], "expect_failed");
    assert_eq!(replayed["lattice"]["replayedFrom"], run_id);
    let tagged = sandbox.run_json(&["history", ws, "--tag", "expect:fail"]);
    // Four misses so far (json, human, quiet, replay); the replay carries the
    // source's tags and is tagged once, not twice.
    assert_eq!(tagged["runs"].as_array().unwrap().len(), 4);
    assert_eq!(tagged["runs"][0]["tags"], json!(["smoke", "expect:fail"]));
}

#[test]
fn dry_run_previews_without_sending_or_recording() {
    let sandbox = Sandbox::new();
    // A dead port proves nothing is sent.
    let workspace = sandbox.workspace("http://127.0.0.1:9");
    let ws = workspace.to_str().unwrap();
    let preview = sandbox.run_json(&[
        "request",
        "run",
        ws,
        "items/0",
        "--environment",
        "local",
        "--dry-run",
    ]);
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["request"]["method"], "POST");
    assert_eq!(preview["request"]["url"], "http://127.0.0.1:9/echo");
    assert_eq!(
        preview["request"]["query"],
        json!([{ "disabled": false, "name": "mode", "value": "cli" }])
    );
    let headers = preview["request"]["headers"].as_array().unwrap();
    assert!(
        headers
            .iter()
            .any(|h| h["name"] == "X-Probe" && h["value"] == "phase-five")
    );
    assert!(
        headers
            .iter()
            .all(|h| h["name"] != "Authorization" || h["value"] == "<redacted>"),
        "{headers:?}"
    );
    assert_eq!(preview["request"]["body"]["content"], r#"{"source":"cli"}"#);
    assert_eq!(preview["request"]["body"]["sizeBytes"], 16);
    assert_eq!(preview["lattice"]["recorded"], false);
    assert_eq!(preview["lattice"]["reason"], "dry_run");
    assert_eq!(
        preview["lattice"]["requestHash"].as_str().unwrap().len(),
        64
    );
    assert_eq!(
        preview["lattice"]["secrets"],
        json!({ "hydrated": [], "source": null })
    );
    assert!(preview.get("response").is_none());
    assert_golden("run_dry_run.json", &normalize(preview.clone()));
    assert!(!sandbox.root().join(".facet").exists(), "nothing recorded");

    // The hash is the one a real run stores.
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let again = sandbox.run_json(&[
        "request",
        "run",
        ws,
        "items/0",
        "--environment",
        "local",
        "--dry-run",
    ]);
    let real = sandbox.run_json(&["request", "run", ws, "items/0", "--environment", "local"]);
    server.join().unwrap();
    assert_eq!(
        again["lattice"]["requestHash"],
        real["lattice"]["requestHash"]
    );
    assert_eq!(run_count(&sandbox, &["history", ws]), 1);

    // Hydrated secrets are redacted in the preview.
    let secret_ws = sandbox.secret_workspace("http://127.0.0.1:9");
    let sws = secret_ws.to_str().unwrap();
    sandbox.run_json(&[
        "env",
        "set",
        sws,
        "--environment",
        "local",
        "--name",
        "token",
        "--value",
        SECRET_VALUE,
        "--secret",
    ]);
    let hydrated = sandbox.run_json(&[
        "request",
        "run",
        sws,
        "items/0",
        "--environment",
        "local",
        "--dry-run",
    ]);
    assert_eq!(hydrated["lattice"]["secrets"]["hydrated"], json!(["token"]));
    assert!(!hydrated.to_string().contains(SECRET_VALUE), "{hydrated}");
    assert!(
        hydrated["request"]["url"]
            .as_str()
            .unwrap()
            .contains("t=<redacted>")
    );
    assert_eq!(
        hydrated["request"]["body"]["content"],
        r#"{"token":"<redacted>"}"#
    );

    // Human mode and the two exclusions.
    let human = sandbox
        .facet()
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.starts_with("POST http://"), "{text}");
    assert!(text.contains("Lattice: not recorded (dry_run)"), "{text}");
    for extra in [["--expect", "2xx"], ["--output", "x"]] {
        let mut arguments = vec!["request", "run", ws, "items/0", "--dry-run"];
        arguments.extend_from_slice(&extra);
        let (code, _) = sandbox.run_error_json(&arguments);
        assert_eq!(code, 2);
    }
}

#[test]
fn git_head_auto_tag_rides_on_record_and_not_on_replay() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let sha = sandbox.git_init();
    assert_eq!(sha.len(), 40);
    let clean_tag = format!("git:{sha}");

    let clean = sandbox.run_json(&[
        "request",
        "run",
        ws,
        "items/0",
        "--environment",
        "local",
        "--tag",
        "smoke",
    ]);
    server.join().unwrap();
    let clean_id = clean["lattice"]["runId"].as_str().unwrap().to_owned();
    let row = sandbox.run_json(&["history", ws, "--id", &clean_id]);
    assert_eq!(row["run"]["tags"], json!(["smoke", clean_tag]));

    // A tracked change makes the tree dirty; untracked `.facet/` never does.
    // Appending a comment keeps the YAML (and the URL) intact.
    let source = fs::read_to_string(&workspace).unwrap();
    fs::write(
        &workspace,
        format!(
            "{source}# dirty
"
        ),
    )
    .unwrap();
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let dirty = sandbox.run_json(&["request", "run", ws, "items/0", "--environment", "local"]);
    again.join().unwrap();
    let dirty_id = dirty["lattice"]["runId"].as_str().unwrap().to_owned();
    let row = sandbox.run_json(&["history", ws, "--id", &dirty_id]);
    assert_eq!(row["run"]["tags"], json!([format!("git:{sha}-dirty")]));

    // `history --tag git:…` is the query.
    assert_eq!(
        run_count(&sandbox, &["history", ws, "--tag", &clean_tag]),
        1
    );
    assert_eq!(
        run_count(
            &sandbox,
            &["history", ws, "--tag", &format!("git:{sha}-dirty")]
        ),
        1
    );

    // A replay keeps the source's user tags but recomputes auto-tags: the
    // clean run's `git:<sha>` must not be copied onto a dirty-tree replay.
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let replayed = sandbox.run_json(&["replay", &clean_id, ws, "--tag", "retry"]);
    again.join().unwrap();
    let replay_id = replayed["lattice"]["runId"].as_str().unwrap();
    let row = sandbox.run_json(&["history", ws, "--id", replay_id]);
    assert_eq!(
        row["run"]["tags"],
        json!(["smoke", "retry", format!("git:{sha}-dirty")])
    );

    // An expect:fail source does not taint a passing replay.
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let output = sandbox
        .facet()
        .args([
            "request",
            "run",
            ws,
            "items/0",
            "--environment",
            "local",
            "--expect",
            "500",
            "--json",
        ])
        .output()
        .unwrap();
    again.join().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let failed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let failed_id = failed["lattice"]["runId"].as_str().unwrap();
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let passing = sandbox.run_json(&["replay", failed_id, ws, "--expect", "200"]);
    again.join().unwrap();
    let row = sandbox.run_json(&[
        "history",
        ws,
        "--id",
        passing["lattice"]["runId"].as_str().unwrap(),
    ]);
    assert_eq!(row["run"]["tags"], json!([format!("git:{sha}-dirty")]));

    // Outside a repository nothing is added (every other test relies on it).
    let plain = Sandbox::new();
    let value = record_one_run(&plain, &[]);
    let row = plain.run_json(&[
        "history",
        plain.root().to_str().unwrap(),
        "--id",
        value["lattice"]["runId"].as_str().unwrap(),
    ]);
    assert_eq!(row["run"]["tags"], json!([]));
}

#[test]
fn last_prints_the_newest_matching_run_or_exit_4() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().to_str().unwrap().to_owned();
    let (code, error) = sandbox.run_error_json(&["last", &root]);
    assert_eq!(code, 4, "no store: nothing to substitute");
    assert_eq!(error["error"]["category"], "run_not_found");

    let first = record_one_run(&sandbox, &["--tag", "a"]);
    let second = record_one_run(&sandbox, &["--tag", "b"]);
    let second_id = second["lattice"]["runId"].as_str().unwrap();
    let first_id = first["lattice"]["runId"].as_str().unwrap();

    let output = sandbox.facet().args(["last", &root]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        second_id,
        "bare ULID"
    );
    let last = sandbox.run_json(&["last", &root]);
    assert_eq!(last["run"]["id"], second_id);
    assert!(last.get("runs").is_none(), "history --id shape");
    assert_golden("last.json", &normalize(last));
    let by_tag = sandbox.run_json(&["last", &root, "--tag", "a"]);
    assert_eq!(by_tag["run"]["id"], first_id);
    let by_status = sandbox.run_json(&["last", &root, "--status", "200", "--request", "items/0"]);
    assert_eq!(by_status["run"]["id"], second_id);
    let (code, error) = sandbox.run_error_json(&["last", &root, "--status", "500"]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
    assert_golden("error_last_none.json", &normalize_error(error));
    let (code, _) = sandbox.run_error_json(&["last", &root, "--session", "current"]);
    assert_eq!(code, 5);
    let (code, _) = sandbox.run_error_json(&["last", &root, "--limit", "2"]);
    assert_eq!(code, 2, "last has no --limit");
    let with_bodies = sandbox.run_json(&["last", &root, "--bodies"]);
    assert_eq!(
        with_bodies["run"]["response"]["body"]["content"],
        r#"{"users":[]}"#
    );
}

#[test]
fn pins_name_runs_in_the_machine_store() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().to_str().unwrap().to_owned();
    let run = record_one_run(&sandbox, &[]);
    let run_id = run["lattice"]["runId"].as_str().unwrap().to_owned();

    let pinned = sandbox.run_json(&["pin", &run_id, "--as", "auth-ok", &root]);
    assert_eq!(pinned["pin"]["name"], "auth-ok");
    assert_eq!(pinned["pin"]["runId"], run_id);
    assert_eq!(pinned["pin"]["workspaceId"], run["lattice"]["workspaceId"]);
    assert_eq!(pinned["pin"]["dangling"], false);

    // `pin get` prints the bare id so replay can substitute it.
    let output = sandbox
        .facet()
        .args(["pin", "get", "auth-ok", &root])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), run_id);
    let got = sandbox.run_json(&["pin", "get", "auth-ok", &root]);
    assert_eq!(got["pin"]["dangling"], false);
    assert_golden("pin_get.json", &normalize(got));
    let listed = sandbox.run_json(&["pin", "list", &root]);
    assert_eq!(listed["pins"].as_array().unwrap().len(), 1);
    assert_golden("pin_list.json", &normalize(listed));
    // Without a workspace to check against, dangling is unknown.
    let elsewhere = Sandbox::new();
    let output = elsewhere
        .facet()
        .env("FACET_DATA_DIR", sandbox.root().join("machine"))
        .args([
            "pin",
            "get",
            "auth-ok",
            elsewhere.root().to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    let unknown: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(unknown["pin"]["dangling"].is_null());

    // Re-pin overwrites; bad names and unknown runs are refused.
    let again = record_one_run(&sandbox, &[]);
    let again_id = again["lattice"]["runId"].as_str().unwrap();
    let repinned = sandbox.run_json(&["pin", again_id, "--as", "auth-ok", &root]);
    assert_eq!(repinned["pin"]["runId"], again_id);
    let (code, _) = sandbox.run_error_json(&["pin", again_id, "--as", "bad name", &root]);
    assert_eq!(code, 2);
    let (code, error) = sandbox.run_error_json(&["pin", UNKNOWN_ULID, "--as", "x", &root]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "run_not_found");
    let (code, _) = sandbox.run_error_json(&["pin", "not-a-ulid", "--as", "x", &root]);
    assert_eq!(code, 2);
    let (code, error) = sandbox.run_error_json(&["pin", "get", "nope", &root]);
    assert_eq!(code, 4);
    assert_eq!(error["error"]["category"], "pin_not_found");
    assert_golden("error_pin_not_found.json", &normalize_error(error));

    // A pin survives the run: gc the run away and the pin dangles.
    sandbox.backdate_run(again_id, lattice::now_ms() - 3 * 86_400_000);
    sandbox.run_json(&["gc", &root, "--history-retention", "1d", "--yes"]);
    let dangling = sandbox.run_json(&["pin", "get", "auth-ok", &root]);
    assert_eq!(dangling["pin"]["dangling"], true);
    let deleted = sandbox.run_json(&["pin", "delete", "auth-ok"]);
    assert_eq!(deleted["deleted"], true);
    let deleted = sandbox.run_json(&["pin", "delete", "auth-ok"]);
    assert_eq!(deleted["deleted"], false);
    assert_eq!(sandbox.run_json(&["pin", "list", &root])["pins"], json!([]));
}

fn theme_fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/themes")
        .join(name)
        .display()
        .to_string()
}

#[test]
fn theme_check_validates_a_theme_file() {
    let sandbox = Sandbox::new();
    let checked = sandbox.run_json(&["theme", "check", &theme_fixture("midnight-honey.toml")]);
    assert_eq!(checked["theme"]["valid"], true);
    assert_eq!(checked["theme"]["name"], "midnight-honey");
    assert_eq!(checked["theme"]["extends"], "graphite");
    assert_eq!(checked["theme"]["overrides"], 4);
    assert_golden("theme_check_valid.json", &normalize(checked));
}

#[test]
fn theme_check_rejects_an_invalid_file_with_file_and_field() {
    let sandbox = Sandbox::new();
    let (code, error) =
        sandbox.run_error_json(&["theme", "check", &theme_fixture("broken-accent.toml")]);
    assert_eq!(code, 1, "a rejected theme file is the assertion family");
    assert_eq!(error["error"]["category"], "theme_invalid");
    assert_eq!(error["error"]["details"]["field"], "colors.accent");
    assert_golden("error_theme_invalid.json", &normalize_error(error));

    // A missing file and a missing argument are their own categories.
    let (code, error) = sandbox.run_error_json(&["theme", "check", "nope.toml"]);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["category"], "theme_invalid");
    let (code, _) = sandbox.run_error_json(&["theme", "check"]);
    assert_eq!(code, 2);
}

#[test]
fn theme_list_shows_built_ins_and_discovers_files() {
    let sandbox = Sandbox::new();
    // Sandbox config dir has no themes yet: built-ins only.
    let list = sandbox.run_json(&["theme", "list"]);
    let names: Vec<&str> = list["themes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|theme| theme["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["graphite", "porcelain"]);
    assert_golden("theme_list.json", &normalize(list));

    // A file in <config>/themes is discovered; a broken sibling lists
    // with its error instead of failing the command.
    let themes_dir = sandbox.root().join("config/themes");
    fs::create_dir_all(&themes_dir).unwrap();
    fs::write(
        themes_dir.join("midnight-honey.toml"),
        "version = 1\nextends = \"graphite\"\n[colors]\naccent = \"#e7821b\"\n",
    )
    .unwrap();
    fs::write(themes_dir.join("oops.toml"), "version = 9\n").unwrap();
    let list = sandbox.run_json(&["theme", "list"]);
    let themes = list["themes"].as_array().unwrap();
    assert_eq!(themes.len(), 4);
    assert_eq!(themes[2]["name"], "midnight-honey");
    assert_eq!(themes[2]["source"], "file");
    assert_eq!(themes[2]["valid"], true);
    assert_eq!(themes[3]["name"], "oops");
    assert_eq!(themes[3]["valid"], false);
    assert!(
        themes[3]["error"]
            .as_str()
            .unwrap()
            .contains("unsupported theme version 9")
    );
}

#[test]
fn cluster_configuration_failure_is_recorded_without_sending_or_exposing_credentials() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.workspace("https://127.0.0.1:1");
    let config = sandbox.root().join("invalid-kubeconfig");
    fs::write(&config, "client-key: [credential-canary-do-not-print").unwrap();
    let output = sandbox
        .facet()
        .env("FACET_KUBECONFIG", &config)
        .args([
            "request",
            "run",
            workspace.to_str().unwrap(),
            "items/0",
            "--environment",
            "local",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("credential-canary-do-not-print"));
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["error"]["details"]["lattice"]["recorded"], true);
    let history = sandbox.run_json(&["history"]);
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert!(history["runs"][0]["status"].is_null());
    assert!(
        !history
            .to_string()
            .contains("credential-canary-do-not-print")
    );
}

#[cfg(unix)]
#[test]
fn github_gh_auth_and_api_never_persist_token() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = Sandbox::new();
    let bin = sandbox.root().join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nif [ \"$1\" = auth ]; then printf gh-secret-token; else printf '{\"source\":\"gh\"}'; fi\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let source = r#"opencollection: 1.0.0
info: { name: GitHub auth }
bundled: true
config:
  environments:
    - name: auth
      variables:
        - name: githubToken
          value: ""
items:
  - info: { name: Get repo, type: http }
    http:
      method: GET
      url: https://api.github.com.invalid/repos/example/project
      headers:
        - name: Authorization
          value: Bearer {{githubToken}}
"#;
    let workspace = sandbox.root().join("github.yml");
    fs::write(&workspace, source).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let output = sandbox
        .facet()
        .env("PATH", path)
        .args([
            "request",
            "run",
            workspace.to_str().unwrap(),
            "items/0",
            "--environment",
            "auth",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"source\":\"gh\""));
    assert!(!stdout.contains("gh-secret-token"));
    assert!(!source.contains("gh-secret-token"));
    let rewritten = fs::read_to_string(&workspace).unwrap();
    assert!(!rewritten.contains("gh-secret-token"));
    let db = rusqlite::Connection::open(sandbox.root().join(".facet/lattice.db")).unwrap();
    let headers: String = db
        .query_row("SELECT req_headers FROM runs LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(!headers.contains("gh-secret-token"));
}
