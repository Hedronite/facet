//! P1a G5 / FN-2: the `githubToken` Facet takes from `gh` goes only to
//! `https://api.github.com`.
//!
//! A collection that names `{{githubToken}}` for any other host fails
//! `github_token_host_refused` before `gh` runs and before anything is sent,
//! in `request run`, `--dry-run` and `replay`. The stub `gh` writes a marker
//! when it runs and prints a canary token, so "gh did not run", "nothing was
//! sent" and "the token never printed" are each observed directly. The
//! positive controls are a request with no `{{githubToken}}` and a
//! `--var githubToken=…` request, both of which reach the same loopback mock.
//!
//! These drive the shipped `facet` binary and touch no network: every URL
//! they send to is loopback, and every non-loopback URL is refused or only
//! previewed. Every value below is a canary, not a credential, and no
//! assertion message prints one.

#![cfg(unix)]

mod common;

use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use serde_json::{Value, json};

/// What the stub `gh auth token` prints.
const GH_TOKEN: &str = "fn2-gh-canary";
/// A user's own `--var githubToken=…`, sent as given.
const VAR_TOKEN: &str = "fn2-var-token";
const SECRET_KEY: &str = "fn2-master-key-canary";
const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];
/// `CONFIGURATION_EXIT_CODE`, as for `host_override_refused`.
const REFUSED_EXIT: i32 = 5;

/// A loopback host that answers 200 and keeps every request it received.
struct Mock {
    url: String,
    requests: Receiver<String>,
}

impl Mock {
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
                let request = read_head(&mut stream);
                if sender.send(request).is_err() {
                    return;
                }
                let body = "fn2-mock-body";
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        Self { url, requests }
    }

    /// The one request this host received. Never printed.
    fn hit(&self, what: &str) -> String {
        let request = self
            .requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what}: expected a request at the mock"));
        self.untouched(what);
        request
    }

    fn untouched(&self, what: &str) {
        assert!(
            self.requests.try_recv().is_err(),
            "{what}: a request reached the mock"
        );
    }
}

/// The request head, as text.
fn read_head(stream: &mut impl Read) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
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

/// A collection, a private machine store and config, and a stub `gh` that
/// leaves a marker, all under one temporary directory.
struct Sandbox {
    dir: tempfile::TempDir,
    workspace: PathBuf,
    marker: PathBuf,
}

impl Sandbox {
    fn new(collection_url: &str) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github_token.yml");
        let source = std::fs::read_to_string(fixture).expect("fixture");
        let workspace = dir.path().join("workspace.yml");
        std::fs::write(
            &workspace,
            source.replace("__COLLECTION_URL__", collection_url),
        )
        .unwrap();
        let marker = dir.path().join("gh-ran");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\n: > '{}'\nprintf '%s\\n' '{GH_TOKEN}'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            dir,
            workspace,
            marker,
        }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    fn run(&self, arguments: &[&str]) -> Run {
        let root = self.dir.path();
        let mut command = Command::new(common::bins::shipped("facet"));
        command
            .args(arguments)
            .env("FACET_GH", root.join("bin").join("gh"))
            .env_remove("FACET_GIT")
            .env_remove("FACET_NO_SUBPROCESS")
            .env("FACET_DATA_DIR", root.join("machine"))
            .env("FACET_CONFIG_DIR", root.join("config"))
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
            .stdin(Stdio::null())
            .current_dir(root);
        for name in PROXY_VARS {
            command.env_remove(name);
        }
        let output = command.output().expect("facet should run");
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn request(&self, item: &str, extra: &[&str]) -> Run {
        let mut arguments = vec!["request", "run", self.ws(), item];
        arguments.extend_from_slice(extra);
        self.run(&arguments)
    }

    fn gh_ran(&self) -> bool {
        self.marker.exists()
    }

    /// The refusal oracle: exit 5, `github_token_host_refused` (with
    /// `details.host` when `host` is given) in JSON or `error[...]` on
    /// stderr, `gh` never ran, and the canary is in neither stream.
    fn assert_refused(&self, what: &str, run: &Run, json_output: bool, host: Option<&str>) {
        assert_no_canary(what, run);
        assert!(!self.gh_ran(), "{what}: gh ran");
        assert_eq!(run.code, Some(REFUSED_EXIT), "{what}: exit");
        if json_output {
            let document = json(what, run);
            let error = &document["error"];
            assert_eq!(error["category"], "github_token_host_refused", "{what}");
            assert_eq!(error["exitCode"], REFUSED_EXIT, "{what}");
            if let Some(host) = host {
                assert_eq!(error["details"]["host"], host, "{what}: details.host");
                assert!(
                    error["message"].as_str().unwrap_or_default().contains(host),
                    "{what}: the message names the host"
                );
            }
        } else {
            assert!(
                run.stderr.contains("github_token_host_refused"),
                "{what}: error line"
            );
        }
    }
}

fn json(what: &str, run: &Run) -> Value {
    serde_json::from_str(&run.stdout)
        .unwrap_or_else(|error| panic!("{what}: stdout should be JSON ({error})"))
}

/// The `gh` canary is in neither stream. Reports where, not what.
fn assert_no_canary(what: &str, run: &Run) {
    for (stream, text) in [("stdout", &run.stdout), ("stderr", &run.stderr)] {
        if let Some(offset) = text.find(GH_TOKEN) {
            panic!("{what}: the gh canary is in {stream} at byte {offset}");
        }
    }
}

#[test]
fn gh_token_for_a_loopback_host_is_refused_before_gh_runs() {
    let mock = Mock::spawn();
    let sandbox = Sandbox::new(&mock.url);
    for environment in [&[][..], &["--environment", "local"][..]] {
        for dry_run in [false, true] {
            for json_output in [false, true] {
                let what = format!(
                    "run items/0, environment {}, dry-run {dry_run}, json {json_output}",
                    !environment.is_empty()
                );
                let mut extra = environment.to_vec();
                if dry_run {
                    extra.push("--dry-run");
                }
                if json_output {
                    extra.push("--json");
                }
                let run = sandbox.request("items/0", &extra);
                sandbox.assert_refused(&what, &run, json_output, Some("127.0.0.1"));
                mock.untouched(&what);
            }
        }
    }
}

#[test]
fn gh_token_never_goes_to_a_host_that_only_looks_like_github() {
    // Previewed only: even a regression here would send nothing.
    let mock = Mock::spawn();
    let sandbox = Sandbox::new(&mock.url);
    for (item, host) in [
        ("items/2", None),
        ("items/3", Some("api.github.com")),
        ("items/4", Some("api.github.com.invalid")),
    ] {
        let what = format!("dry-run {item}");
        let run = sandbox.request(item, &["--dry-run", "--json"]);
        sandbox.assert_refused(&what, &run, true, host);
        let run = sandbox.request(item, &["--dry-run"]);
        sandbox.assert_refused(&what, &run, false, None);
    }
    mock.untouched("look-alike hosts");
}

#[test]
fn requests_without_the_gh_token_reach_the_mock() {
    let mock = Mock::spawn();
    let sandbox = Sandbox::new(&mock.url);

    // No `{{githubToken}}`: sent, and gh never asked.
    let run = sandbox.request("items/1", &["--json"]);
    assert_no_canary("control", &run);
    assert_eq!(run.code, Some(0), "control: exit");
    assert!(
        mock.hit("control").starts_with("GET /plain "),
        "control path"
    );
    assert!(run.stdout.contains("fn2-mock-body"), "control: body prints");
    assert!(!sandbox.gh_ran(), "control: gh ran");

    // The user's own `--var githubToken=…` goes where the YAML says, as
    // before: no gh, no host check.
    let var = format!("githubToken={VAR_TOKEN}");
    let run = sandbox.request("items/0", &["--var", &var, "--json"]);
    assert_no_canary("--var", &run);
    assert_eq!(run.code, Some(0), "--var: exit");
    let request = mock.hit("--var");
    assert!(request.starts_with("GET /gh "), "--var path");
    assert!(
        request.contains(VAR_TOKEN),
        "--var: the user's value is sent"
    );
    assert!(!sandbox.gh_ran(), "--var: gh ran");
}

#[test]
fn replay_of_a_gh_token_request_is_refused_before_gh_runs() {
    let mock = Mock::spawn();
    let sandbox = Sandbox::new(&mock.url);
    let run_id = |what: &str, run: &Run| {
        json(what, run)["lattice"]["runId"]
            .as_str()
            .unwrap_or_else(|| panic!("{what}: the run is recorded"))
            .to_owned()
    };

    // Recorded with the user's own token, which replay does not keep: the
    // replay asks gh, and the loopback host refuses it.
    let var = format!("githubToken={VAR_TOKEN}");
    let recorded = sandbox.request("items/0", &["--var", &var, "--json"]);
    assert_eq!(recorded.code, Some(0), "recorded run: exit");
    mock.hit("recorded run");
    let token_run = run_id("recorded run", &recorded);
    for json_output in [false, true] {
        let what = format!("replay items/0, json {json_output}");
        let mut arguments = vec!["replay", token_run.as_str(), sandbox.ws()];
        if json_output {
            arguments.push("--json");
        }
        let replay = sandbox.run(&arguments);
        sandbox.assert_refused(&what, &replay, json_output, Some("127.0.0.1"));
        if json_output {
            assert_eq!(
                json(&what, &replay)["error"]["details"]["replayedFrom"],
                json!(token_run),
                "{what}: lineage"
            );
        }
        mock.untouched(&what);
    }

    // Control: replay of the request with no `{{githubToken}}` is sent.
    let control = sandbox.request("items/1", &["--json"]);
    assert_eq!(control.code, Some(0), "control run: exit");
    mock.hit("control run");
    let control_run = run_id("control run", &control);
    let replay = sandbox.run(&["replay", control_run.as_str(), sandbox.ws(), "--json"]);
    assert_no_canary("control replay", &replay);
    assert_eq!(replay.code, Some(0), "control replay: exit");
    assert!(mock.hit("control replay").starts_with("GET /plain "));
    assert!(!sandbox.gh_ran(), "control replay: gh ran");
}
