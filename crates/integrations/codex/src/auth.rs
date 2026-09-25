//! Credential access is a control-plane operation. Neither credential contents
//! nor credential-store handles are exposed to the model or tool executors.
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use harness_integration_openai_responses::client::ResponsesAuth;
use harness_model::ModelError;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

// Serialize refreshes across all sessions in this harness process, not just one
// backend. Each turn reloads the store so login/logout and rotation are observed.
static AUTH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

pub struct CodexAuth {
    auth_path: Option<PathBuf>,
    client: reqwest::Client,
    token_url: String,
}

impl CodexAuth {
    pub fn new(auth_path: Option<PathBuf>) -> Self {
        Self {
            auth_path,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("valid OAuth HTTP client"),
            token_url: TOKEN_URL.into(),
        }
    }

    fn path(&self) -> Result<PathBuf, ModelError> {
        if let Some(path) = &self.auth_path {
            return Ok(path.clone());
        }
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Ok(PathBuf::from(home).join("auth.json"));
        }
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".codex/auth.json"))
            .ok_or_else(|| {
                auth_error("Cannot locate Codex credentials. Sign in to Codex in Settings.")
            })
    }

    async fn load_headers(&self) -> Result<HeaderMap, ModelError> {
        let _guard = AUTH_LOCK.lock().await;
        let path = self.path()?;
        let allow_keychain = self.auth_path.is_none();
        let (store, mut credentials) =
            tokio::task::spawn_blocking(move || Store::load(path, allow_keychain))
                .await
                .map_err(|_| auth_error("Could not read the Codex credential store."))??;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let access = credentials
            .pointer("/tokens/access_token")
            .and_then(Value::as_str)
            .unwrap_or("");
        let expires = claims(access).and_then(|v| v.get("exp").and_then(Value::as_u64));
        if access.is_empty() || expires.map_or(true, |exp| exp <= now + 60) {
            let refresh = credentials
                .pointer("/tokens/refresh_token")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    auth_error("No ChatGPT subscription credentials. Sign in to Codex in Settings.")
                })?;
            let response = self
                .client
                .post(&self.token_url)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh),
                    ("client_id", CLIENT_ID),
                ])
                .send()
                .await
                .map_err(|_| {
                    auth_error("Could not refresh the Codex sign-in. Check your connection.")
                })?;
            if !response.status().is_success() {
                // Never include the response body: auth servers may echo secrets.
                return Err(auth_error(&format!(
                    "Codex token refresh failed (HTTP {}). Sign in again in Settings.",
                    response.status().as_u16()
                )));
            }
            let tokens: Value = response
                .json()
                .await
                .map_err(|_| auth_error("Invalid Codex token refresh response."))?;
            merge_refreshed_tokens(&mut credentials, &tokens)?;
            let saved = credentials.clone();
            tokio::task::spawn_blocking(move || store.save(&saved))
                .await
                .map_err(|_| auth_error("Could not save refreshed Codex credentials."))??;
        }
        headers_from_credentials(&credentials)
    }
}

#[async_trait]
impl ResponsesAuth for CodexAuth {
    async fn headers(&self, _: &Value) -> Result<HeaderMap, ModelError> {
        self.load_headers().await
    }
}

fn auth_error(message: &str) -> ModelError {
    ModelError::BackendError {
        code: "codex_auth".into(),
        message: message.into(),
    }
}

fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()
}

fn claim_string(claims: &Value, name: &str) -> Option<String> {
    claims
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get(name))
        .or_else(|| claims.get(name))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn headers_from_credentials(credentials: &Value) -> Result<HeaderMap, ModelError> {
    let token = credentials
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| auth_error("Missing Codex subscription access token."))?;
    let access_claims = claims(token).unwrap_or_default();
    let id_claims = credentials
        .pointer("/tokens/id_token")
        .and_then(Value::as_str)
        .and_then(claims)
        .unwrap_or_default();
    let account = credentials
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| claim_string(&id_claims, "chatgpt_account_id"))
        .or_else(|| claim_string(&access_claims, "chatgpt_account_id"))
        .ok_or_else(|| {
            auth_error("Missing ChatGPT account ID. Sign in to Codex again in Settings.")
        })?;
    let mut headers = HeaderMap::new();
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| auth_error("Invalid Codex access token."))?;
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    headers.insert(
        "chatgpt-account-id",
        HeaderValue::from_str(&account).map_err(|_| auth_error("Invalid ChatGPT account ID."))?,
    );
    if let Some(residency) =
        claim_string(&access_claims, "chatgpt_compute_residency").filter(|s| s != "no_constraint")
    {
        headers.insert(
            "x-openai-internal-codex-residency",
            HeaderValue::from_str(&residency)
                .map_err(|_| auth_error("Invalid Codex residency claim."))?,
        );
    }
    Ok(headers)
}

fn merge_refreshed_tokens(credentials: &mut Value, response: &Value) -> Result<(), ModelError> {
    if response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(auth_error("Codex refresh response has no access token."));
    }
    for key in ["access_token", "refresh_token", "id_token"] {
        if let Some(token) = response
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            credentials["tokens"][key] = token.into();
        }
    }
    // Retain account selection: a refresh must not silently switch workspaces.
    credentials["last_refresh"] = chrono::Utc::now().to_rfc3339().into();
    Ok(())
}

enum Store {
    File(PathBuf),
    #[cfg(target_os = "macos")]
    Keychain(String),
}
impl Store {
    fn load(path: PathBuf, _allow_keychain: bool) -> Result<(Self, Value), ModelError> {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let value = serde_json::from_slice(&bytes).map_err(|_| {
                    auth_error("Invalid Codex credentials JSON. Sign in again in Settings.")
                })?;
                return Ok((Self::File(path), value));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(auth_error("Cannot read Codex credentials.")),
        }
        #[cfg(target_os = "macos")]
        if _allow_keychain {
            use sha2::{Digest, Sha256};
            let home = path
                .parent()
                .ok_or_else(|| auth_error("Invalid Codex home."))?;
            let canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
            let digest = format!(
                "{:x}",
                Sha256::digest(canonical.to_string_lossy().as_bytes())
            );
            let account = format!("cli|{}", &digest[..16]);
            if let Ok(bytes) =
                security_framework::passwords::get_generic_password("Codex Auth", &account)
            {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|_| auth_error("Invalid Codex keychain credentials."))?;
                return Ok((Self::Keychain(account), value));
            }
        }
        Err(auth_error(
            "No ChatGPT subscription credentials found. Sign in to Codex in Settings.",
        ))
    }
    fn save(self, value: &Value) -> Result<(), ModelError> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|_| auth_error("Cannot serialize Codex credentials."))?;
        match self {
            Self::File(path) => {
                use std::io::Write;
                let parent = path
                    .parent()
                    .ok_or_else(|| auth_error("Invalid Codex credential path."))?;
                let mut file = tempfile::NamedTempFile::new_in(parent)
                    .map_err(|_| auth_error("Cannot save Codex credentials."))?;
                // tempfile creates mode 0600 on Unix; rename atomically preserves it.
                file.write_all(&bytes)
                    .and_then(|_| file.as_file().sync_all())
                    .map_err(|_| auth_error("Cannot save Codex credentials."))?;
                file.persist(path)
                    .map_err(|_| auth_error("Cannot replace Codex credentials."))?;
            }
            #[cfg(target_os = "macos")]
            Self::Keychain(account) => {
                security_framework::passwords::set_generic_password("Codex Auth", &account, &bytes)
                    .map_err(|_| auth_error("Cannot save Codex keychain credentials."))?
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn jwt(payload: Value) -> String {
        format!(
            "e30.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        )
    }
    #[tokio::test]
    async fn loads_subscription_headers_and_observes_logout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let token = jwt(
            serde_json::json!({"exp":4102444800u64,"https://api.openai.com/auth":{"chatgpt_account_id":"account","chatgpt_compute_residency":"eu"}}),
        );
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"tokens":{"access_token":token}})).unwrap(),
        )
        .unwrap();
        let auth = CodexAuth::new(Some(path.clone()));
        let headers = auth.load_headers().await.unwrap();
        assert_eq!(headers["chatgpt-account-id"], "account");
        assert_eq!(headers["x-openai-internal-codex-residency"], "eu");
        assert!(headers[AUTHORIZATION].is_sensitive());
        std::fs::remove_file(path).unwrap();
        assert!(auth.load_headers().await.is_err());
    }
    #[test]
    fn refresh_preserves_account_and_rotates_only_returned_tokens() {
        let mut credentials = serde_json::json!({"tokens":{"access_token":"old","refresh_token":"refresh","account_id":"selected"},"other":"keep"});
        merge_refreshed_tokens(&mut credentials, &serde_json::json!({"access_token":"new"}))
            .unwrap();
        assert_eq!(credentials["tokens"]["refresh_token"], "refresh");
        assert_eq!(credentials["tokens"]["account_id"], "selected");
        assert_eq!(credentials["other"], "keep");
        assert!(credentials["last_refresh"].is_string());
    }
    #[test]
    fn missing_tokens_and_invalid_header_values_fail_without_secrets() {
        assert!(headers_from_credentials(&serde_json::json!({"OPENAI_API_KEY":"secret"})).is_err());
        let error = headers_from_credentials(
            &serde_json::json!({"tokens":{"access_token":"secret\nvalue","account_id":"account"}}),
        )
        .unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
    #[tokio::test]
    async fn expired_credentials_refresh_once_across_concurrent_sessions_and_persist() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/oauth/token", listener.local_addr().unwrap());
        let token = jwt(serde_json::json!({"exp":4102444800u64}));
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
            assert!(request.contains("grant_type=refresh_token"));
            assert!(request.contains("refresh_token=old-refresh"));
            assert!(request.contains(CLIENT_ID));
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", response.len(), response).as_bytes()).await.unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        std::fs::write(&path, serde_json::json!({"tokens":{"access_token":jwt(serde_json::json!({"exp":1})),"refresh_token":"old-refresh","account_id":"selected"}}).to_string()).unwrap();
        let mut first = CodexAuth::new(Some(path.clone()));
        first.token_url = url.clone();
        let mut second = CodexAuth::new(Some(path.clone()));
        second.token_url = url;
        let (a, b) = tokio::join!(first.load_headers(), second.load_headers());
        assert_eq!(a.unwrap()[AUTHORIZATION], b.unwrap()[AUTHORIZATION]);
        server.await.unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["tokens"]["refresh_token"], "rotated");
        assert_eq!(saved["tokens"]["account_id"], "selected");
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
