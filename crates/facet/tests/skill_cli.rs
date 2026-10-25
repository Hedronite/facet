//! The agent skill in `.agents/skills/facet/` is a thin wrapper over Facet CLI reads.
//! These tests lock the argv it is allowed to build and the CLI facts that
//! wrapper depends on: `--json` after the subcommand, metadata history, env
//! list without values, and hash-first diff.

#[allow(dead_code, unused_imports)]
mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use common::{Sandbox, serve_once};
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn skill_nu() -> String {
    fs::read_to_string(repo_root().join(".agents/skills/facet/facet.nu")).expect("skill body")
}

fn flag_tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut flags = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'-' && bytes[index + 1] == b'-' {
            let start = index;
            index += 2;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'-')
            {
                index += 1;
            }
            if index > start + 2 {
                flags.push(source[start..index].to_owned());
            }
        } else {
            index += 1;
        }
    }
    flags
}

fn allow_flags(source: &str) -> Vec<String> {
    let line = source
        .lines()
        .find(|line| line.starts_with("# allow-flags:"))
        .expect("allow-flags comment");
    line.trim_start_matches("# allow-flags:")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[test]
fn skill_source_stays_on_the_safe_command_set() {
    let source = skill_nu();
    let mut found = flag_tokens(&source);
    found.sort();
    found.dedup();
    let mut allowed = allow_flags(&source);
    allowed.sort();
    assert_eq!(found, allowed);

    for denied in [
        "--bodies",
        "--value",
        "--dry-run",
        "--output",
        "--probe",
        "--var",
        "--expect",
        "--frozen",
        "--secret",
        "--yes",
        "blob",
        "replay",
        "request run",
        "ncl apply",
        "updatedAt",
    ] {
        assert!(
            !source.contains(denied),
            "skill body contains {denied}, which is outside the safe command set"
        );
    }

    assert!(source.contains("^facet ...$args --json"));
    assert!(!source.contains("^facet --json"));
    assert!(source.contains("FACET_ACTOR"));
    assert!(source.contains("\"human\""));
    assert!(source.contains("opencollection.yml"));
    assert!(source.contains("lattice.db"));
    assert!(source.contains("cd $found.dir"));
    assert!(!source.contains("facet-skill"));
    for command in [
        "\"history\"",
        "\"last\"",
        "\"session\"",
        "\"list\"",
        "\"show\"",
        "\"pin\"",
        "\"env\"",
        "\"diff\"",
        "\"doctor\"",
        "\"ncl\"",
        "\"check\"",
        "\"export\"",
        "\"--id\"",
        "\"--sql\"",
        "\"get\"",
        "stderr:",
        "project-names",
    ] {
        assert!(source.contains(command), "missing {command}");
    }

    let skill =
        fs::read_to_string(repo_root().join(".agents/skills/facet/SKILL.md")).expect("SKILL.md");
    for phrase in [
        "history --bodies",
        "history --id",
        "history --sql",
        "diff --bodies",
        "request run",
        "request list",
        "request get",
        "ncl check",
        "ncl export",
        "ncl apply",
        "session start",
        "session end",
        "env set",
        "env delete",
        "sql_read_only",
        "--dry-run",
        "blob",
        "names only",
        "facet history --json",
        "FACET_ACTOR",
        "human",
        "ok: false",
        "stderr",
        "opencollection.yml",
        ".facet/lattice.db",
    ] {
        assert!(skill.contains(phrase), "SKILL.md missing {phrase}");
    }

    let readme = fs::read_to_string(repo_root().join("README.md")).expect("README");
    assert!(readme.contains("## Agent use: skills + CLI first, MCP still shipped"));
    assert!(readme.contains(".agents/skills/facet/"));
    assert!(readme.contains("scripts/sync-skills"));
    assert!(readme.contains("`facet mcp`"));

    let keys = frontmatter_keys(&skill);
    assert_eq!(keys, BTreeSet::from(["description", "name"]));
}

/// Harness copies are ordinary files with the same bytes as `.agents/skills/`.
#[test]
fn skill_copies_match_canonical_bytes() {
    let root = repo_root();
    let canonical = root.join(".agents/skills");
    let source = skill_tree(&canonical);
    assert!(
        source.files.contains_key(Path::new("facet/SKILL.md")),
        "canonical skill is missing"
    );
    assert!(source.files.contains_key(Path::new("facet/facet.nu")));

    for copy in [".claude/skills", ".grok/skills", ".cursor/skills"] {
        let tree = skill_tree(&root.join(copy));
        assert_eq!(tree.dirs, source.dirs, "{copy} directories differ");
        assert_eq!(
            tree.files.keys().collect::<Vec<_>>(),
            source.files.keys().collect::<Vec<_>>(),
            "{copy} file paths differ"
        );
        for (path, bytes) in &source.files {
            assert_eq!(
                tree.files.get(path).map(Vec::as_slice),
                Some(bytes.as_slice()),
                "{copy} {path:?} bytes differ"
            );
        }
    }

    for absent in [".omp/skills", ".codex/skills", "skills"] {
        assert!(!root.join(absent).exists(), "{absent} should not exist");
    }

    let claude = fs::read_to_string(root.join("CLAUDE.md")).expect("CLAUDE.md");
    assert_eq!(claude, "@AGENTS.md\n");
}

fn frontmatter_keys(skill: &str) -> BTreeSet<&str> {
    let body = skill
        .strip_prefix("---\n")
        .expect("SKILL.md frontmatter start");
    let end = body.find("\n---\n").expect("SKILL.md frontmatter end");
    body[..end]
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(' ') && !line.starts_with('\t'))
        .map(|line| line.split_once(':').expect("frontmatter key").0)
        .collect()
}

struct SkillTree {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

fn skill_tree(root: &Path) -> SkillTree {
    let mut tree = SkillTree {
        dirs: BTreeSet::new(),
        files: BTreeMap::new(),
    };
    walk_skill_tree(root, root, &mut tree);
    tree
}

fn walk_skill_tree(root: &Path, path: &Path, tree: &mut SkillTree) {
    let meta = fs::symlink_metadata(path).unwrap_or_else(|error| {
        panic!("stat {}: {error}", path.display());
    });
    assert!(
        !meta.file_type().is_symlink(),
        "{} is a symlink",
        path.display()
    );
    if meta.is_dir() {
        if path != root {
            tree.dirs
                .insert(path.strip_prefix(root).expect("prefix").to_path_buf());
        }
        for entry in fs::read_dir(path).unwrap_or_else(|error| {
            panic!("read {}: {error}", path.display());
        }) {
            let entry = entry.expect("dir entry");
            walk_skill_tree(root, &entry.path(), tree);
        }
    } else {
        let rel = path.strip_prefix(root).expect("prefix").to_path_buf();
        tree.files
            .insert(rel, fs::read(path).expect("read skill file"));
    }
}

#[test]
fn json_after_subcommand_selects_facet_history() {
    let sandbox = Sandbox::new();

    let help = sandbox
        .facet()
        .arg("--help")
        .output()
        .expect("facet --help");
    let help_text = String::from_utf8(help.stdout).expect("help utf-8");
    for flag in [
        "--limit",
        "--request",
        "--status",
        "--session",
        "--open",
        "--environment",
        "--bodies",
        "--id",
        "--json",
        "--dry-run",
    ] {
        assert!(help_text.contains(flag), "help missing {flag}");
    }
    for line in [
        "session list",
        "session show",
        "pin list",
        "env list",
        "facet doctor",
    ] {
        assert!(
            help_text.contains(line) || help_text.contains(line.trim_start_matches("facet ")),
            "help missing {line}"
        );
    }

    // The first token selects Facet or Probe, before `--json` is stripped.
    let leading = sandbox
        .facet()
        .args(["--json", "history"])
        .output()
        .expect("facet --json history");
    assert!(!leading.status.success());
    let leading_text = String::from_utf8(leading.stdout).expect("stdout");
    assert!(!leading_text.contains("\"runs\""));

    let trailing = sandbox
        .facet()
        .args(["history", "--json"])
        .output()
        .expect("facet history --json");
    assert!(trailing.status.success());
    let document: Value = serde_json::from_slice(&trailing.stdout).expect("history json");
    assert_eq!(document["schemaVersion"], 1);
    assert!(document.get("runs").is_some());

    let unknown = sandbox
        .facet()
        .args(["history", "--not-a-skill-flag", "--json"])
        .output()
        .expect("unknown flag");
    assert_eq!(unknown.status.code(), Some(2));

    // Real flags the skill must not pass. An empty store has nothing to print.
    let bodies = sandbox
        .facet()
        .args(["history", "--bodies", "--json"])
        .output()
        .expect("history --bodies");
    assert!(bodies.status.success());
    let by_id = sandbox
        .facet()
        .args(["history", "--id", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--json"])
        .output()
        .expect("history --id");
    assert_eq!(by_id.status.code(), Some(4));

    let doctor = sandbox
        .facet()
        .args(["doctor", "--json"])
        .output()
        .expect("doctor");
    assert_eq!(
        doctor.status.code(),
        Some(1),
        "no workspace store is a warning, and the document is still on stdout"
    );
    let doctor_json: Value = serde_json::from_slice(&doctor.stdout).expect("doctor json");
    assert_eq!(doctor_json["schemaVersion"], 1);
    assert_eq!(doctor_json["doctor"]["secrets"]["probe"], false);
    assert!(!doctor_json.to_string().contains("test-master-key"));
}

#[test]
fn wrapped_commands_omit_bodies_and_stored_values() {
    let sandbox = Sandbox::new();
    let (url, server) = serve_once(br#"{"skill":"body-marker"}"#.to_vec(), "application/json");
    let workspace = sandbox.workspace(&url);
    let path = workspace.to_str().expect("workspace path").to_owned();
    let first = sandbox.run_json(&["request", "run", &path, "items/0", "--environment", "local"]);
    server.join().expect("mock server");
    let first_id = first["lattice"]["runId"]
        .as_str()
        .expect("run id")
        .to_owned();

    let history = sandbox.run_json(&[
        "history",
        &path,
        "--limit",
        "20",
        "--request",
        "items/0",
        "--status",
        "200",
    ]);
    assert!(
        history["runs"][0]["response"]["body"]
            .get("content")
            .is_none()
    );
    assert!(
        !history.to_string().contains("body-marker"),
        "history included a stored body"
    );

    let filtered = sandbox.run_json(&[
        "history",
        &path,
        "--limit",
        "1",
        "--session",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    ]);
    assert_eq!(filtered["runs"].as_array().expect("runs").len(), 0);

    let last = sandbox.run_json(&["last", &path, "--request", "items/0"]);
    assert_eq!(last["run"]["id"], first_id);
    assert!(last["run"]["response"]["body"].get("content").is_none());
    assert!(
        !last.to_string().contains("body-marker"),
        "last included a stored body"
    );

    let again = common::serve_again(&url, br#"{"skill":"other-body"}"#.to_vec());
    let second = sandbox.run_json(&["request", "run", &path, "items/0", "--environment", "local"]);
    again.join().expect("second response");
    let second_id = second["lattice"]["runId"]
        .as_str()
        .expect("second run id")
        .to_owned();

    let diff = sandbox
        .facet()
        .args(["diff", &first_id, &second_id, &path, "--json"])
        .output()
        .expect("diff");
    assert_eq!(diff.status.code(), Some(1));
    let diff_json: Value = serde_json::from_slice(&diff.stdout).expect("diff json");
    assert_eq!(diff_json["equal"], false);
    assert!(diff_json["response"]["body"]["text"].is_null());
    let diff_text = diff_json.to_string();
    assert!(
        !diff_text.contains("body-marker") && !diff_text.contains("other-body"),
        "diff without --bodies included body text"
    );

    let marker = "env-value-marker";
    sandbox.run_json(&[
        "env",
        "set",
        &path,
        "--environment",
        "local",
        "--name",
        "token",
        "--value",
        marker,
    ]);
    let listed = sandbox.run_json(&["env", "list", &path, "--environment", "local"]);
    assert_eq!(listed["entries"][0]["environment"], "local");
    assert_eq!(listed["entries"][0]["name"], "token");
    assert!(listed["entries"][0].get("value").is_none());
    assert!(listed["entries"][0].get("name").is_some());
    assert!(
        !listed.to_string().contains(marker),
        "env list printed a stored value"
    );

    let sessions = sandbox.run_json(&["session", "list", "--open"]);
    assert!(sessions.get("sessions").is_some());
    let started = sandbox.run_json(&["session", "start"]);
    let session_id = started["session"]["id"].as_str().expect("session id");
    let shown = sandbox.run_json(&["session", "show", session_id]);
    assert_eq!(shown["session"]["id"], session_id);
    let current = sandbox
        .facet_in_session(session_id)
        .args(["session", "show", "current", "--json"])
        .output()
        .expect("session show current");
    assert!(current.status.success());

    let pins = sandbox.run_json(&["pin", "list", &path]);
    assert!(pins.get("pins").is_some());
}
