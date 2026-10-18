//! What a repository says about itself: the headings and opening sentence of each section of its
//! README, for vocabulary and purpose (never for paths or counts), and the sections that tell a
//! newcomer how to set it up, kept verbatim for the commands in them.

/// The digest stays small: a long README handed to a writer outvotes the structure.
const DIGEST_CHARS: usize = 2_500;
const SETUP_CHARS: usize = 4_000;
const SETUP_WORDS: [&str; 14] = [
    "install",
    "setup",
    "set up",
    "getting started",
    "quick start",
    "quickstart",
    "build",
    "usage",
    "run",
    "test",
    "develop",
    "requirement",
    "prerequisite",
    "contributing",
];

pub const README_NAMES: [&str; 6] = [
    "README.md",
    "Readme.md",
    "readme.md",
    "README",
    "README.rst",
    "README.txt",
];

pub struct Section<'a> {
    pub heading: &'a str,
    pub level: usize,
    pub body: Vec<&'a str>,
}

/// Splits Markdown into sections by heading, skipping fenced code when looking for headings.
pub fn sections(text: &str) -> Vec<Section<'_>> {
    let mut out = vec![Section {
        heading: "",
        level: 0,
        body: Vec::new(),
    }];
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let trimmed = line.trim_start();
        if !fenced && trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let heading = trimmed[level..].trim();
            if (1..=6).contains(&level) && !heading.is_empty() {
                out.push(Section {
                    heading,
                    level,
                    body: Vec::new(),
                });
                continue;
            }
        }
        if let Some(last) = out.last_mut() {
            last.body.push(line);
        }
    }
    out
}

/// The first sentence of prose in a section body (not code, not badges, not tables).
fn opener(body: &[&str]) -> Option<String> {
    let mut fenced = false;
    let mut paragraph = String::new();
    for line in body {
        let t = line.trim();
        if t.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced
            || t.starts_with('|')
            || t.starts_with("![")
            || t.starts_with("<")
            || t.starts_with("[![")
        {
            continue;
        }
        if t.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push_str(t.trim_start_matches(['-', '*', '>', ' ']));
        paragraph.push(' ');
    }
    let paragraph = paragraph.trim();
    if paragraph.is_empty() {
        return None;
    }
    let end = paragraph.find(". ").map_or(paragraph.len(), |i| i + 1);
    Some(paragraph[..end].chars().take(300).collect())
}

/// Headings and opening sentences, within the digest budget.
pub fn digest(text: &str) -> String {
    let mut out = String::new();
    for section in sections(text) {
        let mut entry = String::new();
        if !section.heading.is_empty() {
            entry.push_str(&format!(
                "{} {}\n",
                "#".repeat(section.level.min(4)),
                section.heading
            ));
        }
        if let Some(sentence) = opener(&section.body) {
            entry.push_str(&sentence);
            entry.push('\n');
        }
        if out.len() + entry.len() > DIGEST_CHARS {
            break;
        }
        out.push_str(&entry);
    }
    out
}

/// Sections about installing, building, running or testing, verbatim, within the setup budget.
pub fn setup_sections(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut used = 0;
    for section in sections(text) {
        let heading = section.heading.to_lowercase();
        if !SETUP_WORDS.iter().any(|w| heading.contains(w)) {
            continue;
        }
        let body = section.body.join("\n").trim().to_owned();
        if body.is_empty() || used + body.len() > SETUP_CHARS {
            continue;
        }
        used += body.len();
        out.push((section.heading.to_owned(), body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const README: &str = "# Rusty\n\n[![ci](x)](y)\n\nRusty is an IDE that plans work as a graph. It runs agents.\n\n## Install\n\n```sh\nnpm install\n```\n\n## Architecture\n\nThe engine drives sessions.\n\n```\n# not a heading\n```\n\n## Testing\n\nRun `cargo test`.\n";

    #[test]
    fn digest_keeps_headings_and_first_sentences() {
        let d = digest(README);
        assert!(d.contains("# Rusty\nRusty is an IDE that plans work as a graph."));
        assert!(d.contains("## Architecture\nThe engine drives sessions."));
        assert!(!d.contains("[![ci]"));
        assert!(!d.contains("It runs agents"), "first sentence only");
    }

    #[test]
    fn setup_sections_are_kept_verbatim() {
        let s = setup_sections(README);
        let headings: Vec<&str> = s.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(headings, ["Install", "Testing"]);
        assert!(s[0].1.contains("npm install"));
        assert_eq!(
            sections(README)
                .iter()
                .filter(|s| s.heading == "not a heading")
                .count(),
            0
        );
    }
}
