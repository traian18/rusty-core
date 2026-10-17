//! Workspace membership: which declared members a package inherits.
use std::path::Path;

use crate::repository_intelligence::{Package, RootRelativePath, WorkspaceDeclaration};

/// Gives each package the member list of its nearest enclosing workspace root.
///
/// A manifest that declares its own workspace is a root and keeps only its own
/// declaration. Other packages inherit only when a wildcard-free member covers
/// them; glob expansion is deliberately not guessed.
pub(super) fn apply_inherited_members(
    packages: &mut [Package],
    applies_to: impl Fn(&Package) -> bool,
    declarations: &[WorkspaceDeclaration],
) {
    for package in packages.iter_mut().filter(|package| applies_to(package)) {
        let directory = package.manifest.directory().to_path_buf();
        let directory = directory.as_path();
        if declarations.iter().any(|d| d.base == directory) {
            continue;
        }
        let Some(base) = nearest_enclosing_base(declarations, directory) else {
            continue;
        };
        let members = literal_members(declarations, base);
        if members.iter().any(|member| covers(member, directory)) {
            package.facts.workspace_members.extend(members);
        }
    }
}

fn nearest_enclosing_base<'a>(
    declarations: &'a [WorkspaceDeclaration],
    directory: &Path,
) -> Option<&'a Path> {
    declarations
        .iter()
        .map(|declaration| declaration.base.as_path())
        .filter(|base| directory.starts_with(base))
        .max_by_key(|base| base.components().count())
}

fn literal_members(declarations: &[WorkspaceDeclaration], base: &Path) -> Vec<RootRelativePath> {
    declarations
        .iter()
        .filter(|declaration| declaration.base == base)
        .flat_map(|declaration| declaration.members.iter())
        .filter(|member| {
            let pattern = member.as_path().to_string_lossy();
            !pattern.contains('*') && !pattern.contains('?')
        })
        .cloned()
        .collect()
}

fn covers(member: &RootRelativePath, directory: &Path) -> bool {
    directory == member.as_path() || directory.starts_with(member.as_path())
}
