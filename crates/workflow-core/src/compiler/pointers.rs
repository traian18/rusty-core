//! JSON Pointers in bindings.

use super::Issues;
use crate::definition::OutputBinding;

/// The pointer a binding reads from its source.
pub(super) fn binding_pointer(binding: &OutputBinding) -> &str {
    match binding {
        OutputBinding::RunInput { pointer } | OutputBinding::NodeOutput { pointer, .. } => pointer,
    }
}

/// A pointer is empty (the whole value) or starts with `/`.
pub(super) fn check_pointer(pointer: &str, path: &str, issues: &mut Issues) {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        issues.add(
            path,
            "invalid_pointer",
            "bindings use JSON Pointer syntax: empty or starting with '/'",
        );
    }
}
