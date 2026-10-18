//! Normalized history facts, independent of how they were read.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harness_workspace::repository_intelligence::RootRelativePath;

/// A commit object id as lowercase hex.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct CommitId(pub String);

/// Stable, anonymous identity for a contributor: a hash of the lowercased email.
/// The address itself is never stored.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ContributorId(pub String);

impl ContributorId {
    pub fn from_email(email: &str) -> Self {
        let digest = Sha256::digest(email.trim().to_lowercase().as_bytes());
        Self(digest.iter().take(8).map(|b| format!("{b:02x}")).collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Contributor {
    pub id: ContributorId,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// One file touched by one commit, at the path it had in that commit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: RootRelativePath,
    /// The path before a rename.
    pub previous_path: Option<RootRelativePath>,
    pub kind: ChangeKind,
    /// Absent for binary files and when line statistics were skipped.
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommitFact {
    pub id: CommitId,
    pub parent_count: usize,
    /// Seconds since the Unix epoch, from the author signature.
    pub author_time: i64,
    pub contributor: Contributor,
    /// The first line of the message.
    #[serde(default)]
    pub subject: String,
    /// Empty for merge commits, whose changes the merged commits already carry.
    pub changes: Vec<FileChange>,
}

impl CommitFact {
    pub fn is_merge(&self) -> bool {
        self.parent_count > 1
    }

    /// Whether the subject says the commit fixes a bug ([`crate::is_fix_commit`]).
    pub fn is_fix(&self) -> bool {
        crate::fix_rule::is_fix_commit(&self.subject)
    }
}
