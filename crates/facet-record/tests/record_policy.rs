//! P1a G3 / H4: the Facet-owned `facet:` key.
//!
//! - `facet: { record: never }` on the collection, or `facet: { record:
//!   false }` on an item, writes no run row and no blob, whatever the
//!   flags, `FACET_NO_RECORD` or MCP `noRecord`. The request is still sent.
//! - Inside `facet:`, an unknown key, a wrong type or an unknown value is
//!   `facet_key_invalid`: a non-zero exit, nothing sent, and no value
//!   echoed.
//! - Outside `facet:`, unknown YAML is still accepted, and a command that
//!   rewrites the YAML keeps the `facet:` key.
//!
//! Each case runs through the shipped `facet` binary for `request run`,
//! `replay` and MCP, in the bundled and the unbundled layout. Every value
//! below is a canary, and no assertion message prints one.

mod common;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

/// A value inside a bad `facet:` key. It must never be echoed.
const VALUE_CANARY: &str = "g3-value-canary";
const SECRET_KEY: &str = "g3-master-key-canary";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
      Content-Length: 17\r\nConnection: close\r\n\r\n{\"g3\":\"response\"}";

/// A loopback host that answers every request `200` with a small body and
/// reports each request line.
struct Host {
    url: String,
    requests: Receiver<String>,
}

impl Host {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut head = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => head.extend_from_slice(&buffer[..read]),
                    }
                }
                let line = String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if sender.send(line).is_err() {
                    return;
                }
                let _ = stream.write_all(OK);
            }
        });
        Self { url, requests }
    }

    fn hit(&self, what: &str) {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: expected a request at this host"));
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: more than one request at this host"
        );
    }

    fn untouched(&self, what: &str) {
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: a request reached this host"
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    /// One `workspace.yml` with `bundled: true`.
    Bundled,
    /// A directory: `opencollection.yml` plus one file per request.
    Unbundled,
}

/// Where a `facet:` value goes.
#[derive(Clone, Copy, Debug)]
enum Level {
    Collection,
    /// On the first request (`One`) only.
    Item,
}

/// What the collection carries.
#[derive(Clone, Copy, Debug, Default)]
struct Shape<'a> {
    /// The YAML value of the collection's `facet:` key.
    collection: Option<&'a str>,
    /// The YAML value of `One`'s `facet:` key.
    item: Option<&'a str>,
    /// Unknown keys outside `facet:`, at both levels.
    extras: bool,
}

impl<'a> Shape<'a> {
    fn at(level: Level, value: &'a str) -> Self {
        match level {
            Level::Collection => Self {
                collection: Some(value),
                ..Self::default()
            },
            Level::Item => Self {
                item: Some(value),
                ..Self::default()
            },
        }
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A collection, its `.facet/` store, and a private machine store and
/// config, all under one temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    layout: Layout,
    url: String,
    /// The `<path>` argument: the file, or the collection directory.
    workspace: PathBuf,
}

impl Sandbox {
    fn new(layout: Layout, url: &str, shape: Shape<'_>) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let workspace = match layout {
            Layout::Bundled => dir.path().join("workspace.yml"),
            Layout::Unbundled => dir.path().join("collection"),
        };
        let sandbox = Self {
            dir,
            layout,
            url: url.to_owned(),
            workspace,
        };
        sandbox.write(shape);
        sandbox
    }

    /// (Re)writes the collection with `shape`, keeping the store.
    fn write(&self, shape: Shape<'_>) {
        let mut header = "opencollection: 1.0.0\ninfo:\n  name: Record policy fixture\n".to_owned();
        header.push_str(match self.layout {
            Layout::Bundled => "bundled: true\n",
            Layout::Unbundled => "bundled: false\n",
        });
        if let Some(value) = shape.collection {
            header.push_str(&format!("facet: {value}\n"));
        }
        if shape.extras {
            header.push_str("x-vendor:\n  anything: g3-plain\n");
        }
        let mut one = format!(
            "info:\n  name: One\n  type: http\n  seq: 1\nhttp:\n  method: POST\n  \
             url: \"{}/one\"\n  body:\n    type: json\n    data: '{{\"g3\":\"request\"}}'\n",
            self.url
        );
        if let Some(value) = shape.item {
            one.push_str(&format!("facet: {value}\n"));
        }
        if shape.extras {
            one.push_str("x-note: keep me\n");
        }
        let two = format!(
            "info:\n  name: Two\n  type: http\n  seq: 2\nhttp:\n  method: GET\n  url: \"{}/two\"\n",
            self.url
        );
        match self.layout {
            Layout::Bundled => {
                let items = [one, two]
                    .iter()
                    .map(|item| {
                        let mut lines = item.lines();
                        let first = lines.next().unwrap_or_default();
                        let rest: String = lines.map(|line| format!("    {line}\n")).collect();
                        format!("  - {first}\n{rest}")
                    })
                    .collect::<String>();
                std::fs::write(&self.workspace, format!("{header}items:\n{items}")).unwrap();
            }
            Layout::Unbundled => {
                std::fs::create_dir_all(&self.workspace).unwrap();
                std::fs::write(self.workspace.join("opencollection.yml"), header).unwrap();
                std::fs::write(self.workspace.join("one.yml"), one).unwrap();
                std::fs::write(self.workspace.join("two.yml"), two).unwrap();
            }
        }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    /// The directory that holds `.facet/`.
    fn root(&self) -> &Path {
        match self.layout {
            Layout::Bundled => self.dir.path(),
            Layout::Unbundled => &self.workspace,
        }
    }

    fn facet(&self) -> Command {
        let root = self.dir.path();
        let mut command = Command::new(common::bins::shipped("facet"));
        command
            .env("FACET_DATA_DIR", root.join("machine"))
            .env("FACET_CONFIG_DIR", root.join("config"))
            // Encrypted secrets backend: deterministic, no OS keyring prompts.
            .env("FACET_SECRET_KEY", SECRET_KEY)
            .env_remove("FACET_ACTOR")
            .env_remove("FACET_SESSION")
            .env_remove("FACET_NO_RECORD")
            .env_remove("FACET_KUBECONFIG")
            .env_remove("FACET_KUBE_CONTEXT")
            .env_remove("FACET_HEDRON_DB")
            .env_remove("FACET_HEDRON_TOKEN")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .current_dir(root);
        for name in PROXY_VARS {
            command.env_remove(name);
        }
        command
    }

    fn run_with(&self, arguments: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = self.facet();
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .expect("facet should run");
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn run(&self, arguments: &[&str]) -> Run {
        self.run_with(arguments, &[])
    }

    /// The selector of the request named `name`.
    fn selector(&self, name: &str) -> String {
        let run = self.run(&["request", "list", self.ws(), "--json"]);
        assert_eq!(run.code, Some(0), "request list");
        let list: Value = serde_json::from_str(&run.stdout).expect("request list JSON");
        list["requests"]
            .as_array()
            .and_then(|requests| {
                requests
                    .iter()
                    .find(|request| request["name"] == name)
                    .and_then(|request| request["selector"].as_str())
            })
            .unwrap_or_else(|| panic!("no request named {name}"))
            .to_owned()
    }

    /// Runs `name` with blobs forced (`--inline-body-max 0`), plus `extra`.
    fn request(&self, name: &str, extra: &[&str], env: &[(&str, &str)]) -> Run {
        let selector = self.selector(name);
        let mut arguments = vec![
            "request",
            "run",
            self.ws(),
            selector.as_str(),
            "--inline-body-max",
            "0",
            "--json",
        ];
        arguments.extend_from_slice(extra);
        self.run_with(&arguments, env)
    }

    /// Recorded runs in the workspace store (none when there is no store).
    fn rows(&self) -> usize {
        let run = self.run(&["history", self.ws(), "--json"]);
        assert_eq!(run.code, Some(0), "history");
        let history: Value = serde_json::from_str(&run.stdout).expect("history JSON");
        history["runs"].as_array().map_or(0, Vec::len)
    }

    /// Body blob files in the workspace store.
    fn blobs(&self) -> usize {
        std::fs::read_dir(self.root().join(".facet/blobs")).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .count()
        })
    }

    fn counts(&self) -> (usize, usize) {
        (self.rows(), self.blobs())
    }
}

fn json(what: &str, run: &Run) -> Value {
    serde_json::from_str(&run.stdout)
        .unwrap_or_else(|error| panic!("{what}: stdout should be JSON ({error})"))
}

fn assert_no_canary(what: &str, stream: &str, text: &str) {
    if let Some(offset) = text.find(VALUE_CANARY) {
        panic!("{what}: the facet value is echoed in {stream} at byte {offset}");
    }
}

/// A `facet mcp` session over stdio.
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
            .stderr(Stdio::null())
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

    /// Calls `name`; returns the raw response line and its result.
    fn call(&mut self, name: &str, arguments: Value) -> (String, Value) {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        })
        .to_string();
        message.push('\n');
        self.stdin.write_all(message.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "{name}: server closed stdout"
        );
        let response: Value =
            serde_json::from_str(&line).unwrap_or_else(|_| panic!("{name}: response not JSON"));
        assert_eq!(response["id"], id, "{name}: response id");
        (line, response["result"].clone())
    }

    fn finish(mut self) {
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success(), "clean EOF exits 0");
    }
}

/// A way to ask for no recording: its name, extra arguments, environment.
type Flag<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);

const LAYOUTS: [Layout; 2] = [Layout::Bundled, Layout::Unbundled];
/// The two policies that turn recording off, and the request each covers.
const POLICIES: [(Level, &str); 2] = [
    (Level::Collection, "{ record: never }"),
    (Level::Item, "{ record: false }"),
];

#[test]
fn recording_still_happens_without_the_key() {
    // The control for every count below: the same collection records a row
    // and places blobs when it carries no `facet:` key.
    for layout in LAYOUTS {
        let what = format!("no key, {layout:?}");
        let host = Host::spawn();
        let sandbox = Sandbox::new(layout, &host.url, Shape::default());
        let run = sandbox.request("One", &[], &[]);
        assert_eq!(run.code, Some(0), "{what}: run");
        host.hit(&what);
        let (rows, blobs) = sandbox.counts();
        assert_eq!(rows, 1, "{what}: one row");
        assert!(blobs > 0, "{what}: bodies placed as blobs");
        assert_eq!(json(&what, &run)["lattice"]["recorded"], true, "{what}");
    }
}

#[test]
fn record_policy_writes_no_row_and_no_blob_whatever_the_flags() {
    for layout in LAYOUTS {
        for (level, value) in POLICIES {
            let host = Host::spawn();
            let sandbox = Sandbox::new(layout, &host.url, Shape::at(level, value));
            let flags: [Flag<'_>; 3] = [
                ("no flag", &[], &[]),
                ("--no-record", &["--no-record"], &[]),
                ("FACET_NO_RECORD", &[], &[("FACET_NO_RECORD", "1")]),
            ];
            for (flag, extra, env) in flags {
                let what = format!("{level:?} policy, {layout:?}, {flag}");
                let run = sandbox.request("One", extra, env);
                assert_eq!(run.code, Some(0), "{what}: the run itself succeeds");
                host.hit(&what);
                assert_eq!(json(&what, &run)["lattice"]["recorded"], false, "{what}");
                assert_eq!(sandbox.counts(), (0, 0), "{what}: no row, no blob");
            }

            let ws = sandbox.ws().to_owned();
            let selector = sandbox.selector("One");
            let mut mcp = Mcp::start(&sandbox, &[]);
            for no_record in [false, true] {
                let what = format!("{level:?} policy, {layout:?}, MCP noRecord {no_record}");
                let (_, result) = mcp.call(
                    "request_run",
                    json!({ "path": ws, "selector": selector, "noRecord": no_record }),
                );
                assert_eq!(result["isError"], false, "{what}");
                host.hit(&what);
                assert_eq!(
                    result["structuredContent"]["lattice"]["recorded"], false,
                    "{what}"
                );
                assert_eq!(sandbox.counts(), (0, 0), "{what}: no row, no blob");
            }
            mcp.finish();

            if let Level::Item = level {
                // The item key covers that item only: `Two` still records.
                let what = format!("item policy, {layout:?}, the other item");
                let run = sandbox.request("Two", &[], &[]);
                assert_eq!(run.code, Some(0), "{what}");
                host.hit(&what);
                assert_eq!(sandbox.rows(), 1, "{what}: one row");
            }
        }
    }
}

#[test]
fn record_policy_also_stops_replay_recording() {
    for layout in LAYOUTS {
        for (level, value) in POLICIES {
            let what = format!("replay under {level:?} policy, {layout:?}");
            let host = Host::spawn();
            // Recorded before the key was added.
            let sandbox = Sandbox::new(layout, &host.url, Shape::default());
            let run = sandbox.request("One", &[], &[]);
            assert_eq!(run.code, Some(0), "{what}: first run");
            host.hit(&what);
            let run_id = json(&what, &run)["lattice"]["runId"]
                .as_str()
                .expect("recorded")
                .to_owned();
            let before = sandbox.counts();
            sandbox.write(Shape::at(level, value));

            let replay = sandbox.run(&["replay", &run_id, sandbox.ws(), "--json"]);
            assert_eq!(replay.code, Some(0), "{what}: replay succeeds");
            host.hit(&what);
            assert_eq!(json(&what, &replay)["lattice"]["recorded"], false, "{what}");
            assert_eq!(sandbox.counts(), before, "{what}: nothing new");

            let mut mcp = Mcp::start(&sandbox, &[]);
            let (_, result) = mcp.call("run_replay", json!({ "id": run_id, "path": sandbox.ws() }));
            assert_eq!(result["isError"], false, "{what}: MCP");
            host.hit(&what);
            assert_eq!(sandbox.counts(), before, "{what}: nothing new from MCP");
            mcp.finish();
        }
    }
}

/// Bad `facet:` values. The canary sits where a value would be echoed.
fn invalid_cases() -> Vec<(&'static str, Level, String)> {
    vec![
        (
            "collection unknown key",
            Level::Collection,
            format!("{{ record: never, g3-unknown: {VALUE_CANARY} }}"),
        ),
        (
            "collection wrong type",
            Level::Collection,
            format!("{{ record: [{VALUE_CANARY}] }}"),
        ),
        (
            "collection unknown value",
            Level::Collection,
            format!("{{ record: {VALUE_CANARY} }}"),
        ),
        (
            "collection key not a mapping",
            Level::Collection,
            VALUE_CANARY.to_owned(),
        ),
        (
            "item unknown key",
            Level::Item,
            format!("{{ record: false, g3-unknown: {VALUE_CANARY} }}"),
        ),
        (
            "item wrong type",
            Level::Item,
            format!("{{ record: {VALUE_CANARY} }}"),
        ),
        (
            "item key not a mapping",
            Level::Item,
            VALUE_CANARY.to_owned(),
        ),
    ]
}

/// The refusal: `facet_key_invalid`, its exit code, details that name the
/// key path, and no value.
fn assert_invalid(what: &str, unknown_key: bool, error: &Value, line: &str) {
    assert_no_canary(what, "output", line);
    assert_eq!(error["category"], "facet_key_invalid", "{what}");
    assert_ne!(error["exitCode"], 0, "{what}: non-zero");
    let path = error["details"]["path"].as_str().unwrap_or_default();
    assert!(path.contains("facet"), "{what}: details name the key path");
    if unknown_key {
        assert!(
            path.contains("g3-unknown"),
            "{what}: the unknown key is named"
        );
    }
}

#[test]
fn invalid_facet_key_is_refused_before_anything_is_sent() {
    for layout in LAYOUTS {
        for (case, level, value) in invalid_cases() {
            let what = format!("{case}, {layout:?}");
            let unknown_key = case.contains("unknown key");
            let host = Host::spawn();
            let sandbox = Sandbox::new(layout, &host.url, Shape::default());
            // A recorded run to replay once the key turns bad.
            let first = sandbox.request("One", &[], &[]);
            assert_eq!(first.code, Some(0), "{what}: first run");
            host.hit(&what);
            let run_id = json(&what, &first)["lattice"]["runId"]
                .as_str()
                .expect("recorded")
                .to_owned();
            let selector = sandbox.selector("One");
            let before = sandbox.counts();
            sandbox.write(Shape::at(level, &value));

            for json_output in [true, false] {
                let what = format!("{what}, run json {json_output}");
                let mut arguments = vec!["request", "run", sandbox.ws(), selector.as_str()];
                if json_output {
                    arguments.push("--json");
                }
                let run = sandbox.run(&arguments);
                assert_no_canary(&what, "stderr", &run.stderr);
                assert_no_canary(&what, "stdout", &run.stdout);
                let code = run.code.expect("facet should exit, not be signalled");
                assert_ne!(code, 0, "{what}: non-zero");
                if json_output {
                    let envelope = json(&what, &run);
                    assert_invalid(&what, unknown_key, &envelope["error"], &run.stdout);
                    assert_eq!(envelope["error"]["exitCode"], code, "{what}");
                } else {
                    assert!(
                        run.stderr.contains("error[facet_key_invalid]"),
                        "{what}: error line"
                    );
                }
                host.untouched(&what);
            }

            let replay = sandbox.run(&["replay", &run_id, sandbox.ws(), "--json"]);
            let what_replay = format!("{what}, replay");
            assert_no_canary(&what_replay, "stderr", &replay.stderr);
            assert_ne!(replay.code, Some(0), "{what_replay}: non-zero");
            assert_invalid(
                &what_replay,
                unknown_key,
                &json(&what_replay, &replay)["error"],
                &replay.stdout,
            );
            host.untouched(&what_replay);

            let mut mcp = Mcp::start(&sandbox, &[]);
            for (tool, arguments) in [
                (
                    "request_run",
                    json!({ "path": sandbox.ws(), "selector": selector }),
                ),
                ("run_replay", json!({ "id": run_id, "path": sandbox.ws() })),
            ] {
                let what = format!("{what}, MCP {tool}");
                let (line, result) = mcp.call(tool, arguments);
                assert_eq!(result["isError"], true, "{what}");
                assert_invalid(
                    &what,
                    unknown_key,
                    &result["structuredContent"]["error"],
                    &line,
                );
                host.untouched(&what);
            }
            mcp.finish();
            assert_eq!(sandbox.counts(), before, "{what}: nothing recorded");
        }
    }
}

#[test]
fn unknown_keys_outside_facet_are_still_accepted() {
    for layout in LAYOUTS {
        let what = format!("extras, {layout:?}");
        let host = Host::spawn();
        let sandbox = Sandbox::new(
            layout,
            &host.url,
            Shape {
                extras: true,
                ..Shape::default()
            },
        );
        let run = sandbox.request("One", &[], &[]);
        assert_eq!(run.code, Some(0), "{what}: run");
        host.hit(&what);
        let run_id = json(&what, &run)["lattice"]["runId"]
            .as_str()
            .expect("recorded")
            .to_owned();
        let replay = sandbox.run(&["replay", &run_id, sandbox.ws(), "--json"]);
        assert_eq!(replay.code, Some(0), "{what}: replay");
        host.hit(&what);
        let mut mcp = Mcp::start(&sandbox, &[]);
        let (_, result) = mcp.call(
            "request_run",
            json!({ "path": sandbox.ws(), "selector": sandbox.selector("One") }),
        );
        assert_eq!(result["isError"], false, "{what}: MCP");
        host.hit(&what);
        mcp.finish();
        assert_eq!(sandbox.rows(), 3, "{what}: every send recorded");
    }
}

#[test]
fn a_yaml_round_trip_keeps_the_facet_key() {
    for layout in LAYOUTS {
        for (level, value) in POLICIES {
            let what = format!("round trip, {level:?} policy, {layout:?}");
            let host = Host::spawn();
            let sandbox = Sandbox::new(
                layout,
                &host.url,
                Shape {
                    extras: true,
                    ..Shape::at(level, value)
                },
            );
            let selector = sandbox.selector("One");
            let set = sandbox.run(&[
                "request",
                "set",
                sandbox.ws(),
                &selector,
                "--name",
                "One",
                "--json",
            ]);
            assert_eq!(set.code, Some(0), "{what}: request set");
            let file = match (layout, level) {
                (Layout::Bundled, _) => sandbox.workspace.clone(),
                (Layout::Unbundled, Level::Collection) => {
                    sandbox.workspace.join("opencollection.yml")
                }
                (Layout::Unbundled, Level::Item) => sandbox.workspace.join("one.yml"),
            };
            let source = std::fs::read_to_string(file).unwrap();
            assert!(source.contains("facet:"), "{what}: facet key kept");
            assert!(
                source.contains(if let Level::Collection = level {
                    "record: never"
                } else {
                    "record: false"
                }),
                "{what}: its value kept"
            );
            assert!(
                source.contains("x-vendor") || source.contains("x-note"),
                "{what}: extras kept"
            );

            // The kept key still applies.
            let run = sandbox.request("One", &[], &[]);
            assert_eq!(run.code, Some(0), "{what}: run");
            host.hit(&what);
            assert_eq!(sandbox.counts(), (0, 0), "{what}: still not recorded");
        }
    }
}
