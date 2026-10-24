//! Readers for the Markdown formats. Reading never fails: what cannot be understood is skipped and
//! `validate` reports it. Line numbers are 1-based and refer to the file as stored.
pub mod capability;
pub mod front;
pub mod spec;
pub mod tasks;

/// Words of `text` that are `prefix` followed by digits (`R1`, `C12`), with their byte offsets.
pub(crate) fn ids_with_prefix(text: &str, prefix: char) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut start = None;
    let mut push = |from: usize, to: usize| {
        let word = &text[from..to];
        let digits = word.strip_prefix(prefix);
        if digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) {
            found.push((from, word.to_owned()));
        }
    };
    for (index, c) in text.char_indices() {
        match (c.is_ascii_alphanumeric(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                push(from, index);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        push(from, text.len());
    }
    found
}
