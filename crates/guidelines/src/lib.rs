#![warn(clippy::all)]

//! Code guidelines: a project's own style and rules (`.rusty/guidelines/rules.json` and
//! `style.md`), checked against files as they are edited, next to code health.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `config` | The rules file as written: rules, limits, detector thresholds |
//! | `compile` | Parsing and validating it into [`Guidelines`] |
//! | `glob` | Which files a rule applies to |
//! | `pattern` | Regex and tree-sitter query rules |
//! | `metric` | Size and complexity limits over a file's measurements |
//! | `boundary` | Imports one part of the code must not make |
//! | `style` | `style.md` sections, by the files they apply to |
//! | `check` | One file through every rule that applies, as [`Violation`]s |
//! | `changed` | Keeping only what a change introduced |
//!
//! No I/O and no model: the host reads the files and resolves imports; prose rules are handed to
//! the model as instructions, never judged here.
mod boundary;
mod changed;
mod check;
mod compile;
mod config;
mod glob;
mod metric;
mod pattern;
mod style;

pub use boundary::Import;
pub use changed::{retain_introduced, ChangedLines};
pub use check::{check_file, FileInput, Kinds, Violation, LIMITS_RULE};
pub use compile::{ConfigError, Guidelines};
pub use config::{GuidelinesFile, MetricLimits, Rule, RuleKind, Severity, SCHEMA_VERSION};
pub use glob::Glob;
pub use style::{StyleGuide, StyleSection};

/// The JSON schema of `rules.json`, published as `schema/guidelines-v1.schema.json`.
pub fn guidelines_json_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(GuidelinesFile)
}

/// Where a repository keeps its guidelines, relative to its root.
pub const GUIDELINES_DIR: &str = ".rusty/guidelines";
/// The rules file inside [`GUIDELINES_DIR`].
pub const RULES_FILE: &str = "rules.json";
/// The prose style guide inside [`GUIDELINES_DIR`].
pub const STYLE_FILE: &str = "style.md";
