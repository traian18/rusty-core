use std::sync::Arc;

use async_trait::async_trait;
use harness_skills::SkillCatalog;
use harness_tools::{
    CancellationToken, ToolDescriptor, ToolError, ToolExecutor, ToolId, ToolInput, ToolResult,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tracing::info;

pub const SKILL_READ: &str = "skill.read";

/// Input for the `skill.read` tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// Name of the skill that bundles the file.
    pub skill: String,
    /// Path of the file, relative to the skill's own directory, as listed
    /// by `skill.load`.
    pub path: String,
    /// First line to return (1-based, inclusive). Omit to start at the top.
    #[serde(default)]
    pub start_line: Option<u32>,
    /// Last line to return (1-based, inclusive). Omit to read to the end.
    #[serde(default)]
    pub end_line: Option<u32>,
    /// Maximum bytes to return; the cut falls on a line boundary and the
    /// result says where to continue. Omit for the exact, complete range.
    #[serde(default)]
    pub max_bytes: Option<u32>,
}

/// A selected slice of a skill file, reported with enough position data for
/// the model to request exactly the next part.
#[derive(Debug, PartialEq)]
struct Slice<'a> {
    content: &'a str,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    next_start_line: Option<usize>,
}

/// Selects `[start_line, end_line]` from `content` and, if `max_bytes` is
/// set, keeps only whole lines that fit (at least one line, cut at a
/// character boundary if that single line is itself too long). Never alters
/// the selected text otherwise, so exact instructions stay exact.
fn select(
    content: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    max_bytes: Option<u32>,
) -> Result<Slice<'_>, String> {
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            content
                .match_indices('\n')
                .map(|(index, _)| index + 1)
                .filter(|&index| index < content.len()),
        )
        .collect();
    let total_lines = if content.is_empty() {
        0
    } else {
        line_starts.len()
    };
    let start = start_line.map_or(1, |n| n as usize);
    let end = end_line.map_or(total_lines, |n| (n as usize).min(total_lines));
    if start == 0 {
        return Err("start_line must be at least 1".to_string());
    }
    if total_lines == 0 {
        return Ok(Slice {
            content: "",
            start_line: 0,
            end_line: 0,
            total_lines,
            next_start_line: None,
        });
    }
    if start > total_lines {
        return Err(format!(
            "start_line {start} is past the end of the file ({total_lines} lines)"
        ));
    }
    if end < start {
        return Err(format!(
            "end_line {} is before start_line {start}",
            end_line.unwrap_or(0)
        ));
    }
    let line_end = |line: usize| {
        if line < total_lines {
            line_starts[line]
        } else {
            content.len()
        }
    };
    let from = line_starts[start - 1];
    let mut last = end;
    if let Some(max_bytes) = max_bytes.map(|n| n.max(1) as usize) {
        while last > start && line_end(last) - from > max_bytes {
            last -= 1;
        }
        if line_end(last) - from > max_bytes {
            let mut cut = from + max_bytes;
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            let next = if start < total_lines {
                Some(start + 1)
            } else {
                None
            };
            return Ok(Slice {
                content: &content[from..cut],
                start_line: start,
                end_line: start,
                total_lines,
                next_start_line: next,
            });
        }
    }
    let next_start_line = (last < end).then_some(last + 1);
    Ok(Slice {
        content: &content[from..line_end(last)],
        start_line: start,
        end_line: last,
        total_lines,
        next_start_line,
    })
}

/// Reads a file bundled inside one skill's directory.
///
/// Deliberately separate from `fs.read`: skill directories — especially the
/// user-level `$HOME/.harness/skills` — sit outside the workspace root,
/// where `FsWorkspace`'s traversal guard correctly refuses to read. Rather
/// than widen that guard, this tool grants a second, much narrower scope:
/// one skill's own directory, enforced by `Skill::read_bundled`.
pub struct SkillReadTool {
    catalog: Arc<SkillCatalog>,
}

impl SkillReadTool {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl ToolExecutor for SkillReadTool {
    fn descriptor(&self) -> ToolDescriptor {
        let schema = schemars::schema_for!(ReadInput);
        ToolDescriptor {
            id: ToolId::new(SKILL_READ),
            name: "Read skill file".to_string(),
            description: "Read a file bundled with a skill, using a path relative to that skill's \
                 directory as reported by skill.load. Returns the exact file by default; use \
                 start_line/end_line and max_bytes to read a large file in parts."
                .to_string(),
            input_schema: serde_json::to_value(schema).unwrap_or(json!({})),
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let input: ReadInput = input.parse().map_err(|_| ToolError::ExecutionFailed)?;

        if cancel.is_cancelled() {
            return Err(ToolError::Timeout);
        }

        info!(skill = %input.skill, path = %input.path, "skill.read: reading bundled file");

        let Some(skill) = self.catalog.get(&input.skill) else {
            return Ok(error_result(format!("no skill named {:?}", input.skill)));
        };

        match skill.read_bundled(&input.path).await {
            Ok(content) => {
                let ranged = input.start_line.is_some()
                    || input.end_line.is_some()
                    || input.max_bytes.is_some();
                if !ranged {
                    // Unchanged exact read: skills hold instructions and templates
                    // that must not be silently cut.
                    return Ok(ToolResult {
                        call_id: SKILL_READ.to_string(),
                        output: json!({ "content": content }),
                        is_error: false,
                    });
                }
                match select(&content, input.start_line, input.end_line, input.max_bytes) {
                    Ok(slice) => {
                        let mut output = json!({
                            "content": slice.content,
                            "start_line": slice.start_line,
                            "end_line": slice.end_line,
                            "total_lines": slice.total_lines,
                        });
                        if let Some(next) = slice.next_start_line {
                            output["truncated"] = json!(true);
                            output["next_start_line"] = json!(next);
                        }
                        Ok(ToolResult {
                            call_id: SKILL_READ.to_string(),
                            output,
                            is_error: false,
                        })
                    }
                    Err(message) => Ok(error_result(message)),
                }
            }
            // Includes the refusals from `read_bundled`'s scoping checks. A
            // rejected path is a logical error the model can see and correct,
            // not an infrastructure fault that should abort the run.
            Err(error) => Ok(error_result(error.to_string())),
        }
    }
}

fn error_result(message: String) -> ToolResult {
    ToolResult {
        call_id: SKILL_READ.to_string(),
        output: json!({ "error": message }),
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "one\ntwo\nthree\nfour\n";

    #[test]
    fn selects_an_inclusive_line_range_verbatim() {
        let slice = select(FILE, Some(2), Some(3), None).unwrap();
        assert_eq!(slice.content, "two\nthree\n");
        assert_eq!(
            (
                slice.start_line,
                slice.end_line,
                slice.total_lines,
                slice.next_start_line
            ),
            (2, 3, 4, None)
        );
        assert_eq!(
            select(FILE, Some(3), None, None).unwrap().content,
            "three\nfour\n"
        );
        assert_eq!(select(FILE, None, Some(99), None).unwrap().content, FILE);
    }

    #[test]
    fn max_bytes_keeps_whole_lines_and_says_where_to_continue() {
        let slice = select(FILE, None, None, Some(9)).unwrap();
        assert_eq!(slice.content, "one\ntwo\n");
        assert_eq!((slice.end_line, slice.next_start_line), (2, Some(3)));
        let rest = select(FILE, Some(3), None, Some(100)).unwrap();
        assert_eq!(format!("{}{}", slice.content, rest.content), FILE);
    }

    #[test]
    fn a_single_oversized_line_is_cut_on_a_character_boundary() {
        let slice = select("€€€€\nnext\n", None, None, Some(4)).unwrap();
        assert_eq!(slice.content, "€");
        assert_eq!(slice.next_start_line, Some(2));
    }

    #[test]
    fn handles_files_without_a_trailing_newline_and_empty_files() {
        assert_eq!(select("a\nb", Some(2), None, None).unwrap().content, "b");
        assert_eq!(select("a\nb", None, None, None).unwrap().total_lines, 2);
        assert_eq!(select("", None, None, None).unwrap().total_lines, 0);
    }

    #[test]
    fn rejects_impossible_ranges() {
        assert!(select(FILE, Some(0), None, None)
            .unwrap_err()
            .contains("at least 1"));
        assert!(select(FILE, Some(9), None, None)
            .unwrap_err()
            .contains("past the end"));
        assert!(select(FILE, Some(3), Some(2), None)
            .unwrap_err()
            .contains("before start_line"));
    }
}
