//! Stable identities and the root-relative path type shared by every repository model.
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::DiscoveryError;

/// Stable identity for one repository scan.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

/// Stable identity for a discovered repository.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RepositoryId(pub String);

/// Stable identity for a package declaration.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct PackageId(pub String);

/// A path expressed relative to the authorized repository root.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RootRelativePath(PathBuf);

impl RootRelativePath {
    /// Creates a normalized relative path, rejecting absolute paths and parent traversal.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, DiscoveryError> {
        let path = path.into();
        let mut normalized = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(value) => normalized.push(value),
                _ => return Err(DiscoveryError::InvalidRelativePath(path)),
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(DiscoveryError::InvalidRelativePath(path));
        }
        Ok(Self(normalized))
    }

    /// Returns the relative path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The directory containing this path; empty for entries at the repository root.
    pub fn directory(&self) -> &Path {
        self.0.parent().unwrap_or(Path::new(""))
    }
}
