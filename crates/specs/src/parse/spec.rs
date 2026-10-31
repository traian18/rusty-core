//! A change's `spec.md`: requirements (`### R1: Title`), their criteria (`- C1: WHEN … THEN …`),
//! the `Capability:` line, and the open questions.
use super::ids_with_prefix;
use crate::model::{Criterion, Requirement, SpecDoc};

pub fn parse(text: &str) -> SpecDoc {
    let mut doc = SpecDoc::default();
    let mut section = String::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(title) = line.strip_prefix("# ") {
            doc.title.get_or_insert_with(|| title.trim().to_owned());
        } else if let Some(name) = line.strip_prefix("## ") {
            section = name.trim().to_ascii_lowercase();
            doc.sections.push(section.clone());
        } else if section == "requirements" {
            read_requirement_line(&mut doc, line, number);
        } else if section == "open questions" {
            if let Some(question) = open_question(line) {
                doc.open_questions.push((number, question));
            }
        }
    }
    doc
}

/// Whether `text` is a spec that was never written: the template's empty requirement (`### R1:`
/// with `WHEN … THEN …`), or no requirement with a titled, written criterion at all. A run that
/// plans or builds from such a spec would have nothing to go on.
pub fn is_unwritten(text: &str) -> bool {
    !parse(text).requirements.iter().any(|requirement| {
        !requirement.title.trim().is_empty()
            && requirement
                .criteria
                .iter()
                .any(|criterion| !criterion.text.trim().is_empty() && !criterion.text.contains('…'))
    })
}

fn read_requirement_line(doc: &mut SpecDoc, line: &str, number: usize) {
    if let Some(heading) = line.strip_prefix("### ") {
        let (id, title) = split_heading(heading, doc.requirements.len() + 1);
        doc.requirements.push(Requirement {
            id,
            title,
            line: number,
            capability: None,
            detail: String::new(),
            criteria: Vec::new(),
        });
        return;
    }
    let Some(requirement) = doc.requirements.last_mut() else {
        return;
    };
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("Capability:") {
        requirement.capability = Some(rest.trim().to_owned());
    } else if let Some(bullet) = trimmed.strip_prefix("- ") {
        if let Some((id, text)) = criterion(bullet, requirement.criteria.len() + 1) {
            requirement.criteria.push(Criterion {
                id,
                text,
                line: number,
            });
        } else {
            add_detail(&mut requirement.detail, trimmed);
        }
    } else {
        add_detail(&mut requirement.detail, trimmed);
    }
}

fn add_detail(detail: &mut String, line: &str) {
    if line.is_empty() {
        return;
    }
    if !detail.is_empty() {
        detail.push('\n');
    }
    detail.push_str(line);
}

/// What a requirement's `Capability:` line says it does to a capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Adds,
    Changes,
    Removes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRef {
    /// The capability's folder name.
    pub capability: String,
    pub action: Action,
    /// The named requirement it adds, changes or removes; the heading of the requirement when absent.
    pub requirement: Option<String>,
}

/// `albums (adds "Album creation")`, `albums (changes "X")`, `albums (removes "X")`, or just `albums`
/// (which adds the requirement under its own title).
pub fn capability_ref(line: &str) -> Option<CapabilityRef> {
    let line = line.trim();
    let (name, rest) = match line.split_once('(') {
        Some((name, rest)) => (name.trim(), rest.trim_end_matches(')').trim()),
        None => (line, ""),
    };
    let capability = crate::layout::slugify(name);
    if name.is_empty() {
        return None;
    }
    let (word, quoted) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let action = match word.to_ascii_lowercase().as_str() {
        "changes" | "change" | "modifies" => Action::Changes,
        "removes" | "remove" | "deletes" => Action::Removes,
        _ => Action::Adds,
    };
    let requirement = quoted
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '“' || c == '”')
        .trim();
    Some(CapabilityRef {
        capability,
        action,
        requirement: (!requirement.is_empty()).then(|| requirement.to_owned()),
    })
}

/// `R1: Albums can be created` or `Albums can be created` (numbered by position).
fn split_heading(heading: &str, position: usize) -> (String, String) {
    let heading = heading.trim();
    if let Some((first, rest)) = heading.split_once(':') {
        if let Some((_, id)) = ids_with_prefix(first, 'R')
            .into_iter()
            .next()
            .filter(|(at, id)| *at == 0 && id.len() == first.trim().len())
        {
            return (id, rest.trim().to_owned());
        }
    }
    (format!("R{position}"), heading.to_owned())
}

/// `C1: WHEN … THEN …`, or a bare `WHEN …` which is numbered by position.
fn criterion(bullet: &str, position: usize) -> Option<(String, String)> {
    if let Some((first, rest)) = bullet.split_once(':') {
        if let Some((0, id)) = ids_with_prefix(first, 'C')
            .into_iter()
            .next()
            .filter(|(_, id)| id.len() == first.trim().len())
        {
            return Some((id, rest.trim().to_owned()));
        }
    }
    bullet
        .trim_start()
        .starts_with("WHEN ")
        .then(|| (format!("C{position}"), bullet.trim().to_owned()))
}

fn open_question(line: &str) -> Option<String> {
    let text = line.trim().strip_prefix("- ")?.trim();
    let none = text.is_empty()
        || ["none", "n/a", "no", "none."].contains(&text.to_ascii_lowercase().as_str());
    (!none).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "# Photo albums\n\n## Why\nGroup shoots.\n\n## Requirements\n\n### R1: Albums can be created and named\nCapability: albums (adds \"Album creation\")\nA user creates an album.\n\n- C1: WHEN the user creates an album with a new name THEN it appears.\n- C2: WHEN the name is taken THEN nothing is created.\n\n### Albums can be deleted\n- WHEN the user deletes an album THEN its photos stay.\n\n## Open questions\n- Can albums nest?\n- None\n";

    #[test]
    fn the_template_is_unwritten_and_a_real_spec_is_not() {
        let template = "# T\n\n## Requirements\n\n### R1: \nCapability: \n\n- C1: WHEN … THEN ….\n\n## Out of scope\n";
        assert!(is_unwritten(template));
        assert!(is_unwritten(""));
        assert!(is_unwritten(
            "# T\n\n## Requirements\n\n### R1: Title only\n"
        ));
        assert!(!is_unwritten(SPEC));
    }

    #[test]
    fn reads_requirements_criteria_and_capability() {
        let doc = parse(SPEC);
        assert_eq!(doc.title.as_deref(), Some("Photo albums"));
        assert_eq!(doc.requirements.len(), 2);
        let first = &doc.requirements[0];
        assert_eq!(
            (first.id.as_str(), first.title.as_str(), first.line),
            ("R1", "Albums can be created and named", 8)
        );
        assert_eq!(
            first.capability.as_deref(),
            Some("albums (adds \"Album creation\")")
        );
        assert_eq!(
            first
                .criteria
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["C1", "C2"]
        );
        assert_eq!(first.criteria[1].line, 13);
    }

    #[test]
    fn the_prose_under_a_heading_is_the_requirement_detail() {
        let doc = parse(SPEC);
        assert_eq!(doc.requirements[0].detail, "A user creates an album.");
        assert_eq!(doc.requirements[1].detail, "");
    }

    #[test]
    fn capability_lines_say_what_they_do_and_to_what() {
        let line = capability_ref("albums (adds \"Album creation\")").unwrap();
        assert_eq!(
            (
                line.capability.as_str(),
                line.action,
                line.requirement.as_deref()
            ),
            ("albums", Action::Adds, Some("Album creation"))
        );
        assert_eq!(
            capability_ref("Photo Albums (changes 'Rename')")
                .unwrap()
                .action,
            Action::Changes
        );
        assert_eq!(
            capability_ref("albums (removes “Old”)")
                .unwrap()
                .requirement
                .as_deref(),
            Some("Old")
        );
        let bare = capability_ref("albums").unwrap();
        assert_eq!((bare.action, bare.requirement), (Action::Adds, None));
        assert_eq!(capability_ref("  "), None);
    }

    #[test]
    fn a_heading_without_an_id_is_numbered_by_position() {
        let second = &parse(SPEC).requirements[1];
        assert_eq!(
            (second.id.as_str(), second.title.as_str()),
            ("R2", "Albums can be deleted")
        );
        assert_eq!(second.criteria[0].id, "C1");
    }

    #[test]
    fn open_questions_skip_none() {
        assert_eq!(
            parse(SPEC).open_questions,
            [(19, "Can albums nest?".to_owned())]
        );
    }

    #[test]
    fn sections_are_listed_lowercase() {
        assert_eq!(
            parse(SPEC).sections,
            ["why", "requirements", "open questions"]
        );
    }

    #[test]
    fn the_sections_around_the_requirements_do_not_leak_into_them() {
        let doc = parse("# A\n\n## User stories\n- US1 (P1): As a user, I want albums. Independent check: create one.\n\n## Requirements\n\n### R1: Albums\nServes US1.\n\n- C1: WHEN x THEN y.\n\n## Success criteria\n- S1: Users create an album in under 30 seconds.\n\n## Key entities\n- Album: a named group of photos.\n\n## Open questions\n- Q1: Can albums nest? Why it matters: navigation. Recommended: A. Options: A) no; B) yes.\n");
        assert_eq!(doc.requirements.len(), 1);
        assert_eq!(doc.requirements[0].criteria.len(), 1);
        assert_eq!(doc.requirements[0].detail, "Serves US1.");
        assert_eq!(doc.open_questions.len(), 1);
        assert!(doc.open_questions[0].1.starts_with("Q1: Can albums nest?"));
    }
}
