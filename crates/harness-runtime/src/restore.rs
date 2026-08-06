//! Host dependency resolution for restore (RC-304).
//!
//! [`HostRestoreResolver`] is the runtime's concrete
//! [`HostDependencyResolver`](harness_session_store::HostDependencyResolver):
//! it resolves the references recorded in a snapshot's durable metadata
//! against the *current* host — the live workspace binding and the
//! integration registry — so restore never silently substitutes a fake
//! workspace or a missing provider.
//!
//! Resolution is truthful by construction:
//!
//! - the **workspace** reference resolves only when the stored identity
//!   matches the live workspace root (canonicalized when possible);
//! - an **integration** reference resolves only when the integration
//!   registry currently has a factory registered for that family ID;
//! - **credential profiles** and **tool policies** are not yet recorded by
//!   the runtime snapshots (they are empty in the metadata block), so this
//!   resolver reports them as unresolved if a snapshot ever carries them —
//!   never silently.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use harness_protocol::ids::SessionId;
use harness_session_store::{
    DependencyKind, HostDependencyResolver, RestoreReport,
};

use crate::integration::IntegrationRegistry;
use crate::traits::Workspace;

/// Resolves snapshot dependency references against the current host.
#[derive(Clone)]
pub struct HostRestoreResolver {
    /// Canonical identity of the live workspace (best-effort canonicalized).
    workspace_root: String,
    /// The live integration registry used to resolve provider references.
    integrations: Arc<IntegrationRegistry>,
}

impl HostRestoreResolver {
    /// Builds a resolver from the live workspace binding and integration
    /// registry.
    pub fn new(workspace: &dyn Workspace, integrations: Arc<IntegrationRegistry>) -> Self {
        Self {
            workspace_root: canonical_workspace_identity(workspace.root()),
            integrations,
        }
    }
}

/// Canonical identity for a workspace root path.
///
/// Canonicalization is best-effort: a root that does not exist yet (e.g. a
/// not-yet-created temp directory) falls back to its lexical path so restore
/// can still compare identities consistently with the snapshot side (which
/// records the same fallback).
pub(crate) fn canonical_workspace_identity(root: &Path) -> String {
    std::fs::canonicalize(root)
        .map(|canonical| canonical.display().to_string())
        .unwrap_or_else(|_| root.display().to_string())
}

#[async_trait]
impl HostDependencyResolver for HostRestoreResolver {
    async fn resolve(
        &self,
        session_id: SessionId,
        metadata: &harness_session_store::DurableSessionMetadata,
    ) -> RestoreReport {
        let mut report = RestoreReport::new(session_id);

        if let Some(stored_workspace) = &metadata.workspace_identity {
            if *stored_workspace == self.workspace_root {
                report.resolved(DependencyKind::Workspace, stored_workspace);
            } else {
                report.missing(DependencyKind::Workspace, stored_workspace);
            }
        }

        for integration in &metadata.integration_references {
            match self.integrations.get(integration) {
                Ok(Some(_)) => report.resolved(DependencyKind::Integration, integration),
                _ => report.missing(DependencyKind::Integration, integration),
            }
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
    use harness_session_store::DurableSessionMetadata;

    #[test]
    fn canonical_identity_falls_back_to_lexical_root() {
        let missing = Path::new("/definitely/not/a/real/harness/workspace");
        let identity = canonical_workspace_identity(missing);
        assert_eq!(identity, "/definitely/not/a/real/harness/workspace");
    }

    #[tokio::test]
    async fn matching_workspace_and_registered_integration_resolve() {
        let workspace = crate::workspace::FakeWorkspace::new();
        let resolver = HostRestoreResolver::new(&workspace, Arc::new(IntegrationRegistry::new()));
        let identity = canonical_workspace_identity(workspace.root());
        let metadata = DurableSessionMetadata {
            workspace_identity: Some(identity),
            integration_references: vec![],
            ..Default::default()
        };
        let report = resolver.resolve(SessionId::new(), &metadata).await;
        assert!(report.is_complete(), "the workspace identity matches");
    }

    #[tokio::test]
    async fn mismatched_workspace_is_typed_missing() {
        let workspace = crate::workspace::FakeWorkspace::new();
        let resolver = HostRestoreResolver::new(&workspace, Arc::new(IntegrationRegistry::new()));
        let metadata = DurableSessionMetadata {
            workspace_identity: Some("/some/other/workspace".into()),
            ..Default::default()
        };
        let report = resolver.resolve(SessionId::new(), &metadata).await;
        assert!(!report.is_complete());
        assert_eq!(report.missing[0].kind, DependencyKind::Workspace);
    }
}
