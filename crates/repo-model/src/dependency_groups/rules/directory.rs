//! Inferred directory groups for files that no manifest owns.
use super::bucket;
use crate::dependency_groups::{
    analyzer::{GroupInput, GroupRule},
    model::{Group, GroupBasis, GroupId, GroupKind},
};

/// Only files without an owning package are grouped by directory: a package is a
/// stronger boundary, so owned files never get directory groups. Proximity is a hint,
/// not a dependency.
pub struct DirectoryRule;

impl GroupRule for DirectoryRule {
    fn id(&self) -> &'static str {
        "directory"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> GroupKind {
        GroupKind::Directory
    }

    fn groups(&self, input: &GroupInput<'_>) -> Vec<Group> {
        let unowned = input
            .imports
            .files
            .iter()
            .filter(|file| file.package.is_none())
            .map(|file| (file.path.directory().to_path_buf(), file.path.clone()));
        bucket(unowned)
            .into_iter()
            .map(|(directory, members)| {
                let label = if directory.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    directory.display().to_string()
                };
                Group {
                    id: GroupId(format!("directory:{label}")),
                    kind: GroupKind::Directory,
                    explanation: format!(
                        "Inferred: these files sit in `{label}` and no manifest declares an owner for them."
                    ),
                    label,
                    basis: GroupBasis::Inferred,
                    evidence: Vec::new(),
                    cycle_sites: Vec::new(),
                    evidence_truncated: false,
                    involves_type_only: false,
                    members,
                }
            })
            .collect()
    }
}
