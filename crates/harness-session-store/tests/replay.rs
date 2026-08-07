//! Replay tests: committed JSON fixtures captured from **real failures and
//! edge cases** discovered during development, replayed against **both**
//! [`SessionStore`] implementations — [`SqliteSessionStore`] and
//! [`JsonlSessionStore`] (Task 7.8).
//!
//! This is the persistence-layer realization of spec §68.4:
//!
//! > persist command/event fixtures from real failures and replay them
//!
//! It extends the Phase 2 Task 2.10 replay-fixture pattern from
//! `crates/harness-runtime/tests/replay.rs` to the session store: instead of
//! hand-writing assertions, we commit the exact write transcript and the
//! golden reconstructed session that exposed a bug, then replay it forever.
//!
//! # Fixture convention
//!
//! ## Storage location
//!
//! Fixtures live in **`tests/fixtures/<name>.json`** relative to this crate's
//! root, loaded via `env!("CARGO_MANIFEST_DIR")` (the same mechanism the
//! Phase 2 pattern uses). They are committed golden records, never generated
//! at test time.
//!
//! ## Naming
//!
//! `kebab-case` names describing the failure/edge case they capture (e.g.
//! `roundtrip-nanosecond-timestamp`, `duplicate-sequence-rejected`) — no
//! `fixture-`/`test-` prefixes. Every committed fixture must be registered in
//! [`FIXTURE_NAMES`]; [`every_committed_fixture_is_registered_and_replayed`]
//! fails when a fixture file exists without a registry entry (or a registry
//! entry without a file), so a fixture can never silently stop being replayed.
//!
//! ## Capture procedure
//!
//! When a real failure/edge case is discovered during development:
//!
//! 1. **Reduce** it to the minimal write transcript — the ordered
//!    `append` / `save_snapshot` mutations that reproduce it.
//! 2. **Fix** the store, then **capture** the post-fix behavior as a JSON
//!    fixture: the transcript plus either the golden reconstructed
//!    [`StoredSession`] (`expected_session`) or per-mutation expected
//!    outcomes (`expect`), with `captured_from` metadata documenting the bug
//!    and its fix.
//! 3. **Register** the fixture name in [`FIXTURE_NAMES`] and commit the
//!    fixture in the *same change* as the fix.
//! 4. From then on the fixture is replayed against both stores on every CI
//!    run — a regression in either backend fails loudly at this shared
//!    boundary.
//!
//! ## Fixture schema
//!
//! ```json
//! {
//!   "name": "edge-case-name",
//!   "description": "what this fixture proves",
//!   "captured_from": { "phase": "...", "bug": "...", "fix": "...", "replay": "..." },
//!   "mutations": [
//!     { "kind": "append", "event": { "session_sequence": 1, "envelope": { ... } },
//!       "expect": { "sqlite": { "ok": true }, "jsonl": { "ok": true } } },
//!     { "kind": "save_snapshot", "snapshot": { ... } }
//!   ],
//!   "expected_session": {
//!     "session_id": "...",
//!     "snapshot": { ... },
//!     "events": [ ... ]
//!   }
//! }
//! ```
//!
//! - `event` / `snapshot` are the serde forms of [`DurableSessionEvent`] /
//!   [`DurableSessionSnapshot`], so a fixture deserializes directly into the
//!   store's own payload types.
//! - `expect` (optional) records per-store write outcomes when behavior
//!   legitimately diverges between backends (see below). A mutation without
//!   `expect` is expected to succeed on every store; a store that is missing
//!   from a present `expect` map fails the replay loudly, forcing the author
//!   to consciously record the new backend's behavior.
//! - `expected_session` (optional) is compared **exactly** — full
//!   field-by-field equality via the JSON projection — against what
//!   [`SessionStore::load_session`] reconstructs (snapshot + trailing events).
//!
//! ## Cross-implementation divergence
//!
//! The two stores share the [`SessionStore`] contract but legitimately differ
//! in places (SQLite enforces the append-only invariant with a UNIQUE index;
//! the JSONL store is an unindexed append-only file). When a fixture touches
//! such a place, each mutation's `expect` map records the outcome per store,
//! so a future change to either backend must consciously update the fixture.
//!
//! # Committed fixtures
//!
//! - **`roundtrip-nanosecond-timestamp`** — captured from a real Phase 7
//!   bug: `SqliteSessionStore` first persisted snapshot timestamps as
//!   truncated unix-epoch *milliseconds*, losing nanosecond precision and
//!   breaking the Tasks 7.3/7.4 exact-match round-trip acceptance test. The
//!   fix stores the RFC3339 representation in the `snapshots.timestamp`
//!   column; this fixture replays the exact transcript that exposed it.
//! - **`duplicate-sequence-rejected`** — the append-only edge case: a second
//!   durable event reusing an already-committed `(session, session_sequence)`
//!   must be rejected by SQLite (UNIQUE index) as [`StoreError::InvalidState`]
//!   at write time, while the unindexed JSONL store accepts it (documented
//!   divergence).
//! - **`migrate-v0-to-v1`** — a pre-RC-300 (schema version 0) checkpoint plus
//!   one trailing durable event, proving both stores still read legacy
//!   checkpoints and that the v0 → v1 migration (RC-305) is a committed,
//!   pure transformation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use harness_protocol::ids::SessionId;
use harness_session_store::{
    migrate_snapshot, DurableSessionEvent, DurableSessionSnapshot, JsonlSessionStore, SessionStore,
    SqliteSessionStore, StoreError, StoredSession, SCHEMA_VERSION,
};
use serde::Deserialize;

/// Store identifier used as the key in a mutation's `expect` map.
const STORE_SQLITE: &str = "sqlite";
/// Store identifier used as the key in a mutation's `expect` map.
const STORE_JSONL: &str = "jsonl";

/// Every committed replay fixture. The name must match the file
/// `tests/fixtures/<name>.json` (enforced by
/// [`every_committed_fixture_is_registered_and_replayed`]).
const FIXTURE_NAMES: &[&str] = &[
    "roundtrip-nanosecond-timestamp",
    "duplicate-sequence-rejected",
    "migrate-v0-to-v1",
];

// ===========================================================================
// Fixture types (JSON deserialisation)
// ===========================================================================

/// Top-level replay fixture: a write transcript plus the golden result.
#[derive(Debug, Deserialize)]
struct Fixture {
    /// Must match the fixture file name (without extension).
    name: String,
    /// Optional human-readable description.
    #[serde(default, rename = "description")]
    _description: Option<String>,
    /// Metadata documenting which real bug/edge case the fixture was captured
    /// from and the fix that made the recorded behavior the expected one.
    #[serde(default, rename = "captured_from")]
    _captured_from: Option<CapturedFrom>,
    /// The ordered write transcript to replay.
    mutations: Vec<FixtureMutation>,
    /// Golden reconstructed session, checked exactly against
    /// `SessionStore::load_session`. Absent for fixtures whose mutations
    /// expect failures (e.g. rejection fixtures).
    #[serde(default)]
    expected_session: Option<StoredSession>,
}

/// Free-form provenance metadata for the capture procedure.
#[derive(Debug, Deserialize)]
struct CapturedFrom {
    #[serde(default, rename = "phase")]
    _phase: Option<String>,
    #[serde(default, rename = "bug")]
    _bug: Option<String>,
    #[serde(default, rename = "fix")]
    _fix: Option<String>,
    #[serde(default, rename = "replay")]
    _replay: Option<String>,
}

/// One step of the write transcript.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FixtureMutation {
    /// Append a durable event.
    Append {
        /// The event payload (serde form of [`DurableSessionEvent`]).
        event: DurableSessionEvent,
        /// Per-store expected write outcome; a missing store entry fails the
        /// replay loudly. `None` means every store must succeed.
        #[serde(default)]
        expect: Option<HashMap<String, MutationExpectation>>,
    },
    /// Save (replace) a session snapshot.
    SaveSnapshot {
        /// The snapshot payload (serde form of [`DurableSessionSnapshot`]).
        snapshot: DurableSessionSnapshot,
        /// Per-store expected write outcome (same semantics as `Append`).
        #[serde(default)]
        expect: Option<HashMap<String, MutationExpectation>>,
    },
}

impl FixtureMutation {
    /// The per-store write expectations for this mutation, if any.
    fn expect(&self) -> &Option<HashMap<String, MutationExpectation>> {
        match self {
            FixtureMutation::Append { expect, .. }
            | FixtureMutation::SaveSnapshot { expect, .. } => expect,
        }
    }
}

/// Expected outcome of a single write on one store.
#[derive(Debug, Clone, Deserialize)]
struct MutationExpectation {
    /// `true` documents that the write is expected to succeed.
    #[serde(default)]
    ok: Option<bool>,
    /// Expected [`StoreError`] variant name when the write must fail
    /// (e.g. `"InvalidState"`). Takes precedence over `ok`.
    #[serde(default)]
    error: Option<String>,
}

// ===========================================================================
// Fixture loader
// ===========================================================================

/// Loads a JSON fixture by name (without extension) from `tests/fixtures/`
/// relative to the crate root.
///
/// # Panics
///
/// Panics if the file cannot be read or deserialised.
fn load_fixture(name: &str) -> Fixture {
    let path = {
        // `CARGO_MANIFEST_DIR` points to the crate root at compile time.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        format!("{manifest_dir}/tests/fixtures/{name}.json")
    };
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture '{name}' at {path}: {error}"));
    serde_json::from_str(&content)
        .unwrap_or_else(|error| panic!("failed to parse fixture '{name}': {error}"))
}

/// The names of the committed `.json` files in `tests/fixtures/`.
fn committed_fixture_names() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("failed to list tests/fixtures: {error}"))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

// ===========================================================================
// Replay driver
// ===========================================================================

/// The [`StoreError`] variant name, used to match a mutation's expected error.
fn store_error_variant_name(error: &StoreError) -> &'static str {
    match error {
        StoreError::NotFound(_) => "NotFound",
        StoreError::Serialization(_) => "Serialization",
        StoreError::Io(_) => "Io",
        StoreError::Backend(_) => "Backend",
        StoreError::InvalidState(_) => "InvalidState",
    }
}

/// Resolves the per-store expectation for one mutation.
///
/// A mutation without `expect` expects success on every store; a mutation
/// with `expect` must name the store being replayed, so a backend added
/// later cannot silently skip a recorded divergence.
fn expectation_for(
    store_name: &str,
    fixture_name: &str,
    mutation_index: usize,
    expect: &Option<HashMap<String, MutationExpectation>>,
) -> MutationExpectation {
    match expect {
        None => MutationExpectation {
            ok: Some(true),
            error: None,
        },
        Some(map) => map.get(store_name).cloned().unwrap_or_else(|| {
            panic!(
                "fixture '{fixture_name}' mutation {mutation_index} declares expectations \
                     but not for the '{store_name}' store — record it explicitly"
            )
        }),
    }
}

/// Asserts a write's outcome matches its recorded expectation.
fn check_write_result(
    store_name: &str,
    fixture_name: &str,
    mutation_index: usize,
    expectation: &MutationExpectation,
    result: Result<(), StoreError>,
) {
    match (expectation.error.as_deref(), result) {
        (Some(expected_variant), Ok(())) => panic!(
            "{store_name} replay of '{fixture_name}': mutation {mutation_index} succeeded \
             but was expected to fail with StoreError::{expected_variant}"
        ),
        (Some(expected_variant), Err(error)) => {
            let actual = store_error_variant_name(&error);
            assert_eq!(
                actual, expected_variant,
                "{store_name} replay of '{fixture_name}': mutation {mutation_index} failed \
                 with {error:?}, expected StoreError::{expected_variant}"
            );
        }
        (None, Ok(())) => {
            if expectation.ok == Some(false) {
                panic!(
                    "{store_name} replay of '{fixture_name}': mutation {mutation_index} was \
                     expected to fail (ok: false) but succeeded"
                );
            }
        }
        (None, Err(error)) => panic!(
            "{store_name} replay of '{fixture_name}': mutation {mutation_index} unexpectedly \
             failed: {error:?}"
        ),
    }
}

/// Structural equality for [`StoredSession`]s via their JSON projection.
///
/// `serde_json::Value` object keys are sorted, so the comparison is immune to
/// `HashMap` iteration order inside snapshot agent state and to JSON key-order
/// noise, while still asserting full field-by-field equality.
fn assert_stored_sessions_match(expected: &StoredSession, actual: &StoredSession) {
    let expected = serde_json::to_value(expected).expect("serialize expected stored session");
    let actual = serde_json::to_value(actual).expect("serialize loaded stored session");
    assert_eq!(
        expected, actual,
        "reconstructed StoredSession must match the fixture's golden record exactly"
    );
}

/// Replays one committed fixture against a live store.
async fn replay_fixture(store: &Arc<dyn SessionStore>, store_name: &str, fixture_name: &str) {
    let fixture = load_fixture(fixture_name);
    assert_eq!(
        fixture.name, fixture_name,
        "fixture 'name' field must match the file name"
    );

    for (index, mutation) in fixture.mutations.iter().enumerate() {
        let expectation = expectation_for(store_name, fixture_name, index, mutation.expect());
        let result = match mutation {
            FixtureMutation::Append { event, .. } => store.append(event.clone()).await,
            FixtureMutation::SaveSnapshot { snapshot, .. } => {
                store.save_snapshot(snapshot.clone()).await
            }
        };
        check_write_result(store_name, fixture_name, index, &expectation, result);
    }

    if let Some(expected) = &fixture.expected_session {
        let session_id: SessionId = expected.session_id;
        let loaded = store
            .load_session(session_id)
            .await
            .unwrap_or_else(|error| {
                panic!("{store_name} replay of '{fixture_name}': load_session failed: {error:?}")
            });
        assert_stored_sessions_match(expected, &loaded);
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// Enforces the fixture convention: every `.json` file committed under
/// `tests/fixtures/` is registered in [`FIXTURE_NAMES`], and every registered
/// name has a committed file. A fixture that is never replayed (or a name
/// that points at nothing) fails loudly here.
#[test]
fn every_committed_fixture_is_registered_and_replayed() {
    let mut registered: Vec<&str> = FIXTURE_NAMES.to_vec();
    registered.sort_unstable();
    assert_eq!(
        committed_fixture_names(),
        registered,
        "tests/fixtures/ must contain exactly the fixtures registered in FIXTURE_NAMES \
         (add the fixture file and its name together)"
    );
}

/// Every committed fixture is replayed against [`SqliteSessionStore`].
#[tokio::test]
async fn sqlite_store_replays_every_committed_fixture() {
    for name in FIXTURE_NAMES {
        let store: Arc<dyn SessionStore> = Arc::new(
            SqliteSessionStore::open(temp_db(&format!("sqlite-replay-{name}")))
                .expect("open sqlite store"),
        );
        replay_fixture(&store, STORE_SQLITE, name).await;
    }
}

/// Every committed fixture is replayed against [`JsonlSessionStore`].
#[tokio::test]
async fn jsonl_store_replays_every_committed_fixture() {
    for name in FIXTURE_NAMES {
        let store: Arc<dyn SessionStore> = Arc::new(JsonlSessionStore::new(temp_dir(&format!(
            "jsonl-replay-{name}"
        ))));
        replay_fixture(&store, STORE_JSONL, name).await;
    }
}

/// RC-305: the committed `migrate-v0-to-v1` fixture's legacy checkpoint
/// (schema version 0, no metadata) upgrades to the current schema version
/// through `migrate_snapshot` — the committed backward-migration proof.
#[test]
fn legacy_fixture_snapshot_migrates_to_current_version() {
    let fixture = load_fixture("migrate-v0-to-v1");
    let expected = fixture
        .expected_session
        .expect("fixture carries an expected session");
    let snapshot = expected.snapshot.expect("fixture carries a snapshot");
    assert_eq!(
        snapshot.schema_version, 0,
        "the legacy fixture's snapshot is version 0"
    );

    let migrated = migrate_snapshot(snapshot).expect("v0 migrates to the current version");
    assert_eq!(
        migrated.schema_version, SCHEMA_VERSION,
        "migration stamps the current schema version"
    );
    assert_eq!(
        migrated.session_sequence, 1,
        "migration preserves the checkpoint point"
    );
    assert_eq!(
        migrated.root_agent_id.to_string(),
        "22222222-2222-2222-2222-222222222222",
        "migration preserves identity fields"
    );
}

// ===========================================================================
// Scratch-path helpers
// ===========================================================================

/// Creates a unique scratch directory for one test run and returns the
/// SQLite database path inside it.
fn temp_db(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "harness-session-store-replay-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp store dir");
    dir.join("store.db")
}

/// Creates a unique scratch directory for one test run.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "harness-session-store-replay-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp store dir");
    dir
}
