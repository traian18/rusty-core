use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// ChatGPT subscription inference. Login credentials are loaded by the harness;
/// no CLI binary, working directory, or execution flags are accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexConfig {
    pub auth_path: Option<PathBuf>,
    pub default_model: String,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            auth_path: None,
            default_model: "gpt-5.5".into(),
        }
    }
}

impl CodexConfig {
    pub fn new() -> Self {
        Self::default()
    }
}
