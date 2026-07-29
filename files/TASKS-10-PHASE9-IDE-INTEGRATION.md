# Phase 9 — IDE Integration

**Goal (spec Section 71):** two supported IDE embedding modes — current-style sidecar (WebSocket) and native/embedded (direct Rust calls) — coexisting on the same session semantics and event schema.
**Depends on:** Phase 8 complete.
**Crates touched:** `harness-transport-websocket`, `harness-transport-stdio` (optional, if the sidecar uses stdio instead/in addition), `apps/harnessd`, `harness-engine` (remote client trait).

---

## Tasks

### Task 9.1 — `RemoteSessionClient` trait-level contract
- **Files:** `crates/harness-engine/src/handles.rs`
- **Description:** Ensure `SessionApi`/`SessionClient` (spec Sections 48, 76) is implemented by both a `LocalSessionClient` (already existing from Phase 2) and a new `RemoteSessionClient` so a frontend genuinely cannot tell which it received (spec Section 49). Define the wire contract for commands/snapshots/events that any transport must carry — reuse the `AgentEventEnvelope`/`SessionEvent`/`SessionCommand` types directly (already `serde`-friendly since Phase 1), no separate wire schema needed.
- **Acceptance criteria:** a test swaps a `LocalSessionClient` for a `RemoteSessionClient` (backed by an in-process mock transport, no real socket) behind the same `SessionApi` and passes identical assertions against both — proving transport independence (spec Section 72's "transport independence" invariant).
- **Effort:** M
- **Depends on:** Phase 8 complete

### Task 9.2 — WebSocket transport
- **Files:** `crates/transports/websocket/src/lib.rs`, `crates/transports/websocket/src/server.rs`, `crates/transports/websocket/src/client.rs`
- **Description:** Implement using `tokio-tungstenite` (locked decision, `TASKS-00-OVERVIEW.md` §2) for both server (daemon side, accepting frontend connections) and client (if a Rust-side client is ever needed, e.g. for testing) roles. Server: accept a WebSocket connection, deserialize incoming `SessionCommand` frames, serialize outgoing `SessionEvent`/snapshot-response frames, one connection per session subscription (or a light multiplexing envelope if one socket serves multiple sessions — decide based on the actual frontend's needs; default recommendation is one logical subscription per session to keep framing simple).
- **Acceptance criteria:** an integration test opens a real WebSocket connection to a locally spawned server, sends a `Prompt` command as a JSON frame, and receives the expected ordered `SessionEvent` frames back.
- **Effort:** L
- **Depends on:** Task 9.1

### Task 9.3 — `harnessd` daemon binary
- **Files:** `apps/harnessd/src/main.rs`, `apps/harnessd/src/config.rs`
- **Description:** Build the actual daemon: constructs `Harness` (same builder pattern as `apps/harness`, Phase 8 Task 8.2), binds the WebSocket transport (Task 9.2) to a configurable address/port, and serves session commands/events for any number of connecting frontends. This directly replaces/parallels "the current sidecar architecture" referenced in spec Section 49/51.3.
- **Acceptance criteria:** `harnessd` starts, a test client connects, creates a session, prompts it, and observes the full event stream over the wire — end-to-end sidecar mode working.
- **Effort:** M
- **Depends on:** Task 9.2

### Task 9.4 — Native/embedded mode
- **Files:** none new (this validates direct use of `harness-engine` in-process, as already exercised by `apps/harness` in Phase 8)
- **Description:** Explicitly document and test the "no transport" embedding path (spec Section 51.2): an embedding IDE process links `harness-engine` directly and calls `SessionApi` methods in-process, with zero WebSocket/serialization overhead. This is largely already proven by Phase 8's TUI; this task's job is to produce the second required proof point (an IDE-shaped host, not a terminal-shaped one) — a minimal example/test harness simulating "IDE-specific tools" (spec Section 51.2 diagram: editor, language services, IDE-specific tools alongside the Harness Engine) is sufficient; a full real IDE integration is out of scope for this repo.
- **Acceptance criteria:** a test or example crate demonstrates constructing `Harness` with at least one IDE-flavored `ContextProvider`/tool stub (e.g. a fake "open buffers" context provider) alongside the standard integrations, proving the engine composes cleanly with IDE-specific extensions without any transport layer.
- **Effort:** M
- **Depends on:** Task 9.1

### Task 9.5 — Shared session-semantics conformance test
- **Files:** `crates/harness-engine/tests/transport_parity.rs`
- **Description:** One test suite, run twice — once against `LocalSessionClient` (native/embedded), once against a `RemoteSessionClient` over the real WebSocket transport (Task 9.2) — asserting byte-for-byte-equivalent (modulo transport framing) `SessionEvent` sequences for the same scripted interaction. This is the concrete verification of spec Section 72's "changing WebSocket to direct Rust calls must not change session semantics."
- **Acceptance criteria:** the shared test suite passes against both transports with identical event assertions.
- **Effort:** M
- **Depends on:** Tasks 9.2, 9.4

### Task 9.6 — Reconnect / snapshot-then-stream flow over transport
- **Files:** `crates/transports/websocket/src/server.rs`
- **Description:** Implement the reconnect path explicitly for a remote frontend: on (re)connect, the client first calls `snapshot()` over the wire, then `subscribe()`, exactly mirroring spec Section 45's "snapshot answers what is true now, the stream answers what changed." Handle the case where events occurred between a dropped connection and reconnect (client's last-seen `session_sequence` can optionally be used to request only missed durable events from the `SessionStore`, Phase 7 — stretch goal, minimum requirement is snapshot+fresh-subscribe working correctly).
- **Acceptance criteria:** a test disconnects a WebSocket client mid-session, reconnects, calls snapshot, and observes state consistent with the session's true current status.
- **Effort:** M
- **Depends on:** Task 9.2, Phase 7 (for durable event replay, optional stretch)

---

## Testing (this phase)

- Transport-parity conformance suite (Task 9.5) is the centerpiece.
- Real-socket integration tests for the WebSocket transport (Task 9.2/9.3).
- Reconnect/snapshot test (Task 9.6).

## Exit criteria

- Both sidecar (WebSocket) and native/embedded modes work against the same `Harness`/session semantics, verified by one shared conformance suite.
- `harnessd` is a real, runnable daemon serving real sessions over WebSocket.
- No session-semantic code exists inside the transport crate — `harness-transport-websocket` only serializes/deserializes and forwards.

## Trade-offs / open decisions

- **One socket per session vs. multiplexed socket:** default to one logical subscription per session for framing simplicity; revisit if the real frontend strongly prefers a single multiplexed connection carrying multiple sessions' events (would require adding a `session_id` envelope at the transport framing level, which is a small, transport-local change).
- **Missed-event replay on reconnect:** treated as a stretch goal in Task 9.6; minimum viable behavior (snapshot + fresh subscribe, potentially missing ephemeral events during the gap) is acceptable for this phase's exit criteria.
