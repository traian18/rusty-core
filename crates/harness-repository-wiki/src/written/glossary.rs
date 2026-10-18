//! The repository's own words, defined in its own sentences, with no model in the path: a
//! glossary is the page a reader trusts because they do not know the answer, so an invented
//! definition does the most harm there. Terms come from definition-shaped lines of the
//! repository's documents (`- **Term** — meaning`, `- \`term\`: meaning`, `**Term**: meaning`).
use std::collections::BTreeMap;

pub const DOCUMENTS: [&str; 12] = [
    "README.md",
    "Readme.md",
    "readme.md",
    "ARCHITECTURE.md",
    "CONTRIBUTING.md",
    "GLOSSARY.md",
    "docs/README.md",
    "docs/index.md",
    "docs/architecture.md",
    "docs/glossary.md",
    "docs/concepts.md",
    "docs/overview.md",
];
/// Fewer terms than this make no page.
pub const MIN_TERMS: usize = 3;
const MAX_TERMS: usize = 80;
const MAX_TERM_CHARS: usize = 40;
const MAX_TERM_WORDS: usize = 5;
const MAX_DEFINITION_CHARS: usize = 240;
const SEPARATORS: [&str; 5] = [" — ", " – ", " - ", ": ", ":"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub term: String,
    pub definition: Option<String>,
    pub source: String,
}

/// `**Term**` or `` `Term` `` at the start of `text`, and what follows it.
fn marked(text: &str) -> Option<(&str, &str)> {
    for (open, close) in [("**", "**"), ("`", "`")] {
        if let Some(rest) = text.strip_prefix(open) {
            let end = rest.find(close)?;
            return Some((&rest[..end], &rest[end + close.len()..]));
        }
    }
    None
}

fn definition(rest: &str) -> Option<String> {
    let rest = rest.trim_start_matches(['*', '`']);
    let after = SEPARATORS.iter().find_map(|sep| {
        rest.strip_prefix(sep.trim_start())
            .or_else(|| rest.strip_prefix(sep))
    })?;
    let text = after.trim();
    if text.is_empty() {
        return None;
    }
    let end = text.find(". ").map_or(text.len(), |i| i + 1);
    let sentence: String = text[..end].chars().take(MAX_DEFINITION_CHARS).collect();
    Some(sentence.trim().to_owned())
}

pub fn mine(documents: &[(String, String)]) -> Vec<Term> {
    let mut terms: BTreeMap<String, Term> = BTreeMap::new();
    for (path, text) in documents {
        let mut fenced = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            let item = t
                .strip_prefix("- ")
                .or_else(|| t.strip_prefix("* "))
                .unwrap_or(t);
            let Some((term, rest)) = marked(item) else {
                continue;
            };
            let term = term.trim().trim_end_matches(':');
            if term.is_empty()
                || term.len() > MAX_TERM_CHARS
                || term.split_whitespace().count() > MAX_TERM_WORDS
                || term.contains('/')
            {
                continue;
            }
            let Some(definition) = definition(rest) else {
                continue;
            };
            terms.entry(term.to_lowercase()).or_insert(Term {
                term: term.to_owned(),
                definition: Some(definition),
                source: path.clone(),
            });
            if terms.len() >= MAX_TERMS {
                break;
            }
        }
    }
    terms.into_values().collect()
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

pub fn render(name: &str, terms: &[Term]) -> String {
    let mut out = format!("The words {name} uses for itself, each defined in the repository's own sentence and linked to where that sentence is. Nothing here was written by a model.\n\n| Term | Meaning | Where |\n|---|---|---|\n");
    for t in terms {
        out.push_str(&format!(
            "| **{}** | {} | `{}` |\n",
            cell(&t.term),
            cell(t.definition.as_deref().unwrap_or("—")),
            t.source
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mines_definition_shaped_lines_only() {
        let doc = "# Rusty\n\n- **Canvas** — the board where tasks are planned. It has nodes.\n- **Skill**: a reusable agent profile\n- `repo_context` - the read-only repository tool\n- Plain bullet that defines nothing\n- **src/a.rs** — a path is not a term\n**Snapshot**: an immutable index of the repository.\n```\n- **Hidden** — inside code\n```\n";
        let terms = mine(&[("README.md".into(), doc.into())]);
        let names: Vec<&str> = terms.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(names, ["Canvas", "repo_context", "Skill", "Snapshot"]);
        assert_eq!(
            terms[0].definition.as_deref(),
            Some("the board where tasks are planned.")
        );
        assert!(render("Rusty", &terms)
            .contains("| **Skill** | a reusable agent profile | `README.md` |"));
    }
}
