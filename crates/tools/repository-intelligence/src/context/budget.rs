//! Fits an answer into the caller's size budget: sections are filled in a fixed order, items in
//! their given order, and everything left out is counted.
use std::collections::BTreeMap;

use serde_json::Value;

/// Characters kept back for the envelope, status, support block and next calls.
const OVERHEAD: usize = 900;

pub(super) struct Budget {
    remaining: usize,
    omitted: BTreeMap<String, usize>,
}

impl Budget {
    pub fn new(limit: usize) -> Self {
        Self {
            remaining: limit.saturating_sub(OVERHEAD).max(200),
            omitted: BTreeMap::new(),
        }
    }

    /// The items that still fit, in order; the rest are counted under `section`. The first item
    /// of a section is always kept so no requested section comes back empty by accident.
    pub fn fill(&mut self, section: &str, items: Vec<Value>) -> Vec<Value> {
        self.fill_up_to(section, items, self.remaining)
    }

    /// Like [`Budget::fill`], but the section may use at most `1/parts` of what is left, so an
    /// early section cannot starve the ones after it.
    pub fn fill_share(&mut self, section: &str, items: Vec<Value>, parts: usize) -> Vec<Value> {
        self.fill_up_to(section, items, self.remaining / parts.max(1))
    }

    fn fill_up_to(&mut self, section: &str, items: Vec<Value>, cap: usize) -> Vec<Value> {
        let mut kept = Vec::new();
        let mut spent = 0;
        let total = items.len();
        for item in items {
            let size = item.to_string().len() + 1;
            if !kept.is_empty() && spent + size > cap {
                break;
            }
            spent += size;
            kept.push(item);
        }
        self.remaining = self.remaining.saturating_sub(spent);
        if kept.len() < total {
            self.omitted.insert(section.to_owned(), total - kept.len());
        }
        kept
    }

    pub fn omitted(&self) -> &BTreeMap<String, usize> {
        &self.omitted
    }
}
