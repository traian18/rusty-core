//! Finding indexed files named in a message.
use harness_protocol::messages::{AgentMessage, ContentBlock, MessageRole};
use harness_workspace::{import_graph::ImportIndex, repository_intelligence::RootRelativePath};

/// The text of the most recent user message, or empty when there is none.
pub(crate) fn latest_user_text(messages: &[AgentMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Files in `text` that exactly match an indexed path, in order of first mention.
///
/// Matching is exact on the workspace-relative path (a leading `./` is ignored). There is no
/// fuzzy matching, so the same message always selects the same files.
pub(crate) fn mentioned_files(
    text: &str,
    imports: &ImportIndex,
    limit: usize,
) -> Vec<RootRelativePath> {
    let mut found: Vec<RootRelativePath> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || "\"'`()[]<>,;:{}".contains(c)) {
        let token = token.trim_end_matches(['.', '?', '!']);
        let Ok(path) = RootRelativePath::new(token) else {
            continue;
        };
        if imports.has_file(&path) && !found.contains(&path) {
            found.push(path);
            if found.len() == limit {
                break;
            }
        }
    }
    found
}
