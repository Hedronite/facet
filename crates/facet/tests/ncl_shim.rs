//! Hermetic tests for the one-release `facet ncl` → `tetractl` shim.

#[allow(dead_code, unused_imports)]
mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use common::Sandbox;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn write_spy(root: &Path, exit_code: i32) -> PathBuf {
    let script = root.join("tetra-spy.sh");
    let argv_log = root.join("argv.log");
    let env_log = root.join("env.log");
    let stdin_log = root.join("stdin.log");
    let body = format!(
        r#"#!/bin/sh
printf '%s\n' "$@" >> "{argv_log}"
env >> "{env_log}"
cat >> "{stdin_log}"
printf 'child-out\n'
exit {exit_code}
"#,
        argv_log = argv_log.display(),
        env_log = env_log.display(),
        stdin_log = stdin_log.display(),
    );
    fs::write(&script, body).unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn run_ncl(
    sandbox: &Sandbox,
    tetra: &Path,
    extra_env: &[(&str, &str)],
    args: &[&str],
) -> std::process::Output {
    let mut command = sandbox.facet();
    command.env("TETRA_BIN", tetra);
    command.env("HOME", sandbox.root());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.arg("ncl");
    command.args(args);
    if args.contains(&"-") {
        command.stdin(std::process::Stdio::piped());
        let mut child = command.spawn().expect("spawn facet ncl");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stdin-payload\n")
            .unwrap();
        child.wait_with_output().expect("wait facet ncl")
    } else {
        command.output().expect("facet ncl")
    }
}

fn deprecation_lines(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.contains("deprecated"))
        .map(str::to_owned)
        .collect()
}

fn argv_log(sandbox: &Sandbox) -> Vec<String> {
    fs::read_to_string(sandbox.root().join("argv.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn ncl_shim_maps_all_verbs_and_forwards_args() {
    let sandbox = Sandbox::new();
    let spy = write_spy(sandbox.root(), 0);
    for (verb, mapped) in [
        ("check", "check"),
        ("export", "eval"),
        ("apply", "apply"),
        ("pack", "pack"),
    ] {
        fs::remove_file(sandbox.root().join("argv.log")).ok();
        let out = run_ncl(
            &sandbox,
            &spy,
            &[],
            &[
                verb,
                "world.ncl",
                "--var",
                "replicas=2",
                "--import-path",
                "/pack",
                "--json",
                "-q",
            ],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(deprecation_lines(&out.stderr).len(), 1);
        assert!(String::from_utf8_lossy(&out.stderr).contains("tetractl"));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "child-out");
        let argv = argv_log(&sandbox);
        assert_eq!(argv[0], mapped);
        assert!(argv.iter().any(|arg| arg == "world.ncl"));
        assert!(argv.iter().any(|arg| arg == "--var"));
        assert!(argv.iter().any(|arg| arg == "replicas=2"));
        assert!(argv.iter().any(|arg| arg == "--import-path"));
        assert!(argv.iter().any(|arg| arg == "/pack"));
        assert!(argv.iter().any(|arg| arg == "--json"));
        assert!(argv.iter().any(|arg| arg == "-q"));
    }
}

#[test]
fn ncl_shim_forwards_stdin_and_exit_codes() {
    let sandbox = Sandbox::new();
    for (code, expected) in [(0, 0), (3, 3), (5, 5), (42, 42)] {
        let spy = write_spy(sandbox.root(), code);
        let out = run_ncl(&sandbox, &spy, &[], &["pack", "-"]);
        assert_eq!(out.status.code(), Some(expected));
        let stdin = fs::read_to_string(sandbox.root().join("stdin.log")).unwrap();
        assert!(stdin.contains("stdin-payload"));
    }
}

#[test]
fn ncl_shim_sets_facet_bin_by_default_and_preserves_explicit() {
    let sandbox = Sandbox::new();
    let spy = write_spy(sandbox.root(), 0);
    let _ = run_ncl(&sandbox, &spy, &[], &["check", "world.ncl"]);
    let env = fs::read_to_string(sandbox.root().join("env.log")).unwrap();
    assert!(env.contains("FACET_BIN="));
    assert!(!env.contains("FACET_BIN=/stale/homebrew/facet"));

    fs::remove_file(sandbox.root().join("env.log")).unwrap();
    let explicit = sandbox.root().join("custom-facet");
    fs::write(&explicit, "stub").unwrap();
    let _ = run_ncl(
        &sandbox,
        &spy,
        &[("FACET_BIN", explicit.to_str().unwrap())],
        &["check", "world.ncl"],
    );
    let env = fs::read_to_string(sandbox.root().join("env.log")).unwrap();
    assert!(env.contains(&format!("FACET_BIN={}", explicit.display())));
}

#[test]
fn ncl_shim_missing_tetra_bin_fails_loudly() {
    let sandbox = Sandbox::new();
    let missing = sandbox.root().join("no-tetractl");
    let out = run_ncl(&sandbox, &missing, &[], &["check", "world.ncl"]);
    assert_eq!(out.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("TETRA_BIN"));
    assert!(stderr.contains("tetractl"));
}

#[test]
fn ncl_shim_unknown_verb_errors() {
    let sandbox = Sandbox::new();
    let spy = write_spy(sandbox.root(), 0);
    let out = run_ncl(&sandbox, &spy, &[], &["freeze", "world.ncl"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown ncl subcommand"));
}

#[test]
fn ncl_shim_bare_ncl_usage_error() {
    let sandbox = Sandbox::new();
    let spy = write_spy(sandbox.root(), 0);
    let out = run_ncl(&sandbox, &spy, &[], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}
