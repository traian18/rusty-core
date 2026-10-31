#![warn(clippy::all)]
#![warn(clippy::undocumented_unsafe_blocks)]

//! What is known about a repository: its packages and manifests, the import graph between
//! files, framework relations and dependency groups. Data and the rules that derive it; reading
//! the files themselves is `rusty-workspace`'s job.

pub mod dependency_groups;
pub mod folder_tree;
pub mod framework_relations;
pub mod import_graph;
pub mod repository_intelligence;
