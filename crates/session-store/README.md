# rusty-session-store

Durable sessions. Every durable event is written as it happens, with periodic snapshots, so a
session survives a restart and a reconnecting client can resume from a sequence number without
gaps or duplicates. Streaming deltas are deliberately never stored.

## Why it exists

An agent run can take hours and the process around it can stop at any time: a crash, an update,
the user closing the laptop. Restoring a session needs a record that is written before anything
depends on it, ordered without gaps, and checked when it is read back. That is a different job
from running the agent, with its own failure modes (torn writes, duplicate sequences, snapshots
from older versions), so it lives in its own crate. The runtime depends on the `SessionStore`
trait and never on a particular database.

## How it works

A session is a sequence of **durable events** plus at most one **snapshot** (a checkpoint of every
agent's state at some sequence). To restore, a store returns the latest snapshot and the events
after it, and `replay_snapshot` applies those events to the snapshot.

1. **Commit.** `SessionCommitter::commit` gives an event its final session sequence
   (`SessionSequencer`, gap-free, resuming from `store.current_sequence` after a restart), appends
   it if it is durable, and asks for a checkpoint at the end of a run or every
   `DEFAULT_SNAPSHOT_EVERY` (512) durable events.
2. **Store.** `append` and `save_snapshot` persist. Two stores exist: `JsonlSessionStore` (one
   append-only file per session) and `SqliteSessionStore` (WAL-mode SQLite behind a single writer
   task).
3. **Restore.** `load_session` returns snapshot plus trailing events; `check_snapshot_version`
   and `migrate_snapshot` bring old snapshots forward and reject newer ones;
   `validate_trailing_replay` checks the events are contiguous and their transitions legal;
   `replay_snapshot` applies them; `assess_restore` checks the host can still provide what the
   snapshot refers to (workspace, integration, credentials).

## Key types and concepts

- **`SessionStore`**: the storage contract: `append`, `save_snapshot`, `load_session`,
  `events_since`, `current_sequence`, `raw_records`, `list_sessions`, `prune_events_before`.
- **`DurableSessionEvent`**: an event envelope with the session sequence it was committed under.
- **`DurableSessionSnapshot`**: every agent's `StoredAgentState` at a sequence, the schema version
  that wrote it, and `DurableSessionMetadata`, the references (workspace identity, integrations,
  credential profiles) a restore must resolve.
- **`StoredSession`**: what `load_session` returns: the snapshot plus only the events after it.
- **`SessionCommitter` / `SessionSequencer`**: the one boundary through which events are numbered
  and stored.
- **`DurabilityPolicy`**: `Strict` fails the commit when the store fails; `BestEffort` publishes
  the event marked `degraded` so a storage outage does not stop a run.
- **`MemoryStore` / `FaultInjectingStore`** (feature `testing`): an in-memory store and a
  decorator that fails scripted calls, for crash and restart tests.

## A worked example

```rust
use rusty_session_store::{
    replay_snapshot, validate_trailing_replay, GapPolicy, JsonlSessionStore, SessionCommitter,
    SessionStore,
};
use std::sync::Arc;

let store = Arc::new(JsonlSessionStore::new("/var/lib/rusty/sessions"));

// Write path: every event of a live session passes through the committer.
let committer = SessionCommitter::new(store.clone(), session_id);
let committed = committer.commit(envelope).await?;      // Some(CommittedEvent) once sequenced

// Restart: read the session back and rebuild its state.
let stored = store.load_session(session_id).await?;      // snapshot + trailing events
let events = validate_trailing_replay(&stored, GapPolicy::default())?;
let snapshot = replay_snapshot(
    stored.snapshot.expect("a checkpoint exists"),
    &events,
)?;
```

If the process died in the middle of an append, the JSONL file ends in a half-written line with no
newline. `load_session` drops that last line with a warning and returns everything before it. A bad
line anywhere else is corruption and fails with `StoreError::InvalidState`.

## Public API

- `SessionStore`, `StoreError`, `StoredSession`, `SessionSummary` and the durable payload types.
- `JsonlSessionStore`, `SqliteSessionStore` (feature `sqlite`, on by default).
- `SessionCommitter`, `SessionSequencer`, `DurabilityPolicy`, `CommittedEvent`, `CommitError`,
  `CheckpointReason`, `CheckpointRequester`, `RecordingSink`.
- `validate_trailing_replay`, `ReplayValidator`, `GapPolicy`, `ReplayError`, `replay_snapshot`.
- `check_snapshot_version`, `migrate_snapshot`, `SCHEMA_VERSION`, `SnapshotVersionError`.
- `assess_restore`, `HostDependencyResolver`, `RestorePolicy`, `RestoreReport`, `RestoreError`.
- `plan_compaction`, `prune_plan`, `mark_compacted`, `RetentionPolicy`.
- `diagnose_session`, `diagnose_store`, `repair_jsonl`.
- `MemoryStore`, `FaultInjectingStore` (feature `testing`).

## Errors

- `StoreError`: `NotFound` (no such session), `InvalidState` (corrupt data, a duplicate sequence,
  an attempt to persist an ephemeral event), `Backend` (the database or task failed), `Io`,
  `Serialization`.
- `CommitError`: wraps `StoreError`, plus `DuplicateSequence`, `Closed` and `InvalidEvent`.
- `ReplayError`: `Gap`, `OutOfOrder`, `DuplicateEventId`, `CorruptPayload`, `InvalidTransition`,
  `FutureSnapshotVersion` and `AncientSnapshotVersion`, each saying where the log stopped making
  sense. Restore stops there; it never guesses.
- `SnapshotVersionError::FutureVersion`: the snapshot comes from a newer build.

## Design decisions

- **Streaming deltas are never persisted.** Only events `is_durable` says matter for restore
  reach the store; appending an ephemeral one is an error. History stays small and a restore
  never replays token streams.
- **One writer per store.** SQLite serializes writers anyway, so a single task owns the write
  connection and every mutation is one transaction; reads use a pool and run concurrently.
  The JSONL store runs one writer task per session file, which keeps each file's appends ordered.
- **Replay is strict.** Unknown agents, impossible transitions and sequence gaps are errors, not
  repairs. The projection lists every event kind explicitly so a new one forces a decision.
- **Pruning is opt-in and checked by the caller.** Compaction takes a snapshot first; deleting
  events below it is a separate, explicit step the store does not verify (`prune_plan` does).
- **Snapshots reference, never contain, secrets.** They record which workspace, integration and
  credential profile were in use; the host resolves those again at restore.

## Testing

Unit tests sit beside each module. `tests/conformance.rs` runs one contract suite against every
store, `tests/rc300.rs` covers commit, restore and dependency resolution end to end, and
`tests/replay.rs` replays committed fixtures. Run `cargo test -p rusty-session-store`.

## In the workspace

- **Depends on:** `rusty-protocol`
- **Used by:** `harness`, `harnessd`, `rusty-engine`, `rusty-harness-sdk`, `rusty-runtime`, `rusty-workflow-runtime`

## Must never

- Run an agent, call a model, or run a tool; it only stores and checks records.
- Persist streaming deltas or secrets.
- Depend on any workspace crate except `rusty-protocol`.
