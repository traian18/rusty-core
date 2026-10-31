//! Writing this project's specs in another tool's layout, so a team that works in OpenSpec or GitHub
//! Spec Kit can use them there. A one-way, non-destructive copy: the specs here stay the source,
//! nothing that already exists in the target is overwritten, and what has no equivalent in the target
//! is listed rather than forced into it. The conversions are best effort.
//!
//! | Module | Responsibility |
//! |---|---|
//! | `source` | Reads this project's changes (active and archived) into one plain shape |
//! | `openspec` | `openspec/`: project, capability specs, changes with delta specs |
//! | `speckit` | `.specify/memory/constitution.md` and `specs/NNN-feature/` |
mod openspec;
mod source;
mod speckit;

use serde::Serialize;

use crate::{
    import::Source,
    layout::Layout,
    parse::front,
    store::{SpecError, SpecStore},
};

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    /// Files and folders that were written.
    pub written: Vec<String>,
    /// What was left alone, or has no equivalent in the target, and why.
    pub skipped: Vec<String>,
}

pub fn export(
    store: &dyn SpecStore,
    target: Source,
    date: &str,
) -> Result<ExportReport, SpecError> {
    match target {
        Source::OpenSpec => openspec::export(store, date),
        Source::SpecKit => speckit::export(store, date),
    }
}

/// Records in the change's history where it was exported to. Importing from that folder later then
/// recognises the change as ours and does not bring it in a second time.
pub(crate) fn note_export(
    store: &dyn SpecStore,
    layout: &Layout,
    change: &source::ChangeDoc,
    target: &str,
    date: &str,
) -> Result<(), SpecError> {
    let parent = if change.archived {
        layout.archive()
    } else {
        layout.changes()
    };
    let path = format!("{parent}/{}/change.md", change.dir);
    if let Some(text) = store.read(&path) {
        store.write(
            &path,
            &front::add_history(&text, &format!("{date} Exported to {target}.")),
        )?;
    }
    Ok(())
}

/// Writes `text` unless the file exists; records either way.
pub(crate) fn write_new(
    store: &dyn SpecStore,
    report: &mut ExportReport,
    path: &str,
    text: &str,
) -> Result<bool, SpecError> {
    if store.read(path).is_some() {
        report.skipped.push(format!("{path} already exists"));
        return Ok(false);
    }
    store.write(path, text)?;
    report.written.push(path.to_owned());
    Ok(true)
}

/// `WHEN a (given g) THEN b AND c` as `(given, when, then, and)`.
pub(crate) struct Scenario {
    pub given: Option<String>,
    pub when: String,
    pub then: String,
    pub and: Vec<String>,
}

pub(crate) fn scenario(text: &str) -> Scenario {
    let upper = text.to_ascii_uppercase();
    let when_at = upper.find("WHEN ");
    // Where the THEN clause's own text starts.
    let then_at = upper
        .find(" THEN ")
        .map(|t| (t, t + 6))
        .or_else(|| upper.starts_with("THEN ").then_some((0, 5)));
    let clean = |s: &str| s.trim().trim_end_matches('.').trim().to_owned();
    let (when_text, then_text) = match (when_at, then_at) {
        (Some(w), Some((t, start))) if t > w => (clean(&text[w + 5..t]), clean(&text[start..])),
        (None, Some((_, start))) => (String::new(), clean(&text[start..])),
        _ => (String::new(), clean(text)),
    };
    let (when_text, given) = match when_text.rfind("(given ") {
        Some(g) if when_text.ends_with(')') => (
            when_text[..g].trim().to_owned(),
            Some(when_text[g + 7..when_text.len() - 1].trim().to_owned()),
        ),
        _ => (when_text, None),
    };
    let mut parts = then_text.split(" AND ").map(str::to_owned);
    let then = parts.next().unwrap_or_default();
    Scenario {
        given,
        when: if when_text.is_empty() {
            "the system is used".into()
        } else {
            when_text
        },
        then,
        and: parts.collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_criterion_splits_into_given_when_then_and_ands() {
        let s = scenario("WHEN a name is entered (given the library is open) THEN the album appears AND the name is free.");
        assert_eq!(
            (
                s.given.as_deref(),
                s.when.as_str(),
                s.then.as_str(),
                s.and.as_slice()
            ),
            (
                Some("the library is open"),
                "a name is entered",
                "the album appears",
                &["the name is free".to_owned()][..]
            )
        );
    }

    #[test]
    fn a_criterion_without_when_or_then_still_becomes_a_scenario() {
        let s = scenario("It just works.");
        assert_eq!(
            (s.when.as_str(), s.then.as_str()),
            ("the system is used", "It just works")
        );
        assert_eq!(scenario("THEN it holds").then, "it holds");
    }
}
