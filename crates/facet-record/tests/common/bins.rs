//! The shipped `facet` and `probe` binaries for tests that drive them.
//!
//! Every test process asks for them, and nextest runs each test in its own
//! process. `cargo build` re-links `<target-dir>/<profile>/<bin>` on every
//! call, fresh or not, so a process spawning that path while another builds
//! can see it missing. Two rules keep every spawn safe:
//!
//! - The build uses its own target dir, `<target>/facet-record-shipped/target`,
//!   so it never re-links `target/<profile>/facet` or `probe`, which the
//!   `facet-cli` and `probe-cli` tests spawn as `CARGO_BIN_EXE_*`.
//! - Each process takes a lock, runs the one build invocation below, and
//!   copies each binary to a path named for its contents. Tests spawn the
//!   copy, which no build touches.

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::UNIX_EPOCH,
};

use serde_json::Value;

/// `facet` or `probe`, built from this workspace with the profile running
/// this test. The build is a no-op when fresh and rebuilds
/// when a source changed, so the binary is never stale.
pub fn shipped(bin: &str) -> &'static Path {
    static BINS: OnceLock<[PathBuf; 2]> = OnceLock::new();
    let [facet, probe] = BINS.get_or_init(build);
    match bin {
        "facet" => facet,
        "probe" => probe,
        other => panic!("no shipped binary named {other}"),
    }
}

fn build() -> [PathBuf; 2] {
    let exe = std::env::current_exe().expect("test executable path");
    // <target>/[<triple>/]<profile>/deps/<test>-<hash>
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("profile directory");
    let profile = profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("profile name");
    let parent = profile_dir.parent().expect("target directory");
    let (target, triple) = if parent.join("CACHEDIR.TAG").exists() {
        (parent, None)
    } else {
        (
            parent.parent().expect("target directory"),
            Some(parent.file_name().expect("target triple")),
        )
    };
    let shipped = target.join("facet-record-shipped");
    let copies = shipped.join(profile).join("bins");
    fs::create_dir_all(&copies).expect("shipped binary directory");
    // Held until this function returns: one build and copy at a time.
    let lock = File::create(copies.join(".lock")).expect("lock file");
    lock.lock().expect("lock the shipped binary build");

    let mut command = Command::new(env!("CARGO"));
    command
        .args(["build", "--locked"])
        .args(["-p", "facet-cli", "--bin", "facet"])
        .args(["-p", "probe-cli", "--bin", "probe"])
        .args(["--message-format", "json-render-diagnostics"])
        .args([
            "--profile",
            if profile == "debug" { "dev" } else { profile },
        ])
        .arg("--target-dir")
        .arg(shipped.join("target"))
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if let Some(triple) = triple {
        command.arg("--target").arg(triple);
    }
    let output = command.output().expect("cargo should run");
    assert!(
        output.status.success(),
        "cargo build -p facet-cli -p probe-cli failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let built = |bin: &str| {
        let path = messages
            .iter()
            .filter(|message| {
                message["reason"] == "compiler-artifact" && message["target"]["name"] == bin
            })
            .find_map(|message| message["executable"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| panic!("cargo should report the {bin} executable"));
        copy(&path, &copies, bin)
    };
    let bins = [built("facet"), built("probe")];
    drop(lock);
    bins
}

/// `path` copied to `<copies>/<bin>-<size>-<mtime>`, written once and
/// renamed into place, so a spawn never sees a partial file. Older copies
/// of `bin` are removed; a process still running one keeps its inode.
fn copy(path: &Path, copies: &Path, bin: &str) -> PathBuf {
    let metadata = fs::metadata(path).expect("built binary metadata");
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let name = format!("{bin}-{}-{modified}", metadata.len());
    let target = copies.join(&name);
    if !target.exists() {
        let partial = copies.join(format!(".{name}.partial"));
        fs::copy(path, &partial).expect("copy the built binary");
        fs::rename(&partial, &target).expect("move the copy into place");
    }
    for entry in fs::read_dir(copies).expect("shipped binary directory") {
        let entry = entry.expect("shipped binary entry");
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with(&format!("{bin}-")) && file_name != name {
            let _ = fs::remove_file(entry.path());
        }
    }
    target
}
