//! Turning SQLite failures into [`StoreError`]s.

use rusqlite::ffi::{
    SQLITE_CONSTRAINT_FOREIGNKEY, SQLITE_CONSTRAINT_PRIMARYKEY, SQLITE_CONSTRAINT_UNIQUE,
};
use rusty_protocol::ids::Timestamp;

use crate::store::StoreError;

/// A constraint violation means the caller sent inconsistent data (a duplicate sequence, an
/// unknown session); any other failure is the backend's.
pub(super) fn map_sqlite_error(operation: &str, error: rusqlite::Error) -> StoreError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error {
        let violates_constraint = matches!(
            code.extended_code,
            SQLITE_CONSTRAINT_FOREIGNKEY | SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE
        );
        if violates_constraint {
            return StoreError::InvalidState(format!("{operation}: {error}"));
        }
    }
    StoreError::Backend(format!("{operation}: {error}"))
}

pub(super) fn now_ms() -> i64 {
    Timestamp::now().timestamp_millis()
}
