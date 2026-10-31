//! Readers for the Markdown formats. Reading never fails: what cannot be understood is skipped and
//! `validate` reports it. Line numbers are 1-based and refer to the file as stored.
pub mod capability;
pub mod checklist;
pub mod front;
pub mod spec;
pub mod tasks;

/// Widest range expanded; a wider one is taken as its two ends only.
const MAX_RANGE: u32 = 500;

/// What may stand between the two ends of a range: `C1–C3`, `C1-C3`, `C1 — C3`, `C1..C3`,
/// `C1 to C3`.
fn is_range_separator(between: &str) -> bool {
    matches!(
        between.trim(),
        "-" | "–" | "—" | ".." | "..." | "to" | "through"
    )
}

/// The ids `text` names with `prefix`, ranges included (`C1–C3` is C1, C2 and C3), in order and
/// each once. Tasks write `R1 (C1–C6)` as often as `R1 (C1, C2, C3)`.
pub(crate) fn ids_with_ranges(text: &str, prefix: char) -> Vec<String> {
    let found = ids_with_prefix(text, prefix);
    let number = |id: &str| id[prefix.len_utf8()..].parse::<u32>().ok();
    let mut out: Vec<String> = Vec::new();
    let push = |id: String, out: &mut Vec<String>| {
        if !out.contains(&id) {
            out.push(id);
        }
    };
    for (index, (at, id)) in found.iter().enumerate() {
        push(id.clone(), &mut out);
        let Some((next_at, next)) = found.get(index + 1) else {
            continue;
        };
        let between = &text[at + id.len()..*next_at];
        if let (true, Some(from), Some(to)) =
            (is_range_separator(between), number(id), number(next))
        {
            if from < to && to - from <= MAX_RANGE {
                for n in from + 1..to {
                    push(format!("{prefix}{n}"), &mut out);
                }
            }
        }
    }
    out
}

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
