//! Host dependency resolution for restore (RC-304).
//!
//! A snapshot records **references** — workspace identity, integration/model
//! references, credential profiles, tool-policy IDs, audit sinks — never the
//! secrets or live bindings themselves. At restore time the embedding host
//! resolves those references against its current, authorized state. This
//! module defines:
//!
//! - [`DependencyKind`] — the kinds of dependency a snapshot can reference.
//! - [`MissingDependency`] / [`DependencyResolution`] — typed outcomes.
//! - [`HostDependencyResolver`] — the host-implemented trait that produces a
//!   [`RestoreReport`].
//! - [`assess_restore`] — applies a [`RestorePolicy`] to a report, so a
//!   strict restore **fails** instead of silently substituting a fake
//!   workspace, empty tools, or missing credentials.
//!
//! # Invariants
//!
//! - Secrets never enter snapshots ([`DurableSessionMetadata`](crate::store::DurableSessionMetadata)
//!   contains references only).
//! - Missing dependencies produce distinct errors — never a silent fallback.
//! - Restore never substitutes a fake workspace, empty tools, or missing
//!   credentials unless the host explicitly opts into
//!   [`RestorePolicy::PermitMissing`] (and even then the report documents
//!   exactly what was missing).

use harness_protocol::ids::SessionId;

use crate::store::DurableSessionMetadata;

/// The kind of host dependency a snapshot can reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// The durable workspace identity → a currently authorized binding.
    Workspace,
    /// An integration/model reference → an available provider backend.
    Integration,
    /// A credential-profile reference → current credentials.
    CredentialProfile,
    /// A tool/plugin ID → a registry entry and policy.
    ToolPolicy,
    /// An audit/external event sink reference.
    AuditSink,
}

use serde::{Deserialize, Serialize};

/// A dependency referenced by a snapshot that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingDependency {
    /// Which kind of dependency is missing.
    pub kind: DependencyKind,
    /// The reference ID recorded in the snapshot.
    pub id: String,
}

/// The outcome of resolving one snapshot reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyResolution {
    /// The reference was resolved to a live, authorized binding.
    Resolved {
        /// The kind of dependency.
        kind: DependencyKind,
        /// The reference ID.
        id: String,
    },
    /// The reference could not be resolved.
    Missing(MissingDependency),
}

/// The full result of resolving every reference in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    /// The session being restored.
    pub session_id: SessionId,
    /// Every reference the host resolved.
    pub resolved: Vec<DependencyResolution>,
    /// Every reference the host could not resolve.
    pub missing: Vec<MissingDependency>,
}

impl RestoreReport {
    /// Builds an empty report for `session_id`.
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            resolved: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// Records a resolved dependency.
    pub fn resolved(&mut self, kind: DependencyKind, id: impl Into<String>) {
        self.resolved.push(DependencyResolution::Resolved {
            kind,
            id: id.into(),
        });
    }

    /// Records a missing dependency.
    pub fn missing(&mut self, kind: DependencyKind, id: impl Into<String>) {
        let dependency = MissingDependency {
            kind,
            id: id.into(),
        };
        self.missing.push(dependency.clone());
        self.resolved.push(DependencyResolution::Missing(dependency));
    }

    /// `true` when every recorded reference resolved.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

/// How a restore reacts to unresolved dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestorePolicy {
    /// Any missing dependency rejects the restore with a typed error.
    #[default]
    RejectMissing,
    /// The restore proceeds, but the report documents every missing
    /// dependency explicitly (the host acknowledges the substitution).
    PermitMissing,
}

/// Typed error raised when a strict restore rejects a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreError {
    /// One or more dependencies could not be resolved and the policy rejects
    /// the restore.
    #[error("restore rejected: {count} missing dependencies: {missing:?}")]
    MissingDependencies {
        /// The missing dependencies.
        missing: Vec<MissingDependency>,
        /// How many were missing.
        count: usize,
    },
}

/// The host-implemented resolver (RC-304).
///
/// Implementations must be **truthful**: resolve a reference only when a
/// currently authorized binding exists, and report [`Missing`](DependencyResolution::Missing)
/// otherwise. `resolve` performs host-side I/O if needed but must never
/// mutate the store or the snapshot.
#[async_trait::async_trait]
pub trait HostDependencyResolver: Send + Sync {
    /// Resolves every reference recorded in `metadata`, returning the report.
    async fn resolve(&self, session_id: SessionId, metadata: &DurableSessionMetadata)
        -> RestoreReport;
}

/// Applies `policy` to `report`, returning the report or a typed rejection.
pub fn assess_restore(report: &RestoreReport, policy: RestorePolicy) -> Result<(), RestoreError> {
    match policy {
        RestorePolicy::PermitMissing => Ok(()),
        RestorePolicy::RejectMissing if report.is_complete() => Ok(()),
        RestorePolicy::RejectMissing => Err(RestoreError::MissingDependencies {
            missing: report.missing.clone(),
            count: report.missing.len(),
        }),
    }
}

/// A resolver that resolves nothing and reports every reference as missing.
///
/// Useful as an explicit baseline (and for tests): a snapshot that recorded
/// dependency references can never be silently restored through this
/// resolver — every reference surfaces in the report.
#[derive(Debug, Clone, Copy, Default)]
pub struct PermissiveResolver;

#[async_trait::async_trait]
impl HostDependencyResolver for PermissiveResolver {
    async fn resolve(
        &self,
        session_id: SessionId,
        metadata: &DurableSessionMetadata,
    ) -> RestoreReport {
        let mut report = RestoreReport::new(session_id);
        if let Some(workspace) = &metadata.workspace_identity {
            report.missing(DependencyKind::Workspace, workspace);
        }
        for integration in &metadata.integration_references {
            report.missing(DependencyKind::Integration, integration);
        }
        for profile in &metadata.credential_profiles {
            report.missing(DependencyKind::CredentialProfile, profile);
        }
        for tool_policy in &metadata.tool_policy_ids {
            report.missing(DependencyKind::ToolPolicy, tool_policy);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_metadata_resolves_completely() {
        let report = PermissiveResolver
            .resolve(SessionId::new(), &DurableSessionMetadata::default())
            .await;
        assert!(report.is_complete());
        assert!(assess_restore(&report, RestorePolicy::RejectMissing).is_ok());
    }

    #[tokio::test]
    async fn missing_workspace_is_typed_not_silent() {
        let metadata = DurableSessionMetadata {
            workspace_identity: Some("/srv/app".into()),
            ..Default::default()
        };
        let report = PermissiveResolver.resolve(SessionId::new(), &metadata).await;
        assert!(!report.is_complete());
        let missing = &report.missing[0];
        assert_eq!(missing.kind, DependencyKind::Workspace);
        assert_eq!(missing.id, "/srv/app");

        let error = assess_restore(&report, RestorePolicy::RejectMissing).expect_err("rejected");
        assert!(matches!(error, RestoreError::MissingDependencies { .. }));
    }

    #[tokio::test]
    async fn permit_missing_returns_the_report() {
        let metadata = DurableSessionMetadata {
            workspace_identity: Some("/srv/app".into()),
            ..Default::default()
        };
        let report = PermissiveResolver.resolve(SessionId::new(), &metadata).await;
        assert!(assess_restore(&report, RestorePolicy::PermitMissing).is_ok());
    }
}
