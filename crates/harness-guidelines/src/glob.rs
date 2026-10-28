//! Globs over repository-relative paths: `*` (within one segment), `**` (any number of
//! segments), `?` (one character) and `{a,b}` (alternatives). A glob without a `/` matches the
//! file name in any folder, as in `.gitignore`.

/// A compiled glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    source: String,
    /// The glob with `{a,b}` expanded, each split into segments.
    alternatives: Vec<Vec<String>>,
}

impl Glob {
    pub fn new(pattern: &str) -> Result<Self, String> {
        let trimmed = pattern.trim().trim_start_matches("./");
        if trimmed.is_empty() {
            return Err("an empty glob".into());
        }
        let anchored = if trimmed.contains('/') {
            trimmed.trim_start_matches('/').to_owned()
        } else {
            format!("**/{trimmed}")
        };
        let alternatives = expand(&anchored)?
            .into_iter()
            .map(|alt| alt.split('/').map(str::to_owned).collect())
            .collect();
        Ok(Self {
            source: pattern.to_owned(),
            alternatives,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    pub fn matches(&self, path: &str) -> bool {
        let path = path.trim_start_matches("./").replace('\\', "/");
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        self.alternatives
            .iter()
            .any(|glob| match_segments(glob, &segments))
    }
}

/// `{a,b}` groups expanded into every combination; groups do not nest.
fn expand(pattern: &str) -> Result<Vec<String>, String> {
    let Some(open) = pattern.find('{') else {
        return Ok(vec![pattern.to_owned()]);
    };
    let close = pattern[open..]
        .find('}')
        .map(|i| open + i)
        .ok_or_else(|| format!("unclosed '{{' in '{pattern}'"))?;
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    let mut out = Vec::new();
    for choice in pattern[open + 1..close].split(',') {
        out.extend(expand(&format!("{head}{choice}{tail}"))?);
    }
    Ok(out)
}

fn match_segments(glob: &[String], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((segment, others)) => {
                match_segment(first.as_bytes(), segment.as_bytes()) && match_segments(rest, others)
            }
            None => false,
        },
    }
}

fn match_segment(glob: &[u8], text: &[u8]) -> bool {
    match glob.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|skip| match_segment(rest, &text[skip..])),
        Some((b'?', rest)) => !text.is_empty() && match_segment(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && match_segment(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::Glob;

    fn matches(glob: &str, path: &str) -> bool {
        Glob::new(glob).unwrap().matches(path)
    }

    #[test]
    fn stars_stay_in_one_segment_and_double_stars_cross_them() {
        assert!(matches("src/*.ts", "src/a.ts"));
        assert!(!matches("src/*.ts", "src/x/a.ts"));
        assert!(matches("src/**/*.ts", "src/a.ts"));
        assert!(matches("src/**/*.ts", "src/x/y/a.ts"));
        assert!(matches("src/**", "src/x/y/a.ts"));
        assert!(!matches("src/**/*.ts", "lib/a.ts"));
    }

    #[test]
    fn a_glob_without_a_slash_matches_the_name_anywhere() {
        assert!(matches("*.test.ts", "src/deep/a.test.ts"));
        assert!(matches("tests", "crates/x/tests"));
        assert!(!matches("*.test.ts", "src/a.ts"));
    }

    #[test]
    fn alternatives_and_question_marks() {
        assert!(matches("**/*.{rs,ts}", "a/b.rs"));
        assert!(matches("**/*.{rs,ts}", "b.ts"));
        assert!(!matches("**/*.{rs,ts}", "b.py"));
        assert!(matches("a?.rs", "ab.rs"));
        assert!(!matches("a?.rs", "a.rs"));
        assert!(Glob::new("src/{a").is_err());
    }
}
