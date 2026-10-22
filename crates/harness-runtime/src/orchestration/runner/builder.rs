//! Building an [`OrchestrationRunner`]: its collaborators and run options.

use std::sync::Arc;

use harness_core::orchestration::{CompiledOrchestration, RunOptions};

use super::super::{
    schema::{BasicSchemaValidator, InMemorySchemaResolver, SchemaResolver, SchemaValidator},
    steps::{AgentStepExecutor, ArtifactResolver, ReferenceArtifactResolver, SubflowExecutor},
    store::OrchestrationStore,
};
use super::OrchestrationRunner;

impl OrchestrationRunner {
    pub fn new(compiled: Arc<CompiledOrchestration>, agent: Arc<dyn AgentStepExecutor>) -> Self {
        Self {
            compiled,
            agent,
            schemas: Arc::new(InMemorySchemaResolver::new()),
            validator: Arc::new(BasicSchemaValidator),
            artifacts: Arc::new(ReferenceArtifactResolver),
            store: None,
            available_tools: None,
            options: RunOptions::default(),
            subflows: None,
            depth: 0,
        }
    }

    /// Lets `subflow` nodes run other flows (without it they fail).
    pub fn with_subflows(mut self, subflows: Arc<dyn SubflowExecutor>) -> Self {
        self.subflows = Some(subflows);
        self
    }

    /// Marks this run as the child of a run `depth - 1` levels from the top.
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    /// Options for runs this runner starts (a resumed or retried run keeps
    /// the options it was started with).
    pub fn with_options(mut self, options: RunOptions) -> Self {
        self.options = options;
        self
    }

    pub fn with_schemas(mut self, schemas: Arc<dyn SchemaResolver>) -> Self {
        self.schemas = schemas;
        self
    }

    pub fn with_validator(mut self, validator: Arc<dyn SchemaValidator>) -> Self {
        self.validator = validator;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<dyn ArtifactResolver>) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// Persist events and snapshots, enabling [`resume`](Self::resume).
    pub fn with_store(mut self, store: Arc<dyn OrchestrationStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// The host's registered tools. Allow-listed tools are checked against it
    /// before a run starts, and `ToolScope::Inherit` resolves to it. Without
    /// it, `Inherit` fails closed.
    pub fn with_available_tools(mut self, tools: impl IntoIterator<Item = String>) -> Self {
        let mut tools: Vec<_> = tools.into_iter().collect();
        tools.sort();
        tools.dedup();
        self.available_tools = Some(Arc::new(tools));
        self
    }

    pub fn compiled(&self) -> &Arc<CompiledOrchestration> {
        &self.compiled
    }
}
