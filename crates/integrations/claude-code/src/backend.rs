use std::sync::Arc;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tokio::sync::broadcast;

use harness_protocol::backend::{BackendCapabilities, BackendDescriptor, ExecutionRequest, ExecutionEvent, ExecutionResult, ExecutionError};
use harness_protocol::ids::BackendId;
use harness_runtime::traits::ExecutionBackend;
use harness_runtime::IntegrationFactory;

use crate::config::ClaudeCodeConfig;
use crate::executor::ClaudeCodeExecutor;

/// ExecutionBackend implementation that wraps the Claude Code CLI.
pub struct ClaudeCodeBackend {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeBackend {
    /// Create a new Claude Code backend from configuration.
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl ExecutionBackend for ClaudeCodeBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "Claude Code".to_string(),
            description: "Claude Code CLI invoked as a subprocess".to_string(),
            capabilities: BackendCapabilities {
                streaming: true,
                tool_calls: true,
                host_managed_tools: false, // Claude Code manages its own tools
                ..Default::default()
            },
        }
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.descriptor().capabilities
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        let executor = ClaudeCodeExecutor::new(self.config.clone());
        let request_id = request.request_id;

        // Spawn the subprocess
        let child = executor.spawn(&request).await?;

        // Create a task to monitor cancellation
        let child_arc = Arc::new(tokio::sync::Mutex::new(Some(child)));
        let child_clone = Arc::clone(&child_arc);
        let cancel_clone = cancel.clone();

        let cancel_task = tokio::spawn(async move {
            cancel_clone.cancelled().await;
            if let Some(mut child) = child_clone.lock().await.take() {
                let _ = child.kill().await;
            }
        });

        // Read and stream events from stdout
        let child = child_arc.lock().await.take().ok_or_else(|| ExecutionError::BackendError {
            message: "Child process already consumed".to_string(),
            code: "INTERNAL_ERROR".to_string(),
        })?;

        let events = match executor.read_events(child, request_id).await {
            Ok(events) => events,
            Err(e) => {
                let _ = cancel_task.abort();
                return Err(e);
            }
        };

        // Send all events through the sink
        for event in events {
            // A broadcast send only fails when there are no active receivers.
            if sink.send(event).is_err() {
                break;
            }
        }

        let _ = cancel_task.abort();

        Ok(ExecutionResult {
            request_id,
            usage: harness_protocol::usage::ModelUsage::default(),
            cost: harness_protocol::usage::Cost {
                amount_usd: None,
                source: None,
            },
            finish_reason: "end_turn".to_string(),
        })
    }
}

/// Registry factory for the `claude-code` integration.
pub struct ClaudeCodeFactory;

#[async_trait]
impl IntegrationFactory for ClaudeCodeFactory {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: BackendId::new(),
            name: "Claude Code".to_string(),
            description: "Claude Code CLI invoked as a subprocess".to_string(),
            capabilities: BackendCapabilities {
                streaming: true,
                tool_calls: true,
                host_managed_tools: false,
                ..Default::default()
            },
        }
    }

    async fn create(
        &self,
        config: serde_json::Value,
    ) -> Result<Arc<dyn ExecutionBackend>, Box<dyn std::error::Error + Send + Sync>> {
        let config: ClaudeCodeConfig = serde_json::from_value(config)?;
        Ok(Arc::new(ClaudeCodeBackend::new(config)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_descriptor_is_correct() {
        let backend = ClaudeCodeBackend::new(ClaudeCodeConfig::default());
        let descriptor = backend.descriptor();
        assert_eq!(descriptor.name, "Claude Code");
        assert!(descriptor.capabilities.streaming);
        assert!(descriptor.capabilities.tool_calls);
        assert!(!descriptor.capabilities.host_managed_tools);
    }

    #[tokio::test]
    async fn factory_constructs_backend_from_json() {
        let backend = ClaudeCodeFactory
            .create(serde_json::json!({
                "binary_path": "claude",
                "permission_mode": "autonomous"
            }))
            .await
            .expect("valid Claude Code configuration");
        assert!(backend.capabilities().streaming);
        assert!(backend.capabilities().tool_calls);
    }
}
