//! Framework relations (roadmap Phase 4): routes, injected providers, rendered components and
//! exposed commands, read by framework adapters and stored in the snapshot beside the imports.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `model` | Relations, evidence, capabilities, limits and the immutable index |
//! | `adapter` | `FrameworkAdapter`, `FrameworkInput`, `NameResolver` |
//! | `analyzer` | The default analyzer that runs every adapter on every package |
//! | `queries` | Paged, snapshot-bound queries |
//!
//! The adapters themselves live outside this crate (`rusty-frameworks`). An adapter
//! runs only for packages where it can show the framework applies; every relation it emits
//! carries a rule, a version and source evidence, and conventions it cannot read statically are
//! diagnostics, never guesses.
mod adapter;
mod analyzer;
mod model;
mod queries;

pub use adapter::{
    declares, FrameworkAdapter, FrameworkAnalyzer, FrameworkInput, FrameworkOutput, NameResolver,
};
pub use analyzer::DefaultFrameworkAnalyzer;
pub use model::{
    Certainty, FrameworkApplication, FrameworkCapability, FrameworkIndex, FrameworkIndexParts,
    FrameworkLimits, FrameworkRelation, RelationDraft, RelationEvidence, RelationId, RelationKind,
    EVIDENCE_TEXT_LIMIT,
};
pub use queries::{
    FrameworkCursor, FrameworkFilter, FrameworkPage, FrameworkQuery, FrameworkQueryError,
};

#[cfg(test)]
mod tests;
