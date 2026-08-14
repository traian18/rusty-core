//! A deliberately small YAML-frontmatter reader.
//!
//! `SKILL.md` frontmatter is a flat block of `key: value` pairs and short
//! string lists — no nesting, no anchors, no multi-document streams. Parsing
//! that subset by hand is ~100 lines; taking a real YAML parser to do it
//! would mean a dependency the workspace has no other use for. `serde_yaml`
//! is archived upstream, and `deny.toml` sets `yanked = "deny"` while
//! carrying exactly one grudging advisory exception — adding a second is a
//! worse trade than this module. It also matches how
//! `harness-tool-mcp`'s JSON-RPC client was written from scratch rather than
//! pulling `rmcp` (which `xtask check-deps` bans from core outright).
//!
//! Anything outside the supported subset is *ignored*, not rejected: unknown
//! keys are dropped so a skill written for a future field still loads today.

use std::collections::BTreeMap;

/// One frontmatter value. Scalars and flat string lists are the only shapes
/// a `SKILL.md` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Scalar(String),
    List(Vec<String>),
}

impl Value {
    /// A list reads as a list; a scalar reads as a one-element list. Lets
    /// `allowed-tools: fs.read` and `allowed-tools: [fs.read]` mean the same
    /// thing, which is what an author will expect.
    pub(crate) fn into_list(self) -> Vec<String> {
        match self {
            Value::Scalar(value) if value.is_empty() => Vec::new(),
            Value::Scalar(value) => vec![value],
            Value::List(values) => values,
        }
    }

    pub(crate) fn as_scalar(&self) -> Option<&str> {
        match self {
            Value::Scalar(value) => Some(value),
            Value::List(_) => None,
        }
    }
}

/// A parsed `SKILL.md`: its frontmatter fields and the markdown body that
/// followed the closing fence.
#[derive(Debug, Clone)]
pub(crate) struct Document {
    pub(crate) fields: BTreeMap<String, Value>,
    pub(crate) body: String,
}

impl Document {
    pub(crate) fn scalar(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(Value::as_scalar)
    }

    pub(crate) fn list(&self, key: &str) -> Vec<String> {
        self.fields
            .get(key)
            .cloned()
            .map(Value::into_list)
            .unwrap_or_default()
    }
}

/// Splits `text` into frontmatter fields and body.
///
/// Returns `None` when there is no frontmatter block at all — the caller
/// turns that into a `MissingFrontmatter` error carrying the file path,
/// which this module has no business knowing about.
pub(crate) fn parse(text: &str) -> Option<Document> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = strip_fence(text)?;

    let mut fields = BTreeMap::new();
    let mut pending_list: Option<(String, Vec<String>)> = None;
    let mut body_start = rest.len();

    for line in LineIter::new(rest) {
        let content = line.text;
        let trimmed = content.trim();

        if is_fence(trimmed) {
            body_start = line.end;
            break;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // A `- item` line continues the block list opened by the last bare
        // `key:` line. Anything else closes it.
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| (trimmed == "-").then_some(""))
        {
            if let Some((_, items)) = pending_list.as_mut() {
                items.push(unquote(item.trim()));
                continue;
            }
        }

        if let Some((key, items)) = pending_list.take() {
            fields.insert(key, Value::List(items));
        }

        let Some((key, value)) = split_pair(trimmed) else {
            continue;
        };

        if value.is_empty() {
            // Either the head of a block list or a genuinely empty scalar;
            // which one is only knowable from the next line.
            pending_list = Some((key, Vec::new()));
        } else if let Some(items) = parse_inline_list(value) {
            fields.insert(key, Value::List(items));
        } else {
            fields.insert(key, Value::Scalar(unquote(value)));
        }
    }

    if let Some((key, items)) = pending_list.take() {
        fields.insert(
            key,
            if items.is_empty() {
                Value::Scalar(String::new())
            } else {
                Value::List(items)
            },
        );
    }

    Some(Document {
        fields,
        body: rest[body_start..].trim_start_matches('\n').to_string(),
    })
}

/// Consumes the opening `---` fence, returning everything after it.
fn strip_fence(text: &str) -> Option<&str> {
    let first_line_end = text.find('\n').unwrap_or(text.len());
    if !is_fence(text[..first_line_end].trim()) {
        return None;
    }
    Some(text.get(first_line_end + 1..).unwrap_or(""))
}

fn is_fence(trimmed: &str) -> bool {
    trimmed == "---"
}

/// Splits `key: value`, tolerating a missing space after the colon. Returns
/// `None` for a line with no colon at all, which is malformed and skipped.
fn split_pair(line: &str) -> Option<(String, &str)> {
    let colon = line.find(':')?;
    let key = line[..colon].trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_ascii_lowercase(), line[colon + 1..].trim()))
}

/// `[a, b, c]` → `["a", "b", "c"]`. `None` when `value` isn't bracketed.
fn parse_inline_list(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(
        inner
            .split(',')
            .map(|item| unquote(item.trim()))
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// Strips one matching pair of surrounding quotes. No escape processing —
/// nothing in a `SKILL.md` header needs it, and pretending otherwise would
/// mean implementing YAML string semantics for real.
fn unquote(value: &str) -> String {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// Yields each line along with the byte offset just past its newline, so the
/// caller can slice the body without re-scanning.
struct LineIter<'a> {
    text: &'a str,
    offset: usize,
}

struct Line<'a> {
    text: &'a str,
    end: usize,
}

impl<'a> LineIter<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, offset: 0 }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = Line<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.text.len() {
            return None;
        }
        let remainder = &self.text[self.offset..];
        let (text, end) = match remainder.find('\n') {
            Some(index) => (&remainder[..index], self.offset + index + 1),
            None => (remainder, self.text.len()),
        };
        self.offset = end;
        Some(Line {
            text: text.strip_suffix('\r').unwrap_or(text),
            end,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_body() {
        let document = parse("---\nname: pdf-report\ndescription: Make a PDF.\n---\nBody text.\n")
            .expect("frontmatter should parse");
        assert_eq!(document.scalar("name"), Some("pdf-report"));
        assert_eq!(document.scalar("description"), Some("Make a PDF."));
        assert_eq!(document.body, "Body text.\n");
    }

    #[test]
    fn text_without_an_opening_fence_is_not_frontmatter() {
        assert!(parse("# Just markdown\n").is_none());
        assert!(parse("name: pdf-report\n").is_none());
    }

    #[test]
    fn parses_inline_and_block_lists() {
        let inline = parse("---\nallowed-tools: [fs.read, shell.exec]\n---\n").expect("inline");
        assert_eq!(
            inline.list("allowed-tools"),
            vec!["fs.read".to_string(), "shell.exec".to_string()]
        );

        let block =
            parse("---\nallowed-tools:\n  - fs.read\n  - shell.exec\n---\n").expect("block");
        assert_eq!(
            block.list("allowed-tools"),
            vec!["fs.read".to_string(), "shell.exec".to_string()]
        );
    }

    #[test]
    fn a_bare_scalar_reads_as_a_one_element_list() {
        let document = parse("---\nallowed-tools: fs.read\n---\n").expect("scalar");
        assert_eq!(document.list("allowed-tools"), vec!["fs.read".to_string()]);
    }

    #[test]
    fn strips_quotes_and_ignores_comments_and_unknown_keys() {
        let document = parse(
            "---\n# a comment\ndescription: \"Quoted: with a colon\"\nfuture-field: whatever\n---\n",
        )
        .expect("frontmatter should parse");
        assert_eq!(document.scalar("description"), Some("Quoted: with a colon"));
        assert_eq!(document.scalar("future-field"), Some("whatever"));
    }

    #[test]
    fn a_block_list_is_closed_by_the_next_key() {
        let document = parse("---\nallowed-tools:\n  - fs.read\nname: demo\n---\n").expect("parse");
        assert_eq!(document.list("allowed-tools"), vec!["fs.read".to_string()]);
        assert_eq!(document.scalar("name"), Some("demo"));
    }

    #[test]
    fn an_unterminated_fence_yields_fields_and_an_empty_body() {
        let document = parse("---\nname: demo\n").expect("parse");
        assert_eq!(document.scalar("name"), Some("demo"));
        assert_eq!(document.body, "");
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let document =
            parse("---\r\nname: demo\r\ndescription: Hi.\r\n---\r\nBody.\r\n").expect("parse");
        assert_eq!(document.scalar("name"), Some("demo"));
        assert_eq!(document.scalar("description"), Some("Hi."));
    }

    #[test]
    fn keys_are_case_insensitive() {
        let document = parse("---\nName: demo\nDESCRIPTION: Hi.\n---\n").expect("parse");
        assert_eq!(document.scalar("name"), Some("demo"));
        assert_eq!(document.scalar("description"), Some("Hi."));
    }
}
