//! One rule per group kind.
mod cycles;
mod directory;
mod package;
mod source_root;

pub use cycles::CycleRule;
pub use directory::DirectoryRule;
pub use package::DeclaredPackageRule;
pub use source_root::SourceRootRule;

use std::collections::BTreeMap;

use crate::repository_intelligence::RootRelativePath;

/// Sorted member lists keyed by group key; shared by the rules that bucket files.
pub(super) fn bucket<K: Ord>(
    files: impl Iterator<Item = (K, RootRelativePath)>,
) -> BTreeMap<K, Vec<RootRelativePath>> {
    let mut buckets: BTreeMap<K, Vec<RootRelativePath>> = BTreeMap::new();
    for (key, file) in files {
        buckets.entry(key).or_default().push(file);
    }
    for members in buckets.values_mut() {
        members.sort();
        members.dedup();
    }
    buckets
}
