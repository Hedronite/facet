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
//!
//! N2: a request carrying a `--var` for a declared secret gets the locked
//! transport (no redirects, no env proxy) even where hydration returns early:
//! no workspace, no `--environment`, or failed environment discovery.

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

use facet_record::overlay_secrets;
use probe_core::{Environment, EnvironmentVariable, Header, HttpRequest, SecretVariable};
use serde_json::{Value, json};

const STORE_MARK: &str = "h13-store-canary";
const TOKEN: &str = "h13-token-canary-not-a-credential";
const SECRET_KEY: &str = "h13-master-key-canary";
const USER_MARK: &str = "h13-user-canary";
const PASS_MARK: &str = "h13-pass-canary";
/// Never resolves: only a proxy can deliver a request for this host.
const PROXY_TARGET: &str = "http://n2-proxy-target.invalid";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
      Content-Length: 2\r\nConnection: close\r\n\r\n{}";

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
        Self::serve(address, OK.to_vec())
    }

    /// A host that answers every request `302` to `location`.
    fn redirect_to(location: &str) -> Self {
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        Self::serve("127.0.0.1:0", response.into_bytes())
    }

    fn serve(address: &str, response: Vec<u8>) -> Self {
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
                let _ = stream.write_all(&response);
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

    fn run(&self, arguments: &[&str]) -> Run {
        self.run_with(arguments, None, None)
    }

    /// [`Self::run`] with every proxy variable pointed at `proxy`, and the
    /// collection on stdin when `stdin` is given.
    fn run_with(&self, arguments: &[&str], proxy: Option<&Host>, stdin: Option<&str>) -> Run {
        let mut command = self.facet();
        command.args(arguments);
        if let Some(proxy) = proxy {
            for name in PROXY_VARS {
                command.env(name, &proxy.url);
            }
        }
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("facet should run");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().expect("facet should exit");
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

/// Where a `--var` secret request is missing what hydration needs.
#[derive(Clone, Copy, Debug)]
enum EarlyReturn {
    /// The collection comes from stdin, so there is no workspace root.
    NoWorkspace,
    /// No `--environment`.
    NoEnvironment,
    /// Discovery cannot run: a malformed reference, or an unknown environment.
    MalformedReference,
    UnknownEnvironment,
}

impl EarlyReturn {
    /// Runs the secret item (`items/0` redirect, `items/1` proxied) with the
    /// token as `--var`, shaped for this early return.
    fn run(self, sandbox: &Sandbox, item: usize, proxy: Option<&Host>) -> Run {
        let var = format!("token={TOKEN}");
        let item = match self {
            Self::MalformedReference => format!("items/{}", item + 4),
            _ => format!("items/{item}"),
        };
        let source;
        let (workspace, stdin) = match self {
            Self::NoWorkspace => {
                source = std::fs::read_to_string(&sandbox.workspace).unwrap();
                ("-", Some(source.as_str()))
            }
            _ => (sandbox.ws(), None),
        };
        let mut arguments = vec!["request", "run", workspace, item.as_str()];
        match self {
            Self::NoEnvironment => {}
            Self::UnknownEnvironment => arguments.extend(["--environment", "n2-absent"]),
            _ => arguments.extend(["--environment", "local"]),
        }
        arguments.extend(["--var", var.as_str(), "--json"]);
        sandbox.run_with(&arguments, proxy, stdin)
    }

    /// Whether the request can be built at all. Discovery and resolve parse
    /// the same strings, so a discovery failure also stops the send.
    fn sends(self) -> bool {
        matches!(self, Self::NoWorkspace | Self::NoEnvironment)
    }
}

const EARLY_RETURNS: [EarlyReturn; 4] = [
    EarlyReturn::NoWorkspace,
    EarlyReturn::NoEnvironment,
    EarlyReturn::MalformedReference,
    EarlyReturn::UnknownEnvironment,
];

/// What a `--var` secret run may show: the token stays out of stderr and out
/// of everything Lattice reports or keeps.
fn assert_var_secret_clean(sandbox: &Sandbox, what: &str, run: &Run) {
    sandbox.assert_clean(what, &run.stderr);
    if let Ok(value) = serde_json::from_str::<Value>(&run.stdout) {
        sandbox.assert_clean(what, &value["lattice"].to_string());
        sandbox.assert_clean(what, &value["error"].to_string());
    }
}

#[test]
fn var_secret_does_not_follow_a_redirect_on_any_early_return() {
    for early in EARLY_RETURNS {
        let what = format!("--var secret, {early:?}, redirect");
        let landing = Host::spawn_at("[::1]:0");
        let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
        let sandbox = Sandbox::with_fixture("secret_transport.yml", &redirect.url);

        let run = early.run(&sandbox, 0, None);
        assert_var_secret_clean(&sandbox, &what, &run);
        landing.untouched(&what);
        if early.sends() {
            assert_eq!(run.code, Some(0), "{what}: {}", sandbox.scrub(&run.stderr));
            let head = redirect.hit(&what);
            assert!(
                head.starts_with("GET /secret "),
                "{what}: unexpected request line"
            );
            let value = sandbox.json(&what, &run);
            assert_eq!(
                value["response"]["status"], 302,
                "{what}: the 302 is the answer"
            );
        } else {
            assert_ne!(
                run.code,
                Some(0),
                "{what}: discovery failure must not succeed"
            );
            redirect.untouched(&what);
        }
    }
}

#[test]
fn var_secret_does_not_use_an_env_proxy_on_any_early_return() {
    for early in EARLY_RETURNS {
        let what = format!("--var secret, {early:?}, env proxy");
        let proxy = Host::spawn();
        let sandbox = Sandbox::with_fixture("secret_transport.yml", "http://127.0.0.1:9");

        let run = early.run(&sandbox, 1, Some(&proxy));
        assert_var_secret_clean(&sandbox, &what, &run);
        proxy.untouched(&what);
        // Direct, the target never resolves; through the proxy it would be 200.
        assert_ne!(run.code, Some(0), "{what}: only a proxy could answer");
    }
}

#[test]
fn plain_requests_still_follow_redirects_and_use_an_env_proxy() {
    // The controls for the two tests above: the same hosts and proxy work.
    let landing = Host::spawn_at("[::1]:0");
    let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
    let sandbox = Sandbox::with_fixture("secret_transport.yml", &redirect.url);
    let run = sandbox.request("items/2", &[], true);
    assert_eq!(
        run.code,
        Some(0),
        "redirect: {}",
        sandbox.scrub(&run.stdout)
    );
    assert!(redirect.hit("redirect").starts_with("GET /plain "));
    assert!(landing.hit("redirect").starts_with("GET /landing "));

    let proxy = Host::spawn();
    let run = sandbox.run_with(
        &[
            "request",
            "run",
            sandbox.ws(),
            "items/3",
            "--environment",
            "local",
            "--json",
        ],
        Some(&proxy),
        None,
    );
    assert_eq!(run.code, Some(0), "proxy: {}", sandbox.scrub(&run.stdout));
    let head = proxy.hit("proxy");
    assert!(
        head.starts_with(&format!("GET {PROXY_TARGET}/plain ")),
        "proxy: unexpected request line"
    );
}

#[test]
fn overlay_marks_var_secret_requests_on_every_early_return() {
    let environments = [Environment {
        name: "local".to_owned(),
        color: None,
        extends: None,
        dot_env_file_path: None,
        variables: vec![EnvironmentVariable::Secret(SecretVariable {
            name: Some("token".to_owned()),
            value_type: None,
            disabled: false,
        })],
    }];
    let request = |url: &str| HttpRequest {
        url: Some(url.to_owned()),
        headers: vec![Header {
            name: "X-Token".to_owned(),
            value: "{{token}}".to_owned(),
            disabled: false,
        }],
        ..HttpRequest::default()
    };
    let good = request("http://127.0.0.1:9/secret");
    let malformed = request("http://127.0.0.1:9/secret?q={{}}");
    let var = [("token".to_owned(), TOKEN.to_owned())];
    // An empty directory: no store can be opened, and none is needed, since
    // every case below returns before the machine store.
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, Option<&Path>, Option<&str>, &HttpRequest); 4] = [
        ("no workspace", None, Some("local"), &good),
        ("no environment", Some(dir.path()), None, &good),
        (
            "malformed reference",
            Some(dir.path()),
            Some("local"),
            &malformed,
        ),
        (
            "unknown environment",
            Some(dir.path()),
            Some("n2-absent"),
            &good,
        ),
    ];
    for (what, root, environment, request) in cases {
        let hydration = overlay_secrets(root, environment, request, &environments, &var)
            .unwrap_or_else(|_| panic!("{what}: hydration should not fail"));
        assert!(
            hydration.references_secret,
            "{what}: a --var secret request must get the locked transport"
        );
        assert!(
            hydration.overrides.is_empty(),
            "{what}: nothing from Lattice"
        );
        assert!(
            !format!("{:?}", hydration.json()).contains(TOKEN),
            "{what}: names only"
        );
    }
}

#[test]
fn secret_request_refuses_scheme_port_ip_literal_and_scheme_relative_moves() {
    // Same host as the collection: only the scheme changes.
    let collection = Host::spawn();
    let authority = collection.url.strip_prefix("http://").unwrap().to_owned();
    let other_port = Host::spawn();
    let other_port_authority = other_port.url.strip_prefix("http://").unwrap().to_owned();
    let v6 = Host::spawn_at("[::1]:0");
    let v6_port = v6.url.rsplit(':').next().unwrap().to_owned();
    let cases = [
        ("scheme switch", format!("https://{authority}/{STORE_MARK}")),
        (
            "port change",
            format!("http://{other_port_authority}/{STORE_MARK}"),
        ),
        ("IPv6 literal", format!("{}/{STORE_MARK}", v6.url)),
        (
            "long-form IPv6 literal",
            format!("http://[0:0:0:0:0:0:0:1]:{v6_port}/{STORE_MARK}"),
        ),
    ];
    for (label, value) in &cases {
        for_each_token(
            |token, extra, sandbox| {
                let what = format!("apiUrl {label} with token from {token:?}");
                sandbox.store_value("apiUrl", value);
                sandbox.assert_refused(
                    &what,
                    "items/0",
                    extra,
                    "apiUrl",
                    &[&collection, &other_port, &v6],
                );
            },
            "host_override_names.yml",
            &collection.url,
        );
    }

    // The collection names its host; the store swaps in the IP literal it
    // resolves to. A different spelling of the authority is a different pin.
    let collection = Host::spawn();
    let port = collection.url.rsplit(':').next().unwrap().to_owned();
    let by_name = format!("http://localhost:{port}");
    for_each_token(
        |token, extra, sandbox| {
            let what = format!("IPv4 literal for localhost with token from {token:?}");
            sandbox.store_value("apiUrl", &format!("http://127.0.0.1:{port}/{STORE_MARK}"));
            sandbox.assert_refused(&what, "items/0", extra, "apiUrl", &[&collection]);
        },
        "host_override_names.yml",
        &by_name,
    );

    // `http:{{path}}`: a scheme-relative `//host` path is the authority.
    let collection = Host::spawn();
    let store = Host::spawn_at("[::1]:0");
    let store_authority = store.url.strip_prefix("http://").unwrap().to_owned();
    for_each_token(
        |token, extra, sandbox| {
            let what = format!("//host path with token from {token:?}");
            sandbox.store_value("path", &format!("//{store_authority}/{STORE_MARK}"));
            sandbox.assert_refused(&what, "items/5", extra, "path", &[&collection, &store]);
        },
        "host_override_names.yml",
        &collection.url,
    );

    // `{{apiUrl}}{{suffix}}`: a `//host` suffix after the authority is only
    // a path. It may be refused or sent to the collection host, never to
    // the host it spells.
    let collection = Host::spawn();
    let store = Host::spawn_at("[::1]:0");
    let store_authority = store.url.strip_prefix("http://").unwrap().to_owned();
    for_each_token(
        |token, extra, sandbox| {
            let what = format!("//host suffix with token from {token:?}");
            sandbox.store_value("suffix", &format!("//{store_authority}/{STORE_MARK}"));
            let run = sandbox.request("items/6", extra, true);
            sandbox.assert_clean(&what, &run.stderr);
            store.untouched(&what);
            while collection.requests.try_recv().is_ok() {}
        },
        "host_override_names.yml",
        &collection.url,
    );
}

/// FN-1: a store row with `secret: true` makes the request secret even
/// when the YAML does not declare the variable secret (`apiKey` is declared
/// plain, `ghost` not at all).
const STORE_SECRETS: [(&str, &str, &str); 2] = [
    ("apiKey", "items/0", "items/1"),
    ("ghost", "items/2", "items/3"),
];

#[test]
fn store_secret_row_locks_a_request_the_yaml_does_not_declare_secret() {
    for (name, redirect_item, proxied_item) in STORE_SECRETS {
        let what = format!("store secret {name}, redirect");
        let landing = Host::spawn_at("[::1]:0");
        let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
        let sandbox = Sandbox::with_fixture("store_secret.yml", &redirect.url);
        sandbox.env_set(name, TOKEN, true);
        let run = sandbox.request(redirect_item, &[], true);
        sandbox.assert_clean(&what, &run.stdout);
        sandbox.assert_clean(&what, &run.stderr);
        assert_eq!(run.code, Some(0), "{what}: {}", sandbox.scrub(&run.stderr));
        let head = redirect.hit(&what);
        assert!(
            head.lines()
                .any(|line| line.eq_ignore_ascii_case(&format!("x-key: {TOKEN}"))),
            "{what}: the store secret is hydrated and sent"
        );
        landing.untouched(&what);
        assert_eq!(
            sandbox.json(&what, &run)["response"]["status"],
            302,
            "{what}: the 302 is the answer"
        );

        let what = format!("store secret {name}, env proxy");
        let proxy = Host::spawn();
        let sandbox = Sandbox::with_fixture("store_secret.yml", "http://127.0.0.1:9");
        sandbox.env_set(name, TOKEN, true);
        let run = sandbox.run_with(
            &[
                "request",
                "run",
                sandbox.ws(),
                proxied_item,
                "--environment",
                "local",
                "--json",
            ],
            Some(&proxy),
            None,
        );
        sandbox.assert_clean(&what, &run.stdout);
        sandbox.assert_clean(&what, &run.stderr);
        proxy.untouched(&what);
        // Direct, the target never resolves; through the proxy it would be 200.
        assert_ne!(run.code, Some(0), "{what}: only a proxy could answer");
    }
}

#[test]
fn store_secret_row_makes_a_store_host_override_refused() {
    for (name, item, _) in STORE_SECRETS {
        let what = format!("store secret {name}, store serverUrl");
        let collection = Host::spawn();
        let store = Host::spawn_at("[::1]:0");
        let mut sandbox = Sandbox::with_fixture("store_secret.yml", &collection.url);
        sandbox.env_set(name, TOKEN, true);
        sandbox.store_host("serverUrl", &store);
        sandbox.assert_refused(&what, item, &[], "serverUrl", &[&store, &collection]);
    }
}

#[test]
fn unreferenced_store_secret_leaves_plain_requests_open() {
    // The controls for the two tests above: with the same store secret rows
    // present but not referenced, the same hosts and proxy are used.
    let landing = Host::spawn_at("[::1]:0");
    let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
    let sandbox = Sandbox::with_fixture("store_secret.yml", &redirect.url);
    sandbox.env_set("apiKey", TOKEN, true);
    sandbox.env_set("ghost", TOKEN, true);
    let run = sandbox.request("items/4", &[], true);
    assert_eq!(
        run.code,
        Some(0),
        "redirect: {}",
        sandbox.scrub(&run.stdout)
    );
    assert!(redirect.hit("redirect").starts_with("GET /plain "));
    assert!(landing.hit("redirect").starts_with("GET /landing "));

    let proxy = Host::spawn();
    let run = sandbox.run_with(
        &[
            "request",
            "run",
            sandbox.ws(),
            "items/5",
            "--environment",
            "local",
            "--json",
        ],
        Some(&proxy),
        None,
    );
    assert_eq!(run.code, Some(0), "proxy: {}", sandbox.scrub(&run.stdout));
    assert!(
        proxy
            .hit("proxy")
            .starts_with(&format!("GET {PROXY_TARGET}/plain ")),
        "proxy: unexpected request line"
    );

    // A store host for a plain request still applies.
    let collection = Host::spawn();
    let store = Host::spawn();
    let mut sandbox = Sandbox::with_fixture("store_secret.yml", &collection.url);
    sandbox.env_set("apiKey", TOKEN, true);
    sandbox.store_host("serverUrl", &store);
    let run = sandbox.request("items/4", &[], true);
    assert_eq!(
        run.code,
        Some(0),
        "store host: {}",
        sandbox.scrub(&run.stdout)
    );
    store.hit("store host");
    collection.untouched("store host");
}

/// The stderr lines that warn about a case-only secret name.
fn case_warnings(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.contains("differs only by case"))
        .collect()
}

#[test]
fn case_only_secret_name_warns_on_run_and_replay_without_changing_the_lock() {
    // `{{Token}}` against the declared secret `token`: one warning naming
    // both, no value, and the transport stays open (the redirect is
    // followed), as it was before the warning existed.
    let landing = Host::spawn_at("[::1]:0");
    let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
    let sandbox = Sandbox::with_fixture("case_secret.yml", &redirect.url);
    let var = format!("Token={TOKEN}");
    let run = sandbox.request("items/0", &["--var", &var], true);
    sandbox.assert_clean("case-only run", &run.stderr);
    assert_eq!(
        run.code,
        Some(0),
        "case-only run: {}",
        sandbox.scrub(&run.stderr)
    );
    let warnings = case_warnings(&run.stderr);
    assert_eq!(warnings.len(), 1, "case-only run: one warning");
    assert!(
        warnings[0].starts_with("warning: ")
            && warnings[0].contains("{{Token}}")
            && warnings[0].contains("token"),
        "case-only run: the warning names both variables"
    );
    assert!(redirect.hit("case-only run").starts_with("GET /case "));
    assert!(
        landing.hit("case-only run").starts_with("GET /landing "),
        "case-only run: the lock is unchanged, the redirect is followed"
    );

    let run_id = sandbox.json("case-only run", &run)["lattice"]["runId"]
        .as_str()
        .expect("recorded")
        .to_owned();
    let replay = sandbox.run(&["replay", &run_id, sandbox.ws(), "--var", &var, "--json"]);
    sandbox.assert_clean("case-only replay", &replay.stderr);
    assert_eq!(
        replay.code,
        Some(0),
        "case-only replay: {}",
        sandbox.scrub(&replay.stderr)
    );
    assert_eq!(
        case_warnings(&replay.stderr).len(),
        1,
        "case-only replay: one warning"
    );
    redirect.hit("case-only replay");
    landing.hit("case-only replay");

    // The exact name and an unrelated name do not warn.
    for (item, extra) in [
        (
            "items/1",
            vec!["--var".to_owned(), format!("token={TOKEN}")],
        ),
        ("items/2", Vec::new()),
    ] {
        let what = format!("{item} does not warn");
        let host = Host::spawn();
        let sandbox = Sandbox::with_fixture("case_secret.yml", &host.url);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        let run = sandbox.request(item, &extra, true);
        sandbox.assert_clean(&what, &run.stderr);
        assert_eq!(run.code, Some(0), "{what}: {}", sandbox.scrub(&run.stderr));
        host.hit(&what);
        assert!(case_warnings(&run.stderr).is_empty(), "{what}");
    }
}
