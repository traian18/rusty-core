#![warn(clippy::all)]

//! Filesystem skills: runtime-extensible agent capabilities.
//!
//! A skill is a directory containing a `SKILL.md` — YAML frontmatter naming
//! and describing it, then a markdown body of instructions — plus any files
//! the instructions reference. Dropping such a directory into
//! `.harness/skills/` gives an agent a new capability with **no
//! recompilation**, which is the gap the compile-time-only `Plugin` trait in
//! `harness-extension-api` leaves open.
//!
//! ```text
//! .harness/skills/pdf-report/
//! ├── SKILL.md
//! └── template.tex
//! ```
//!
//! ```markdown
//! ---
//! name: pdf-report
//! description: Generate a formatted PDF report from CSV data.
//! allowed-tools: [fs.read, shell.exec]
//! ---
//!
//! 1. Read the CSV with `fs.read`.
//! 2. Fill `template.tex` (read it with `skill.read`).
//! 3. Render it with `shell.exec`.
//! ```
//!
//! # Progressive disclosure
//!
//! The system prompt carries only each skill's **name and description** (see
//! [`SkillCatalog::catalog_prompt`]). Instruction bodies and bundled files
//! reach the model only when it calls the `skill.load` / `skill.read` tools
//! from `harness-tool-skills`. Thirty installed skills therefore cost thirty
//! lines per request, not thirty documents — the difference between skills
//! being free to install and being something you ration.
//!
//! # Layering
//!
//! This crate does filesystem I/O, so it must stay outside `harness-core`
//! and `harness-protocol` (enforced by `xtask check-deps`). It reaches the
//! model through two existing seams and adds no new ones:
//!
//! - [`SkillsContextProvider`] is a
//!   [`ContextProvider`](harness_context::ContextProvider), the same
//!   decorator seam compaction already uses.
//! - The tools are ordinary `ToolExecutor`s (from `harness-tools`),
//!   registered into the session's normal registry.
//!
//! Nothing here touches `harness-core`'s state machine.

mod catalog;
mod error;
mod frontmatter;
mod provider;
mod skill;

pub use catalog::{SkillCatalog, SkillsConfig, HARNESS_DIR, SKILLS_DIR};
pub use error::SkillError;
pub use provider::SkillsContextProvider;
pub use skill::{Skill, SkillSource, SKILL_FILE};
