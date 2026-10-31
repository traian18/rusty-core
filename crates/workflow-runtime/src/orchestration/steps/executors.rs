//! The contracts a host implements to run agent steps and child flows.

use async_trait::async_trait;
use serde_json::Value;

use crate::orchestration::steps::context::{StepContext, SubflowRequest};
use crate::orchestration::steps::request::{
    AgentExecutionError, AgentStepOutput, AgentStepRequest,
};

/// Runs the child of a `subflow` node to its end and returns its output. The
/// engine supplies it, since it knows the other flows and the session the
/// child shares tools and permissions with.
#[async_trait]
pub trait SubflowExecutor: Send + Sync {
    async fn execute(
        &self,
        request: SubflowRequest,
        context: &mut StepContext,
    ) -> Result<Value, AgentExecutionError>;
}

#[async_trait]
pub trait AgentStepExecutor: Send + Sync {
    async fn execute(
        &self,
        request: AgentStepRequest,
        context: StepContext,
    ) -> Result<AgentStepOutput, AgentExecutionError>;
}
