#![warn(clippy::all)]

//! Tools that let an agent reach into the skill catalog.
//!
//! `harness-skills` puts each skill's **name and description** into the
//! system prompt. These two tools are how the model pays for the rest, and
//! only for the skill it actually needs:
//!
//! - [`SkillLoadTool`] (`skill.load`) — one skill's full instructions plus a
//!   listing of the files it bundles.
//! - [`SkillReadTool`] (`skill.read`) — one of those bundled files, scoped
//!   to that skill's directory.
//!
//! Both are ordinary `ToolExecutor`s registered into the session's normal
//! registry, so permission policy, cancellation, and the tool-call event
//! stream all apply to them exactly as they do to `fs.read`.
//!
//! # Failure convention
//!
//! An unknown skill name, a missing file, or a path that escapes the skill
//! directory all return `Ok(ToolResult { is_error: true })` rather than
//! `Err(ToolError)`. `ToolError` is reserved for infrastructure faults that
//! should abort the call; a bad argument is something the model can read and
//! correct on the next turn. This matches `fs.read` and `McpToolExecutor`.

mod load;
mod read;

pub use load::{LoadInput, SkillLoadTool, SKILL_LOAD};
pub use read::{ReadInput, SkillReadTool, SKILL_READ};

use std::sync::Arc;

use harness_skills::SkillCatalog;
use harness_tools::ToolExecutor;

/// Builds both skill tools over one shared catalog.
///
/// Convenience for the registration site in `harness-engine`, which wants to
/// fold every skill tool into the session registry without naming them
/// individually — so adding a third tool here doesn't require an edit there.
pub fn skill_tools(catalog: Arc<SkillCatalog>) -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(SkillLoadTool::new(catalog.clone())),
        Arc::new(SkillReadTool::new(catalog)),
    ]
}
