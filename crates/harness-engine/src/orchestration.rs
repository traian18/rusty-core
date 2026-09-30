//! High-level orchestration API built on an existing engine session.
//!
//! Orchestration is optional and additive: a harness built without
//! [`HarnessBuilder::orchestration`](crate::HarnessBuilder::orchestration)
//! behaves exactly as before, and direct sessions never go through it.
//!
//! ```no_run
//! # use harness_engine::{Harness, HarnessError, OrchestrationConfig, OrchestrationRequest};
//! # async fn example(session: harness_engine::SessionHandle) -> Result<(), HarnessError> {
//! let mut run = session
//!     .start_orchestration(OrchestrationRequest::default_workflow("run-1", "Add password reset"))
//!     .await?;
//! let mut updates = run.subscribe();
//! // ... observe updates, resolve permissions with run.resolve_permission(..) ...
//! let output = run.wait().await.map_err(|e| HarnessError::OrchestrationRuntime(e.to_string()))?;
//! # let _ = (updates, output); Ok(()) }
//! ```

use std::sync::{Arc, RwLock};

use harness_core::orchestration::{
    DefinitionRef, DefinitionRegistry, OrchestrationDefinition, OrchestrationDefinitionId,
    OrchestrationRunId,
};
use harness_runtime::{
    orchestration::{
        InMemorySchemaResolver, OrchestrationHandle, OrchestrationRunOutput, OrchestrationRunner,
        OrchestrationStore, SchemaResolver, WorkspaceArtifactResolver,
    },
    session_agent_executor::IsolatedSessionAgentExecutor,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{HarnessError, SessionHandle};

/// Identifier of the built-in `Input → Agent → Verify → Output` definition.
pub const DEFAULT_ORCHESTRATION_ID: &str = "rusty.default";

/// Engine-level orchestration configuration. Cheap to clone; clones share
/// the definition registry.
#[derive(Clone)]
pub struct OrchestrationConfig {
    registry: Arc<RwLock<DefinitionRegistry>>,
    store: Option<Arc<dyn OrchestrationStore>>,
    schemas: Arc<dyn SchemaResolver>,
}

impl Default for OrchestrationConfig {
    /// The built-in default definition, in-memory schema registry, and no
    /// durable store (runs cannot be resumed after a restart).
    fn default() -> Self {
        Self {
            registry: Arc::new(RwLock::new(DefinitionRegistry::with_builtin())),
            store: None,
            schemas: Arc::new(InMemorySchemaResolver::new()),
        }
    }
}

impl OrchestrationConfig {
    /// Persist orchestration events and snapshots, enabling resume.
    pub fn with_store(mut self, store: Arc<dyn OrchestrationStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Resolve `SchemaReference::Registry` schemas.
    pub fn with_schemas(mut self, schemas: Arc<dyn SchemaResolver>) -> Self {
        self.schemas = schemas;
        self
    }

    /// Execute draft definitions (development mode).
    pub fn allow_drafts(self, allow: bool) -> Self {
        {
            let mut registry = self.registry.write().expect("registry lock poisoned");
            *registry = std::mem::take(&mut *registry).allow_drafts(allow);
        }
        self
    }

    /// Validate, compile, and register a definition revision. Published
    /// revisions are immutable: re-registering one with different content
    /// is rejected.
    pub fn register(&self, definition: OrchestrationDefinition) -> Result<(), HarnessError> {
        self.registry
            .write()
            .expect("registry lock poisoned")
            .register(definition)
            .map(|_| ())
            .map_err(|error| HarnessError::OrchestrationDefinition(error.to_string()))
    }
}

impl OrchestrationConfig {
    /// Register a definition given as JSON (for example a
    /// `.rusty/workflows/*.json` file) and return its id and revision.
    pub fn register_json(&self, document: Value) -> Result<(String, u64), HarnessError> {
        let definition: OrchestrationDefinition = serde_json::from_value(document)
            .map_err(|error| HarnessError::OrchestrationDefinition(error.to_string()))?;
        let key = (definition.id.to_string(), definition.revision);
        self.register(definition)?;
        Ok(key)
    }
}

/// A request to start an orchestration run.
#[derive(Debug, Clone)]
pub struct OrchestrationRequest {
    pub run_id: OrchestrationRunId,
    pub definition: DefinitionRef,
    pub input: Value,
}

impl OrchestrationRequest {
    /// Run exactly revision `revision` of definition `id` on `input`.
    pub fn exact(
        run_id: impl Into<String>,
        id: impl Into<String>,
        revision: u64,
        input: Value,
    ) -> Self {
        Self {
            run_id: OrchestrationRunId::new(run_id),
            definition: DefinitionRef::Exact {
                id: OrchestrationDefinitionId::new(id),
                revision,
            },
            input,
        }
    }

    /// Run the built-in default workflow on a plain-text request.
    pub fn default_workflow(run_id: impl Into<String>, request: impl Into<String>) -> Self {
        Self {
            run_id: OrchestrationRunId::new(run_id),
            definition: DefinitionRef::LatestPublished(OrchestrationDefinitionId::from(
                DEFAULT_ORCHESTRATION_ID,
            )),
            input: json!({ "request": request.into(), "attachments": [] }),
        }
    }
}

impl SessionHandle {
    /// Start a workflow. Every agent-step attempt runs in a fresh, isolated,
    /// tool-scoped session that inherits this session's backend, workspace,
    /// active model, and tool permission policies. Permission requests are
    /// surfaced through the returned handle.
    pub async fn start_orchestration(
        &self,
        request: OrchestrationRequest,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .start(request.run_id, request.input)
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Start a workflow and wait for its result, cancelling it when
    /// `cancellation` fires.
    pub async fn run_orchestration(
        &self,
        request: OrchestrationRequest,
        cancellation: CancellationToken,
    ) -> Result<OrchestrationRunOutput, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .run(request.run_id, request.input, cancellation)
            .await
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Resume a run from its last durable step boundary (requires a store on
    /// the [`OrchestrationConfig`]). `definition` must name the exact
    /// revision the run started on.
    pub async fn resume_orchestration(
        &self,
        run_id: OrchestrationRunId,
        definition: DefinitionRef,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&definition)
            .await?
            .resume(run_id)
            .await
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    /// Retry only the failed step of a validated checkpoint, on this session's backend.
    pub async fn retry_orchestration(
        &self,
        request: OrchestrationRequest,
        state: harness_core::orchestration::OrchestrationRunState,
        guidance: String,
    ) -> Result<OrchestrationHandle, HarnessError> {
        self.orchestration_runner(&request.definition)
            .await?
            .retry_failed(state, request.run_id, guidance)
            .map_err(|error| HarnessError::OrchestrationRuntime(error.to_string()))
    }

    async fn orchestration_runner(
        &self,
        definition: &DefinitionRef,
    ) -> Result<OrchestrationRunner, HarnessError> {
        let config = self
            .orchestration
            .as_ref()
            .ok_or(HarnessError::OrchestrationNotConfigured)?;
        let parent = self
            .session_manager
            .session_handle(self.session_id)
            .await
            .ok_or(HarnessError::SessionUnavailable(self.session_id))?;
        let compiled = config
            .registry
            .read()
            .expect("registry lock poisoned")
            .resolve(definition)
            .map_err(|error| HarnessError::OrchestrationDefinition(error.to_string()))?
            .clone();

        // `inherit` means exactly what this session's root agent may use:
        // enabled in its toolset and actually registered.
        let registered: std::collections::HashSet<String> = parent
            .tool_registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id.to_string())
            .collect();
        let available_tools: Vec<String> = parent
            .root_toolset()
            .enabled_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name.clone())
            .filter(|name| registered.contains(name))
            .collect();

        // Every profile a step names must resolve before anything runs.
        for node in compiled.nodes.values() {
            if let harness_core::orchestration::OrchestrationNodeKind::Agent(config) = &node.kind {
                if let Some(reference) = &config.profile {
                    let closure = self
                        .profiles
                        .resolve(reference)
                        .and_then(|profile| self.profiles.resolve_closure(&profile))
                        .map_err(|error| {
                            HarnessError::OrchestrationDefinition(format!(
                                "node {}: {error}",
                                node.id
                            ))
                        })?;
                    crate::profiles::check_command_trust(
                        &self.profiles,
                        &closure,
                        self.command_trust,
                    )
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {}: {error}", node.id))
                    })?;
                }
            }
        }

        let mut runner = OrchestrationRunner::new(
            Arc::new(compiled),
            Arc::new(
                IsolatedSessionAgentExecutor::new(parent.clone())
                    .with_profiles(self.profiles.clone()),
            ),
        )
        .with_schemas(config.schemas.clone())
        .with_artifacts(Arc::new(WorkspaceArtifactResolver::new(
            parent.workspace.clone(),
        )))
        .with_available_tools(available_tools);
        if let Some(store) = &config.store {
            runner = runner.with_store(store.clone());
        }
        Ok(runner)
    }
}
