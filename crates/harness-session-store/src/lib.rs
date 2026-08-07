#![warn(clippy::all)]

//! Persistence interfaces and session/event restoration contracts.
//!
//! RC-300 (truthful persistence and recovery) is implemented across this
//! crate's modules:
//!
//! - [`store`] — the [`SessionStore`] contract and durable payloads.
//! - [`commit`] — the authoritative event commit boundary (RC-301).
//! - [`version`] — snapshot versioning and migration (RC-305).
//! - [`replay`] — side-effect-free trailing validation (RC-303).
//! - [`projection`] — side-effect-free trailing state reduction (RC-303).
//! - [`resolver`] — strict host dependency resolution (RC-304).
//! - [`retention`] and [`diagnostics`] — lifecycle tooling (RC-305).
//! - [`testing`] — [`MemoryStore`] and
//!   [`FaultInjectingStore`], used by this
//!   crate's and embedding crates' M2 crash/restart and durability-policy
//!   fixtures.

pub mod commit;
pub mod diagnostics;
pub mod jsonl;
pub mod projection;
pub mod replay;
pub mod resolver;
pub mod retention;
pub mod sqlite;
pub mod store;
pub mod testing;
pub mod version;

pub use commit::{
    CheckpointReason, CheckpointRequester, CommitError, CommittedEvent, DurabilityPolicy,
    RecordingSink, SessionCommitter, SessionSequencer, DEFAULT_SNAPSHOT_EVERY,
};
pub use diagnostics::{
    diagnose_session, diagnose_store, repair_jsonl, RepairReport, SessionDiagnostics,
    StoreDiagnostics,
};
pub use jsonl::JsonlSessionStore;
pub use projection::replay_snapshot;
pub use replay::{validate_trailing_replay, GapPolicy, ReplayError, ReplayValidator};
pub use resolver::{
    assess_restore, DependencyKind, DependencyResolution, HostDependencyResolver,
    MissingDependency, PermissiveResolver, RestoreError, RestorePolicy, RestoreReport,
};
pub use retention::{mark_compacted, plan_compaction, prune_plan, CompactionPlan, RetentionPolicy};
pub use sqlite::SqliteSessionStore;
pub use store::{
    is_durable, summarize_session, DurableSessionEvent, DurableSessionMetadata,
    DurableSessionSnapshot, RawRecord, SessionStore, SessionSummary, StoreError, StoredAgentState,
    StoredPendingToolCall, StoredSession,
};
pub use testing::{FaultInjectingStore, MemoryStore};
pub use version::{
    check_snapshot_version, migrate_snapshot, SnapshotVersionError, MIN_SUPPORTED_SNAPSHOT_VERSION,
    SCHEMA_VERSION,
};
