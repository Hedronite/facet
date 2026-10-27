//! P1a G2 / H3, H3a: one redactor on every output surface.
//!
//! Each run's known secret values (hydrated from the store, passed with
//! `--var`, or a token the run sends) and their URL-encoded forms never
//! reach stdout, stderr or an MCP result, and sensitive headers are masked
//! whatever their value. Covered: live run, `--dry-run`, replay, a 5xx, an
//! unreachable host and a URL-bearing transport error, each human and
//! `--json`, plus MCP `request_run` and `run_replay`. Every case has a
//! positive control: the secret really was sent, and plain headers, query
//! values and bodies still print.
//!
//! These drive the shipped `facet` binary. Every value below is a canary,
//! not a credential, and no assertion message prints one: a leak is
//! reported by stream and byte offset only.

#![cfg(unix)]

mod common;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

/// A space and a slash, so the query carries a partly encoded copy.
const TOKEN: &str = "g2-token canary/1";
const PASS: &str = "g2-pass-canary";
const COOKIE: &str = "g2-cookie-canary";
const MINTED: &str = "g2-minted-canary";
/// What the fake `gh auth token` prints.
const GH_TOKEN: &str = "g2-gh-canary";
const SECRET_KEY: &str = "g2-master-key-canary";
/// Each canary starts with one of these. They are unreserved in a URL, so
/// any encoded copy of a canary still contains its stem.
const STEMS: [&str; 6] = [
    "g2-token",
    "g2-pass",
    "g2-cookie",
    "g2-minted",
    "g2-gh",
    "g2-master-key",
];
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// A loopback host that answers every request with `status`, a cookie and a
/// minted token in sensitive headers, a plain header, and a body that
/// echoes the whole request back.
struct Echo {
    url: String,
    requests: Receiver<String>,
}

impl Echo {
    fn spawn(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream);
                let body = format!("plain-body-marker\n{request}");
                let reason = if status >= 500 { "Server Error" } else { "OK" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\n\
                     Set-Cookie: sid={COOKIE}; Path=/\r\nX-Auth-Token: {MINTED}\r\n\
                     X-Plain: plain-header-value\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                if sender.send(request).is_err() {
                    return;
                }
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { url, requests }
    }

    /// The one request this host received, head and body. Never printed.
    fn hit(&self, what: &str) -> String {
        let request = self
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: expected a request at this host"));
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: more than one request at this host"
        );
        request
    }

    fn untouched(&self, what: &str) {
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: a request reached this host"
        );
    }
}

/// The request head and its `Content-Length` body, as text.
fn read_request(stream: &mut impl Read) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&request).into_owned(),
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < head_end + length {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A collection, a private machine store and config, and a fake `gh`, all
/// under one temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    workspace: PathBuf,
}

impl Sandbox {
    fn new(collection_url: &str) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/redaction.yml");
        let source = std::fs::read_to_string(fixture).expect("fixture");
        let workspace = dir.path().join("workspace.yml");
        std::fs::write(
            &workspace,
            source.replace("__COLLECTION_URL__", collection_url),
        )
        .unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, format!("#!/bin/sh\necho '{GH_TOKEN}'\n")).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, workspace }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    fn facet(&self) -> Command {
        let root = self.dir.path();
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(common::bins::shipped("facet"));
        command
            .env("PATH", path)
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
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
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

    fn run(&self, arguments: &[&str]) -> Run {
        let output = self
            .facet()
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

    fn env_set(&self, name: &str, value: &str) {
        let run = self.run(&[
            "env",
            "set",
            self.ws(),
            "--environment",
            "local",
            "--name",
            name,
            "--value",
            value,
            "--secret",
            "--json",
        ]);
        assert_eq!(run.code, Some(0), "env set {name}");
    }
}

/// No canary, in any encoding, anywhere in `text`. Reports where, not what.
fn assert_clean(what: &str, stream: &str, text: &str) {
    for stem in STEMS {
        if let Some(offset) = text.find(stem) {
            panic!("{what}: a canary is in {stream} at byte {offset}");
        }
    }
}

fn assert_run_clean(what: &str, run: &Run) {
    assert_clean(what, "stdout", &run.stdout);
    assert_clean(what, "stderr", &run.stderr);
}

fn json(what: &str, run: &Run) -> Value {
    serde_json::from_str(&run.stdout)
        .unwrap_or_else(|error| panic!("{what}: stdout should be JSON ({error})"))
}

/// The value of the response header `name` in a JSON document.
fn header<'a>(document: &'a Value, name: &str) -> Option<&'a str> {
    document["response"]["headers"]
        .as_array()?
        .iter()
        .find(|header| {
            header["name"]
                .as_str()
                .is_some_and(|header| header.eq_ignore_ascii_case(name))
        })
        .and_then(|header| header["value"].as_str())
}

/// Where the declared secrets come from.
#[derive(Clone, Copy, Debug)]
enum Source {
    /// `facet env set --secret` rows, hydrated by the run.
    Store,
    /// `--var` for the declared secrets.
    Var,
}

impl Source {
    /// Prepares `sandbox` and returns the arguments that supply the secrets.
    fn arguments(self, sandbox: &Sandbox) -> Vec<String> {
        match self {
            Self::Store => {
                sandbox.env_set("token", TOKEN);
                sandbox.env_set("pass", PASS);
                Vec::new()
            }
            Self::Var => vec![
                "--var".to_owned(),
                format!("token={TOKEN}"),
                "--var".to_owned(),
                format!("pass={PASS}"),
            ],
        }
    }
}

/// `request run <ws> <item> --environment local` plus `extra`, and `--json`
/// when `json`.
fn request(sandbox: &Sandbox, item: &str, extra: &[String], json: bool) -> Run {
    let mut arguments = vec![
        "request",
        "run",
        sandbox.ws(),
        item,
        "--environment",
        "local",
    ];
    arguments.extend(extra.iter().map(String::as_str));
    if json {
        arguments.push("--json");
    }
    sandbox.run(&arguments)
}

/// What every echo response must still show: plain values print, secrets
/// and sensitive headers are `<redacted>`.
fn assert_echo_controls(what: &str, run: &Run, json_output: bool, status: u16) {
    let lower = run.stdout.to_ascii_lowercase();
    for plain in [
        "plain-header-value",
        "plain-body-marker",
        "plain-query-value",
        "plain-request-header",
        "plain-request-body",
    ] {
        assert!(run.stdout.contains(plain), "{what}: {plain} should print");
    }
    assert!(
        run.stdout.contains("<redacted>"),
        "{what}: nothing redacted"
    );
    if json_output {
        let document = json(what, run);
        assert_eq!(document["response"]["status"], status, "{what}");
        assert_eq!(header(&document, "x-plain"), Some("plain-header-value"));
        assert_eq!(
            header(&document, "set-cookie"),
            Some("<redacted>"),
            "{what}"
        );
        assert_eq!(
            header(&document, "x-auth-token"),
            Some("<redacted>"),
            "{what}"
        );
        for field in [&document["request"]["url"], &document["response"]["url"]] {
            let url = field.as_str().unwrap_or_default();
            assert!(
                url.contains("t=<redacted>"),
                "{what}: query value not redacted"
            );
            assert!(
                url.contains("plain=plain-query-value"),
                "{what}: plain query lost"
            );
        }
    } else {
        assert!(lower.contains("set-cookie: <redacted>"), "{what}: cookie");
        assert!(lower.contains("x-auth-token: <redacted>"), "{what}: minted");
        assert!(lower.contains("final url: http://"), "{what}: final url");
    }
}

#[test]
fn live_run_and_5xx_redact_urls_headers_and_bodies() {
    for status in [200, 500] {
        for source in [Source::Store, Source::Var] {
            for json_output in [false, true] {
                let what = format!("run {status} from {source:?}, json {json_output}");
                let echo = Echo::spawn(status);
                let sandbox = Sandbox::new(&echo.url);
                let extra = source.arguments(&sandbox);

                let run = request(&sandbox, "items/0", &extra, json_output);
                assert_run_clean(&what, &run);
                assert_eq!(run.code, Some(0), "{what}: exit");
                let sent = echo.hit(&what);
                assert!(sent.contains(TOKEN), "{what}: the token was sent");
                assert_echo_controls(&what, &run, json_output, status);
            }
        }
    }
}

#[test]
fn dry_run_uses_the_same_redaction() {
    for source in [Source::Store, Source::Var] {
        for json_output in [false, true] {
            let what = format!("dry run from {source:?}, json {json_output}");
            let echo = Echo::spawn(200);
            let sandbox = Sandbox::new(&echo.url);
            let mut extra = source.arguments(&sandbox);
            extra.push("--dry-run".to_owned());

            let run = request(&sandbox, "items/0", &extra, json_output);
            assert_run_clean(&what, &run);
            assert_eq!(run.code, Some(0), "{what}: exit");
            echo.untouched(&what);
            for plain in [
                "plain-request-header",
                "plain-query-value",
                "plain-request-body",
            ] {
                assert!(run.stdout.contains(plain), "{what}: {plain} should print");
            }
            assert!(
                run.stdout.contains("<redacted>"),
                "{what}: nothing redacted"
            );
        }
    }
}

#[test]
fn transport_errors_are_redacted_on_stdout_and_stderr() {
    // `items/1`: the token in the query of an unreachable URL. `items/2`:
    // the password in its userinfo.
    for (item, redacted_in_message) in [("items/1", true), ("items/2", false)] {
        for source in [Source::Store, Source::Var] {
            for json_output in [false, true] {
                let what = format!("unreachable {item} from {source:?}, json {json_output}");
                let sandbox = Sandbox::new("http://127.0.0.1:9");
                let extra = source.arguments(&sandbox);

                let run = request(&sandbox, item, &extra, json_output);
                assert_run_clean(&what, &run);
                assert_ne!(run.code, Some(0), "{what}: must fail");
                let message = if json_output {
                    let document = json(&what, &run);
                    assert_eq!(document["error"]["category"], "network_execution", "{what}");
                    document["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                } else {
                    assert!(run.stderr.contains("error["), "{what}: error line");
                    run.stderr.clone()
                };
                assert!(message.contains("127.0.0.1:9"), "{what}: the URL is named");
                if redacted_in_message {
                    assert!(message.contains("t=<redacted>"), "{what}: query redacted");
                }
            }
        }
    }
}

#[test]
fn replay_redacts_and_knows_the_token_it_sends() {
    // `items/0` takes the store token. The echo body would show it unless
    // it is known. The `gh` token never reaches this loopback host: FN-2
    // refuses it before `gh` runs (`github_token.rs`).
    let (item, sent_token) = ("items/0", TOKEN);
    let what = format!("replay {item}");
    let echo = Echo::spawn(200);
    let sandbox = Sandbox::new(&echo.url);
    sandbox.env_set("token", TOKEN);

    let run = request(&sandbox, item, &[], true);
    assert_run_clean(&what, &run);
    assert_eq!(run.code, Some(0), "{what}: first run");
    assert!(echo.hit(&what).contains(sent_token), "{what}: run sends it");
    let run_id = json(&what, &run)["lattice"]["runId"]
        .as_str()
        .expect("the run is recorded")
        .to_owned();

    for json_output in [false, true] {
        let what = format!("{what}, json {json_output}");
        let mut arguments = vec!["replay", run_id.as_str(), sandbox.ws()];
        if json_output {
            arguments.push("--json");
        }
        let replay = sandbox.run(&arguments);
        assert_run_clean(&what, &replay);
        assert_eq!(replay.code, Some(0), "{what}: exit");
        assert!(
            echo.hit(&what).contains(sent_token),
            "{what}: replay sends it"
        );
        assert!(replay.stdout.contains("plain-body-marker"), "{what}: body");
        assert!(
            replay.stdout.contains("<redacted>"),
            "{what}: nothing redacted"
        );
        if json_output {
            let document = json(&what, &replay);
            assert_eq!(document["lattice"]["replayedFrom"], run_id.as_str());
            assert_eq!(
                document["lattice"]["hashChanged"], false,
                "{what}: replay resolves the request the run sent"
            );
        }
    }
}

/// A `facet mcp` session over stdio.
struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn start(sandbox: &Sandbox) -> Self {
        let mut child = sandbox
            .facet()
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

    /// Calls `name` and returns the raw response line and its result.
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

#[test]
fn mcp_request_run_and_run_replay_are_redacted() {
    let echo = Echo::spawn(200);
    let sandbox = Sandbox::new(&echo.url);
    sandbox.env_set("token", TOKEN);
    let ws = sandbox.ws().to_owned();
    let mut mcp = Mcp::start(&sandbox);

    let (line, result) = mcp.call(
        "request_run",
        json!({ "path": ws, "selector": "items/0", "environment": "local" }),
    );
    assert_clean("mcp request_run", "result", &line);
    assert!(echo.hit("mcp request_run").contains(TOKEN), "run sends it");
    let document = &result["structuredContent"];
    assert_eq!(result["isError"], false, "mcp request_run");
    assert_eq!(header(document, "x-plain"), Some("plain-header-value"));
    assert_eq!(header(document, "set-cookie"), Some("<redacted>"));
    assert!(
        document["response"]["body"]["content"]
            .as_str()
            .is_some_and(|body| body.contains("plain-body-marker") && body.contains("<redacted>")),
        "mcp request_run: body"
    );
    let run_id = document["lattice"]["runId"].as_str().unwrap().to_owned();

    let (line, result) = mcp.call("run_replay", json!({ "id": run_id, "path": ws }));
    assert_clean("mcp run_replay", "result", &line);
    assert!(
        echo.hit("mcp run_replay").contains(TOKEN),
        "replay sends it"
    );
    assert_eq!(result["isError"], false, "mcp run_replay");
    assert_eq!(
        header(&result["structuredContent"], "x-plain"),
        Some("plain-header-value")
    );

    // An MCP error envelope is redacted too.
    let (line, result) = mcp.call(
        "request_run",
        json!({ "path": ws, "selector": "items/1", "environment": "local" }),
    );
    assert_clean("mcp transport error", "result", &line);
    assert_eq!(result["isError"], true, "mcp transport error");
    assert!(
        result["structuredContent"]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("t=<redacted>")),
        "mcp transport error: query redacted"
    );
    mcp.finish();
}

#[test]
fn plain_request_prints_headers_and_body_unchanged() {
    for json_output in [false, true] {
        let what = format!("plain, json {json_output}");
        let echo = Echo::spawn(200);
        let sandbox = Sandbox::new(&echo.url);
        let run = request(&sandbox, "items/4", &[], json_output);
        assert_run_clean(&what, &run);
        assert_eq!(run.code, Some(0), "{what}: exit");
        echo.hit(&what);
        assert!(run.stdout.contains("plain-header-value"), "{what}: header");
        assert!(run.stdout.contains("plain-body-marker"), "{what}: body");
        assert!(
            run.stdout
                .to_ascii_lowercase()
                .contains("x-plain-request: plain-request-header"),
            "{what}: the echoed request header"
        );
        if json_output {
            let document = json(&what, &run);
            assert_eq!(
                document["request"]["url"],
                format!("{}/plain", echo.url),
                "{what}: url unchanged"
            );
            let body = document["response"]["body"]["content"].as_str().unwrap();
            assert!(!body.contains("<redacted>"), "{what}: nothing to redact");
        }
    }
}
