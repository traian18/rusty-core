//! Which lines hold code, and how many do in a range.
use tree_sitter::Node;

use super::dialect::Dialect;

pub struct CodeLines {
    /// `prefix[n]` = code lines among the first `n` lines (0-based rows).
    prefix: Vec<u32>,
}

fn mark(node: Node<'_>, source: &[u8], dialect: &dyn Dialect, code: &mut Vec<bool>) {
    if dialect.is_comment(node) || dialect.is_docstring(node, source) {
        return;
    }
    if node.child_count() == 0 {
        if node.end_byte() > node.start_byte() {
            let (start, end) = (node.start_position().row, node.end_position().row);
            if code.len() <= end {
                code.resize(end + 1, false);
            }
            code[start..=end].iter_mut().for_each(|line| *line = true);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        mark(child, source, dialect, code);
    }
}

impl CodeLines {
    pub fn new(root: Node<'_>, source: &[u8], dialect: &dyn Dialect) -> Self {
        let mut code = vec![false; root.end_position().row + 1];
        mark(root, source, dialect, &mut code);
        let mut prefix = Vec::with_capacity(code.len() + 1);
        prefix.push(0);
        for line in code {
            let last = *prefix.last().unwrap_or(&0);
            prefix.push(last + u32::from(line));
        }
        Self { prefix }
    }

    /// Code lines in the 0-based rows `start..=end`.
    pub fn count(&self, start: usize, end: usize) -> u32 {
        let last = self.prefix.len() - 1;
        let (start, end) = (start.min(last), (end + 1).min(last));
        self.prefix[end].saturating_sub(self.prefix[start])
    }

    pub fn total(&self) -> u32 {
        *self.prefix.last().unwrap_or(&0)
    }
}
