#![warn(clippy::all)]

//! # `rusty-harness-sdk`
//!
//! Stable, application-agnostic Rust facade over the Rusty agent harness
//! engine (`harness-engine`).
//!
//! This crate exists so Rust applications embedding the harness depend on
//! one small, documented surface instead of the internal `harness-*`
//! workspace crates, whose APIs may change between workspace versions.
//! Non-Rust applications, or applications that want the engine out of
//! process, should instead run `harnessd` and use a wire-protocol client
//! (see `sdk/typescript`) — both integration modes expose the same session,
//! event, and permission semantics.
//!
//! ## Quick start
//!
//! ```no_run
//! use rusty_harness_sdk::{Client, Session};
//!
//! # async fn run() -> Result<(), rusty_harness_sdk::SdkError> {
//! let client = Client::builder().build().await?;
//!
//! let handle = client
//!     .session()
//!     .integration("anthropic", serde_json::json!({}))?
//!     .start()
//!     .await?;
//! let session = Session::from(handle);
//!
//! let mut events = session.events();
//! session.send("hello").await?;
//! while let Some(event) = events.next().await {
//!     let _event = event?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! See `sdk/rust/examples/basic_chat.rs` for a runnable version and
//! `sdk_plan.md` for the full production roadmap and current gaps.

mod client;
mod error;
mod events;
mod session;

pub use client::{Client, ClientBuilder};
pub use error::SdkError;
pub use events::EventStream;
pub use session::Session;

/// Wire-protocol types shared with non-Rust SDKs and `harnessd` clients.
///
/// Re-exported here so SDK consumers can reference IDs, events, and command
/// payloads without depending on `harness-protocol` directly. See
/// `schema/protocol-v1.schema.json` for the language-neutral description of
/// the same shapes.
pub mod protocol {
    pub use harness_protocol::commands::{Attachment, PermissionDecision, UserInput};
    pub use harness_protocol::events::{
        AgentEvent, AgentEventEnvelope, AgentOutcome, EventVisibility,
    };
    pub use harness_protocol::ids::{
        AgentId, EventId, MessageId, PermissionId, RequestId, RunId, SessionId, Timestamp,
        ToolCallId,
    };
}

/// Provider, model, and credential discovery types.
///
/// Re-exported from `harness-engine` so applications can inspect available
/// providers/models/credentials through this crate alone.
pub mod providers {
    pub use harness_engine::{
        AdapterKind, AuthFlowHandle, AuthFlowState, AuthMethod, BackendSelection,
        CredentialProfileId, CredentialProfileSummary, CredentialState, CredentialStore,
        EnvironmentCredentialStore, ModelCapabilities, ModelDescriptor, ProviderDescriptor,
        ProviderHealth, ProviderKey, SecretString, SecureCredentialStore,
    };
}

/// Traits needed to author a custom integration (model backend) registered
/// via [`ClientBuilder::register_integration`].
pub mod integration {
    pub use harness_runtime::integration::IntegrationFactory;
}

/// Session persistence types needed to configure or implement a custom
/// store via [`ClientBuilder::session_store`].
pub mod store {
    pub use harness_session_store::jsonl::JsonlSessionStore;
    pub use harness_session_store::sqlite::SqliteSessionStore;
    pub use harness_session_store::store::{
        DurableSessionEvent, DurableSessionSnapshot, SessionStore, SessionSummary, StoreError,
        StoredSession,
    };
}
