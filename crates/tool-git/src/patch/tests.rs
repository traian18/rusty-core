use super::filters::{
    truncate_at_char_boundary, DEFAULT_MAX_FILES, MAX_DIFF_BYTES, MAX_FILES_LIMIT,
};
use super::*;

#[test]
fn truncation_never_splits_a_character() {
    let text = "ab€cd"; // '€' is 3 bytes, occupying bytes 2..5
    assert_eq!(truncate_at_char_boundary(text, 3), "ab");
    assert_eq!(truncate_at_char_boundary(text, 5), "ab€");
    assert_eq!(truncate_at_char_boundary(text, 50), text);
}

#[test]
fn requested_limits_are_clamped_to_the_hard_caps() {
    let filters = DiffFilters {
        max_files: Some(10_000),
        max_bytes: Some(10_000_000),
        max_hunks_per_file: Some(0),
        ..Default::default()
    };
    assert_eq!(filters.max_files(), MAX_FILES_LIMIT);
    assert_eq!(filters.max_bytes(), MAX_DIFF_BYTES);
    assert_eq!(filters.max_hunks(), 1);
    assert_eq!(DiffFilters::default().max_files(), DEFAULT_MAX_FILES);
}
