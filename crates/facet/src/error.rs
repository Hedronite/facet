//! Facet error contract. Categories and exit codes for delegated behavior
//! mirror `probe-cli`'s `error.rs` exactly; Facet adds `lattice_*`,
//! `blob_not_found`, `run_not_found`, `session_not_found`, `session_not_set`,
//! `invalid_sql`, and `sql_read_only`.

use facet_record::{HydrateError, PolicyError, Redactor};
use lattice::LatticeError;
use probe_core::EnvironmentResolutionError;
use probe_http::HttpError;
use serde_json::Value;

use crate::{
    ASSERTION_EXIT_CODE, CONFIGURATION_EXIT_CODE, EXECUTION_EXIT_CODE, INVALID_ARGUMENTS_EXIT_CODE,
    INVALID_WORKSPACE_EXIT_CODE, LATTICE_EXIT_CODE, REQUEST_NOT_FOUND_EXIT_CODE,
};
use serde_json::json;

/// A structured command failure.
#[derive(Debug)]
pub struct FacetError {
    /// Stable programmatic category.
    pub category: &'static str,
    /// Human diagnostic; not part of the stable contract.
    pub message: String,
    /// Process exit code.
    pub exit_code: u8,
    /// Optional structured details.
    pub details: Option<Value>,
}

impl FacetError {
    fn new(category: &'static str, message: impl Into<String>, exit_code: u8) -> Self {
        Self {
            category,
            message: message.into(),
            exit_code,
            details: None,
        }
    }

    /// The versioned JSON error document (`schemaVersion` + `error`).
    #[must_use]
    pub fn envelope(&self) -> Value {
        let mut value = json!({
            "schemaVersion": crate::JSON_SCHEMA_VERSION,
            "error": {
                "category": self.category,
                "exitCode": self.exit_code,
                "message": self.message,
            }
        });
        if let Some(details) = &self.details {
            value["error"]["details"] = details.clone();
        }
        value
    }

    /// The message and every string in `details` through
    /// [`redact_message`].
    #[must_use]
    pub(crate) fn redact(mut self, redactor: &Redactor) -> Self {
        self.message = redact_message(redactor, &self.message);
        if let Some(details) = &mut self.details {
            crate::redact_strings(details, &|text| {
                let redacted = redact_message(redactor, text);
                (redacted != text).then_some(redacted)
            });
        }
        self
    }

    /// Attaches structured details.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message, INVALID_ARGUMENTS_EXIT_CODE)
    }

    pub(crate) fn invalid_workspace(message: impl Into<String>) -> Self {
        Self::new("invalid_workspace", message, INVALID_WORKSPACE_EXIT_CODE)
    }

    pub(crate) fn request_not_found(selector: &str) -> Self {
        Self::new(
            "request_not_found",
            format!("request selector not found: {selector}"),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
    }

    pub(crate) fn blob_not_found(hash: &str) -> Self {
        Self::new(
            "blob_not_found",
            format!("blob not found: {hash}"),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
    }

    /// `history --id` with an unknown run id. Extends "not found" (exit 4)
    /// the same way `blob_not_found` does.
    pub(crate) fn run_not_found(id: &str) -> Self {
        Self::new(
            "run_not_found",
            format!("run not found: {id}"),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
    }

    /// `diff` with one or both runs missing (exit 4); `details.missing` lists them.
    pub(crate) fn runs_not_found(missing: &[&str]) -> Self {
        Self::new(
            "run_not_found",
            format!("run not found: {}", missing.join(", ")),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
        .with_details(json!({ "missing": missing }))
    }

    /// `replay --frozen` when the current YAML resolves to a different
    /// request than the recorded one. Assertion family (exit 1): no network,
    /// no Lattice row.
    pub(crate) fn replay_changed(run_id: &str, recorded: &str, current: &str) -> Self {
        Self::new(
            "replay_changed",
            format!("request changed since {run_id}; --frozen refuses to replay"),
            ASSERTION_EXIT_CODE,
        )
        .with_details(json!({
            "replayedFrom": run_id,
            "recordedHash": recorded,
            "currentHash": current,
        }))
    }

    /// `facet theme check` on a rejected theme file (exit 1, assertion
    /// family): the TUI would fall back to a built-in; `check` reports it
    /// instead, naming the file and field in `details`.
    pub(crate) fn theme_invalid(error: &facet_tui::ThemeFileError) -> Self {
        Self::new("theme_invalid", error.to_string(), ASSERTION_EXIT_CODE).with_details(json!({
            "path": error.path().to_string_lossy(),
            "field": error.field(),
        }))
    }

    /// `pin get <name>` with no such pin (exit 4).
    pub(crate) fn pin_not_found(name: &str) -> Self {
        Self::new(
            "pin_not_found",
            format!("pin not found: {name}"),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
    }

    /// A stored value Facet wrote itself cannot be read back (exit 9).
    pub(crate) fn lattice_corrupt(message: impl Into<String>) -> Self {
        Self::new("lattice_error", message, LATTICE_EXIT_CODE)
    }

    /// `session end|show <id>` with an unknown session id (exit 4).
    pub(crate) fn session_not_found(id: &str) -> Self {
        Self::new(
            "session_not_found",
            format!("session not found: {id}"),
            REQUEST_NOT_FOUND_EXIT_CODE,
        )
    }

    /// `--session current` or `session end` with no `FACET_SESSION` in the
    /// environment. Configuration family (exit 5): the caller fixes inputs.
    pub(crate) fn session_not_set() -> Self {
        Self::new(
            "session_not_set",
            "FACET_SESSION is not set; run `export FACET_SESSION=$(facet session start)` \
             or pass an explicit session id",
            CONFIGURATION_EXIT_CODE,
        )
    }

    pub(crate) fn lattice_not_found(path: &std::path::Path) -> Self {
        Self::new(
            "lattice_not_found",
            format!(
                "no Lattice store found at or above {}; run a request with facet first",
                path.display()
            ),
            LATTICE_EXIT_CODE,
        )
    }

    pub(crate) fn lattice(error: LatticeError) -> Self {
        match error {
            LatticeError::ReadOnlyQuery => Self::new(
                "sql_read_only",
                error.to_string(),
                INVALID_ARGUMENTS_EXIT_CODE,
            ),
            error if error.is_query_error() => Self::new(
                "invalid_sql",
                error.to_string(),
                INVALID_ARGUMENTS_EXIT_CODE,
            ),
            LatticeError::Config(error) => Self::new(
                "invalid_arguments",
                error.to_string(),
                INVALID_ARGUMENTS_EXIT_CODE,
            ),
            // The secrets layer cannot serve: configuration family, so the
            // caller fixes inputs (set FACET_SECRET_KEY / keyring) and retries.
            LatticeError::Secret(error) => Self::new(
                "secret_backend_unavailable",
                error.to_string(),
                CONFIGURATION_EXIT_CODE,
            ),
            error => Self::new("lattice_error", error.to_string(), LATTICE_EXIT_CODE),
        }
    }

    /// Secret hydration could not proceed for a declared secret.
    pub(crate) fn hydrate(error: HydrateError) -> Self {
        match error {
            HydrateError::BackendUnavailable { name, reason } => Self::new(
                "secret_backend_unavailable",
                format!("secret backend unavailable for {name}: {reason}"),
                CONFIGURATION_EXIT_CODE,
            )
            .with_details(json!({ "variable": name })),
            HydrateError::Lattice(error) => Self::lattice(error),
            HydrateError::HostOverride { name } => Self::new(
                "host_override_refused",
                format!("host override refused for {name}"),
                CONFIGURATION_EXIT_CODE,
            )
            .with_details(json!({ "variable": name })),
        }
    }

    pub(crate) fn output(path: &std::path::Path, error: &std::io::Error) -> Self {
        Self::new(
            "output_error",
            format!("cannot write {}: {error}", path.display()),
            EXECUTION_EXIT_CODE,
        )
    }

    /// Mirrors `probe-cli`'s environment-resolution mapping.
    /// A bad `facet:` key (exit 3, like any invalid collection). Details
    /// name the key path only; neither part carries a value.
    pub(crate) fn policy(error: &PolicyError) -> Self {
        Self::new(
            error.category(),
            error.to_string(),
            INVALID_WORKSPACE_EXIT_CODE,
        )
        .with_details(json!({ "path": error.path() }))
    }

    pub(crate) fn configuration(error: EnvironmentResolutionError) -> Self {
        if matches!(
            error,
            EnvironmentResolutionError::InvalidVariableName
                | EnvironmentResolutionError::InvalidEnvironmentName
        ) {
            return Self::invalid_arguments(error.to_string());
        }
        let category = match error {
            EnvironmentResolutionError::EnvironmentNotFound(_) => "environment_not_found",
            EnvironmentResolutionError::DuplicateEnvironment(_) => "duplicate_environment",
            EnvironmentResolutionError::ParentEnvironmentNotFound { .. } => {
                "parent_environment_not_found"
            }
            EnvironmentResolutionError::EnvironmentInheritanceCycle(_) => {
                "environment_inheritance_cycle"
            }
            EnvironmentResolutionError::MissingVariable(_) => "missing_variable",
            EnvironmentResolutionError::VariableNotFound { .. } => "variable_not_found",
            EnvironmentResolutionError::SecretVariableUnavailable(_) => {
                "secret_variable_unavailable"
            }
            EnvironmentResolutionError::DuplicateVariable { .. } => "duplicate_variable",
            EnvironmentResolutionError::EnvironmentInUse(_) => "environment_in_use",
            _ => "environment_resolution",
        };
        Self::new(category, error.to_string(), CONFIGURATION_EXIT_CODE)
    }

    /// Mirrors `probe-cli`'s HTTP mapping.
    pub(crate) fn http(error: &HttpError) -> Self {
        let category = if error.is_configuration() {
            "request_configuration"
        } else {
            match error {
                HttpError::Timeout => "request_timeout",
                HttpError::Cancelled => "request_cancelled",
                HttpError::ResponseOutput { .. } => "output_error",
                _ => "network_execution",
            }
        };
        let exit_code = if error.is_configuration() {
            CONFIGURATION_EXIT_CODE
        } else {
            EXECUTION_EXIT_CODE
        };
        Self::new(category, error.to_string(), exit_code)
    }

    pub(crate) fn runtime(error: &std::io::Error) -> Self {
        Self::new(
            "runtime_error",
            format!("cannot start asynchronous HTTP runtime: {error}"),
            EXECUTION_EXIT_CODE,
        )
    }

    pub(crate) fn stdin(error: &std::io::Error) -> Self {
        Self::new(
            "stdin_error",
            format!("cannot read OpenCollection YAML from stdin: {error}"),
            INVALID_WORKSPACE_EXIT_CODE,
        )
    }
}

/// `text` with every known value replaced, and every URL in it (a transport
/// error names the URL it failed on) through [`Redactor::url`], which also
/// strips userinfo.
pub(crate) fn redact_message(redactor: &Redactor, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(separator) = rest.find("://") {
        let start = rest[..separator]
            .rfind(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
            })
            .map_or(0, |index| index + 1);
        let end = rest[separator..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ')' | '"' | '\'' | '<' | '>' | '`')
            })
            .map_or(rest.len(), |offset| separator + offset);
        out.push_str(&redactor.text(&rest[..start]));
        out.push_str(&redactor.url(&rest[start..end]));
        rest = &rest[end..];
    }
    out.push_str(&redactor.text(rest));
    out
}
