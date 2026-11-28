//! Cargo.lock guard: Facet must not host the Nickel VM or its dependency tree.

use std::fs;
use std::path::PathBuf;

fn workspace_lock_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock = manifest.join("../../Cargo.lock");
    assert!(
        lock.is_file(),
        "workspace Cargo.lock missing at {}",
        lock.display()
    );
    lock
}

/// Parse `name = "..."` from each `[[package]]` block in a lockfile.
fn package_names(lock: &str) -> Vec<String> {
    lock.split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            for line in block.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("name = ") else {
                    continue;
                };
                let rest = rest.trim();
                let quoted = rest.strip_prefix('"')?.strip_suffix('"')?;
                return Some(quoted.to_owned());
            }
            None
        })
        .collect()
}

fn forbidden_vm_packages(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            name.as_str() == "nickel-lang-core"
                || name.as_str() == "hedron-ncl"
                || name.starts_with("nickel-lang-")
                || name.starts_with("malachite")
        })
        .cloned()
        .collect()
}

fn assert_no_vm_packages(names: &[String]) {
    let hits = forbidden_vm_packages(names);
    assert!(
        hits.is_empty(),
        "Cargo.lock must not list Nickel VM packages (found: {hits:?})"
    );
}

#[test]
fn lockfile_has_no_nickel_vm() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    let names = package_names(&lock);
    assert!(
        !names.is_empty(),
        "Cargo.lock parse found zero packages (vacuous pass)"
    );
    assert_no_vm_packages(&names);
}

#[test]
fn parser_detects_injected_nickel_lang_core() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    let mut poisoned = lock.clone();
    poisoned.push_str("\n[[package]]\nname = \"nickel-lang-core\"\nversion = \"0.18.0\"\n");
    let names = package_names(&poisoned);
    assert!(
        names.iter().any(|name| name == "nickel-lang-core"),
        "self-test: parser must see injected nickel-lang-core"
    );
    let hits = forbidden_vm_packages(&names);
    assert!(
        hits.iter().any(|name| name == "nickel-lang-core"),
        "self-test: forbidden filter must catch nickel-lang-core"
    );
}
