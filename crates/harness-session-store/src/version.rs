//! Snapshot/event schema versioning, forward rejection, and backward migration
//! (RC-305).
//!
//! Every [`DurableSessionSnapshot`]
//! written by this build carries [`SCHEMA_VERSION`]. Older checkpoints (the
//! pre-RC-300 shape, which has no `schema_version` field) deserialize with
//! version `0` and are upgraded in place by [`migrate_snapshot`] before a
//! restore is attempted.
//!
//! # Version policy
//!
//! - **Forward rejection** — a snapshot with `schema_version > SCHEMA_VERSION`
//!   was produced by a newer build and is rejected with
//!   [`SnapshotVersionError::FutureVersion`] instead of being mis-read.
//! - **Backward migration** — a snapshot with a version below the current one
//!   is upgraded through [`migrate_snapshot`], which is a pure, side-effect-free
//!   transformation (it never touches the store or any external system).
//! - Migrations are committed with golden fixtures in
//!   `tests/fixtures/migrate-v0-to-v1.json` and replayed in
//!   `tests/rc300_version.rs`; the same fixture runs against both the JSONL
//!   and SQLite stores via the shared conformance entry points.
//!
//! Durable *events* share the envelope's own payload schema; version drift in
//! event payloads is detected at replay time by
//! [`crate::replay::ReplayError::CorruptPayload`].

use crate::store::DurableSessionSnapshot;

/// The snapshot schema version written by this build.
///
/// Version `0` is the pre-RC-300 checkpoint shape (no `schema_version`
/// field, no dependency metadata block). Version `1` adds
/// [`DurableSessionSnapshot::schema_version`] and
/// [`DurableSessionSnapshot::metadata`].
pub const SCHEMA_VERSION: u64 = 1;

/// The oldest snapshot version this build can still migrate.
pub const MIN_SUPPORTED_SNAPSHOT_VERSION: u64 = 0;

/// Typed snapshot-version failures surfaced before restore.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotVersionError {
    /// The checkpoint was produced by a newer build and cannot be read.
    #[error("snapshot schema version {found} is newer than this build supports ({supported})")]
    FutureVersion {
        /// The version found in the checkpoint.
        found: u64,
        /// The newest version this build supports.
        supported: u64,
    },
    /// The checkpoint predates the oldest migratable version.
    #[error(
        "snapshot schema version {found} is older than the oldest supported version ({supported})"
    )]
    AncientVersion {
        /// The version found in the checkpoint.
        found: u64,
        /// The oldest version this build can migrate.
        supported: u64,
    },
}

/// Validates `version` against this build's supported range.
///
/// Rejects versions newer than [`SCHEMA_VERSION`] (forward rejection) and
/// versions older than [`MIN_SUPPORTED_SNAPSHOT_VERSION`]. Versions inside
/// the range are accepted; anything below the current version should be
/// passed through [`migrate_snapshot`] before use.
#[allow(clippy::absurd_extreme_comparisons)]
pub fn check_snapshot_version(version: u64) -> Result<(), SnapshotVersionError> {
    if version > SCHEMA_VERSION {
        return Err(SnapshotVersionError::FutureVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < MIN_SUPPORTED_SNAPSHOT_VERSION {
        return Err(SnapshotVersionError::AncientVersion {
            found: version,
            supported: MIN_SUPPORTED_SNAPSHOT_VERSION,
        });
    }
    Ok(())
}

/// Migrates a snapshot forward to the current [`SCHEMA_VERSION`].
///
/// This is a pure transformation with no side effects:
///
/// - **v0 → v1** — stamps `schema_version = 1` and fills the
///   [`DurableSessionMetadata`](crate::store::DurableSessionMetadata) block
///   with its defaults (a v0 checkpoint predates dependency recording, so the
///   block stays empty and the restore-time resolver reports every reference
///   as unresolvable rather than inventing one).
///
/// Snapshots already at the current version are returned unchanged. A
/// snapshot from a newer build fails with
/// [`SnapshotVersionError::FutureVersion`].
pub fn migrate_snapshot(
    snapshot: DurableSessionSnapshot,
) -> Result<DurableSessionSnapshot, SnapshotVersionError> {
    check_snapshot_version(snapshot.schema_version)?;
    if snapshot.schema_version == SCHEMA_VERSION {
        return Ok(snapshot);
    }
    // v0 → v1: the serde defaults already produced an empty metadata block
    // and schema_version = 0; stamp the current version.
    Ok(DurableSessionSnapshot {
        schema_version: SCHEMA_VERSION,
        ..snapshot
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{DurableSessionMetadata, DurableSessionSnapshot};
    use harness_protocol::ids::{AgentId, SessionId, Timestamp};

    fn snapshot(version: u64) -> DurableSessionSnapshot {
        DurableSessionSnapshot {
            session_id: SessionId::new(),
            root_agent_id: AgentId::new(),
            agents: Vec::new(),
            session_sequence: 0,
            timestamp: Timestamp::now(),
            schema_version: version,
            metadata: DurableSessionMetadata::default(),
        }
    }

    #[test]
    fn current_version_is_accepted() {
        check_snapshot_version(SCHEMA_VERSION).expect("current version is supported");
        assert!(
            migrate_snapshot(snapshot(SCHEMA_VERSION))
                .expect("current snapshot migrates to itself")
                .schema_version
                == SCHEMA_VERSION
        );
    }

    #[test]
    fn future_version_is_rejected() {
        let error = check_snapshot_version(SCHEMA_VERSION + 1).expect_err("future version");
        assert!(matches!(error, SnapshotVersionError::FutureVersion { .. }));
        assert!(migrate_snapshot(snapshot(SCHEMA_VERSION + 1)).is_err());
    }

    #[test]
    fn v0_snapshot_migrates_to_v1() {
        let migrated = migrate_snapshot(snapshot(0)).expect("v0 migrates to v1");
        assert_eq!(migrated.schema_version, 1);
        assert_eq!(migrated.metadata, DurableSessionMetadata::default());
    }
}
