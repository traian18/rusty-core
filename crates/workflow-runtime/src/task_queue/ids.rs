//! Loose identifiers: a planner writes `C-1`, `c1` and `C1` for one criterion.

use std::collections::HashSet;

/// The comparable form of an id: letters and digits, lower case.
pub(super) fn id_key(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Hands out ids that are not yet taken, numbering the missing or repeated ones.
pub(super) struct Numbering {
    prefix: &'static str,
    used: HashSet<String>,
}

impl Numbering {
    pub(super) fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            used: HashSet::new(),
        }
    }

    /// `wanted` when it is free, else the next unused `prefix` + number.
    pub(super) fn claim(&mut self, wanted: String) -> String {
        let mut id = wanted;
        let mut number = self.used.len();
        while id.is_empty() || self.used.contains(&id_key(&id)) {
            number += 1;
            id = format!("{}{number}", self.prefix);
        }
        self.used.insert(id_key(&id));
        id
    }
}
