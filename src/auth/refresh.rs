//! Keeping the access token fresh.
//!
//! Two rules govern this module:
//!
//! * Refresh **proactively**, a few minutes before the token expires, so a
//!   request never fails because the token expired mid-flight.
//! * Refresh **once**, even when several requests fail at the same moment. The
//!   in-process mutex, shared credential-file lock, and re-read after locking
//!   mean a burst of parallel 401s results in a single refresh.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use super::client::{AuthError, OAuthClient, TokenSet};
use super::store::{self, StoreError, StoredAuth};
use crate::timeutil;

/// How long before expiry we refresh the access token.
pub const REFRESH_SKEW_SECS: i64 = 5 * 60;

/// A daemon refresh check should not hold shutdown open during an outage.
const BACKGROUND_REFRESH_TIMEOUT: Duration = Duration::from_secs(4);

/// Errors raised while obtaining a usable access token.
#[derive(Debug)]
pub enum TokenError {
    /// There are no credentials on this machine.
    NotLoggedIn,
    /// The credential store could not be read or written.
    Store(StoreError),
    /// The shared refresh lock could not be acquired.
    Lock(std::io::Error),
    /// The refresh request failed.
    Auth(AuthError),
    /// The stored credentials carry no refresh token.
    NoRefreshToken,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::NotLoggedIn => write!(f, "not logged in — run `eggpass login`"),
            TokenError::Store(err) => write!(f, "{err}"),
            TokenError::Lock(err) => {
                write!(f, "could not acquire the credential refresh lock ({err})")
            }
            TokenError::Auth(err) => write!(
                f,
                "the stored credentials could not be refreshed ({err}) — run `eggpass login`"
            ),
            TokenError::NoRefreshToken => {
                write!(
                    f,
                    "the stored credentials have expired and carry no refresh token — run `eggpass login`"
                )
            }
        }
    }
}

impl std::error::Error for TokenError {}

impl From<StoreError> for TokenError {
    fn from(err: StoreError) -> TokenError {
        TokenError::Store(err)
    }
}

impl From<AuthError> for TokenError {
    fn from(err: AuthError) -> TokenError {
        TokenError::Auth(err)
    }
}

/// Hands out usable access tokens.
///
/// One manager per provider: the credential file, OAuth client, and in-process
/// refresh mutex are per provider. A sidecar file lock coordinates refreshes
/// across processes without blocking a different provider.
#[derive(Debug)]
pub struct TokenManager {
    path: PathBuf,
    client: OAuthClient,
    import_source: Option<PathBuf>,
    lock: Mutex<()>,
    skew: i64,
}

impl crate::upstream::TokenProvider for TokenManager {
    fn token(&self) -> Result<String, TokenError> {
        self.access_token()
    }

    fn refresh(&self, failed_token: &str) -> Result<String, TokenError> {
        self.force_refresh(failed_token)
    }
}

impl TokenManager {
    /// Build a manager that stores one provider's credentials in `path`.
    pub fn new(path: PathBuf, client: OAuthClient) -> TokenManager {
        TokenManager {
            path,
            client,
            import_source: None,
            lock: Mutex::new(()),
            skew: REFRESH_SKEW_SECS,
        }
    }

    /// Build a manager that falls back to another file when ours is missing.
    ///
    /// This is how the official Grok CLI's credential file is imported.
    pub fn with_import_source(mut self, source: Option<PathBuf>) -> TokenManager {
        self.import_source = source;
        self
    }

    /// The credentials currently on disk, without refreshing anything.
    pub fn current(&self) -> Result<Option<StoredAuth>, TokenError> {
        Ok(store::load_or_import(
            &self.path,
            self.import_source.as_deref(),
        )?)
    }

    /// An access token that is good to use right now.
    ///
    /// Refreshes the token first if it is about to expire.
    pub fn access_token(&self) -> Result<String, TokenError> {
        self.usable_auth(None)?
            .map(|auth| auth.access_token)
            .ok_or(TokenError::NotLoggedIn)
    }

    /// Refresh the stored credentials when they are near expiry.
    ///
    /// A daemon can call this periodically, independently of provider traffic.
    /// No credentials is not an error: the provider may not be signed in yet.
    pub fn refresh_if_needed(&self) -> Result<(), TokenError> {
        self.usable_auth(Some(BACKGROUND_REFRESH_TIMEOUT))
            .map(|_| ())
    }

    /// Refresh the stored credentials now, even when the access token is fresh.
    ///
    /// Unlike [`refresh_if_needed`](Self::refresh_if_needed), this operation is
    /// intended for an explicit user request and does not skip a refresh based
    /// on the token's expiry time.
    pub fn refresh_now(&self) -> Result<(), TokenError> {
        if self.current()?.is_none() {
            return Err(TokenError::NotLoggedIn);
        }
        let guard = self.lock();
        let _process_guard = self.process_lock()?;
        let Some(auth) = self.current()? else {
            return Err(TokenError::NotLoggedIn);
        };
        self.refresh_locked(&guard, &auth, None).map(|_| ())
    }

    /// Return current credentials after refreshing them when necessary.
    fn usable_auth(
        &self,
        refresh_timeout: Option<Duration>,
    ) -> Result<Option<StoredAuth>, TokenError> {
        let Some(auth) = self.current()? else {
            return Ok(None);
        };

        if auth.is_fresh(timeutil::now_unix(), self.skew) {
            return Ok(Some(auth));
        }

        let guard = self.lock();
        let _process_guard = self.process_lock()?;
        // Re-read after acquiring the locks in case another process refreshed.
        let Some(auth) = self.current()? else {
            return Ok(None);
        };
        if auth.is_fresh(timeutil::now_unix(), self.skew) {
            return Ok(Some(auth));
        }

        self.refresh_locked(&guard, &auth, refresh_timeout)
            .map(Some)
    }

    /// Refresh after an upstream request was rejected with `401`.
    ///
    /// Only refreshes when the token that failed is still the current one, so
    /// a burst of parallel requests cannot trigger a refresh stampede.
    pub fn force_refresh(&self, failed_token: &str) -> Result<String, TokenError> {
        if self.current()?.is_none() {
            return Err(TokenError::NotLoggedIn);
        }
        let guard = self.lock();
        let _process_guard = self.process_lock()?;
        let Some(auth) = self.current()? else {
            return Err(TokenError::NotLoggedIn);
        };

        if auth.access_token != failed_token {
            // Somebody already replaced the token; use theirs.
            return Ok(auth.access_token);
        }

        Ok(self.refresh_locked(&guard, &auth, None)?.access_token)
    }

    /// Perform the refresh. The caller must hold the lock.
    fn refresh_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        current: &StoredAuth,
        timeout: Option<Duration>,
    ) -> Result<StoredAuth, TokenError> {
        let Some(refresh_token) = current.refresh_token.clone() else {
            return Err(TokenError::NoRefreshToken);
        };

        // Sign in with ChatGPT issues a client id during registration, and the
        // refresh has to present that id rather than the entrypoint.
        let client_id = if current.client_id.is_empty() {
            self.client.client_id().to_string()
        } else {
            current.client_id.clone()
        };
        let tokens = match timeout {
            Some(timeout) => {
                self.client
                    .refresh_as_with_timeout(&refresh_token, &client_id, timeout)?
            }
            None => self.client.refresh_as(&refresh_token, &client_id)?,
        };
        let refreshed = merge_refreshed(current, &tokens);

        store::save(&self.path, &refreshed)?;
        Ok(refreshed)
    }

    /// Lock the credential file across processes so the daemon and CLI cannot
    /// use the same rotating refresh token at the same time.
    fn process_lock(&self) -> Result<File, TokenError> {
        let Some(file_name) = self.path.file_name() else {
            return Err(TokenError::Lock(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "credential path has no file name",
            )));
        };
        let mut lock_name = file_name.to_os_string();
        lock_name.push(".refresh.lock");
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(TokenError::Lock)?;
        let lock_path = parent.join(lock_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(TokenError::Lock)?;
        crate::paths::restrict_permissions(&lock_path, 0o600).map_err(TokenError::Lock)?;
        file.lock().map_err(TokenError::Lock)?;
        Ok(file)
    }

    /// Take the in-process lock, ignoring poisoning — a panic elsewhere must not
    /// stop the proxy from refreshing its token.
    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// Fold a refresh response into the stored credentials.
///
/// The subtle part is refresh-token rotation: when the response omits a new
/// refresh token we keep the old one. Losing it would break the *next*
/// refresh.
pub fn merge_refreshed(current: &StoredAuth, tokens: &TokenSet) -> StoredAuth {
    StoredAuth {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens
            .refresh_token
            .clone()
            .or_else(|| current.refresh_token.clone()),
        id_token: tokens.id_token.clone().or_else(|| current.id_token.clone()),
        token_type: if tokens.token_type.is_empty() {
            current.token_type.clone()
        } else {
            tokens.token_type.clone()
        },
        scope: tokens.scope.clone().or_else(|| current.scope.clone()),
        client_id: if current.client_id.is_empty() {
            self::client_id_default()
        } else {
            current.client_id.clone()
        },
        issuer: if current.issuer.is_empty() {
            self::issuer_default()
        } else {
            current.issuer.clone()
        },
        expires_at: tokens.expires_at,
        obtained_at: timeutil::now_unix(),
        user_id: current.user_id.clone(),
        email: current.email.clone(),
        ext_agent_host_id: current.ext_agent_host_id.clone(),
        plan_enabled: current.plan_enabled,
    }
}

/// The default client id, used when the stored credentials do not name one.
fn client_id_default() -> String {
    super::DEFAULT_CLIENT_ID.to_string()
}

/// The default issuer, used when the stored credentials do not name one.
fn issuer_default() -> String {
    super::DEFAULT_ISSUER.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> StoredAuth {
        StoredAuth {
            access_token: "old-access".to_string(),
            refresh_token: Some("old-refresh".to_string()),
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: None,
            client_id: "client".to_string(),
            issuer: "https://auth.x.ai".to_string(),
            expires_at: Some(1000),
            obtained_at: 0,
            user_id: Some("user".to_string()),
            email: Some("a@b.c".to_string()),
            ext_agent_host_id: None,
            plan_enabled: false,
        }
    }

    fn tokens(access: &str, refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: access.to_string(),
            refresh_token: refresh.map(str::to_string),
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: None,
            expires_at: Some(9999),
        }
    }

    #[test]
    fn a_new_refresh_token_is_stored() {
        let merged = merge_refreshed(&stored(), &tokens("new-access", Some("new-refresh")));
        assert_eq!(merged.access_token, "new-access");
        assert_eq!(merged.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn a_missing_refresh_token_keeps_the_old_one() {
        let merged = merge_refreshed(&stored(), &tokens("new-access", None));
        assert_eq!(merged.refresh_token.as_deref(), Some("old-refresh"));
        assert_eq!(merged.access_token, "new-access");
    }

    #[test]
    fn account_details_survive_a_refresh() {
        let merged = merge_refreshed(&stored(), &tokens("new-access", None));
        assert_eq!(merged.email.as_deref(), Some("a@b.c"));
        assert_eq!(merged.user_id.as_deref(), Some("user"));
    }

    fn local_token_manager(path: &std::path::Path) -> TokenManager {
        let config = crate::config::Config::default();
        let mut spec = crate::provider::spec_for(crate::provider::ProviderId::OpenAi, &config);
        // Keep any accidental refresh attempt on loopback; these tests never
        // send credentials to a real provider.
        spec.issuer = "http://127.0.0.1:9".to_string();
        let client = OAuthClient::for_provider(&spec, &spec.entry_client_id);
        TokenManager::new(path.to_path_buf(), client)
    }

    #[test]
    fn periodic_refresh_is_a_noop_without_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let manager = local_token_manager(&directory.path().join("auth.json"));

        assert!(manager.refresh_if_needed().is_ok());
    }

    #[test]
    fn manual_refresh_without_credentials_reports_not_logged_in() {
        let directory = tempfile::tempdir().unwrap();
        let manager = local_token_manager(&directory.path().join("auth.json"));

        assert!(matches!(
            manager.refresh_now(),
            Err(TokenError::NotLoggedIn)
        ));
    }

    #[test]
    fn periodic_refresh_leaves_fresh_credentials_alone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let now = timeutil::now_unix();
        let mut auth = stored();
        auth.expires_at = Some(now + 3600);
        auth.obtained_at = now;
        store::save(&path, &auth).unwrap();
        let manager = local_token_manager(&path);

        assert!(manager.refresh_if_needed().is_ok());
        assert_eq!(
            manager.current().unwrap().unwrap().access_token,
            "old-access"
        );
    }

    #[test]
    fn manual_refresh_updates_credentials_even_when_the_access_token_is_fresh() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("local OAuth stub binds");
        let address = listener.local_addr().expect("local OAuth stub address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("refresh request connects");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = stream.read(&mut chunk).expect("refresh request reads");
                assert_ne!(read, 0, "client closed before sending request headers");
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }

            let response_body = br#"{"access_token":"fresh-access","refresh_token":"rotated-refresh","id_token":"fresh-id","token_type":"Bearer","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("response headers write");
            stream
                .write_all(response_body)
                .expect("response body write");
        });

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let now = timeutil::now_unix();
        let mut auth = stored();
        auth.expires_at = Some(now + 3600);
        auth.obtained_at = now;
        store::save(&path, &auth).unwrap();

        let config = crate::config::Config::default();
        let mut spec = crate::provider::spec_for(crate::provider::ProviderId::OpenAi, &config);
        spec.issuer = format!("http://{address}");
        let client = OAuthClient::for_provider(&spec, &spec.entry_client_id);
        let manager = TokenManager::new(path, client);

        manager.refresh_now().expect("manual refresh succeeds");
        server.join().expect("OAuth stub finishes");

        let refreshed = manager.current().unwrap().unwrap();
        assert_eq!(refreshed.access_token, "fresh-access");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(refreshed.id_token.as_deref(), Some("fresh-id"));
    }

    #[test]
    fn periodic_refresh_attempts_due_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.json");
        let now = timeutil::now_unix();
        let mut auth = stored();
        auth.expires_at = Some(now + REFRESH_SKEW_SECS - 1);
        auth.obtained_at = now;
        store::save(&path, &auth).unwrap();
        let manager = local_token_manager(&path);

        assert!(matches!(
            manager.refresh_if_needed(),
            Err(TokenError::Auth(_))
        ));
    }
}
