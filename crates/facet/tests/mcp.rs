//! `facet mcp` through the binary: JSON-RPC over stdio, every tool result
//! the same `schemaVersion: 1` document the CLI prints, errors the same
//! envelope, and the secret-argument refusal.

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Stdio},
};

use common::*;

const ECHO_BODY: &[u8] = br#"{"users":[]}"#;

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(sandbox: &Sandbox, env: &[(&str, &str)]) -> Self {
        let mut command = sandbox.facet();
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("facet mcp should start");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send(&mut self, message: &Value) {
        let mut text = message.to_string();
        text.push('\n');
        self.stdin.write_all(text.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).unwrap();
        assert!(read > 0, "server closed stdout");
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let response = self.receive();
        assert_eq!(response["id"], id, "{response}");
        response
    }

    /// Calls a tool and returns `(structuredContent, isError)`.
    fn call(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let result = &response["result"];
        assert!(result.is_object(), "expected a result: {response}");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            text, result["structuredContent"],
            "text and structured content agree"
        );
        (
            result["structuredContent"].clone(),
            result["isError"] == true,
        )
    }

    fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let (document, is_error) = self.call(name, arguments);
        assert!(!is_error, "{name} failed: {document}");
        assert_eq!(document["schemaVersion"], 1);
        document
    }

    fn err(&mut self, name: &str, arguments: Value) -> Value {
        let (document, is_error) = self.call(name, arguments);
        assert!(is_error, "{name} should fail: {document}");
        assert_eq!(document["schemaVersion"], 1);
        document
    }

    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "clean EOF exits 0");
    }
}

#[test]
fn mcp_handshake_and_tool_list_are_golden() {
    let sandbox = Sandbox::new();
    let mut mcp = Mcp::start(&sandbox, &[]);
    let init = mcp.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18",
        "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "facet");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    mcp.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let pong = mcp.request("ping", json!({}));
    assert_eq!(pong["result"], json!({}));

    let list = mcp.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 14);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "session_start",
            "session_end",
            "request_list",
            "request_get",
            "request_run",
            "history_list",
            "history_get",
            "blob_get",
            "run_diff",
            "run_replay",
            "ncl_check",
            "ncl_export",
            "ncl_apply",
            "sql_query",
        ]
    );
    assert_golden("mcp_tools.json", &list["result"]);

    let missing = mcp.request("resources/list", json!({}));
    assert_eq!(missing["error"]["code"], -32601);
    let unknown = mcp.request("tools/call", json!({ "name": "gc_apply", "arguments": {} }));
    assert_eq!(unknown["error"]["code"], -32602);
    mcp.finish();
}

#[test]
fn mcp_tools_return_the_cli_documents() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(ECHO_BODY.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let ws = workspace.to_str().unwrap();
    let mut mcp = Mcp::start(&sandbox, &[("FACET_ACTOR", "mcp-test")]);

    let session = mcp.ok("session_start", json!({ "meta": { "note": "mcp" } }));
    assert_eq!(session["session"]["actor"], "mcp-test");
    assert_eq!(session["session"]["meta"]["note"], "mcp");
    assert!(lattice::is_ulid(session["session"]["id"].as_str().unwrap()));

    let list = mcp.ok("request_list", json!({ "path": ws }));
    assert_eq!(list["requests"][0]["selector"], "items/0");
    let get = mcp.ok(
        "request_get",
        json!({ "path": ws, "selector": "items/0", "environment": "local" }),
    );
    assert_eq!(get["method"], "POST");
    assert!(get["url"].as_str().unwrap().starts_with(&url), "{get}");
    let missing = mcp.err("request_get", json!({ "path": ws, "selector": "items/99" }));
    assert_eq!(missing["error"]["category"], "request_not_found");
    assert_eq!(missing["error"]["exitCode"], 4);

    let run = mcp.ok(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local",
            "tag": ["mcp"], "expect": [200, "3xx"],
        }),
    );
    assert_eq!(server.join().unwrap(), br#"{"source":"cli"}"#);
    assert_eq!(run["response"]["status"], 200);
    assert_eq!(run["lattice"]["recorded"], true);
    assert!(run.get("error").is_none());
    let run_id = run["lattice"]["runId"].as_str().unwrap().to_owned();

    let history = mcp.ok("history_list", json!({ "path": ws, "tag": ["mcp"] }));
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["runs"][0]["id"], run_id);
    let one = mcp.ok(
        "history_get",
        json!({ "id": run_id, "path": ws, "bodies": true }),
    );
    assert_eq!(one["run"]["response"]["body"]["content"], r#"{"users":[]}"#);
    let request_hash = one["run"]["request"]["body"]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let blob = mcp.ok("blob_get", json!({ "hash": request_hash, "path": ws }));
    assert_eq!(blob["blob"]["body"]["content"], r#"{"source":"cli"}"#);
    let count = mcp.ok(
        "sql_query",
        json!({ "sql": "SELECT count(*) FROM runs", "path": ws }),
    );
    assert_eq!(count["rows"][0][0], 1);
    let write = mcp.err(
        "sql_query",
        json!({ "sql": "DELETE FROM runs", "path": ws }),
    );
    assert_eq!(write["error"]["category"], "sql_read_only");
    let nope = mcp.err(
        "history_get",
        json!({ "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "path": ws }),
    );
    assert_eq!(nope["error"]["category"], "run_not_found");
    assert_eq!(nope["error"]["exitCode"], 4);

    // Replay and diff through the same functions.
    let again = serve_again(&url, ECHO_BODY.to_vec());
    let replayed = mcp.ok(
        "run_replay",
        json!({ "id": run_id, "path": ws, "expect": "2xx" }),
    );
    again.join().unwrap();
    assert_eq!(replayed["lattice"]["replayedFrom"], run_id);
    let replay_id = replayed["lattice"]["runId"].as_str().unwrap().to_owned();
    let diff = mcp.ok(
        "run_diff",
        json!({ "a": run_id, "b": replay_id, "path": ws }),
    );
    assert_eq!(diff["equal"], true);

    // An --expect miss is a success document that carries `error`, isError.
    let miss_server = serve_again(&url, ECHO_BODY.to_vec());
    let miss = mcp.err(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local", "expect": "404",
        }),
    );
    miss_server.join().unwrap();
    assert_eq!(miss["response"]["status"], 200);
    assert_eq!(miss["error"]["category"], "expect_failed");
    assert_eq!(miss["error"]["exitCode"], 1);
    assert!(miss["lattice"]["runId"].is_string());

    // Dry run sends nothing.
    let preview = mcp.ok(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local", "dryRun": true,
        }),
    );
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["lattice"]["reason"], "dry_run");

    // Transport failure keeps the upstream envelope and exit 6.
    let dead = mcp.err(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local",
            "var": { "serverUrl": "http://127.0.0.1:9" },
        }),
    );
    assert_eq!(dead["error"]["category"], "network_execution");
    assert_eq!(dead["error"]["exitCode"], 6);

    // Session end via argument, then with nothing set.
    let ended = mcp.ok("session_end", json!({ "id": session["session"]["id"] }));
    assert_eq!(ended["alreadyEnded"], false);
    let unset = mcp.err("session_end", json!({}));
    assert_eq!(unset["error"]["category"], "session_not_set");
    assert_eq!(unset["error"]["exitCode"], 5);
    mcp.finish();
}

#[test]
fn mcp_refuses_secret_values_as_arguments() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.secret_workspace("http://127.0.0.1:9");
    let ws = workspace.to_str().unwrap();
    sandbox.env_set_secret(ws, "local", "token", "stored-secret");
    let mut mcp = Mcp::start(&sandbox, &[]);
    let refused = mcp.err(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local",
            "var": { "token": "leaked-in-transcript" },
        }),
    );
    assert_eq!(refused["error"]["category"], "invalid_arguments");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("facet env set")
    );
    // The refusal happens before any run: nothing recorded.
    let history = mcp.ok("history_list", json!({ "path": ws }));
    assert_eq!(history["runs"], json!([]));
    // A name with no stored value is an ordinary override.
    let other = mcp.err(
        "request_run",
        json!({
            "path": ws, "selector": "items/0", "environment": "local",
            "var": { "serverUrl": "http://127.0.0.1:9" },
        }),
    );
    assert_eq!(other["error"]["category"], "network_execution");
    assert_eq!(
        other["error"]["details"]["lattice"]["secrets"]["hydrated"],
        json!(["token"])
    );
    mcp.finish();
}

#[test]
fn mcp_ncl_tools_match_cli_documents() {
    let sandbox = Sandbox::new();
    let root = sandbox.root();
    std::fs::write(
        root.join("lib.ncl"),
        r#"{
  pod = fun args => {
    apiVersion = "v1",
    kind = "Pod",
    metadata.name = args.name,
    metadata.annotations.replicas = std.to_string args.n,
  },
}"#,
    )
    .unwrap();
    let world = root.join("world.ncl");
    std::fs::write(
        &world,
        r#"let lib = import "lib.ncl" in
{
  replicas = 1,
  cluster = [ lib.pod { name = "web", n = replicas } ],
  intent = [
    {
      name = "test-docs-eod",
      importance = 0.5,
      spec = { kind = "docs_eod", date = "2026-09-12", required_briefs = [] },
    },
  ],
  calls = {
    opencollection = "1.0.0",
    environments = [ { name = "dev", variables = [ { name = "TOKEN", secret_ref = "kr:me" } ] } ],
    plaintext_token | not_exported = "hunter2",
  },
}"#,
    )
    .unwrap();
    let path = world.to_str().unwrap();

    let cli_check = sandbox.run_json(&["ncl", "check", path]);
    let cli_export = sandbox.run_json(&["ncl", "export", path]);
    let cli_apply = sandbox.run_json(&["ncl", "apply", path]);

    let mut mcp = Mcp::start(&sandbox, &[]);
    let mcp_check = mcp.ok("ncl_check", json!({ "path": path }));
    let mcp_export = mcp.ok("ncl_export", json!({ "path": path }));
    let mcp_apply = mcp.ok("ncl_apply", json!({ "path": path }));
    mcp.finish();

    assert_eq!(mcp_check, cli_check);
    assert_eq!(mcp_export, cli_export);
    assert_eq!(mcp_apply, cli_apply);
    for key in [
        "cluster",
        "intent",
        "calls",
        "moduleHash",
        "exportHash",
        "contractSet",
    ] {
        assert!(mcp_export.get(key).is_some(), "missing {key}");
    }
    assert!(!mcp_export.to_string().contains("hunter2"));
}
