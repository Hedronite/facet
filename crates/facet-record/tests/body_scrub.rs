//! P1a G4 / H5a: request bodies are scrubbed of known secrets at write.
//!
//! A raw JSON body and a form-urlencoded body each carry a declared secret,
//! from the store and from `--var`. The secret has a space and characters
//! that URL-encode, so the form body carries its encoded form. After the
//! run, `blob`, `history --bodies`, `history --sql` and every file under
//! the sandbox hold no copy of it, at either inline threshold.
//! The controls: the host received the secret in the body, the stored body
//! keeps its plain text with `<redacted>` in place of the secret, and
//! `requestHash` still hashes the raw body, so a dry run of the same
//! request and a `--frozen` replay both match it.
//!
//! A store row stored with `--secret` is a known secret even when the YAML
//! declares its variable plain (SecOps review #54): its value is scrubbed from the
//! stored body the same way. The control is the same request over a plain
//! store row, whose stored body keeps the value.
//!
//! These drive the shipped `facet` binary. Every value below is a canary,
//! and no assertion message prints one.

mod common;

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use serde_json::Value;

/// A space and `&`, `/`, `=`, `+`, so the form body encodes it.
const TOKEN: &str = "g4-token canary&/=+1";
/// `TOKEN` as a form-urlencoded value.
const TOKEN_FORM: &str = "g4-token+canary%26%2F%3D%2B1";
/// Every copy of the token, in any encoding, starts with this.
const STEM: &str = "g4-token";
const SECRET_KEY: &str = "g4-master-key-canary";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];
/// The response never echoes the request: response bodies at rest are not
/// scrubbed in this step.
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
      Content-Length: 11\r\nConnection: close\r\n\r\n{\"g4\":\"ok\"}";

/// A loopback host that answers `200` and reports each request body.
struct Host {
    url: String,
    bodies: Receiver<Vec<u8>>,
}

impl Host {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, bodies) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                if sender.send(read_body(&mut stream)).is_err() {
                    return;
                }
                let _ = stream.write_all(OK);
            }
        });
        Self { url, bodies }
    }

    /// The body of the one request this host received. Never printed.
    fn body(&self, what: &str) -> Vec<u8> {
        let body = self
            .bodies
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: expected a request at this host"));
        assert!(
            self.bodies.try_recv().is_err(),
            "{what}: more than one request at this host"
        );
        body
    }
}

/// The `Content-Length` body after the request head.
fn read_body(stream: &mut impl Read) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return Vec::new(),
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
    request.split_off(head_end)
}

struct Run {
    code: Option<i32>,
    stdout: String,
}

/// The collection, its `.facet/` store, and a private machine store and
/// config, all under one temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    workspace: PathBuf,
}

impl Sandbox {
    fn new(collection_url: &str) -> Self {
        Self::with_fixture(collection_url, "body_scrub.yml")
    }

    fn with_fixture(collection_url: &str, fixture: &str) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let source = std::fs::read_to_string(fixture).expect("fixture");
        let workspace = dir.path().join("workspace.yml");
        std::fs::write(
            &workspace,
            source.replace("__COLLECTION_URL__", collection_url),
        )
        .unwrap();
        Self { dir, workspace }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    fn run(&self, arguments: &[&str]) -> Run {
        let output = self.output(arguments);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_clean(&format!("facet {}", arguments[0]), "stderr", &output.stderr);
        Run {
            code: output.status.code(),
            stdout,
        }
    }

    /// `facet <arguments>`, unchecked: the plain-row control expects the
    /// token in what it reads back.
    fn output(&self, arguments: &[&str]) -> std::process::Output {
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
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .expect("facet should run")
    }

    fn json(&self, what: &str, arguments: &[&str]) -> Value {
        let run = self.run(arguments);
        assert_eq!(run.code, Some(0), "{what}: exit");
        assert_clean(what, "stdout", run.stdout.as_bytes());
        serde_json::from_str(&run.stdout)
            .unwrap_or_else(|error| panic!("{what}: stdout should be JSON ({error})"))
    }

    /// Every file under the sandbox except the collection itself: the
    /// workspace store, its blobs, and the machine store.
    fn assert_store_clean(&self, what: &str) {
        let files = self.store_files();
        for (name, bytes) in &files {
            assert_clean(what, name, bytes);
        }
        assert!(!files.is_empty(), "{what}: the store was written");
    }

    /// Each file under the sandbox except the collection, by relative name.
    fn store_files(&self) -> Vec<(String, Vec<u8>)> {
        let mut pending = vec![self.dir.path().to_owned()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path != self.workspace {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    let name = path
                        .strip_prefix(self.dir.path())
                        .unwrap_or(&path)
                        .display()
                        .to_string();
                    files.push((name, bytes));
                }
            }
        }
        files
    }
}

/// No copy of the token, in any encoding. Reports where, not what.
fn assert_clean(what: &str, place: &str, bytes: &[u8]) {
    if let Some(offset) = find(bytes, STEM) {
        panic!("{what}: the token is in {place} at byte {offset}");
    }
}

fn find(bytes: &[u8], needle: &str) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
}

/// Where the declared secret comes from.
#[derive(Clone, Copy, Debug)]
enum Source {
    Store,
    Var,
}

/// One body case: the item, what the host must have received, and the
/// plain text the stored body must keep.
struct Body {
    item: &'static str,
    sent: &'static str,
    plain: &'static str,
}

const BODIES: [Body; 2] = [
    Body {
        item: "items/0",
        sent: TOKEN,
        plain: "g4-plain-body",
    },
    Body {
        item: "items/1",
        sent: TOKEN_FORM,
        plain: "note=g4+plain+body",
    },
];

#[test]
fn request_bodies_are_scrubbed_at_write_and_still_hash_raw() {
    let var = format!("token={TOKEN}");
    for body in BODIES {
        for source in [Source::Store, Source::Var] {
            // Both thresholds: `0` blobs every body, the default inlines
            // the small response. Request bodies go by the reader rule.
            for inline_max in ["0", "64KiB"] {
                let what = format!("{} from {source:?}, inline max {inline_max}", body.item);
                let host = Host::spawn();
                let sandbox = Sandbox::new(&host.url);
                let mut extra = vec!["--environment", "local"];
                match source {
                    Source::Store => {
                        let set = sandbox.run(&[
                            "env",
                            "set",
                            sandbox.ws(),
                            "--environment",
                            "local",
                            "--name",
                            "token",
                            "--value",
                            TOKEN,
                            "--secret",
                            "--json",
                        ]);
                        assert_eq!(set.code, Some(0), "{what}: env set");
                    }
                    Source::Var => extra.extend(["--var", var.as_str()]),
                }

                let mut arguments = vec!["request", "run", sandbox.ws(), body.item];
                arguments.extend_from_slice(&extra);
                arguments.extend(["--inline-body-max", inline_max, "--json"]);
                let run = sandbox.json(&what, &arguments);
                // Control: the raw body on the wire carried the token.
                let sent = host.body(&what);
                assert!(
                    sent.windows(body.sent.len())
                        .any(|window| window == body.sent.as_bytes()),
                    "{what}: the token was sent in the body"
                );
                let run_id = run["lattice"]["runId"]
                    .as_str()
                    .expect("recorded")
                    .to_owned();
                let request_hash = run["lattice"]["requestHash"]
                    .as_str()
                    .expect("request hash")
                    .to_owned();

                // `requestHash` still hashes the raw body: a dry run of the
                // same request, which records nothing, agrees with it.
                let mut dry = vec!["request", "run", sandbox.ws(), body.item];
                dry.extend_from_slice(&extra);
                dry.extend(["--dry-run", "--json"]);
                let preview = sandbox.json(&format!("{what}, dry run"), &dry);
                assert_eq!(
                    preview["lattice"]["requestHash"], request_hash,
                    "{what}: requestHash is the raw request's"
                );

                // The stored body: plain text kept, the token replaced.
                let row = sandbox.json(
                    &what,
                    &[
                        "history",
                        sandbox.ws(),
                        "--id",
                        &run_id,
                        "--bodies",
                        "--json",
                    ],
                );
                let stored = &row["run"]["request"]["body"];
                // A non-NULL hash means a blob file (the reader rule).
                let content = if let Some(hash) = stored["hash"].as_str() {
                    let hash = hash.to_owned();
                    let target = sandbox.dir.path().join("request-body.out");
                    sandbox.json(
                        &what,
                        &[
                            "blob",
                            &hash,
                            sandbox.ws(),
                            "--output",
                            target.to_str().unwrap(),
                            "--json",
                        ],
                    );
                    let bytes = std::fs::read(&target).unwrap();
                    std::fs::remove_file(&target).unwrap();
                    assert_eq!(
                        lattice::sha256_hex(&bytes),
                        hash,
                        "{what}: the blob hash is the hash of the stored bytes"
                    );
                    let blob = sandbox.json(&what, &["blob", &hash, sandbox.ws(), "--json"]);
                    blob["blob"]["body"]["content"]
                        .as_str()
                        .expect("utf8 blob")
                        .to_owned()
                } else {
                    stored["content"]
                        .as_str()
                        .expect("an inline body")
                        .to_owned()
                };
                assert!(content.contains(body.plain), "{what}: plain text kept");
                assert!(content.contains("<redacted>"), "{what}: the token replaced");

                let sql = sandbox.json(
                    &what,
                    &[
                        "history",
                        sandbox.ws(),
                        "--sql",
                        "SELECT * FROM runs",
                        "--json",
                    ],
                );
                assert!(sql["rows"].as_array().is_some_and(|rows| !rows.is_empty()));
                sandbox.json(
                    &what,
                    &[
                        "history",
                        sandbox.ws(),
                        "--sql",
                        "SELECT * FROM blobs",
                        "--json",
                    ],
                );
                sandbox.assert_store_clean(&what);

                // `--frozen` replays the same request: the raw hash matches.
                let mut replay = vec!["replay", run_id.as_str(), sandbox.ws(), "--frozen"];
                if let Source::Var = source {
                    replay.extend(["--var", var.as_str()]);
                }
                replay.extend(["--inline-body-max", inline_max, "--json"]);
                let replayed = sandbox.json(&format!("{what}, frozen replay"), &replay);
                host.body(&what);
                assert_eq!(replayed["lattice"]["hashChanged"], false, "{what}: frozen");
                assert_eq!(
                    replayed["lattice"]["requestHash"], request_hash,
                    "{what}: the replay hashes the same raw request"
                );
                sandbox.assert_store_clean(&format!("{what}, after replay"));
            }
        }
    }
}

#[test]
fn a_secret_store_row_over_a_plain_yaml_variable_is_scrubbed_from_the_body() {
    for body in BODIES {
        for secret in [true, false] {
            let row = if secret { "secret" } else { "plain" };
            let what = format!("{} over a {row} store row", body.item);
            let host = Host::spawn();
            let sandbox = Sandbox::with_fixture(&host.url, "body_scrub_plain.yml");
            let mut set = vec![
                "env",
                "set",
                sandbox.ws(),
                "--environment",
                "local",
                "--name",
                "token",
                "--value",
                TOKEN,
                "--json",
            ];
            if secret {
                set.push("--secret");
            }
            let set = sandbox.output(&set);
            assert_eq!(set.status.code(), Some(0), "{what}: env set");

            let run = sandbox.output(&[
                "request",
                "run",
                sandbox.ws(),
                body.item,
                "--environment",
                "local",
                "--json",
            ]);
            assert_eq!(run.status.code(), Some(0), "{what}: run exit");
            // The store row was hydrated and sent either way.
            let sent = host.body(&what);
            assert!(
                find(&sent, body.sent).is_some(),
                "{what}: the token was sent"
            );
            let document: Value = serde_json::from_slice(&run.stdout)
                .unwrap_or_else(|error| panic!("{what}: run stdout should be JSON ({error})"));
            let run_id = document["lattice"]["runId"]
                .as_str()
                .unwrap_or_else(|| panic!("{what}: the run is recorded"))
                .to_owned();

            let history = sandbox.output(&[
                "history",
                sandbox.ws(),
                "--id",
                &run_id,
                "--bodies",
                "--json",
            ]);
            assert_eq!(history.status.code(), Some(0), "{what}: history exit");
            let row_json: Value = serde_json::from_slice(&history.stdout).unwrap();
            let hash = row_json["run"]["request"]["body"]["hash"]
                .as_str()
                .unwrap_or_else(|| panic!("{what}: the request body is a blob"))
                .to_owned();
            let target = sandbox.dir.path().join("request-body.out");
            let blob = sandbox.output(&[
                "blob",
                &hash,
                sandbox.ws(),
                "--output",
                target.to_str().unwrap(),
                "--json",
            ]);
            assert_eq!(blob.status.code(), Some(0), "{what}: blob exit");
            let stored = std::fs::read(&target).unwrap();
            std::fs::remove_file(&target).unwrap();
            let blob_json = sandbox.output(&["blob", &hash, sandbox.ws(), "--json"]);
            assert_eq!(blob_json.status.code(), Some(0), "{what}: blob --json exit");
            let sql: Vec<_> = ["SELECT * FROM runs", "SELECT * FROM blobs"]
                .into_iter()
                .map(|query| {
                    let sql = sandbox.output(&["history", sandbox.ws(), "--sql", query, "--json"]);
                    assert_eq!(sql.status.code(), Some(0), "{what}: {query}");
                    sql
                })
                .collect();

            if secret {
                // Zero hits: what the commands print, the blob, and every
                // file under the store.
                for (place, output) in [
                    ("run", &run),
                    ("history --bodies", &history),
                    ("blob", &blob_json),
                    ("history --sql runs", &sql[0]),
                    ("history --sql blobs", &sql[1]),
                ] {
                    assert_clean(&what, &format!("{place} stdout"), &output.stdout);
                    assert_clean(&what, &format!("{place} stderr"), &output.stderr);
                }
                assert_clean(&what, "the blob", &stored);
                assert!(
                    find(&stored, body.plain).is_some(),
                    "{what}: plain text kept"
                );
                assert!(
                    find(&stored, "<redacted>").is_some(),
                    "{what}: the token replaced"
                );
                sandbox.assert_store_clean(&what);
            } else {
                // Control: a plain row is not a known secret, so the stored
                // body keeps it, and so does the store on disk.
                assert!(
                    find(&stored, body.sent).is_some(),
                    "{what}: the plain value is stored in the blob"
                );
                assert!(
                    find(&stored, "<redacted>").is_none(),
                    "{what}: nothing redacted"
                );
                assert!(
                    sandbox
                        .store_files()
                        .iter()
                        .any(|(_, bytes)| find(bytes, STEM).is_some()),
                    "{what}: the plain value is in a store file"
                );
            }
        }
    }
}
