//! `facet env set|list|delete`: Facet's machine-store environment values,
//! keyed by workspace. This is **not** Probe's `environment` command (which
//! edits YAML and rejects secrets); the two stores are different and the
//! names do not collide. Values are written here and read by the hydration
//! overlay at resolve time. No command ever prints a value.
//!
//! `set --value-stdin` reads the value from a pipe instead of argv, where
//! other processes can see it. The buffer is zeroed on drop.

use std::io::{IsTerminal, Read};

use facet_record::Zeroizing;
use lattice::{LatticeConfig, LatticeError, MachineStore, SecretConfig};
use serde_json::{Value, json};

use crate::{
    CommandOutput, FacetError, args,
    presentation::format_utc,
    workspace::{ConfigOverrides, WorkspaceInput, locate_root, open_store},
};

/// The one `env set --value --secret` warning. It never holds the value.
const ARGV_SECRET_WARNING: &str = "--value with --secret puts the value on argv, where other \
     processes on this machine can read it; pipe it to --value-stdin instead";

pub(crate) fn env(args: &[String], stdin: &mut impl Read) -> Result<CommandOutput, FacetError> {
    let Some((verb, rest)) = args.split_first() else {
        return Err(FacetError::invalid_arguments(
            "env requires a subcommand: set, list, or delete",
        ));
    };
    match verb.as_str() {
        "set" => set(rest, stdin),
        "list" => list(rest),
        "delete" => delete(rest),
        other => Err(FacetError::invalid_arguments(format!(
            "unknown env subcommand: {other} (expected set, list, or delete)"
        ))),
    }
}

fn set(args: &[String], stdin: &mut impl Read) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(
        args,
        &["--environment", "--name", "--value"],
        &["--secret", "--value-stdin"],
    )?;
    let path = single_optional_path(parsed.positionals(), "env set")?;
    let environment = required(&parsed, "--environment")?;
    let name = required(&parsed, "--name")?;
    let secret = parsed.switch("--secret");
    let on_argv = parsed.value("--value")?;
    let value = match (on_argv, parsed.switch("--value-stdin")) {
        (Some(_), true) => {
            return Err(FacetError::invalid_arguments(
                "--value and --value-stdin cannot be used together",
            ));
        }
        (Some(value), false) => Zeroizing::new(value.to_owned()),
        (None, true) => value_from_stdin(stdin, std::io::stdin().is_terminal())?,
        (None, false) => {
            return Err(FacetError::invalid_arguments(
                "--value or --value-stdin is required",
            ));
        }
    };

    // `set` may be the first Lattice write for a collection: create the
    // workspace store beside it when none exists yet.
    let (start, root) = locate_root(path);
    let root = match root {
        Some(root) => root,
        None => WorkspaceInput::from_argument(&start.to_string_lossy())
            .base_directory()
            .ok_or_else(|| FacetError::invalid_arguments("env set needs a collection on disk"))?,
    };
    let store = open_store(&root, &ConfigOverrides::default())?;
    let machine = open_machine(&store.config().clone())?;
    if secret {
        // Fail before writing when the backend cannot hold a secret.
        SecretConfig::from_env()
            .map_err(|error| FacetError::lattice(LatticeError::Secret(error)))?;
    }
    machine
        .set_environment(store.workspace_id(), environment, name, &value, secret)
        .map_err(FacetError::lattice)?;
    let row = machine
        .environments(store.workspace_id())
        .map_err(FacetError::lattice)?
        .into_iter()
        .find(|row| row.name == environment && row.key == name)
        .unwrap_or_else(|| lattice::EnvironmentRow {
            name: environment.to_owned(),
            key: name.to_owned(),
            secret,
            updated_at: lattice::now_ms(),
        });
    let output = CommandOutput::new(
        format!(
            "{}/{} set ({}) for workspace {}\n",
            row.name,
            row.key,
            if row.secret { "secret" } else { "plain" },
            store.workspace_id()
        ),
        json!({
            "workspace": workspace_json(&store),
            "entry": entry_json(&row),
        }),
    );
    Ok(if secret && on_argv.is_some() {
        output.warn(ARGV_SECRET_WARNING)
    } else {
        output
    })
}

/// `--value-stdin`: everything on `reader` (stdin) less one trailing `\n`
/// or `\r\n`, in buffers zeroed on drop. Refuses a terminal, since there is
/// no echo-off prompt, an empty value and one that is not UTF-8. No error
/// holds anything that was read.
fn value_from_stdin(
    reader: &mut impl Read,
    is_terminal: bool,
) -> Result<Zeroizing<String>, FacetError> {
    if is_terminal {
        return Err(FacetError::invalid_arguments(
            "--value-stdin reads a pipe, not a terminal: pipe the value in",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(4096));
    reader
        .read_to_end(&mut bytes)
        .map_err(|_| FacetError::invalid_arguments("--value-stdin could not read stdin"))?;
    let keep = if bytes.ends_with(b"\r\n") {
        bytes.len() - 2
    } else if bytes.ends_with(b"\n") {
        bytes.len() - 1
    } else {
        bytes.len()
    };
    bytes.truncate(keep);
    if bytes.is_empty() {
        return Err(FacetError::invalid_arguments(
            "--value-stdin read an empty value",
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        FacetError::invalid_arguments("--value-stdin read a value that is not UTF-8")
    })?;
    Ok(Zeroizing::new(text.to_owned()))
}

fn list(args: &[String]) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(args, &["--environment"], &[])?;
    let path = single_optional_path(parsed.positionals(), "env list")?;
    let filter = parsed.value("--environment")?;
    let (_, root) = locate_root(path);
    let Some(root) = root else {
        return Ok(CommandOutput::new(
            "No Lattice store found; nothing set.\n",
            json!({ "workspace": Value::Null, "entries": [] }),
        ));
    };
    let store = open_store(&root, &ConfigOverrides::default())?;
    let machine = open_machine(&store.config().clone())?;
    let rows: Vec<_> = machine
        .environments(store.workspace_id())
        .map_err(FacetError::lattice)?
        .into_iter()
        .filter(|row| filter.is_none_or(|name| row.name == name))
        .collect();
    let mut lines = vec!["ENVIRONMENT\tNAME\tSECRET\tUPDATED".to_owned()];
    for row in &rows {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            row.name,
            row.key,
            if row.secret { "yes" } else { "no" },
            format_utc(row.updated_at)
        ));
    }
    Ok(CommandOutput::new(
        format!("{}\n", lines.join("\n")),
        json!({
            "workspace": workspace_json(&store),
            "entries": rows.iter().map(entry_json).collect::<Vec<_>>(),
        }),
    ))
}

fn delete(args: &[String]) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(args, &["--environment", "--name"], &[])?;
    let path = single_optional_path(parsed.positionals(), "env delete")?;
    let environment = required(&parsed, "--environment")?;
    let name = required(&parsed, "--name")?;
    let (start, root) = locate_root(path);
    let root = root.ok_or_else(|| FacetError::lattice_not_found(&start))?;
    let store = open_store(&root, &ConfigOverrides::default())?;
    let machine = open_machine(&store.config().clone())?;
    let deleted = machine
        .delete_environment(store.workspace_id(), environment, name)
        .map_err(FacetError::lattice)?;
    Ok(CommandOutput::new(
        if deleted {
            format!("{environment}/{name} deleted\n")
        } else {
            format!("{environment}/{name} was not set\n")
        },
        json!({
            "workspace": workspace_json(&store),
            "environment": environment,
            "name": name,
            "deleted": deleted,
        }),
    ))
}

fn open_machine(config: &LatticeConfig) -> Result<MachineStore, FacetError> {
    MachineStore::open(config).map_err(FacetError::lattice)
}

fn required<'a>(parsed: &'a args::Parsed, flag: &str) -> Result<&'a str, FacetError> {
    parsed
        .value(flag)?
        .ok_or_else(|| FacetError::invalid_arguments(format!("{flag} is required")))
}

fn single_optional_path<'a>(
    positionals: &'a [String],
    command: &str,
) -> Result<Option<&'a str>, FacetError> {
    match positionals {
        [] => Ok(None),
        [path] => Ok(Some(path.as_str())),
        _ => Err(FacetError::invalid_arguments(format!(
            "{command} accepts at most one <path>"
        ))),
    }
}

fn workspace_json(store: &lattice::WorkspaceStore) -> Value {
    json!({
        "id": store.workspace_id(),
        "path": store.root().to_string_lossy(),
    })
}

/// Metadata only. There is no field for the value and there never will be.
fn entry_json(row: &lattice::EnvironmentRow) -> Value {
    json!({
        "environment": row.name,
        "name": row.key,
        "secret": row.secret,
        "updatedAt": row.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY: &str = "h12-stdin-canary";

    fn read(input: &[u8], is_terminal: bool) -> Result<String, FacetError> {
        value_from_stdin(&mut &input[..], is_terminal).map(|value| value.as_str().to_owned())
    }

    fn refused(input: &[u8], is_terminal: bool) -> FacetError {
        let error = read(input, is_terminal).expect_err("should refuse");
        assert_eq!(error.category, "invalid_arguments");
        assert!(!error.message.contains(CANARY), "the value was echoed");
        error
    }

    #[test]
    fn one_trailing_newline_is_stripped() {
        assert_eq!(
            read(format!("{CANARY}\n").as_bytes(), false).unwrap(),
            CANARY
        );
        assert_eq!(
            read(format!("{CANARY}\r\n").as_bytes(), false).unwrap(),
            CANARY
        );
        assert_eq!(read(CANARY.as_bytes(), false).unwrap(), CANARY);
        assert_eq!(
            read(format!("{CANARY}\n\n").as_bytes(), false).unwrap(),
            format!("{CANARY}\n")
        );
        assert_eq!(
            read(format!(" {CANARY} \n").as_bytes(), false).unwrap(),
            format!(" {CANARY} ")
        );
    }

    #[test]
    fn empty_terminal_and_non_utf8_are_refused_without_echo() {
        refused(b"", false);
        refused(b"\n", false);
        refused(b"\r\n", false);
        refused(format!("{CANARY}\n").as_bytes(), true);
        let mut bytes = CANARY.as_bytes().to_vec();
        bytes.push(0xff);
        refused(&bytes, false);
    }
}
