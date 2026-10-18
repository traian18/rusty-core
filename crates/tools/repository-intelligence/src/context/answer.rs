//! The answer every mode returns: one status, the target, what could be read, a one-line summary,
//! the sections that fit the budget, what was left out, and the calls to make next.
use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Status {
    Ok,
    Partial,
    Unsupported,
    NotIndexed,
    Stale,
    NotFound,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
            Self::NotIndexed => "not_indexed",
            Self::Stale => "stale",
            Self::NotFound => "not_found",
        }
    }
}

pub(super) struct Answer {
    pub status: Status,
    pub target: Value,
    pub support: Value,
    pub summary: String,
    pub sections: Map<String, Value>,
    pub omitted: BTreeMap<String, usize>,
    pub next: Vec<Value>,
    /// Git snapshot id when history was read.
    pub git_snapshot: Option<String>,
    /// The answer is smaller than the full one because a query bound was hit.
    pub bounded: Option<&'static str>,
}

impl Answer {
    /// An answer about a target that has nothing to read: only status, target, support and a hint.
    pub fn absent(status: Status, target: Value, support: Value, summary: String) -> Self {
        Self {
            status,
            target,
            support,
            summary,
            sections: Map::new(),
            omitted: BTreeMap::new(),
            next: Vec::new(),
            git_snapshot: None,
            bounded: None,
        }
    }

    pub fn truncation(&self) -> Option<&'static str> {
        if self.bounded.is_some() {
            return self.bounded;
        }
        (!self.omitted.is_empty()).then_some("budget")
    }

    pub fn into_result(self) -> Value {
        let truncated = if self.omitted.is_empty() && self.bounded.is_none() {
            Value::Null
        } else {
            json!({"reason": self.bounded.unwrap_or("budget"), "omitted": self.omitted})
        };
        json!({
            "status": self.status.name(),
            "target": self.target,
            "support": self.support,
            "summary": self.summary,
            "sections": self.sections,
            "truncated": truncated,
            "next": self.next,
        })
    }
}

/// One suggested follow-up call.
pub(super) fn next(tool: &str, arguments: Value, why: impl Into<String>) -> Value {
    json!({"tool": tool, "arguments": arguments, "why": why.into()})
}

pub(super) fn refresh_next(why: &str) -> Value {
    next("repo_graph.refresh", json!({}), why)
}
