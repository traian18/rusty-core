//! Boundary rules: imports one part of the code must not make.
use crate::glob::Glob;

/// An import a file makes: the repository-relative file it resolves to, and the line it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub target: String,
    pub line: u32,
}

/// The imports among `imports` that land in a forbidden file.
pub(crate) fn crossings<'a>(forbidden: &[Glob], imports: &'a [Import]) -> Vec<&'a Import> {
    imports
        .iter()
        .filter(|import| forbidden.iter().any(|g| g.matches(&import.target)))
        .collect()
}
