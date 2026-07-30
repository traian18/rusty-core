//! Provider-neutral `ModelClient` adapter for the runtime `ExecutionBackend`.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::ModelRequest;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionResult,
};
use harness_protocol::ids::{BackendId, RequestId};
use harness_protocol::tools::ToolCall;
use harness_runtime::traits::ExecutionBackend;

/// Adapts any provider-neutral model client to the harness backend contract.
pub struct GenericModelBackend {
    model_client: Arc<dyn ModelClient>,
    descriptor: BackendDescriptor,
    capabilities: BackendCapabilities,
}

impl GenericModelBackend {
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        let model = model_client.capabilities();
        let capabilities = BackendCapabilities {
            streaming: model.streaming,
            reasoning_stream: model.reasoning,
            tool_calls: model.tool_calls,
            parallel_tool_calls: model.parallel_tool_calls,
            host_managed_tools: true,
            ..Default::default()
        };
        Self {
            model_client,
            descriptor: BackendDescriptor {
                id: BackendId::new(),
                name: "generic-model-backend".to_string(),
                description: "Provider-neutral model backend".to_string(),
                capabilities: capabilities.clone(),
            },
            capabilities,
        }
    }

    fn translate_event(
        event: ModelEvent,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
    ) -> Option<Result<ExecutionResult, ExecutionError>> {
        match event {
            ModelEvent::TextDelta { delta } => {
                let _ = sink.send(ExecutionEvent::TextDelta { request_id, delta });
                None
            }
            ModelEvent::ReasoningDelta { delta } => {
                let _ = sink.send(ExecutionEvent::ReasoningDelta { request_id, delta });
                None
            }
            ModelEvent::ToolCallStarted { .. } | ModelEvent::ToolCallDelta { .. } => None,
            ModelEvent::ToolCallCompleted { id, name, input } => {
                let _ = sink.send(ExecutionEvent::ToolCallRequested {
                    request_id,
                    call: ToolCall {
                        id,
                        name,
                        arguments: input,
                    },
                });
                None
            }
            ModelEvent::UsageUpdate { usage } => {
                let _ = sink.send(ExecutionEvent::UsageUpdate { request_id, usage });
                None
            }
            ModelEvent::Completed { result } => {
                Some(Ok(to_execution_result(request_id, result)))
            }
            ModelEvent::Error { error } => Some(Err(to_execution_error(error))),
        }
    }
}

fn to_execution_result(request_id: RequestId, result: ModelResult) -> ExecutionResult {
    ExecutionResult {
        request_id,
        usage: result.usage,
        cost: result.cost,
        finish_reason: result.stop_reason,
    }
}

fn to_execution_error(error: ModelError) -> ExecutionError {
    match error {
        ModelError::BackendError { message, code } => ExecutionError::BackendError { message, code },
        ModelError::RateLimited { retry_after } => ExecutionError::RateLimited { retry_after },
        ModelError::InvalidRequest { message } => ExecutionError::InvalidRequest { message },
        ModelError::Cancelled => ExecutionError::Cancelled,
        ModelError::Timeout => ExecutionError::Timeout,
        ModelError::Protocol { message } => ExecutionError::BackendError {
            message,
            code: "PROTOCOL_ERROR".to_string(),
        },
    }
}

fn emit_terminal(
    sink: &broadcast::Sender<ExecutionEvent>,
    request_id: RequestId,
    result: &Result<ExecutionResult, ExecutionError>,
) {
    match result {
        Ok(result) => {
            let _ = sink.send(ExecutionEvent::Completed {
                request_id,
                result: result.clone(),
            });
        }
        Err(error) => {
            let _ = sink.send(ExecutionEvent::Error {
                request_id,
                error: error.clone(),
            });
        }
    }
}

#[async_trait]
impl ExecutionBackend for GenericModelBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities.clone()
    }

    async fn execute(
        &self,
        request: harness_protocol::backend::ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let request_id = request.request_id;
        let model_request = ModelRequest {
            system_prompt: request.system_prompt,
            messages: request.messages,
            tools: request.tools,
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            extended_thinking: request.extended_thinking,
        };
        let (model_tx, mut model_rx) = broadcast::channel(256);
        let client = self.model_client.clone();
        let stream_cancel = cancel.clone();
        let mut stream = tokio::spawn(async move {
            client.stream(model_request, model_tx, stream_cancel).await
        });
        let mut stream_joined = false;

        let final_result = loop {
            tokio::select! {
                message = model_rx.recv() => match message {
                    Ok(event) => {
                        if let Some(result) = Self::translate_event(event, request_id, &sink) {
                            emit_terminal(&sink, request_id, &result);
                            break result;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let result = Err(ExecutionError::BackendError {
                            message: format!("lost {count} model events"),
                            code: "EVENT_LAG".to_string(),
                        });
                        emit_terminal(&sink, request_id, &result);
                        break result;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        stream_joined = true;
                        let result = match (&mut stream).await {
                            Ok(Ok(result)) => Ok(to_execution_result(request_id, result)),
                            Ok(Err(error)) => Err(to_execution_error(error)),
                            Err(error) => Err(ExecutionError::BackendError {
                                message: format!("model client task failed: {error}"),
                                code: "TASK_PANIC".to_string(),
                            }),
                        };
                        emit_terminal(&sink, request_id, &result);
                        break result;
                    }
                },
                _ = cancel.cancelled() => {
                    let result = Err(ExecutionError::Cancelled);
                    emit_terminal(&sink, request_id, &result);
                    break result;
                }
            }
        };

        if !stream_joined {
            let _ = stream.await;
        }
        final_result
    }
}
