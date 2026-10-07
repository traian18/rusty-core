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
    self as orchestration, CompiledOrchestration, DefinitionRef, DefinitionRegistry,
    OrchestrationDefinition, OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationNodeKind,
    OrchestrationRunId, RunOptions, SubflowTarget, MAX_SUBFLOW_DEPTH,
};
use harness_runtime::{
    orchestration::{
        drive_child, AgentExecutionError, InMemorySchemaResolver, OrchestrationHandle,
        OrchestrationRunOutput, OrchestrationRunner, OrchestrationStore, SchemaResolver,
        StepContext, SubflowExecutor, SubflowRequest, WorkspaceArtifactResolver,
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
    /// Choices for this run, e.g. auto-approving approval steps.
    pub options: RunOptions,
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
            options: RunOptions::default(),
        }
    }

    /// Pass approval steps that allow it without asking the user.
    pub fn with_auto_approve(mut self, auto_approve: bool) -> Self {
        self.options.auto_approve = auto_approve;
        self
    }

    /// Run the built-in default workflow on a plain-text request.
    pub fn default_workflow(run_id: impl Into<String>, request: impl Into<String>) -> Self {
        Self {
            run_id: OrchestrationRunId::new(run_id),
            definition: DefinitionRef::LatestPublished(OrchestrationDefinitionId::from(
                DEFAULT_ORCHESTRATION_ID,
            )),
            input: json!({ "request": request.into(), "attachments": [] }),
            options: RunOptions::default(),
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
            .with_options(request.options)
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
            .with_options(request.options)
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
        let parts = Arc::new(RunnerParts {
            session_id: self.session_id,
            session_manager: self.session_manager.clone(),
            config: config.clone(),
            profiles: self.profiles.clone(),
            command_trust: self.command_trust,
        });
        let compiled = Arc::new(parts.resolve(definition)?);
        // Everything the run may reach -- other flows, steps run as flows --
        // must exist, must not call back into itself and must have profiles
        // that resolve, before anything runs.
        parts.check_reachable(&compiled)?;
        parts.runner(compiled, 0).await
    }
}

/// What it takes to build runners for the flows of one session: the session
/// they borrow tools, workspace and backend from, the registered flows, and
/// the profiles they may name. It is also what runs `subflow` nodes.
#[derive(Clone)]
struct RunnerParts {
    session_id: harness_protocol::ids::SessionId,
    session_manager: Arc<harness_runtime::session_manager::SessionManager>,
    config: OrchestrationConfig,
    profiles: Arc<harness_core::behavior::ProfileRegistry>,
    command_trust: crate::profiles::CommandTrust,
}

impl RunnerParts {
    fn resolve(&self, reference: &DefinitionRef) -> Result<CompiledOrchestration, HarnessError> {
        self.config
            .registry
            .read()
            .expect("registry lock poisoned")
            .resolve(reference)
            .cloned()
            .map_err(|error| HarnessError::OrchestrationDefinition(error.to_string()))
    }

    /// The flow a subflow target runs. A step target becomes a one-step
    /// flow whose inputs are `inputs`.
    fn target(
        &self,
        node: &OrchestrationNodeId,
        target: &SubflowTarget,
        inputs: &[String],
    ) -> Result<Arc<CompiledOrchestration>, HarnessError> {
        match target {
            SubflowTarget::Flow { id, revision } => {
                let reference = match revision {
                    Some(revision) => DefinitionRef::Exact {
                        id: id.clone(),
                        revision: *revision,
                    },
                    None => DefinitionRef::Latest(id.clone()),
                };
                self.resolve(&reference).map(Arc::new).map_err(|error| {
                    HarnessError::OrchestrationDefinition(format!("node {node}: {error}"))
                })
            }
            SubflowTarget::Step { .. } => {
                let definition = target
                    .step_definition(node, inputs)
                    .expect("a step target has a definition");
                orchestration::compile(definition)
                    .map(Arc::new)
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {node}: {error}"))
                    })
            }
        }
    }

    /// Every flow `compiled` can reach through subflow nodes must exist and
    /// the chain must not loop or nest deeper than [`MAX_SUBFLOW_DEPTH`].
    /// The profiles of all of them must resolve.
    fn check_reachable(&self, compiled: &Arc<CompiledOrchestration>) -> Result<(), HarnessError> {
        let mut chain = Vec::new();
        self.check_flow(compiled, 0, &mut chain)
    }

    fn check_flow(
        &self,
        compiled: &Arc<CompiledOrchestration>,
        depth: u32,
        chain: &mut Vec<(String, u64)>,
    ) -> Result<(), HarnessError> {
        let key = (compiled.definition_id.to_string(), compiled.revision);
        if chain.contains(&key) {
            return Err(HarnessError::OrchestrationDefinition(format!(
                "flow {}@{} runs itself through its subflows",
                key.0, key.1
            )));
        }
        chain.push(key);
        self.check_profiles(compiled)?;
        for node in compiled.nodes.values() {
            let targets: Vec<&SubflowTarget> = match &node.kind {
                OrchestrationNodeKind::Subflow(config) => vec![&config.target],
                OrchestrationNodeKind::Agent(config) => config
                    .task_queue
                    .iter()
                    .flat_map(|queue| queue.flows.values())
                    .collect(),
                _ => continue,
            };
            if targets.is_empty() {
                continue;
            }
            if depth + 1 > MAX_SUBFLOW_DEPTH {
                return Err(HarnessError::OrchestrationDefinition(format!(
                    "node {}: flows may run flows at most {MAX_SUBFLOW_DEPTH} levels deep",
                    node.id
                )));
            }
            let inputs: Vec<String> = node
                .input_bindings
                .iter()
                .map(|binding| binding.target.clone())
                .collect();
            for target in targets {
                let child = self.target(&node.id, target, &inputs)?;
                self.check_flow(&child, depth + 1, chain)?;
            }
        }
        chain.pop();
        Ok(())
    }

    /// Every profile a step names must resolve before anything runs.
    fn check_profiles(&self, compiled: &CompiledOrchestration) -> Result<(), HarnessError> {
        for node in compiled.nodes.values() {
            let OrchestrationNodeKind::Agent(config) = &node.kind else {
                continue;
            };
            let reviewer = config
                .task_queue
                .as_ref()
                .map(|queue| &queue.review_profile);
            for reference in config.profile.iter().chain(reviewer) {
                let closure = self
                    .profiles
                    .resolve(reference)
                    .and_then(|profile| self.profiles.resolve_closure(&profile))
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {}: {error}", node.id))
                    })?;
                crate::profiles::check_command_trust(&self.profiles, &closure, self.command_trust)
                    .map_err(|error| {
                        HarnessError::OrchestrationDefinition(format!("node {}: {error}", node.id))
                    })?;
            }
        }
        Ok(())
    }

    /// A runner for `compiled` on this session, `depth` levels down.
    async fn runner(
        self: &Arc<Self>,
        compiled: Arc<CompiledOrchestration>,
        depth: u32,
    ) -> Result<OrchestrationRunner, HarnessError> {
        let parent = self
            .session_manager
            .session_handle(self.session_id)
            .await
            .ok_or(HarnessError::SessionUnavailable(self.session_id))?;

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

        let mut runner = OrchestrationRunner::new(
            compiled,
            Arc::new(
                IsolatedSessionAgentExecutor::new(parent.clone())
                    .with_profiles(self.profiles.clone())
                    .with_subflows(self.clone()),
            ),
        )
        .with_schemas(self.config.schemas.clone())
        .with_artifacts(Arc::new(WorkspaceArtifactResolver::new(
            parent.workspace.clone(),
        )))
        .with_available_tools(available_tools)
        .with_subflows(self.clone())
        .with_depth(depth);
        // Child runs are part of their parent's run: only the top one is stored.
        if depth == 0 {
            if let Some(store) = &self.config.store {
                runner = runner.with_store(store.clone());
            }
        }
        Ok(runner)
    }
}

#[async_trait::async_trait]
impl SubflowExecutor for RunnerParts {
    async fn execute(
        &self,
        request: SubflowRequest,
        context: &mut StepContext,
    ) -> Result<Value, AgentExecutionError> {
        let fail =
            |error: HarnessError| AgentExecutionError::new("subflow_failed", error.to_string());
        let inputs: Vec<String> = request
            .input
            .as_object()
            .map(|fields| fields.keys().cloned().collect())
            .unwrap_or_default();
        let child = self
            .target(&request.node_id, &request.target, &inputs)
            .map_err(fail)?;
        if request.depth > MAX_SUBFLOW_DEPTH {
            return Err(AgentExecutionError::new(
                "subflow_too_deep",
                format!("flows may run flows at most {MAX_SUBFLOW_DEPTH} levels deep"),
            ));
        }
        // The child borrows the same session, flows and profiles.
        let parts = Arc::new(self.clone());
        let handle = parts
            .runner(child, request.depth)
            .await
            .map_err(fail)?
            .with_options(request.options)
            .start(
                OrchestrationRunId::new(format!(
                    "{}-{}-{}",
                    request.run_id, request.node_id, request.attempt
                )),
                request.input,
            )
            .map_err(|error| AgentExecutionError::new("subflow_failed", error.to_string()))?;
        drive_child(handle, context, request.node_id.as_str()).await
    }
}
