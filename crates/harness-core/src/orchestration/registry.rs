use std::collections::BTreeMap;

use super::compiler::{compile, CompiledOrchestration, DefinitionValidationError};
use super::default::default_orchestration_definition;
use super::definition::{DefinitionStatus, OrchestrationDefinition, OrchestrationDefinitionId};

/// Reference to a definition revision held by a [`DefinitionRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionRef {
    /// An exact revision.
    Exact {
        id: OrchestrationDefinitionId,
        revision: u64,
    },
    /// The highest published revision of `id`.
    LatestPublished(OrchestrationDefinitionId),
    /// The highest revision of `id` that is not deprecated; a draft counts
    /// when the registry allows drafts.
    Latest(OrchestrationDefinitionId),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("definition {id}@{revision} is already registered with different content")]
    ImmutableRevision {
        id: OrchestrationDefinitionId,
        revision: u64,
    },
    #[error("definition {0} is not registered")]
    NotFound(String),
    #[error("definition {id}@{revision} is a draft and drafts are not executable")]
    DraftNotExecutable {
        id: OrchestrationDefinitionId,
        revision: u64,
    },
    #[error("{0}")]
    Invalid(DefinitionValidationError),
}

/// In-memory store of immutable, compiled definition revisions.
///
/// Every registered revision is compiled up front, so an invalid graph is
/// rejected at registration and never reaches a run. Published revisions are
/// immutable: re-registering the same id and revision is accepted only when
/// the content is identical. Deprecated revisions still resolve by exact
/// reference so historical runs can be restored, but are skipped by
/// [`DefinitionRef::LatestPublished`].
#[derive(Debug, Clone, Default)]
pub struct DefinitionRegistry {
    revisions: BTreeMap<(OrchestrationDefinitionId, u64), CompiledOrchestration>,
    allow_drafts: bool,
}

impl DefinitionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry pre-loaded with the built-in default orchestration.
    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        registry
            .register(default_orchestration_definition())
            .expect("built-in default orchestration is valid");
        registry
    }

    /// Permit resolving draft revisions (development mode).
    pub fn allow_drafts(mut self, allow: bool) -> Self {
        self.allow_drafts = allow;
        self
    }

    pub fn register(
        &mut self,
        definition: OrchestrationDefinition,
    ) -> Result<&CompiledOrchestration, RegistryError> {
        let key = (definition.id.clone(), definition.revision);
        if let Some(existing) = self.revisions.get(&key) {
            if existing.definition != definition {
                return Err(RegistryError::ImmutableRevision {
                    id: key.0,
                    revision: key.1,
                });
            }
        } else {
            let compiled = compile(definition).map_err(RegistryError::Invalid)?;
            self.revisions.insert(key.clone(), compiled);
        }
        Ok(&self.revisions[&key])
    }

    pub fn resolve(
        &self,
        reference: &DefinitionRef,
    ) -> Result<&CompiledOrchestration, RegistryError> {
        let compiled = match reference {
            DefinitionRef::Exact { id, revision } => {
                self.revisions
                    .get(&(id.clone(), *revision))
                    .ok_or_else(|| RegistryError::NotFound(format!("{id}@{revision}")))?
            }
            DefinitionRef::LatestPublished(id) => self
                .revisions
                .range((id.clone(), 0)..=(id.clone(), u64::MAX))
                .rev()
                .map(|(_, compiled)| compiled)
                .find(|compiled| compiled.definition.status == DefinitionStatus::Published)
                .ok_or_else(|| RegistryError::NotFound(id.to_string()))?,
            DefinitionRef::Latest(id) => self
                .revisions
                .range((id.clone(), 0)..=(id.clone(), u64::MAX))
                .rev()
                .map(|(_, compiled)| compiled)
                .find(|compiled| compiled.definition.status != DefinitionStatus::Deprecated)
                .ok_or_else(|| RegistryError::NotFound(id.to_string()))?,
        };
        if compiled.definition.status == DefinitionStatus::Draft && !self.allow_drafts {
            return Err(RegistryError::DraftNotExecutable {
                id: compiled.definition_id.clone(),
                revision: compiled.revision,
            });
        }
        Ok(compiled)
    }
}
