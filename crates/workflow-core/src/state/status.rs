//! What a run and each of its steps is doing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationStatus {
    /// Run identity exists, no input admitted.
    Created,
    /// A step may be scheduled.
    Ready,
    /// A step is executing.
    Running,
    /// The running step's delegated agent awaits a tool permission decision.
    WaitingForPermission,
    /// The running step asked the user a question (an approval, or what to
    /// do after a failure) and waits for the answer.
    WaitingForInput,
    /// No new work is admitted. A step already running may still finish.
    Paused,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl OrchestrationStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Ready,
    Running,
    WaitingForPermission,
    WaitingForInput,
    RetryScheduled,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl StepStatus {
    /// A step with an attempt currently in flight.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Running | Self::WaitingForPermission | Self::WaitingForInput
        )
    }
}
