//! P0 / H13: a machine-store `baseUrl` or `serverUrl` row must not become the
//! host of a request that references a declared secret. The run fails closed
//! (`host_override_refused`, non-zero, nothing on the wire) and the store
//! value appears in no output. A request that references no declared secret
//! still takes the store host.
//!
//! These drive the shipped `facet` binary, so `overlay_secrets` and the CLI
//! error mapping are exercised together, each run with its own private
//! machine store. Every value below is a canary, not a credential, and no
//! assertion message prints one.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        OnceLock,
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};

const STORE_MARK: &str = "h13-store-canary";
const TOKEN: &str = "h13-token-canary-not-a-credential";
const SECRET_KEY: &str = "h13-master-key-canary";
const USER_MARK: &str = "h13-user-canary";
const PASS_MARK: &str = "h13-pass-canary";

/// `facet` built from this workspace into the target dir and profile running
/// this test. `cargo build` is a no-op when it is fresh and rebuilds when
/// `hydrate.rs` or `error.rs` changed, so the binary is never stale.
fn facet_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().expect("test executable path");
        // <target>/[<triple>/]<profile>/deps/host_override-<hash>
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("profile directory");
        let profile = profile_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("profile name");
        let mut command = Command::new(env!("CARGO"));
        command
            .args(["build", "--locked", "-p", "facet-cli", "--bin", "facet"])
            .args(["--message-format", "json-render-diagnostics"])
            .args([
                "--profile",
                if profile == "debug" { "dev" } else { profile },
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        let parent = profile_dir.parent().expect("target directory");
        if parent.join("CACHEDIR.TAG").exists() {
            command.arg("--target-dir").arg(parent);
        } else {
            let triple = parent.file_name().expect("target triple");
            command
                .arg("--target-dir")
                .arg(parent.parent().expect("target directory"))
                .arg("--target")
                .arg(triple);
        }
        let output = command.output().expect("cargo should run");
        assert!(
            output.status.success(),
            "cargo build -p facet-cli failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|message| {
                message["reason"] == "compiler-artifact" && message["target"]["name"] == "facet"
            })
            .find_map(|message| message["executable"].as_str().map(PathBuf::from))
            .expect("cargo should report the facet executable")
    })
}

/// A loopback HTTP host. Each request it receives is answered `200 {}` and
/// its head is sent to `requests`, so a test can prove a hit or its absence.
struct Host {
    url: String,
    requests: Receiver<String>,
}

impl Host {
    fn spawn() -> Self {
        Self::spawn_at("127.0.0.1:0")
    }

    /// A host on another loopback address (`[::1]:0`): a different host by
    /// address, whatever the port.
    fn spawn_at(address: &str) -> Self {
        let listener = TcpListener::bind(address).expect("loopback listener");
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
                if sender
                    .send(String::from_utf8_lossy(&head).into_owned())
                    .is_err()
                {
                    return;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                      Content-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            }
        });
        Self { url, requests }
    }

    /// The one request this host received. The head is returned, not printed.
    fn hit(&self, what: &str) -> String {
        let head = self
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: expected a request at this host"));
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: more than one request at this host"
        );
        head
    }

    fn untouched(&self, what: &str) {
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: a request reached this host"
        );
    }
}

/// Where a declared-secret `token` comes from on the secret request.
#[derive(Clone, Copy, Debug)]
enum Token {
    /// A `facet env set --secret` row, hydrated by `overlay_secrets`.
    Store,
    /// `--var token=…`, which skips Lattice but is still a declared secret.
    Var,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A collection, its `.facet/`, and a private machine store and config, all
/// under one temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    workspace: PathBuf,
    canaries: Vec<String>,
}

impl Sandbox {
    fn new(collection_url: &str) -> Self {
        Self::with_fixture("host_override.yml", collection_url)
    }

    /// `fixture` under `tests/fixtures/`, with `__COLLECTION_URL__` and
    /// `__COLLECTION_AUTHORITY__` (the URL without its scheme) filled in.
    fn with_fixture(fixture: &str, collection_url: &str) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let source = std::fs::read_to_string(path).expect("fixture");
        let authority = collection_url
            .strip_prefix("http://")
            .expect("loopback hosts are http");
        let workspace = dir.path().join("workspace.yml");
        std::fs::write(
            &workspace,
            source
                .replace("__COLLECTION_URL__", collection_url)
                .replace("__COLLECTION_AUTHORITY__", authority),
        )
        .unwrap();
        Self {
            dir,
            workspace,
            canaries: vec![
                STORE_MARK.to_owned(),
                USER_MARK.to_owned(),
                PASS_MARK.to_owned(),
                TOKEN.to_owned(),
                SECRET_KEY.to_owned(),
            ],
        }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    fn facet(&self) -> Command {
        let root = self.dir.path();
        let mut command = Command::new(facet_bin());
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
            .current_dir(root);
        command
    }

    fn run(&self, arguments: &[&str]) -> Run {
        let output = self
            .facet()
            .args(arguments)
            .output()
            .expect("facet should run");
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// `text` with every canary replaced, safe to put in a failure message.
    fn scrub(&self, text: &str) -> String {
        self.canaries.iter().fold(text.to_owned(), |text, canary| {
            text.replace(canary, "<canary>")
        })
    }

    fn assert_clean(&self, what: &str, text: &str) {
        for canary in &self.canaries {
            assert!(
                !text.contains(canary.as_str()),
                "{what}: a canary value is in the output:\n{}",
                self.scrub(text)
            );
        }
    }

    fn env_set(&self, name: &str, value: &str, secret: bool) {
        let mut arguments = vec![
            "env",
            "set",
            self.ws(),
            "--environment",
            "local",
            "--name",
            name,
            "--value",
            value,
            "--json",
        ];
        if secret {
            arguments.push("--secret");
        }
        let run = self.run(&arguments);
        assert_eq!(
            run.code,
            Some(0),
            "env set {name}: {}",
            self.scrub(&run.stderr)
        );
    }

    /// A store host row whose value carries the canary mark.
    fn store_host(&mut self, name: &str, host: &Host) -> String {
        self.store_value(name, &format!("{}/{STORE_MARK}", host.url))
    }

    /// A plain store row with an arbitrary value, kept out of all output.
    fn store_value(&mut self, name: &str, value: &str) -> String {
        self.env_set(name, value, false);
        self.canaries.insert(0, value.to_owned());
        value.to_owned()
    }

    fn request(&self, item: &str, extra: &[&str], json: bool) -> Run {
        let mut arguments = vec!["request", "run", self.ws(), item, "--environment", "local"];
        arguments.extend_from_slice(extra);
        if json {
            arguments.push("--json");
        }
        self.run(&arguments)
    }

    /// The refusal oracle: `host_override_refused` naming `name`, a non-zero
    /// exit in JSON and human output, no request at any of `hosts`, and no
    /// canary in the output or in what the run left in Lattice.
    fn assert_refused(&self, what: &str, item: &str, extra: &[&str], name: &str, hosts: &[&Host]) {
        let run = self.request(item, extra, true);
        self.assert_clean(what, &run.stdout);
        self.assert_clean(what, &run.stderr);
        let code = run.code.expect("facet should exit, not be signalled");
        assert_ne!(code, 0, "{what}: the refusal must exit non-zero");
        let envelope = self.json(what, &run);
        let error = &envelope["error"];
        assert_eq!(
            error["category"],
            "host_override_refused",
            "{what}: {}",
            self.scrub(&envelope.to_string())
        );
        assert_eq!(error["exitCode"], json!(code), "{what}");
        assert_eq!(error["details"], json!({ "variable": name }), "{what}");
        assert!(
            error["message"].as_str().unwrap_or_default().contains(name),
            "{what}: the message names the variable"
        );
        for host in hosts {
            host.untouched(what);
        }

        // Human output fails the same way and is just as clean.
        let human = self.request(item, extra, false);
        self.assert_clean(what, &human.stdout);
        self.assert_clean(what, &human.stderr);
        assert_eq!(human.code, Some(code), "{what}: human exit");
        for host in hosts {
            host.untouched(what);
        }

        // Nothing the refusal left in Lattice carries the value either.
        let history = self.run(&["history", self.ws(), "--json"]);
        self.assert_clean(what, &history.stdout);
        self.assert_clean(what, &history.stderr);
        let dump = self.run(&[
            "history",
            self.ws(),
            "--sql",
            "SELECT url, req_headers, error FROM runs",
            "--json",
        ]);
        self.assert_clean(what, &dump.stdout);
        self.assert_clean(what, &dump.stderr);
    }

    fn json(&self, what: &str, run: &Run) -> Value {
        serde_json::from_str(&run.stdout).unwrap_or_else(|error| {
            panic!(
                "{what}: stdout should be JSON ({error}), exit {:?}\nstdout: {}\nstderr: {}",
                run.code,
                self.scrub(&run.stdout),
                self.scrub(&run.stderr)
            )
        })
    }
}

#[test]
fn secret_request_refuses_store_host_row() {
    for (item, name) in [("items/0", "serverUrl"), ("items/1", "baseUrl")] {
        for token in [Token::Store, Token::Var] {
            let what = format!("{name} with token from {token:?}");
            let collection = Host::spawn();
            let store = Host::spawn();
            let mut sandbox = Sandbox::new(&collection.url);
            sandbox.store_host(name, &store);
            let var = format!("token={TOKEN}");
            let extra: &[&str] = match token {
                Token::Store => {
                    sandbox.env_set("token", TOKEN, true);
                    &[]
                }
                Token::Var => &["--var", &var],
            };

            sandbox.assert_refused(&what, item, extra, name, &[&store, &collection]);
        }
    }
}

/// Runs `case` once with the token from the store and once from `--var`.
fn for_each_token(mut case: impl FnMut(Token, &[&str], &mut Sandbox), fixture: &str, url: &str) {
    let var = format!("token={TOKEN}");
    for token in [Token::Store, Token::Var] {
        let mut sandbox = Sandbox::with_fixture(fixture, url);
        let extra: &[&str] = match token {
            Token::Store => {
                sandbox.env_set("token", TOKEN, true);
                &[]
            }
            Token::Var => &["--var", &var],
        };
        case(token, extra, &mut sandbox);
    }
}

#[test]
fn secret_request_refuses_store_host_under_any_variable_name() {
    // `{{apiUrl}}/…` and `http://{{host}}/…`: no `baseUrl` or `serverUrl`
    // in sight, and the store row still must not move the host.
    let cases = [("items/0", "apiUrl", false), ("items/1", "host", true)];
    for (item, name, authority_only) in cases {
        let collection = Host::spawn();
        let store = Host::spawn_at("[::1]:0");
        for_each_token(
            |token, extra, sandbox| {
                let what = format!("{name} with token from {token:?}");
                if authority_only {
                    let authority = store.url.strip_prefix("http://").unwrap();
                    sandbox.store_value(name, authority);
                } else {
                    sandbox.store_host(name, &store);
                }
                sandbox.assert_refused(&what, item, extra, name, &[&store, &collection]);
            },
            "host_override_names.yml",
            &collection.url,
        );
    }
}

#[test]
fn secret_request_refuses_store_host_under_case_variants() {
    // Names that differ from `serverUrl` / `baseUrl` only by case.
    for (item, name) in [
        ("items/2", "ServerURL"),
        ("items/3", "BASEURL"),
        ("items/4", "serverurl"),
    ] {
        let collection = Host::spawn();
        let store = Host::spawn_at("[::1]:0");
        for_each_token(
            |token, extra, sandbox| {
                let what = format!("{name} with token from {token:?}");
                sandbox.store_host(name, &store);
                sandbox.assert_refused(&what, item, extra, name, &[&store, &collection]);
            },
            "host_override_names.yml",
            &collection.url,
        );
    }

    // An upper-case DNS name that is not the collection's host.
    for (fixture, item, name) in [
        ("host_override.yml", "items/0", "serverUrl"),
        ("host_override_names.yml", "items/0", "apiUrl"),
    ] {
        let collection = Host::spawn();
        let store = Host::spawn();
        let port = store.url.rsplit(':').next().unwrap();
        for_each_token(
            |token, extra, sandbox| {
                let what = format!("{name} as LOCALHOST with token from {token:?}");
                sandbox.store_value(name, &format!("http://LOCALHOST:{port}/{STORE_MARK}"));
                sandbox.assert_refused(&what, item, extra, name, &[&store, &collection]);
            },
            fixture,
            &collection.url,
        );
    }
}

#[test]
fn secret_request_refuses_store_host_with_userinfo() {
    for (fixture, item, name) in [
        ("host_override.yml", "items/0", "serverUrl"),
        ("host_override.yml", "items/1", "baseUrl"),
        ("host_override_names.yml", "items/0", "apiUrl"),
    ] {
        let collection = Host::spawn();
        let store = Host::spawn_at("[::1]:0");
        let collection_authority = collection.url.strip_prefix("http://").unwrap();
        let store_authority = store.url.strip_prefix("http://").unwrap();
        // Same host as the collection, userinfo added; then userinfo that
        // spells the collection host in front of a different one.
        let values = [
            format!("http://{USER_MARK}:{PASS_MARK}@{collection_authority}/{STORE_MARK}"),
            format!("http://{USER_MARK}@{collection_authority}/{STORE_MARK}"),
            format!("http://{collection_authority}@{store_authority}/{STORE_MARK}"),
        ];
        for (index, value) in values.iter().enumerate() {
            for_each_token(
                |token, extra, sandbox| {
                    let what = format!("{name} userinfo case {index} with token from {token:?}");
                    sandbox.store_value(name, value);
                    sandbox.assert_refused(&what, item, extra, name, &[&store, &collection]);
                },
                fixture,
                &collection.url,
            );
        }
    }
}

#[test]
fn non_secret_request_still_takes_store_host_row() {
    for (item, name) in [("items/2", "serverUrl"), ("items/3", "baseUrl")] {
        let what = format!("plain request via {name}");
        let collection = Host::spawn();
        let store = Host::spawn();
        let mut sandbox = Sandbox::new(&collection.url);
        let value = sandbox.store_host(name, &store);
        // A declared secret elsewhere in the store does not make this
        // request secret: it does not reference `token`.
        sandbox.env_set("token", TOKEN, true);

        let run = sandbox.request(item, &[], true);
        assert_eq!(run.code, Some(0), "{what}: {}", sandbox.scrub(&run.stdout));
        let value_json = sandbox.json(&what, &run);
        assert_eq!(
            value_json["lattice"]["secrets"],
            json!({ "hydrated": [name], "source": "plain" }),
            "{what}"
        );
        assert!(
            value_json["request"]["url"]
                .as_str()
                .unwrap_or_default()
                .starts_with(&value),
            "{what}: the store host is the request host"
        );
        let head = store.hit(&what);
        assert!(
            head.starts_with(&format!("GET /{STORE_MARK}/plain ")),
            "{what}: unexpected request line"
        );
        collection.untouched(&what);
        sandbox.assert_clean(&what, &run.stderr);
    }
}

#[test]
fn hosts_and_secrets_still_resolve_without_a_store_host_row() {
    // Plain request, no store row: the collection host, as before.
    let collection = Host::spawn();
    let sandbox = Sandbox::new(&collection.url);
    let run = sandbox.request("items/2", &[], true);
    assert_eq!(run.code, Some(0), "plain: {}", sandbox.scrub(&run.stdout));
    assert_eq!(
        sandbox.json("plain", &run)["lattice"]["secrets"],
        json!({ "hydrated": [], "source": null })
    );
    assert!(collection.hit("plain").starts_with("GET /plain "));

    // Secret request, token in the store, no host row: the token still
    // hydrates and the request goes to the collection host.
    for item in ["items/0", "items/1"] {
        let what = format!("secret {item} without a host row");
        let collection = Host::spawn();
        let sandbox = Sandbox::new(&collection.url);
        sandbox.env_set("token", TOKEN, true);
        let run = sandbox.request(item, &[], true);
        assert_eq!(run.code, Some(0), "{what}: {}", sandbox.scrub(&run.stdout));
        let value = sandbox.json(&what, &run);
        assert_eq!(
            value["lattice"]["secrets"],
            json!({ "hydrated": ["token"], "source": "encrypted" }),
            "{what}"
        );
        sandbox.assert_clean(&what, &value["lattice"].to_string());
        sandbox.assert_clean(&what, &run.stderr);
        let head = collection.hit(&what);
        assert!(head.starts_with("GET /secret "), "{what}");
        assert!(
            head.lines()
                .any(|line| line.eq_ignore_ascii_case(&format!("x-token: {TOKEN}"))),
            "{what}: the hydrated token is on the wire"
        );
    }
}
