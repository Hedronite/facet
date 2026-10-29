//! P1b G1 / H1: every `gh` and `git` Facet starts goes through
//! `facet_record::child`.
//!
//! - The program comes from `FACET_GH` / `FACET_GIT` or a fixed list, never
//!   from `PATH`, and a relative or missing override is refused unechoed.
//! - The child env is `HOME`, `GH_CONFIG_DIR` (each when set) and
//!   `PATH=/usr/bin:/bin`, nothing else.
//! - `--no-subprocess` and `FACET_NO_SUBPROCESS` stop every spawn on run,
//!   replay and a recorded run. `{{githubToken}}` then fails
//!   `subprocess_disabled` before the send, after the host check.
//! - `gh api` never carries a locked send or a sensitive header.
//!
//! The stubs are Perl, which adds nothing to the environment it is given,
//! so a dump is exactly the child's env. Each appends its argv and env to a
//! file and answers as `gh` or `git` would. No test touches the network:
//! sends go to loopback, `--dry-run` sends nothing, and an empty
//! `FACET_KUBECONFIG` fails the native engine before it connects, which is
//! how the fallback cases reach `gh api`. Every positive control spawns the
//! same stub its refusal does not. Every value below is a canary, not a
//! credential, and no assertion message prints one.

#![cfg(unix)]

#[allow(dead_code, unused_imports)]
mod common;

use std::os::unix::fs::PermissionsExt;

use common::*;

/// What the stub `gh auth token` prints.
const GH_TOKEN: &str = "h1-gh-canary";
/// Set in the parent under every name in [`PARENT_ENV`]; no child sees it.
const PARENT_CANARY: &str = "h1-parent-env-canary";
/// A sensitive header's value, which `gh api` must never get on argv.
const HEADER_CANARY: &str = "h1-header-canary";
/// A `--var` value that makes the send a locked one.
const VAR_CANARY: &str = "h1-var-canary";
/// What the stub `git rev-parse HEAD` prints.
const STUB_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
/// `CONFIGURATION_EXIT_CODE`.
const REFUSED_EXIT: i32 = 5;
/// The only keys a child may see.
const CHILD_ENV: [&str; 3] = ["GH_CONFIG_DIR", "HOME", "PATH"];
/// Set in the parent for the env check. Proxy names are only set where
/// nothing is sent.
const PARENT_ENV: [&str; 8] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "FACET_H1_CANARY",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "SSH_AUTH_SOCK",
    "GIT_DIR",
];
/// Loader variables, set in the parent but empty: glibc's loader would try
/// to preload a canary path into `facet` itself and print it on stderr, while
/// an empty value is ignored. The child env check catches the name alone.
const LOADER_ENV: [&str; 1] = ["LD_PRELOAD"];

const COLLECTION: &str = r#"opencollection: 1.0.0
info: { name: H1 child processes }
bundled: true
items:
  - info: { name: Token, type: http, seq: 1 }
    http:
      method: GET
      url: https://api.github.com/user
      headers:
        - name: Authorization
          value: Bearer {{githubToken}}
  - info: { name: Elsewhere, type: http, seq: 2 }
    http:
      method: GET
      url: https://api.github.com.invalid/user
      headers:
        - name: Authorization
          value: Bearer {{githubToken}}
  - info: { name: Fallback, type: http, seq: 3 }
    http:
      method: GET
      url: https://api.github.com/repos/example/project
      headers:
        - name: X-Api-Key
          value: __HEADER_CANARY__
        - name: X-Plain
          value: h1-plain
  - info: { name: Locked fallback, type: http, seq: 4 }
    http:
      method: GET
      url: https://api.github.com/repos/example/project
      headers:
        - name: X-Token
          value: "{{token}}"
"#;

/// One stub run: its argv and its whole environment.
struct Call {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

/// A Perl stub for `gh` or `git` that appends each call to its dump file.
struct Stub {
    path: PathBuf,
    dump: PathBuf,
}

impl Stub {
    fn new(dir: &Path, name: &str) -> Self {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let dump = dir.join(format!("{name}.dump"));
        let script = format!(
            r#"#!/usr/bin/perl
open(my $dump, '>>', '{dump}') or exit 97;
print $dump join("\t", 'argv', @ARGV), "\n";
print $dump "env\t$_=$ENV{{$_}}\n" for sort keys %ENV;
close($dump);
if (@ARGV && $ARGV[0] eq 'auth') {{ print "{GH_TOKEN}\n"; exit 0; }}
if (@ARGV && $ARGV[0] eq 'api') {{ print '{{"source":"h1-stub-gh"}}'; exit 0; }}
if (grep {{ $_ eq 'rev-parse' }} @ARGV) {{ print "{STUB_SHA}\n"; }}
exit 0;
"#,
            dump = dump.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Self { path, dump }
    }

    fn calls(&self) -> Vec<Call> {
        let text = fs::read_to_string(&self.dump).unwrap_or_default();
        let mut calls: Vec<Call> = Vec::new();
        for line in text.lines() {
            let mut fields = line.split('\t');
            match fields.next() {
                Some("argv") => calls.push(Call {
                    argv: fields.map(str::to_owned).collect(),
                    env: Vec::new(),
                }),
                Some("env") => {
                    let pair = fields.collect::<Vec<_>>().join("\t");
                    let (key, value) = pair.split_once('=').unwrap_or((&pair, ""));
                    calls
                        .last_mut()
                        .expect("env line after argv")
                        .env
                        .push((key.to_owned(), value.to_owned()));
                }
                _ => panic!("unexpected stub dump line"),
            }
        }
        calls
    }

    fn ran(&self) -> bool {
        !self.calls().is_empty()
    }

    fn dump_contains(&self, needle: &str) -> bool {
        fs::read_to_string(&self.dump).is_ok_and(|text| text.contains(needle))
    }
}

/// The sandbox, the GitHub collection, a stub at `FACET_GH`, a stub at
/// `FACET_GIT`, and a decoy `gh` and `git` first on `PATH`.
struct Fx {
    sandbox: Sandbox,
    workspace: PathBuf,
    gh: Stub,
    git: Stub,
    decoy_dir: PathBuf,
    decoy_gh: Stub,
    decoy_git: Stub,
}

impl Fx {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let workspace = sandbox.root().join("github.yml");
        fs::write(
            &workspace,
            COLLECTION.replace("__HEADER_CANARY__", HEADER_CANARY),
        )
        .unwrap();
        let stubs = sandbox.root().join("stubs");
        let decoy_dir = sandbox.root().join("decoy");
        Self {
            gh: Stub::new(&stubs, "gh"),
            git: Stub::new(&stubs, "git"),
            decoy_gh: Stub::new(&decoy_dir, "gh"),
            decoy_git: Stub::new(&decoy_dir, "git"),
            decoy_dir,
            workspace,
            sandbox,
        }
    }

    fn ws(&self) -> &str {
        self.workspace.to_str().unwrap()
    }

    /// `facet` with both stubs set, the decoys first on `PATH`, and an empty
    /// `FACET_KUBECONFIG`, so no send leaves the machine whatever the code
    /// under test does.
    fn facet(&self) -> Command {
        let mut command = self.sandbox.facet();
        command
            .env("FACET_GH", &self.gh.path)
            .env("FACET_GIT", &self.git.path)
            .env("FACET_KUBECONFIG", "")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.decoy_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        command
    }

    /// [`Self::facet`] for a loopback send: the native engine is left alone.
    fn facet_local(&self) -> Command {
        let mut command = self.facet();
        command.env_remove("FACET_KUBECONFIG");
        command
    }

    fn run(&self, command: &mut Command) -> Run {
        let output = command.arg("--json").output().expect("facet should run");
        Run {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn request(&self, item: &str, extra: &[&str]) -> Run {
        let mut command = self.facet();
        command
            .args(["request", "run", self.ws(), item])
            .args(extra);
        self.run(&mut command)
    }

    fn assert_no_decoy(&self, what: &str) {
        assert!(!self.decoy_gh.ran(), "{what}: the gh on PATH ran");
        assert!(!self.decoy_git.ran(), "{what}: the git on PATH ran");
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn category(&self) -> String {
        let envelope: Value = serde_json::from_str(&self.stdout)
            .unwrap_or_else(|_| panic!("stdout is not JSON (exit {:?})", self.code));
        envelope["error"]["category"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn assert_refused(&self, what: &str, category: &str) {
        assert_eq!(self.code, Some(REFUSED_EXIT), "{what}: {}", self.stderr);
        assert_eq!(self.category(), category, "{what}");
    }

    fn mentions(&self, needle: &str) -> bool {
        self.stdout.contains(needle) || self.stderr.contains(needle)
    }
}

/// Every call's env holds only [`CHILD_ENV`] keys, `PATH` is fixed, and no
/// parent canary is in any key, value or argument.
fn assert_child_env(what: &str, calls: &[Call]) {
    assert!(!calls.is_empty(), "{what}: no call to check");
    for call in calls {
        for (key, value) in &call.env {
            assert!(
                CHILD_ENV.contains(&key.as_str()),
                "{what}: child env has {key}"
            );
            assert!(
                !value.contains(PARENT_CANARY),
                "{what}: a parent canary is in child env {key}"
            );
        }
        let path = call.env.iter().find(|(key, _)| key == "PATH");
        assert_eq!(
            path.map(|(_, value)| value.as_str()),
            Some("/usr/bin:/bin"),
            "{what}: child PATH"
        );
        assert!(
            !call.argv.iter().any(|arg| arg.contains(PARENT_CANARY)),
            "{what}: a parent canary is on argv"
        );
    }
}

fn env_of<'a>(call: &'a Call, key: &str) -> Option<&'a str> {
    call.env
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn run_id(run: &Run) -> String {
    let value: Value = serde_json::from_str(&run.stdout).expect("run JSON");
    value["lattice"]["runId"]
        .as_str()
        .expect("recorded run id")
        .to_owned()
}

fn tags(sandbox: &Sandbox, run_id: &str) -> Vec<String> {
    let db = rusqlite::Connection::open(sandbox.root().join(".facet/lattice.db")).unwrap();
    let tags: Option<String> = db
        .query_row("SELECT tags FROM runs WHERE id = ?1", [run_id], |row| {
            row.get(0)
        })
        .unwrap();
    tags.map(|text| serde_json::from_str(&text).unwrap())
        .unwrap_or_default()
}

#[test]
fn gh_runs_from_facet_gh_with_the_fixed_env_never_from_path() {
    let fx = Fx::new();
    let home = fx.sandbox.root().join("home");
    let gh_config = fx.sandbox.root().join("gh-config");
    let mut command = fx.facet();
    command
        .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
        .env("HOME", &home)
        .env("GH_CONFIG_DIR", &gh_config);
    for name in PARENT_ENV {
        command.env(name, PARENT_CANARY);
    }
    for name in LOADER_ENV {
        command.env(name, "");
    }
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "dry run: {}", run.stderr);
    assert!(!run.mentions(GH_TOKEN), "dry run: the gh token printed");
    assert!(
        !run.mentions(PARENT_CANARY),
        "dry run: a parent canary printed"
    );
    fx.assert_no_decoy("dry run");
    let calls = fx.gh.calls();
    assert_eq!(calls.len(), 1, "dry run: gh auth token runs once");
    assert_eq!(calls[0].argv, ["auth", "token"]);
    assert_child_env("dry run", &calls);
    for name in LOADER_ENV {
        assert_eq!(env_of(&calls[0], name), None, "dry run: {name} reached gh");
    }
    assert_eq!(env_of(&calls[0], "HOME"), home.to_str());
    assert_eq!(env_of(&calls[0], "GH_CONFIG_DIR"), gh_config.to_str());
    assert!(!fx.gh.dump_contains(GH_TOKEN), "gh received its own token");

    // No GH_CONFIG_DIR in the parent: none in the child.
    let mut command = fx.facet();
    command
        .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
        .env("HOME", &home)
        .env_remove("GH_CONFIG_DIR");
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "no GH_CONFIG_DIR: {}", run.stderr);
    let calls = fx.gh.calls();
    assert_eq!(calls.len(), 2);
    let mut keys: Vec<&str> = calls[1].env.iter().map(|(key, _)| key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["HOME", "PATH"]);
    fx.assert_no_decoy("no GH_CONFIG_DIR");
}

#[test]
fn facet_gh_must_be_an_absolute_regular_file_and_is_never_echoed() {
    let fx = Fx::new();
    // A real stub at the relative path, so only "relative" refuses it.
    let relative = "stubs/gh";
    assert!(fx.sandbox.root().join(relative).is_file());
    let missing = fx.sandbox.root().join("missing").join("gh");
    let directory = fx.sandbox.root().join("stubs");
    for (what, value) in [
        ("relative", relative.to_owned()),
        ("missing", missing.display().to_string()),
        ("directory", directory.display().to_string()),
    ] {
        let mut command = fx.facet();
        command
            .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
            .env("FACET_GH", &value);
        let run = fx.run(&mut command);
        run.assert_refused(what, "subprocess_path_refused");
        assert!(
            !run.mentions(&value),
            "{what}: the FACET_GH path was echoed"
        );
        assert!(!fx.gh.ran(), "{what}: gh ran");
        fx.assert_no_decoy(what);
    }

    // Positive control: the absolute stub runs.
    let run = fx.request("items/0", &["--dry-run"]);
    assert_eq!(run.code, Some(0), "control: {}", run.stderr);
    assert!(fx.gh.ran(), "control: the FACET_GH stub should run");
}

#[test]
fn the_switch_stops_gh_and_fails_subprocess_disabled_before_send() {
    let fx = Fx::new();
    for (what, flag, env) in [
        ("flag", Some("--no-subprocess"), None),
        ("env", None, Some("1")),
        ("env any value", None, Some("yes")),
    ] {
        for dry_run in [false, true] {
            let mut command = fx.facet();
            command.args(["request", "run", fx.ws(), "items/0", "--no-record"]);
            if dry_run {
                command.arg("--dry-run");
            }
            if let Some(flag) = flag {
                command.arg(flag);
            }
            if let Some(value) = env {
                command.env("FACET_NO_SUBPROCESS", value);
            }
            let run = fx.run(&mut command);
            run.assert_refused(what, "subprocess_disabled");
            assert!(!fx.gh.ran(), "{what}: gh ran");
            fx.assert_no_decoy(what);
        }
    }

    // A supplied githubToken needs no gh, switch or not.
    let var = format!("githubToken={VAR_CANARY}");
    let run = fx.request("items/0", &["--dry-run", "--no-subprocess", "--var", &var]);
    assert_eq!(run.code, Some(0), "--var: {}", run.stderr);
    assert!(!fx.gh.ran(), "--var: gh ran");

    // Positive control: an empty FACET_NO_SUBPROCESS leaves children on.
    let mut command = fx.facet();
    command
        .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
        .env("FACET_NO_SUBPROCESS", "");
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "control: {}", run.stderr);
    assert!(fx.gh.ran(), "control: gh should run");
}

#[test]
fn the_host_check_fires_before_any_spawn_with_the_switch_on_and_off() {
    let fx = Fx::new();
    for (what, extra, env) in [
        ("switch off", None, None),
        ("flag", Some("--no-subprocess"), None),
        ("env", None, Some("1")),
    ] {
        let mut command = fx.facet();
        command.args(["request", "run", fx.ws(), "items/1"]);
        if let Some(flag) = extra {
            command.arg(flag);
        }
        if let Some(value) = env {
            command.env("FACET_NO_SUBPROCESS", value);
        }
        let run = fx.run(&mut command);
        run.assert_refused(what, "github_token_host_refused");
        assert!(!fx.gh.ran(), "{what}: gh ran");
        assert!(!fx.git.ran(), "{what}: git ran");
        fx.assert_no_decoy(what);
    }
}

#[test]
fn gh_api_fallback_skips_locked_sends_and_keeps_sensitive_headers_off_argv() {
    let fx = Fx::new();

    // Positive control: an open send whose native engine failed falls back
    // to `gh api`, without the sensitive header.
    let run = fx.request("items/2", &["--no-record"]);
    assert_eq!(run.code, Some(0), "fallback: {}", run.stderr);
    assert!(
        run.stdout.contains("h1-stub-gh"),
        "fallback: no gh api answer"
    );
    let calls = fx.gh.calls();
    assert_eq!(calls.len(), 1, "fallback: gh api runs once");
    let argv = &calls[0].argv;
    assert_eq!(argv[..2], ["api", "/repos/example/project"]);
    assert!(argv.iter().any(|arg| arg == "X-Plain: h1-plain"));
    assert!(
        !argv
            .iter()
            .any(|arg| arg.to_ascii_lowercase().starts_with("x-api-key")),
        "fallback: a sensitive header reached gh's argv"
    );
    assert!(
        !fx.gh.dump_contains(HEADER_CANARY),
        "fallback: the sensitive header value reached gh"
    );
    assert_child_env("fallback", &calls);
    fx.assert_no_decoy("fallback");

    // The switch: no fallback, the native error stands.
    let run = fx.request("items/2", &["--no-record", "--no-subprocess"]);
    assert_ne!(run.code, Some(0), "switch: the send should fail");
    assert_eq!(fx.gh.calls().len(), 1, "switch: gh api ran");

    // A locked send: no fallback, the native error stands.
    let var = format!("token={VAR_CANARY}");
    let run = fx.request("items/3", &["--no-record", "--var", &var]);
    assert_ne!(run.code, Some(0), "locked: the send should fail");
    assert_eq!(fx.gh.calls().len(), 1, "locked: gh api ran");
    assert!(!fx.gh.dump_contains(VAR_CANARY), "locked: gh got the value");
    assert!(!run.mentions(VAR_CANARY), "locked: the value printed");
    fx.assert_no_decoy("locked");
}

/// A loopback collection for the git cases: one request, sent and recorded.
fn loopback(fx: &Fx, url: &str) -> PathBuf {
    let workspace = fx.sandbox.root().join("local.yml");
    fs::write(
        &workspace,
        format!(
            "opencollection: 1.0.0\ninfo: {{ name: H1 local }}\nbundled: true\nitems:\n  - info: {{ name: Local, type: http, seq: 1 }}\n    http:\n      method: GET\n      url: {url}/h1\n"
        ),
    )
    .unwrap();
    workspace
}

#[test]
fn the_switch_stops_git_on_a_recorded_run_and_replay() {
    let fx = Fx::new();
    let (url, server) = serve_once(b"{}".to_vec(), "application/json");
    let workspace = loopback(&fx, &url);
    let ws = workspace.to_str().unwrap();
    let git_tag = format!("git:{STUB_SHA}");
    let send = |extra: &[&str], env: Option<&str>| {
        let mut command = fx.facet_local();
        command.args(["request", "run", ws, "items/0"]).args(extra);
        if let Some(value) = env {
            command.env("FACET_NO_SUBPROCESS", value);
        }
        fx.run(&mut command)
    };

    // Positive control: a recorded run asks the FACET_GIT stub, with the
    // fixed env, and records its tag.
    let run = send(&[], None);
    assert_eq!(run.code, Some(0), "control: {}", run.stderr);
    server.join().unwrap();
    let control = run_id(&run);
    assert!(tags(&fx.sandbox, &control).contains(&git_tag));
    let calls = fx.git.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.argv.iter().any(|arg| arg == "rev-parse")),
        "control: git rev-parse should run"
    );
    assert_child_env("git control", &calls);
    let after_control = calls.len();

    // The switch, as a flag and as the env: no git, no git tag.
    let mut refused = Vec::new();
    for (what, extra, env) in [
        ("flag", &["--no-subprocess"][..], None),
        ("env", &[][..], Some("1")),
    ] {
        let server = serve_again(&url, b"{}".to_vec());
        let run = send(extra, env);
        assert_eq!(run.code, Some(0), "{what}: {}", run.stderr);
        server.join().unwrap();
        let id = run_id(&run);
        assert!(
            !tags(&fx.sandbox, &id).contains(&git_tag),
            "{what}: git tag"
        );
        assert_eq!(fx.git.calls().len(), after_control, "{what}: git ran");
        refused.push(id);
    }

    // A relative FACET_GIT is refused: no git, no tag, the run still works.
    let server = serve_again(&url, b"{}".to_vec());
    let mut command = fx.facet_local();
    command
        .args(["request", "run", ws, "items/0"])
        .env("FACET_GIT", "stubs/git");
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "relative FACET_GIT: {}", run.stderr);
    server.join().unwrap();
    assert!(!tags(&fx.sandbox, &run_id(&run)).contains(&git_tag));
    assert_eq!(fx.git.calls().len(), after_control, "relative: git ran");

    // Replay with the switch, as a flag and as the env: no git, no tag.
    for (what, extra, env) in [
        ("replay flag", &["--no-subprocess"][..], None),
        ("replay env", &[][..], Some("1")),
    ] {
        let server = serve_again(&url, b"{}".to_vec());
        let mut command = fx.facet_local();
        command.args(["replay", &control, ws]).args(extra);
        if let Some(value) = env {
            command.env("FACET_NO_SUBPROCESS", value);
        }
        let run = fx.run(&mut command);
        assert_eq!(run.code, Some(0), "{what}: {}", run.stderr);
        server.join().unwrap();
        assert!(
            !tags(&fx.sandbox, &run_id(&run)).contains(&git_tag),
            "{what}"
        );
        assert_eq!(fx.git.calls().len(), after_control, "{what}: git ran");
    }

    // Positive control: replay of a run that had no tag asks git again.
    let server = serve_again(&url, b"{}".to_vec());
    let mut command = fx.facet_local();
    command.args(["replay", &refused[0], ws]);
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "replay control: {}", run.stderr);
    server.join().unwrap();
    assert!(tags(&fx.sandbox, &run_id(&run)).contains(&git_tag));
    assert!(
        fx.git.calls().len() > after_control,
        "replay control: no git"
    );
    assert!(!fx.gh.ran(), "gh ran for a loopback request");
    fx.assert_no_decoy("git");
}

#[test]
fn replay_that_needs_gh_with_the_switch_fails_before_send() {
    let fx = Fx::new();
    let (url, server) = serve_once(b"{}".to_vec(), "application/json");
    let workspace = loopback(&fx, &url);
    let ws = workspace.to_str().unwrap();
    let mut command = fx.facet_local();
    command.args(["request", "run", ws, "items/0"]);
    let run = fx.run(&mut command);
    assert_eq!(run.code, Some(0), "record: {}", run.stderr);
    server.join().unwrap();
    let recorded = run_id(&run);

    // The current YAML now asks for the gh token at api.github.com.
    fs::write(
        &workspace,
        "opencollection: 1.0.0\ninfo: { name: H1 local }\nbundled: true\nitems:\n  - info: { name: Local, type: http, seq: 1 }\n    http:\n      method: GET\n      url: https://api.github.com/user\n      headers:\n        - name: Authorization\n          value: Bearer {{githubToken}}\n",
    )
    .unwrap();
    for (what, extra, env) in [
        ("flag", &["--no-subprocess"][..], None),
        ("env", &[][..], Some("1")),
    ] {
        let mut command = fx.facet();
        command.args(["replay", &recorded, ws]).args(extra);
        if let Some(value) = env {
            command.env("FACET_NO_SUBPROCESS", value);
        }
        let run = fx.run(&mut command);
        run.assert_refused(what, "subprocess_disabled");
        assert!(!fx.gh.ran(), "{what}: gh ran");
        fx.assert_no_decoy(what);
    }
}

/// Wraps `command` in a macOS sandbox profile that denies reading the
/// fixed `gh` locations, so the shipped binary finds no `gh` at any of them
/// while every other path, the stubs included, stays readable. Linux has
/// no unprivileged equivalent, so the two tests below run on macOS only
/// (tower and the macOS CI leg).
#[cfg(target_os = "macos")]
fn without_fixed_gh(command: &Command) -> Command {
    const PROFILE: &str = r#"(version 1)(allow default)(deny file-read* (literal "/opt/homebrew/bin/gh") (literal "/usr/local/bin/gh") (literal "/usr/bin/gh"))"#;
    let mut sandboxed = Command::new("/usr/bin/sandbox-exec");
    sandboxed
        .arg("-p")
        .arg(PROFILE)
        .arg(command.get_program())
        .args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => sandboxed.env(key, value),
            None => sandboxed.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        sandboxed.current_dir(dir);
    }
    sandboxed
}

/// PM ruling 2: no override and no `gh` at a fixed location is no token,
/// as on the base, not an error. `PATH` is not searched as a fallback.
#[cfg(target_os = "macos")]
#[test]
fn no_override_and_no_fixed_gh_is_no_token_not_an_error() {
    let fx = Fx::new();

    // Positive control: inside the same sandbox, FACET_GH still runs.
    let mut command = fx.facet();
    command.args(["request", "run", fx.ws(), "items/0", "--dry-run"]);
    let run = fx.run(&mut without_fixed_gh(&command));
    assert_eq!(run.code, Some(0), "control: {}", run.stderr);
    assert_eq!(
        fx.gh.calls().len(),
        1,
        "control: the FACET_GH stub should run"
    );

    let mut command = fx.facet();
    command
        .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
        .env_remove("FACET_GH");
    let run = fx.run(&mut without_fixed_gh(&command));
    assert_eq!(run.code, Some(0), "no gh: {}", run.stderr);
    assert!(
        !run.mentions("subprocess_path_refused"),
        "no gh: a missing gh is not a refused override"
    );
    assert!(!run.mentions(GH_TOKEN), "no gh: a token appeared");
    assert_eq!(
        fx.gh.calls().len(),
        1,
        "no gh: the stub ran without FACET_GH"
    );
    fx.assert_no_decoy("no gh");
}

/// PM ruling 2, the other side: a bad override is `subprocess_path_refused`
/// whether or not a fixed `gh` exists, and never falls back to the list or
/// to `PATH`.
#[cfg(target_os = "macos")]
#[test]
fn a_bad_override_is_refused_even_with_no_fixed_gh() {
    let fx = Fx::new();
    let missing = fx.sandbox.root().join("missing").join("gh");
    for (what, value) in [
        ("relative", "stubs/gh".to_owned()),
        ("missing", missing.display().to_string()),
    ] {
        let mut command = fx.facet();
        command
            .args(["request", "run", fx.ws(), "items/0", "--dry-run"])
            .env("FACET_GH", &value);
        let run = fx.run(&mut without_fixed_gh(&command));
        run.assert_refused(what, "subprocess_path_refused");
        assert!(
            !run.mentions(&value),
            "{what}: the FACET_GH path was echoed"
        );
        assert!(!fx.gh.ran(), "{what}: gh ran");
        fx.assert_no_decoy(what);
    }
}
