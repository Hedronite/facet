//! One place for every subprocess Facet spawns: program resolution, the
//! child environment, and the disable switch. Product code adds no other
//! `Command::new`; `github.rs` (both `gh` spawns) and `git_tag` route
//! through here.
//!
//! A program resolves from a fixed candidate list, or from an absolute
//! `FACET_GH` / `FACET_GIT` that names an existing regular file, or it is
//! refused before it spawns. `PATH` is never searched: whatever a stub test
//! plants on `PATH` cannot run, and whatever a run's environment holds
//! cannot reach a child.
//!
//! When [`disable`] has been called, or `FACET_NO_SUBPROCESS` is
//! non-empty, every spawn is refused with [`ChildError::Disabled`] before
//! the child exists; `git_tag` then records no tag and the `gh` fallback
//! stays out of the run.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

static DISABLED: AtomicBool = AtomicBool::new(false);

/// One-way, process-wide disable latch. `--no-subprocess` (and any
/// surface that refuses children in principle) calls it once; nothing
/// turns it back on, because the latch only ever turns more off.
pub fn disable() {
    DISABLED.store(true, Ordering::SeqCst);
}

/// Whether the disable switch is on: the one-way latch from [`disable`],
/// or a non-empty `FACET_NO_SUBPROCESS` in the environment.
#[must_use]
pub fn disabled() -> bool {
    DISABLED.load(Ordering::SeqCst)
        || std::env::var_os("FACET_NO_SUBPROCESS").is_some_and(|value| !value.is_empty())
}

/// The programs Facet is allowed to run at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// `gh` from the GitHub CLI.
    Gh,
    /// `git` for the recorded workspace tag.
    Git,
}

impl Program {
    /// The absolute-path override that replaces the fixed list.
    #[must_use]
    fn override_var(self) -> &'static str {
        match self {
            Self::Gh => "FACET_GH",
            Self::Git => "FACET_GIT",
        }
    }

    /// The fixed candidates, in order; the first regular file wins.
    #[must_use]
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Self::Gh => &["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"],
            Self::Git => &[
                "/usr/bin/git",
                "/opt/homebrew/bin/git",
                "/usr/local/bin/git",
            ],
        }
    }
}

/// Why a child was refused before it spawned. Neither variant ever names
/// a path: a caller-supplied path may be confidential, and the category
/// contract must hold for every program and every surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildError {
    /// The disable switch: [`disable`] or non-empty `FACET_NO_SUBPROCESS`.
    Disabled,
    /// A relative, missing or non-regular-file override, or no candidate
    /// on the fixed list. The fix is `FACET_GH` / `FACET_GIT`.
    PathRefused,
}

impl ChildError {
    /// Stable programmatic category.
    #[must_use]
    pub fn category(self) -> &'static str {
        match self {
            Self::Disabled => "subprocess_disabled",
            Self::PathRefused => "subprocess_path_refused",
        }
    }
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(
                formatter,
                "subprocess disabled by FACET_NO_SUBPROCESS (or --no-subprocess)"
            ),
            Self::PathRefused => write!(
                formatter,
                "subprocess path refused; set FACET_GH or FACET_GIT to an absolute path to an existing regular file"
            ),
        }
    }
}

impl std::error::Error for ChildError {}

/// A resolved program. Construct with [`child`]; the resolved path stays
/// private, because it never travels in an error or a message.
#[derive(Clone, Debug)]
pub struct Child {
    program: Program,
    path: PathBuf,
}

impl Child {
    /// The program this child resolves to.
    #[must_use]
    pub fn program(&self) -> Program {
        self.program
    }

    /// A preconfigured [`Command`] for the resolved path: environment
    /// cleared, then exactly `HOME` and `GH_CONFIG_DIR` when set, plus
    /// `PATH=/usr/bin:/bin`; stdin null by default. Nothing else reaches
    /// the child, and no known value can reach its argv, stdin or env
    /// through this module.
    #[must_use]
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command.env_clear();
        if let Some(home) = std::env::var_os("HOME") {
            command.env("HOME", home);
        }
        if let Some(config) = std::env::var_os("GH_CONFIG_DIR") {
            command.env("GH_CONFIG_DIR", config);
        }
        command.env("PATH", "/usr/bin:/bin");
        command.stdin(Stdio::null());
        command
    }
}

/// Resolves `program`, honouring the disable switch, its absolute-path
/// override and the fixed list. `Err` is only for the switch or a bad
/// override; no candidate present on the fixed list and no override is
/// `Ok(None)`, like a spawn that cannot find its file on the base.
pub fn child(program: Program) -> Result<Option<Child>, ChildError> {
    if disabled() {
        return Err(ChildError::Disabled);
    }
    if let Some(given) = std::env::var_os(program.override_var()) {
        let path = PathBuf::from(given);
        if !path.is_absolute() || !regular_file(&path) {
            return Err(ChildError::PathRefused);
        }
        return Ok(Some(Child { program, path }));
    }
    Ok(program
        .candidates()
        .iter()
        .find(|candidate| regular_file(Path::new(candidate)))
        .map(|candidate| Child {
            program,
            path: PathBuf::from(*candidate),
        }))
}

/// Runs `program` with `args` to completion: stdin null, stdout and stderr
/// piped. `Err` only when the switch is on or the path is refused; a
/// spawn that fails for any other reason is `Ok(None)`, as on the base.
pub fn output<I>(program: Program, args: I) -> Result<Option<Output>, ChildError>
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    let Some(child) = child(program)? else {
        return Ok(None);
    };
    let Ok(output) = child.command().args(args).output() else {
        return Ok(None);
    };
    Ok(Some(output))
}

/// An existing regular file, following symlinks. `PATH` plays no part.
fn regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}
