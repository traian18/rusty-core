//! `style.md`: prose guidelines. A `##` heading whose whole text is a glob in backticks
//! (`` ## `src/**/*.tsx` ``) starts a section for those files; everything else, including any
//! other heading, belongs to the section it is in, and text before the first glob heading
//! applies to every file.
use serde::Serialize;

use crate::glob::Glob;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StyleSection {
    /// `None`: every file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct StyleGuide {
    sections: Vec<(Option<Glob>, StyleSection)>,
}

fn glob_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## ")?.trim();
    rest.strip_prefix('`')?
        .strip_suffix('`')
        .filter(|g| !g.is_empty())
}

impl StyleGuide {
    pub fn parse(markdown: &str) -> Self {
        let mut sections: Vec<(Option<Glob>, StyleSection)> = vec![(None, StyleSection::default())];
        for line in markdown.lines() {
            if let Some(glob) = glob_heading(line).and_then(|g| Glob::new(g).ok()) {
                let applies = Some(glob.as_str().to_owned());
                sections.push((
                    Some(glob),
                    StyleSection {
                        applies,
                        text: String::new(),
                    },
                ));
                continue;
            }
            let text = &mut sections.last_mut().expect("never empty").1.text;
            text.push_str(line);
            text.push('\n');
        }
        for (_, section) in &mut sections {
            section.text = section.text.trim().to_owned();
        }
        sections.retain(|(_, s)| !s.text.is_empty());
        Self { sections }
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// The sections that apply to `path`, the general one first.
    pub fn for_path(&self, path: &str) -> Vec<&StyleSection> {
        self.sections
            .iter()
            .filter(|(glob, _)| glob.as_ref().is_none_or(|g| g.matches(path)))
            .map(|(_, s)| s)
            .collect()
    }

    /// Every section, for an overview of the guide.
    pub fn sections(&self) -> impl Iterator<Item = &StyleSection> {
        self.sections.iter().map(|(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::StyleGuide;

    #[test]
    fn sections_follow_glob_headings_and_other_headings_stay_inside() {
        let guide = StyleGuide::parse(
            "# Style\nPrefer small modules.\n\n## `src/**/*.tsx`\nOne component per file.\n## Naming\nHooks start with use.\n\n## `**/*.rs`\nNo unwrap.\n",
        );
        let tsx: Vec<_> = guide
            .for_path("src/a/B.tsx")
            .iter()
            .map(|s| s.text.clone())
            .collect();
        assert_eq!(tsx.len(), 2);
        assert!(tsx[0].contains("Prefer small modules"));
        assert!(tsx[1].contains("One component") && tsx[1].contains("Hooks start with use"));
        let rs = guide.for_path("crates/x/src/lib.rs");
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[1].text, "No unwrap.");
    }
}
