//! `facet doctor`: preflight checks for the Lattice stores and the secret
//! backend. Reports, never fixes. Exit 0 healthy, 1 warnings, 9 when the
//! machine store cannot be opened. `--probe` round-trips a throwaway
//! secret through the secrets layer to test usability; otherwise usability
//! is reported as null (not checked). No value or secret_ref is ever printed.

use lattice::{
    LatticeConfig, MachineStore, SecretConfig, WorkspaceStore, machine_config_dir, machine_data_dir,
};
use serde_json::{Value, json};

use crate::{ASSERTION_EXIT_CODE, CommandOutput, FacetError, LATTICE_EXIT_CODE, args};

const DOCTOR_FLAGS: &[&str] = &[];
const DOCTOR_SWITCHES: &[&str] = &["--probe"];

pub(crate) fn doctor(args: &[String]) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(args, DOCTOR_FLAGS, DOCTOR_SWITCHES)?;
    if !parsed.positionals().is_empty() {
        return Err(FacetError::invalid_arguments(
            "doctor takes no positional arguments (only --probe, --json, --quiet)",
        ));
    }
    let probe = parsed.switch("--probe");

    let mut warnings: Vec<String> = Vec::new();
    let data_dir = machine_data_dir();
    let config_dir = machine_config_dir();
    let db_path = data_dir.as_ref().map(|dir| dir.join(lattice::DB_FILE));

    let config = LatticeConfig::default();

    // The machine store is the one thing that can force a non-zero exit on its
    // own: when it cannot be opened, Lattice is unreachable (exit 9).
    let (machine_json, machine_open) = match MachineStore::open(&config) {
        Ok(store) => {
            let json = json!({
                "opened": true,
                "dataDir": data_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                "configDir": config_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                "dbPath": db_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                "schemaVersion": store.schema_version().ok(),
            });
            (json, Some(store))
        }
        Err(error) => {
            let machine_json = json!({
                "opened": false,
                "dataDir": data_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                "configDir": config_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                "dbPath": db_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            });
            let doctor_json = json!({
                "machine": machine_json,
                "workspace": json!({ "found": false }),
                "secrets": json!({ "backend": "unknown", "usable": probe.then_some(false), "probe": probe }),
                "env": env_json(),
            });
            warnings.push(format!("machine store unreachable: {error}"));
            return Ok(finish(doctor_json, warnings, LATTICE_EXIT_CODE));
        }
    };

    // Workspace store discovered from the current directory. Missing is a
    // warning, not fatal: a fresh checkout has no `.facet` until a run lands.
    let mut workspace_json = json!({ "found": false });
    let cwd = std::path::Path::new(".");
    match WorkspaceStore::discover(cwd) {
        Some(root) => match WorkspaceStore::open(&root, config.clone()) {
            Ok(store) => {
                workspace_json = json!({
                    "found": true,
                    "id": store.workspace_id(),
                    "path": store.root().to_string_lossy(),
                    "schemaVersion": store.schema_version().ok(),
                });
            }
            Err(error) => {
                warnings.push(format!("workspace store found but not openable: {error}"));
                workspace_json = json!({ "found": true, "path": root.to_string_lossy() });
            }
        },
        None => warnings.push("no workspace store found from the current directory".into()),
    }

    // Secret backend in use (keyring vs encrypted) from the environment.
    // `FACET_SECRET_KEY` set and empty is encrypted-but-unusable; surfaced
    // as usable=false rather than an error.
    let (backend, usable): (&'static str, Option<bool>) = match SecretConfig::from_env() {
        Ok(secret_config) => {
            let backend = if secret_config.is_encrypted() {
                "encrypted"
            } else {
                "keyring"
            };
            let usable = if probe {
                Some(probe_secret_usable(&secret_config, &mut warnings))
            } else {
                None
            };
            (backend, usable)
        }
        Err(lattice::SecretError::EmptySecretKey) => {
            warnings.push(format!(
                "{} is set but empty; secrets backend is encrypted but unusable",
                lattice::SECRET_KEY_ENV,
            ));
            ("encrypted", probe.then_some(false))
        }
        Err(error) => {
            warnings.push(format!("secret backend could not be resolved: {error}"));
            ("unknown", probe.then_some(false))
        }
    };
    let _ = machine_open; // machine store presence does not change backend usability

    let secrets_json = json!({
        "backend": backend,
        "usable": usable,
        "probe": probe,
    });

    let doctor_json = json!({
        "machine": machine_json,
        "workspace": workspace_json,
        "secrets": secrets_json,
        "env": env_json(),
    });
    let exit_code = if warnings.is_empty() {
        0
    } else {
        ASSERTION_EXIT_CODE
    };
    Ok(finish(doctor_json, warnings, exit_code))
}

/// Whether `FACET_NO_SUBPROCESS` turns every child process off: any
/// non-empty value. `--no-subprocess` is per command, so doctor never sees it.
fn no_subprocess() -> bool {
    std::env::var_os("FACET_NO_SUBPROCESS").is_some_and(|value| !value.is_empty())
}

/// `FACET_*` presence (names only; values are never printed by any command).
fn env_json() -> Value {
    json!({
        "FACET_ACTOR": std::env::var_os("FACET_ACTOR").is_some(),
        "FACET_SESSION": std::env::var_os("FACET_SESSION").is_some(),
        "FACET_DATA_DIR": std::env::var_os("FACET_DATA_DIR").is_some(),
        "FACET_CONFIG_DIR": std::env::var_os("FACET_CONFIG_DIR").is_some(),
        "FACET_NO_RECORD": std::env::var_os("FACET_NO_RECORD").is_some(),
        "FACET_NO_SUBPROCESS": no_subprocess(),
        "FACET_SECRET_KEY": std::env::var_os("FACET_SECRET_KEY").is_some(),
    })
}

/// Round-trips a throwaway secret through the secrets layer to test usability.
/// Uses a fresh keyring account / encrypted blob, then deletes it. Best
/// effort: a failure sets `usable=false` and pushes a warning.
fn probe_secret_usable(config: &SecretConfig, warnings: &mut Vec<String>) -> bool {
    const PROBE_VALUE: &str = "facet-doctor-probe-not-a-real-secret";
    match lattice::put_secret_with(PROBE_VALUE, config) {
        Ok(stored) => {
            let got = lattice::get_secret_with(&stored.reference, config);
            let _ = lattice::delete_secret_with(&stored.reference, config);
            match got {
                Ok(Some(round_tripped)) if round_tripped.as_str() == PROBE_VALUE => true,
                Ok(_) => {
                    // Never print what came back: it is a stored value.
                    warnings.push("secret backend round-trip mismatch; marked unusable".to_owned());
                    false
                }
                Err(error) => {
                    warnings.push(format!("secret backend read-back failed: {error}"));
                    false
                }
            }
        }
        Err(error) => {
            warnings.push(format!("secret backend probe failed: {error}"));
            false
        }
    }
}

fn finish(doctor_json: Value, warnings: Vec<String>, exit_code: u8) -> CommandOutput {
    let human = doctor_human(&doctor_json, &warnings);
    let mut output =
        CommandOutput::new(human, json!({ "doctor": doctor_json })).with_exit_code(exit_code);
    output = warnings
        .iter()
        .fold(output, |acc, warning| acc.warn(warning.clone()));
    output
}

fn doctor_human(doctor: &Value, warnings: &[String]) -> Vec<u8> {
    let machine = &doctor["machine"];
    let workspace = &doctor["workspace"];
    let secrets = &doctor["secrets"];
    let mut lines = Vec::new();
    lines.push("Facet doctor".into());
    lines.push(format!(
        "  machine store: {}",
        if machine["opened"].as_bool() == Some(true) {
            format!("schema v{}", machine["schemaVersion"].as_i64().unwrap_or(0))
        } else {
            "not openable".into()
        }
    ));
    lines.push(format!(
        "  workspace: {}",
        if workspace["found"].as_bool() == Some(true) {
            workspace["id"]
                .as_str()
                .map(|id| format!("id {id}"))
                .unwrap_or_else(|| "found but no id".into())
        } else {
            "not found from cwd".into()
        }
    ));
    let backend = secrets["backend"].as_str().unwrap_or("unknown");
    let usable = match secrets["usable"] {
        Value::Bool(true) => "usable",
        Value::Bool(false) => "not usable",
        _ => "usability not checked (pass --probe)",
    };
    lines.push(format!("  secrets: backend {backend}, {usable}"));
    lines.push(format!(
        "  child processes: {} (FACET_NO_SUBPROCESS)",
        if no_subprocess() { "off" } else { "on" }
    ));
    lines.push("  env: FACET_* presence checked (values never printed)".into());
    if warnings.is_empty() {
        lines.push("healthy".into());
    } else {
        lines.push(format!("{} warning(s)", warnings.len()));
        for warning in warnings {
            lines.push(format!("  - {warning}"));
        }
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n").into_bytes()
}
