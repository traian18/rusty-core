//! The `---` block at the top of `change.md`: flat `key: value` lines and `[a, b]` lists.
use std::collections::BTreeMap;

use crate::model::{ChangeFront, Status};

/// The fields and how many lines the block takes (0 when there is none).
pub fn split(text: &str) -> (BTreeMap<String, String>, usize) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (BTreeMap::new(), 0);
    }
    let mut fields = BTreeMap::new();
    for (index, line) in lines.enumerate() {
        if line.trim() == "---" {
            return (fields, index + 2);
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(
                key.trim().to_ascii_lowercase(),
                unquote(value.trim()).to_owned(),
            );
        }
    }
    (BTreeMap::new(), 0)
}

fn unquote(value: &str) -> &str {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn list(value: &str) -> Vec<String> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| unquote(item.trim()).to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

pub fn change(text: &str) -> ChangeFront {
    let (fields, _) = split(text);
    let get = |key: &str| fields.get(key).filter(|v| !v.is_empty()).cloned();
    let status = get("status");
    ChangeFront {
        id: get("id"),
        title: get("title"),
        unknown_status: status.clone().filter(|word| Status::parse(word).is_none()),
        status: status.as_deref().and_then(Status::parse),
        created: get("created"),
        branch: get("branch"),
        capabilities: get("capabilities").map(|v| list(&v)).unwrap_or_default(),
    }
}

/// The text of `change.md` for a new change.
pub fn render_change(front: &ChangeFront, history: &[String]) -> String {
    let mut out = String::from("---\n");
    let mut field = |key: &str, value: &str| out.push_str(&format!("{key}: {value}\n"));
    field(
        "id",
        &format!("\"{}\"", front.id.clone().unwrap_or_default()),
    );
    field("title", &front.title.clone().unwrap_or_default());
    field("status", front.status.unwrap_or_default().word());
    field("created", &front.created.clone().unwrap_or_default());
    if let Some(branch) = &front.branch {
        field("branch", branch);
    }
    field(
        "capabilities",
        &format!("[{}]", front.capabilities.join(", ")),
    );
    out.push_str("---\n");
    out.push_str(&format!(
        "# {}\n\n## History\n",
        front.title.clone().unwrap_or_default()
    ));
    for entry in history {
        out.push_str(&format!("- {entry}\n"));
    }
    out
}

/// `text` with the front matter field `key` set; the field is added when missing and a block is
/// created when there is none. Everything else is untouched.
pub fn set_field(text: &str, key: &str, value: &str) -> String {
    let (fields, consumed) = split(text);
    let line = format!("{key}: {value}");
    if consumed == 0 {
        return format!("---\n{line}\n---\n{text}");
    }
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let existing = lines[1..consumed - 1].iter().position(|l| {
        l.split_once(':')
            .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
    });
    match existing {
        Some(at) => lines[at + 1] = line,
        None => lines.insert(consumed - 1, line),
    }
    let _ = fields;
    finish(lines, text)
}

/// `text` with `- entry` added at the end of the `## History` section (created when missing).
pub fn add_history(text: &str, entry: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let bullet = format!("- {entry}");
    match lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case("## history"))
    {
        Some(start) => {
            let end = lines[start + 1..]
                .iter()
                .position(|l| l.starts_with('#'))
                .map_or(lines.len(), |at| start + 1 + at);
            let mut at = end;
            while at > start + 1 && lines[at - 1].trim().is_empty() {
                at -= 1;
            }
            lines.insert(at, bullet);
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("## History".into());
            lines.push(bullet);
        }
    }
    finish(lines, text)
}

fn finish(lines: Vec<String>, original: &str) -> String {
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = lines.join(newline);
    out.push_str(newline);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_lists_and_quotes() {
        let text = "---\nid: \"0007\"\ntitle: Photo albums\nstatus: Planned\nbranch: spec/0007-photo-albums\ncapabilities: [albums, \"sidebar\"]\n---\n# Photo albums\n";
        let front = change(text);
        assert_eq!(front.id.as_deref(), Some("0007"));
        assert_eq!(front.status, Some(Status::Planned));
        assert_eq!(front.branch.as_deref(), Some("spec/0007-photo-albums"));
        assert_eq!(front.capabilities, ["albums", "sidebar"]);
        assert_eq!(split(text).1, 7);
    }

    #[test]
    fn an_unknown_status_is_kept_for_a_warning() {
        let front = change("---\nstatus: wip\n---\n");
        assert_eq!(front.status, None);
        assert_eq!(front.unknown_status.as_deref(), Some("wip"));
    }

    #[test]
    fn no_block_or_an_unclosed_one_means_no_fields() {
        assert_eq!(split("# Title\n").1, 0);
        assert_eq!(split("---\nid: 1\n").1, 0);
    }

    #[test]
    fn what_is_rendered_reads_back() {
        let front = ChangeFront {
            id: Some("0003".into()),
            title: Some("Dark mode".into()),
            status: Some(Status::Draft),
            created: Some("2026-10-23".into()),
            branch: Some("spec/0003-dark-mode".into()),
            capabilities: vec!["theme".into()],
            ..Default::default()
        };
        let text = render_change(&front, &["2026-10-23 Created.".into()]);
        assert_eq!(change(&text), front);
        assert!(text.contains("- 2026-10-23 Created."));
    }

    #[test]
    fn a_field_is_replaced_added_or_started() {
        let text = "---\nid: \"0001\"\nstatus: draft\n---\n# A\n";
        assert_eq!(
            set_field(text, "status", "planned"),
            "---\nid: \"0001\"\nstatus: planned\n---\n# A\n"
        );
        assert!(
            set_field(text, "branch", "spec/x").contains("status: draft\nbranch: spec/x\n---\n")
        );
        assert_eq!(
            change(&set_field("# A\n", "status", "done")).status,
            Some(Status::Done)
        );
    }

    #[test]
    fn history_entries_go_at_the_end_of_their_section() {
        let text = "---\nid: 1\n---\n# A\n\n## History\n- one\n\n## Other\ntext\n";
        let out = add_history(text, "two");
        assert!(out.contains("## History\n- one\n- two\n\n## Other"));
        assert!(add_history("# A\n", "x").ends_with("## History\n- x\n"));
    }
}
