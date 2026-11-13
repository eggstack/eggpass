//! Keeping the access token fresh.
//!
//! Two rules govern this module:
//!
//! * Refresh **proactively**, a few minutes before the token expires, so a
//!   request never fails because the token expired mid-flight.
//! * Refresh **once**, even when several requests fail at the same moment. The
//!   mutex plus the re-read inside it means a burst of parallel 401s results
//!   in a single refresh.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use super::client::{AuthError, OAuthClient, TokenSet};
use super::store::{self, StoreError, StoredAuth};
use crate::timeutil;

/// How long before expiry we refresh the access token.
pub const REFRESH_SKEW_SECS: i64 = 5 * 60;

/// Errors raised while obtaining a usable access token.
#[derive(Debug)]
pub enum TokenError {
    /// There are no credentials on this machine.
    NotLoggedIn,
    /// The credential store could not be read or written.
    Store(StoreError),
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
/// One manager per provider: the credential file, the OAuth client and the
/// refresh mutex are all per provider, so a refresh on one provider never
/// blocks another.
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
        let Some(auth) = self.current()? else {
            return Err(TokenError::NotLoggedIn);
        };

        if auth.is_fresh(timeutil::now_unix(), self.skew) {
            return Ok(auth.access_token);
        }

        let guard = self.lock();
        // Another process may have refreshed while we waited for the lock.
        let auth = self.current()?.ok_or(TokenError::NotLoggedIn)?;
        if auth.is_fresh(timeutil::now_unix(), self.skew) {
            return Ok(auth.access_token);
        }

        Ok(self.refresh_locked(&guard, &auth)?.access_token)
    }

    /// Refresh after an upstream request was rejected with `401`.
    ///
    /// Only refreshes when the token that failed is still the current one, so
    /// a burst of parallel requests cannot trigger a refresh stampede.
    pub fn force_refresh(&self, failed_token: &str) -> Result<String, TokenError> {
        let guard = self.lock();
        let Some(auth) = self.current()? else {
            return Err(TokenError::NotLoggedIn);
        };

        if auth.access_token != failed_token {
            // Somebody already replaced the token; use theirs.
            return Ok(auth.access_token);
        }

        Ok(self.refresh_locked(&guard, &auth)?.access_token)
    }

    /// Perform the refresh. The caller must hold the lock.
    fn refresh_locked(
        &self,
        _guard: &MutexGuard<'_, ()>,
        current: &StoredAuth,
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
        let tokens = self.client.refresh_as(&refresh_token, &client_id)?;
        let refreshed = merge_refreshed(current, &tokens);

        store::save(&self.path, &refreshed)?;
        Ok(refreshed)
    }

    /// Take the lock, ignoring poisoning — a panic elsewhere must not stop the
    /// proxy from refreshing its token.
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
}
