//! One-release deprecation shim: `facet ncl` execs `tetractl` (`TETRA_BIN`).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const EXIT_NOT_FOUND: u8 = 127;

pub fn run(args: &[String]) -> ExitCode {
    ExitCode::from(run_inner(args))
}

fn run_inner(args: &[String]) -> u8 {
    let Some((verb, rest)) = args.split_first() else {
        usage_error();
        return 2;
    };
    let mapped = match verb.as_str() {
        "check" | "apply" | "pack" => verb.as_str(),
        "export" => "eval",
        other => {
            eprintln!(
                "facet: unknown ncl subcommand: {other} (expected check, export, apply, or pack)"
            );
            return 2;
        }
    };

    let tetra_name = env::var("TETRA_BIN").unwrap_or_else(|_| "tetractl".to_owned());
    let Some(tetra_path) = find_executable(&tetra_name) else {
        eprintln!(
            "facet: TETRA_BIN={tetra_name:?} is missing or not executable; install tetractl on PATH or set TETRA_BIN"
        );
        return EXIT_NOT_FOUND;
    };

    eprintln!(
        "facet: 'facet ncl {verb}' is deprecated and will be removed in the next release; use 'tetractl {mapped}'"
    );

    let mut command = Command::new(&tetra_path);
    command.arg(mapped);
    for argument in rest {
        command.arg(argument);
    }
    if env::var_os("FACET_BIN").is_none()
        && let Ok(exe) = env::current_exe()
    {
        command.env("FACET_BIN", exe);
    }
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    #[cfg(unix)]
    {
        let error = command.exec();
        eprintln!("facet: failed to exec {}: {error}", tetra_path.display());
        EXIT_NOT_FOUND
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => status.code().unwrap_or(128 + signal_from_status(status)) as u8,
        Err(error) => {
            eprintln!("facet: failed to run {}: {error}", tetra_path.display());
            EXIT_NOT_FOUND
        }
    }
}

#[cfg(not(unix))]
fn signal_from_status(_status: std::process::ExitStatus) -> i32 {
    0
}

fn usage_error() {
    eprintln!("facet: usage: facet ncl <check|export|apply|pack> [args...]");
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute() || name.contains(std::path::MAIN_SEPARATOR) {
        return is_executable(path).then(|| path.to_path_buf());
    }
    let paths = env::var_os("PATH")?;
    for directory in env::split_paths(&paths) {
        let candidate = directory.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
