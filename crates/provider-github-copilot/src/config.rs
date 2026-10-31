use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitHubCopilotConfig {
    pub credentials_path: Option<PathBuf>,
    pub default_model: String,
    pub github_host: String,
}
impl Default for GitHubCopilotConfig {
    fn default() -> Self {
        Self {
            credentials_path: None,
            default_model: "gpt-4.1".into(),
            github_host: std::env::var("COPILOT_GH_HOST").unwrap_or_else(|_| "github.com".into()),
        }
    }
}
