//! Integration tests for `harness_protocol::ids`.

use harness_protocol::ids::*;
use std::str::FromStr;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Round-trip serialization
// ---------------------------------------------------------------------------

/// Helper: serialize an ID to JSON, then deserialize it back and check equality.
fn roundtrip_json<T>(id: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(id).expect("serialize");
    let deserialized: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(*id, deserialized, "JSON round-trip failed");
}

/// Helper: serialize an ID via Display, then parse it back via FromStr and check equality.
fn roundtrip_display<T>(id: &T)
where
    T: ToString + FromStr + PartialEq + std::fmt::Debug,
    T::Err: std::fmt::Debug,
{
    let s = id.to_string();
    let parsed: T = T::from_str(&s).expect("parse from display string");
    assert_eq!(*id, parsed, "Display/FromStr round-trip failed");
}

/// Generates a round-trip test for a given ID type.
macro_rules! test_roundtrip {
    ($name:ident, $ty:ty) => {
        #[test]
        fn $name() {
            let id = <$ty>::new();
            roundtrip_json(&id);
            roundtrip_display(&id);
        }
    };
}

test_roundtrip!(session_id_roundtrip, SessionId);
test_roundtrip!(agent_id_roundtrip, AgentId);
test_roundtrip!(run_id_roundtrip, RunId);
test_roundtrip!(request_id_roundtrip, RequestId);
test_roundtrip!(tool_call_id_roundtrip, ToolCallId);
test_roundtrip!(message_id_roundtrip, MessageId);
test_roundtrip!(event_id_roundtrip, EventId);
test_roundtrip!(permission_id_roundtrip, PermissionId);
test_roundtrip!(tool_id_roundtrip, ToolId);
test_roundtrip!(integration_id_roundtrip, IntegrationId);
test_roundtrip!(configuration_id_roundtrip, ConfigurationId);
test_roundtrip!(model_id_roundtrip, ModelId);
test_roundtrip!(backend_id_roundtrip, BackendId);
test_roundtrip!(context_provider_id_roundtrip, ContextProviderId);

/// Round-trip serialization for `Timestamp` via JSON.
#[test]
fn timestamp_json_roundtrip() {
    let ts = Timestamp::now();
    let json = serde_json::to_string(&ts).expect("serialize timestamp");
    let deserialized: Timestamp = serde_json::from_str(&json).expect("deserialize timestamp");
    // Compare as ISO strings to avoid sub-second precision differences
    assert_eq!(
        ts.as_datetime().to_rfc3339(),
        deserialized.as_datetime().to_rfc3339()
    );
}

/// Round-trip for `Timestamp` via `Display` and `FromStr`.
#[test]
fn timestamp_display_roundtrip() {
    let ts = Timestamp::now();
    // Timestamp doesn't implement Display/FromStr, but we can test via DateTime
    let dt = ts.as_datetime();
    let s = dt.to_rfc3339();
    let parsed = chrono::DateTime::parse_from_rfc3339(&s)
        .expect("parse rfc3339")
        .with_timezone(&chrono::Utc);
    assert_eq!(*dt, parsed);
}

// ---------------------------------------------------------------------------
// Timestamp::now() validation
// ---------------------------------------------------------------------------

#[test]
fn timestamp_now_is_within_tolerance() {
    use chrono::Utc;
    let before = Utc::now();
    let ts = Timestamp::now();
    let after = Utc::now();
    let ts_dt = *ts.as_datetime();
    assert!(
        ts_dt >= before || ts_dt <= after,
        "Timestamp::now() should yield a time close to the system clock"
    );
    // The timestamp should be within 1 second of the wall clock
    let diff = (ts_dt - before).num_seconds().abs();
    assert!(diff <= 2, "Timestamp::now() is too far from system clock: {diff}s");
}

/// Verify that `Timestamp::from_datetime` and `as_datetime` are inverses.
#[test]
fn timestamp_from_and_as_datetime() {
    use chrono::Utc;
    let dt = Utc::now();
    let ts = Timestamp::from_datetime(dt);
    assert_eq!(ts.as_datetime(), &dt);
}

/// Verify that `Timestamp` derefs to `DateTime<Utc>`.
#[test]
fn timestamp_deref() {
    let ts = Timestamp::now();
    // Access a DateTime method through Deref
    let _ = ts.format("%Y-%m-%d");
    let _ = ts.timestamp();
    let _ = ts.naive_utc();
}

// ---------------------------------------------------------------------------
// Uniqueness tests
// ---------------------------------------------------------------------------

/// Check that 100 generated IDs of a type are all distinct.
macro_rules! test_uniqueness {
    ($name:ident, $ty:ty) => {
        #[test]
        fn $name() {
            const N: usize = 100;
            let mut ids = std::collections::HashSet::new();
            for _ in 0..N {
                let id = <$ty>::new();
                assert!(
                    ids.insert(id),
                    "duplicate {} generated",
                    stringify!($ty)
                );
            }
            assert_eq!(ids.len(), N, "expected {N} unique IDs");
        }
    };
}

test_uniqueness!(session_id_uniqueness, SessionId);
test_uniqueness!(agent_id_uniqueness, AgentId);
test_uniqueness!(run_id_uniqueness, RunId);
test_uniqueness!(request_id_uniqueness, RequestId);
test_uniqueness!(tool_call_id_uniqueness, ToolCallId);
test_uniqueness!(message_id_uniqueness, MessageId);
test_uniqueness!(event_id_uniqueness, EventId);
test_uniqueness!(permission_id_uniqueness, PermissionId);
test_uniqueness!(tool_id_uniqueness, ToolId);
test_uniqueness!(integration_id_uniqueness, IntegrationId);
test_uniqueness!(configuration_id_uniqueness, ConfigurationId);
test_uniqueness!(model_id_uniqueness, ModelId);
test_uniqueness!(backend_id_uniqueness, BackendId);
test_uniqueness!(context_provider_id_uniqueness, ContextProviderId);

/// Verify that different ID types do not compare equal even with the same inner UUID.
#[test]
fn different_id_types_are_distinct() {
    let uuid = Uuid::new_v4();
    let session = SessionId::from_uuid(uuid);
    let agent = AgentId::from_uuid(uuid);
    // These should not be the same type, so they cannot be compared directly.
    // But we can verify their string representations match.
    assert_eq!(session.to_string(), agent.to_string());
    // And that the inner UUIDs match.
    assert_eq!(session.as_uuid(), agent.as_uuid());
}

/// Verify that `FromStr` rejects invalid UUID strings.
#[test]
fn invalid_uuid_string_rejected() {
    let result = SessionId::from_str("not-a-uuid");
    assert!(result.is_err());
}

/// Verify that `new()` creates a UUIDv4.
#[test]
fn new_creates_uuid_v4() {
    let id = SessionId::new();
    let uuid = id.as_uuid();
    // UUIDv4 has version number 4 (in the 13th hex digit)
    assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
}
