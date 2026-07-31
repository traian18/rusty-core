#![warn(clippy::all)]

//! Persistence interfaces and session/event restoration contracts.

mod store;
mod sqlite;
mod jsonl;
pub use store::{
    is_durable, DurableSessionEvent, DurableSessionSnapshot, SessionStore, StoredAgentState,
    StoredPendingToolCall, StoredSession, StoreError,
};
pub use sqlite::SqliteSessionStore;
pub use jsonl::JsonlSessionStore;
