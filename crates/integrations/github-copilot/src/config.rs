use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GitHubCopilotConfig {
    pub binary_path: PathBuf,
    pub model: String,
    pub working_dir: Option<PathBuf>,
}

impl Default for GitHubCopilotConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("copilot"),
            model: "auto".into(),
            working_dir: None,
        }
    }
}
