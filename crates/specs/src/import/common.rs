//! Helpers the two converters share.
use crate::{
    layout::{change_dir_name, Layout},
    model::{ChangeFront, Status},
    parse::front,
    scaffold::plan_change,
    store::{SpecError, SpecStore},
};

/// `add-dark-mode` becomes `Add dark mode`.
pub fn humanize(slug: &str) -> String {
    let words = slug
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    chars.next().map_or(String::new(), |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// The text without Markdown bold markers.
pub fn plain(text: &str) -> String {
    text.replace("**", "")
}

/// `(heading, lines)` for each `## ` section, headings lower-cased; text before the first is dropped.
pub fn sections(text: &str) -> Vec<(String, Vec<String>)> {
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            found.push((heading.trim().to_ascii_lowercase(), Vec::new()));
        } else if let Some((_, lines)) = found.last_mut() {
            lines.push(line.to_owned());
        }
    }
    found
}

/// The section's lines as text, trimmed.
pub fn body(lines: &[String]) -> String {
    lines.join("\n").trim().to_owned()
}

/// What the tasks say about how far a change got.
pub fn status_from(has_spec: bool, tasks: &[bool]) -> Status {
    if tasks.is_empty() {
        return if has_spec {
            Status::Specified
        } else {
            Status::Draft
        };
    }
    match (tasks.iter().filter(|done| **done).count(), tasks.len()) {
        (0, _) => Status::Planned,
        (done, total) if done == total => Status::Verifying,
        _ => Status::Building,
    }
}

/// A change converted from another tool.
pub struct Converted<'a> {
    pub title: &'a str,
    /// The folder name the source used; becomes the slug.
    pub name: &'a str,
    pub status: Status,
    pub date: &'a str,
    /// Written in the history so the same source is not imported twice.
    pub marker: &'a str,
    /// `(file name, text)`: `spec.md`, `design.md`, `tasks.md`, ...
    pub files: Vec<(&'a str, String)>,
}

/// Whether a change in this repository already records `marker` in its history.
pub fn imported_before(store: &dyn SpecStore, marker: &str) -> bool {
    let layout = Layout::of(store);
    [layout.changes(), layout.archive()].iter().any(|parent| {
        store.dirs(parent).iter().any(|dir| {
            store
                .read(&format!("{parent}/{dir}/change.md"))
                .is_some_and(|text| text.contains(marker))
        })
    })
}

/// Writes the converted change into the next free number; returns its folder name.
pub fn write_change(store: &dyn SpecStore, change: &Converted) -> Result<String, SpecError> {
    let layout = Layout::of(store);
    let plan = plan_change(store, change.name);
    let dir = change_dir_name(plan.number, &plan.slug);
    let front = ChangeFront {
        id: Some(format!("{:04}", plan.number)),
        title: Some(change.title.to_owned()),
        status: Some(change.status),
        created: Some(change.date.to_owned()),
        ..Default::default()
    };
    let history = [format!("{} Imported from {}.", change.date, change.marker)];
    let base = layout.change(&dir);
    store.write(
        &format!("{base}/change.md"),
        &front::render_change(&front, &history),
    )?;
    for (name, text) in &change.files {
        store.write(&format!("{base}/{name}"), text)?;
    }
    Ok(dir)
}

/// `text` with a `Purpose:` line after its title when it has a `## Purpose` section instead, and a
/// `## Principles` section appended when it has none (for a project file).
pub fn with_principles(text: &str, note: &str) -> String {
    let has = text.lines().any(|l| {
        l.starts_with('#')
            && l.trim_start_matches('#')
                .trim()
                .eq_ignore_ascii_case("principles")
    });
    if has {
        format!("{}\n", text.trim_end())
    } else {
        format!("{}\n\n## Principles\n{note}\n", text.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_become_titles_and_sections_split_on_headings() {
        assert_eq!(humanize("add-dark_mode"), "Add dark mode");
        let found = sections("# T\nintro\n## Why\nbecause\n## What Changes\n- a\n");
        assert_eq!(
            found.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(),
            ["why", "what changes"]
        );
        assert_eq!(body(&found[1].1), "- a");
    }

    #[test]
    fn progress_follows_the_ticks() {
        assert_eq!(status_from(true, &[]), Status::Specified);
        assert_eq!(status_from(false, &[]), Status::Draft);
        assert_eq!(status_from(true, &[false, false]), Status::Planned);
        assert_eq!(status_from(true, &[true, false]), Status::Building);
        assert_eq!(status_from(true, &[true, true]), Status::Verifying);
    }

    #[test]
    fn principles_are_added_only_when_missing() {
        assert!(with_principles("# P\n", "note").contains("## Principles\nnote"));
        assert_eq!(
            with_principles("# P\n\n## Principles\n- a\n", "note"),
            "# P\n\n## Principles\n- a\n"
        );
    }
}
