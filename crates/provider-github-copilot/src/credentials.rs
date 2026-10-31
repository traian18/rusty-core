//! Credentials for the direct Copilot OAuth flow, separate from CLI sign-in.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CREDENTIAL_FORMAT: &str = "rusty-copilot-oauth-v1";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "rusty-copilot-inference";
/// Refresh this long before GitHub's stated expiry, so a request never
/// starts with a token that lapses mid-flight or under clock skew.
const REFRESH_MARGIN_SECS: u64 = 300;
const EXPIRED: &str = "GitHub Copilot sign-in expired. Sign in again in Rusty's settings.";

/// A GitHub OAuth token response. Apps with expiring user tokens (opt-in for
/// OAuth apps) also return a refresh token and both lifetimes; otherwise the
/// access token does not expire and only `access_token` is present.
#[derive(Deserialize)]
pub struct TokenGrant {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token_expires_in: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct Credential {
    format: String,
    host: String,
    login: String,
    #[serde(default)]
    oauth_client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth_refresh_token: Option<String>,
    /// Unix seconds; absent for non-expiring tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    keychain: bool,
}

/// The secret half of a credential, held in Keychain on macOS.
#[derive(Serialize, Deserialize)]
struct Secret {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn needs_refresh(credential: &Credential) -> bool {
    credential
        .expires_at
        .is_some_and(|expires_at| now() + REFRESH_MARGIN_SECS >= expires_at)
}

fn read(path: &Path) -> Result<Credential, String> {
    let data = std::fs::read(path).map_err(|_| "Sign in to GitHub Copilot in Rusty's settings. Existing Copilot CLI sign-in does not authenticate the direct model API.")?;
    let credential: Credential =
        serde_json::from_slice(&data).map_err(|_| "Invalid Rusty Copilot credential file.")?;
    if credential.format != CREDENTIAL_FORMAT || credential.login.is_empty() {
        return Err("Invalid Rusty Copilot credential metadata.".into());
    }
    crate::api_root(&credential.host)?;
    Ok(credential)
}

/// Non-secret account metadata used by the native settings UI.
pub fn credential_account(path: &Path) -> Result<(String, String), String> {
    let credential = read(path)?;
    Ok((credential.host, credential.login))
}

/// Public registration identity, used to reject credentials from another app.
pub fn credential_client_id(path: &Path) -> Result<String, String> {
    let client_id = read(path)?.oauth_client_id;
    if client_id.is_empty() {
        return Err("This Copilot sign-in used an older app registration. Sign in again to authorize Rusty.".into());
    }
    Ok(client_id)
}

fn check_host(credential: &Credential, expected_host: &str) -> Result<(), String> {
    if normalize_host(&credential.host) != normalize_host(expected_host) {
        return Err("Copilot sign-in host differs from the inference host.".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn keychain_account(credential: &Credential) -> String {
    format!(
        "https://{}:{}",
        normalize_host(&credential.host),
        credential.login
    )
}

fn read_secret(credential: &Credential) -> Result<Secret, String> {
    if let Some(access_token) = credential
        .oauth_token
        .clone()
        .filter(|token| !token.is_empty())
    {
        return Ok(Secret {
            access_token,
            refresh_token: credential.oauth_refresh_token.clone(),
        });
    }
    #[cfg(target_os = "macos")]
    if credential.keychain {
        let stored = security_framework::passwords::get_generic_password(
            KEYCHAIN_SERVICE,
            &keychain_account(credential),
        )
        .map_err(|_| "Cannot read Rusty's Copilot credential from Keychain. Sign in again.")?;
        return parse_keychain_secret(stored);
    }
    Err("No direct Copilot API credential. Sign in again in Rusty's settings.".into())
}

/// Earlier versions stored the bare access token in Keychain.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_keychain_secret(stored: Vec<u8>) -> Result<Secret, String> {
    let secret = serde_json::from_slice(&stored).or_else(|_| {
        String::from_utf8(stored).map(|access_token| Secret {
            access_token,
            refresh_token: None,
        })
    });
    secret
        .ok()
        .filter(|secret: &Secret| !secret.access_token.is_empty())
        .ok_or_else(|| "Invalid Copilot credential.".into())
}

/// The stored access token, or `None` when it is about to expire and
/// `refresh_token` has to renew it first.
pub(crate) fn load_token(path: &Path, expected_host: &str) -> Result<Option<String>, String> {
    let credential = read(path)?;
    check_host(&credential, expected_host)?;
    if needs_refresh(&credential) {
        return Ok(None);
    }
    read_secret(&credential).map(|secret| Some(secret.access_token))
}

/// Serializes refreshes in this process. GitHub rotates the refresh token on
/// every use and invalidates the previous pair, so two concurrent refreshes
/// would leave one request (and possibly the stored credential) with a dead
/// token.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Renews an expiring access token with the stored refresh token and saves
/// the rotated pair before returning the new access token.
pub(crate) async fn refresh_token(path: &Path, expected_host: &str) -> Result<String, String> {
    refresh_at(path, expected_host, None).await
}

async fn refresh_at(
    path: &Path,
    expected_host: &str,
    oauth_root: Option<&str>,
) -> Result<String, String> {
    let _guard = REFRESH.lock().await;
    let stored = path.to_owned();
    let (credential, secret) = tokio::task::spawn_blocking(move || {
        let credential = read(&stored)?;
        let secret = read_secret(&credential)?;
        Ok::<_, String>((credential, secret))
    })
    .await
    .map_err(|_| "Cannot read Copilot credentials.")??;
    check_host(&credential, expected_host)?;
    // Another request may have refreshed while this one waited for the lock.
    if !needs_refresh(&credential) {
        return Ok(secret.access_token);
    }
    let refresh = secret
        .refresh_token
        .clone()
        .filter(|token| !token.is_empty() && !credential.oauth_client_id.is_empty())
        .filter(|_| !matches!(credential.refresh_expires_at, Some(at) if now() >= at))
        .ok_or(EXPIRED)?;
    let root = oauth_root.map_or_else(|| format!("https://{}", credential.host), str::to_owned);
    let response = reqwest::Client::new()
        .post(format!("{root}/login/oauth/access_token"))
        .header("accept", "application/json")
        .header("user-agent", "rusty-harness/0.2")
        .json(&json!({
            "client_id": credential.oauth_client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh,
        }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status);
    let payload: Value = match response {
        Ok(response) => response.json().await.unwrap_or(Value::Null),
        // GitHub is unreachable, not refusing: keep using the current token
        // while it is still inside the refresh margin.
        Err(_) if credential.expires_at.is_some_and(|at| now() < at) => {
            return Ok(secret.access_token)
        }
        Err(_) => return Err("Cannot reach GitHub to renew the Copilot sign-in.".into()),
    };
    // A rejected refresh (`bad_refresh_token`, revoked app) has no token.
    let grant: TokenGrant = serde_json::from_value(payload).map_err(|_| EXPIRED)?;
    if grant.access_token.is_empty() {
        return Err(EXPIRED.into());
    }
    let path = path.to_owned();
    let access_token = grant.access_token.clone();
    tokio::task::spawn_blocking(move || {
        let renewed = Credential {
            // GitHub omits the refresh token only if expiry was turned off.
            oauth_refresh_token: grant.refresh_token.clone().or(secret.refresh_token),
            ..credential
        };
        store(&path, renewed, &grant)
    })
    .await
    .map_err(|_| "Cannot save the renewed Copilot credential.")??;
    Ok(access_token)
}

fn normalize_host(host: &str) -> &str {
    host.trim_start_matches("https://").trim_end_matches('/')
}

/// Saves an OAuth token without exposing it to the frontend. On macOS only
/// account metadata is written to disk; the tokens are held in Keychain.
pub fn save_credential(
    path: &Path,
    host: &str,
    login: &str,
    client_id: &str,
    grant: &TokenGrant,
) -> Result<(), String> {
    crate::api_root(host)?;
    if login.is_empty() || client_id.is_empty() || grant.access_token.is_empty() {
        return Err("Missing Copilot account or credential.".into());
    }
    let credential = Credential {
        format: CREDENTIAL_FORMAT.into(),
        host: normalize_host(host).into(),
        login: login.into(),
        oauth_client_id: client_id.into(),
        oauth_token: None,
        oauth_refresh_token: None,
        expires_at: None,
        refresh_expires_at: None,
        keychain: cfg!(target_os = "macos"),
    };
    store(path, credential, grant)
}

/// Writes `grant` into the storage `credential` already uses (Keychain or
/// the metadata file) together with its expiry times.
fn store(path: &Path, mut credential: Credential, grant: &TokenGrant) -> Result<(), String> {
    let issued = now();
    let refresh_token = grant
        .refresh_token
        .clone()
        .or(credential.oauth_refresh_token.take())
        .filter(|token| !token.is_empty());
    credential.expires_at = grant.expires_in.map(|lifetime| issued + lifetime);
    credential.refresh_expires_at = match grant.refresh_token_expires_in {
        Some(lifetime) => Some(issued + lifetime),
        None if grant.refresh_token.is_some() => None,
        None => credential.refresh_expires_at,
    };
    credential.oauth_token = None;
    credential.oauth_refresh_token = None;
    if credential.keychain {
        #[cfg(target_os = "macos")]
        {
            let secret = Secret {
                access_token: grant.access_token.clone(),
                refresh_token,
            };
            security_framework::passwords::set_generic_password(
                KEYCHAIN_SERVICE,
                &keychain_account(&credential),
                &serde_json::to_vec(&secret).map_err(|_| "Cannot encode Copilot credentials.")?,
            )
            .map_err(|_| "Cannot save the Copilot credential to Keychain.")?;
        }
        #[cfg(not(target_os = "macos"))]
        return Err("Keychain credentials are only available on macOS.".into());
    } else {
        credential.oauth_token = Some(grant.access_token.clone());
        credential.oauth_refresh_token = refresh_token;
    }
    let parent = path.parent().ok_or("Invalid Copilot credential path.")?;
    std::fs::create_dir_all(parent).map_err(|_| "Cannot create Copilot credential directory.")?;
    let temporary = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options
        .open(&temporary)
        .map_err(|_| "Cannot write Copilot credential metadata.")?;
    file.write_all(
        &serde_json::to_vec(&credential).map_err(|_| "Cannot encode Copilot credentials.")?,
    )
    .map_err(|_| "Cannot save Copilot credentials.")?;
    std::fs::rename(temporary, path)
        .map_err(|_| "Cannot install Copilot credential metadata.".into())
}

pub fn remove_credential(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let credential = read(path)?;
        if credential.keychain {
            // A missing Keychain item is already signed out. Other failures
            // leave the metadata intact so logout can be retried.
            if let Err(error) = security_framework::passwords::delete_generic_password(
                KEYCHAIN_SERVICE,
                &keychain_account(&credential),
            ) {
                if error.code() != -25300 {
                    return Err("Cannot remove Rusty's Copilot credential from Keychain.".into());
                }
            }
        }
    }
    std::fs::remove_file(path).map_err(|_| "Cannot remove Copilot credential metadata.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dedicated_credentials_do_not_fall_back_to_cli_or_other_hosts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        assert!(load_token(&path, "github.com").is_err());
        std::fs::write(&path, serde_json::json!({"format":CREDENTIAL_FORMAT,"host":"github.com","login":"octocat","oauth_token":"fixture"}).to_string()).unwrap();
        assert_eq!(
            load_token(&path, "github.com").unwrap().as_deref(),
            Some("fixture")
        );
        assert!(load_token(&path, "other.ghe.com").is_err());
        assert_eq!(
            credential_account(&path).unwrap(),
            ("github.com".into(), "octocat".into())
        );
    }

    /// Answers one OAuth request with `response` and returns its JSON body.
    async fn oauth_fixture(response: Value) -> (String, tokio::task::JoinHandle<Value>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0; 4096];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0);
                bytes.extend_from_slice(&chunk[..size]);
                let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                assert!(headers.starts_with("post /login/oauth/access_token "));
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                }
            };
            let response = response.to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            body
        });
        (root, server)
    }

    fn expiring_fixture(path: &Path, expires_at: u64, refresh: Option<&str>) {
        std::fs::write(path, json!({"format":CREDENTIAL_FORMAT,"host":"github.com","login":"octocat","oauth_client_id":"RustyOAuthFixture123","oauth_token":"access-old","oauth_refresh_token":refresh,"expires_at":expires_at,"refresh_expires_at":now() + 3600}).to_string()).unwrap();
    }

    #[tokio::test]
    async fn expiring_tokens_are_refreshed_and_the_rotated_pair_is_saved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        expiring_fixture(&path, now() + 3600, Some("refresh-old"));
        assert_eq!(
            load_token(&path, "github.com").unwrap().as_deref(),
            Some("access-old")
        );
        expiring_fixture(&path, now() + 60, Some("refresh-old"));
        assert_eq!(load_token(&path, "github.com").unwrap(), None);
        let (root, server) = oauth_fixture(json!({"access_token":"access-new","refresh_token":"refresh-new","expires_in":28800,"refresh_token_expires_in":15897600})).await;
        assert_eq!(
            refresh_at(&path, "github.com", Some(&root)).await.unwrap(),
            "access-new"
        );
        assert_eq!(
            server.await.unwrap(),
            json!({"client_id":"RustyOAuthFixture123","grant_type":"refresh_token","refresh_token":"refresh-old"})
        );
        assert_eq!(
            load_token(&path, "github.com").unwrap().as_deref(),
            Some("access-new")
        );
        let saved = read(&path).unwrap();
        assert_eq!(saved.oauth_refresh_token.as_deref(), Some("refresh-new"));
        assert!(saved.expires_at.unwrap() >= now() + 28000);
        assert!(saved.refresh_expires_at.unwrap() >= now() + 15000000);
        assert_eq!(saved.oauth_client_id, "RustyOAuthFixture123");
    }

    #[tokio::test]
    async fn rejected_or_missing_refresh_tokens_ask_for_a_new_sign_in() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        expiring_fixture(&path, now(), None);
        assert_eq!(
            refresh_at(&path, "github.com", Some("http://127.0.0.1:9"))
                .await
                .unwrap_err(),
            EXPIRED
        );
        expiring_fixture(&path, now(), Some("refresh-old"));
        let (root, server) =
            oauth_fixture(json!({"error":"bad_refresh_token","error_description":"SECRET"})).await;
        let error = refresh_at(&path, "github.com", Some(&root))
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error, EXPIRED);
        assert_eq!(
            read(&path).unwrap().oauth_token.as_deref(),
            Some("access-old")
        );
    }

    #[test]
    fn keychain_secrets_from_earlier_versions_hold_only_the_access_token() {
        let legacy = parse_keychain_secret(b"gho_fixture".to_vec()).unwrap();
        assert_eq!(legacy.access_token, "gho_fixture");
        assert!(legacy.refresh_token.is_none());
        let current = parse_keychain_secret(
            br#"{"access_token":"ghu_fixture","refresh_token":"ghr_fixture"}"#.to_vec(),
        )
        .unwrap();
        assert_eq!(current.refresh_token.as_deref(), Some("ghr_fixture"));
        assert!(parse_keychain_secret(Vec::new()).is_err());
    }
}
