//! The folder rule every consumer shares: direct sub-folders, one entry for loose files, leaf when
//! there are no sub-folders.
use std::path::Path;

use super::{base_directory, directory_group, list};
use crate::{dependency_groups::Group, repository_intelligence::RootRelativePath};

fn files(paths: &[&str]) -> Vec<RootRelativePath> {
    let mut files: Vec<_> = paths
        .iter()
        .map(|p| RootRelativePath::new(p).unwrap())
        .collect();
    files.sort();
    files
}

fn focus(path: &str, members: &[&str]) -> Group {
    directory_group(Path::new(path), path.to_owned(), files(members))
}

#[test]
fn a_folder_lists_its_direct_sub_folders_and_its_loose_files() {
    let group = focus(
        "app",
        &[
            "app/a.ts",
            "app/ui/b.ts",
            "app/ui/deep/c.ts",
            "app/api/d.ts",
        ],
    );
    let listing = list(&group);
    let ids: Vec<_> = listing
        .folders
        .iter()
        .map(|f| f.group.id.0.as_str())
        .collect();
    assert_eq!(ids, ["dir:app/api", "dir:app/ui"]);
    assert_eq!(listing.loose, files(&["app/a.ts"]));
    // Only `ui` has more folders below it.
    let deeper: Vec<_> = listing.folders.iter().map(|f| f.has_folders).collect();
    assert_eq!(deeper, [false, true]);
    assert!(!listing.is_leaf());
}

#[test]
fn a_folder_with_only_files_is_a_leaf() {
    let group = focus("app/api", &["app/api/x.ts", "app/api/y.ts"]);
    let listing = list(&group);
    assert!(listing.is_leaf());
    assert_eq!(listing.loose.len(), 2);
}

#[test]
fn a_folder_that_holds_one_folder_still_shows_it() {
    let group = focus("app", &["app/solo/inner/a.ts", "app/solo/inner/b.ts"]);
    let listing = list(&group);
    assert_eq!(listing.folders.len(), 1);
    assert_eq!(listing.folders[0].group.label, "solo");
    assert!(listing.folders[0].has_folders);
    assert!(listing.loose.is_empty());
}

#[test]
fn a_sibling_whose_name_sorts_between_does_not_split_a_folder_from_its_files() {
    let group = focus("a", &["a/x-y/1.ts", "a/x/2.ts", "a/x/deep/3.ts"]);
    let listing = list(&group);
    let labels: Vec<_> = listing
        .folders
        .iter()
        .map(|f| f.group.label.as_str())
        .collect();
    assert_eq!(labels, ["x", "x-y"]);
    assert_eq!(listing.folders[0].group.members.len(), 2);
}

#[test]
fn a_group_that_is_not_a_folder_is_read_from_its_deepest_common_folder() {
    let mut package = focus(
        "ignored",
        &[
            "crate/src/lib.rs",
            "crate/src/tools/a.rs",
            "crate/src/tools/b.rs",
        ],
    );
    package.id.0 = "declared_package:crate/Cargo.toml:crate".into();
    assert_eq!(base_directory(&package), Path::new("crate/src"));
    let listing = list(&package);
    assert_eq!(listing.folders.len(), 1);
    assert_eq!(listing.loose, files(&["crate/src/lib.rs"]));
}
