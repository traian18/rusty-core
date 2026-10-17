//! One tool = one [`Operation`]; [`OperationTool`] adapts any operation to `ToolExecutor`.
use std::sync::Arc;

use async_trait::async_trait;
use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};
use serde_json::{json, Value};

use crate::failure::Failure;

/// Version of the response envelope shared by every tool in this crate.
pub const SCHEMA_VERSION: u32 = 1;

/// What a successful operation reports besides its payload.
pub(crate) struct Success {
    pub snapshot: Option<String>,
    pub git_snapshot: Option<String>,
    pub components: Vec<(String, u32)>,
    pub partial: bool,
    /// Why the result is smaller than the full answer, when it is.
    pub truncation: Option<&'static str>,
    pub result: Value,
}

impl Success {
    pub fn plain(result: Value) -> Self {
        Self {
            snapshot: None,
            git_snapshot: None,
            components: Vec::new(),
            partial: false,
            truncation: None,
            result,
        }
    }
}

#[async_trait]
pub(crate) trait Operation: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    async fn run(&self, arguments: Value, cancel: &CancellationToken) -> Result<Success, Failure>;
}

pub(crate) struct OperationTool {
    operation: Arc<dyn Operation>,
}

impl OperationTool {
    pub fn executor(operation: impl Operation + 'static) -> Arc<dyn ToolExecutor> {
        Arc::new(Self {
            operation: Arc::new(operation),
        })
    }
}

#[async_trait]
impl ToolExecutor for OperationTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: ToolId::new(self.operation.name()),
            name: self.operation.name().into(),
            description: self.operation.description().into(),
            input_schema: self.operation.schema(),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let name = self.operation.name();
        let outcome = if cancel.is_cancelled() {
            Err(Failure::cancelled())
        } else {
            self.operation.run(input.arguments, &cancel).await
        };
        let (output, is_error) = match outcome {
            Ok(success) => (envelope(success), false),
            Err(failure) => (error_envelope(failure), true),
        };
        Ok(ToolResult {
            call_id: name.into(),
            output,
            is_error,
        })
    }
}

fn envelope(success: Success) -> Value {
    let components = success
        .components
        .iter()
        .map(|(id, version)| json!({"id": id, "version": version}))
        .collect::<Vec<_>>();
    json!({
        "schema_version": SCHEMA_VERSION,
        "snapshot": success.snapshot,
        "git_snapshot": success.git_snapshot,
        "component_versions": components,
        "partial": success.partial,
        "truncation": success.truncation,
        "result": success.result,
    })
}

fn error_envelope(failure: Failure) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "error": {"code": failure.code, "message": failure.message},
    })
}
