#![warn(clippy::all)]

//! Spec-driven development: the folder layout, the Markdown formats and the checks for changes
//! that move from a spec to a plan to finished work. Files are the source of truth; nothing here
//! calls a model. See `rusty-core/docs/design/spec-driven-development.md`.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `drift` | Capability and project text that names files which no longer exist |
//! | `export` | One-way copy of these specs into OpenSpec's or Spec Kit's layout |
//! | `import` | One-time conversion of OpenSpec and Spec Kit folders into this layout |
//! | `layout` | Where things live: root, change folders, numbering, slugs, branch names |
//! | `store` | The `SpecStore` trait over a repository's files, with disk and memory implementations |
//! | `model` | Plain data the formats parse into and the overview reports |
//! | `parse` | Front matter, `spec.md`, `tasks.md` and capability specs, read leniently |
//! | `validate` | Warnings with file and line for one change |
//! | `onboarding` | The capability list a survey proposes for an existing project, and which are written |
//! | `overview` | Everything under the specs root in one value, with progress and next steps |
//! | `archive` | Merging a finished change into the capabilities and moving it to the archive |
//! | `context` | The text a spec workflow starts from, composed from the files |
//! | `plan` | The task plan the queue runs, composed from the spec and the unticked tasks |
//! | `progress` | Ticks, task documents, `verification.md` and the status an Implement run leaves |
//! | `record` | Writing a workflow step's Markdown into a change and moving its status on |
//! | `scaffold` | Setting up the skeleton and creating a change from templates |
//! | `templates` | The text of the files `scaffold` writes |
pub mod archive;
pub mod context;
pub mod drift;
pub mod export;
pub mod import;
pub mod layout;
pub mod model;
pub mod onboarding;
pub mod overview;
pub mod parse;
pub mod plan;
pub mod progress;
pub mod record;
pub mod scaffold;
pub mod store;
mod templates;
pub mod validate;

pub use model::{
    CapabilitySummary, ChangeSummary, NextStep, Severity, SpecsOverview, Status, Warning,
};
pub use overview::overview;
pub use scaffold::{create_change, init, plan_change, ChangeRequest, NewChange};
pub use store::{FsStore, MemoryStore, SpecError, SpecStore};
