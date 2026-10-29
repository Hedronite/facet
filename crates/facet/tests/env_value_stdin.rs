//! P1b G3 / H12: `facet env set --value-stdin`, and the `--value --secret`
//! warning.
//!
//! A piped value is stored without one trailing newline, `env list` shows
//! metadata only, and a later run hydrates the exact value. Every refusal
//! (empty, a terminal, `--value` too, not UTF-8) is `invalid_arguments`
//! (exit 2), stores nothing and echoes nothing. `--value` with `--secret`
//! prints one stderr warning that holds no value.
//!
//! These drive the shipped binary and touch no network: the one send goes
//! to a loopback mock. Every value below is a canary, not a credential.

#![cfg(unix)]

#[allow(dead_code, unused_imports)]
mod common;

use std::process::{Output, Stdio};

use common::*;

const CANARY: &str = "h12-stdin-canary-value";
const ARGV_CANARY: &str = "h12-argv-canary-value";
const INVALID_ARGUMENTS_EXIT: i32 = 2;

/// What a refusal case is called, its extra flags, and what it pipes in.
type Case<'a> = (&'a str, Vec<&'a str>, Option<&'a [u8]>);

fn env_set(sandbox: &Sandbox, ws: &str, extra: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut command = sandbox.facet();
    command
        .args([
            "env",
            "set",
            ws,
            "--environment",
            "local",
            "--name",
            "token",
        ])
        .args(extra)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Some(_) => command.stdin(Stdio::piped()),
        None => command.stdin(Stdio::null()),
    };
    let mut child = command.spawn().expect("facet should run");
    if let Some(bytes) = stdin {
        // A refusal may exit before reading; a closed pipe is not a failure.
        let _ = child.stdin.take().unwrap().write_all(bytes);
    }
    child.wait_with_output().unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_canary(what: &str, text: &str) {
    for canary in [CANARY, ARGV_CANARY] {
        assert!(!text.contains(canary), "{what}: a value was printed");
    }
}

fn entries(sandbox: &Sandbox, ws: &str) -> Value {
    let list = sandbox
        .facet()
        .args(["env", "list", ws, "--json"])
        .output()
        .unwrap();
    assert!(list.status.success(), "env list");
    assert_no_canary("env list", &text(&list));
    let document: Value = serde_json::from_slice(&list.stdout).unwrap();
    document["entries"].clone()
}

#[test]
fn a_piped_value_round_trips_without_its_newline_and_lists_as_metadata() {
    for newline in ["\n", "\r\n", ""] {
        let sandbox = Sandbox::new();
        let (url, server) = serve_once(br#"{"ok":true}"#.to_vec(), "application/json");
        let workspace = sandbox.secret_workspace(&url);
        let ws = workspace.to_str().unwrap();

        let piped = format!("{CANARY}{newline}");
        let set = env_set(
            &sandbox,
            ws,
            &["--value-stdin", "--secret"],
            Some(piped.as_bytes()),
        );
        let what = format!("newline {newline:?}");
        assert!(set.status.success(), "{what}: {}", text(&set));
        assert_no_canary(&what, &text(&set));
        assert!(
            !String::from_utf8_lossy(&set.stderr).contains("warning:"),
            "{what}: --value-stdin warned"
        );

        // Metadata only: no field for the value, and no value anywhere.
        let listed = entries(&sandbox, ws);
        assert_eq!(listed.as_array().unwrap().len(), 1, "{what}");
        let entry = &listed[0];
        assert_eq!(entry["environment"], "local");
        assert_eq!(entry["name"], "token");
        assert_eq!(entry["secret"], true);
        let mut keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["environment", "name", "secret", "updatedAt"],
            "{what}"
        );

        // A later run hydrates exactly the value, with no newline kept.
        let run = sandbox
            .facet()
            .args([
                "request",
                "run",
                ws,
                "items/0",
                "--environment",
                "local",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(run.status.success(), "{what}: run {}", text(&run));
        assert_no_canary(&format!("{what} run"), &text(&run));
        // `serve_once` returns the request body, `{"token":"{{token}}"}`
        // rendered: exactly the value, with no newline kept.
        let received = server.join().unwrap();
        assert!(
            received == format!(r#"{{"token":"{CANARY}"}}"#).into_bytes(),
            "{what}: the hydrated value differs"
        );
    }
}

#[test]
fn every_value_stdin_refusal_is_invalid_arguments_and_stores_nothing() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.secret_workspace("http://127.0.0.1:9");
    let ws = workspace.to_str().unwrap();
    let mut not_utf8 = CANARY.as_bytes().to_vec();
    not_utf8.push(0xff);
    let piped = format!("{CANARY}\n");
    let cases: [Case<'_>; 6] = [
        ("empty", vec!["--value-stdin"], Some(b"")),
        ("newline only", vec!["--value-stdin"], Some(b"\n")),
        ("crlf only", vec!["--value-stdin"], Some(b"\r\n")),
        ("not UTF-8", vec!["--value-stdin"], Some(&not_utf8)),
        (
            "--value too",
            vec!["--value-stdin", "--value", ARGV_CANARY],
            Some(piped.as_bytes()),
        ),
        ("neither", vec![], None),
    ];
    for (what, extra, stdin) in cases {
        for secret in [false, true] {
            let mut extra = extra.clone();
            if secret {
                extra.push("--secret");
            }
            let output = env_set(&sandbox, ws, &extra, stdin);
            assert_eq!(
                output.status.code(),
                Some(INVALID_ARGUMENTS_EXIT),
                "{what}: {}",
                text(&output)
            );
            let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(envelope["error"]["category"], "invalid_arguments", "{what}");
            assert_no_canary(what, &text(&output));
        }
    }
    assert_eq!(entries(&sandbox, ws), json!([]), "a refusal stored a value");
}

/// macOS only: `script` gives the shipped binary a terminal for stdin. The
/// refusal logic itself is unit-tested in `env.rs` on every platform.
#[cfg(target_os = "macos")]
#[test]
fn a_terminal_stdin_is_refused() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.secret_workspace("http://127.0.0.1:9");
    let ws = workspace.to_str().unwrap();
    let facet = sandbox.facet();
    let mut command = Command::new("/usr/bin/script");
    command
        .args(["-q", "/dev/null"])
        .arg(facet.get_program())
        .args([
            "env",
            "set",
            ws,
            "--environment",
            "local",
            "--name",
            "token",
            "--value-stdin",
            "--secret",
            "--json",
        ])
        .stdin(Stdio::null());
    for (key, value) in facet.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    if let Some(dir) = facet.get_current_dir() {
        command.current_dir(dir);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(INVALID_ARGUMENTS_EXIT),
        "{}",
        text(&output)
    );
    assert!(text(&output).contains("invalid_arguments"));
    assert_eq!(
        entries(&sandbox, ws),
        json!([]),
        "a terminal stored a value"
    );

    // Positive control: the same command with a pipe stores the value.
    let set = env_set(
        &sandbox,
        ws,
        &["--value-stdin", "--secret"],
        Some(CANARY.as_bytes()),
    );
    assert!(set.status.success(), "control: {}", text(&set));
    assert_eq!(entries(&sandbox, ws).as_array().unwrap().len(), 1);
}

#[test]
fn value_with_secret_warns_once_without_the_value() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.secret_workspace("http://127.0.0.1:9");
    let ws = workspace.to_str().unwrap();

    let output = env_set(&sandbox, ws, &["--value", ARGV_CANARY, "--secret"], None);
    assert!(output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("warning:"))
        .collect();
    assert_eq!(warnings.len(), 1, "one warning");
    assert!(
        warnings[0].contains("--value-stdin"),
        "the warning names the fix"
    );
    assert!(
        warnings[0].contains("argv"),
        "the warning names the exposure"
    );
    assert_no_canary("--value --secret", &text(&output));
    assert_eq!(entries(&sandbox, ws)[0]["secret"], true);

    // Without --secret, nothing changes: no warning.
    let output = env_set(&sandbox, ws, &["--value", ARGV_CANARY], None);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("warning:"),
        "--value without --secret warned"
    );
}
