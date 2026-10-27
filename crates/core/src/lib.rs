//! Shared domain and application layer for Probe.
//!
//! These models describe application concepts and deliberately contain no YAML or
//! serialization concerns.

#![forbid(unsafe_code)]

mod arena;
mod collection;
mod environment;
mod environment_edit;
mod environment_model;
mod path_parameters;
mod request;
mod request_resolution;
mod workspace;

pub use collection::{
    Author, Collection, CollectionItem, CollectionMetadata, Folder, ImportDiagnostic,
    ImportDiagnosticSeverity, ItemMetadata, lossy_import_diagnostic, lossy_import_diagnostic_count,
    nonempty_string, sort_import_diagnostics, warning_import_diagnostic,
};
pub use environment::{
    EffectiveEnvironmentVariable, EnvironmentResolutionError, ResolvedEnvironment,
    resolve_environment, resolve_environment_with_overrides, validate_environments,
};
pub use environment_edit::{
    create_environment, delete_environment, effective_environment_variables, replace_environment,
    revert_created_environment, set_environment_variable, unset_environment_variable,
    validate_unique_variable_names,
};
pub use environment_model::{
    Environment, EnvironmentVariable, SecretVariable, Variable, VariableValue, VariableValueSet,
    VariableValueType, VariableValueVariant,
};
pub use path_parameters::{
    add_path_parameter, apply_path_parameters, ensure_path_parameters_from_url,
    path_variable_ranges, remove_path_parameter_at, rename_path_parameter_at,
    synchronize_path_parameters,
};
pub use request::{
    Authentication, AuthenticationKind, AuthenticationValue, Body, BodyVariant, FileReference,
    FormField, Header, HttpRequest, MultipartPart, MultipartPartKind, MultipartValue,
    QueryParameter, RawBody, RawBodyKind, RequestBody, RequestSettings, RequestUpdate,
};
pub use request_resolution::{
    RequestVariableInfo, TransportLock, VariableUsage, discover_request_variables, resolve_request,
    resolve_request_strict, transport_lock,
};
pub use workspace::{
    FolderKey, RequestKey, Workspace, WorkspaceEditError, WorkspaceFolder, WorkspaceItemRef,
    WorkspaceParent,
};
