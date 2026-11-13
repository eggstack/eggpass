//! The credential store.
//!
//! Credentials live in `~/.config/grokproxy/auth.json`, inside a directory with
//! mode `0700`, in a file with mode `0600`, written atomically so that a
//! crash mid-write can never leave a half-written token behind.
//!
//! If our file is missing we also look at `~/.grok/auth.json`, the file the
//! official Grok Build CLI writes, so a user who has already run `grok login`
//! does not have to log in again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::restrict_permissions;
use crate::timeutil;

/// Errors raised while reading or writing credentials.
#[derive(Debug)]
pub enum StoreError {
    /// The file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file exists but does not hold credentials we understand.
    Invalid { path: PathBuf, message: String },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            StoreError::Invalid { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// The credentials we keep on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAuth {
    /// The bearer token used against the API.
    pub access_token: String,
    /// The token used to obtain a new access token.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// The OpenID Connect identity token.
    #[serde(default)]
    pub id_token: Option<String>,
    /// The token type, normally `Bearer`.
    #[serde(default = "default_token_type")]
    pub token_type: String,
    /// The scopes the token was granted.
    #[serde(default)]
    pub scope: Option<String>,
    /// The client id the token belongs to.
    #[serde(default)]
    pub client_id: String,
    /// The issuer that issued the token.
    #[serde(default)]
    pub issuer: String,
    /// The absolute expiry, in seconds since the unix epoch.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// When the token was obtained, in seconds since the unix epoch.
    #[serde(default)]
    pub obtained_at: i64,
    /// The provider's identifier for the account.
    #[serde(default)]
    pub user_id: Option<String>,
    /// The account's email address.
    #[serde(default)]
    pub email: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl StoredAuth {
    /// The moment the access token stops being usable, in unix seconds.
    ///
    /// Falls back to a conservative 30-day lifetime — the same fallback the
    /// official Grok CLI uses — when the server did not tell us.
    pub fn effective_expires_at(&self) -> i64 {
        self.expires_at.unwrap_or(self.obtained_at + 30 * 86_400)
    }

    /// Whether the token is still usable, allowing for the refresh skew.
    pub fn is_fresh(&self, now: i64, skew: i64) -> bool {
        now + skew < self.effective_expires_at()
    }

    /// A short description of the account for `grokproxy status`.
    pub fn account_label(&self) -> String {
        self.email
            .clone()
            .or_else(|| self.user_id.clone())
            .unwrap_or_else(|| "unknown account".to_string())
    }
}

/// Read the credentials, returning `None` when there are none.
pub fn load(path: &Path) -> Result<Option<StoredAuth>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let auth: StoredAuth = serde_json::from_str(&text).map_err(|err| StoreError::Invalid {
        path: path.to_path_buf(),
        message: format!("could not parse the credential file: {err}"),
    })?;

    if auth.access_token.is_empty() {
        return Err(StoreError::Invalid {
            path: path.to_path_buf(),
            message: "the credential file holds no access token".to_string(),
        });
    }

    Ok(Some(auth))
}

/// Write the credentials atomically, with restrictive permissions.
pub fn save(path: &Path, auth: &StoredAuth) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    restrict_permissions(parent, 0o700).map_err(|source| StoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let text = serde_json::to_string_pretty(auth).map_err(|err| StoreError::Invalid {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;

    let temporary = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));

    write_private_file(&temporary, text.as_bytes())?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => {}
        Err(source) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    }

    Ok(())
}

/// Delete the credentials. Returns `true` when a file was removed.
pub fn remove(path: &Path) -> Result<bool, StoreError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Read the official Grok Build CLI's credential file and convert it.
///
/// The official file is keyed by the sign-in origin:
/// `{"https://accounts.x.ai/sign-in": {"key": "...", ...}}`.
pub fn import_grok_cli(path: &Path) -> Result<Option<StoredAuth>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let value: Value = serde_json::from_str(&text).map_err(|err| StoreError::Invalid {
        path: path.to_path_buf(),
        message: format!("could not parse the credential file: {err}"),
    })?;

    let entry = pick_grok_cli_entry(&value).ok_or_else(|| StoreError::Invalid {
        path: path.to_path_buf(),
        message: "no usable credentials found".to_string(),
    })?;

    let access_token = entry
        .get("key")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| StoreError::Invalid {
            path: path.to_path_buf(),
            message: "no access token found".to_string(),
        })?
        .to_string();

    let expires_at = match entry.get("expires_at") {
        Some(Value::String(text)) => timeutil::parse_rfc3339_to_unix(text),
        Some(Value::Number(number)) => number.as_i64(),
        _ => None,
    };

    let obtained_at = match entry.get("create_time") {
        Some(Value::String(text)) => {
            timeutil::parse_rfc3339_to_unix(text).unwrap_or_else(timeutil::now_unix)
        }
        _ => timeutil::now_unix(),
    };

    Ok(Some(StoredAuth {
        access_token,
        refresh_token: entry
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        id_token: None,
        token_type: default_token_type(),
        scope: None,
        client_id: entry
            .get("oidc_client_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        issuer: entry
            .get("oidc_issuer")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        expires_at,
        obtained_at,
        user_id: entry
            .get("user_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        email: entry
            .get("email")
            .and_then(Value::as_str)
            .filter(|email| !email.is_empty())
            .map(str::to_string),
    }))
}

/// Choose the entry of the official credential file to import.
///
/// Entries that carry a refresh token win, because without one the import
/// would expire the first time the access token does.
fn pick_grok_cli_entry(value: &Value) -> Option<&Value> {
    let Value::Object(map) = value else {
        return None;
    };

    let mut fallback = None;
    for entry in map.values() {
        if !entry.is_object() {
            continue;
        }
        let has_key = entry
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.is_empty());
        if !has_key {
            continue;
        }
        let has_refresh = entry
            .get("refresh_token")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.is_empty());
        if has_refresh {
            return Some(entry);
        }
        if fallback.is_none() {
            fallback = Some(entry);
        }
    }
    fallback
}

/// Read the credentials, falling back to the official Grok CLI's file.
pub fn load_or_import(
    path: &Path,
    grok_cli_path: Option<&Path>,
) -> Result<Option<StoredAuth>, StoreError> {
    if let Some(auth) = load(path)? {
        return Ok(Some(auth));
    }
    let Some(grok_cli_path) = grok_cli_path else {
        return Ok(None);
    };
    import_grok_cli(grok_cli_path)
}

/// Write bytes to a file that only the owner can read.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    restrict_permissions(path, 0o600).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredAuth {
        StoredAuth {
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: None,
            client_id: "client".to_string(),
            issuer: "https://auth.x.ai".to_string(),
            expires_at: Some(1_000),
            obtained_at: 0,
            user_id: Some("user".to_string()),
            email: Some("a@b.c".to_string()),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save(&path, &sample()).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.access_token, "access");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(loaded.email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn the_file_is_not_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        save(&path, &sample()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("absent.json")).unwrap().is_none());
    }

    #[test]
    fn removing_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert!(!remove(&path).unwrap());
        save(&path, &sample()).unwrap();
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
    }

    #[test]
    fn the_official_grok_cli_file_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{
              "https://accounts.x.ai/sign-in": {
                "key": "official-access",
                "auth_mode": "oidc",
                "create_time": "2026-11-13T05:12:18Z",
                "user_id": "user-1",
                "email": "me@example.com",
                "refresh_token": "official-refresh",
                "expires_at": "2026-12-13T05:12:18Z",
                "oidc_issuer": "https://auth.x.ai",
                "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828"
              }
            }"#,
        )
        .unwrap();

        let imported = import_grok_cli(&path).unwrap().unwrap();
        assert_eq!(imported.access_token, "official-access");
        assert_eq!(imported.refresh_token.as_deref(), Some("official-refresh"));
        assert_eq!(imported.email.as_deref(), Some("me@example.com"));
        assert_eq!(imported.user_id.as_deref(), Some("user-1"));
        assert_eq!(imported.expires_at, Some(1_797_138_738));
        assert_eq!(imported.obtained_at, 1_794_546_738);
    }

    #[test]
    fn our_own_file_wins_over_the_official_one() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("ours.json");
        let theirs = dir.path().join("theirs.json");
        save(&ours, &sample()).unwrap();
        std::fs::write(
            &theirs,
            r#"{"https://accounts.x.ai/sign-in":{"key":"other"}}"#,
        )
        .unwrap();

        let loaded = load_or_import(&ours, Some(&theirs)).unwrap().unwrap();
        assert_eq!(loaded.access_token, "access");
    }

    #[test]
    fn freshness_uses_the_server_expiry() {
        let auth = sample();
        assert!(auth.is_fresh(0, 300));
        assert!(!auth.is_fresh(900, 300));
    }
}
