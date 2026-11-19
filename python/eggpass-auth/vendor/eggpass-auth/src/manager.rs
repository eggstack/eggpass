use std::future::Future;

use tokio::sync::Mutex;

use crate::client::OAuthClient;
use crate::error::StorageError;
use crate::token::Credentials;
use crate::{Error, Result};

/// Caller-owned asynchronous credential storage.
///
/// Implementations should make `save` an atomic replacement of one credential
/// set. This contract does not itself coordinate separate processes; an
/// application needing cross-process refresh coordination must implement it in
/// its storage layer. The trait is generic/RPITIT-based and is not a `dyn`
/// trait; this keeps store errors and futures allocation-free.
pub trait CredentialStore: Send + Sync {
    /// Load the current credentials, or `None` when the user is not logged in.
    fn load(
        &self,
    ) -> impl Future<Output = std::result::Result<Option<Credentials>, StorageError>> + Send;

    /// Atomically replace the stored credentials.
    fn save(
        &self,
        credentials: Credentials,
    ) -> impl Future<Output = std::result::Result<(), StorageError>> + Send;

    /// Remove credentials. Removing an absent entry should succeed.
    fn clear(&self) -> impl Future<Output = std::result::Result<(), StorageError>> + Send;
}

/// Async token manager over caller-owned credential storage.
///
/// One manager coalesces proactive and compare-and-refresh requests within this
/// process. It is not a cross-process lock; storage implementations must add
/// coordination if multiple processes can refresh the same account.
pub struct TokenManager<S> {
    client: OAuthClient,
    store: S,
    refresh_lock: Mutex<()>,
    refresh_skew_seconds: i64,
}

impl<S> std::fmt::Debug for TokenManager<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenManager")
            .field("client", &self.client)
            .field("refresh_skew_seconds", &self.refresh_skew_seconds)
            .field("store", &"<caller-owned credential store>")
            .finish()
    }
}

impl<S: CredentialStore> TokenManager<S> {
    /// Create a manager using the standard five-minute proactive refresh skew.
    pub fn new(client: OAuthClient, store: S) -> Self {
        Self {
            client,
            store,
            refresh_lock: Mutex::new(()),
            refresh_skew_seconds: 5 * 60,
        }
    }

    /// Configure how long before expiry a token should be refreshed.
    pub fn with_refresh_skew(mut self, seconds: i64) -> Self {
        self.refresh_skew_seconds = seconds.max(0);
        self
    }

    /// Read credentials without refreshing.
    pub async fn current(&self) -> Result<Option<Credentials>> {
        let credentials = self.store.load().await.map_err(Error::Storage)?;
        if let Some(credentials) = &credentials {
            self.client.validate_credentials(credentials)?;
        }
        Ok(credentials)
    }

    /// Store credentials obtained from an authorization or device flow.
    pub async fn save(&self, credentials: Credentials) -> Result<()> {
        credentials.validate()?;
        self.store.save(credentials).await.map_err(Error::Storage)
    }

    /// Remove credentials from the caller-owned store.
    pub async fn clear(&self) -> Result<()> {
        self.store.clear().await.map_err(Error::Storage)
    }

    /// Get a usable access token, refreshing proactively when it is near expiry.
    pub async fn access_token(&self) -> Result<String> {
        let current = self.current().await?.ok_or(Error::NotLoggedIn)?;
        if current.is_fresh(Credentials::now_unix(), self.refresh_skew_seconds) {
            return Ok(current.access_token().to_string());
        }

        let _guard = self.refresh_lock.lock().await;
        let current = self.current().await?.ok_or(Error::NotLoggedIn)?;
        if current.is_fresh(Credentials::now_unix(), self.refresh_skew_seconds) {
            return Ok(current.access_token().to_string());
        }
        self.refresh_current(&current).await
    }

    /// Refresh after an upstream request was rejected with HTTP 401.
    ///
    /// The refresh is performed only if `failed_access_token` is still current;
    /// if another task already replaced it, that newer token is returned.
    pub async fn force_refresh(&self, failed_access_token: &str) -> Result<String> {
        let _guard = self.refresh_lock.lock().await;
        let current = self.current().await?.ok_or(Error::NotLoggedIn)?;
        if current.access_token() != failed_access_token {
            return Ok(current.access_token().to_string());
        }
        self.refresh_current(&current).await
    }

    async fn refresh_current(&self, current: &Credentials) -> Result<String> {
        let refreshed = self.client.refresh_credentials(current).await?;
        let access_token = refreshed.access_token().to_string();
        self.store.save(refreshed).await.map_err(Error::Storage)?;
        Ok(access_token)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::token::TokenSet;

    #[derive(Clone, Default)]
    struct MemoryStore(Arc<tokio::sync::Mutex<Option<Credentials>>>);

    impl CredentialStore for MemoryStore {
        fn load(
            &self,
        ) -> impl Future<Output = std::result::Result<Option<Credentials>, StorageError>> + Send
        {
            let inner = self.0.clone();
            async move { Ok(inner.lock().await.clone()) }
        }

        fn save(
            &self,
            credentials: Credentials,
        ) -> impl Future<Output = std::result::Result<(), StorageError>> + Send {
            let inner = self.0.clone();
            async move {
                *inner.lock().await = Some(credentials);
                Ok(())
            }
        }

        fn clear(&self) -> impl Future<Output = std::result::Result<(), StorageError>> + Send {
            let inner = self.0.clone();
            async move {
                *inner.lock().await = None;
                Ok(())
            }
        }
    }

    fn credentials() -> Credentials {
        Credentials {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            id_token: Some("id-secret".into()),
            token_type: "Bearer".into(),
            scope: Some("openid offline_access".into()),
            client_id: "client".into(),
            issuer: "https://auth.x.ai".into(),
            expires_at: Some(1),
            obtained_at: 0,
        }
    }

    #[test]
    fn secret_bearing_credentials_have_redacted_debug() {
        let credentials = credentials();
        let debug = format!("{credentials:?}");
        for secret in ["access-secret", "refresh-secret", "id-secret"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn refresh_merge_retains_refresh_token_when_rotation_is_omitted() {
        let current = credentials();
        let tokens = TokenSet {
            access_token: "new-access".into(),
            refresh_token: None,
            id_token: None,
            token_type: "Bearer".into(),
            scope: None,
            expires_at: Some(10_000),
        };
        let merged = current.merged_refresh(tokens, 500);
        assert_eq!(merged.access_token(), "new-access");
        assert_eq!(merged.refresh_token(), Some("refresh-secret"));
        assert_eq!(merged.id_token(), Some("id-secret"));
        assert_eq!(merged.client_id(), "client");
    }

    #[tokio::test]
    async fn manager_rejects_missing_credentials_and_supports_clear() {
        let store = MemoryStore::default();
        let client = OAuthClient::new(
            eggfetch_core::Client::new(),
            crate::provider::OAuthProviderConfig::xai(),
        );
        let manager = TokenManager::new(client, store.clone());
        assert!(matches!(
            manager.access_token().await,
            Err(Error::NotLoggedIn)
        ));
        manager.save(credentials()).await.unwrap();
        assert!(manager.current().await.unwrap().is_some());
        manager.clear().await.unwrap();
        assert!(manager.current().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn manager_rejects_credentials_for_a_different_issuer() {
        let store = MemoryStore::default();
        let client = OAuthClient::new(
            eggfetch_core::Client::new(),
            crate::provider::OAuthProviderConfig::xai(),
        );
        let manager = TokenManager::new(client, store.clone());
        let mut wrong_issuer = credentials();
        wrong_issuer.issuer = crate::provider::OPENAI_ISSUER.into();
        store.save(wrong_issuer).await.unwrap();

        assert!(matches!(
            manager.current().await,
            Err(Error::InvalidConfiguration(_))
        ));
    }

    #[tokio::test]
    async fn force_refresh_uses_a_newer_token_without_another_refresh() {
        let store = MemoryStore::default();
        let client = OAuthClient::new(
            eggfetch_core::Client::new(),
            crate::provider::OAuthProviderConfig::xai(),
        );
        let manager = TokenManager::new(client, store.clone());
        let mut current = credentials();
        current.access_token = "newer-access".into();
        store.save(current).await.unwrap();

        assert_eq!(
            manager.force_refresh("failed-old-access").await.unwrap(),
            "newer-access"
        );
    }

    #[test]
    fn public_owned_api_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<OAuthClient>();
        assert_send_sync::<crate::client::PendingAuthorization>();
        assert_send_sync::<Credentials>();
        assert_send_sync::<crate::token::TokenSet>();
        assert_send_sync::<crate::token::DeviceCode>();
        assert_send_sync::<TokenManager<MemoryStore>>();
    }
}
