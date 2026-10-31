//! Source-root groups: the directories packages declare as their sources.
use std::{collections::BTreeMap, path::PathBuf};

use super::bucket;
use crate::{
    dependency_groups::{
        analyzer::{GroupInput, GroupRule},
        model::{Group, GroupBasis, GroupId, GroupKind},
    },
    repository_intelligence::RootRelativePath,
};

/// The directory of each declared Cargo source root or npm entry point is a group;
/// a file joins the deepest declared root directory above it within its own package.
pub struct SourceRootRule;

impl GroupRule for SourceRootRule {
    fn id(&self) -> &'static str {
        "source-root"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> GroupKind {
        GroupKind::SourceRoot
    }

    fn groups(&self, input: &GroupInput<'_>) -> Vec<Group> {
        let roots = declared_roots(input);
        let assigned = input.imports.files.iter().filter_map(|file| {
            let package = file.package.as_ref()?;
            let directories = roots.get(package)?;
            let deepest = directories
                .keys()
                .filter(|directory| file.path.as_path().starts_with(directory))
                .max_by_key(|directory| directory.components().count())?;
            Some(((package.clone(), deepest.clone()), file.path.clone()))
        });
        bucket(assigned)
            .into_iter()
            .map(|((package, directory), members)| {
                let evidence = roots[&package][&directory].clone();
                Group {
                    id: GroupId(format!("source_root:{}", directory.display())),
                    kind: GroupKind::SourceRoot,
                    label: directory.display().to_string(),
                    basis: GroupBasis::Declared,
                    explanation: format!(
                        "Directory of a source root declared in the package manifest ({}).",
                        evidence
                            .iter()
                            .map(|path| path.as_path().display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    evidence,
                    cycle_sites: Vec::new(),
                    evidence_truncated: false,
                    involves_type_only: false,
                    members,
                }
            })
            .collect()
    }
}

type RootsByDirectory = BTreeMap<PathBuf, Vec<RootRelativePath>>;

/// Per package: directory → the declared root files that live in it.
fn declared_roots(
    input: &GroupInput<'_>,
) -> BTreeMap<crate::repository_intelligence::PackageId, RootsByDirectory> {
    let mut roots = BTreeMap::new();
    for package in &input.structure.packages {
        let declared = package
            .facts
            .source_roots
            .iter()
            .chain(&package.facts.entry_points);
        for root in declared {
            let directory = root.directory().to_path_buf();
            let holds_files = input.imports.files.iter().any(|file| {
                file.package.as_ref() == Some(&package.id)
                    && file.path.as_path().starts_with(&directory)
            });
            if holds_files {
                let entry: &mut RootsByDirectory = roots.entry(package.id.clone()).or_default();
                let files = entry.entry(directory).or_default();
                if !files.contains(root) {
                    files.push(root.clone());
                }
            }
        }
    }
    roots
}
