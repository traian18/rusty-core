//! Credentials for the direct Copilot OAuth flow, separate from CLI sign-in.
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const CREDENTIAL_FORMAT: &str = "rusty-copilot-oauth-v1";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "rusty-copilot-inference";

#[derive(Serialize, Deserialize)]
struct Credential {
    format: String,
    host: String,
    login: String,
    #[serde(default)]
    oauth_client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth_token: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    keychain: bool,
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

pub(crate) fn load_token(path: &Path, expected_host: &str) -> Result<String, String> {
    let credential = read(path)?;
    if normalize_host(&credential.host) != normalize_host(expected_host) {
        return Err("Copilot sign-in host differs from the inference host.".into());
    }
    if let Some(token) = credential.oauth_token.filter(|token| !token.is_empty()) {
        return Ok(token);
    }
    #[cfg(target_os = "macos")]
    if credential.keychain {
        let account = format!(
            "https://{}:{}",
            normalize_host(&credential.host),
            credential.login
        );
        let token = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, &account)
            .map_err(|_| "Cannot read Rusty's Copilot credential from Keychain. Sign in again.")?;
        return String::from_utf8(token).map_err(|_| "Invalid Copilot credential.".into());
    }
    Err("No direct Copilot API credential. Sign in again in Rusty's settings.".into())
}

fn normalize_host(host: &str) -> &str {
    host.trim_start_matches("https://").trim_end_matches('/')
}

/// Saves an OAuth token without exposing it to the frontend. On macOS only
/// account metadata is written to disk; the token is held in Keychain.
pub fn save_credential(
    path: &Path,
    host: &str,
    login: &str,
    client_id: &str,
    token: &str,
) -> Result<(), String> {
    crate::api_root(host)?;
    if login.is_empty() || client_id.is_empty() || token.is_empty() {
        return Err("Missing Copilot account or credential.".into());
    }
    let credential = Credential {
        format: CREDENTIAL_FORMAT.into(),
        host: normalize_host(host).into(),
        login: login.into(),
        oauth_client_id: client_id.into(),
        oauth_token: Some(token.into()),
        keychain: false,
    };
    #[cfg(target_os = "macos")]
    let credential = {
        let mut credential = credential;
        let account = format!("https://{}:{login}", credential.host);
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            &account,
            token.as_bytes(),
        )
        .map_err(|_| "Cannot save the Copilot credential to Keychain.")?;
        credential.oauth_token = None;
        credential.keychain = true;
        credential
    };
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
            let account = format!(
                "https://{}:{}",
                normalize_host(&credential.host),
                credential.login
            );
            // A missing Keychain item is already signed out. Other failures
            // leave the metadata intact so logout can be retried.
            if let Err(error) =
                security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, &account)
            {
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
        assert_eq!(load_token(&path, "github.com").unwrap(), "fixture");
        assert!(load_token(&path, "other.ghe.com").is_err());
        assert_eq!(
            credential_account(&path).unwrap(),
            ("github.com".into(), "octocat".into())
        );
    }
}
