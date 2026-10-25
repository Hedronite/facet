//! Secret hydration (Goal 5): overlay Lattice machine-store environment
//! values as `--var` overrides **before** resolve.
//!
//! Three layers, in order. (1) OpenCollection YAML declares a variable,
//! plain or `secret: true`; probe-core refuses to interpolate a declared
//! secret with no value (`secret_variable_unavailable`). (2) The Lattice
//! machine store holds values keyed `(workspace, environment, key)`, plain or
//! through the secrets layer. (3) `--var` is the public override channel and
//! always wins. This module produces layer 2 as overrides placed *before*
//! the user's, so `resolve_environment_with_overrides` sees user values last.
//!
//! Rules: only with a selected environment; only names the request
//! references; never substitute an empty value or the name; values never
//! leave the resolved request (only names are reported).

use std::path::Path;

use lattice::{
    LatticeConfig, LatticeError, MachineStore, SECRET_KEY_ENV, SecretConfig, WorkspaceStore,
};
use probe_core::{Environment, HttpRequest, discover_request_variables};
use serde_json::{Value, json};

/// What the overlay contributed to one resolve.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hydration {
    /// `(name, value)` pairs to place before the user's `--var` overrides.
    /// Values live here only until resolve; never log or store them.
    pub overrides: Vec<(String, String)>,
    /// Names taken from Lattice, in request-reference order.
    pub hydrated: Vec<String>,
    /// How many of `hydrated` came through the secrets layer.
    pub secrets: usize,
    /// `keyring` or `encrypted` when a secret was read, `plain` when only
    /// plaintext rows were used, `None` when nothing was hydrated.
    pub source: Option<&'static str>,
    /// Values of variables the YAML declares `secret` (hydrated or passed
    /// with `--var`), for the recorder to scrub from stored text. Never
    /// logged; only ever compared against.
    pub redact: Vec<String>,
}

impl Hydration {
    /// The `lattice.secrets` field: names only.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({ "hydrated": self.hydrated, "source": self.source })
    }

    /// `hydrated 1 secret from keyring`, or `None` when nothing happened.
    #[must_use]
    pub fn human(&self) -> Option<String> {
        if self.hydrated.is_empty() {
            return None;
        }
        let noun = match (self.secrets, self.hydrated.len()) {
            (1, 1) => "secret".to_owned(),
            (secrets, total) if secrets == total => "secrets".to_owned(),
            (0, 1) => "value".to_owned(),
            (0, _) => "values".to_owned(),
            (secrets, _) => format!("values ({secrets} secret)"),
        };
        Some(format!(
            "hydrated {} {noun} from {}",
            self.hydrated.len(),
            self.source.unwrap_or("lattice")
        ))
    }

    /// Overlay first, user overrides last (they win on collisions).
    #[must_use]
    pub fn merged(&self, user: &[(String, String)]) -> Vec<(String, String)> {
        let mut merged = self.overrides.clone();
        merged.extend(user.iter().cloned());
        merged
    }
}

/// Why hydration could not proceed. Only raised when a referenced variable
/// is declared `secret` in the YAML; otherwise problems are silent and the
/// YAML value (or `secret_variable_unavailable`) applies.
#[derive(Debug)]
pub enum HydrateError {
    /// The secrets backend cannot serve a declared secret (no keyring and no
    /// `FACET_SECRET_KEY`, empty key, wrong key, keyring failure).
    BackendUnavailable {
        /// The variable that needed the backend.
        name: String,
        /// Underlying reason.
        reason: String,
    },
    /// The machine store failed while a declared secret was referenced.
    Lattice(LatticeError),
    /// A machine-store `baseUrl` or `serverUrl` on a request that references
    /// a declared secret. The store value is not read and not applied.
    HostOverride {
        /// Variable name (`baseUrl` or `serverUrl`). Never a value.
        name: String,
    },
}

impl std::fmt::Display for HydrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackendUnavailable { name, reason } => write!(
                f,
                "secret backend unavailable for {name}: {reason}; \
                 set {SECRET_KEY_ENV} or make the OS keyring available"
            ),
            Self::Lattice(error) => write!(f, "{error}"),
            Self::HostOverride { name } => write!(f, "host override refused for {name}"),
        }
    }
}

/// Builds the overlay for `request` resolved in `environment` from the
/// workspace whose collection lives under `root`. Empty when there is no
/// environment, no store, or nothing referenced. `user_overrides` are
/// skipped (they win anyway, and their secrets are never read).
pub fn overlay_secrets(
    root: Option<&Path>,
    environment: Option<&str>,
    request: &HttpRequest,
    environments: &[Environment],
    user_overrides: &[(String, String)],
) -> Result<Hydration, HydrateError> {
    let (Some(root), Some(environment)) = (root, environment) else {
        return Ok(Hydration::default());
    };
    // Discovery errors (unknown environment, bad inheritance) are core's to
    // report at resolve time with the right category; do nothing here.
    let Ok(referenced) = discover_request_variables(request, environments, Some(environment))
    else {
        return Ok(Hydration::default());
    };
    // Includes secrets satisfied by `--var`. Those still make this a
    // secret-referencing request, so a store host must not apply.
    let any_secret = referenced.iter().any(|info| info.secret);
    // A user `--var` for a declared secret is still a secret: remember its
    // value for redaction even though Lattice is not consulted for it.
    let mut redact: Vec<String> = referenced
        .iter()
        .filter(|info| info.secret)
        .filter_map(|info| {
            user_overrides
                .iter()
                .find(|(name, _)| *name == info.name)
                .map(|(_, value)| value.clone())
        })
        .collect();
    let referenced: Vec<_> = referenced
        .into_iter()
        .filter(|info| !user_overrides.iter().any(|(name, _)| *name == info.name))
        .collect();
    if referenced.is_empty() {
        return Ok(Hydration {
            redact,
            ..Hydration::default()
        });
    }
    let strict = referenced.iter().any(|info| info.secret);

    // A store beside the collection is the only way to have Lattice values.
    let Some(workspace_root) = WorkspaceStore::discover(root) else {
        return Ok(Hydration {
            redact,
            ..Hydration::default()
        });
    };
    let opened = LatticeConfig::load(&workspace_root)
        .map_err(LatticeError::from)
        .and_then(|config| WorkspaceStore::open_existing(&workspace_root, config));
    let store = match opened {
        Ok(Some(store)) => store,
        Ok(None) => {
            return Ok(Hydration {
                redact,
                ..Hydration::default()
            });
        }
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(Hydration {
                redact,
                ..Hydration::default()
            });
        }
    };
    let machine = match MachineStore::open(store.config()) {
        Ok(machine) => machine,
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(Hydration {
                redact,
                ..Hydration::default()
            });
        }
    };
    let rows = match machine.environments(store.workspace_id()) {
        Ok(rows) => rows,
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(Hydration {
                redact,
                ..Hydration::default()
            });
        }
    };

    let secret_config = SecretConfig::from_env();
    let backend_label = match std::env::var(SECRET_KEY_ENV) {
        Ok(value) if !value.is_empty() => "encrypted",
        _ => "keyring",
    };
    let plain_config = SecretConfig::keyring();

    let mut hydration = Hydration::default();
    for info in referenced {
        let Some(row) = rows
            .iter()
            .find(|row| row.name == environment && row.key == info.name)
        else {
            continue;
        };
        if any_secret && matches!(row.key.as_str(), "baseUrl" | "serverUrl") {
            return Err(HydrateError::HostOverride { name: info.name });
        }
        let config = if row.secret {
            match &secret_config {
                Ok(config) => config,
                Err(error) if info.secret => {
                    return Err(HydrateError::BackendUnavailable {
                        name: info.name,
                        reason: error.to_string(),
                    });
                }
                Err(_) => continue,
            }
        } else {
            &plain_config
        };
        match machine.environment_with(store.workspace_id(), environment, &info.name, config) {
            Ok(Some(value)) if !value.is_empty() => {
                if row.secret || info.secret {
                    hydration.secrets += usize::from(row.secret);
                    redact.push(value.clone());
                }
                hydration.overrides.push((info.name.clone(), value));
                hydration.hydrated.push(info.name);
            }
            Ok(_) => {}
            Err(LatticeError::Secret(error)) if info.secret => {
                return Err(HydrateError::BackendUnavailable {
                    name: info.name,
                    reason: error.to_string(),
                });
            }
            Err(error) if strict && !matches!(error, LatticeError::Secret(_)) => {
                return Err(HydrateError::Lattice(error));
            }
            Err(_) => {}
        }
    }
    hydration.redact = redact;
    hydration.source = if hydration.secrets > 0 {
        Some(backend_label)
    } else if hydration.hydrated.is_empty() {
        None
    } else {
        Some("plain")
    };
    Ok(hydration)
}

#[cfg(test)]
mod tests {
    use super::Hydration;

    #[test]
    fn user_overrides_come_last_and_human_counts() {
        let hydration = Hydration {
            overrides: vec![("token".to_owned(), "abc".to_owned())],
            hydrated: vec!["token".to_owned()],
            secrets: 1,
            source: Some("encrypted"),
            redact: vec!["abc".to_owned()],
        };
        let merged = hydration.merged(&[("token".to_owned(), "user".to_owned())]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].1, "user", "last one wins in core");
        assert_eq!(
            hydration.human().as_deref(),
            Some("hydrated 1 secret from encrypted")
        );
        assert_eq!(hydration.json()["hydrated"][0], "token");
        assert!(hydration.json().to_string().find("abc").is_none());
        assert!(Hydration::default().human().is_none());
    }
}
