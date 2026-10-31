//! Step executors: the agent-step contract plus the deterministic input,
//! verify, and output nodes. Executors return an outcome; they never choose
//! the next node — that belongs to the reducer.
//!
//! The verify node lives in [`verify`], the criteria it judges in
//! [`criteria`], and the approval node in [`approval`].

//!
//! The contract types live in `request`, `context` and `executors`; `inputs` resolves bindings and
//! runs the input node, `artifacts` checks claimed artifacts.

mod approval;
mod artifacts;
mod context;
mod criteria;
mod executors;
mod inputs;
mod request;
mod verify;

pub use artifacts::{ArtifactResolver, ReferenceArtifactResolver, WorkspaceArtifactResolver};
pub use context::{InputAnswer, PermissionResolution, StepContext, StepSignal, SubflowRequest};
pub use executors::{AgentStepExecutor, SubflowExecutor};
pub use request::{AgentExecutionError, AgentStepOutput, AgentStepRequest};

pub(crate) use approval::execute_approval;
pub use approval::{is_user_change, CHANGES_APPROVED, CHANGES_REQUESTED};
pub(crate) use criteria::match_results;
#[cfg(test)]
pub(crate) use criteria::requirement_coverage;
pub(crate) use inputs::{execute_input, resolve_binding, resolve_inputs, Validation};
pub(crate) use verify::execute_verify;
