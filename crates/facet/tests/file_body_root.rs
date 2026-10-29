//! P1b G2 / H7: a file body or multipart file part is sent only from inside
//! the collection root.
//!
//! Each refusal (`..`, absolute outside, a symlink out, a multipart part
//! out, a missing file, no root) exits 5 with `file_body_outside_root`
//! before anything is sent: a loopback mock sees no connection, and no
//! output names the path. This holds on `request run`, `--dry-run`,
//! `replay` and MCP; the TUI is frontend's test. The inside cases
//! (relative, absolute, a symlink in) send exactly the file's bytes.
//! `--allow-file-outside-root` lets an outside file through on run and
//! replay; MCP has no way to ask for it.
//!
//! These drive the shipped binary and touch no network. Every value below
//! is a canary, not a credential.

#![cfg(unix)]

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    io::ErrorKind,
    process::{Output, Stdio},
    time::Duration,
};

use common::*;

const INSIDE: &[u8] = b"h7-inside-bytes";
const OUTSIDE: &[u8] = b"h7-outside-bytes";
const REFUSED_EXIT: i32 = 5;
/// Strings that would mean a path leaked into output.
const LEAKS: [&str; 5] = ["out.bin", "nope.bin", "link-out", "../", "h7-outside-bytes"];

/// Selector, file body YAML. `__OUT__` and `__IN__` become absolute paths.
const ITEMS: [(&str, &str); 9] = [
    ("relative inside", "file: in.bin"),
    ("absolute inside", "file: __IN__"),
    ("symlink in", "file: link-in"),
    ("dotdot out", "file: ../out.bin"),
    ("absolute out", "file: __OUT__"),
    ("symlink out", "file: link-out"),
    ("multipart out", "multipart: ../out.bin"),
    ("missing", "file: nope.bin"),
    ("multipart in", "multipart: in.bin"),
];

/// `<sandbox>/col/` holds the collection, `in.bin`, `link-in -> in.bin` and
/// `link-out -> ../out.bin`; `<sandbox>/out.bin` is outside the root.
struct Fx {
    sandbox: Sandbox,
    collection: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let col = sandbox.root().join("col");
        fs::create_dir(&col).unwrap();
        fs::write(col.join("in.bin"), INSIDE).unwrap();
        fs::write(sandbox.root().join("out.bin"), OUTSIDE).unwrap();
        std::os::unix::fs::symlink("in.bin", col.join("link-in")).unwrap();
        std::os::unix::fs::symlink("../out.bin", col.join("link-out")).unwrap();
        let collection = col.join("collection.yml");
        Self {
            sandbox,
            collection,
        }
    }

    fn ws(&self) -> &str {
        self.collection.to_str().unwrap()
    }

    /// Writes the collection with every item pointing at `url`, and with
    /// item `swap` (if any) reading `swap_body` instead.
    fn write(&self, url: &str, swap: Option<(usize, &str)>) -> String {
        let source = collection_yaml(self.sandbox.root(), url, swap);
        fs::write(&self.collection, &source).unwrap();
        source
    }

    fn facet(&self, arguments: &[&str]) -> Output {
        self.sandbox
            .facet()
            .args(arguments)
            .arg("--json")
            .output()
            .expect("facet should run")
    }
}

fn collection_yaml(root: &Path, url: &str, swap: Option<(usize, &str)>) -> String {
    let inside = root.join("col/in.bin");
    let outside = root.join("out.bin");
    let mut source = String::from(
        "opencollection: 1.0.0\ninfo: { name: H7 file root }\nbundled: true\nitems:\n",
    );
    for (index, (name, body)) in ITEMS.iter().enumerate() {
        let body = match swap {
            Some((swapped, replacement)) if swapped == index => replacement,
            _ => body,
        };
        let body = body
            .replace("__IN__", inside.to_str().unwrap())
            .replace("__OUT__", outside.to_str().unwrap());
        let yaml = match body.split_once(": ").unwrap() {
            ("file", path) => format!(
                "      body:\n        type: file\n        data:\n          - filePath: \"{path}\"\n            contentType: application/octet-stream\n            selected: true\n"
            ),
            ("multipart", path) => format!(
                "      body:\n        type: multipart-form\n        data:\n          - name: upload\n            type: file\n            value: \"{path}\"\n"
            ),
            _ => unreachable!(),
        };
        source.push_str(&format!(
            "  - info: {{ name: {name}, type: http, seq: {} }}\n    http:\n      method: POST\n      url: {url}/h7\n{yaml}",
            index + 1
        ));
    }
    source
}

/// A loopback listener. [`Mock::serve`] answers one request and returns its
/// body; [`Mock::untouched`] proves nothing connected.
struct Mock {
    listener: TcpListener,
    url: String,
}

impl Mock {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let url = format!("http://{}", listener.local_addr().unwrap());
        Self { listener, url }
    }

    fn serve(&self) -> JoinHandle<Vec<u8>> {
        let listener = self.listener.try_clone().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let body = read_body(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
            body
        })
    }

    fn untouched(&self, what: &str) {
        self.listener.set_nonblocking(true).unwrap();
        match self.listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Ok(_) => panic!("{what}: the mock received a connection"),
            Err(error) => panic!("{what}: accept failed: {error}"),
        }
        self.listener.set_nonblocking(false).unwrap();
    }
}

/// The request body, by `Content-Length` or chunked transfer encoding.
fn read_body(stream: &mut impl Read) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let header_end = loop {
        if let Some(position) = data.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break position + 4;
        }
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "connection closed before headers");
        data.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&data[..header_end]).to_ascii_lowercase();
    let mut rest = data[header_end..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        while !rest.ends_with(b"0\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "connection closed inside a chunked body");
            rest.extend_from_slice(&buffer[..count]);
        }
        let mut body = Vec::new();
        let mut cursor = rest.as_slice();
        loop {
            let line_end = cursor
                .windows(2)
                .position(|bytes| bytes == b"\r\n")
                .unwrap();
            let size_text = String::from_utf8_lossy(&cursor[..line_end]).into_owned();
            let size =
                usize::from_str_radix(size_text.split(';').next().unwrap().trim(), 16).unwrap();
            cursor = &cursor[line_end + 2..];
            if size == 0 {
                return body;
            }
            body.extend_from_slice(&cursor[..size]);
            cursor = &cursor[size + 2..];
        }
    }
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim() == "content-length")
        .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
    while rest.len() < length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "connection closed inside a body");
        rest.extend_from_slice(&buffer[..count]);
    }
    rest.truncate(length);
    rest
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Exit 5, `file_body_outside_root` for `selector` with `reason`, and no
/// path or outside byte anywhere in the output.
fn assert_refused(what: &str, output: &Output, selector: &str, reason: &str) {
    assert_eq!(output.status.code(), Some(REFUSED_EXIT), "{what}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{what}: stdout is not JSON"));
    assert_eq!(
        envelope["error"]["category"], "file_body_outside_root",
        "{what}"
    );
    assert_eq!(envelope["error"]["details"]["request"], selector, "{what}");
    assert_eq!(envelope["error"]["details"]["reason"], reason, "{what}");
    assert_no_leak(what, &format!("{stdout}{stderr}"));
}

fn assert_no_leak(what: &str, text: &str) {
    for leak in LEAKS {
        assert!(!text.contains(leak), "{what}: output names {leak}");
    }
    assert!(!text.contains("/col/"), "{what}: output names the root");
}

const REFUSALS: [(usize, &str); 5] = [
    (3, "outside_root"),
    (4, "outside_root"),
    (5, "outside_root"),
    (6, "outside_root"),
    (7, "missing"),
];

#[test]
fn refusals_send_nothing_and_name_no_path_on_run_and_dry_run() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);
    for (index, reason) in REFUSALS {
        let selector = format!("items/{index}");
        let what = ITEMS[index].0;
        let run = fx.facet(&["request", "run", fx.ws(), &selector, "--no-record"]);
        assert_refused(what, &run, &selector, reason);
        let dry = fx.facet(&["request", "run", fx.ws(), &selector, "--dry-run"]);
        assert_refused(&format!("{what} --dry-run"), &dry, &selector, reason);
        mock.untouched(what);
    }
}

#[test]
fn inside_files_send_their_exact_bytes() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);
    for index in [0, 1, 2] {
        let server = mock.serve();
        let run = fx.facet(&["request", "run", fx.ws(), &format!("items/{index}")]);
        let what = ITEMS[index].0;
        assert!(
            run.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&run.stdout)
        );
        assert_eq!(server.join().unwrap(), INSIDE, "{what}: bytes differ");
    }
    // A multipart file part inside the root carries the file's bytes.
    let server = mock.serve();
    let run = fx.facet(&["request", "run", fx.ws(), "items/8", "--no-record"]);
    assert!(run.status.success(), "multipart in");
    let body = server.join().unwrap();
    assert!(contains(&body, INSIDE), "multipart in: file bytes missing");
    assert!(!contains(&body, OUTSIDE));
    // --dry-run accepts the inside cases and sends nothing.
    for index in [0, 1, 2, 8] {
        let dry = fx.facet(&[
            "request",
            "run",
            fx.ws(),
            &format!("items/{index}"),
            "--dry-run",
        ]);
        assert!(dry.status.success(), "{} --dry-run", ITEMS[index].0);
    }
    mock.untouched("dry runs");
}

#[test]
fn no_root_is_refused_and_the_flag_lets_an_absolute_file_through() {
    let fx = Fx::new();
    let mock = Mock::new();
    let source = collection_yaml(fx.sandbox.root(), &mock.url, None);
    let stdin_run = |extra: &[&str]| {
        let mut child = fx
            .sandbox
            .facet()
            .args(["request", "run", "-"])
            .args(extra)
            .arg("--json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    for (selector, what) in [("items/0", "stdin relative"), ("items/1", "stdin absolute")] {
        let run = stdin_run(&[selector]);
        assert_refused(what, &run, selector, "no_base");
        let dry = stdin_run(&[selector, "--dry-run"]);
        assert_refused(what, &dry, selector, "no_base");
    }
    mock.untouched("no root");

    // The flag: with no root an absolute path goes through, canonicalized.
    let server = mock.serve();
    let run = stdin_run(&["items/4", "--allow-file-outside-root"]);
    assert!(run.status.success(), "flag with no root");
    assert_eq!(server.join().unwrap(), OUTSIDE);
}

#[test]
fn the_flag_lets_an_outside_file_through_on_run_but_never_a_missing_one() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);
    for index in [3, 4, 5] {
        let server = mock.serve();
        let run = fx.facet(&[
            "request",
            "run",
            fx.ws(),
            &format!("items/{index}"),
            "--allow-file-outside-root",
            "--no-record",
        ]);
        let what = ITEMS[index].0;
        assert!(run.status.success(), "{what} with the flag");
        assert_eq!(server.join().unwrap(), OUTSIDE, "{what}: bytes differ");
    }
    let server = mock.serve();
    let run = fx.facet(&[
        "request",
        "run",
        fx.ws(),
        "items/6",
        "--allow-file-outside-root",
        "--no-record",
    ]);
    assert!(run.status.success(), "multipart out with the flag");
    assert!(contains(&server.join().unwrap(), OUTSIDE));

    let run = fx.facet(&[
        "request",
        "run",
        fx.ws(),
        "items/7",
        "--allow-file-outside-root",
    ]);
    assert_refused("missing with the flag", &run, "items/7", "missing");
    mock.untouched("missing with the flag");
}

#[test]
fn replay_refuses_an_outside_file_unless_flagged() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);
    let server = mock.serve();
    let run = fx.facet(&["request", "run", fx.ws(), "items/0"]);
    assert!(run.status.success(), "record");
    assert_eq!(server.join().unwrap(), INSIDE);
    let recorded: Value = serde_json::from_slice(&run.stdout).unwrap();
    let id = recorded["lattice"]["runId"].as_str().unwrap().to_owned();

    // Positive control: the unchanged YAML replays and sends the same bytes.
    let server = mock.serve();
    let replay = fx.facet(&["replay", &id, fx.ws(), "--no-record"]);
    assert!(replay.status.success(), "control replay");
    assert_eq!(server.join().unwrap(), INSIDE);

    // The current YAML now reads each outside case for that item.
    for (body, reason) in [
        ("file: ../out.bin", "outside_root"),
        ("file: __OUT__", "outside_root"),
        ("file: link-out", "outside_root"),
        ("multipart: ../out.bin", "outside_root"),
        ("file: nope.bin", "missing"),
    ] {
        fx.write(&mock.url, Some((0, body)));
        let replay = fx.facet(&["replay", &id, fx.ws(), "--no-record"]);
        assert_refused(&format!("replay {body}"), &replay, "items/0", reason);
        let envelope: Value = serde_json::from_slice(&replay.stdout).unwrap();
        assert_eq!(envelope["error"]["details"]["replayedFrom"], id.as_str());
        mock.untouched(body);
    }

    // The flag lets an outside file through on replay.
    fx.write(&mock.url, Some((0, "file: ../out.bin")));
    let server = mock.serve();
    let replay = fx.facet(&[
        "replay",
        &id,
        fx.ws(),
        "--allow-file-outside-root",
        "--no-record",
    ]);
    assert!(replay.status.success(), "replay with the flag");
    assert_eq!(server.join().unwrap(), OUTSIDE);
}

/// `facet mcp`, fed `calls` as `tools/call` after the handshake; returns
/// each call's `(structuredContent, isError)` and the raw stdout.
fn mcp(fx: &Fx, calls: &[Value]) -> (Vec<(Value, bool)>, String) {
    let mut child = fx
        .sandbox
        .facet()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("facet mcp should start");
    let mut input = vec![
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "h7", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ];
    for (index, call) in calls.iter().enumerate() {
        input.push(
            json!({ "jsonrpc": "2.0", "id": index + 1, "method": "tools/call", "params": call }),
        );
    }
    let mut stdin = child.stdin.take().unwrap();
    for message in &input {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut results = vec![(Value::Null, false); calls.len()];
    for line in stdout.lines() {
        let response: Value = serde_json::from_str(line).unwrap();
        let Some(id) = response["id"].as_u64().filter(|id| *id > 0) else {
            continue;
        };
        let index = usize::try_from(id).unwrap() - 1;
        results[index] = match response.get("result") {
            Some(result) => (
                result["structuredContent"].clone(),
                result["isError"] == true,
            ),
            // A JSON-RPC error is a refusal too.
            None => (response["error"].clone(), true),
        };
    }
    (results, stdout)
}

#[test]
fn mcp_refuses_outside_files_with_no_way_to_allow_them() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);

    // Positive control: an inside file is sent through MCP.
    let server = mock.serve();
    let (results, _) = mcp(
        &fx,
        &[json!({ "name": "request_run",
            "arguments": { "path": fx.ws(), "selector": "items/0", "noRecord": true } })],
    );
    assert!(!results[0].1, "mcp inside: {}", results[0].0);
    assert_eq!(server.join().unwrap(), INSIDE);

    let mut calls = Vec::new();
    for (index, _) in REFUSALS {
        calls.push(json!({ "name": "request_run",
            "arguments": { "path": fx.ws(), "selector": format!("items/{index}") } }));
    }
    // An outside-root argument is not a way in.
    calls.push(json!({ "name": "request_run",
        "arguments": { "path": fx.ws(), "selector": "items/3",
            "allowFileOutsideRoot": true } }));
    let (results, stdout) = mcp(&fx, &calls);
    for (position, (index, reason)) in REFUSALS.iter().enumerate() {
        let (document, is_error) = &results[position];
        let what = ITEMS[*index].0;
        assert!(is_error, "mcp {what}: {document}");
        assert_eq!(
            document["error"]["category"], "file_body_outside_root",
            "mcp {what}"
        );
        assert_eq!(
            document["error"]["details"]["reason"], *reason,
            "mcp {what}"
        );
    }
    assert!(
        results[REFUSALS.len()].1,
        "mcp allowFileOutsideRoot was honoured"
    );
    assert_no_leak("mcp", &stdout);
    mock.untouched("mcp");
}

#[test]
fn mcp_replay_refuses_an_outside_file() {
    let fx = Fx::new();
    let mock = Mock::new();
    fx.write(&mock.url, None);
    let server = mock.serve();
    let run = fx.facet(&["request", "run", fx.ws(), "items/0"]);
    assert!(run.status.success(), "record");
    assert_eq!(server.join().unwrap(), INSIDE);
    let recorded: Value = serde_json::from_slice(&run.stdout).unwrap();
    let id = recorded["lattice"]["runId"].as_str().unwrap().to_owned();

    fx.write(&mock.url, Some((0, "file: ../out.bin")));
    let (results, stdout) = mcp(
        &fx,
        &[json!({ "name": "run_replay",
            "arguments": { "id": id, "path": fx.ws(), "noRecord": true } })],
    );
    let (document, is_error) = &results[0];
    assert!(is_error, "mcp replay: {document}");
    assert_eq!(document["error"]["category"], "file_body_outside_root");
    assert_no_leak("mcp replay", &stdout);
    mock.untouched("mcp replay");
}
