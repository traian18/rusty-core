use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use harness_protocol::ids::ToolCallId;

use serde::{Deserialize, Serialize};

use super::compiler::{compile, CompiledProfile, ProfileValidationError};
use super::default::default_profile;
use super::definition::{BehaviorProfile, ProfileRef};
use super::registry::{ProfileRegistry, ProfileRegistryError};

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
    /// Firings per rule id, for `max_fires`.
    #[serde(default)]
    pub fired: BTreeMap<String, u32>,
    /// Tools that actually executed, in order, with the turn they ran in.
    #[serde(default)]
    pub executed: Vec<ExecutedCall>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStreak {
    /// Tool name and canonical arguments.
    pub signature: String,
    pub count: u32,
}

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

    /// Account for a new model request (not a re-issue after resume) and
    /// decide its shape against the profile's limits.
    pub fn begin_turn(&mut self) -> TurnPlan {
        if self.run.final_turn {
            return TurnPlan::Exceeded;
        }
        self.run.turns += 1;
        let limits = &self.profile.profile.limits;
        let turns_exhausted = limits.max_turns.is_some_and(|max| self.run.turns >= max);
        let tools_exhausted = limits
            .max_tool_calls
            .is_some_and(|max| self.run.tool_calls >= max);
        if turns_exhausted || tools_exhausted {
            self.run.final_turn = true;
            TurnPlan::Final {
                prompt: limits.final_turn_prompt.clone(),
            }
        } else {
            TurnPlan::Normal
        }
    }

    /// Count a tool call, or refuse it when the run is out of tool calls or
    /// on its final turn.
    pub fn admit_tool_call(&mut self) -> bool {
        let over_budget = self
            .profile
            .profile
            .limits
            .max_tool_calls
            .is_some_and(|max| self.run.tool_calls >= max);
        if self.run.final_turn || over_budget {
            return false;
        }
        self.run.tool_calls += 1;
        true
    }

    /// Durable form: the full profile document (so a snapshot is
    /// self-contained), its hash, and the counters.
    pub fn to_stored(&self) -> serde_json::Value {
        serde_json::to_value(StoredBehavior {
            profile: self.profile.profile.clone(),
            content_hash: self.profile.content_hash.clone(),
            run: self.run.clone(),
            library: self.library.documents(),
            entered_from: self.entered_from.clone(),
            commands_trusted: self.commands_trusted,
        })
        .expect("behavior state always serializes")
    }

    /// Rebuild from [`to_stored`](Self::to_stored). The profile is
    /// recompiled and its hash must match, so a tampered or corrupted
    /// snapshot fails closed instead of running under different rules.
    /// `None` (a snapshot from before the behavior layer) restores the
    /// built-in default.
    pub fn from_stored(value: Option<&serde_json::Value>) -> Result<Self, BehaviorRestoreError> {
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(Self::default());
        };
        let stored: StoredBehavior = serde_json::from_value(value.clone())
            .map_err(|error| BehaviorRestoreError::Malformed(error.to_string()))?;
        let compiled = compile(stored.profile).map_err(BehaviorRestoreError::Invalid)?;
        if compiled.content_hash != stored.content_hash {
            return Err(BehaviorRestoreError::HashMismatch(compiled.reference()));
        }
        let library =
            ProfileRegistry::from_library(stored.library).map_err(BehaviorRestoreError::Library)?;
        Ok(Self {
            profile: Arc::new(compiled),
            library: Arc::new(library),
            entered_from: stored.entered_from,
            commands_trusted: stored.commands_trusted,
            run: stored.run,
        })
    }
}

fn empty_library() -> Arc<ProfileRegistry> {
    static EMPTY: OnceLock<Arc<ProfileRegistry>> = OnceLock::new();
    EMPTY
        .get_or_init(|| Arc::new(ProfileRegistry::new()))
        .clone()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredBehavior {
    profile: BehaviorProfile,
    content_hash: String,
    run: RunCounters,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    library: Vec<BehaviorProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entered_from: Option<EnteredFrom>,
    #[serde(default)]
    commands_trusted: bool,
}

/// The profile active before a switch, captured when it happened (the new
/// library may not contain it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredFrom {
    pub profile: ProfileRef,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BehaviorRestoreError {
    #[error("stored behavior state is malformed: {0}")]
    Malformed(String),
    #[error("stored behavior profile is no longer valid: {0}")]
    Invalid(ProfileValidationError),
    #[error("stored behavior profile {0} does not match its recorded content hash")]
    HashMismatch(ProfileRef),
    #[error("stored behavior profile library is no longer valid: {0}")]
    Library(ProfileRegistryError),
}
