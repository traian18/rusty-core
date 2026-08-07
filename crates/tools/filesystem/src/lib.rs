#![warn(clippy::all)]

//! Concrete filesystem tools (fs.read, fs.edit, workspace.search) backed by the harness Workspace trait.

pub mod edit;
pub mod read;
pub mod search;

pub use edit::{EditInput, EditTool};
pub use read::{ReadInput, ReadTool};
pub use search::{SearchInput, SearchTool};
