//! Provider-neutral `ModelClient` adapter for the runtime `ExecutionBackend`.

//!
//! The request flow is split by responsibility: `capabilities` rejects what
//! the provider cannot serve, `circuit` fails fast after repeated transient
//! failures, `execution` runs the attempt/retry loop, `retry` decides whether
//! and how to retry, `stream` runs one provider call, and `convert` maps
//! model results and errors onto the execution protocol.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_model::client::ModelClient;
use harness_protocol::backend::{
    BackendCapabilities, BackendDescriptor, ExecutionError, ExecutionEvent, ExecutionRequest,
    ExecutionResult,
};
use harness_protocol::ids::BackendId;
use harness_runtime::traits::ExecutionBackend;

mod capabilities;
mod circuit;
mod convert;
mod execution;
mod retry;
mod stream;

use circuit::CircuitBreaker;

/// Bounded recovery settings for one provider backend instance.
///
/// Serializable so it can be embedded directly in a provider config struct
/// (e.g. `AnthropicConfig::recovery`) and set from the RPC-supplied
/// `integration_config` JSON, rather than only being constructible in code.
/// Fields round-trip through JSON as whole seconds, matching the convention
/// every provider config in this workspace already uses for its own
/// `request_timeout_secs` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryPolicy {
    /// Total calls allowed for a request, including its initial attempt.
    pub max_attempts: usize,
    /// Maximum wait for the first stream event of an attempt. Longer than
    /// `idle_timeout` because the provider may queue the request or think
    /// silently before emitting anything.
    #[serde(
        rename = "first_event_timeout_secs",
        serialize_with = "crate::duration_serde::serialize",
        deserialize_with = "crate::duration_serde::deserialize"
    )]
    pub first_event_timeout: Duration,
    /// Maximum inactivity once the stream has started; reset on every model
    /// stream event. Active streams have no wall-clock deadline.
    #[serde(
        rename = "idle_timeout_secs",
        alias = "total_deadline_secs",
        serialize_with = "crate::duration_serde::serialize",
        deserialize_with = "crate::duration_serde::deserialize"
    )]
    pub idle_timeout: Duration,
    /// Consecutive transient request failures that open the circuit.
    pub circuit_failure_threshold: u32,
    /// How long an open circuit fails fast before one probe is allowed.
    #[serde(
        rename = "circuit_open_duration_secs",
        serialize_with = "crate::duration_serde::serialize",
        deserialize_with = "crate::duration_serde::deserialize"
    )]
    pub circuit_open_duration: Duration,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            first_event_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(120),
            circuit_failure_threshold: 3,
            circuit_open_duration: Duration::from_secs(30),
        }
    }
}

pub struct GenericModelBackend {
    model_client: Arc<dyn ModelClient>,
    descriptor: BackendDescriptor,
    capabilities: BackendCapabilities,
    recovery: RecoveryPolicy,
    circuit: CircuitBreaker,
}

impl GenericModelBackend {
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        Self::new_with_recovery(model_client, RecoveryPolicy::default())
    }

    /// Construct a backend with an explicit, per-provider recovery policy.
    pub fn new_with_recovery(model_client: Arc<dyn ModelClient>, recovery: RecoveryPolicy) -> Self {
        let model = model_client.capabilities();
        let capabilities = BackendCapabilities {
            streaming: model.streaming,
            reasoning_stream: model.reasoning,
            tool_calls: model.tool_calls,
            parallel_tool_calls: model.parallel_tool_calls,
            images: model.images,
            structured_output: model.structured_output,
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
            circuit: CircuitBreaker::new(&recovery),
            recovery,
        }
    }

    /// The recovery policy this backend was constructed with. Exposed mainly
    /// for tests that verify a provider config's `recovery` field was
    /// actually threaded through to the backend, rather than silently
    /// falling back to the default.
    pub fn recovery_policy(&self) -> &RecoveryPolicy {
        &self.recovery
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
        request: ExecutionRequest,
        sink: broadcast::Sender<ExecutionEvent>,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutionError> {
        self.execute_request(request, sink, cancel).await
    }
}

#[cfg(test)]
mod tests;
