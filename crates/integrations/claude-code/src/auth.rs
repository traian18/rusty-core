//! OAuth credential handling. No model or tool executor can access this store.
use async_trait::async_trait;
use harness_model::{auth::InferenceAuth, ModelError};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

static AUTH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const SERVICE: &str = "Claude Code-credentials";

pub struct ClaudeAuth {
    path: Option<PathBuf>,
    client: reqwest::Client,
    token_url: String,
}
impl ClaudeAuth {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("OAuth client"),
            token_url: TOKEN_URL.into(),
        }
    }
    async fn token(&self) -> Result<String, ModelError> {
        let _lock = AUTH_LOCK.lock().await;
        if self.path.is_none() {
            if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
                if !token.is_empty() {
                    return Ok(token);
                }
            }
        }
        let custom_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
        let path = self
            .path
            .clone()
            .or_else(|| {
                custom_dir
                    .clone()
                    .map(|d| PathBuf::from(d).join(".credentials.json"))
            })
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|h| PathBuf::from(h).join(".claude/.credentials.json"))
            })
            .ok_or_else(|| error("Cannot locate Claude subscription credentials."))?;
        let keychain = self.path.is_none() && custom_dir.is_none();
        let (store, mut credentials) =
            tokio::task::spawn_blocking(move || Store::load(path, keychain))
                .await
                .map_err(|_| error("Cannot read Claude credentials."))??;
        let key = oauth_key(&credentials).ok_or_else(|| {
            error("No Claude subscription credential. Sign in again in Settings.")
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let token = credentials[key]["accessToken"].as_str().unwrap_or("");
        let expiry = credentials[key]["expiresAt"].as_u64();
        if token.is_empty() || expiry.is_some_and(|exp| exp <= now + 60_000) {
            let refresh = credentials[key]["refreshToken"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("Claude sign-in expired. Sign in again in Settings."))?;
            let response = self
                .client
                .post(&self.token_url)
                .json(&serde_json::json!({
                    "grant_type":"refresh_token", "refresh_token":refresh, "client_id":CLIENT_ID,
                }))
                .send()
                .await
                .map_err(|_| error("Could not refresh Claude sign-in. Check your connection."))?;
            if !response.status().is_success() {
                return Err(error(&format!(
                    "Claude token refresh failed (HTTP {}). Sign in again in Settings.",
                    response.status().as_u16()
                )));
            }
            let response: Value = response
                .json()
                .await
                .map_err(|_| error("Invalid Claude token refresh response."))?;
            let token = response["access_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("Claude refresh response has no access token."))?;
            credentials[key]["accessToken"] = token.into();
            if let Some(refresh) = response["refresh_token"].as_str().filter(|s| !s.is_empty()) {
                credentials[key]["refreshToken"] = refresh.into();
            }
            credentials[key]["expiresAt"] = now
                .saturating_add(
                    response["expires_in"]
                        .as_u64()
                        .unwrap_or(3600)
                        .saturating_mul(1000),
                )
                .into();
            let saved = credentials.clone();
            tokio::task::spawn_blocking(move || store.save(&saved))
                .await
                .map_err(|_| error("Cannot save Claude credentials."))??;
        }
        credentials[key]["accessToken"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| error("Missing Claude access token."))
    }
}
#[async_trait]
impl InferenceAuth for ClaudeAuth {
    async fn headers(&self, _: &Value) -> Result<HeaderMap, ModelError> {
        let token = self.token().await?;
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| error("Invalid Claude access token."))?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20,interleaved-thinking-2025-05-14"),
        );
        Ok(headers)
    }
}
fn error(message: &str) -> ModelError {
    ModelError::BackendError {
        code: "claude_auth".into(),
        message: message.into(),
    }
}
fn oauth_key(value: &Value) -> Option<&'static str> {
    ["claudeAiOauth", "oauth"]
        .into_iter()
        .find(|key| value[*key].is_object())
}
enum Store {
    File(PathBuf),
    #[cfg(target_os = "macos")]
    Keychain(String),
}
impl Store {
    fn load(path: PathBuf, _keychain: bool) -> Result<(Self, Value), ModelError> {
        match std::fs::read(&path) {
            Ok(bytes) => {
                return Ok((
                    Self::File(path),
                    serde_json::from_slice(&bytes)
                        .map_err(|_| error("Invalid Claude credential JSON."))?,
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error("Cannot read Claude credential file.")),
        }
        #[cfg(target_os = "macos")]
        if _keychain {
            use security_framework::item::{ItemClass, ItemSearchOptions};
            // Query only this service's account metadata; never enumerate other secrets.
            let items = match ItemSearchOptions::new()
                .class(ItemClass::generic_password()).service(SERVICE).load_attributes(true).search() {
                Ok(items) => items,
                Err(e) if e.code() == -25300 => Vec::new(), // errSecItemNotFound
                Err(e) => return Err(error(&format!("macOS could not access Claude's Keychain credentials ({}). Allow Rusty Keychain access or sign in again in Settings.", e.code()))),
            };
            for item in items {
                if let Some(account) = item.simplify_dict().and_then(|d| d.get("acct").cloned()) {
                    let bytes = security_framework::passwords::get_generic_password(SERVICE, &account)
                        .map_err(|e| error(&format!("macOS denied access to Claude's Keychain credential ({}). Allow Rusty Keychain access when prompted.", e.code())))?;
                    return Ok((
                        Self::Keychain(account),
                        serde_json::from_slice(&bytes)
                            .map_err(|_| error("Invalid Claude keychain credentials."))?,
                    ));
                }
            }
        }
        Err(error(
            "No Claude subscription credentials found. Sign in in Settings.",
        ))
    }
    fn save(self, credentials: &Value) -> Result<(), ModelError> {
        let bytes = serde_json::to_vec_pretty(credentials)
            .map_err(|_| error("Cannot serialize Claude credentials."))?;
        match self {
            Self::File(path) => {
                use std::io::Write;
                let mut file = tempfile::NamedTempFile::new_in(
                    path.parent()
                        .ok_or_else(|| error("Invalid credential path."))?,
                )
                .map_err(|_| error("Cannot save Claude credentials."))?;
                file.write_all(&bytes)
                    .and_then(|_| file.as_file().sync_all())
                    .map_err(|_| error("Cannot save Claude credentials."))?;
                file.persist(path)
                    .map_err(|_| error("Cannot replace Claude credentials."))?;
            }
            #[cfg(target_os = "macos")]
            Self::Keychain(account) => {
                security_framework::passwords::set_generic_password(SERVICE, &account, &bytes)
                    .map_err(|_| error("Cannot save Claude keychain credentials."))?
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oauth_headers_are_sensitive_and_reload_after_logout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("credentials.json");
        std::fs::write(&path, serde_json::json!({"claudeAiOauth":{"accessToken":"fixture","expiresAt":4102444800000u64}}).to_string()).unwrap();
        let auth = ClaudeAuth::new(Some(path.clone()));
        let headers = auth.headers(&Value::Null).await.unwrap();
        assert_eq!(headers[AUTHORIZATION], "Bearer fixture");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert!(!headers.contains_key("x-api-key"));
        std::fs::remove_file(path).unwrap();
        assert!(auth.headers(&Value::Null).await.is_err());
    }
    #[tokio::test]
    async fn expired_credentials_refresh_once_across_concurrent_sessions_and_persist() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let token = "refreshed-access";
        let response =
            serde_json::json!({"access_token":token,"refresh_token":"rotated"}).to_string();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.contains("\"grant_type\":\"refresh_token\""));
            assert!(request.contains("\"refresh_token\":\"old-refresh\""));
            assert!(request.contains(CLIENT_ID));
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", response.len(), response).as_bytes()).await.unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(&path, serde_json::json!({"claudeAiOauth":{"accessToken":"expired","expiresAt":1,"refreshToken":"old-refresh","subscriptionType":"max"}}).to_string()).unwrap();
        let mut first = ClaudeAuth::new(Some(path.clone()));
        first.token_url = url.clone();
        let mut second = ClaudeAuth::new(Some(path.clone()));
        second.token_url = url;
        let (a, b) = tokio::join!(first.headers(&Value::Null), second.headers(&Value::Null));
        assert_eq!(a.unwrap()[AUTHORIZATION], b.unwrap()[AUTHORIZATION]);
        server.await.unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(saved["claudeAiOauth"]["subscriptionType"], "max");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
