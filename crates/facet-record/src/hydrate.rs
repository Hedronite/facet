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

use std::net::IpAddr;
use std::path::Path;

use lattice::{
    LatticeConfig, LatticeError, MachineStore, SECRET_KEY_ENV, SecretConfig, WorkspaceStore,
};
use probe_core::{
    Environment, EnvironmentResolutionError, HttpRequest, ResolvedEnvironment, TransportLock,
    apply_path_parameters, discover_request_variables, resolve_environment_with_overrides,
    transport_lock,
};
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
    /// Decided once by [`transport_lock`] from variable names only: how
    /// this request must run transport-side. Beside `redact` so the secret
    /// fields retire or retune together. Not part of `lattice.secrets`.
    pub lock: TransportLock,
    /// The request references a declared secret, including one satisfied
    /// by `--var`, or secret discovery could not run. Not part of
    /// `lattice.secrets`; while run, replay and the TUI still read this
    /// flag directly it is set from [`TransportLock::is_locked`].
    pub references_secret: bool,
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
    /// On a secret-referencing request, the store-resolved URL adds userinfo
    /// or changes the canonical scheme or host. `name` is the store variable.
    /// The store value is not part of this error.
    HostOverride {
        /// Store variable that changed the authority. Never a value.
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
    // One decision for every return path, made from variable names only:
    // no value enters it, and no error carries one. Store rows are keyed by
    // environment, so without a selection only `--var` names count as
    // supplied here.
    let lock = transport_lock(request, environments, environment, |name| {
        user_overrides.iter().any(|(var, _)| var == name)
    });
    let Some(environment) = environment else {
        return Ok(idle(Vec::new(), lock));
    };
    let Some(root) = root else {
        let Ok(referenced) = discover_request_variables(request, environments, Some(environment))
        else {
            return Ok(idle(Vec::new(), lock));
        };
        return Ok(idle(secret_var_redact(&referenced, user_overrides), lock));
    };
    // Discovery errors (unknown environment, bad inheritance) are core's to
    // report at resolve time with the right category. Lock transport anyway.
    let Ok(referenced) = discover_request_variables(request, environments, Some(environment))
    else {
        return Ok(idle(Vec::new(), lock));
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
        return Ok(idle(redact, lock));
    }
    let strict = referenced.iter().any(|info| info.secret);

    // A store beside the collection is the only way to have Lattice values.
    let Some(workspace_root) = WorkspaceStore::discover(root) else {
        return Ok(idle(redact, lock));
    };
    let opened = LatticeConfig::load(&workspace_root)
        .map_err(LatticeError::from)
        .and_then(|config| WorkspaceStore::open_existing(&workspace_root, config));
    let store = match opened {
        Ok(Some(store)) => store,
        Ok(None) => {
            return Ok(idle(redact, lock));
        }
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(idle(redact, lock));
        }
    };
    let machine = match MachineStore::open(store.config()) {
        Ok(machine) => machine,
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(idle(redact, lock));
        }
    };
    let rows = match machine.environments(store.workspace_id()) {
        Ok(rows) => rows,
        Err(error) if strict => return Err(HydrateError::Lattice(error)),
        Err(_) => {
            return Ok(idle(redact, lock));
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
    hydration.lock = lock;
    hydration.references_secret = lock.is_locked();
    hydration.source = if hydration.secrets > 0 {
        Some(backend_label)
    } else if hydration.hydrated.is_empty() {
        None
    } else {
        Some("plain")
    };
    if any_secret
        && let Some(name) = refused_host_name(
            request,
            environments,
            environment,
            &hydration.overrides,
            user_overrides,
        )
    {
        return Err(HydrateError::HostOverride { name });
    }
    Ok(hydration)
}

fn idle(redact: Vec<String>, lock: TransportLock) -> Hydration {
    Hydration {
        redact,
        lock,
        references_secret: lock.is_locked(),
        ..Hydration::default()
    }
}

/// Store variable whose resolved URL changes the pinned authority, if any.
/// The returned string is the variable name. Callers must not format the URL.
fn refused_host_name(
    request: &HttpRequest,
    environments: &[Environment],
    environment: &str,
    store_overrides: &[(String, String)],
    user_overrides: &[(String, String)],
) -> Option<String> {
    let declared = resolved_url(request, environments, environment, user_overrides).ok()??;
    let mut applied = Vec::new();
    for (name, value) in store_overrides {
        if user_overrides.iter().any(|(user, _)| user == name) {
            continue;
        }
        applied.push((name.clone(), value.clone()));
        let mut merged = applied.clone();
        merged.extend(user_overrides.iter().cloned());
        let url = match resolved_url(request, environments, environment, &merged) {
            Ok(Some(url)) => url,
            Ok(None) => return None,
            Err(_) => return Some(name.clone()),
        };
        if hosts_differ(&declared, &url) {
            return Some(name.clone());
        }
    }
    None
}

fn resolved_url(
    request: &HttpRequest,
    environments: &[Environment],
    environment: &str,
    overrides: &[(String, String)],
) -> Result<Option<String>, EnvironmentResolutionError> {
    let Some(template) = request.url.as_deref() else {
        return Ok(None);
    };
    let resolved = resolve_environment_with_overrides(environments, Some(environment), overrides)?;
    let url = interpolate_preserving(template, &resolved)?;
    let mut parameters = request.path_parameters.clone();
    for parameter in &mut parameters {
        parameter.name = interpolate_preserving(&parameter.name, &resolved)?;
        parameter.value = interpolate_preserving(&parameter.value, &resolved)?;
    }
    Ok(Some(apply_path_parameters(&url, &parameters)))
}

/// Substitutes resolved values and leaves missing names in place, including
/// declared secrets that have no value yet.
fn interpolate_preserving(
    template: &str,
    resolved: &ResolvedEnvironment,
) -> Result<String, EnvironmentResolutionError> {
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let Some(end) = expression.find("}}") else {
            return Err(EnvironmentResolutionError::MalformedInterpolation);
        };
        let name = expression[..end].trim();
        if name.is_empty() || name.contains("{{") {
            return Err(EnvironmentResolutionError::MalformedInterpolation);
        }
        if let Some(value) = resolved.variable(name) {
            output.push_str(value);
        } else {
            output.push_str(&remaining[start..start + 2 + end + 2]);
        }
        remaining = &expression[end + 2..];
    }
    output.push_str(remaining);
    Ok(output)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostPin {
    Dns(String),
    Ip(IpAddr),
}

struct AuthorityPin {
    scheme: String,
    host: HostPin,
    port: Option<u16>,
    has_userinfo: bool,
}

fn hosts_differ(declared: &str, overridden: &str) -> bool {
    if declared == overridden {
        return false;
    }
    if authority_slice(declared).is_some_and(authority_forbidden)
        || authority_slice(overridden).is_some_and(authority_forbidden)
    {
        return true;
    }
    match (parse_authority(declared), parse_authority(overridden)) {
        (Some(left), Some(right)) => authority_conflict(&left, &right),
        (None, None) => canonical_blob(declared) != canonical_blob(overridden),
        _ => true,
    }
}

fn authority_slice(url: &str) -> Option<&str> {
    let (_, rest) = url.trim().split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn authority_forbidden(authority: &str) -> bool {
    authority
        .chars()
        .any(|character| character == '\\' || character == '%' || character.is_control())
}

fn authority_conflict(declared: &AuthorityPin, overridden: &AuthorityPin) -> bool {
    if overridden.has_userinfo && !declared.has_userinfo {
        return true;
    }
    declared.scheme != overridden.scheme
        || declared.host != overridden.host
        || effective_port(&declared.scheme, declared.port)
            != effective_port(&overridden.scheme, overridden.port)
}

fn effective_port(scheme: &str, port: Option<u16>) -> Option<u16> {
    port.or(match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    })
}

fn canonical_blob(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut blob = if scheme.is_empty() {
        rest[..end].to_ascii_lowercase()
    } else {
        format!(
            "{}://{}",
            scheme.to_ascii_lowercase(),
            rest[..end].to_ascii_lowercase()
        )
    };
    if blob.ends_with('.') {
        blob.pop();
    }
    blob
}

fn parse_authority(url: &str) -> Option<AuthorityPin> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = match parsed.host()? {
        url::Host::Domain(domain) => {
            let mut dns = domain.to_ascii_lowercase();
            if dns.ends_with('.') {
                dns.pop();
            }
            if dns.is_empty() {
                return None;
            }
            HostPin::Dns(dns)
        }
        url::Host::Ipv4(address) => HostPin::Ip(IpAddr::V4(address)),
        url::Host::Ipv6(address) => HostPin::Ip(IpAddr::V6(address)),
    };
    Some(AuthorityPin {
        scheme: parsed.scheme().to_ascii_lowercase(),
        host,
        port: parsed.port(),
        has_userinfo: !parsed.username().is_empty() || parsed.password().is_some(),
    })
}

fn secret_var_redact(
    referenced: &[probe_core::RequestVariableInfo],
    user_overrides: &[(String, String)],
) -> Vec<String> {
    referenced
        .iter()
        .filter(|info| info.secret)
        .filter_map(|info| {
            user_overrides
                .iter()
                .find(|(name, _)| name == &info.name)
                .map(|(_, value)| value.clone())
        })
        .collect()
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
            lock: probe_core::TransportLock::Open,
            references_secret: false,
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
