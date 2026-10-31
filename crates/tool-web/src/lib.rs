#![warn(clippy::all)]

//! Web tools. Currently just `web.fetch` — see `crates/tools/web/PLAN.md`
//! for why `web.search` is deferred (it needs a search-provider decision
//! that `web.fetch` doesn't).

mod fetch;
mod ssrf;

pub use fetch::{FetchInput, FetchTool};
