//! P1a G1 / N2b: one transport-lock predicate decides, for `facet` and
//! `probe` alike, whether a send is locked (no redirects, no env proxy).
//!
//! | Case | Transport |
//! |---|---|
//! | Discovery fails (malformed reference, unknown environment) | locked |
//! | Environment selected, a referenced variable is a declared secret | locked |
//! | No environment, the request references a supplied `--var` | locked |
//! | No environment, the request references no supplied variable | open |
//! | Environment selected, no referenced variable is a declared secret | open |
//!
//! Every row drives the shipped `facet` and `probe` binaries twice: once at
//! a host that answers `302` (locked returns the 302, open follows it) and
//! once at a host only an env proxy can reach (locked fails direct, open
//! reaches the proxy). Rows that need no machine store also run with the
//! collection on stdin, since a workspace does not change the table.
//!
//! Every value below is a canary, not a credential, and no assertion message
//! prints one.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        OnceLock,
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};

use probe_core::{
    Environment, EnvironmentVariable, Header, HttpRequest, SecretVariable, TransportLock,
    transport_lock,
};
use serde_json::Value;

const TOKEN: &str = "n2b-token-canary-not-a-credential";
const SECRET_KEY: &str = "n2b-master-key-canary";
/// A plain value for the plain `tag` variable. Not a secret, so it may show.
const TAG: &str = "n2b-tag-plain";
/// Never resolves: only a proxy can deliver a request for this host.
const PROXY_TARGET: &str = "http://n2b-proxy-target.invalid";
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

/// `bin` from `package`, built from this workspace into the target dir and
/// profile running this test. `cargo build` is a no-op when it is fresh and
/// rebuilds when the predicate or a call site changed, so it is never stale.
fn build(package: &str, bin: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("test executable path");
    // <target>/[<triple>/]<profile>/deps/transport_lock-<hash>
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
        .args(["build", "--locked", "-p", package, "--bin", bin])
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
        "cargo build -p {package} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| {
            message["reason"] == "compiler-artifact" && message["target"]["name"] == bin
        })
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| panic!("cargo should report the {bin} executable"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Bin {
    Facet,
    Probe,
}

impl Bin {
    fn path(self) -> &'static Path {
        static FACET: OnceLock<PathBuf> = OnceLock::new();
        static PROBE: OnceLock<PathBuf> = OnceLock::new();
        match self {
            Self::Facet => FACET.get_or_init(|| build("facet-cli", "facet")),
            Self::Probe => PROBE.get_or_init(|| build("probe-cli", "probe")),
        }
    }
}

/// A loopback HTTP host. Each request it receives gets `response` and its
/// head is sent to `requests`, so a test can prove a hit or its absence.
struct Host {
    url: String,
    requests: Receiver<String>,
}

impl Host {
    fn spawn() -> Self {
        Self::serve("127.0.0.1:0", OK.to_vec())
    }

    /// A host on another loopback address, so a redirect there changes host.
    fn spawn_v6() -> Self {
        Self::serve("[::1]:0", OK.to_vec())
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

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The collection and a private machine store and config, all under one
/// temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    workspace: PathBuf,
}

impl Sandbox {
    /// `tests/fixtures/transport_lock.yml` with `__COLLECTION_URL__` filled in.
    fn new(collection_url: &str) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transport_lock.yml");
        let source = std::fs::read_to_string(path).expect("fixture");
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

    /// `arguments` under `bin`, with every proxy variable pointed at `proxy`
    /// when given and cleared otherwise, and `stdin` piped in when given.
    fn run(&self, bin: Bin, arguments: &[&str], proxy: Option<&Host>, stdin: Option<&str>) -> Run {
        let root = self.dir.path();
        let mut command = Command::new(bin.path());
        command.args(arguments).current_dir(root);
        if bin == Bin::Facet {
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
                .env_remove("HERDR_PANE_ID");
        }
        command.env_remove("NO_PROXY").env_remove("no_proxy");
        for name in PROXY_VARS {
            match proxy {
                Some(proxy) => command.env(name, &proxy.url),
                None => command.env_remove(name),
            };
        }
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("binary should run");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().expect("binary should exit");
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// A `facet env set --secret` row for `token` in `local`.
    fn store_token(&self) {
        let run = self.run(
            Bin::Facet,
            &[
                "env",
                "set",
                self.ws(),
                "--environment",
                "local",
                "--name",
                "token",
                "--value",
                TOKEN,
                "--secret",
                "--json",
            ],
            None,
            None,
        );
        assert_eq!(run.code, Some(0), "env set token: {}", scrub(&run.stderr));
    }
}

/// `text` with every canary replaced, safe to put in a failure message.
fn scrub(text: &str) -> String {
    [TOKEN, SECRET_KEY]
        .iter()
        .fold(text.to_owned(), |text, canary| {
            text.replace(canary, "<canary>")
        })
}

fn assert_clean(what: &str, text: &str) {
    for canary in [TOKEN, SECRET_KEY] {
        assert!(
            !text.contains(canary),
            "{what}: a canary value is in the output:\n{}",
            scrub(text)
        );
    }
}

/// The token stays out of stderr and out of everything Lattice or an error
/// reports. The live document's request headers are G2's to redact.
fn assert_run_clean(what: &str, run: &Run) {
    assert_clean(what, &run.stderr);
    if let Ok(value) = serde_json::from_str::<Value>(&run.stdout) {
        assert_clean(what, &value["lattice"].to_string());
        assert_clean(what, &value["error"].to_string());
    }
}

fn status(what: &str, run: &Run) -> Value {
    let value: Value = serde_json::from_str(&run.stdout).unwrap_or_else(|error| {
        panic!(
            "{what}: stdout should be JSON ({error}), exit {:?}\nstdout: {}\nstderr: {}",
            run.code,
            scrub(&run.stdout),
            scrub(&run.stderr)
        )
    });
    value["response"]["status"].clone()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Lock {
    /// Sent with `Policy::none()` and `no_proxy()`.
    Locked,
    /// Sent with redirects followed and env proxies honored.
    Open,
    /// Refused before anything is built or sent.
    NotSent,
}

#[derive(Clone, Copy, Debug)]
enum Input {
    File,
    Stdin,
}

#[derive(Clone, Copy, Debug)]
enum Var {
    /// `--var token=…`: a declared secret in `local`.
    Token,
    /// `--var tag=…`: a plain variable in `local`.
    Tag,
}

/// One row of the table: a fixture item and how it is run.
struct Case {
    what: &'static str,
    /// `items/0` plain, `items/1` `{{token}}`, `items/2` `{{tag}}`,
    /// `items/3` `{{}}`.
    item: usize,
    environment: Option<&'static str>,
    var: Option<Var>,
    /// The token comes from the machine store. Facet only, with a workspace.
    store_token: bool,
}

impl Case {
    const fn new(what: &'static str, item: usize) -> Self {
        Self {
            what,
            item,
            environment: None,
            var: None,
            store_token: false,
        }
    }

    const fn env(mut self, environment: &'static str) -> Self {
        self.environment = Some(environment);
        self
    }

    const fn var(mut self, var: Var) -> Self {
        self.var = Some(var);
        self
    }

    const fn store_token(mut self) -> Self {
        self.store_token = true;
        self
    }

    fn path(&self) -> &'static str {
        ["plain", "secret", "tagged", "malformed"][self.item]
    }

    fn run(&self, bin: Bin, input: Input, sandbox: &Sandbox, proxy: Option<&Host>) -> Run {
        let item = format!("items/{}", self.item);
        let var = match self.var {
            Some(Var::Token) => Some(format!("token={TOKEN}")),
            Some(Var::Tag) => Some(format!("tag={TAG}")),
            None => None,
        };
        let source;
        let (workspace, stdin) = match input {
            Input::File => (sandbox.ws(), None),
            Input::Stdin => {
                source = std::fs::read_to_string(&sandbox.workspace).unwrap();
                ("-", Some(source.as_str()))
            }
        };
        let mut arguments = vec!["request", "run", workspace, item.as_str()];
        if let Some(environment) = self.environment {
            arguments.extend(["--environment", environment]);
        }
        if let Some(var) = &var {
            arguments.extend(["--var", var.as_str()]);
        }
        arguments.push("--json");
        sandbox.run(bin, &arguments, proxy, stdin)
    }
}

/// Runs `case` through both binaries, from a file and from stdin, against a
/// redirect and an env proxy, and checks each run against `lock`.
fn check(case: &Case, lock: Lock) {
    let bins: &[Bin] = if case.store_token {
        &[Bin::Facet]
    } else {
        &[Bin::Facet, Bin::Probe]
    };
    let inputs: &[Input] = if case.store_token {
        &[Input::File]
    } else {
        &[Input::File, Input::Stdin]
    };
    for &bin in bins {
        for &input in inputs {
            check_redirect(case, lock, bin, input);
            check_proxy(case, lock, bin, input);
        }
    }
}

fn check_redirect(case: &Case, lock: Lock, bin: Bin, input: Input) {
    let what = format!("{} [{bin:?}, {input:?}, redirect]", case.what);
    let landing = Host::spawn_v6();
    let redirect = Host::redirect_to(&format!("{}/landing", landing.url));
    let sandbox = Sandbox::new(&redirect.url);
    if case.store_token {
        sandbox.store_token();
    }

    let run = case.run(bin, input, &sandbox, None);
    assert_run_clean(&what, &run);
    match lock {
        Lock::NotSent => {
            assert_ne!(run.code, Some(0), "{what}: must not succeed");
            redirect.untouched(&what);
            landing.untouched(&what);
        }
        Lock::Locked | Lock::Open => {
            assert_eq!(run.code, Some(0), "{what}: {}", scrub(&run.stdout));
            let head = redirect.hit(&what);
            assert!(
                head.starts_with(&format!("GET /{}", case.path())),
                "{what}: unexpected request line"
            );
            if lock == Lock::Locked {
                landing.untouched(&what);
                assert_eq!(status(&what, &run), 302, "{what}: the 302 is the answer");
            } else {
                assert!(
                    landing.hit(&what).starts_with("GET /landing "),
                    "{what}: the redirect is followed"
                );
                assert_eq!(status(&what, &run), 200, "{what}: the landing answers");
            }
        }
    }
}

fn check_proxy(case: &Case, lock: Lock, bin: Bin, input: Input) {
    let what = format!("{} [{bin:?}, {input:?}, env proxy]", case.what);
    let proxy = Host::spawn();
    let sandbox = Sandbox::new(PROXY_TARGET);
    if case.store_token {
        sandbox.store_token();
    }

    let run = case.run(bin, input, &sandbox, Some(&proxy));
    assert_run_clean(&what, &run);
    if lock == Lock::Open {
        assert_eq!(run.code, Some(0), "{what}: {}", scrub(&run.stdout));
        let head = proxy.hit(&what);
        assert!(
            head.starts_with(&format!("GET {PROXY_TARGET}/{} ", case.path())),
            "{what}: unexpected request line"
        );
        assert_eq!(status(&what, &run), 200, "{what}: the proxy answers");
    } else {
        proxy.untouched(&what);
        // Direct, the target never resolves; through the proxy it would be 200.
        assert_ne!(run.code, Some(0), "{what}: only a proxy could answer");
    }
}

#[test]
fn discovery_failure_locks_transport() {
    // No environment and no `--var`: nothing resolves the request, so the
    // malformed reference goes out as written, and only the failed
    // discovery locks it. The item references no secret.
    check(&Case::new("malformed reference", 3), Lock::Locked);
    // Discovery and resolve parse the same strings, so wherever resolve runs
    // the same failure also stops the send.
    check(
        &Case::new("malformed reference, environment selected", 3).env("local"),
        Lock::NotSent,
    );
    check(
        &Case::new("unknown environment", 0).env("n2b-absent"),
        Lock::NotSent,
    );
    check(
        &Case::new("unknown environment, secret item", 1)
            .env("n2b-absent")
            .var(Var::Token),
        Lock::NotSent,
    );
}

/// The predicate itself, so a discovery failure cannot hide behind another
/// locked branch: the binaries above would stay green if it did.
#[test]
fn discovery_failure_is_its_own_transport_lock_branch() {
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
    let request = |url: &str, token: bool| HttpRequest {
        url: Some(format!("http://127.0.0.1:9{url}")),
        headers: if token {
            vec![Header {
                name: "X-Token".to_owned(),
                value: "{{token}}".to_owned(),
                disabled: false,
            }]
        } else {
            Vec::new()
        },
        ..HttpRequest::default()
    };
    let plain = request("/plain", false);
    let secret = request("/secret", true);
    let malformed = request("/malformed?q={{}}", false);
    let malformed_secret = request("/malformed?q={{}}", true);
    // (what, request, selected environment, `--var token` supplied, lock)
    let cases = [
        (
            "malformed reference, no environment",
            &malformed,
            None,
            false,
            TransportLock::DiscoveryFailed,
        ),
        (
            "malformed reference, environment selected",
            &malformed,
            Some("local"),
            false,
            TransportLock::DiscoveryFailed,
        ),
        // A supplied `--var` would make this `VarUnclassified` if discovery
        // had run: the failure must win.
        (
            "malformed reference, no environment, --var token",
            &malformed_secret,
            None,
            true,
            TransportLock::DiscoveryFailed,
        ),
        (
            "unknown environment",
            &plain,
            Some("n2b-absent"),
            false,
            TransportLock::DiscoveryFailed,
        ),
        (
            "unknown environment, --var token",
            &secret,
            Some("n2b-absent"),
            true,
            TransportLock::DiscoveryFailed,
        ),
        (
            "plain, no environment",
            &plain,
            None,
            false,
            TransportLock::Open,
        ),
        (
            "--var token, no environment",
            &secret,
            None,
            true,
            TransportLock::VarUnclassified,
        ),
        (
            "secret reference, environment selected",
            &secret,
            Some("local"),
            false,
            TransportLock::SecretReferenced,
        ),
        (
            "plain, environment selected",
            &plain,
            Some("local"),
            true,
            TransportLock::Open,
        ),
    ];
    for (what, request, selected, supplied, expected) in cases {
        let lock = transport_lock(request, &environments, selected, |name| {
            supplied && name == "token"
        });
        assert_eq!(lock, expected, "{what}");
        assert_eq!(
            lock.is_locked(),
            expected != TransportLock::Open,
            "{what}: is_locked"
        );
    }
    assert!(TransportLock::DiscoveryFailed.is_locked());
    assert!(!TransportLock::Open.is_locked());
}

#[test]
fn referenced_declared_secret_with_an_environment_locks_transport() {
    check(
        &Case::new("secret from --var", 1)
            .env("local")
            .var(Var::Token),
        Lock::Locked,
    );
    check(
        &Case::new("secret from the store", 1)
            .env("local")
            .store_token(),
        Lock::Locked,
    );
}

#[test]
fn supplied_var_without_an_environment_locks_transport() {
    // With no environment there are no declarations, so a supplied value is
    // unclassified, whether some environment calls it a secret or not.
    check(&Case::new("--var token", 1).var(Var::Token), Lock::Locked);
    check(&Case::new("--var tag", 2).var(Var::Tag), Lock::Locked);
}

#[test]
fn plain_request_without_an_environment_follows_redirects_and_uses_an_env_proxy() {
    check(&Case::new("plain", 0), Lock::Open);
    // A `--var` the request does not reference supplies nothing to it.
    check(
        &Case::new("plain, unreferenced --var", 0).var(Var::Tag),
        Lock::Open,
    );
}

#[test]
fn plain_request_with_an_environment_follows_redirects_and_uses_an_env_proxy() {
    check(&Case::new("plain", 0).env("local"), Lock::Open);
    check(
        &Case::new("plain variable from the environment", 2).env("local"),
        Lock::Open,
    );
    check(
        &Case::new("plain variable from --var", 2)
            .env("local")
            .var(Var::Tag),
        Lock::Open,
    );
    // A declared secret in the store that this request does not reference.
    check(
        &Case::new("plain, unreferenced secret in the store", 0)
            .env("local")
            .store_token(),
        Lock::Open,
    );
}
