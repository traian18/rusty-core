//! Scripted provider-neutral model client used by backend contract tests.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_model::client::ModelClient;
use harness_model::events::{ModelError, ModelEvent, ModelResult};
use harness_model::request::{ModelCapabilities, ModelRequest};

const STEP_DELAY: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Default)]
pub struct FakeModelClient {
    events: Vec<ModelEvent>,
    result: Option<ModelResult>,
    error: Option<ModelError>,
    block_until_cancelled: bool,
}

impl FakeModelClient {
    pub fn new() -> Self { Self::default() }
    pub fn with_events(mut self, events: Vec<ModelEvent>) -> Self { self.events = events; self }
    pub fn with_result(mut self, result: ModelResult) -> Self { self.result = Some(result); self }
    pub fn with_error(mut self, error: ModelError) -> Self { self.error = Some(error); self }
    pub fn block_until_cancelled(mut self) -> Self { self.block_until_cancelled = true; self }
}

#[async_trait]
impl ModelClient for FakeModelClient {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities { streaming: true, reasoning: true, tool_calls: true, parallel_tool_calls: true }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        events: broadcast::Sender<ModelEvent>,
        cancel: CancellationToken,
    ) -> Result<ModelResult, ModelError> {
        if self.block_until_cancelled {
            cancel.cancelled().await;
            return Err(ModelError::Cancelled);
        }
        for event in &self.events {
            let _ = events.send(event.clone());
            tokio::select! {
                _ = tokio::time::sleep(STEP_DELAY) => {},
                _ = cancel.cancelled() => return Err(ModelError::Cancelled),
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(STEP_DELAY) => {},
            _ = cancel.cancelled() => return Err(ModelError::Cancelled),
        }
        if let Some(error) = self.error.clone() {
            let _ = events.send(ModelEvent::Error { error: error.clone() });
            return Err(error);
        }
        if let Some(result) = self.result.clone() {
            let _ = events.send(ModelEvent::Completed { result: result.clone() });
            return Ok(result);
        }
        Err(ModelError::BackendError {
            message: "FakeModelClient has no scripted result or error".to_string(),
            code: "FAKE_MODEL_CLIENT_UNSCRIPTED".to_string(),
        })
    }
}
