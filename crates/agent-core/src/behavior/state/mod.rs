use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use rusty_protocol::ids::ToolCallId;

use serde::{Deserialize, Serialize};

use crate::behavior::compiler::CompiledProfile;
use crate::behavior::default::default_profile;
use crate::behavior::definition::ProfileRef;
use crate::behavior::registry::ProfileRegistry;

/// Switches allowed within one run before the run fails; guards against
/// rules that bounce between profiles.
pub const MAX_SWITCHES_PER_RUN: u32 = 16;

/// Per-agent behavior: the active profile, the profiles it may switch to,
/// and this run's counters.
#[derive(Debug, Clone)]
pub struct BehaviorState {
    pub profile: Arc<CompiledProfile>,
    /// Profiles reachable through `switch_profile`, resolved by the host
    /// when the profile was installed. The core never needs a registry.
    pub library: Arc<ProfileRegistry>,
    /// Set when this profile became active by a switch, until the next
    /// model request announces it and fires `ProfileEntered` rules.
    pub entered_from: Option<EnteredFrom>,
    /// Whether the host allowed this agent's profiles to run shell
    /// commands (`command` evaluators). Decided by the host at install time.
    pub commands_trusted: bool,
    pub run: RunCounters,
}

/// Counters and rule bookkeeping for the current run. Reset when a run
/// starts. Durable, so a restored agent keeps `max_fires` and loop state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCounters {
    /// Model requests made in this run.
    pub turns: u32,
    /// Tool calls admitted in this run.
    pub tool_calls: u32,
    /// The last request was sent as the final turn (no tools offered).
    pub final_turn: bool,
    /// Extra requests granted after the final turn because the model still
    /// asked for tools. Bounded by [`FINAL_TURN_GRACE`].
    #[serde(default)]
    pub final_grace_used: u32,
    /// Firings per rule id, for `max_fires`.
    #[serde(default)]
    pub fired: BTreeMap<String, u32>,
    /// Tools that actually executed, in order, with the turn they ran in.
    #[serde(default)]
    pub executed: Vec<ExecutedCall>,
    /// Names of the tools the latest request that offered any tools offered,
    /// sorted. A forced final turn offers none and leaves this as it was, so
    /// `tool_offered` still answers for the tools the agent really had.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offered_tools: Vec<String>,
    /// The latest run of identical tool requests, for loop detection.
    #[serde(default)]
    pub streak: Option<CallStreak>,
    /// Wrapped context for the next outgoing request only.
    #[serde(default)]
    pub pending_request: Vec<String>,
    /// Context carried by the latest request, repeated if it is re-issued.
    #[serde(default)]
    pub last_request: Vec<String>,
    /// Wrapped context to append to a pending tool call's result.
    #[serde(default)]
    pub pending_results: Vec<(ToolCallId, String)>,
    /// Completion-gate evaluations started in this run; identifies the
    /// current one so stale verdicts are ignored.
    #[serde(default)]
    pub gate_attempts: u32,
    /// Times the gate sent a rejection back and the run continued.
    #[serde(default)]
    pub gate_continuations: u32,
    /// The status line of the latest proposed final answer (see
    /// `Condition::FinalStatus`); empty when it had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_status: Option<String>,
    /// Profile switches in this run. Carried across switches (which reset
    /// the other counters) and bounded by [`MAX_SWITCHES_PER_RUN`].
    #[serde(default)]
    pub switches: u32,
    /// The previous profile's id while `ProfileEntered` rules evaluate.
    #[serde(skip)]
    pub entered_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutedCall {
    pub turn: u32,
    pub tool: String,
    /// The call's result was an error. Absent from older saved state, which
    /// counts as a success; omitted when false so that state is unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub failed: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStreak {
    /// Tool name and canonical arguments.
    pub signature: String,
    pub count: u32,
}

/// How many times the model may call tools on the final turn and still be
/// asked again for a text answer before the run fails.
pub const FINAL_TURN_GRACE: u32 = 2;

/// What the next model request should look like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPlan {
    Normal,
    /// Offer no tools; append the prompt, if any, to the request.
    Final {
        prompt: Option<String>,
    },
    /// The model kept calling tools after its final turn.
    Exceeded,
}

impl Default for BehaviorState {
    fn default() -> Self {
        Self::new(default_profile())
    }
}

impl BehaviorState {
    pub fn new(profile: Arc<CompiledProfile>) -> Self {
        Self {
            profile,
            library: empty_library(),
            entered_from: None,
            commands_trusted: false,
            run: RunCounters::default(),
        }
    }

    pub fn with_commands_trusted(mut self, trusted: bool) -> Self {
        self.commands_trusted = trusted;
        self
    }

    pub fn with_library(mut self, library: Arc<ProfileRegistry>) -> Self {
        self.library = library;
        self
    }

    /// The state after switching to `profile` within the same library:
    /// counters start fresh (limits and `max_fires` are per profile), the
    /// run's switch count carries over, and the switch is pending
    /// announcement.
    pub fn switched_to(
        &self,
        profile: Arc<CompiledProfile>,
        library: Arc<ProfileRegistry>,
    ) -> Self {
        let mut next = Self::new(profile)
            .with_library(library)
            .with_commands_trusted(self.commands_trusted);
        next.run.switches = self.run.switches + 1;
        next.entered_from = Some(EnteredFrom {
            profile: self.reference(),
            name: self.profile.profile.name.clone(),
        });
        next
    }

    /// Resolve a switch target within this agent's library.
    pub fn resolve_switch(&self, target: &ProfileRef) -> Option<Arc<CompiledProfile>> {
        self.library.resolve(target).ok()
    }

    pub fn reference(&self) -> ProfileRef {
        self.profile.reference()
    }

    pub fn reset_run(&mut self) {
        self.run = RunCounters::default();
    }
}

fn empty_library() -> Arc<ProfileRegistry> {
    static EMPTY: OnceLock<Arc<ProfileRegistry>> = OnceLock::new();
    EMPTY
        .get_or_init(|| Arc::new(ProfileRegistry::new()))
        .clone()
}

/// The profile active before a switch, captured when it happened (the new
/// library may not contain it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredFrom {
    pub profile: ProfileRef,
    pub name: String,
}

mod stored;
mod turns;

pub use stored::BehaviorRestoreError;
