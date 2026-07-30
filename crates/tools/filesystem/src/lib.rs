#![warn(clippy::all)]

//! Concrete filesystem tools (fs.read, fs.edit, workspace.search) backed by the harness Workspace trait.

pub mod read;
pub mod edit;
pub mod search;

pub use read::{ReadInput, ReadTool};
pub use edit::{EditInput, EditTool};
pub use search::{SearchInput, SearchTool};
