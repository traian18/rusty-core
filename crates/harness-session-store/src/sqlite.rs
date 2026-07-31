//! WAL-mode SQLite [`SessionStore`] backed by a single-writer actor (Tasks 7.2/7.3).
//!
//! # Architecture
//!
//! SQLite serializes writers regardless of driver, so all mutations are routed
//! through **one background task** that owns the single WAL-mode
//! `rusqlite::Connection`:
//!
//! ```text
//!              append / save_snapshot
//!                        │
//!              mpsc channel (WriteCommand)
//!                        ▼
//!            writer actor ── owns ──> WAL-mode Connection
//!                        │
//!              pooled reads (r2d2_sqlite)  ◄── load_session
//! ```
//!
//! - **Writes**: [`SqliteSessionStore::append`] and
//!   [`SqliteSessionStore::save_snapshot`] send a [`WriteCommand`] over an
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
//! (`IF NOT EXISTS`, `migrations/0001_init.sql`) re-applies cleanly.
//!
//! # Schema mapping
//!
//! `append` writes exactly one `durable_events` row (the full
//! `AgentEventEnvelope` as JSON in `envelope`); `save_snapshot` upserts the
//! session's single `snapshots` row (replacing any earlier one), carrying the
//! `Vec<StoredAgentState>` as JSON; `load_session` returns the latest snapshot
//! plus the durable events appended after it (`session_sequence >
//! snapshot.session_sequence`, ordered by sequence). The per-agent projection
//! (`agents`) and usage denormalization (`usage_records`) tables defined by
//! the migration are intentionally not populated here — the snapshot's JSON
//! `agents` column is the source of truth for restore, and usage is derivable
//! from the event log (spec §71 "snapshot + event restoration").
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
    is_durable, DurableSessionEvent, DurableSessionSnapshot, SessionStore, StoredAgentState,
    StoredSession, StoreError,
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

        Ok(Self { write_tx, read_pool })
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
/// `root_agent_id` to the snapshot's root agent.
fn handle_save_snapshot(
    conn: &mut Connection,
    snapshot: DurableSessionSnapshot,
) -> Result<(), StoreError> {
    let tx = conn
        .transaction()
        .map_err(|error| map_sqlite_error("begin snapshot transaction", error))?;

    ensure_session(&tx, &snapshot.session_id, &snapshot.root_agent_id, now_ms(), true)?;

    let agents_json = serde_json::to_string(&snapshot.agents)?;
    // The timestamp column holds the RFC3339-serialized `Timestamp` (via
    // `to_rfc3339`) so the snapshot timestamp round-trips losslessly without
    // re-deriving it from a truncated millisecond value. SQLite stores the
    // text value in the INTEGER-affinity column without complaint, and
    // `to_rfc3339` output sorts lexicographically like timestamps.
    let timestamp_rfc3339 = snapshot.timestamp.to_rfc3339();
    tx.execute(
        "INSERT INTO snapshots (session_id, root_agent_id, agents, session_sequence, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id) DO UPDATE SET
             root_agent_id    = excluded.root_agent_id,
             agents           = excluded.agents,
             session_sequence = excluded.session_sequence,
             timestamp        = excluded.timestamp",
        params![
            snapshot.session_id.to_string(),
            snapshot.root_agent_id.to_string(),
            agents_json,
            snapshot.session_sequence as i64,
            timestamp_rfc3339,
        ],
    )
    .map_err(|error| map_sqlite_error("save snapshot", error))?;

    tx.commit()
        .map_err(|error| map_sqlite_error("commit snapshot transaction", error))?;
    Ok(())
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
        params![session_id.to_string(), root_agent_id.to_string(), updated_at],
    )
    .map_err(|error| map_sqlite_error("ensure session row", error))?;
    Ok(())
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
    let snapshot_row: Option<(String, String, i64, String)> = conn
        .query_row(
            "SELECT root_agent_id, agents, session_sequence, timestamp
             FROM snapshots WHERE session_id = ?1",
            params![id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| map_sqlite_error("load snapshot", error))?;

    let snapshot = match snapshot_row {
        Some((root_agent_id, agents_json, session_sequence, timestamp_rfc3339)) => {
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
            Some(DurableSessionSnapshot {
                session_id: id,
                root_agent_id,
                agents,
                session_sequence: session_sequence as u64,
                timestamp,
            })
        }
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
        let mut envelope: AgentEventEnvelope = serde_json::from_str(&envelope_json).map_err(
            |error| {
                StoreError::InvalidState(format!("corrupt durable event envelope: {error}"))
            },
        )?;
        envelope.session_sequence = Some(session_sequence);
        events.push(DurableSessionEvent {
            envelope,
            session_sequence: Some(session_sequence),
        });
    }

    if snapshot.is_none() && events.is_empty() {
        return Err(StoreError::NotFound(id));
    }

    // Replay only the events not already captured by the snapshot (spec §71).
    if let Some(snap) = &snapshot {
        events.retain(|event| {
            event
                .session_sequence
                .map_or(true, |seq| seq > snap.session_sequence)
        });
    }

    Ok(StoredSession {
        session_id: id,
        snapshot,
        events,
    })
}

/// Current wall clock in unix epoch milliseconds (store write time).
fn now_ms() -> i64 {
    Timestamp::now().timestamp_millis()
}

/// Maps a `rusqlite` error onto [`StoreError`], surfacing constraint
/// violations (duplicate sequence/event, missing session) as
/// [`StoreError::InvalidState`] and everything else as
/// [`StoreError::Backend`].
fn map_sqlite_error(context: &str, error: rusqlite::Error) -> StoreError {
    match &error {
        rusqlite::Error::SqliteFailure(ffi_error, _) => match ffi_error.extended_code {
            SQLITE_CONSTRAINT_UNIQUE | SQLITE_CONSTRAINT_PRIMARYKEY => StoreError::InvalidState(
                format!("{context}: duplicate durable key (UNIQUE/PRIMARY KEY violated): {error}"),
            ),
            SQLITE_CONSTRAINT_FOREIGNKEY => StoreError::InvalidState(format!(
                "{context}: foreign key violated (session row missing): {error}"
            )),
            _ => StoreError::Backend(format!("{context}: {error}")),
        },
        other => StoreError::Backend(format!("{context}: {other}")),
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn load_session(&self, id: SessionId) -> Result<StoredSession, StoreError> {
        // Reads go through the pooled connection — never the writer channel.
        let pool = self.read_pool.clone();
        tokio::task::spawn_blocking(move || load_session_sync(&pool, id))
            .await
            .map_err(|join_error| {
                StoreError::Backend(format!("read task panicked: {join_error}"))
            })?
    }

    async fn append(&self, event: DurableSessionEvent) -> Result<(), StoreError> {
        if !is_durable(&event.envelope.event) {
            return Err(StoreError::InvalidState(format!(
                "refusing to persist ephemeral event: {:?}",
                event.envelope.event
            )));
        }
        let (reply, ack) = oneshot::channel();
        self.write_tx
            .send(WriteCommand::Append { event, reply })
            .await
            .map_err(|_| StoreError::Backend("sqlite writer task is not running".into()))?;
        ack.await
            .map_err(|_| {
                StoreError::Backend("sqlite writer task terminated before acknowledging".into())
            })?
    }

    async fn save_snapshot(&self, snapshot: DurableSessionSnapshot) -> Result<(), StoreError> {
        let (reply, ack) = oneshot::channel();
        self.write_tx
            .send(WriteCommand::SaveSnapshot { snapshot, reply })
            .await
            .map_err(|_| StoreError::Backend("sqlite writer task is not running".into()))?;
        ack.await
            .map_err(|_| {
                StoreError::Backend("sqlite writer task terminated before acknowledging".into())
            })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use harness_protocol::commands::AgentStatus;
    use harness_protocol::events::{AgentEvent, AgentEventEnvelope, EventVisibility};
    use harness_protocol::ids::{AgentId, EventId, RunId, Timestamp};

    /// Creates a unique scratch database path for one test.
    fn temp_db(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "harness-session-store-sqlite-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp store dir");
        dir.join("store.db")
    }

    /// A fully-qualified, sequenced envelope for testing.
    fn envelope(session: SessionId, seq: u64) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: EventId::new(),
            session_id: session,
            agent_id: AgentId::new(),
            parent_agent_id: None,
            run_id: Some(RunId::new()),
            agent_sequence: seq,
            session_sequence: Some(seq),
            timestamp: Timestamp::now(),
            visibility: EventVisibility::User,
            event: AgentEvent::StateChanged {
                from: AgentStatus::Idle,
                to: AgentStatus::PreparingContext,
            },
        }
    }

    /// A durable event with the given session sequence.
    fn event(session: SessionId, seq: u64) -> DurableSessionEvent {
        DurableSessionEvent {
            session_sequence: Some(seq),
            envelope: envelope(session, seq),
        }
    }

    /// A minimal snapshot at the given session sequence.
    fn snapshot(session: SessionId, seq: u64) -> DurableSessionSnapshot {
        DurableSessionSnapshot {
            session_id: session,
            root_agent_id: AgentId::new(),
            agents: Vec::new(),
            session_sequence: seq,
            timestamp: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn roundtrip_events_and_snapshot() {
        let db_path = temp_db("roundtrip");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        let mut appended_ids = Vec::new();
        for seq in 1..=5 {
            let ev = event(session, seq);
            appended_ids.push(ev.envelope.event_id);
            store.append(ev).await.expect("append event");
        }
        store
            .save_snapshot(snapshot(session, 3))
            .await
            .expect("save snapshot");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(stored.session_id, session);

        let loaded_snapshot = stored.snapshot.expect("snapshot present");
        assert_eq!(loaded_snapshot.session_id, session);
        assert_eq!(loaded_snapshot.session_sequence, 3);

        // Only the events appended after the snapshot's sequence remain.
        let sequences: Vec<Option<u64>> = stored
            .events
            .iter()
            .map(|e| e.session_sequence)
            .collect();
        assert_eq!(sequences, vec![Some(4), Some(5)]);

        // The envelopes round-tripped intact (payload + event ids).
        assert_eq!(stored.events[0].envelope.event_id, appended_ids[3]);
        assert_eq!(stored.events[1].envelope.event_id, appended_ids[4]);
        for event in &stored.events {
            assert_eq!(event.envelope.session_id, session);
            assert!(matches!(event.envelope.event, AgentEvent::StateChanged { .. }));
        }
    }

    #[tokio::test]
    async fn load_returns_all_events_when_no_snapshot() {
        let db_path = temp_db("no-snapshot");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store.append(event(session, 2)).await.expect("append event");

        let stored = store.load_session(session).await.expect("load session");
        assert!(stored.snapshot.is_none());
        let sequences: Vec<Option<u64>> = stored
            .events
            .iter()
            .map(|e| e.session_sequence)
            .collect();
        assert_eq!(sequences, vec![Some(1), Some(2)]);
    }

    #[tokio::test]
    async fn load_missing_session_is_not_found() {
        let db_path = temp_db("not-found");
        let store = SqliteSessionStore::open(&db_path).expect("open store");

        let error = store
            .load_session(SessionId::new())
            .await
            .expect_err("no data exists for an unknown session");
        assert!(matches!(error, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn data_survives_store_recreation() {
        let db_path = temp_db("restart");
        let session = SessionId::new();

        {
            let store = SqliteSessionStore::open(&db_path).expect("open store");
            store.append(event(session, 1)).await.expect("append event");
            store.append(event(session, 2)).await.expect("append event");
            store
                .save_snapshot(snapshot(session, 2))
                .await
                .expect("save snapshot");
        } // store dropped -> channel closed -> writer checkpoints and exits

        let reopened = SqliteSessionStore::open(&db_path).expect("reopen store");
        let stored = reopened.load_session(session).await.expect("reload session");
        assert_eq!(
            stored.snapshot.as_ref().map(|s| s.session_sequence),
            Some(2)
        );
        assert_eq!(stored.events.len(), 0, "both events captured by the snapshot");
    }

    #[tokio::test]
    async fn wal_mode_is_confirmed_and_persistent() {
        let db_path = temp_db("wal");
        // `open` itself refuses to proceed unless PRAGMA journal_mode=WAL
        // returns "wal" — reaching here is the confirmation.
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();
        store.append(event(session, 1)).await.expect("append event");
        drop(store);

        // WAL is a persistent database property: a brand-new connection sees it.
        let conn = Connection::open(&db_path).expect("open raw db");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read journal mode");
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn concurrent_appends_never_interleave_or_corrupt() {
        let db_path = temp_db("concurrent");
        let store = std::sync::Arc::new(SqliteSessionStore::open(&db_path).expect("open store"));
        let session_a = SessionId::new();
        let session_b = SessionId::new();

        // Interleave appends from two sessions, plus a same-session burst.
        let mut handles = Vec::new();
        for seq in 0..16 {
            let store_a = store.clone();
            handles.push(tokio::spawn(async move {
                store_a.append(event(session_a, seq)).await.expect("append to A");
            }));
            let store_b = store.clone();
            handles.push(tokio::spawn(async move {
                store_b.append(event(session_b, seq)).await.expect("append to B");
            }));
        }
        for seq in 16..48 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store.append(event(session_a, seq)).await.expect("burst append to A");
            }));
        }
        for handle in handles {
            handle.await.expect("join append task");
        }

        let a = store.load_session(session_a).await.expect("load session A");
        let b = store.load_session(session_b).await.expect("load session B");

        // Every append landed exactly once, as a complete, intact row.
        let seqs_a: Vec<u64> = a.events.iter().map(|e| e.session_sequence.unwrap()).collect();
        let seqs_b: Vec<u64> = b.events.iter().map(|e| e.session_sequence.unwrap()).collect();
        assert_eq!(seqs_a, (0..48).collect::<Vec<u64>>());
        assert_eq!(seqs_b, (0..16).collect::<Vec<u64>>());
        assert!(a.events.iter().all(|e| e.envelope.event_id.to_string().len() == 36));
        assert!(b.events.iter().all(|e| e.envelope.event_id.to_string().len() == 36));
    }

    #[tokio::test]
    async fn unsequenced_events_are_assigned_sequences() {
        let db_path = temp_db("unsequenced");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        let mut unsequenced = event(session, 1);
        unsequenced.session_sequence = None;
        unsequenced.envelope.session_sequence = None;
        store.append(unsequenced).await.expect("append unsequenced");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(stored.events.len(), 1);
        assert_eq!(stored.events[0].session_sequence, Some(1));
        assert_eq!(stored.events[0].envelope.session_sequence, Some(1));
    }

    #[tokio::test]
    async fn duplicate_sequence_is_rejected() {
        let db_path = temp_db("duplicate");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append first");
        // Same (session, sequence) with a fresh event_id violates the UNIQUE index.
        let error = store
            .append(event(session, 1))
            .await
            .expect_err("duplicate sequence must be rejected");
        assert!(matches!(error, StoreError::InvalidState(_)));
    }

    #[tokio::test]
    async fn later_snapshot_replaces_earlier() {
        let db_path = temp_db("replace");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 1))
            .await
            .expect("save snapshot");
        store.append(event(session, 2)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 2))
            .await
            .expect("save snapshot");

        let stored = store.load_session(session).await.expect("load session");
        assert_eq!(
            stored.snapshot.as_ref().map(|s| s.session_sequence),
            Some(2),
            "the latest snapshot row replaces the earlier one"
        );
        assert_eq!(stored.events.len(), 0);
    }

    #[tokio::test]
    async fn snapshot_timestamp_roundtrips_exactly() {
        let db_path = temp_db("timestamp");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        let mut snap = snapshot(session, 1);
        snap.timestamp = Timestamp::now(); // nanosecond precision
        store
            .save_snapshot(snap.clone())
            .await
            .expect("save snapshot");

        let stored = store.load_session(session).await.expect("load session");
        let loaded = stored.snapshot.expect("snapshot present");
        assert_eq!(
            loaded.timestamp, snap.timestamp,
            "snapshot timestamp must survive the sqlite round-trip exactly"
        );
    }

    #[tokio::test]
    async fn event_and_snapshot_rows_land_in_expected_tables() {
        let db_path = temp_db("rows");
        let store = SqliteSessionStore::open(&db_path).expect("open store");
        let session = SessionId::new();

        store.append(event(session, 1)).await.expect("append event");
        store.append(event(session, 2)).await.expect("append event");
        store
            .save_snapshot(snapshot(session, 2))
            .await
            .expect("save snapshot");

        let conn = Connection::open(&db_path).expect("open raw db");
        let events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM durable_events WHERE session_id = ?1",
                params![session.to_string()],
                |row| row.get(0),
            )
            .expect("count durable events");
        assert_eq!(events, 2, "one row per appended event");

        let snapshots: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM snapshots WHERE session_id = ?1",
                params![session.to_string()],
                |row| row.get(0),
            )
            .expect("count snapshots");
        assert_eq!(snapshots, 1, "exactly one snapshot row per session");

        let sessions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE session_id = ?1",
                params![session.to_string()],
                |row| row.get(0),
            )
            .expect("count sessions");
        assert_eq!(sessions, 1, "session row auto-created by first write");
    }

    #[tokio::test]
    async fn crash_recovery_leaves_no_partial_rows() {
        let db_path = temp_db("crash");
        let session = SessionId::new();

        // Simulate a crash mid-batch: an *uncommitted* transaction writes a
        // session row and two event rows, then the connection is dropped
        // without COMMIT — SQLite rolls the transaction back on close.
        {
            let conn = Connection::open(&db_path).expect("open raw db");
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
                .expect("raw pragmas");
            conn.execute_batch(SCHEMA_SQL).expect("raw schema");
            conn.execute("BEGIN IMMEDIATE", []).expect("begin batch");
            let now = now_ms();
            conn.execute(
                "INSERT INTO sessions (session_id, root_agent_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)",
                params![session.to_string(), session.to_string(), now],
            )
            .expect("insert session row");
            for sequence in 1..=2 {
                conn.execute(
                    "INSERT INTO durable_events
                         (event_id, session_id, agent_id, parent_agent_id, run_id,
                          agent_sequence, session_sequence, timestamp, visibility, envelope, appended_at)
                     VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?4, ?5, '\"User\"', '{}', ?5)",
                    params![
                        EventId::new().to_string(),
                        session.to_string(),
                        AgentId::new().to_string(),
                        sequence,
                        now,
                    ],
                )
                .expect("insert event row");
            }
            // Dropped here without COMMIT -> rollback.
        }

        // Reopen through the store: WAL replays/rolls back cleanly.
        let store = SqliteSessionStore::open(&db_path).expect("reopen store");

        let raw = Connection::open(&db_path).expect("open raw db");
        let integrity: String = raw
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("integrity check");
        assert_eq!(integrity, "ok", "no partial/corrupt rows after the crash");

        let error = store
            .load_session(session)
            .await
            .expect_err("the uncommitted batch must not be visible");
        assert!(matches!(error, StoreError::NotFound(_)));

        // The store remains fully writable after recovery.
        store.append(event(session, 1)).await.expect("append after recovery");
        let stored = store.load_session(session).await.expect("load after recovery");
        assert_eq!(stored.events.len(), 1);
    }
}
