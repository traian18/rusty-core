//! Markdown replies for structured agent steps.
//!
//! Models are asked to answer in plain Markdown, never JSON. The step's output
//! schema still defines the data the host passes between nodes; this module
//! derives a Markdown layout from that schema (`instructions`) and reads a
//! reply back into the matching value (`parse`).
//!
//! Layout, by schema shape:
//! - top-level properties are `## name` sections;
//! - a scalar is the section text, a list of scalars is a bullet list;
//! - a list of `{id, text}` pairs is bullets written `- ID: text`;
//! - a list of other objects is one `### ID` heading per item, with the short
//!   fields as `key: value` lines, the main text as the body, and nested
//!   lists/objects under deeper headings.
//!
//! The pieces: `schema` reads the schema's shape, `template` writes the instructions, `render`
//! writes workflow values as Markdown, and `parse` (with `object`, `items` and `coerce`) reads a
//! reply back.

mod coerce;
mod items;
mod layout;
mod object;
mod parse;
mod render;
mod schema;
mod template;
#[cfg(test)]
mod tests;

pub(crate) use parse::parse;
pub(crate) use render::render_input;
pub(crate) use schema::supports;
pub(crate) use template::instructions;
