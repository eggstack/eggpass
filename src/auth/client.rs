//! Eggpass's synchronous host adapter for the async `eggpass-auth` protocol client.
//!
//! Runtime ownership lives in [`super::bridge`]. This module keeps the CLI and
//! proxy interfaces synchronous while delegating OAuth transport and protocol
//! validation to the shared async library.

use std::fmt;
use std::time::Duration;

use eggpass_auth::{
    CancellationToken, ClientIdPolicy, Credentials, DeviceCode, EggfetchClient,
    OAuthProviderConfig, PendingAuthorization, TokenSet as LibraryTokenSet,
};
use serde_json::Value;

use crate::provider::ProviderSpec;

/// Errors raised while talking to OAuth endpoints.
#[non_exhaustive]
pub enum AuthError {
    /// The request could not be completed.
    Network,
    /// A configured OAuth HTTP request deadline elapsed.
    Timeout(&'static str),
    /// The server answered with an unexpected status code.
    Http { status: u16 },
    /// The server answered with a standard OAuth error code.
    OAuth { error: String },
    /// The provider denied a device or browser authorization.
    ProviderDenied(String),
    /// The device authorization expired before approval.
    DeviceExpired,
    /// The configuration or response could not be understood.
    InvalidResponse,
}

impl fmt::Debug for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => f.write_str("Network"),
            Self::Timeout(operation) => f.debug_tuple("Timeout").field(operation).finish(),
            Self::Http { status } => f.debug_struct("Http").field("status", status).finish(),
            Self::OAuth { error } => f.debug_struct("OAuth").field("error", error).finish(),
            Self::ProviderDenied(error) => f.debug_tuple("ProviderDenied").field(error).finish(),
            Self::DeviceExpired => f.write_str("DeviceExpired"),
            Self::InvalidResponse => f.write_str("InvalidResponse"),
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => f.write_str("OAuth network request failed"),
            Self::Timeout(operation) => write!(f, "OAuth {operation} timed out"),
            Self::Http { status } => write!(f, "HTTP {status} from the OAuth endpoint"),
            Self::OAuth { error } => write!(f, "OAuth endpoint refused the request ({error})"),
            Self::ProviderDenied(error) => write!(f, "the provider denied authorization ({error})"),
            Self::DeviceExpired => f.write_str("device authorization expired"),
            Self::InvalidResponse => f.write_str("the OAuth response or configuration was invalid"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<eggpass_auth::Error> for AuthError {
    fn from(error: eggpass_auth::Error) -> Self {
        use eggpass_auth::Error as LibraryError;
        match error {
            LibraryError::HttpStatus { status } => Self::Http { status },
            LibraryError::OAuth { code, .. } => Self::OAuth { error: code },
            LibraryError::Transport => Self::Network,
            LibraryError::Timeout(operation) => Self::Timeout(operation),
            LibraryError::ProviderDenied(code) => Self::ProviderDenied(code),
            LibraryError::DeviceExpired => Self::DeviceExpired,
            _ => Self::InvalidResponse,
        }
    }
}

/// Host-side token set used by Eggpass's file adapter.
#[derive(Clone, Default)]
pub struct TokenSet {
    /// The bearer token used against the API.
    pub access_token: String,
    /// The token used to obtain a new access token.
    pub refresh_token: Option<String>,
    /// The OpenID Connect identity token.
    pub id_token: Option<String>,
    /// The token type, normally `Bearer`.
    pub token_type: String,
    /// The scopes the token was actually granted.
    pub scope: Option<String>,
    /// The absolute expiry, in seconds since the unix epoch.
    pub expires_at: Option<i64>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Account details needed by Eggpass's status output.
#[derive(Debug, Clone, Default)]
pub struct UserInfo {
    /// The provider's identifier for the account.
    pub user_id: Option<String>,
    /// The account's email address.
    pub email: Option<String>,
}

/// Async OAuth client exposed through Eggpass's synchronous host boundary.
#[derive(Clone)]
pub struct OAuthClient {
    inner: Option<eggpass_auth::OAuthClient>,
    client_id: String,
    issuer: String,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &"<redacted>")
            .field("issuer", &self.issuer)
            .field("library_client", &self.inner.is_some())
            .finish()
    }
}

impl OAuthClient {
    /// Build a client for a provider. Configuration errors are retained and
    /// returned by the first operation, rather than panicking during startup.
    pub fn for_provider(spec: &ProviderSpec, client_id: &str) -> Self {
        match build_library_client(spec, client_id) {
            Ok(inner) => Self {
                inner: Some(inner),
                client_id: client_id.to_string(),
                issuer: spec.issuer.trim_end_matches('/').to_string(),
            },
            Err(_) => Self {
                inner: None,
                client_id: client_id.to_string(),
                issuer: spec.issuer.trim_end_matches('/').to_string(),
            },
        }
    }

    /// The client id in use.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    fn inner(&self) -> Result<&eggpass_auth::OAuthClient, AuthError> {
        self.inner.as_ref().ok_or(AuthError::InvalidResponse)
    }

    /// Begin a browser authorization transaction using the library's PKCE,
    /// state, nonce, and provider-parameter handling.
    pub fn begin_authorization(
        &self,
        redirect_uri: &str,
        existing: Option<&crate::auth::store::StoredAuth>,
    ) -> Result<PendingAuthorization, AuthError> {
        let credentials = existing.map(stored_credentials).transpose()?;
        self.inner()?
            .begin_authorization(redirect_uri, credentials.as_ref())
            .map_err(Into::into)
    }

    /// Validate a callback without consuming its pending transaction, allowing
    /// an interactive caller to reject bad input and keep waiting for a retry.
    pub fn validate_authorization_callback(
        &self,
        pending: &PendingAuthorization,
        callback_url: &str,
    ) -> Result<(), AuthError> {
        self.inner()?
            .validate_authorization_callback(pending, callback_url)
            .map_err(Into::into)
    }

    /// Complete a browser callback and exchange its authorization code.
    pub fn complete_authorization(
        &self,
        pending: PendingAuthorization,
        callback_url: &str,
    ) -> Result<Credentials, AuthError> {
        let inner = self.inner()?.clone();
        let callback_url = callback_url.to_string();
        super::bridge::block_on(async move {
            inner.complete_authorization(pending, &callback_url).await
        })
        .map_err(|_| AuthError::Network)?
        .map_err(Into::into)
    }

    /// Complete a fixed-client-ID authorization from a manually pasted code.
    pub fn complete_authorization_with_code(
        &self,
        pending: PendingAuthorization,
        code: &str,
    ) -> Result<Credentials, AuthError> {
        let inner = self.inner()?.clone();
        let code = code.to_string();
        super::bridge::block_on(async move {
            inner.complete_authorization_with_code(pending, &code).await
        })
        .map_err(|_| AuthError::Network)?
        .map_err(Into::into)
    }

    /// Request a device code through Eggfetch.
    pub fn request_device_code(&self) -> Result<DeviceCode, AuthError> {
        let inner = self.inner()?.clone();
        super::bridge::block_on(async move { inner.request_device_code().await })
            .map_err(|_| AuthError::Network)?
            .map_err(Into::into)
    }

    /// Poll a device authorization through the library's cancellable, RFC 8628
    /// polling loop, bounded by the server expiry and caller timeout.
    pub fn poll_device_authorization(
        &self,
        device: &DeviceCode,
        caller_timeout: Duration,
    ) -> Result<Credentials, AuthError> {
        let inner = self.inner()?.clone();
        let device = device.clone();
        let cancellation = CancellationToken::new();
        super::bridge::block_on(async move {
            inner
                .poll_device_authorization(&device, &cancellation, caller_timeout)
                .await
        })
        .map_err(|_| AuthError::Network)?
        .map_err(Into::into)
    }

    /// Refresh a token. The returned refresh token is optional; callers should
    /// retain the previous value if the provider omits rotation.
    pub fn refresh_as(&self, refresh_token: &str, client_id: &str) -> Result<TokenSet, AuthError> {
        self.refresh_as_with_options(refresh_token, client_id, None)
    }

    /// Refresh a token with a shorter per-request deadline.
    ///
    /// Used by the daemon's background check so a provider outage cannot hold
    /// shutdown open for the interactive OAuth client's longer deadline.
    pub fn refresh_as_with_timeout(
        &self,
        refresh_token: &str,
        client_id: &str,
        timeout: Duration,
    ) -> Result<TokenSet, AuthError> {
        self.refresh_as_with_options(refresh_token, client_id, Some(timeout))
    }

    fn refresh_as_with_options(
        &self,
        refresh_token: &str,
        client_id: &str,
        timeout: Option<Duration>,
    ) -> Result<TokenSet, AuthError> {
        let mut inner = self.inner()?.clone();
        if let Some(timeout) = timeout {
            let mut options = eggpass_auth::ClientOptions::default();
            options.timeout.pool = Some(timeout);
            options.timeout.connect = Some(timeout);
            options.timeout.write = Some(timeout);
            options.timeout.read = Some(timeout);
            options.timeout.total = Some(timeout);
            inner = inner.with_options(options).map_err(AuthError::from)?;
        }
        let refresh_token = refresh_token.to_string();
        let client_id = client_id.to_string();
        let tokens = super::bridge::block_on(async move {
            inner.refresh_token(&refresh_token, Some(&client_id)).await
        })
        .map_err(|_| AuthError::Network)?
        .map_err(AuthError::from)?;
        Ok(token_set(tokens))
    }

    /// Revoke a token at this provider. The endpoint argument is retained for
    /// the host call shape; the validated provider profile selects the actual
    /// endpoint.
    pub fn revoke(&self, _endpoint: &str, token: &str, client_id: &str) -> Result<(), AuthError> {
        let inner = self.inner()?.clone();
        let token = token.to_string();
        let client_id = client_id.to_string();
        super::bridge::block_on(async move { inner.revoke(&token, Some(&client_id)).await })
            .map_err(|_| AuthError::Network)?
            .map_err(Into::into)
    }

    /// Fetch account details for a bearer token.
    pub fn userinfo(&self, access_token: &str) -> Result<UserInfo, AuthError> {
        let inner = self.inner()?.clone();
        let access_token = access_token.to_string();
        let info = super::bridge::block_on(async move { inner.userinfo(&access_token).await })
            .map_err(|_| AuthError::Network)?
            .map_err(AuthError::from)?;
        Ok(UserInfo {
            user_id: info.user_id,
            email: info.email,
        })
    }
}

fn build_library_client(
    spec: &ProviderSpec,
    client_id: &str,
) -> eggpass_auth::Result<eggpass_auth::OAuthClient> {
    let issuer = spec.issuer.trim_end_matches('/');
    let mut builder = OAuthProviderConfig::custom(spec.display_name, issuer, client_id)
        .authorization_endpoint(format!("{issuer}{}", spec.authorize_path))
        .token_endpoint(format!("{issuer}{}", spec.token_path))
        .callback_path(spec.callback_path)
        .scopes(spec.scopes.clone())
        .client_id_policy(if spec.issues_client_id {
            ClientIdPolicy::IssuedOnRegistration
        } else {
            ClientIdPolicy::Fixed
        });
    if let Some(path) = &spec.device_path {
        builder = builder.device_authorization_endpoint(format!("{issuer}{path}"));
    }
    if let Some(path) = &spec.userinfo_path {
        builder = builder.userinfo_endpoint(format!("{issuer}{path}"));
    }
    if let Some(path) = &spec.revocation_path {
        builder = builder.revocation_endpoint(format!("{issuer}{path}"));
    }
    for (name, value) in &spec.extra_authorize_params {
        builder = builder.authorization_parameter(name, value);
        if name == "referrer" && spec.device_path.is_some() {
            builder = builder.device_parameter(name, value);
        }
    }
    for (name, value) in &spec.registration_params {
        builder = builder.registration_parameter(name, value);
    }
    for (name, value) in &spec.extra_token_params {
        builder = builder.token_parameter(name, value);
    }
    for host in spec.trusted_device_hosts {
        builder = builder.trusted_device_host(*host);
    }
    let config = builder.build()?;
    Ok(eggpass_auth::OAuthClient::new(
        EggfetchClient::new(),
        config,
    ))
}

fn stored_credentials(stored: &crate::auth::store::StoredAuth) -> Result<Credentials, AuthError> {
    let value = serde_json::json!({
        "access_token": stored.access_token,
        "refresh_token": stored.refresh_token,
        "id_token": stored.id_token,
        "token_type": stored.token_type,
        "scope": stored.scope,
        "client_id": stored.client_id,
        "issuer": stored.issuer,
        "expires_at": stored.expires_at,
        "obtained_at": stored.obtained_at,
    });
    serde_json::from_value(value).map_err(|_| AuthError::InvalidResponse)
}

fn token_set(tokens: LibraryTokenSet) -> TokenSet {
    TokenSet {
        access_token: tokens.access_token().to_string(),
        refresh_token: tokens.refresh_token().map(str::to_string),
        id_token: tokens.id_token().map(str::to_string),
        token_type: tokens.token_type().to_string(),
        scope: tokens.scope().map(str::to_string),
        expires_at: tokens.expires_at(),
    }
}

/// Extract the `email` and `sub` claims from an OpenID Connect identity token.
///
/// The signature is not checked; these claims are only used to display the
/// account label and the token came directly from the provider's token endpoint.
pub fn claims_from_id_token(id_token: &str) -> UserInfo {
    let Some(payload) = id_token.split('.').nth(1) else {
        return UserInfo::default();
    };
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(payload) else {
        return UserInfo::default();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&decoded) else {
        return UserInfo::default();
    };
    UserInfo {
        user_id: first_string(&value, &["sub", "user_id", "userId"]),
        email: first_string(&value, &["email", "email_address", "emailAddress"]),
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_spec_is_translated_into_the_shared_library_profile() {
        let config = crate::config::Config::default();
        let xai = crate::provider::spec_for(crate::provider::ProviderId::Xai, &config);
        let client = OAuthClient::for_provider(&xai, &xai.entry_client_id);
        let pending = client
            .begin_authorization("http://127.0.0.1:43123/callback", None)
            .unwrap();
        let url = url::Url::parse(pending.authorization_url()).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params.get("referrer").map(String::as_str), Some("eggpass"));
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );

        let openai = crate::provider::spec_for(crate::provider::ProviderId::OpenAi, &config);
        let openai_client = OAuthClient::for_provider(&openai, &openai.entry_client_id);
        let pending = openai_client
            .begin_authorization("http://127.0.0.1:43123/auth/callback", None)
            .unwrap();
        let url = url::Url::parse(pending.authorization_url()).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("agent_name_hint").map(String::as_str),
            Some("eggpass")
        );
        assert_eq!(
            params.get("resource").map(String::as_str),
            Some(crate::provider::openai::RESOURCE)
        );
    }

    #[test]
    fn secret_bearing_host_token_debug_is_redacted() {
        let tokens = TokenSet {
            access_token: "access-secret".to_string(),
            refresh_token: Some("refresh-secret".to_string()),
            id_token: Some("id-secret".to_string()),
            ..TokenSet::default()
        };
        let debug = format!("{tokens:?}");
        for secret in ["access-secret", "refresh-secret", "id-secret"] {
            assert!(!debug.contains(secret));
        }
    }
}
