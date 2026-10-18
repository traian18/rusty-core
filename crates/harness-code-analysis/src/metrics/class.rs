//! Class size and cohesion (LCOM4) from what each method touches on its instance.
use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use super::{
    dialect::Dialect,
    model::{ClassMetrics, FunctionMetrics},
};

pub struct ClassDraft {
    pub metrics: ClassMetrics,
    /// For each method: the instance members it names (fields and methods alike).
    pub references: Vec<(usize, BTreeSet<String>)>,
}

/// Every `receiver.name` in `method`, closures included (they run on the same instance).
pub fn members(
    method: Node<'_>,
    source: &[u8],
    dialect: &dyn Dialect,
    receiver: &str,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![method];
    while let Some(node) = stack.pop() {
        if let Some(name) = dialect.member(node, source, receiver) {
            out.insert(name);
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut root = i;
    while parent[root] != root {
        root = parent[root];
    }
    let mut node = i;
    while parent[node] != root {
        let next = parent[node];
        parent[node] = root;
        node = next;
    }
    root
}

/// Fills in the counts and LCOM4 once every method of the class has been measured.
///
/// Methods are linked when they name the same field or one names the other. LCOM4 counts the
/// linked groups that touch at least one field; a group of stateless helpers is a method to move,
/// not a class to extract, so it is not counted.
pub fn finish(draft: &mut ClassDraft, functions: &[FunctionMetrics]) {
    let methods: Vec<(usize, &str)> = draft
        .references
        .iter()
        .map(|(index, _)| (*index, functions[*index].name.as_str()))
        .collect();
    let method_names: BTreeSet<&str> = methods.iter().map(|(_, name)| *name).collect();
    let mut parent: Vec<usize> = (0..methods.len()).collect();
    let mut by_field: BTreeMap<&str, usize> = BTreeMap::new();
    let mut touches_state = vec![false; methods.len()];
    let position: BTreeMap<&str, usize> = methods
        .iter()
        .enumerate()
        .map(|(i, (_, name))| (*name, i))
        .collect();
    for (i, (_, references)) in draft.references.iter().enumerate() {
        for name in references {
            if method_names.contains(name.as_str()) {
                if let Some(&j) = position.get(name.as_str()) {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
                continue;
            }
            touches_state[i] = true;
            match by_field.get(name.as_str()) {
                Some(&j) => {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
                None => {
                    by_field.insert(name.as_str(), i);
                }
            }
        }
    }
    let mut stateful_groups = BTreeSet::new();
    for (i, touches) in touches_state.iter().enumerate() {
        if *touches {
            stateful_groups.insert(find(&mut parent, i));
        }
    }
    let metrics = &mut draft.metrics;
    metrics.method_count = methods.len() as u32;
    metrics.max_method_ccn = methods
        .iter()
        .map(|(index, _)| functions[*index].ccn)
        .max()
        .unwrap_or(0);
    metrics.field_count = by_field.len() as u32;
    metrics.lcom4 = stateful_groups.len() as u32;
}
