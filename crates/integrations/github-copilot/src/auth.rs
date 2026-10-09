use async_trait::async_trait;
use harness_model::{auth::InferenceAuth, ModelError};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;
use std::path::PathBuf;

/// API version used by OpenCode's direct Copilot integration. It does not
/// make the direct API's catalog equivalent to the Copilot CLI catalog.
const COPILOT_API_VERSION: &str = "2026-06-01";
// Custom OAuth apps need the developer integration route. Without this header
// the same valid credential reaches Copilot's legacy-only model catalog.
const COPILOT_INTEGRATION_ID: &str = "copilot-developer-cli";

pub struct CopilotAuth {
    path: Option<PathBuf>,
    host: String,
}
/// A token ready to send, or Rusty's own expiring credential that has to be
/// refreshed (asynchronously) first.
enum LoadedToken {
    Ready(String),
    Refresh(PathBuf),
}
impl CopilotAuth {
    pub fn new(path: Option<PathBuf>, host: String) -> Self {
        Self { path, host }
    }
    fn load_token(&self) -> Result<LoadedToken, ModelError> {
        if self.path.is_none() {
            for name in ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
                if let Ok(token) = std::env::var(name) {
                    if !token.is_empty() {
                        return Ok(LoadedToken::Ready(token));
                    }
                }
            }
        }
        let path = self
            .path
            .clone()
            .or_else(|| {
                std::env::var_os("COPILOT_HOME").map(|p| PathBuf::from(p).join("config.json"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|h| PathBuf::from(h).join(".copilot/config.json"))
            })
            .ok_or_else(|| error("Cannot locate Copilot sign-in."))?;
        let data = std::fs::read_to_string(&path).map_err(|_| {
            error("No Copilot credentials found. Sign in in Settings or set COPILOT_GITHUB_TOKEN.")
        })?;
        let data = data
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let value: Value = serde_json::from_str(&data)
            .map_err(|_| error("Invalid Copilot credential configuration."))?;
        if value["format"] == crate::credentials::CREDENTIAL_FORMAT {
            return match crate::credentials::load_token(&path, &self.host)
                .map_err(|message| error(&message))?
            {
                Some(token) => Ok(LoadedToken::Ready(token)),
                None => Ok(LoadedToken::Refresh(path)),
            };
        }
        let (account, token) = selected_credential(&value, &self.host)?;
        if let Some(token) = token {
            return Ok(LoadedToken::Ready(token));
        }
        #[cfg(target_os = "macos")]
        if self.path.is_none() {
            if let Ok(token) =
                security_framework::passwords::get_generic_password("copilot-cli", &account)
            {
                return String::from_utf8(token)
                    .map(LoadedToken::Ready)
                    .map_err(|_| error("Invalid Copilot keychain token."));
            }
        }
        let _ = account;
        Err(error("No Copilot token for the selected account. Sign in in Settings or set COPILOT_GITHUB_TOKEN."))
    }
}
fn selected_credential(
    value: &Value,
    expected_host: &str,
) -> Result<(String, Option<String>), ModelError> {
    let users = value["loggedInUsers"].as_array();
    let selected = value
        .get("lastLoggedInUser")
        .filter(|u| u.is_object())
        .or_else(|| users.filter(|u| u.len() == 1).and_then(|u| u.first()))
        .ok_or_else(|| error("No selected Copilot account. Sign in in Settings."))?;
    let login = selected["login"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("Missing Copilot account name."))?;
    let host = selected["host"]
        .as_str()
        .unwrap_or("https://github.com")
        .trim_end_matches('/');
    if host.trim_start_matches("https://")
        != expected_host
            .trim_start_matches("https://")
            .trim_end_matches('/')
    {
        return Err(error("Copilot sign-in host differs from the inference host. Set COPILOT_GH_HOST to the selected account's host."));
    }
    let saved = users.and_then(|users| {
        users.iter().find(|u| {
            u["login"] == login
                && u["host"]
                    .as_str()
                    .unwrap_or("https://github.com")
                    .trim_end_matches('/')
                    == host
        })
    });
    let token = [Some(selected), saved].into_iter().flatten().find_map(|u| {
        ["oauth_token", "oauthToken", "token"]
            .into_iter()
            .find_map(|key| u[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
    });
    let canonical_host = if host.starts_with("https://") {
        host.to_owned()
    } else {
        format!("https://{host}")
    };
    Ok((format!("{canonical_host}:{login}"), token))
}
fn error(message: &str) -> ModelError {
    ModelError::BackendError {
        code: "copilot_auth".into(),
        message: message.into(),
    }
}
fn contains_image(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_image),
        Value::Object(map) => {
            matches!(
                map.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image" | "image")
            ) || map.values().any(contains_image)
        }
        _ => false,
    }
}
#[async_trait]
impl InferenceAuth for CopilotAuth {
    async fn headers(&self, body: &Value) -> Result<HeaderMap, ModelError> {
        let auth = Self {
            path: self.path.clone(),
            host: self.host.clone(),
        };
        let token = match tokio::task::spawn_blocking(move || auth.load_token())
            .await
            .map_err(|_| error("Cannot read Copilot credentials."))??
        {
            LoadedToken::Ready(token) => token,
            LoadedToken::Refresh(path) => crate::credentials::refresh_token(&path, &self.host)
                .await
                .map_err(|message| error(&message))?,
        };
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| error("Invalid Copilot token."))?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(
            "copilot-integration-id",
            HeaderValue::from_static(COPILOT_INTEGRATION_ID),
        );
        headers.insert("user-agent", HeaderValue::from_static("rusty-harness/0.2"));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(COPILOT_API_VERSION),
        );
        headers.insert(
            "openai-intent",
            HeaderValue::from_static("conversation-edits"),
        );
        let messages = body.get("input").or_else(|| body.get("messages"));
        let user_initiated = messages
            .and_then(Value::as_array)
            .and_then(|m| m.last())
            .is_some_and(|m| m["role"] == "user" && !only_tool_results(m));
        headers.insert(
            "x-initiator",
            HeaderValue::from_static(if user_initiated { "user" } else { "agent" }),
        );
        if messages.is_some_and(contains_image) {
            headers.insert("copilot-vision-request", HeaderValue::from_static("true"));
        }
        Ok(headers)
    }
}
fn only_tool_results(message: &Value) -> bool {
    message["content"].as_array().is_some_and(|blocks| {
        !blocks.is_empty() && blocks.iter().all(|block| block["type"] == "tool_result")
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_selection_never_falls_through_to_another_account_or_host() {
        let value = serde_json::json!({"lastLoggedInUser":{"login":"chosen","host":"https://github.com"},"loggedInUsers":[{"login":"other","token":"wrong"},{"login":"chosen","token":"right"}]});
        let (account, token) = selected_credential(&value, "github.com").unwrap();
        assert_eq!(account, "https://github.com:chosen");
        assert_eq!(token.as_deref(), Some("right"));
        assert!(selected_credential(&value, "tenant.ghe.com").is_err());
    }
    #[tokio::test]
    async fn tool_results_are_agent_initiated_and_tokens_are_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({"lastLoggedInUser":{"login":"test","token":"fixture"}}).to_string(),
        )
        .unwrap();
        let auth = CopilotAuth::new(Some(path), "github.com".into());
        let headers = auth
            .headers(&serde_json::json!({"input":[{"type":"function_call_output","output":"ok"}]}))
            .await
            .unwrap();
        assert_eq!(headers["x-initiator"], "agent");
        assert_eq!(headers["x-github-api-version"], COPILOT_API_VERSION);
        assert!(headers[AUTHORIZATION].is_sensitive());
        let headers = auth
            .headers(&serde_json::json!({"messages":[{"role":"user","content":"hello"}]}))
            .await
            .unwrap();
        assert_eq!(headers["x-initiator"], "user");
        assert_eq!(headers["copilot-integration-id"], "copilot-developer-cli");
        let headers = auth.headers(&serde_json::json!({"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":[{"type":"image","source":{}}]}]}]})).await.unwrap();
        assert_eq!(headers["x-initiator"], "agent");
        assert_eq!(headers["copilot-vision-request"], "true");
        let headers = auth.headers(&serde_json::json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,fixture"}}]}]})).await.unwrap();
        assert_eq!(headers["x-initiator"], "user");
        assert_eq!(headers["copilot-vision-request"], "true");
    }
}
