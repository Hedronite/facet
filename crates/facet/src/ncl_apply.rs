//! `facet ncl apply`: export frozen World projections, then act on them.
//!
//! The executor lives in [`crate::frozen_apply`] so the frozen verb and
//! `ncl apply` share one implementation without `hedron_ncl` in the frozen path.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::FacetError;
use crate::frozen_apply::ApplyAction;
use crate::ncl::NclExport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NclApply {
    pub path: PathBuf,
    pub module_hash: String,
    pub export_hash: String,
    pub contract_set: &'static str,
    pub cluster: Value,
    pub intent: Value,
    pub calls: Value,
    pub action: ApplyAction,
}

impl NclApply {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "moduleHash": self.module_hash,
            "exportHash": self.export_hash,
            "contractSet": self.contract_set,
            "cluster": self.cluster,
            "intent": self.intent,
            "calls": self.calls,
            "action": self.action.to_json(),
        })
    }
}

pub fn execute(export: &NclExport) -> Result<ApplyAction, FacetError> {
    crate::frozen_apply::execute_projections(&export.cluster, &export.intent, &export.calls)
}

pub fn from_export(export: NclExport, action: ApplyAction) -> NclApply {
    NclApply {
        path: export.path,
        module_hash: export.module_hash,
        export_hash: export.export_hash,
        contract_set: export.contract_set,
        cluster: export.cluster,
        intent: export.intent,
        calls: export.calls,
        action,
    }
}
