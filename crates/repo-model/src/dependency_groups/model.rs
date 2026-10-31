//! Groups of files and the immutable index that stores them in a snapshot.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    import_graph::{ComponentVersion, ImportFactId},
    repository_intelligence::RootRelativePath,
};

/// Stable group identity of the form `kind:key`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GroupId(pub String);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKind {
    DeclaredPackage,
    SourceRoot,
    Directory,
    ImportCycle,
}

/// How firmly the grouping is established.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBasis {
    /// Stated by a manifest.
    Declared,
    /// Guessed from layout; never a proven dependency.
    Inferred,
    /// Computed from import edges.
    Derived,
}

/// Explicit ceilings for analysis and group queries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupLimits {
    pub max_page_limit: usize,
    pub max_projection_nodes: usize,
    pub max_projection_edges: usize,
    pub max_projection_depth: usize,
    pub max_projection_roots: usize,
    /// Import sites recorded as evidence for one cycle.
    pub max_cycle_evidence: usize,
}

impl Default for GroupLimits {
    fn default() -> Self {
        Self {
            max_page_limit: 500,
            max_projection_nodes: 5_000,
            max_projection_edges: 20_000,
            max_projection_depth: 16,
            max_projection_roots: 64,
            max_cycle_evidence: 200,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub kind: GroupKind,
    pub label: String,
    pub basis: GroupBasis,
    pub explanation: String,
    /// Manifest, source root, or directory that justifies the group.
    pub evidence: Vec<RootRelativePath>,
    /// For cycles: import sites inside the component.
    pub cycle_sites: Vec<ImportFactId>,
    pub evidence_truncated: bool,
    /// For cycles: true when the cycle only exists once type-only imports are counted.
    pub involves_type_only: bool,
    /// Sorted member files.
    pub members: Vec<RootRelativePath>,
}

impl Group {
    pub fn contains(&self, file: &RootRelativePath) -> bool {
        self.members.binary_search(file).is_ok()
    }
}

/// Everything the group analyzer produced for one snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupIndex {
    /// True when an analyzer ran; queries on an unconfigured index fail instead of returning nothing.
    pub configured: bool,
    /// Sorted by kind, then id.
    pub groups: Vec<Group>,
    /// Group ids sorted for lookup; values index into `groups`.
    pub by_id: Vec<(GroupId, u32)>,
    /// Membership sorted by file; values index into `groups`.
    pub by_file: Vec<(RootRelativePath, u32)>,
    pub rules: Vec<ComponentVersion>,
    pub limits: GroupLimits,
    /// Digest of everything above; folded into the snapshot id.
    pub digest: String,
}

impl GroupIndex {
    pub fn new(
        mut groups: Vec<Group>,
        mut rules: Vec<ComponentVersion>,
        limits: GroupLimits,
    ) -> Self {
        groups.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.id.cmp(&b.id)));
        rules.sort();
        let mut by_file = groups
            .iter()
            .enumerate()
            .flat_map(|(position, group)| {
                group
                    .members
                    .iter()
                    .map(move |file| (file.clone(), position as u32))
            })
            .collect::<Vec<_>>();
        by_file.sort();
        let mut by_id = groups
            .iter()
            .enumerate()
            .map(|(position, group)| (group.id.clone(), position as u32))
            .collect::<Vec<_>>();
        by_id.sort();
        let mut index = Self {
            configured: true,
            groups,
            by_id,
            by_file,
            rules,
            limits,
            digest: String::new(),
        };
        index.digest = index.compute_digest();
        index
    }

    fn compute_digest(&self) -> String {
        let bytes =
            serde_json::to_vec(&(&self.groups, &self.rules, &self.limits)).unwrap_or_default();
        format!("{:x}", Sha256::digest(&bytes))
    }

    pub fn group(&self, id: &GroupId) -> Option<&Group> {
        let position = self
            .by_id
            .binary_search_by(|(known, _)| known.cmp(id))
            .ok()?;
        Some(&self.groups[self.by_id[position].1 as usize])
    }

    /// Groups the file belongs to, in index order.
    pub fn groups_containing<'a>(
        &'a self,
        file: &'a RootRelativePath,
    ) -> impl Iterator<Item = &'a Group> {
        let start = self.by_file.partition_point(|(path, _)| path < file);
        self.by_file[start..]
            .iter()
            .take_while(move |(path, _)| path == file)
            .map(|(_, position)| &self.groups[*position as usize])
    }
}
