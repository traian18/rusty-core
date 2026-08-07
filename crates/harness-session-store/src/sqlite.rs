//! WAL-mode SQLite [`SessionStore`] backed by a single-writer actor (Tasks 7.2/7.3).
//!
//! # Architecture
//!
//! SQLite serializes writers regardless of driver, so all mutations are routed
//! through **one background task** that owns the single WAL-mode
//! `rusqlite::Connection`:
//!
//! ```text
//!              append / save_snapshot / prune
//!                        │
//!              mpsc channel (WriteCommand)
//!                        ▼
//!            writer actor ── owns ──> WAL-mode Connection
//!                        │
//!              pooled reads (r2d2_sqlite)  ◄── load_session / current_sequence / raw_records
//! ```
//!
//! - **Writes**: [`SqliteSessionStore::append`] and
//!   [`SqliteSessionStore::save_snapshot`] send a `WriteCommand` over an
//!   `mpsc` channel and await a `oneshot` acknowledgement, so every durable
//!   mutation is executed inside its own transaction by the single writer.
//!   Concurrent appends from many sessions (or many tasks) can never
//!   interleave inside a transaction.
//! - **Reads**: [`SessionStore::load_session`] never touches the writer's
//!   channel. It checks out a connection from a pooled `r2d2_sqlite` pool and
//!   runs the read in `spawn_blocking`. WAL mode allows readers to proceed
//!   concurrently with the writer.
//!
//! # Durability & crash recovery
//!
//! The writer connection runs with `PRAGMA journal_mode = WAL` (confirmed at
//! open time) and `PRAGMA synchronous = NORMAL`. Each append/snapshot is a
//! single ACID transaction, so a crash mid-batch can only lose the *uncommitted*
//! tail — never partially written rows: on the next open, SQLite replays the
//! WAL, rolls back interrupted transactions, and the idempotent migration
//! (`IF NOT EXISTS`, `migrations/0001_init.sql`) re-applies cleanly. The
//! snapshot-versioning columns (`schema_version`, `metadata`) are added to
//! pre-existing databases by `ensure_snapshot_columns`, guarded by
//! `PRAGMA table_info`.
//!
//! # RC-300 additions
//!
//! - [`SessionStore::current_sequence`] resolves the highest committed
//!   sequence with an indexed `MAX(session_sequence)` query, giving the
//!   [`crate::commit::SessionCommitter`] a cheap resume point.
//! - [`SessionStore::raw_records`] streams the unprocessed record stream
//!   (durable events plus the latest snapshot) for
//!   [`crate::diagnostics`].
//! - [`SessionStore::prune_events_before`] implements RC-305 retention as an
//!   explicit maintenance operation: it deletes durable events at or below a
//!   sequence inside the writer actor's own transaction. The caller must have
//!   written a snapshot at or above the prune point first (see
//!   [`crate::retention::prune_plan`]); the store does not verify that
//!   precondition, so the runtime must.
//! - Snapshot rows persist `schema_version` (RC-305) and the
//!   [`DurableSessionMetadata`] block
//!   (RC-304) so versioning and dependency recording survive restarts.
//!
//! # Schema mapping
//!
//! `append` writes exactly one `durable_events` row (the full
//! `AgentEventEnvelope` as JSON in `envelope`); `save_snapshot` upserts the
//! session's single `snapshots` row (replacing any earlier one), carrying the
//! `Vec<StoredAgentState>` as JSON plus the snapshot version/metadata;
//! `load_session` returns the latest snapshot plus the durable events appended
//! after it (`session_sequence > snapshot.session_sequence`, ordered by
//! sequence). The per-agent projection (`agents`) and usage denormalization
//! (`usage_records`) tables defined by the migration are intentionally not
//! populated here — the snapshot's JSON `agents` column is the source of truth
//! for restore, and usage is derivable from the event log (spec §71 "snapshot
//! + event restoration").
//!
//! Events appended without a `session_sequence` (the runtime has not yet
//! assigned one) are assigned the next `MAX(session_sequence) + 1` for their
//! session inside the append transaction, since the schema's
//! `(session_id, session_sequence)` column is `NOT NULL` and UNIQUE-indexed.
//! Duplicate sequences surface as [`StoreError::InvalidState`] at write time.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::{AgentId, SessionId, Timestamp};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::ffi::{
    SQLITE_CONSTRAINT_FOREIGNKEY, SQLITE_CONSTRAINT_PRIMARYKEY, SQLITE_CONSTRAINT_UNIQUE,
};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::{mpsc, oneshot};

use crate::store::{
    is_durable, summarize_session, DurableSessionEvent, DurableSessionMetadata,
    DurableSessionSnapshot, RawRecord, SessionStore, SessionSummary, StoreError, StoredAgentState,
    StoredSession,
};

/// Number of write requests that may be queued to the writer actor before
/// `append`/`save_snapshot` callers await capacity.
const WRITE_CHANNEL_CAPACITY: usize = 1024;

/// Maximum number of pooled read connections.
const READ_POOL_MAX_SIZE: u32 = 8;

/// Busy timeout for the writer and pooled readers, so concurrent WAL
/// checkpoints never fail with `SQLITE_BUSY` under normal operation.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The schema, applied idempotently on every open (all statements use
/// `IF NOT EXISTS`, so re-applying after a crash is safe).
const SCHEMA_SQL: &str = include_str!("../migrations/0001_init.sql");

/// A write request routed to the single-writer actor.
enum WriteCommand {
    /// Append one durable event row.
    Append {
        /// The event to persist.
        event: DurableSessionEvent,
        /// Acknowledges the outcome to the caller.
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// Write/replace the session's latest snapshot row.
    SaveSnapshot {
        /// The snapshot to persist.
        snapshot: DurableSessionSnapshot,
        /// Acknowledges the outcome to the caller.
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// Prune durable events at or below a sequence (RC-305 retention).
    Prune {
        /// The session whose events are pruned.
        session_id: SessionId,
        /// Remove events with `session_sequence <= sequence`.
        sequence: u64,
        /// Acknowledges the number of removed rows.
        reply: oneshot::Sender<Result<u64, StoreError>>,
    },
}

/// [`SessionStore`] backed by a WAL-mode SQLite database.
///
/// See the [module docs](self) for the single-writer actor design, the pooled
/// read path, and the crash-recovery contract.
pub struct SqliteSessionStore {
    /// Channel to the background writer task that owns the WAL-mode connection.
    write_tx: mpsc::Sender<WriteCommand>,
    /// Pooled read connections; `load_session` never touches `write_tx`.
    read_pool: r2d2::Pool<SqliteConnectionManager>,
}

impl SqliteSessionStore {
    /// Opens (creating if needed) the WAL-mode database at `path`.
    ///
    /// Spawns the single-writer actor owning the WAL-mode connection, applies
    /// the schema, and builds the pooled read path. `PRAGMA journal_mode = WAL`
    /// is confirmed at this point — if the database refuses WAL mode the open
    /// fails with [`StoreError::Backend`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }

        // --- the one write connection, owned by the background actor --------
        let mut writer =
            Connection::open(path).map_err(|error| map_sqlite_error("open database", error))?;
        init_writer_connection(&mut writer)?;

        let (write_tx, write_rx) = mpsc::channel(WRITE_CHANNEL_CAPACITY);
        tokio::task::spawn_blocking(move || writer_loop(writer, write_rx));

        // --- pooled read connections (never touch the writer channel) --------
        let manager = SqliteConnectionManager::file(path).with_init(|conn| {
            conn.busy_timeout(BUSY_TIMEOUT)?;
            Ok(())
        });
        let read_pool = r2d2::Pool::builder()
            .max_size(READ_POOL_MAX_SIZE)
            .build(manager)
            .map_err(|error| StoreError::Backend(format!("build read pool: {error}")))?;

        Ok(Self {
            write_tx,
            read_pool,
        })
    }
}

/// Applies the writer-side pragmas, confirms WAL mode, and applies the schema.
fn init_writer_connection(conn: &mut Connection) -> Result<(), StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| map_sqlite_error("set busy timeout", error))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|error| map_sqlite_error("apply writer pragmas", error))?;

    // WAL mode confirmation: executing `PRAGMA journal_mode = WAL` returns a
    // single row with the resulting mode. Anything other than "wal" means the
    // durability contract of this store cannot be met.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(|error| map_sqlite_error("set journal_mode=WAL", error))?;
    if mode != "wal" {
        return Err(StoreError::Backend(format!(
            "expected journal_mode 'wal', got {mode:?}"
        )));
    }

    // Idempotent schema application; safe to re-run after a crash.
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|error| map_sqlite_error("apply schema migration", error))?;

    // RC-305/RC-304: upgrade pre-existing databases with the snapshot
    // versioning and dependency-metadata columns.
    ensure_snapshot_columns(conn)?;

    Ok(())
}

/// Adds the RC-300 `schema_version`/`metadata` columns to a pre-existing
/// `snapshots` table (new databases get them from the schema SQL).
///
/// SQLite lacks `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, so the columns are
/// detected via `PRAGMA table_info` before applying the guarded `ALTER`.
fn ensure_snapshot_columns(conn: &mut Connection) -> Result<(), StoreError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(snapshots)")
        .map_err(|error| map_sqlite_error("inspect snapshots columns", error))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| map_sqlite_error("read snapshots columns", error))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|error| map_sqlite_error("collect snapshots columns", error))?;
    drop(stmt);

    if !columns.iter().any(|column| column == "schema_version") {
        conn.execute_batch(
            "ALTER TABLE snapshots ADD COLUMN schema_version INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(|error| map_sqlite_error("add schema_version column", error))?;
    }
    if !columns.iter().any(|column| column == "metadata") {
        conn.execute_batch("ALTER TABLE snapshots ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';")
            .map_err(|error| map_sqlite_error("add metadata column", error))?;
    }
    Ok(())
}

/// The single-writer actor: owns `conn`, serializes every write request.
///
/// Each command is executed in its own transaction and acknowledged via its
/// `oneshot` reply. Individual command failures are acknowledged to the caller
/// but do not kill the writer — a constraint violation on one append must not
/// take down the store for every other session. When every sender has been
/// dropped (the store handle is gone) the channel closes and the actor
/// checkpoints the WAL before exiting.
fn writer_loop(mut conn: Connection, mut rx: mpsc::Receiver<WriteCommand>) {
    while let Some(command) = rx.blocking_recv() {
        match command {
            WriteCommand::Append { event, reply } => {
                let _ = reply.send(handle_append(&mut conn, event));
            }
            WriteCommand::SaveSnapshot { snapshot, reply } => {
                let _ = reply.send(handle_save_snapshot(&mut conn, snapshot));
            }
            WriteCommand::Prune {
                session_id,
                sequence,
                reply,
            } => {
                let _ = reply.send(handle_prune(&mut conn, session_id, sequence));
            }
        }
    }

    // Graceful shutdown: compact the WAL so a later open doesn't replay stale
    // frames. Best-effort — readers may still be holding the checkpoint lock.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

/// Appends one `durable_events` row inside its own transaction.
///
/// Guarantees concurrent-append safety: the single writer serializes appends,
/// each event is committed atomically, and the UNIQUE index on
/// `(session_id, session_sequence)` rejects duplicate sequences at write time.
/// An event without a runtime-assigned sequence gets `MAX(sequence) + 1` for
/// its session.
fn handle_append(conn: &mut Connection, event: DurableSessionEvent) -> Result<(), StoreError> {
    let tx = conn
        .transaction()
        .map_err(|error| map_sqlite_error("begin append transaction", error))?;
    let session_id = event.envelope.session_id;

    // The events/snapshots tables FK to `sessions`; make sure the row exists.
    // `root_agent_id` is only set on insert — a later `save_snapshot` corrects
    // it to the session's true root agent (see `ensure_session`).
    ensure_session(&tx, &session_id, &event.envelope.agent_id, now_ms(), false)?;

    // Assign a session sequence when the runtime hasn't provided one.
    let sequence = match event.session_sequence.or(event.envelope.session_sequence) {
        Some(sequence) => sequence,
        None => {
            let max: Option<i64> = tx
                .query_row(
                    "SELECT MAX(session_sequence) FROM durable_events WHERE session_id = ?1",
                    params![session_id.to_string()],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .map_err(|error| map_sqlite_error("read max session sequence", error))?;
            max.map_or(1, |m| (m as u64).saturating_add(1))
        }
    };

    let envelope_json = serde_json::to_string(&event.envelope)?;
    let visibility_json = serde_json::to_string(&event.envelope.visibility)?;
    tx.execute(
        "INSERT INTO durable_events
             (event_id, session_id, agent_id, parent_agent_id, run_id,
              agent_sequence, session_sequence, timestamp, visibility, envelope, appended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            event.envelope.event_id.to_string(),
            session_id.to_string(),
            event.envelope.agent_id.to_string(),
            event.envelope.parent_agent_id.map(|id| id.to_string()),
            event.envelope.run_id.map(|id| id.to_string()),
            event.envelope.agent_sequence as i64,
            sequence as i64,
            event.envelope.timestamp.timestamp_millis(),
            visibility_json,
            envelope_json,
            now_ms(),
        ],
    )
    .map_err(|error| map_sqlite_error("append durable event", error))?;

    tx.commit()
        .map_err(|error| map_sqlite_error("commit append transaction", error))?;
    Ok(())
}

/// Writes/replaces the session's latest `snapshots` row inside its own
/// transaction (upsert on `session_id`), correcting the `sessions` row's
/// `root_agent_id` to the snapshot's root agent. Persists the snapshot's
/// schema version (RC-305) and dependency metadata (RC-304).
fn handle_save_snapshot(
    conn: &mut Connection,
    snapshot: DurableSessionSnapshot,
) -> Result<(), StoreError> {
    let tx = conn
        .transaction()
        .map_err(|error| map_sqlite_error("begin snapshot transaction", error))?;

    ensure_session(
        &tx,
        &snapshot.session_id,
        &snapshot.root_agent_id,
        now_ms(),
        true,
    )?;

    let agents_json = serde_json::to_string(&snapshot.agents)?;
    let metadata_json = serde_json::to_string(&snapshot.metadata)?;
    // The timestamp column holds the RFC3339-serialized `Timestamp` (via
    // `to_rfc3339`) so the snapshot timestamp round-trips losslessly without
    // re-deriving it from a truncated millisecond value. SQLite stores the
    // text value in the INTEGER-affinity column without complaint, and
    // `to_rfc3339` output sorts lexicographically like timestamps.
    let timestamp_rfc3339 = snapshot.timestamp.to_rfc3339();
    tx.execute(
        "INSERT INTO snapshots
             (session_id, root_agent_id, agents, session_sequence, timestamp,
              schema_version, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
             root_agent_id    = excluded.root_agent_id,
             agents           = excluded.agents,
             session_sequence = excluded.session_sequence,
             timestamp        = excluded.timestamp,
             schema_version   = excluded.schema_version,
             metadata         = excluded.metadata",
        params![
            snapshot.session_id.to_string(),
            snapshot.root_agent_id.to_string(),
            agents_json,
            snapshot.session_sequence as i64,
            timestamp_rfc3339,
            snapshot.schema_version as i64,
            metadata_json,
        ],
    )
    .map_err(|error| map_sqlite_error("save snapshot", error))?;

    tx.commit()
        .map_err(|error| map_sqlite_error("commit snapshot transaction", error))?;
    Ok(())
}

/// Prunes durable events at or below `sequence` for `session_id` (RC-305).
///
/// The caller must have written a snapshot at or above `sequence` first (see
/// [`crate::retention::prune_plan`]) so a restore can still reconstruct the
/// session; this function does not verify that precondition by design — the
/// retention policy lives with the caller.
fn handle_prune(
    conn: &mut Connection,
    session_id: SessionId,
    sequence: u64,
) -> Result<u64, StoreError> {
    let tx = conn
        .transaction()
        .map_err(|error| map_sqlite_error("begin prune transaction", error))?;
    let removed = tx
        .execute(
            "DELETE FROM durable_events
             WHERE session_id = ?1 AND session_sequence <= ?2",
            params![session_id.to_string(), sequence as i64],
        )
        .map_err(|error| map_sqlite_error("prune durable events", error))?;
    tx.commit()
        .map_err(|error| map_sqlite_error("commit prune transaction", error))?;
    Ok(removed as u64)
}

/// Ensures a `sessions` row exists for `session_id`, touching `updated_at`.
///
/// With `update_root`, an existing row's `root_agent_id` is corrected to the
/// authoritative value (used by `save_snapshot`); without it, `root_agent_id`
/// is only set on first insert.
fn ensure_session(
    conn: &Connection,
    session_id: &SessionId,
    root_agent_id: &AgentId,
    updated_at: i64,
    update_root: bool,
) -> Result<(), StoreError> {
    let sql = if update_root {
        "INSERT INTO sessions (session_id, root_agent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(session_id) DO UPDATE SET
             root_agent_id = excluded.root_agent_id,
             updated_at    = excluded.updated_at"
    } else {
        "INSERT INTO sessions (session_id, root_agent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(session_id) DO UPDATE SET updated_at = excluded.updated_at"
    };
    conn.execute(
        sql,
        params![
            session_id.to_string(),
            root_agent_id.to_string(),
            updated_at
        ],
    )
    .map_err(|error| map_sqlite_error("ensure session row", error))?;
    Ok(())
}

/// Reads the durable history after a reconnect cursor without applying the
/// restore snapshot cutoff. The durable-events index makes this a bounded
/// range query even when a session has a long history.
fn events_since_sync(
    pool: &r2d2::Pool<SqliteConnectionManager>,
    id: SessionId,
    since_seq: u64,
) -> Result<Vec<DurableSessionEvent>, StoreError> {
    let conn = pool
        .get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT session_sequence, envelope
             FROM durable_events
             WHERE session_id = ?1 AND session_sequence > ?2
             ORDER BY session_sequence ASC, appended_at ASC",
        )
        .map_err(|error| map_sqlite_error("prepare resumed event query", error))?;
    let rows = stmt
        .query_map(params![id.to_string(), since_seq as i64], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error("query resumed events", error))?;

    let mut events = Vec::new();
    for row in rows {
        let (session_sequence, envelope_json) =
            row.map_err(|error| map_sqlite_error("read resumed event row", error))?;
        let mut envelope: AgentEventEnvelope =
            serde_json::from_str(&envelope_json).map_err(|error| {
                StoreError::InvalidState(format!("corrupt durable event envelope: {error}"))
            })?;
        envelope.session_sequence = Some(session_sequence);
        events.push(DurableSessionEvent {
            envelope,
            session_sequence: Some(session_sequence),
        });
    }
    Ok(events)
}

/// Decodes a stored `snapshots` row into a [`DurableSessionSnapshot`].
///
/// `schema_version` and `metadata` come from the RC-300 columns; pre-RC-300
/// rows (columns defaulting to 0 / `{}`) are reported as version 0 with empty
/// metadata so [`crate::version::migrate_snapshot`] can upgrade them.
fn decode_snapshot_row(
    id: SessionId,
    row: (String, String, i64, String, i64, String),
) -> Result<DurableSessionSnapshot, StoreError> {
    let (
        root_agent_id,
        agents_json,
        session_sequence,
        timestamp_rfc3339,
        schema_version,
        metadata_json,
    ) = row;
    let agents: Vec<StoredAgentState> = serde_json::from_str(&agents_json)?;
    let root_agent_id = AgentId::from_str(&root_agent_id).map_err(|error| {
        StoreError::InvalidState(format!(
            "corrupt root_agent_id {root_agent_id:?} in snapshot: {error}"
        ))
    })?;
    let timestamp = serde_json::from_str::<Timestamp>(&format!("\"{timestamp_rfc3339}\""))
        .map_err(|error| {
            StoreError::InvalidState(format!(
                "corrupt snapshot timestamp {timestamp_rfc3339:?}: {error}"
            ))
        })?;
    let metadata: DurableSessionMetadata =
        serde_json::from_str(&metadata_json).map_err(|error| {
            StoreError::InvalidState(format!(
                "corrupt snapshot metadata {metadata_json:?}: {error}"
            ))
        })?;
    Ok(DurableSessionSnapshot {
        session_id: id,
        root_agent_id,
        agents,
        session_sequence: session_sequence as u64,
        timestamp,
        schema_version: schema_version as u64,
        metadata,
    })
}

/// Reconstructs a [`StoredSession`] for `id` from the latest snapshot plus the
/// durable events appended after it. Runs on the read pool (blocking call).
fn load_session_sync(
    pool: &r2d2::Pool<SqliteConnectionManager>,
    id: SessionId,
) -> Result<StoredSession, StoreError> {
    let conn = pool
        .get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;

    // Latest restore checkpoint, if any.
    let snapshot_row: Option<(String, String, i64, String, i64, String)> = conn
        .query_row(
            "SELECT root_agent_id, agents, session_sequence, timestamp, schema_version, metadata
             FROM snapshots WHERE session_id = ?1",
            params![id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| map_sqlite_error("load snapshot", error))?;

    let snapshot = match snapshot_row {
        Some(row) => Some(decode_snapshot_row(id, row)?),
        None => None,
    };

    // Durable events in session order. The envelope JSON is the authoritative
    // payload; the denormalized `session_sequence` column is authoritative for
    // ordering (an event appended without a sequence was assigned one here).
    let mut stmt = conn
        .prepare(
            "SELECT session_sequence, envelope
             FROM durable_events
             WHERE session_id = ?1
             ORDER BY session_sequence ASC, appended_at ASC",
        )
        .map_err(|error| map_sqlite_error("prepare event query", error))?;
    let rows = stmt
        .query_map(params![id.to_string()], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
        })
        .map_err(|error| map_sqlite_error("query durable events", error))?;

    let mut events: Vec<DurableSessionEvent> = Vec::new();
    for row in rows {
        let (session_sequence, envelope_json) =
            row.map_err(|error| map_sqlite_error("read durable event row", error))?;
        let mut envelope: AgentEventEnvelope =
            serde_json::from_str(&envelope_json).map_err(|error| {
                StoreError::InvalidState(format!("corrupt durable event envelope: {error}"))
            })?;
        envelope.session_sequence = Some(session_sequence);
        events.push(DurableSessionEvent {
            envelope,
            session_sequence: Some(session_sequence),
        });
    }

    if let Some(snapshot) = &snapshot {
        events.retain(|event| {
            event
                .session_sequence
                .is_some_and(|sequence| sequence > snapshot.session_sequence)
        });
    }

    if snapshot.is_none() && events.is_empty() {
        return Err(StoreError::NotFound(id));
    }

    Ok(StoredSession {
        session_id: id,
        snapshot,
        events,
    })
}

/// Resolves the highest committed sequence for `id` via indexed queries.
fn current_sequence_sync(
    pool: &r2d2::Pool<SqliteConnectionManager>,
    id: SessionId,
) -> Result<u64, StoreError> {
    let conn = pool
        .get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;
    let event_max: Option<i64> = conn
        .query_row(
            "SELECT MAX(session_sequence) FROM durable_events WHERE session_id = ?1",
            params![id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| map_sqlite_error("read max event sequence", error))?;
    let snapshot_max: Option<i64> = conn
        .query_row(
            "SELECT MAX(session_sequence) FROM snapshots WHERE session_id = ?1",
            params![id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(|error| map_sqlite_error("read max snapshot sequence", error))?;
    let event_max = event_max.map(|value| value as u64).unwrap_or(0);
    let snapshot_max = snapshot_max.map(|value| value as u64).unwrap_or(0);
    Ok(event_max.max(snapshot_max))
}

/// Streams the unprocessed record stream (events + latest snapshot), with the
/// snapshot's real identity/version/metadata fields — never placeholders.
fn raw_records_sync(
    pool: &r2d2::Pool<SqliteConnectionManager>,
    id: SessionId,
) -> Result<Vec<RawRecord>, StoreError> {
    let mut records = Vec::new();

    {
        let conn = pool
            .get()
            .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT session_sequence, envelope
                 FROM durable_events
                 WHERE session_id = ?1
                 ORDER BY session_sequence ASC",
            )
            .map_err(|error| map_sqlite_error("prepare raw event query", error))?;
        let rows = stmt
            .query_map(params![id.to_string()], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
            })
            .map_err(|error| map_sqlite_error("query raw events", error))?;
        for row in rows {
            let (session_sequence, envelope_json) =
                row.map_err(|error| map_sqlite_error("read raw event row", error))?;
            let mut envelope: AgentEventEnvelope =
                serde_json::from_str(&envelope_json).map_err(|error| {
                    StoreError::InvalidState(format!("corrupt durable event envelope: {error}"))
                })?;
            envelope.session_sequence = Some(session_sequence);
            records.push(RawRecord::Event(DurableSessionEvent {
                envelope,
                session_sequence: Some(session_sequence),
            }));
        }
    }

    let conn = pool
        .get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;
    let snapshot_row: Option<(String, String, i64, String, i64, String)> = conn
        .query_row(
            "SELECT root_agent_id, agents, session_sequence, timestamp, schema_version, metadata
             FROM snapshots WHERE session_id = ?1",
            params![id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| map_sqlite_error("load raw snapshot", error))?;
    if let Some(row) = snapshot_row {
        records.push(RawRecord::Snapshot(decode_snapshot_row(id, row)?));
    }
    Ok(records)
}

fn list_sessions_sync(
    pool: &r2d2::Pool<SqliteConnectionManager>,
) -> Result<Vec<SessionSummary>, StoreError> {
    let conn = pool
        .get()
        .map_err(|error| StoreError::Backend(format!("acquire read connection: {error}")))?;
    let mut stmt = conn
        .prepare("SELECT session_id FROM sessions ORDER BY updated_at DESC")
        .map_err(|error| map_sqlite_error("prepare session catalog query", error))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| map_sqlite_error("query session catalog", error))?;

    let mut ids = Vec::new();
    for row in rows {
        let value = row.map_err(|error| map_sqlite_error("read session catalog row", error))?;
        let id = SessionId::from_str(&value).map_err(|error| {
            StoreError::InvalidState(format!("corrupt session_id {value:?}: {error}"))
        })?;
        ids.push(id);
    }
    drop(stmt);
    drop(conn);

    let mut summaries = Vec::new();
    for id in ids {
        if let Some(summary) = summarize_session(&load_session_sync(pool, id)?) {
            summaries.push(summary);
        }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    Ok(summaries)
}

fn map_sqlite_error(operation: &str, error: rusqlite::Error) -> StoreError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error {
        if matches!(
            code.extended_code,
            SQLITE_CONSTRAINT_FOREIGNKEY | SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE
        ) {
            return StoreError::InvalidState(format!("{operation}: {error}"));
        }
    }
    StoreError::Backend(format!("{operation}: {error}"))
}

fn now_ms() -> i64 {
    Timestamp::now().timestamp_millis()
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || list_sessions_sync(&pool))
            .await
            .map_err(|error| StoreError::Backend(format!("session catalog task failed: {error}")))?
    }

    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || load_session_sync(&pool, id))
            .await
            .map_err(|error| StoreError::Backend(format!("load session task failed: {error}")))?
    }

    async fn events_since(
        &self,
        id: SessionId,
        since_seq: u64,
    ) -> Result<Vec<DurableSessionEvent>, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || events_since_sync(&pool, id, since_seq))
            .await
            .map_err(|error| StoreError::Backend(format!("event history task failed: {error}")))?
    }

    async fn current_sequence(&self, id: SessionId) -> Result<u64, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || current_sequence_sync(&pool, id))
            .await
            .map_err(|error| StoreError::Backend(format!("sequence task failed: {error}")))?
    }

    async fn raw_records(&self, id: SessionId) -> Result<Vec<RawRecord>, StoreError> {
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || raw_records_sync(&pool, id))
            .await
            .map_err(|error| StoreError::Backend(format!("raw records task failed: {error}")))?
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if !is_durable(&event.envelope.event) {
            return Err(StoreError::InvalidState(format!(
                "refusing to persist ephemeral event: {:?}",
                event.envelope.event
            )));
        }
        let (reply, acknowledgement) = oneshot::channel();
        self.write_tx
            .send(WriteCommand::Append { event, reply })
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task is not running".into()))?;
        acknowledgement
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task terminated".into()))?
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        let (reply, acknowledgement) = oneshot::channel();
        self.write_tx
            .send(WriteCommand::SaveSnapshot { snapshot, reply })
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task is not running".into()))?;
        acknowledgement
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task terminated".into()))?
    }

    async fn prune_events_before(&self, id: SessionId, sequence: u64) -> Result<u64, StoreError> {
        let (reply, acknowledgement) = oneshot::channel();
        self.write_tx
            .send(WriteCommand::Prune {
                session_id: id,
                sequence,
                reply,
            })
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task is not running".into()))?;
        acknowledgement
            .await
            .map_err(|_| StoreError::Backend("SQLite writer task terminated".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId};

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harness-sqlite-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("store.db")
    }

    fn event(session_id: SessionId, sequence: u64) -> DurableSessionEvent {
        DurableSessionEvent {
            envelope: AgentEventEnvelope {
                event_id: EventId::new(),
                session_id,
                agent_id: AgentId::new(),
                parent_agent_id: None,
                run_id: Some(RunId::new()),
                agent_sequence: sequence,
                session_sequence: Some(sequence),
                timestamp: Timestamp::now(),
                visibility: EventVisibility::User,
                event: AgentEvent::StateChanged {
                    from: AgentStatus::Idle,
                    to: AgentStatus::PreparingContext,
                },
            },
            session_sequence: Some(sequence),
        }
    }

    #[test]
    fn opens_a_wal_database() {
        let path = temp_db("wal");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let store = runtime
            .block_on(async { SqliteSessionStore::open(&path) })
            .expect("open store");
        drop(store);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[tokio::test]
    async fn current_sequence_is_zero_for_fresh_session() {
        let store = SqliteSessionStore::open(temp_db("seq-zero")).expect("open store");
        assert_eq!(
            store
                .current_sequence(SessionId::new())
                .await
                .expect("sequence"),
            0
        );
    }

    #[tokio::test]
    async fn current_sequence_tracks_committed_events() {
        let store = SqliteSessionStore::open(temp_db("seq")).expect("open store");
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append 1");
        store.append(event(session, 2)).await.expect("append 2");
        assert_eq!(store.current_sequence(session).await.expect("sequence"), 2);
    }

    #[tokio::test]
    async fn prune_removes_only_events_at_or_below_sequence() {
        let store = SqliteSessionStore::open(temp_db("prune")).expect("open store");
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append 1");
        store.append(event(session, 2)).await.expect("append 2");
        store.append(event(session, 3)).await.expect("append 3");

        let removed = store.prune_events_before(session, 2).await.expect("prune");
        assert_eq!(removed, 2);

        let stored = store.load_session(session).await.expect("load");
        let sequences: Vec<u64> = stored
            .events
            .iter()
            .filter_map(|event| event.session_sequence)
            .collect();
        assert_eq!(
            sequences,
            vec![3],
            "only events above the prune point survive"
        );
    }

    #[tokio::test]
    async fn snapshot_version_and_metadata_round_trip() {
        let store = SqliteSessionStore::open(temp_db("snap-meta")).expect("open store");
        let session = SessionId::new();
        let mut snapshot = crate::testing::test_snapshot(session, 1);
        snapshot.metadata.workspace_identity = Some("/srv/prod".into());
        snapshot.metadata.integration_references = vec!["anthropic".into()];
        store.save_snapshot(snapshot).await.expect("save snapshot");

        let stored = store.load_session(session).await.expect("load");
        let loaded = stored.snapshot.expect("snapshot");
        assert_eq!(loaded.schema_version, crate::version::SCHEMA_VERSION);
        assert_eq!(
            loaded.metadata.workspace_identity.as_deref(),
            Some("/srv/prod")
        );
        assert_eq!(loaded.metadata.integration_references, vec!["anthropic"]);
    }
}
