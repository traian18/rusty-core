//! Top-level public harness entry point.

use std::{collections::HashMap, sync::{Arc, RwLock}};

use harness_protocol::events::AgentEventEnvelope;
use harness_protocol::ids::SessionId;
use harness_protocol::tools::AgentToolset;
use harness_runtime::scheduler::Scheduler;
use harness_runtime::session_manager::SessionManager;
use harness_runtime::traits::{EventSink, SimpleToolRegistry, ToolRegistry, Workspace};
use harness_runtime::workspace::FakeWorkspace;
use harness_runtime::{IntegrationError, IntegrationFactory, IntegrationRegistry};
use harness_session_store::{SessionStore, SessionSummary};

use crate::builder::NoopSessionStore;
use crate::session_builder::{HarnessError, SessionBuilder, SessionHandle};
use crate::providers::{self, AuthFlowHandle, AuthFlowState, AuthMethod, BackendSelection, CredentialProfileId, CredentialProfileSummary, CredentialState, ModelDescriptor, ProviderDescriptor, ProviderHealth, ProviderKey};

/// Public entry point for registering integrations and creating sessions.
pub struct Harness {
    pub(crate) integrations: Arc<IntegrationRegistry>,
    pub(crate) sessions: Arc<SessionManager>,
    pub(crate) session_store: Arc<dyn SessionStore>,
    pub(crate) model_cache: Arc<RwLock<HashMap<ProviderKey, Vec<ModelDescriptor>>>>,
}

impl Harness {
    /// List every registered provider through a storage-neutral engine model.
    pub fn list_providers(&self) -> Result<Vec<ProviderDescriptor>, HarnessError> {
        Ok(self.integrations.list()?.into_iter().map(|(id, descriptor)| providers::descriptor_for(&id, descriptor.capabilities)).collect())
    }

    pub fn list_credential_profiles(&self, provider: &ProviderKey) -> Result<Vec<CredentialProfileSummary>, HarnessError> {
        let descriptor = self.list_providers()?.into_iter().find(|item| &item.id == provider)
            .ok_or_else(|| HarnessError::UnknownProvider(provider.to_string()))?;
        let auth_method = descriptor.auth_methods[0];
        let state = match provider.as_str() {
            "anthropic-api" => if std::env::var_os("ANTHROPIC_API_KEY").is_some() { CredentialState::Available } else { CredentialState::Missing },
            "openai-api" => if std::env::var_os("OPENAI_API_KEY").is_some() { CredentialState::Available } else { CredentialState::Missing },
            _ => CredentialState::ManagedExternally,
        };
        Ok(vec![CredentialProfileSummary { id: CredentialProfileId::new(format!("{}:default", provider.as_str())), provider: provider.clone(), label: descriptor.credential_hint, state, auth_method }])
    }

    pub fn begin_auth(&self, provider: &ProviderKey, method: AuthMethod) -> Result<AuthFlowHandle, HarnessError> {
        let profile = self.list_credential_profiles(provider)?.into_iter().next().ok_or_else(|| HarnessError::UnknownProvider(provider.to_string()))?;
        let next = match method {
            AuthMethod::Environment if profile.state == CredentialState::Available => AuthFlowState::Connected { profile },
            AuthMethod::Environment => AuthFlowState::Failed { safe_message: "Set the provider API-key environment variable and refresh".into() },
            AuthMethod::CliManaged => {
                let (program, args) = match provider.as_str() {
                    "github-copilot" => ("copilot", vec!["login"]),
                    "codex" => ("codex", vec!["login"]),
                    "claude-code" => ("claude", vec![]),
                    _ => return Err(HarnessError::UnknownProvider(provider.to_string())),
                };
                AuthFlowState::WaitingForExternalCommand { program: program.into(), args: args.into_iter().map(str::to_owned).collect() }
            }
        };
        Ok(AuthFlowHandle { provider: provider.clone(), states: vec![AuthFlowState::Starting, next] })
    }

    pub async fn list_models(&self, provider: &ProviderKey, _credential: &CredentialProfileId, refresh: bool) -> Result<Vec<ModelDescriptor>, HarnessError> {
        if !self.list_providers()?.iter().any(|item| &item.id == provider) {
            return Err(HarnessError::UnknownProvider(provider.to_string()));
        }
        if !refresh {
            if let Some(models) = self.model_cache.read().map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?.get(provider).cloned() {
                return Ok(models);
            }
            return Ok(providers::default_models(provider));
        }
        match providers::discover_api_models(provider).await {
            Ok(models) if !models.is_empty() => {
                self.model_cache.write().map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
                    .insert(provider.clone(), models.clone());
                Ok(models)
            }
            Ok(_) => Ok(providers::default_models(provider)),
            Err(_) => {
                let mut fallback = self.model_cache.read().map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
                    .get(provider).cloned().unwrap_or_else(|| providers::default_models(provider));
                for model in &mut fallback { model.stale = true; }
                Ok(fallback)
            }
        }
    }

    /// Validates a model override against this provider's known catalog
    /// before it's applied via `SessionBuilder::execution_params` /
    /// `SessionHandle::set_execution_params`.
    ///
    /// Best-effort, not exhaustive: it checks the cached catalog from the
    /// last `list_models(refresh: true)` call, falling back to the static
    /// `default_models()` list when nothing has been cached yet. A model
    /// that's real but simply hasn't been discovered/cached yet can produce
    /// a false rejection here — callers that need certainty should
    /// `list_models(provider, credential, true)` first. This exists to catch
    /// the common case (a typo'd or stale model id) before it reaches a
    /// provider and fails deep inside a run.
    pub fn validate_model_override(
        &self,
        provider: &ProviderKey,
        provider_model_id: &str,
    ) -> Result<(), HarnessError> {
        let cached = self
            .model_cache
            .read()
            .map_err(|_| HarnessError::ProviderCatalog("model cache lock poisoned".into()))?
            .get(provider)
            .cloned();
        let catalog = cached.unwrap_or_else(|| providers::default_models(provider));
        if catalog
            .iter()
            .any(|model| model.provider_model_id == provider_model_id)
        {
            return Ok(());
        }
        Err(HarnessError::UnknownModel {
            provider: provider.to_string(),
            model_id: provider_model_id.to_string(),
        })
    }

    pub fn provider_health(&self, provider: &ProviderKey) -> Result<ProviderHealth, HarnessError> {
        let profile = self.list_credential_profiles(provider)?.remove(0);
        let program = match provider.as_str() {
            "claude-code" => Some("claude"),
            "codex" => Some("codex"),
            "github-copilot" => Some("copilot"),
            _ => None,
        };
        let executable = program.and_then(providers::find_executable);
        let ready = profile.state != CredentialState::Missing && (program.is_none() || executable.is_some());
        let message = if profile.state == CredentialState::Missing {
            "Credential unavailable".into()
        } else if program.is_some() && executable.is_none() {
            format!("{} executable was not found on PATH", program.unwrap_or("provider"))
        } else {
            "Ready".into()
        };
        Ok(ProviderHealth { provider: provider.clone(), credential: profile.state, executable, ready, message })
    }

    pub fn session_from_selection(&self, selection: &BackendSelection) -> Result<SessionBuilder, HarnessError> {
        let descriptor = self.list_providers()?.into_iter().find(|item| item.id == selection.provider)
            .ok_or_else(|| HarnessError::UnknownProvider(selection.provider.to_string()))?;
        let mut config = match descriptor.integration.as_str() {
            "anthropic" | "openai" => serde_json::json!({"default_model": selection.provider_model_id.clone()}),
            "claude-code" | "codex" | "github-copilot" if selection.provider_model_id == "default" => serde_json::json!({}),
            "claude-code" | "codex" => serde_json::json!({"extra_args": ["--model", selection.provider_model_id.clone()]}),
            "github-copilot" => serde_json::json!({"model": selection.provider_model_id.clone()}),
            _ => serde_json::json!({}),
        };
        if let Some(object) = config.as_object_mut() {
            object.insert("_backend_selection".into(), serde_json::to_value(
                harness_protocol::backend::PersistedBackendSelection::v1(
                    selection.provider.to_string(),
                    selection.credential_profile.0.clone(),
                    selection.provider_model_id.clone(),
                )
            )?);
        }
        self.session().integration(descriptor.integration, config)
    }
    /// Create a harness with an empty integration registry, a fresh
    /// [`SessionManager`], and the in-memory no-op [`SessionStore`].
    ///
    /// This mirrors the composition produced by the default
    /// [`HarnessBuilder`](crate::HarnessBuilder): no integrations, no durable
    /// persistence, and session restore routed through the no-op store (which
    /// reports every session as not found). Prefer
    /// [`Harness::builder()`](Self::builder) — and configure a real
    /// [`SessionStore`] via `.session_store(...)` — when persistence and
    /// restore are required.
    pub fn new() -> Self {
        let store: Arc<dyn SessionStore> = Arc::new(NoopSessionStore);
        Self {
            integrations: Arc::new(IntegrationRegistry::new()),
            sessions: Arc::new(SessionManager::new_with_store(
                Arc::new(Scheduler::new(Default::default())),
                Some(store.clone()),
            )),
            session_store: store,
            model_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a dynamically constructible integration family.
    pub fn register_integration(
        &self,
        factory: Arc<dyn IntegrationFactory>,
    ) -> Result<(), IntegrationError> {
        self.integrations.register(factory)
    }

    /// Return a handle to the shared [`SessionManager`].
    ///
    /// The returned `Arc` can be used to query active sessions, close
    /// sessions, or restore persisted sessions without going through a
    /// [`SessionBuilder`].
    pub fn session_manager(&self) -> Arc<SessionManager> {
        self.sessions.clone()
    }

    /// Return the harness's shared [`SessionStore`].
    ///
    /// Always populated: the in-memory no-op store when no durable store was
    /// configured (see [`Harness::new`] and
    /// [`HarnessBuilder`](crate::HarnessBuilder)), or the store configured
    /// via `.session_store(...)`.
    pub fn session_store(&self) -> Arc<dyn SessionStore> {
        self.session_store.clone()
    }

    /// Begin building a session using this harness's integration registry
    /// and session manager.
    pub fn session(&self) -> SessionBuilder {
        SessionBuilder::with_integrations_and_manager(
            self.integrations.clone(),
            self.sessions.clone(),
        )
    }

    /// Lists durable sessions newest-first for frontend discovery.
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, HarnessError> {
        Ok(self.session_store.list_sessions().await?)
    }

    /// Restores a session with the host's real workspace and tool policy.
    ///
    /// RC-304: restore is **strict** — the snapshot's recorded workspace
    /// identity and integration references are validated against the live
    /// `workspace` and the harness's integration registry, and a mismatch or
    /// missing provider rejects the restore instead of silently substituting
    /// a fake.
    pub async fn restore_session_with_toolset(
        &self,
        id: SessionId,
        toolset: AgentToolset,
        workspace: Arc<dyn Workspace>,
    ) -> Result<SessionHandle, HarnessError> {
        let registry = SimpleToolRegistry::new();
        for descriptor in toolset.enabled_descriptors() {
            let executor = SessionBuilder::build_executor_for(descriptor, workspace.clone());
            let _ = registry.register(executor);
        }
        let tool_registry: Arc<dyn ToolRegistry> = Arc::new(registry);
        let runtime = self
            .sessions
            .restore_session(
                id,
                self.integrations.clone(),
                tool_registry,
                workspace,
                Arc::new(NoopEventSink),
            )
            .await?;
        Ok(SessionHandle::from_runtime(runtime, self.sessions.clone()))
    }

    /// Restore a previously persisted session, returning a live
    /// [`SessionHandle`].
    ///
    /// The harness's integration registry re-creates the stored agents'
    /// execution backends, and the configured [`SessionStore`] supplies the
    /// durable snapshot/event history.
    ///
    /// # RC-300 restore contract
    ///
    /// Restore is **strict** (RC-304): the snapshot's recorded workspace
    /// identity and integration references are validated against the current
    /// host before any backend is created, and a missing dependency rejects
    /// the restore. This method binds an in-memory fake workspace, so it only
    /// succeeds when the snapshot recorded no workspace binding or one that
    /// matches the fake — for sessions created against a real workspace use
    /// [`restore_session_with_toolset`](Self::restore_session_with_toolset),
    /// which validates against the host's actual workspace.
    ///
    /// # Errors
    ///
    /// Returns [`HarnessError::SessionManager`] when no store is configured,
    /// the session is not found, replay validation fails, dependency
    /// resolution rejects the restore, the stored session has no snapshot
    /// checkpoint, or a stored backend cannot be re-created.
    pub async fn restore_session(&self, id: SessionId) -> Result<SessionHandle, HarnessError> {
        let runtime = self
            .sessions
            .restore_session(
                id,
                self.integrations.clone(),
                Arc::new(SimpleToolRegistry::new()),
                Arc::new(FakeWorkspace::new()),
                Arc::new(NoopEventSink),
            )
            .await?;
        Ok(SessionHandle::from_runtime(runtime, self.sessions.clone()))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// No-op [`EventSink`] used when restoring a session through the harness.
///
/// The restored runtime still publishes events to its session-internal event
/// bus (observable via [`SessionHandle::subscribe`]); this sink only covers
/// the external forwarding path, which the harness does not configure.
struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn send(&self, _envelope: AgentEventEnvelope) {}
}
