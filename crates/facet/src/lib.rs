//! Facet command-line adapter.
//!
//! Facet is Probe cut for the terminal with Lattice underneath. Every Probe
//! command is delegated to `probe-cli` verbatim, so the upstream JSON contract
//! (`docs/CLI.md`) holds unchanged. Facet owns four things:
//!
//! - `request run` executes through the same core and HTTP engine as Probe,
//!   then records the run in the workspace Lattice store.
//! - `history`, `blob`, and `gc` read and maintain that store.
//! - `session` starts, ends, lists, and shows agent sessions in the machine store.
//! - `replay` re-sends a recorded run from the current YAML; `diff` compares two runs.
//! - `env` sets and lists machine-store environment values (metadata only on read).
//! - `request run --expect` / `--dry-run`: the assertion (exit 1) and the preview.
//! - `mcp` serves the same functions as Model Context Protocol tools over stdio.
//! - `ncl check` / `ncl export` / `ncl apply` evaluate Nickel in-process
//!   (`hedron-ncl`); Facet is the only Nickel VM on the platform.
//! - `tui` opens the terminal UI.
//!
//! Contract details for the Facet-only commands live in `docs/FACET.md`.

#![forbid(unsafe_code)]

use std::{
    borrow::Cow,
    io::{self, Read},
};

use facet_record::Redactor;
use serde_json::{Value, json};

mod args;
mod diff;
mod doctor;
mod env;
mod error;
mod expect;
mod history;
mod mcp;
pub mod ncl;
mod ncl_apply;
mod ncl_ledger;
mod ncl_pack;
mod pin;
mod presentation;
mod replay;
mod run;
mod session;
mod theme;
mod tui;
mod workspace;

pub use error::FacetError;
pub use mcp::{TOOLS as MCP_TOOLS, run_mcp};
pub use probe_cli::{
    CONFIGURATION_EXIT_CODE, EXECUTION_EXIT_CODE, IMPORT_EXIT_CODE, INVALID_ARGUMENTS_EXIT_CODE,
    INVALID_WORKSPACE_EXIT_CODE, JSON_SCHEMA_VERSION, PERSISTENCE_EXIT_CODE,
    REQUEST_NOT_FOUND_EXIT_CODE,
};
pub use tui::run_tui;

/// Exit code used when the Lattice store cannot be opened, read, or written.
/// Extends the upstream exit-code table; never renumbers it.
pub const LATTICE_EXIT_CODE: u8 = 9;

/// Exit code for the assertion family: the check the caller asked for is
/// false (`replay --frozen` refused, `diff` found differences). Unix's `1`,
/// as in `test`, `grep`, `diff`, `cmp`; unused upstream (constants start at
/// 2), so it never collides.
pub const ASSERTION_EXIT_CODE: u8 = 1;

/// Captured CLI output and process status. `stdout` is bytes because
/// `facet blob` writes a raw body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutput {
    /// Command output written to stdout.
    pub stdout: Vec<u8>,
    /// Diagnostics written to stderr.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: u8,
}

impl RunOutput {
    fn success(stdout: Vec<u8>, stderr: String) -> Self {
        Self {
            stdout,
            stderr,
            exit_code: 0,
        }
    }

    fn failure(error: FacetError, json_output: bool) -> Self {
        if json_output {
            Self {
                stdout: pretty_json(&error.envelope()).into_bytes(),
                stderr: String::new(),
                exit_code: error.exit_code,
            }
        } else {
            Self {
                stdout: Vec::new(),
                stderr: format!("error[{}]: {}\n", error.category, error.message),
                exit_code: error.exit_code,
            }
        }
    }
}

/// A command's human and JSON renderings plus any stderr diagnostics.
#[derive(Debug)]
pub(crate) struct CommandOutput {
    human: Vec<u8>,
    json: Value,
    warnings: Vec<String>,
    /// `error[category]: message` lines for stderr (suppressed by --quiet).
    errors: Vec<String>,
    exit_code: u8,
}

impl CommandOutput {
    pub(crate) fn new(human: impl Into<Vec<u8>>, json: Value) -> Self {
        Self {
            human: human.into(),
            json,
            warnings: Vec::new(),
            errors: Vec::new(),
            exit_code: 0,
        }
    }

    /// A complete success document that nonetheless failed the caller's
    /// assertion (`--expect`): the document stays, `error` is added to the
    /// JSON, the human view gains an `error[...]` line on stderr, and the
    /// process exits with the error's code.
    pub(crate) fn fail(mut self, error: FacetError) -> Self {
        let mut envelope = json!({
            "category": error.category,
            "exitCode": error.exit_code,
            "message": error.message,
        });
        if let Some(details) = error.details {
            envelope["details"] = details;
        }
        self.errors.push(format!(
            "error[{}]: {}
",
            error.category, error.message
        ));
        self.json["error"] = envelope;
        self.exit_code = error.exit_code;
        self
    }

    pub(crate) fn warn(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// The JSON document, warnings, and exit code, for adapters that do not
    /// print (the MCP server).
    pub(crate) fn into_parts(self) -> (Value, Vec<String>, u8) {
        (self.json, self.warnings, self.exit_code)
    }

    /// The known-values pass over everything this output prints: every
    /// JSON string, the human view, warnings and `error[...]` lines.
    pub(crate) fn redact(mut self, redactor: &Redactor) -> Self {
        redact_strings(&mut self.json, &|text| owned(redactor.text(text)));
        if let Cow::Owned(human) = redactor.bytes(&self.human) {
            self.human = human;
        }
        for line in self.warnings.iter_mut().chain(self.errors.iter_mut()) {
            *line = error::redact_message(redactor, line);
        }
        self
    }

    /// A successful document that still signals an assertion outcome
    /// (`diff` differs). Output is rendered as usual; only the code changes.
    pub(crate) fn with_exit_code(mut self, exit_code: u8) -> Self {
        self.exit_code = exit_code;
        self
    }

    fn render(self, json_output: bool, quiet: bool) -> RunOutput {
        let stdout = if quiet {
            Vec::new()
        } else if json_output {
            pretty_json(&versioned_json(self.json)).into_bytes()
        } else {
            self.human
        };
        let mut stderr: String = self
            .warnings
            .iter()
            .map(|warning| format!("warning: {warning}\n"))
            .collect();
        // The JSON document carries `error`; the human view says it on stderr.
        if !quiet && !json_output {
            stderr.extend(self.errors.iter().map(String::as_str));
        }
        RunOutput {
            stdout,
            stderr,
            exit_code: self.exit_code,
        }
    }
}

/// Returns the stable top-level help text.
#[must_use]
pub const fn help() -> &'static str {
    concat!(
        "Facet - Probe, cut for the terminal, with Lattice underneath\n",
        "\n",
        "Usage: facet [OPTIONS] <COMMAND>\n",
        "\n",
        "Every Probe command works unchanged (see `probe --help`). Facet adds:\n",
        "\n",
        "Commands:\n",
        "  request run <path> <selector>       Execute an HTTP request and record it in Lattice\n",
        "      [--expect <codes>] [--dry-run]  Assert the status (exit 1 on miss) or preview only\n",
        "  history [<path>]                    List recorded runs, newest first (metadata only)\n",
        "  history [<path>] --id <ulid>        Show one recorded run by id\n",
        "  last [<path>] [filters]             Newest matching run; prints its ULID (same filters as history)\n",
        "  pin <runId> --as <name> [<path>]    Name a run in the machine store\n",
        "  pin get <name> [<path>]             Print a pinned run id (dangling reported in JSON)\n",
        "  pin list [<path>] | pin delete <name>\n",
        "  history [<path>] --sql <query>      Run read-only SQL against the workspace store\n",
        "  session start                       Start a session; prints its ULID (use with FACET_SESSION)\n",
        "  session end [<id>|current]          End a session (default: $FACET_SESSION); idempotent\n",
        "  session list                        List sessions, newest first\n",
        "  session show <id>|current           Show one session\n",
        "  replay <runId> [<path>]             Re-send a recorded run from the current YAML\n",
        "  diff <idA> <idB> [<path>]           Compare two recorded runs, hashes first (exit 1 if different)\n",
        "  env set [<path>] --environment <e> --name <k> --value <v> [--secret]\n",
        "                                      Store a value in the Facet machine store (hydrated at resolve)\n",
        "  env list [<path>] [--environment <e>] List stored values (metadata only, never values)\n",
        "  env delete [<path>] --environment <e> --name <k>\n",
        "  doctor [--probe]                   Preflight: machine/workspace stores, secret backend, FACET_* env\n",
        "  blob <hash> [<path>]                Fetch one stored body by SHA-256\n",
        "  gc [<path>] [--yes]                 Expire old runs and sweep orphaned blobs\n",
        "  theme check <path>                  Validate a facet-tui theme file (exit 1 if invalid)\n",
        "  theme list                          List built-in and discovered theme files\n",
        "  tui [<path>]                        Open the terminal UI\n",
        "  mcp                                 Serve the same commands as MCP tools over stdio\n",
        "\n",
        "Options:\n",
        "      --no-record             Execute without writing to Lattice\n",
        "      --expect <codes>        Assert the status: 200,201 or 2xx; miss is exit 1 expect_failed\n",
        "      --dry-run               Resolve and preview the request without sending or recording\n",
        "      --no-subprocess         Start no gh or git child (request run, replay); {{githubToken}} needs --var\n",
        "      --tag <tag>             Tag the recorded run, or filter history by tag (AND); may be repeated\n",
        "      --inline-body-max <n>   Inline bodies at or under this size (e.g. 64KiB)\n",
        "      --history-retention <r> Retention window for gc (unlimited or e.g. 30d)\n",
        "      --limit <n>             Maximum history rows (default 50)\n",
        "      --request <selector>    Only history for one request\n",
        "      --status <code>         Only history with one HTTP status\n",
        "      --actor <name>          Only history or sessions by one actor; session start actor\n",
        "      --since <unix-ms>       Only history started at or after this time\n",
        "      --session <id>|current  Only history recorded in one session\n",
        "      --environment <name>    Only history resolved with one environment\n",
        "      --hash <sha256>         Only history whose request, request body, or response body hash matches\n",
        "      --id <ulid>             One run by id (exclusive with the filters above)\n",
        "      --bodies                Include inline bodies in history JSON; unified body diff for diff\n",
        "      --frozen                Refuse to replay when the request hash changed (exit 1)\n",
        "      --secret                Store the env value through the secrets layer (env set)\n",
        "      --meta <json>           Session metadata object (session start)\n",
        "      --open                  Only sessions still open (session list)\n",
        "      --output <file>         Write a blob to a file instead of stdout\n",
        "      --yes                   Apply gc deletions (default is a dry run)\n",
        "      --appearance <name>     TUI appearance: graphite or porcelain\n",
        "      --json                  Emit versioned deterministic JSON\n",
        "  -q, --quiet                 Suppress successful command output\n",
        "  -h, --help                  Print help\n",
        "  -V, --version               Print version\n",
        "\n",
        "Environment: FACET_ACTOR (run actor, default human), FACET_SESSION (session id),\n",
        "FACET_NO_RECORD=1 (never record), FACET_DATA_DIR / FACET_CONFIG_DIR (machine store paths),\n",
        "FACET_SECRET_KEY (encrypted secrets instead of the OS keyring), FACET_NO_SUBPROCESS=1\n",
        "(no gh or git child), FACET_GH / FACET_GIT (absolute path to that program).\n",
    )
}

/// Returns the Facet version, sourced from the Cargo workspace version.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Runs the CLI adapter for arguments that exclude the executable name.
#[must_use]
pub fn run<I, S>(args: I) -> RunOutput
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    run_with_stdin(args, &mut io::empty())
}

/// Runs the CLI adapter with a reader used when the workspace path is `-`.
#[must_use]
pub fn run_with_stdin<I, S, R>(args: I, stdin: &mut R) -> RunOutput
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    R: Read,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect();

    let owned = match args.first().map(String::as_str) {
        None => true,
        Some(
            "history" | "last" | "pin" | "mcp" | "session" | "replay" | "diff" | "env" | "doctor"
            | "blob" | "gc" | "theme" | "tui" | "ncl" | "-V" | "--version" | "-h" | "--help",
        ) => true,
        Some("request") => args.get(1).map(String::as_str) == Some("run"),
        Some(_) => false,
    };
    if !owned {
        let output = probe_cli::run_with_stdin(args, stdin);
        return RunOutput {
            stdout: output.stdout.into_bytes(),
            stderr: output.stderr,
            exit_code: output.exit_code,
        };
    }

    let mut args = args;
    let (json_output, quiet) = match strip_global_flags(&mut args) {
        Ok(flags) => flags,
        Err((error, json_output)) => return RunOutput::failure(error, json_output),
    };

    if args.is_empty() || args == ["-h"] || args == ["--help"] {
        return RunOutput::success(help().as_bytes().to_vec(), String::new());
    }
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return RunOutput::success(help().as_bytes().to_vec(), String::new());
    }
    if args == ["-V"] || args == ["--version"] {
        let output = CommandOutput::new(
            format!("facet {} (probe {})\n", version(), probe_cli::version()),
            json!({
                "name": "facet",
                "version": version(),
                "probeVersion": probe_cli::version(),
            }),
        );
        return output.render(json_output, quiet);
    }

    let result = match args[0].as_str() {
        "request" => run::run(&args[2..], stdin),
        "history" => history::history(&args[1..]),
        "last" => history::last(&args[1..]),
        "pin" => pin::pin(&args[1..]),
        "session" => session::session(&args[1..]),
        "replay" => replay::replay(&args[1..], stdin),
        "diff" => diff::diff(&args[1..]),
        "env" => env::env(&args[1..]),
        "doctor" => doctor::doctor(&args[1..]),
        "blob" => history::blob(&args[1..]),
        "gc" => history::gc(&args[1..]),
        "theme" => theme::theme(&args[1..]),
        "ncl" => args::ncl(&args[1..]),
        "tui" => Err(FacetError::invalid_arguments(
            "tui is interactive and must be started from the facet binary",
        )),
        "mcp" => Err(FacetError::invalid_arguments(
            "mcp serves stdio and must be started from the facet binary",
        )),
        _ => unreachable!("owned commands are routed above"),
    };
    match result {
        Ok(output) => output.render(json_output, quiet),
        Err(error) => RunOutput::failure(error, json_output),
    }
}

/// Removes `--json` and `--quiet` with upstream's exact rules.
fn strip_global_flags(args: &mut Vec<String>) -> Result<(bool, bool), (FacetError, bool)> {
    let json_count = remove_flags(args, &["--json"]);
    let json_output = json_count == 1;
    let quiet_count = remove_flags(args, &["-q", "--quiet"]);
    let quiet = quiet_count == 1;
    if json_count > 1 {
        return Err((
            FacetError::invalid_arguments("--json may only be specified once"),
            true,
        ));
    }
    if quiet_count > 1 {
        return Err((
            FacetError::invalid_arguments("--quiet may only be specified once"),
            json_output,
        ));
    }
    if json_output && quiet {
        return Err((
            FacetError::invalid_arguments("--json and --quiet cannot be used together"),
            true,
        ));
    }
    Ok((json_output, quiet))
}

fn remove_flags(args: &mut Vec<String>, flags: &[&str]) -> usize {
    let count = args
        .iter()
        .filter(|argument| flags.contains(&argument.as_str()))
        .count();
    args.retain(|argument| !flags.contains(&argument.as_str()));
    count
}

/// Replaces each string in `value` (not object keys) for which `redact`
/// returns a new one.
pub(crate) fn redact_strings(value: &mut Value, redact: &dyn Fn(&str) -> Option<String>) {
    match value {
        Value::String(text) => {
            if let Some(redacted) = redact(text) {
                *text = redacted;
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_strings(item, redact);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_strings(item, redact);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// `Some` when a redaction changed `text`.
pub(crate) fn owned(text: Cow<'_, str>) -> Option<String> {
    match text {
        Cow::Owned(text) => Some(text),
        Cow::Borrowed(_) => None,
    }
}

pub(crate) fn versioned_json(mut value: Value) -> Value {
    value
        .as_object_mut()
        .expect("command JSON output must be an object")
        .insert("schemaVersion".to_owned(), json!(JSON_SCHEMA_VERSION));
    value
}

fn pretty_json(value: &Value) -> String {
    let mut output =
        serde_json::to_string_pretty(value).expect("JSON value serialization cannot fail");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::{INVALID_ARGUMENTS_EXIT_CODE, run};

    #[test]
    fn version_names_both_binaries() {
        let output = run(["--version", "--json"]);
        assert_eq!(output.exit_code, 0);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["name"], "facet");
        assert_eq!(value["schemaVersion"], 1);
        assert!(value["probeVersion"].is_string());
    }

    /// `scripts/version-check.sh` parses this exact human line to gate releases
    /// (git tag == Cargo == binary). Reformatting it silently breaks that gate,
    /// so the shape is pinned here rather than only in the JSON payload.
    #[test]
    fn version_human_line_stays_gateable() {
        let output = run(["--version"]);
        assert_eq!(output.exit_code, 0);
        let human = String::from_utf8(output.stdout).unwrap();
        let line = human.lines().next().expect("--version prints a line");

        let facet_version = super::version();
        let probe_version = probe_cli::version();
        assert_eq!(
            line,
            format!("facet {facet_version} (probe {probe_version})")
        );

        // The gate reads the version as the second whitespace-separated field.
        let mut fields = line.split_whitespace();
        assert_eq!(fields.next(), Some("facet"));
        assert_eq!(fields.next(), Some(facet_version));
    }

    /// The Facet CLI and the vendored Probe cut are independent version axes.
    /// Neither is ever bumped just to make the parenthetical read symmetrical,
    /// so this asserts the line reports each crate's own version, not one twice.
    #[test]
    fn version_axes_are_reported_independently() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));

        let output = run(["--version", "--json"]);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["probeVersion"], probe_cli::version());
    }

    #[test]
    fn delegates_unknown_commands_to_probe() {
        let output = run(["collection", "validate", "/nonexistent/path.yml", "--json"]);
        assert_ne!(output.exit_code, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("invalid_workspace"));
    }

    #[test]
    fn rejects_json_with_quiet_like_upstream() {
        let output = run(["history", "--json", "--quiet"]);
        assert_eq!(output.exit_code, INVALID_ARGUMENTS_EXIT_CODE);
        assert!(String::from_utf8_lossy(&output.stdout).contains("invalid_arguments"));
    }
}
