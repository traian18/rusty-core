//! Declared package groups: the manifest that owns each file.
use super::bucket;
use crate::dependency_groups::{
    analyzer::{GroupInput, GroupRule},
    model::{Group, GroupBasis, GroupId, GroupKind},
};

/// A file belongs to the package whose manifest is its nearest ancestor in the same
/// ecosystem, so nested packages take precedence over the packages around them.
pub struct DeclaredPackageRule;

impl GroupRule for DeclaredPackageRule {
    fn id(&self) -> &'static str {
        "declared-package"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> GroupKind {
        GroupKind::DeclaredPackage
    }

    fn groups(&self, input: &GroupInput<'_>) -> Vec<Group> {
        let owned = input
            .imports
            .files
            .iter()
            .filter_map(|file| Some((file.package.clone()?, file.path.clone())));
        bucket(owned)
            .into_iter()
            .filter_map(|(package_id, members)| {
                let package = input.structure.packages.iter().find(|p| p.id == package_id)?;
                Some(Group {
                    id: GroupId(format!("declared_package:{}", package.id.0)),
                    kind: GroupKind::DeclaredPackage,
                    label: package.name.clone(),
                    basis: GroupBasis::Declared,
                    explanation: format!(
                        "Declared by {}; a file belongs to the nearest enclosing manifest of its ecosystem.",
                        package.manifest.as_path().display()
                    ),
                    evidence: vec![package.manifest.clone()],
                    cycle_sites: Vec::new(),
                    evidence_truncated: false,
                    involves_type_only: false,
                    members,
                })
            })
            .collect()
    }
}
