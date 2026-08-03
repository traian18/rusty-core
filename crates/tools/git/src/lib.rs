#![warn(clippy::all)]

//! Read-only git introspection tools (`git.status`, `git.diff`, `git.log`,
//! `git.show`) backed directly by `git2`. Deliberately no mutating
//! operations (commit/add/push/branch) — see `crates/tools/git/PLAN.md` for
//! why that's a scope decision, not an oversight: mutating git history
//! belongs behind an explicit, visible permission check, and `shell.exec`
//! already covers that.

mod diff;
mod log;
mod show;
mod status;

pub use diff::{GitDiffInput, GitDiffTool};
pub use log::{GitLogInput, GitLogTool};
pub use show::{GitShowInput, GitShowTool};
pub use status::{GitStatusInput, GitStatusTool};
