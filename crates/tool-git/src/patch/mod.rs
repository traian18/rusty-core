//! Deterministic limits shared by `git.diff` and `git.show`: pathspec and hunk filters,
//! file/hunk/byte caps, and a summary-only mode. Nothing here rewrites patch content: lines are
//! either included exactly as git2 produced them or left out, and every omission is reported in
//! the result.
//!
//! `filters` holds the request, `render` builds the result file by file, `file_block` renders one
//! file's patch text, and `summary` shapes the per-file summary and the limits report.

mod file_block;
mod filters;
mod render;
mod summary;

pub(crate) use filters::apply_pathspecs;
pub use filters::DiffFilters;
pub(crate) use render::render;

#[cfg(test)]
mod tests;
