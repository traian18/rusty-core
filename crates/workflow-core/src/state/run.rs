//! The state of one workflow run and of each of its steps.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compiler::CompiledOrchestration;
use crate::definition::{OrchestrationDefinitionId, OrchestrationNodeId, OrchestrationRunId};

use super::{
    InputRequest, OrchestrationError, OrchestrationStatus, StepAttempt, StepStatus, UsageSummary,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationRunState {
    pub run_id: OrchestrationRunId,
    pub definition_id: OrchestrationDefinitionId,
    pub definition_revision: u64,
    /// Content hash of the definition this run was started against.
    pub definition_hash: String,
    pub status: OrchestrationStatus,
    /// Set by `Pause`, cleared by `Resume`. Kept separately from `status`
    /// because a pause requested mid-step takes effect when the step settles.
    #[serde(default)]
    pub paused: bool,
    pub input: Option<Value>,
    pub steps: BTreeMap<OrchestrationNodeId, StepRun>,
    pub final_output: Option<Value>,
    pub error: Option<OrchestrationError>,
    /// The terminal failure boundary; completed upstream steps remain reusable.
    #[serde(default)]
    pub failed_step: Option<OrchestrationNodeId>,
    pub total_attempts: u32,
    #[serde(default)]
    pub usage: UsageSummary,
    /// How the run was started; kept with the state so a resumed run behaves the same.
    #[serde(default)]
    pub options: RunOptions,
}

/// Choices made when a run is started rather than in its definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOptions {
    /// Approval steps that allow it pass without asking, recorded as
    /// answered by `auto`. Questions about failures are still asked.
    #[serde(default)]
    pub auto_approve: bool,
}

impl OrchestrationRunState {
    pub fn new(run_id: OrchestrationRunId, compiled: &CompiledOrchestration) -> Self {
        let steps = compiled
            .nodes
            .keys()
            .cloned()
            .map(|id| (id, StepRun::default()))
            .collect();
        Self {
            run_id,
            definition_id: compiled.definition_id.clone(),
            definition_revision: compiled.revision,
            definition_hash: compiled.content_hash.clone(),
            status: OrchestrationStatus::Created,
            paused: false,
            input: None,
            steps,
            final_output: None,
            error: None,
            failed_step: None,
            total_attempts: 0,
            usage: UsageSummary::default(),
            options: RunOptions::default(),
        }
    }

    /// The step whose attempt is currently in flight, if any.
    pub fn active_step(&self) -> Option<(&OrchestrationNodeId, &StepRun)> {
        self.steps.iter().find(|(_, step)| step.status.is_active())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRun {
    /// Durable task progress. Kept on a retry of this step, cleared when an upstream step is invalidated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Value>,
    pub status: StepStatus,
    /// Every attempt, in order. Never overwritten on retry.
    pub attempts: Vec<StepAttempt>,
    pub output: Option<Value>,
    pub error: Option<OrchestrationError>,
    /// Errors that caused this step to be re-run — its own failures and
    /// downstream verification failures — provided to the next attempt as data.
    #[serde(default)]
    pub feedback: Vec<OrchestrationError>,
    /// Permissions the active attempt is blocked on (parallel tool calls
    /// can request several at once).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_permissions: Vec<String>,
    /// The question the active attempt is waiting on the user to answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_input: Option<InputRequest>,
    /// Attempts made before the user last asked for changes to this step.
    /// Its own retry budget, and a verifier's, count from here, so asking
    /// for changes never uses up the retries a failure would get.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_base: u32,
}

pub(super) fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl Default for StepRun {
    fn default() -> Self {
        Self {
            checkpoint: None,
            status: StepStatus::Pending,
            attempts: Vec::new(),
            output: None,
            error: None,
            feedback: Vec::new(),
            pending_permissions: Vec::new(),
            pending_input: None,
            retry_base: 0,
        }
    }
}
