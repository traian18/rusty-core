use serde::{Deserialize, Serialize};
use std::path::PathBuf;
/// Inference configuration. Authentication is separate from tool execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeCodeConfig {
    pub credentials_path: Option<PathBuf>,
    pub default_model: String,
}
impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            credentials_path: None,
            default_model: "claude-haiku-4-5-20251001".into(),
        }
    }
}
impl ClaudeCodeConfig {
    pub fn new() -> Self {
        Self::default()
    }
}
