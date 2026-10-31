//! Planning which documentation pages a model sees with a prompt, and rendering them as data.
//!
//! Retrieval is precision first: pages tied to something real (a path the prompt names, the file
//! being edited, a file that earlier steps handled) are injected as excerpts; related pages are
//! one-line pointers the model can follow; with nothing to anchor on, only the overview is sent.
mod blocks;
mod centrality;
mod focus;
mod ledger;
mod plan;
pub mod replay;

pub use blocks::{sanitize, ContextRenderer, Injected, RenderedContext, Seen, BRIEF_CHARS};
pub use centrality::Centrality;
pub use focus::{FocusSet, FocusSource, StaticFocus, TaskKind};
pub use ledger::ContextLedger;
pub use plan::{
    ContextItem, ContextPlanner, Plan, PlanInput, PlanOptions, Signal, Tier, MAX_ITEMS,
};
