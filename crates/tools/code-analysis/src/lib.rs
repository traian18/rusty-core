use std::sync::Arc;

use async_trait::async_trait;
use harness_code_analysis::{CallGraph, CallGraphError, CallsFromRequest, CallsToRequest};
use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};
use serde_json::json;

struct GraphTool {
    graph: Arc<dyn CallGraph>,
    name: &'static str,
}

pub fn tool(graph: Arc<dyn CallGraph>, name: &'static str) -> Arc<dyn ToolExecutor> {
    Arc::new(GraphTool { graph, name })
}

pub fn tools(graph: Arc<dyn CallGraph>) -> Vec<Arc<dyn ToolExecutor>> {
    [
        "code_graph.refresh",
        "code_graph.capabilities",
        "code_graph.find_callables",
        "code_graph.calls",
        "code_graph.traverse",
    ]
    .into_iter()
    .map(|name| {
        Arc::new(GraphTool {
            graph: graph.clone(),
            name,
        }) as Arc<dyn ToolExecutor>
    })
    .collect()
}

#[async_trait]
impl ToolExecutor for GraphTool {
    fn descriptor(&self) -> ToolDescriptor {
        let input_schema = match self.name {
            "code_graph.capabilities" => json!({"type":"object","additionalProperties":false}),
            "code_graph.refresh" => {
                json!({"type":"object","properties":{"max_files":{"type":"integer","minimum":1}},"additionalProperties":false})
            }
            "code_graph.find_callables" => {
                json!({"type":"object","required":["limit"],"properties":{"snapshot":{"type":"string"},"language":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1}},"additionalProperties":false})
            }
            "code_graph.calls" => {
                json!({"type":"object","required":["snapshot","limit"],"oneOf":[{"required":["caller"]},{"required":["target"]}],"properties":{"snapshot":{"type":"string"},"caller":{"type":"string"},"target":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1}},"additionalProperties":false})
            }
            "code_graph.traverse" => {
                json!({"type":"object","required":["snapshot","roots","depth","limit"],"properties":{"snapshot":{"type":"string"},"roots":{"type":"array","items":{"type":"string"}},"depth":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}},"additionalProperties":false})
            }
            _ => json!({"type":"object","additionalProperties":false}),
        };
        ToolDescriptor {
            id: ToolId::new(self.name),
            name: self.name.into(),
            description: "Query the authorized workspace call graph".into(),
            input_schema,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        if cancel.is_cancelled() {
            return Ok(ToolResult {
                call_id: self.name.into(),
                output: structured_error(CallGraphError::Cancelled),
                is_error: true,
            });
        }
        let result = match self.name {
            "code_graph.capabilities" => Ok(json!(self.graph.capabilities())),
            "code_graph.refresh" => call_refresh(&*self.graph, input, cancel).await,
            "code_graph.find_callables" => call_find(&*self.graph, input, cancel).await,
            "code_graph.calls" => call_calls(&*self.graph, input, cancel).await,
            "code_graph.traverse" => call_traverse(&*self.graph, input, cancel).await,
            _ => Err(CallGraphError::InvalidRequest("unknown graph tool".into())),
        };
        match result {
            Ok(output) => Ok(ToolResult {
                call_id: self.name.into(),
                output,
                is_error: false,
            }),
            Err(error) => Ok(ToolResult {
                call_id: self.name.into(),
                output: structured_error(error),
                is_error: true,
            }),
        }
    }
}

fn structured_error(error: CallGraphError) -> serde_json::Value {
    let code = match &error {
        CallGraphError::InvalidRequest(_) => "invalid_request",
        CallGraphError::SnapshotNotFound(_) => "snapshot_not_found",
        CallGraphError::Cancelled => "cancelled",
        CallGraphError::DependencyFailure(_) => "dependency_failure",
        CallGraphError::LimitExceeded(_) => "limit_exceeded",
    };
    json!({
        "schema_version": 1,
        "error": {
            "code": code,
            "message": error.to_string(),
        }
    })
}

async fn call_refresh(
    graph: &dyn CallGraph,
    input: ToolInput,
    cancel: CancellationToken,
) -> Result<serde_json::Value, CallGraphError> {
    let request = serde_json::from_value(input.arguments)
        .map_err(|error| CallGraphError::InvalidRequest(error.to_string()))?;
    Ok(json!(graph.refresh(request, cancel).await?))
}

async fn call_find(
    graph: &dyn CallGraph,
    input: ToolInput,
    cancel: CancellationToken,
) -> Result<serde_json::Value, CallGraphError> {
    let request = serde_json::from_value(input.arguments)
        .map_err(|error| CallGraphError::InvalidRequest(error.to_string()))?;
    Ok(json!(graph.find_callables(request, cancel).await?))
}

async fn call_calls(
    graph: &dyn CallGraph,
    input: ToolInput,
    cancel: CancellationToken,
) -> Result<serde_json::Value, CallGraphError> {
    let arguments = input.arguments;
    if arguments.get("caller").is_some() {
        let request: CallsFromRequest = serde_json::from_value(arguments)
            .map_err(|error| CallGraphError::InvalidRequest(error.to_string()))?;
        return Ok(json!(graph.calls_from(request, cancel).await?));
    }
    let request: CallsToRequest = serde_json::from_value(arguments)
        .map_err(|error| CallGraphError::InvalidRequest(error.to_string()))?;
    Ok(json!(graph.calls_to(request, cancel).await?))
}
async fn call_traverse(
    graph: &dyn CallGraph,
    input: ToolInput,
    cancel: CancellationToken,
) -> Result<serde_json::Value, CallGraphError> {
    let request = serde_json::from_value(input.arguments)
        .map_err(|error| CallGraphError::InvalidRequest(error.to_string()))?;
    Ok(json!(graph.traverse(request, cancel).await?))
}
