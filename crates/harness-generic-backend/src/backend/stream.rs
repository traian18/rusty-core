//! One provider call: stream its events to the caller and turn the way the
//! stream ended into a result.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::ModelRequest;
use harness_protocol::backend::ExecutionEvent;
use harness_protocol::ids::RequestId;
use harness_protocol::tools::ToolCall;

use super::convert::send_event;
use super::GenericModelBackend;

#[derive(Default)]
pub(super) struct AttemptProgress {
    pub(super) text: String,
    pub(super) requested_tools: bool,
    pub(super) emitted_output: bool,
}

pub(super) struct AttemptOutcome {
    pub(super) result: Result<ModelResult, ModelError>,
    pub(super) emitted_output: bool,
}

/// Bounded and lossless: see `harness_model::client::ModelEventSender` for
/// why this is not a `broadcast` channel. The capacity only sets how far the
/// client may run ahead before `send` makes it wait.
pub(super) const MODEL_EVENT_BUFFER: usize = 256;

enum AttemptExit {
    Completed(ModelResult),
    Error(ModelError),
    Closed,
    Cancelled,
    TimedOut,
}

impl GenericModelBackend {
    pub(super) async fn run_attempt(
        &self,
        model_request: ModelRequest,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
        progress: &mut AttemptProgress,
    ) -> AttemptOutcome {
        progress.text.clear();
        progress.requested_tools = false;
        progress.emitted_output = false;

        let (model_tx, model_rx) = tokio::sync::mpsc::channel(MODEL_EVENT_BUFFER);
        let attempt_cancel = cancel.child_token();
        let stream = spawn_model_stream(
            &self.model_client,
            model_request,
            model_tx,
            attempt_cancel.clone(),
        );
        let exit = self
            .receive_attempt_events(model_rx, request_id, sink, cancel, progress)
            .await;
        let result = finish_attempt(exit, stream, attempt_cancel, progress.requested_tools).await;
        AttemptOutcome {
            result,
            emitted_output: progress.emitted_output,
        }
    }

    async fn receive_attempt_events(
        &self,
        mut model_rx: tokio::sync::mpsc::Receiver<ModelEvent>,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        cancel: &CancellationToken,
        progress: &mut AttemptProgress,
    ) -> AttemptExit {
        let mut deadline = tokio::time::Instant::now() + self.recovery.first_event_timeout;
        loop {
            tokio::select! {
                message = model_rx.recv() => match message {
                    Some(event) => {
                        deadline = tokio::time::Instant::now() + self.recovery.idle_timeout;
                        if let Some(exit) = self.process_attempt_event(event, request_id, sink, progress) {
                            drop(model_rx);
                            return exit;
                        }
                    }
                    None => {
                        drop(model_rx);
                        return AttemptExit::Closed;
                    },
                },
                _ = cancel.cancelled() => {
                    drop(model_rx);
                    return AttemptExit::Cancelled;
                },
                _ = tokio::time::sleep_until(deadline) => {
                    drop(model_rx);
                    return AttemptExit::TimedOut;
                },
            }
        }
    }

    fn process_attempt_event(
        &self,
        event: ModelEvent,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
        progress: &mut AttemptProgress,
    ) -> Option<AttemptExit> {
        match event {
            ModelEvent::Completed { mut result } => {
                if progress.requested_tools {
                    result.stop_reason = "tool_use".into();
                }
                Some(AttemptExit::Completed(result))
            }
            ModelEvent::Error { error } => Some(AttemptExit::Error(error)),
            event => {
                progress.requested_tools |= matches!(event, ModelEvent::ToolCallCompleted { .. });
                if let ModelEvent::TextDelta { delta } = &event {
                    progress.text.push_str(delta);
                }
                progress.emitted_output |= event_produces_output(&event);
                Self::translate_event(event, request_id, sink);
                None
            }
        }
    }

    fn translate_event(
        event: ModelEvent,
        request_id: RequestId,
        sink: &broadcast::Sender<ExecutionEvent>,
    ) {
        match event {
            ModelEvent::TextDelta { delta } => {
                send_event(sink, ExecutionEvent::TextDelta { request_id, delta });
            }
            ModelEvent::ReasoningDelta { delta } => {
                send_event(sink, ExecutionEvent::ReasoningDelta { request_id, delta });
            }
            ModelEvent::ToolCallStarted { .. } | ModelEvent::ToolCallDelta { .. } => {}
            ModelEvent::ToolCallCompleted { id, name, input } => {
                send_event(
                    sink,
                    ExecutionEvent::ToolCallRequested {
                        request_id,
                        call: ToolCall {
                            id,
                            name,
                            arguments: input,
                        },
                    },
                );
            }
            ModelEvent::UsageUpdate { usage } => {
                send_event(sink, ExecutionEvent::UsageUpdate { request_id, usage });
            }
            // Ends the attempt in `process_attempt_event`; `execute_request`
            // emits the one terminal event once retries are settled.
            ModelEvent::Completed { .. } | ModelEvent::Error { .. } => {}
        }
    }
}

fn event_produces_output(event: &ModelEvent) -> bool {
    matches!(
        event,
        ModelEvent::TextDelta { .. }
            | ModelEvent::ReasoningDelta { .. }
            | ModelEvent::ToolCallCompleted { .. }
            | ModelEvent::UsageUpdate { .. }
    )
}

fn spawn_model_stream(
    model_client: &Arc<dyn ModelClient>,
    model_request: ModelRequest,
    model_tx: tokio::sync::mpsc::Sender<ModelEvent>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<ModelResult, ModelError>> {
    let client = Arc::clone(model_client);
    tokio::spawn(async move { client.stream(model_request, model_tx, cancel).await })
}

async fn finish_attempt(
    exit: AttemptExit,
    stream: tokio::task::JoinHandle<Result<ModelResult, ModelError>>,
    attempt_cancel: CancellationToken,
    requested_tools: bool,
) -> Result<ModelResult, ModelError> {
    match exit {
        AttemptExit::Completed(result) => {
            log_stream_join_failure(stream.await);
            Ok(result)
        }
        AttemptExit::Error(error) => {
            log_stream_join_failure(stream.await);
            Err(error)
        }
        AttemptExit::Closed => finish_closed_attempt(stream.await, requested_tools),
        AttemptExit::Cancelled => {
            attempt_cancel.cancel();
            log_stream_join_failure(stream.await);
            Err(ModelError::Cancelled)
        }
        AttemptExit::TimedOut => {
            attempt_cancel.cancel();
            log_stream_join_failure(stream.await);
            Err(ModelError::Timeout)
        }
    }
}

fn finish_closed_attempt(
    stream_result: Result<Result<ModelResult, ModelError>, tokio::task::JoinError>,
    requested_tools: bool,
) -> Result<ModelResult, ModelError> {
    let mut result = match stream_result {
        Ok(result) => result,
        Err(error) => Err(ModelError::BackendError {
            message: format!("model client task failed: {error}"),
            code: "TASK_PANIC".to_string(),
        }),
    };
    if requested_tools {
        if let Ok(result) = &mut result {
            result.stop_reason = "tool_use".into();
        }
    }
    result
}

fn log_stream_join_failure<T>(result: Result<T, tokio::task::JoinError>) {
    if let Err(error) = result {
        warn!(%error, "model client task failed while finishing an attempt");
    }
}
