//! Embedded entry point for hosting the harness engine in-process.

use std::sync::Arc;

use harness_engine::{Harness, HarnessBuilder, SessionBuilder};
use harness_protocol::ids::SessionId;
use harness_runtime::integration::IntegrationFactory;
use harness_session_store::store::{SessionStore, SessionSummary};

use crate::error::SdkError;
use crate::session::Session;

/// Fluent builder for constructing an embedded [`Client`].
///
/// Mirrors [`harness_engine::HarnessBuilder`]'s registration order
/// guarantees, but returns SDK types so applications depend on this crate
/// instead of importing `harness-engine` directly.
pub struct ClientBuilder {
    inner: HarnessBuilder,
}

impl ClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            inner: HarnessBuilder::new(),
        }
    }

    /// Register a model backend integration factory (e.g. Anthropic,
    /// OpenAI, or a custom `IntegrationFactory`). Order matters: the first
    /// factory registered under a given integration id wins.
    pub fn register_integration(mut self, factory: Arc<dyn IntegrationFactory>) -> Self {
        self.inner = self.inner.register_integration(factory);
        self
    }

    /// Configure durable session persistence (`JsonlSessionStore`,
    /// `SqliteSessionStore`, or a custom [`SessionStore`]).
    ///
    /// If never called, the client falls back to a non-durable in-memory
    /// store and logs a warning — sessions will not survive a process
    /// restart. Production hosts should always configure a real store.
    pub fn session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.inner = self.inner.session_store(store);
        self
    }

    /// Finish construction.
    pub async fn build(self) -> Result<Client, SdkError> {
        let inner = self.inner.build().await?;
        Ok(Client { inner })
    }
}

/// Embedded entry point into the harness engine.
///
/// `Client` is the SDK-facing equivalent of [`harness_engine::Harness`]: it
/// composes provider/model discovery, session creation, and session restore
/// behind one stable type. Applications that embed the harness directly in
/// a Rust process should depend on `rusty-harness-sdk` rather than
/// `harness-engine` and its transitive internal crates.
///
/// Applications that are not Rust, or that want the engine in a separate
/// process, should instead run `harnessd` and speak the wire protocol
/// described in `schema/protocol-v2.schema.json` (see the TypeScript SDK
/// under `sdk/typescript` for a reference client).
pub struct Client {
    inner: Harness,
}

impl Client {
    /// Start building a new embedded client.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Begin building a new session against a registered integration.
    ///
    /// Returns the engine's native [`SessionBuilder`] directly — this SDK
    /// does not yet wrap session construction, only the resulting handle
    /// (via [`Session::from`]). See [`harness_engine::SessionBuilder`] for
    /// the full configuration surface (integration config, toolset,
    /// workspace, context provider).
    pub fn session(&self) -> SessionBuilder {
        self.inner.session()
    }

    /// List durable sessions known to the configured session store.
    ///
    /// Returns an empty list for the default non-durable store.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SdkError> {
        self.inner.list_sessions().await.map_err(Into::into)
    }

    /// Restore a previously persisted session and reattach a live handle.
    ///
    /// Restore currently rebuilds session state from the last snapshot;
    /// full trailing-event replay and real workspace/tool/credential
    /// dependency restoration are tracked in `sdk_plan.md`
    /// (SDK-401/SDK-402) and `upgrade_rusty.md` (RST-008/RST-009), and are
    /// not yet complete. Do not depend on restore reconstructing the exact
    /// pre-crash state until those items are closed.
    pub async fn restore_session(&self, id: SessionId) -> Result<Session, SdkError> {
        let handle = self.inner.restore_session(id).await?;
        Ok(Session::from(handle))
    }

    /// Direct access to the underlying engine, for capabilities this SDK
    /// does not yet wrap (provider/model discovery, credential profiles,
    /// auth flows). See [`harness_engine::Harness`].
    pub fn engine(&self) -> &Harness {
        &self.inner
    }
}
