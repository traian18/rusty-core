-- 0001_init.sql — initial SQLite schema for the harness session store.
--
-- This schema backs the `SessionStore` contract (spec §59): append-only durable
-- event history plus periodic restore checkpoints. It is applied against a
-- WAL-mode `rusqlite::Connection` owned by the single-writer actor (Task 7.2);
-- reads go through a pooled read connection.
--
-- Conventions:
--   * All identifiers are UUIDs persisted as TEXT.
--   * All timestamps are unix epoch *milliseconds* (INTEGER), matching the
--     monotonic ordering requirements of the session stream.
--   * Structured payloads (event envelopes, agent state projections, usage
--     records) are stored as JSON TEXT blobs; the store serializes/deserializes
--     them via `serde_json` (see `store.rs` `StoredAgentState`, etc.).
--
-- The migration is deliberately idempotent-friendly: tables are created with
-- IF NOT EXISTS so a re-apply never corrupts an existing database.

-- ---------------------------------------------------------------------------
-- sessions
-- ---------------------------------------------------------------------------
-- One row per session. `root_agent_id` identifies the session's root agent,
-- mirroring `DurableSessionSnapshot::root_agent_id`.
CREATE TABLE IF NOT EXISTS sessions (
    session_id       TEXT    NOT NULL PRIMARY KEY,          -- SessionId
    root_agent_id    TEXT    NOT NULL,                      -- AgentId of the root agent
    created_at       INTEGER NOT NULL,                      -- unix ms
    updated_at       INTEGER NOT NULL                       -- unix ms (last write)
);

-- ---------------------------------------------------------------------------
-- agents
-- ---------------------------------------------------------------------------
-- Durable state for every agent known to a session (root + descendants). This
-- is the per-agent projection captured by `StoredAgentState` at snapshot time:
-- enough to rebuild a full `AgentState` (status, current_operation,
-- system_prompt, messages, active_run, pending_tools, pending_permissions,
-- children, last_error, transition_sequence, depth) plus the backend binding,
-- budget, and opaque capabilities/usage projections.
--
-- Agents are upserted when a snapshot is saved; the primary key is the
-- (session, agent) pair so a session's agent set is scoped cleanly.
CREATE TABLE IF NOT EXISTS agents (
    session_id          TEXT    NOT NULL,                   -- SessionId
    agent_id            TEXT    NOT NULL,                   -- AgentId
    parent_id           TEXT,                               -- Option<AgentId>
    status              TEXT    NOT NULL,                   -- AgentStatus (serialized)
    current_operation   TEXT,                               -- Option<AgentOperation> (JSON)
    system_prompt       TEXT    NOT NULL,                   -- String
    messages            TEXT    NOT NULL,                   -- JSON: Vec<AgentMessage>
    active_run          TEXT,                               -- Option<RunId>
    pending_tools       TEXT    NOT NULL,                   -- JSON: HashMap<ToolCallId, StoredPendingToolCall>
    pending_permissions TEXT    NOT NULL,                   -- JSON: HashMap<PermissionId, ToolCallId>
    children            TEXT    NOT NULL,                   -- JSON: Vec<AgentId>
    last_error          TEXT,                               -- Option<AgentError> (JSON)
    transition_sequence INTEGER NOT NULL,                   -- u64
    depth               INTEGER NOT NULL,                   -- u32
    backend             TEXT    NOT NULL,                   -- JSON: BackendBinding
    budget              TEXT    NOT NULL,                   -- JSON: AgentBudget
    capabilities        TEXT    NOT NULL,                   -- JSON: opaque AgentCapabilities projection
    usage               TEXT    NOT NULL,                   -- JSON: opaque UsageLedger projection
    updated_at          INTEGER NOT NULL,                   -- unix ms

    PRIMARY KEY (session_id, agent_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);

-- Sessions can be restored root-first by walking this tree.
CREATE INDEX IF NOT EXISTS idx_agents_session_parent
    ON agents (session_id, parent_id);

-- ---------------------------------------------------------------------------
-- durable_events
-- ---------------------------------------------------------------------------
-- Append-only durable event log (spec §46): only events passing `is_durable`
-- are ever written here. The full `AgentEventEnvelope` (event_id, agent_id,
-- parent_agent_id, run_id, agent_sequence, session_sequence, timestamp,
-- visibility) is stored as JSON in `envelope`, and `session_sequence` is
-- denormalized onto its own column for indexed ordering and uniqueness.
--
-- Append-only invariant: rows are never updated or deleted. The UNIQUE
-- constraint on (session_id, session_sequence) guarantees no two durable
-- events for the same session share a sequence number, and the composite
-- index is what the store uses to replay events after the latest snapshot
-- (spec §71 "snapshot + event restoration").
CREATE TABLE IF NOT EXISTS durable_events (
    event_id          TEXT    NOT NULL,                     -- EventId
    session_id        TEXT    NOT NULL,                     -- SessionId
    agent_id          TEXT    NOT NULL,                     -- AgentId
    parent_agent_id   TEXT,                                 -- Option<AgentId>
    run_id            TEXT,                                 -- Option<RunId>
    agent_sequence    INTEGER NOT NULL,                     -- u64, per-agent order
    session_sequence  INTEGER NOT NULL,                     -- u64, per-session order (denormalized)
    timestamp         INTEGER NOT NULL,                     -- unix ms
    visibility        TEXT    NOT NULL,                     -- EventVisibility (serialized)
    envelope          TEXT    NOT NULL,                     -- JSON: full AgentEventEnvelope
    appended_at       INTEGER NOT NULL,                     -- unix ms (store write time)

    PRIMARY KEY (event_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);

-- Append-only ordering: replay durable events for a session in sequence order,
-- and surface duplicate-sequence violations at write time via the UNIQUE index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_durable_events_session_sequence
    ON durable_events (session_id, session_sequence);

-- Index over the full event log by (session, sequence) for cross-session
-- ordering queries (e.g. listing sessions by most-recent activity).
CREATE INDEX IF NOT EXISTS idx_durable_events_session
    ON durable_events (session_id, appended_at);

-- ---------------------------------------------------------------------------
-- usage_records
-- ---------------------------------------------------------------------------
-- Token/cost usage captured per event. Usage is emitted by the agent core as
-- `AgentEvent::UsageUpdated` (durable) and denormalized here so aggregations
-- don't need to parse event envelope JSON. `usage` holds the serialized
-- `AgentUsageSnapshot` payload.
CREATE TABLE IF NOT EXISTS usage_records (
    usage_id       TEXT    NOT NULL,                        -- unique record id (EventId-derived)
    session_id     TEXT    NOT NULL,                        -- SessionId
    agent_id       TEXT    NOT NULL,                        -- AgentId
    run_id         TEXT,                                    -- Option<RunId>
    session_sequence INTEGER NOT NULL,                      -- link to the durable event
    usage          TEXT    NOT NULL,                        -- JSON: AgentUsageSnapshot
    recorded_at    INTEGER NOT NULL,                        -- unix ms

    PRIMARY KEY (usage_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);

-- Typical aggregation paths: all usage for a session, and usage for a single run.
CREATE INDEX IF NOT EXISTS idx_usage_records_session
    ON usage_records (session_id, session_sequence);
CREATE INDEX IF NOT EXISTS idx_usage_records_run
    ON usage_records (session_id, run_id);

-- ---------------------------------------------------------------------------
-- snapshots
-- ---------------------------------------------------------------------------
-- The latest restore checkpoint per session (spec §71). `save_snapshot`
-- replaces any previous snapshot for the same session, so the session_id is a
-- unique key. `agents` is the JSON-serialized `Vec<StoredAgentState>` captured
-- in `DurableSessionSnapshot`; `session_sequence` is the point in the durable
-- stream this snapshot was taken at (events after it are replayed on restore).
CREATE TABLE IF NOT EXISTS snapshots (
    session_id        TEXT    NOT NULL PRIMARY KEY,         -- SessionId
    root_agent_id     TEXT    NOT NULL,                     -- AgentId of the root agent
    agents            TEXT    NOT NULL,                     -- JSON: Vec<StoredAgentState>
    session_sequence  INTEGER NOT NULL,                     -- u64 snapshot point in the event stream
    timestamp         INTEGER NOT NULL,                     -- unix ms (when the snapshot was taken)

    FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
);
