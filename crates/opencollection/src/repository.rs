// Modified by Hedronite on 2026-09-26: reworded the editor description for
// the retired desktop surface (Apache-2.0 License, Section 4(b)).
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use atomic_write_file::AtomicWriteFile;
use probe_core::{
    CollectionItem, Environment, EnvironmentResolutionError, EnvironmentVariable, FolderKey,
    RequestKey, RequestUpdate, Variable, VariableValue, VariableValueSet, VariableValueVariant,
    Workspace, WorkspaceItemRef, validate_environments, validate_unique_variable_names,
};
use serde_yaml_ng::Value;

use super::{EnvironmentDocument, ParseError, parse, project_item};

mod create;
mod environment;
mod errors;
mod loading;
mod lock;
mod new_file;
mod yaml;

use create::environment_value;
pub use create::{create_bundled_workspace, create_bundled_workspace_from_collection};
use environment::*;
pub use errors::{CreateError, LoadError, SaveError};
pub(crate) use loading::relative_selector;
pub use loading::{load_workspace, load_workspace_from_str};
pub(crate) use lock::SaveLock;
use new_file::{NewFileError, write_new_file};
use yaml::*;

/// A request and its repository-backed selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatedRequest {
    selector: String,
    key: RequestKey,
    persistence: Option<RequestPersistence>,
}

impl LocatedRequest {
    /// Returns the selector accepted by CLI and repository operations.
    #[must_use]
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Returns the request's session-only workspace key.
    #[must_use]
    pub const fn key(&self) -> RequestKey {
        self.key
    }
}

/// A folder and its repository-backed selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatedFolder {
    selector: String,
    key: FolderKey,
}

impl LocatedFolder {
    /// Returns the stable selector used to restore presentation state.
    #[must_use]
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Returns the folder's session-only workspace key.
    #[must_use]
    pub const fn key(&self) -> FolderKey {
        self.key
    }
}

/// A loaded OpenCollection workspace and its persistence-locator index.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedWorkspace {
    workspace: Workspace,
    requests: Vec<LocatedRequest>,
    folders: Vec<LocatedFolder>,
    request_indices_by_selector: BTreeMap<String, usize>,
    folder_indices_by_selector: BTreeMap<String, usize>,
    request_indices_by_key: BTreeMap<RequestKey, usize>,
    folder_indices_by_key: BTreeMap<FolderKey, usize>,
    environment_persistence: BTreeMap<String, EnvironmentPersistence>,
    pub(crate) documents: BTreeMap<PathBuf, SourceDocument>,
    pub(crate) source: WorkspaceSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceSource {
    Bundled(PathBuf),
    Unbundled(PathBuf),
    Memory,
}

impl LoadedWorkspace {
    /// Returns the in-memory domain workspace.
    #[must_use]
    pub const fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Returns whether repository locators are workspace-relative filesystem paths.
    ///
    /// Unbundled collections use paths. Bundled and in-memory documents use structural
    /// item selectors, which do not collide by file name.
    #[must_use]
    pub const fn uses_path_locators(&self) -> bool {
        matches!(self.source, WorkspaceSource::Unbundled(_))
    }

    /// Returns the filesystem path this workspace was loaded from, if any.
    #[must_use]
    pub fn source_path(&self) -> Option<&Path> {
        match &self.source {
            WorkspaceSource::Bundled(path) | WorkspaceSource::Unbundled(path) => Some(path),
            WorkspaceSource::Memory => None,
        }
    }

    /// Mutably looks up a request in the loaded in-memory workspace.
    ///
    /// Interactive editors use this fast path to apply draft changes immediately. Saving
    /// remains an explicit, separate repository operation.
    pub fn request_mut(&mut self, key: RequestKey) -> Option<&mut probe_core::HttpRequest> {
        self.workspace.request_mut(key)
    }

    /// Updates a plain environment variable in the in-memory workspace.
    pub fn set_environment_variable(
        &mut self,
        environment_name: &str,
        variable_name: &str,
        value: String,
    ) -> Result<(), EnvironmentResolutionError> {
        self.workspace
            .set_environment_variable(environment_name, variable_name, value)
    }

    /// Applies a variable set in memory and atomically persists its OpenCollection document.
    ///
    /// The save is rejected if the source file no longer exactly matches the bytes
    /// loaded by this repository instance. On persistence failure, the in-memory
    /// environment remains updated so callers can report or retry the dirty state.
    pub fn update_environment_variable(
        &mut self,
        environment_name: &str,
        variable_name: &str,
        value: String,
    ) -> Result<(), SaveError> {
        self.workspace
            .set_environment_variable(environment_name, variable_name, value)
            .map_err(SaveError::Environment)?;
        let variable = self
            .plain_environment_variable(environment_name, variable_name)
            .cloned()
            .ok_or_else(|| {
                SaveError::InvalidDocument(format!(
                    "environment '{environment_name}' is missing variable '{variable_name}' after update"
                ))
            })?;
        self.persist_environment_mutation(
            environment_name,
            EnvironmentYamlMutation::Set { variable },
        )
    }

    /// Removes a variable from the named environment and atomically persists the document.
    pub fn unset_environment_variable(
        &mut self,
        environment_name: &str,
        variable_name: &str,
    ) -> Result<(), SaveError> {
        self.workspace
            .unset_environment_variable(environment_name, variable_name)
            .map_err(SaveError::Environment)?;
        self.persist_environment_mutation(
            environment_name,
            EnvironmentYamlMutation::Unset {
                name: variable_name.to_owned(),
            },
        )
    }

    /// Creates a new environment in memory and atomically persists its OpenCollection document.
    pub fn create_environment(
        &mut self,
        name: String,
        extends: Option<String>,
    ) -> Result<(), SaveError> {
        let prepared = self.prepare_environment_create(name, extends)?;
        let name = prepared.environment_name().to_owned();
        match prepared.execute() {
            Ok(saved) => {
                self.complete_environment_create(saved);
                Ok(())
            }
            Err(error) => {
                self.revert_created_environment(&name);
                Err(error)
            }
        }
    }

    /// Creates an environment in memory and captures a filesystem persist for background execution.
    ///
    /// The in-memory workspace contains the new environment after this returns. Call
    /// [`PreparedEnvironmentCreate::execute`] away from the UI thread, then
    /// [`Self::complete_environment_create`] or [`Self::revert_created_environment`].
    pub fn prepare_environment_create(
        &mut self,
        name: String,
        extends: Option<String>,
    ) -> Result<PreparedEnvironmentCreate, SaveError> {
        self.workspace
            .create_environment(name.clone(), extends)
            .map_err(SaveError::Environment)?;
        let environment = self
            .workspace
            .environments()
            .iter()
            .find(|environment| environment.name == name)
            .cloned()
            .expect("created environment must be present");
        match &self.source {
            WorkspaceSource::Memory => {
                self.revert_created_environment(&name);
                Err(SaveError::ReadOnlySource)
            }
            WorkspaceSource::Bundled(document_path) => {
                let document_path = document_path.clone();
                let original_source = self
                    .documents
                    .get(&document_path)
                    .expect("bundled workspace must retain its source document")
                    .original_source
                    .clone();
                Ok(PreparedEnvironmentCreate {
                    environment,
                    kind: EnvironmentCreateKind::Bundled {
                        document_path,
                        original_source,
                        bundled_index: self.workspace.environments().len() - 1,
                    },
                })
            }
            WorkspaceSource::Unbundled(root) => Ok(PreparedEnvironmentCreate {
                environment,
                kind: EnvironmentCreateKind::Unbundled { root: root.clone() },
            }),
        }
    }

    /// Records persistence metadata after a prepared environment create succeeds.
    pub fn complete_environment_create(&mut self, saved: CompletedEnvironmentCreate) {
        self.environment_persistence.insert(
            saved.name,
            EnvironmentPersistence {
                document_path: saved.document_path.clone(),
                bundled_index: saved.bundled_index,
            },
        );
        self.documents.insert(
            saved.document_path,
            SourceDocument {
                original_source: saved.serialized_source,
            },
        );
    }

    /// Drops an in-memory environment that was created but not persisted.
    pub fn revert_created_environment(&mut self, name: &str) {
        self.workspace.revert_created_environment(name);
        self.environment_persistence.remove(name);
    }

    /// Captures an environment-variable save that can be executed away from the UI thread.
    ///
    /// The in-memory workspace must already contain the updated variable. Preparing is
    /// in-memory only. [`PreparedEnvironmentSave::execute`] performs the conflict check
    /// and atomic filesystem write.
    pub fn prepare_environment_variable_save(
        &self,
        environment_name: &str,
        variable_name: &str,
    ) -> Result<PreparedEnvironmentSave, SaveError> {
        let variable = self
            .plain_environment_variable(environment_name, variable_name)
            .cloned()
            .ok_or_else(|| {
                SaveError::Environment(EnvironmentResolutionError::VariableNotFound {
                    environment: environment_name.to_owned(),
                    variable: variable_name.to_owned(),
                })
            })?;
        self.prepare_environment_mutation(
            environment_name,
            EnvironmentYamlMutation::Set { variable },
        )
    }

    /// Refreshes the retained conflict baseline after a prepared environment save succeeds.
    pub fn complete_environment_save(&mut self, saved: CompletedEnvironmentSave) {
        self.documents.insert(
            saved.document_path,
            SourceDocument {
                original_source: saved.serialized_source,
            },
        );
    }

    /// Replaces one environment in memory and atomically persists the OpenCollection document.
    pub fn replace_environment(
        &mut self,
        original_name: &str,
        replacement: Environment,
    ) -> Result<(), SaveError> {
        let prepared = self.prepare_environment_replace(original_name, replacement)?;
        let saved = prepared.execute()?;
        self.complete_environment_replace(saved);
        Ok(())
    }

    /// Captures a validated replacement of one environment for background persistence.
    ///
    /// Secret variables are retained from the source document. The replacement may edit
    /// the environment name, parent, and plain variables. Renaming a parent environment
    /// is rejected because it would require a multi-document transaction.
    pub fn prepare_environment_replace(
        &self,
        original_name: &str,
        replacement: Environment,
    ) -> Result<PreparedEnvironmentReplace, SaveError> {
        if replacement.name != original_name
            && self
                .workspace
                .environments()
                .iter()
                .any(|environment| environment.extends.as_deref() == Some(original_name))
        {
            return Err(SaveError::Environment(
                EnvironmentResolutionError::EnvironmentInUse(original_name.to_owned()),
            ));
        }
        let original = self
            .workspace
            .environments()
            .iter()
            .find(|environment| environment.name == original_name)
            .cloned()
            .ok_or_else(|| {
                SaveError::Environment(EnvironmentResolutionError::EnvironmentNotFound(
                    original_name.to_owned(),
                ))
            })?;
        let replacement = environment_replacement_with_retained_secrets(&original, replacement)?;
        let mut candidate = self.workspace.environments().to_vec();
        probe_core::replace_environment(&mut candidate, original_name, replacement.clone())
            .map_err(SaveError::Environment)?;
        let persistence = self
            .environment_persistence
            .get(original_name)
            .cloned()
            .ok_or(SaveError::ReadOnlySource)?;
        let original_source = self
            .documents
            .get(&persistence.document_path)
            .expect("filesystem environment must retain its source document")
            .original_source
            .clone();
        Ok(PreparedEnvironmentReplace {
            persistence,
            original_source,
            original_name: original_name.to_owned(),
            replacement,
        })
    }

    /// Applies a successfully persisted environment replacement to the in-memory workspace.
    pub fn complete_environment_replace(&mut self, saved: CompletedEnvironmentReplace) {
        self.workspace
            .replace_environment(&saved.original_name, saved.replacement.clone())
            .expect("prepared environment replacement must remain valid");
        let mut persistence = self
            .environment_persistence
            .remove(&saved.original_name)
            .expect("replaced environment must retain persistence metadata");
        if persistence.document_path != saved.document_path {
            self.documents.remove(&persistence.document_path);
            persistence.document_path = saved.document_path.clone();
        }
        self.environment_persistence
            .insert(saved.replacement.name, persistence);
        self.documents.insert(
            saved.document_path,
            SourceDocument {
                original_source: saved.serialized_source,
            },
        );
    }

    /// Deletes an environment in memory and atomically persists the OpenCollection document.
    pub fn delete_environment(&mut self, name: &str) -> Result<(), SaveError> {
        let prepared = self.prepare_environment_delete(name)?;
        let saved = prepared.execute()?;
        self.complete_environment_delete(saved);
        Ok(())
    }

    /// Captures deletion of an environment that has no children.
    pub fn prepare_environment_delete(
        &self,
        name: &str,
    ) -> Result<PreparedEnvironmentDelete, SaveError> {
        let mut candidate = self.workspace.environments().to_vec();
        probe_core::delete_environment(&mut candidate, name).map_err(SaveError::Environment)?;
        let persistence = self
            .environment_persistence
            .get(name)
            .cloned()
            .ok_or(SaveError::ReadOnlySource)?;
        let original_source = self
            .documents
            .get(&persistence.document_path)
            .expect("filesystem environment must retain its source document")
            .original_source
            .clone();
        Ok(PreparedEnvironmentDelete {
            name: name.to_owned(),
            persistence,
            original_source,
        })
    }

    /// Applies a successfully persisted environment deletion in memory.
    pub fn complete_environment_delete(&mut self, saved: CompletedEnvironmentDelete) {
        self.workspace
            .delete_environment(&saved.name)
            .expect("prepared environment deletion must remain valid");
        self.environment_persistence.remove(&saved.name);
        if let Some(serialized_source) = saved.serialized_source {
            self.documents.insert(
                saved.document_path.clone(),
                SourceDocument {
                    original_source: serialized_source,
                },
            );
            if let Some(removed_index) = saved.bundled_index {
                for persistence in self.environment_persistence.values_mut() {
                    if persistence.document_path == saved.document_path
                        && persistence
                            .bundled_index
                            .is_some_and(|index| index > removed_index)
                    {
                        persistence.bundled_index =
                            persistence.bundled_index.map(|index| index - 1);
                    }
                }
            }
        } else {
            self.documents.remove(&saved.document_path);
        }
    }

    fn persist_environment_mutation(
        &mut self,
        environment_name: &str,
        mutation: EnvironmentYamlMutation,
    ) -> Result<(), SaveError> {
        let prepared = self.prepare_environment_mutation(environment_name, mutation)?;
        let saved = prepared.execute()?;
        self.complete_environment_save(saved);
        Ok(())
    }

    fn prepare_environment_mutation(
        &self,
        environment_name: &str,
        mutation: EnvironmentYamlMutation,
    ) -> Result<PreparedEnvironmentSave, SaveError> {
        let persistence = self
            .environment_persistence
            .get(environment_name)
            .cloned()
            .ok_or(SaveError::ReadOnlySource)?;
        let original_source = self
            .documents
            .get(&persistence.document_path)
            .expect("filesystem environment must retain its source document")
            .original_source
            .clone();
        Ok(PreparedEnvironmentSave {
            persistence,
            original_source,
            mutation,
        })
    }

    fn plain_environment_variable(
        &self,
        environment_name: &str,
        variable_name: &str,
    ) -> Option<&Variable> {
        self.workspace
            .environments()
            .iter()
            .find(|environment| environment.name == environment_name)?
            .variables
            .iter()
            .find_map(|variable| match variable {
                EnvironmentVariable::Plain(variable)
                    if variable.name.as_deref() == Some(variable_name) =>
                {
                    Some(variable)
                }
                _ => None,
            })
    }

    /// Returns requests in collection traversal order.
    #[must_use]
    pub fn requests(&self) -> &[LocatedRequest] {
        &self.requests
    }

    /// Returns folders in collection traversal order.
    #[must_use]
    pub fn folders(&self) -> &[LocatedFolder] {
        &self.folders
    }

    /// Resolves a repository-backed selector to a request key.
    #[must_use]
    pub fn request_key(&self, selector: &str) -> Option<RequestKey> {
        self.request_indices_by_selector
            .get(selector)
            .map(|index| self.requests[*index].key)
    }

    /// Resolves a repository-backed selector to a folder key.
    #[must_use]
    pub fn folder_key(&self, selector: &str) -> Option<FolderKey> {
        self.folder_indices_by_selector
            .get(selector)
            .map(|index| self.folders[*index].key)
    }

    /// Returns the stable selector for a request key.
    #[must_use]
    pub fn request_selector(&self, key: RequestKey) -> Option<&str> {
        self.request_indices_by_key
            .get(&key)
            .map(|index| self.requests[*index].selector.as_str())
    }

    /// Returns the stable selector for a folder key.
    #[must_use]
    pub fn folder_selector(&self, key: FolderKey) -> Option<&str> {
        self.folder_indices_by_key
            .get(&key)
            .map(|index| self.folders[*index].selector.as_str())
    }

    /// Applies an update in memory and atomically persists its OpenCollection document.
    ///
    /// The save is rejected if the source file no longer exactly matches the bytes
    /// loaded by this repository instance. On persistence failure, the in-memory
    /// request remains updated so callers can report or retry the dirty state.
    pub fn update_request(
        &mut self,
        selector: &str,
        update: &RequestUpdate,
    ) -> Result<(), SaveError> {
        if update.is_empty() {
            return Err(SaveError::EmptyUpdate);
        }

        let located = self
            .requests
            .iter()
            .find(|request| request.selector == selector)
            .cloned()
            .ok_or_else(|| SaveError::RequestNotFound(selector.to_owned()))?;
        let request = self
            .workspace
            .request_mut(located.key)
            .expect("repository request key must resolve");
        update.apply(request);

        let persistence = located.persistence.ok_or(SaveError::ReadOnlySource)?;
        let source = self
            .documents
            .get(&persistence.document_path)
            .expect("filesystem request must retain its source document");
        let serialized = mutate_existing_document(
            &persistence.document_path,
            &source.original_source,
            |document| {
                let request_document = request_document_mut(document, &persistence.item_path)?;
                apply_request_update(request_document, update)
            },
        )?;

        self.documents.insert(
            persistence.document_path,
            SourceDocument {
                original_source: serialized,
            },
        );
        Ok(())
    }

    /// Captures a request save that can be executed away from the UI thread.
    ///
    /// Preparing is in-memory only. [`PreparedRequestSave::execute`] performs the
    /// conflict check and atomic filesystem write.
    pub fn prepare_request_save(
        &self,
        selector: &str,
        update: RequestUpdate,
    ) -> Result<PreparedRequestSave, SaveError> {
        if update.is_empty() {
            return Err(SaveError::EmptyUpdate);
        }
        let persistence = self
            .requests
            .iter()
            .find(|request| request.selector == selector)
            .ok_or_else(|| SaveError::RequestNotFound(selector.to_owned()))?
            .persistence
            .clone()
            .ok_or(SaveError::ReadOnlySource)?;
        let original_source = self
            .documents
            .get(&persistence.document_path)
            .expect("filesystem request must retain its source document")
            .original_source
            .clone();
        Ok(PreparedRequestSave {
            persistence,
            original_source,
            update,
        })
    }

    /// Refreshes the retained conflict baseline after a prepared save succeeds.
    pub fn complete_request_save(&mut self, saved: CompletedRequestSave) {
        self.documents.insert(
            saved.document_path,
            SourceDocument {
                original_source: saved.serialized_source,
            },
        );
    }
}

/// A filesystem save captured from a loaded workspace for background execution.
#[derive(Debug)]
pub struct PreparedRequestSave {
    persistence: RequestPersistence,
    original_source: Vec<u8>,
    update: RequestUpdate,
}

impl PreparedRequestSave {
    /// Performs the exact-source check and atomic write.
    pub fn execute(self) -> Result<CompletedRequestSave, SaveError> {
        let serialized = mutate_existing_document(
            &self.persistence.document_path,
            &self.original_source,
            |document| {
                let request = request_document_mut(document, &self.persistence.item_path)?;
                apply_request_update(request, &self.update)
            },
        )?;
        Ok(CompletedRequestSave {
            document_path: self.persistence.document_path,
            serialized_source: serialized,
        })
    }
}

/// The refreshed repository baseline produced by a successful prepared save.
#[derive(Debug)]
pub struct CompletedRequestSave {
    document_path: PathBuf,
    serialized_source: Vec<u8>,
}

/// A filesystem environment-variable save captured for background execution.
#[derive(Debug)]
pub struct PreparedEnvironmentSave {
    persistence: EnvironmentPersistence,
    original_source: Vec<u8>,
    mutation: EnvironmentYamlMutation,
}

impl PreparedEnvironmentSave {
    /// Performs the exact-source check and atomic write.
    pub fn execute(self) -> Result<CompletedEnvironmentSave, SaveError> {
        let serialized =
            persist_environment_yaml(&self.persistence, &self.original_source, &self.mutation)?;
        Ok(CompletedEnvironmentSave {
            document_path: self.persistence.document_path,
            serialized_source: serialized,
        })
    }
}

/// The refreshed repository baseline produced by a successful environment save.
#[derive(Debug)]
pub struct CompletedEnvironmentSave {
    document_path: PathBuf,
    serialized_source: Vec<u8>,
}

/// A validated environment replacement captured for background persistence.
#[derive(Debug)]
pub struct PreparedEnvironmentReplace {
    persistence: EnvironmentPersistence,
    original_source: Vec<u8>,
    original_name: String,
    replacement: Environment,
}

impl PreparedEnvironmentReplace {
    /// Performs the exact-source check and atomic write.
    pub fn execute(self) -> Result<CompletedEnvironmentReplace, SaveError> {
        let destination = unbundled_rename_destination(
            &self.persistence,
            &self.original_name,
            &self.replacement.name,
        );
        let (serialized, document_path) = match destination {
            Some(new_path) => persist_unbundled_environment_rename(
                &self.persistence.document_path,
                &new_path,
                &self.original_source,
                &self.replacement,
            )?,
            None => {
                let serialized = persist_environment_yaml(
                    &self.persistence,
                    &self.original_source,
                    &EnvironmentYamlMutation::Replace {
                        environment: self.replacement.clone(),
                    },
                )?;
                (serialized, self.persistence.document_path)
            }
        };
        Ok(CompletedEnvironmentReplace {
            original_name: self.original_name,
            replacement: self.replacement,
            document_path,
            serialized_source: serialized,
        })
    }
}

/// The refreshed repository baseline produced by an environment replacement.
#[derive(Debug)]
pub struct CompletedEnvironmentReplace {
    original_name: String,
    replacement: Environment,
    document_path: PathBuf,
    serialized_source: Vec<u8>,
}

/// A validated environment deletion captured for background persistence.
#[derive(Debug)]
pub struct PreparedEnvironmentDelete {
    name: String,
    persistence: EnvironmentPersistence,
    original_source: Vec<u8>,
}

impl PreparedEnvironmentDelete {
    /// Performs the exact-source check and removes the environment.
    pub fn execute(self) -> Result<CompletedEnvironmentDelete, SaveError> {
        let document_path = self.persistence.document_path.clone();
        let (serialized_source, bundled_index) = match self.persistence.bundled_index {
            Some(index) => (
                Some(persist_bundled_environment_delete(
                    &document_path,
                    &self.original_source,
                    index,
                )?),
                Some(index),
            ),
            None => {
                persist_unbundled_environment_delete(&document_path, &self.original_source)?;
                (None, None)
            }
        };
        Ok(CompletedEnvironmentDelete {
            name: self.name,
            document_path,
            serialized_source,
            bundled_index,
        })
    }
}

/// The refreshed repository baseline produced by an environment deletion.
#[derive(Debug)]
pub struct CompletedEnvironmentDelete {
    name: String,
    document_path: PathBuf,
    serialized_source: Option<Vec<u8>>,
    bundled_index: Option<usize>,
}

/// A filesystem environment-create save captured for background execution.
#[derive(Debug)]
pub struct PreparedEnvironmentCreate {
    environment: Environment,
    kind: EnvironmentCreateKind,
}

#[derive(Debug)]
enum EnvironmentCreateKind {
    Bundled {
        document_path: PathBuf,
        original_source: Vec<u8>,
        bundled_index: usize,
    },
    Unbundled {
        root: PathBuf,
    },
}

impl PreparedEnvironmentCreate {
    fn environment_name(&self) -> &str {
        &self.environment.name
    }

    /// Performs the conflict check and atomic write for a new environment document.
    pub fn execute(self) -> Result<CompletedEnvironmentCreate, SaveError> {
        let name = self.environment.name.clone();
        let (document_path, serialized, bundled_index) = match self.kind {
            EnvironmentCreateKind::Bundled {
                document_path,
                original_source,
                bundled_index,
            } => {
                let serialized = persist_bundled_environment_create(
                    &document_path,
                    &original_source,
                    &self.environment,
                )?;
                (document_path, serialized, Some(bundled_index))
            }
            EnvironmentCreateKind::Unbundled { root } => {
                let (document_path, serialized) =
                    persist_unbundled_environment_create(&root, &self.environment)?;
                (document_path, serialized, None)
            }
        };
        Ok(CompletedEnvironmentCreate {
            name,
            document_path,
            serialized_source: serialized,
            bundled_index,
        })
    }
}

/// The refreshed repository baseline produced by a successful environment create.
#[derive(Debug)]
pub struct CompletedEnvironmentCreate {
    name: String,
    document_path: PathBuf,
    serialized_source: Vec<u8>,
    bundled_index: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EnvironmentPersistence {
    document_path: PathBuf,
    bundled_index: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EnvironmentYamlMutation {
    Set { variable: Variable },
    Unset { name: String },
    Replace { environment: Environment },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequestPersistence {
    document_path: PathBuf,
    item_path: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceDocument {
    pub(crate) original_source: Vec<u8>,
}

/// Loads a bundled OpenCollection file or an unbundled collection directory.
fn request_document_mut<'a>(
    document: &'a mut Value,
    item_path: &[usize],
) -> Result<&'a mut Value, SaveError> {
    let mut current = document;
    for index in item_path {
        let mapping = current.as_mapping_mut().ok_or_else(|| {
            SaveError::InvalidDocument("an item parent is not a mapping".to_owned())
        })?;
        let items = mapping
            .get_mut(Value::String("items".to_owned()))
            .and_then(Value::as_sequence_mut)
            .ok_or_else(|| {
                SaveError::InvalidDocument("an item parent has no items sequence".to_owned())
            })?;
        current = items.get_mut(*index).ok_or_else(|| {
            SaveError::InvalidDocument(format!("item index {index} is out of bounds"))
        })?;
    }
    Ok(current)
}

fn apply_request_update(document: &mut Value, update: &RequestUpdate) -> Result<(), SaveError> {
    let request = document.as_mapping_mut().ok_or_else(|| {
        SaveError::InvalidDocument("the request item is not a mapping".to_owned())
    })?;

    if let Some(name) = &update.name {
        let info = mapping_child(request, "info")?;
        info.insert(
            Value::String("name".to_owned()),
            Value::String(name.clone()),
        );
    }
    if update.method.is_some()
        || update.url.is_some()
        || update.headers.is_some()
        || update.query_parameters.is_some()
        || update.path_parameters.is_some()
        || update.body.is_some()
        || update.authentication.is_some()
    {
        let http = mapping_child(request, "http")?;
        if let Some(method) = &update.method {
            http.insert(
                Value::String("method".to_owned()),
                Value::String(method.clone()),
            );
        }
        if let Some(url) = &update.url {
            http.insert(Value::String("url".to_owned()), Value::String(url.clone()));
        }
        if let Some(headers) = &update.headers {
            merge_sequence_preserving(
                http,
                "headers",
                headers.iter().map(header_value).collect(),
                &[],
            );
        }
        if update.query_parameters.is_some() || update.path_parameters.is_some() {
            merge_parameters(
                http,
                update.query_parameters.as_deref(),
                update.path_parameters.as_deref(),
            );
        }
        if let Some(body) = &update.body {
            set_optional_merged(http, "body", body.as_ref().map(request_body_value));
        }
        if let Some(authentication) = &update.authentication {
            set_optional(
                http,
                "auth",
                authentication.as_ref().map(authentication_value),
            );
        }
    }
    Ok(())
}

fn mapping_child<'a>(
    parent: &'a mut serde_yaml_ng::Mapping,
    name: &str,
) -> Result<&'a mut serde_yaml_ng::Mapping, SaveError> {
    let key = Value::String(name.to_owned());
    if !parent.contains_key(&key) {
        parent.insert(key.clone(), Value::Mapping(serde_yaml_ng::Mapping::new()));
    }
    parent
        .get_mut(&key)
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| SaveError::InvalidDocument(format!("'{name}' is not a mapping")))
}

fn mutate_existing_document(
    path: &Path,
    original_source: &[u8],
    mutate: impl FnOnce(&mut Value) -> Result<(), SaveError>,
) -> Result<Vec<u8>, SaveError> {
    let _save_lock = SaveLock::acquire(path)?;
    let current = fs::read(path).map_err(|source| SaveError::Io {
        path: path.to_owned(),
        source,
    })?;
    if current != original_source {
        return Err(SaveError::ConcurrentModification(path.to_owned()));
    }
    let mut document: Value = serde_yaml_ng::from_slice(original_source).map_err(|error| {
        SaveError::InvalidDocument(format!("retained source cannot be parsed: {error}"))
    })?;
    mutate(&mut document)?;
    let serialized = serde_yaml_ng::to_string(&document)
        .map_err(SaveError::Serialize)?
        .into_bytes();
    atomic_write(path, &serialized, original_source)?;
    Ok(serialized)
}

pub(crate) fn atomic_write(
    path: &Path,
    contents: &[u8],
    expected_source: &[u8],
) -> Result<(), SaveError> {
    let map_io = |source| SaveError::Io {
        path: path.to_owned(),
        source,
    };
    let mut file = AtomicWriteFile::open(path).map_err(map_io)?;
    file.write_all(contents).map_err(map_io)?;
    file.sync_all().map_err(map_io)?;
    let current = fs::read(path).map_err(map_io)?;
    if current != expected_source {
        return Err(SaveError::ConcurrentModification(path.to_owned()));
    }
    file.commit().map_err(map_io)?;
    Ok(())
}
