//! Checking that the artifacts a step claims exist.

use std::sync::Arc;

use async_trait::async_trait;

use rusty_runtime::traits::{Workspace, WorkspaceError};

/// Decides whether a `{kind, reference}` artifact claimed by a model exists.
#[async_trait]
pub trait ArtifactResolver: Send + Sync {
    async fn resolve(&self, kind: &str, reference: &str) -> Result<(), String>;
}

/// Accepts any non-empty reference. Performs no I/O, so it cannot catch a
/// model claiming a file it never wrote; use [`WorkspaceArtifactResolver`]
/// when a workspace is available.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReferenceArtifactResolver;

#[async_trait]
impl ArtifactResolver for ReferenceArtifactResolver {
    async fn resolve(&self, _kind: &str, reference: &str) -> Result<(), String> {
        if reference.trim().is_empty() {
            Err("reference is empty".into())
        } else {
            Ok(())
        }
    }
}

/// Resolves `file`/`path` artifacts against the workspace; other kinds are
/// opaque references that only need to be non-empty.
pub struct WorkspaceArtifactResolver {
    workspace: Arc<dyn Workspace>,
}

impl WorkspaceArtifactResolver {
    pub fn new(workspace: Arc<dyn Workspace>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl ArtifactResolver for WorkspaceArtifactResolver {
    async fn resolve(&self, kind: &str, reference: &str) -> Result<(), String> {
        ReferenceArtifactResolver.resolve(kind, reference).await?;
        if !matches!(kind, "file" | "path") {
            return Ok(());
        }
        match self.workspace.read(reference).await {
            Ok(_) => Ok(()),
            // Present but not UTF-8 text: it exists.
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData => {
                Ok(())
            }
            Err(WorkspaceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("{reference} does not exist in the workspace"))
            }
            Err(error) => Err(format!("{reference} could not be resolved: {error}")),
        }
    }
}
