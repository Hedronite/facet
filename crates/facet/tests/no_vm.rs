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

fn package_header_count(lock: &str) -> usize {
    lock.matches("[[package]]").count()
}

/// Parse `name = "..."` from each `[[package]]` block in a lockfile.
///
/// Every `[[package]]` header must yield exactly one quoted package name.
fn package_names(lock: &str) -> Vec<String> {
    lock.split("[[package]]")
        .skip(1)
        .map(parse_block_package_name)
        .collect()
}

fn parse_block_package_name(block: &str) -> String {
    for line in block.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("name = ") else {
            continue;
        };
        let rest = rest.trim();
        let Some(quoted) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            continue;
        };
        return quoted.to_owned();
    }
    panic!("Cargo.lock [[package]] block without name = \"...\"");
}

fn assert_parser_covers_every_package_header(lock: &str) {
    let headers = package_header_count(lock);
    let names = package_names(lock);
    assert_eq!(
        names.len(),
        headers,
        "parser must yield one name per [[package]] header (headers={headers}, names={})",
        names.len()
    );
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

fn inject_package_append(lock: &str, name: &str, version: &str) -> String {
    format!("{lock}\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n")
}

/// Insert a package block before the first existing `[[package]]` header.
fn inject_package_first(lock: &str, name: &str, version: &str) -> String {
    let block = format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n");
    match lock.find("[[package]]") {
        Some(index) => format!("{}{}{}", &lock[..index], block, &lock[index..]),
        None => format!("{block}{lock}"),
    }
}

/// `(package name, version)` injected one at a time into a copy of the real lock.
const FORBIDDEN_INJECTIONS: &[(&str, &str)] = &[
    ("nickel-lang-core", "0.18.0"),
    ("hedron-ncl", "0.8.1"),
    ("nickel-lang-foo", "0.1.0"),
    ("malachite-base", "0.6.1"),
];

#[test]
fn forbidden_injection_table_lists_four_distinct_arms() {
    assert_eq!(FORBIDDEN_INJECTIONS.len(), 4);
    let mut names: Vec<&str> = FORBIDDEN_INJECTIONS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        4,
        "each forbidden arm must appear once in the table"
    );
}

#[test]
fn parser_sees_nonempty_package_count_on_real_lock() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    assert_parser_covers_every_package_header(&lock);
    let names = package_names(&lock);
    assert!(
        !names.is_empty(),
        "Cargo.lock parse found zero packages (vacuous pass)"
    );
}

#[test]
fn lockfile_has_no_nickel_vm() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    assert_parser_covers_every_package_header(&lock);
    let names = package_names(&lock);
    assert!(
        !names.is_empty(),
        "Cargo.lock parse found zero packages (vacuous pass)"
    );
    assert_no_vm_packages(&names);
}

#[test]
fn forbidden_filter_flags_each_injected_package_individually() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    let mut exercised = 0usize;
    for &(name, version) in FORBIDDEN_INJECTIONS {
        for (position, poisoned) in [
            ("append", inject_package_append(&lock, name, version)),
            ("prepend", inject_package_first(&lock, name, version)),
        ] {
            assert_parser_covers_every_package_header(&poisoned);
            let names = package_names(&poisoned);
            assert!(
                names.iter().any(|parsed| parsed == name),
                "parser must see {position}-injected package {name:?}"
            );
            let hits = forbidden_vm_packages(&names);
            assert_eq!(
                hits.len(),
                1,
                "expected exactly one forbidden hit for {position}-injected {name:?}, got {hits:?}"
            );
            assert_eq!(
                hits[0], name,
                "forbidden filter must return exact name {name:?} ({position})"
            );
            exercised += 1;
        }
    }
    assert_eq!(
        exercised,
        FORBIDDEN_INJECTIONS.len() * 2,
        "must exercise append and prepend for every table entry"
    );
}

#[test]
fn parser_ignores_stray_name_lines_outside_package_blocks() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");
    let poisoned = format!("name = \"nickel-lang-core\"\n{lock}");
    assert_parser_covers_every_package_header(&poisoned);
    let names = package_names(&poisoned);
    assert!(
        !names.iter().any(|n| n == "nickel-lang-core"),
        "parser must only read name = lines inside [[package]] blocks"
    );
    assert_no_vm_packages(&names);
}

#[test]
fn decoys_do_not_trigger_forbidden_filter() {
    let lock = fs::read_to_string(workspace_lock_path()).expect("read Cargo.lock");

    let with_comment = format!("# name = \"nickel-lang-core\"\n{lock}");

    let with_tower = inject_package_append(&lock, "tower", "0.5.2");

    let with_dep_string = format!(
        "{lock}\n[[package]]\nname = \"decoy-crate\"\nversion = \"1.0.0\"\ndependencies = [\n \"nickel-lang-core 0.1\",\n]\n"
    );

    for (label, text) in [
        ("comment line", with_comment.as_str()),
        ("package tower", with_tower.as_str()),
        ("dependencies string", with_dep_string.as_str()),
    ] {
        assert_parser_covers_every_package_header(text);
        let names = package_names(text);
        let hits = forbidden_vm_packages(&names);
        assert!(
            hits.is_empty(),
            "decoy {label} must not trip forbidden filter (hits: {hits:?}, names: {names:?})"
        );
    }
}
