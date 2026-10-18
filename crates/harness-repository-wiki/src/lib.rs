#![warn(clippy::all)]

//! Cited documentation pages for every level of a repository, rendered from published snapshots,
//! and the planning of which pages a model should see with a prompt.
//!
//! Structure is a view: a page is a pure function of the snapshot (and the Git and findings sets
//! that match it), so nothing needs storing or syncing. Every fact carries the citations and links
//! that support it.
mod context;
mod digest;
mod error;
pub mod facts;
mod model;
mod render;
mod source;
pub mod tests_of;
pub mod written;

pub use context::{
    replay, sanitize, Centrality, ContextItem, ContextLedger, ContextPlanner, ContextRenderer,
    FocusSet, FocusSource, Injected, Plan, PlanInput, PlanOptions, RenderedContext, Seen, Signal,
    StaticFocus, TaskKind, Tier, BRIEF_CHARS, MAX_ITEMS,
};
pub use error::WikiError;
pub use model::{Citation, Fact, Page, PageId, PageLevel, PageStatus, Section};
pub use render::{PageRenderer, TreeEntry};
