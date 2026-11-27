//! Generated from HedronDB `DocsEodSpec` (hedrondb @ c681943).
//! Shape contract for `kind: docs_eod`. No nickel-lang-core in this module.

use serde::Deserialize;
use serde_json::Value;

/// `spec.kind` handled by this contract.
pub const KIND: &str = "docs_eod";

/// `kind: docs_eod` spec body (`kind` validated separately).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DocsEodSpec {
    pub date: String,
    pub required_briefs: Vec<String>,
}

/// Shape mismatch for a desired-state spec.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct DocsEodShapeError {
    pub message: String,
}

impl DocsEodShapeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Validate `spec` against the `docs_eod` shape contract.
///
/// Rejects missing `kind`, unknown kinds, and malformed `docs_eod` bodies.
/// Callable from `hedron-core` without linking `nickel-lang-core`.
pub fn check_shape(spec: &Value) -> Result<DocsEodSpec, DocsEodShapeError> {
    let kind = spec
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| DocsEodShapeError::invalid("desired state spec requires kind: <string>"))?;

    if kind != KIND {
        return Err(DocsEodShapeError::invalid(format!(
            "unknown desired state kind {kind:?}"
        )));
    }

    serde_json::from_value(spec.clone())
        .map_err(|err| DocsEodShapeError::invalid(format!("{KIND} spec: {err}")))
}
