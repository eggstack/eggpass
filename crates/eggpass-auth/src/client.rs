use std::future::Future;
use std::time::Duration;

use eggfetch_core::timeout::Timeout;
use eggfetch_core::{Client as HttpClient, redirect::RedirectPolicy};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::provider::{ClientIdPolicy, OAuthProviderConfig};
use crate::token::{Credentials, DeviceCode, DevicePoll, TokenSet, UserInfo};
use crate::{Error, Result};

/// Per-request timeout and response-size policy for OAuth HTTP exchanges.
///
/// Every OAuth request explicitly overrides the supplied Eggfetch client's
/// timeout, redirect, retry, and default-auth settings. This avoids inheriting
/// accidental unbounded waits, redirects carrying credentials, or automatic
/// retries of one-shot code/refresh exchanges.
#[derive(Clone, Copy, Debug)]
pub struct ClientOptions {
    /// Eggfetch phase timeouts, including an absolute request-wide deadline.
    pub timeout: Timeout,
    /// Maximum decoded response body size in bytes.
    pub max_response_body_bytes: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Timeout {
                pool: Some(Duration::from_secs(10)),
                connect: Some(Duration::from_secs(15)),
                write: Some(Duration::from_secs(15)),
                read: Some(Duration::from_secs(30)),
                total: Some(Duration::from_secs(60)),
            },
            max_response_body_bytes: 1024 * 1024,
        }
    }
}

/// Async OAuth protocol client for one provider.
///
/// The caller supplies and owns the cloneable Eggfetch client. This type never
/// creates a Tokio runtime. Methods are awaited on the caller's runtime.
pub struct OAuthClient {
    pub(crate) http: HttpClient,
    pub(crate) provider: OAuthProviderConfig,
    pub(crate) options: ClientOptions,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("provider", &self.provider)
            .field("options", &self.options)
            .field("http", &"<caller-owned Eggfetch client>")
            .finish()
    }
}

impl Clone for OAuthClient {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            provider: self.provider.clone(),
            options: self.options,
        }
    }
}

impl OAuthClient {
    /// Construct a client around a caller-owned Eggfetch client and validated
    /// provider profile. No runtime is created or retained by this library.
    pub fn new(http: HttpClient, provider: OAuthProviderConfig) -> Self {
        Self {
            http,
            provider,
            options: ClientOptions::default(),
        }
    }

    /// Set the explicit request timeout and response body cap.
    pub fn with_options(mut self, options: ClientOptions) -> Result<Self> {
        if options.max_response_body_bytes == 0 {
            return Err(Error::InvalidConfiguration(
                "response body limit must be greater than zero",
            ));
        }
        self.options = options;
        Ok(self)
    }

    /// Provider profile used by this client.
    pub fn provider(&self) -> &OAuthProviderConfig {
        &self.provider
    }

    /// Start an authorization-code transaction. The returned value contains
    /// the authorization URL and private PKCE/state/nonce material; pass it to
    /// [`Self::complete_authorization`] after the caller receives the callback.
    pub fn begin_authorization(
        &self,
        redirect_uri: &str,
        existing: Option<&Credentials>,
    ) -> Result<PendingAuthorization> {
        crate::authorization::begin_authorization(self, redirect_uri, existing)
    }

    /// Validate a full callback URL without consuming the pending transaction.
    ///
    /// Use this when callback input can be retried, such as a URL pasted into a
    /// CLI. Completion performs the same validation again before exchanging it.
    pub fn validate_authorization_callback(
        &self,
        pending: &PendingAuthorization,
        callback_url: &str,
    ) -> Result<()> {
        crate::authorization::validate_authorization_callback(self, pending, callback_url)
    }

    /// Complete a pending transaction from a full callback URL. This validates
    /// callback origin and exact path, state, and (when OpenID Connect is
    /// requested) ID-token nonce before returning credentials.
    pub async fn complete_authorization(
        &self,
        pending: PendingAuthorization,
        callback_url: &str,
    ) -> Result<Credentials> {
        crate::authorization::complete_authorization(self, pending, callback_url).await
    }

    /// Complete an authorization when the provider displayed a code for the
    /// user to copy instead of redirecting to the registered callback.
    ///
    /// The pending PKCE verifier and nonce are still used and the ID-token
    /// nonce is still validated. This is available only for fixed-client-ID
    /// profiles; providers that issue a client ID need the callback metadata.
    pub async fn complete_authorization_with_code(
        &self,
        pending: PendingAuthorization,
        code: &str,
    ) -> Result<Credentials> {
        crate::authorization::complete_authorization_with_code(self, pending, code).await
    }

    /// Exchange an authorization code using caller-supplied PKCE material.
    ///
    /// Prefer [`Self::begin_authorization`] and [`Self::complete_authorization`]
    /// for browser flows; the lower-level form is for callers that manage the
    /// transaction themselves. `client_id` is an issued ID for providers that
    /// have [`ClientIdPolicy::IssuedOnRegistration`], or `None` to use the
    /// configured entry ID.
    pub async fn exchange_authorization_code(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
        client_id: Option<&str>,
    ) -> Result<Credentials> {
        validate_nonempty(code, "authorization code")?;
        validate_nonempty(code_verifier, "PKCE verifier")?;
        validate_redirect_uri(redirect_uri, &self.provider.callback_path)?;
        let client_id = self.client_id(client_id)?;
        let mut parameters = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code.to_string()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
            ("client_id".to_string(), client_id.clone()),
            ("code_verifier".to_string(), code_verifier.to_string()),
        ];
        parameters.extend(self.provider.token_parameters.clone());
        let value = self
            .post_form_json(&self.provider.token_endpoint, &parameters)
            .await?;
        let tokens = TokenSet::from_json(&value, Credentials::now_unix())?;
        Ok(Credentials::from_tokens(
            tokens,
            client_id,
            self.provider.issuer.clone(),
            Credentials::now_unix(),
        ))
    }

    /// Exchange a refresh token and return only the provider response tokens.
    ///
    /// This operation is not automatically retried. A successful response may
    /// rotate the refresh token; callers using explicit tokens must persist the
    /// returned token when present and retain their previous refresh token when
    /// the provider omits rotation.
    pub async fn refresh_token(
        &self,
        refresh_token: &str,
        client_id: Option<&str>,
    ) -> Result<TokenSet> {
        validate_nonempty(refresh_token, "refresh token")?;
        let client_id = self.client_id(client_id)?;
        let mut parameters = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.to_string()),
            ("client_id".to_string(), client_id),
        ];
        parameters.extend(self.provider.token_parameters.clone());
        let value = self
            .post_form_json(&self.provider.token_endpoint, &parameters)
            .await?;
        TokenSet::from_json(&value, Credentials::now_unix())
    }

    /// Refresh a stored credential set and merge it safely. The existing
    /// refresh token is retained if the provider omits a rotated token.
    pub async fn refresh_credentials(&self, current: &Credentials) -> Result<Credentials> {
        self.validate_credentials(current)?;
        let refresh_token = current.refresh_token().ok_or(Error::NoRefreshToken)?;
        let tokens = self
            .refresh_token(refresh_token, Some(current.client_id()))
            .await?;
        Ok(current.merged_refresh(tokens, Credentials::now_unix()))
    }

    /// Revoke a token when the provider publishes a revocation endpoint.
    pub async fn revoke(&self, token: &str, client_id: Option<&str>) -> Result<()> {
        validate_nonempty(token, "token")?;
        let endpoint = self
            .provider
            .revocation_endpoint
            .as_deref()
            .ok_or(Error::Unsupported("token revocation"))?;
        let client_id = self.client_id(client_id)?;
        let parameters = vec![
            ("token".to_string(), token.to_string()),
            ("token_type_hint".to_string(), "refresh_token".to_string()),
            ("client_id".to_string(), client_id),
        ];
        let response = self.post_form_raw(endpoint, &parameters).await?;
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(oauth_error(response.status, &response.body))
        }
    }

    /// Fetch non-secret user details using an explicit access token.
    pub async fn userinfo(&self, access_token: &str) -> Result<UserInfo> {
        validate_nonempty(access_token, "access token")?;
        let endpoint = self
            .provider
            .userinfo_endpoint
            .as_deref()
            .ok_or(Error::Unsupported("user-info lookup"))?;
        let builder = self
            .http
            .get(endpoint)
            .map_err(map_eggfetch_error)?
            .without_auth()
            .header("Authorization", &format!("Bearer {access_token}"))
            .timeout(self.options.timeout)
            .max_decoded_body_size(self.options.max_response_body_bytes)
            .without_retry()
            .redirect_policy(RedirectPolicy::new(false, 0));
        let mut response = builder.send().await.map_err(map_eggfetch_error)?;
        let status = response.status().as_u16();
        let body = read_limited_body(&mut response, self.options.max_response_body_bytes).await?;
        if !(200..300).contains(&status) {
            return Err(oauth_error(status, &body));
        }
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| Error::InvalidResponse("user-info response is not valid JSON"))?;
        Ok(UserInfo {
            user_id: first_string(
                &value,
                &["sub", "user_id", "userId", "preferred_username", "username"],
            ),
            email: first_string(&value, &["email", "email_address", "emailAddress"]),
        })
    }

    /// Request a device authorization. The verification URI and any completed
    /// verification URI are checked against the provider's exact HTTPS host
    /// allowlist before being returned to the caller.
    pub async fn request_device_code(&self) -> Result<DeviceCode> {
        let endpoint = self
            .provider
            .device_authorization_endpoint
            .as_deref()
            .ok_or(Error::Unsupported("device authorization"))?;
        let scope = self.provider.scopes.join(" ");
        let mut parameters = vec![
            ("client_id".to_string(), self.provider.client_id.clone()),
            ("scope".to_string(), scope),
        ];
        parameters.extend(self.provider.device_parameters.clone());
        let value = self.post_form_json(endpoint, &parameters).await?;

        let device_code = value
            .get("device_code")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(Error::InvalidResponse("device response has no device_code"))?
            .to_string();
        let user_code = value
            .get("user_code")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(Error::InvalidResponse("device response has no user_code"))?
            .to_string();
        let verification_uri = value
            .get("verification_uri")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(Error::InvalidResponse(
                "device response has no verification_uri",
            ))?
            .to_string();
        let verification_uri_complete = value
            .get("verification_uri_complete")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .filter(|seconds| *seconds > 0)
            .ok_or(Error::InvalidResponse(
                "device response expires_in must be positive",
            ))?;
        let interval = value
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .max(1);

        require_trusted_verification_url(&verification_uri, &self.provider.trusted_device_hosts)?;
        if let Some(complete) = &verification_uri_complete {
            require_trusted_verification_url(complete, &self.provider.trusted_device_hosts)?;
        }

        let expires_in = Duration::from_secs(expires_in);
        let expires_at =
            tokio::time::Instant::now()
                .checked_add(expires_in)
                .ok_or(Error::InvalidResponse(
                    "device response expiry is out of range",
                ))?;

        Ok(DeviceCode {
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
            expires_in,
            expires_at,
            interval: Duration::from_secs(interval),
        })
    }

    /// Perform one device poll. For a cancellable polling loop, use
    /// [`Self::poll_device_authorization`].
    pub async fn poll_device_code(&self, device_code: &str) -> Result<DevicePoll> {
        validate_nonempty(device_code, "device code")?;
        let client_id = self.client_id(None)?;
        let parameters = vec![
            (
                "grant_type".to_string(),
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
            ("device_code".to_string(), device_code.to_string()),
            ("client_id".to_string(), client_id),
        ];
        let response = self
            .post_form_raw(&self.provider.token_endpoint, &parameters)
            .await?;
        if (200..300).contains(&response.status) {
            let value: Value = serde_json::from_slice(&response.body)
                .map_err(|_| Error::InvalidResponse("device token response is not valid JSON"))?;
            return Ok(DevicePoll::Complete(TokenSet::from_json(
                &value,
                Credentials::now_unix(),
            )?));
        }
        let code = oauth_error_code(&response.body);
        match code.as_deref() {
            Some("authorization_pending") => Ok(DevicePoll::Pending),
            Some("slow_down") => Ok(DevicePoll::SlowDown),
            Some("access_denied" | "authorization_denied") => Ok(DevicePoll::Denied),
            Some("expired_token") => Ok(DevicePoll::Expired),
            _ => Err(oauth_error(response.status, &response.body)),
        }
    }

    /// Poll a device authorization until success, denial, server expiry,
    /// caller deadline, or cancellation. The loop sleeps before its first poll;
    /// each `slow_down` adds five seconds to all subsequent intervals.
    pub async fn poll_device_authorization(
        &self,
        device: &DeviceCode,
        cancellation: &CancellationToken,
        caller_timeout: Duration,
    ) -> Result<Credentials> {
        let client_id = self.client_id(None)?;
        poll_device_until(
            device,
            cancellation,
            caller_timeout,
            client_id,
            self.provider.issuer.clone(),
            |device_code| async move { self.poll_device_code(&device_code).await },
        )
        .await
    }

    pub(crate) fn validate_credentials(&self, credentials: &Credentials) -> Result<()> {
        credentials.validate()?;
        if !credentials.issuer().is_empty() && credentials.issuer() != self.provider.issuer {
            return Err(Error::InvalidConfiguration(
                "stored credentials belong to a different issuer",
            ));
        }
        Ok(())
    }

    pub(crate) fn client_id(&self, override_id: Option<&str>) -> Result<String> {
        match (self.provider.client_id_policy, override_id) {
            (ClientIdPolicy::IssuedOnRegistration, Some(value)) if !value.trim().is_empty() => {
                Ok(value.to_string())
            }
            (ClientIdPolicy::IssuedOnRegistration, _) => Ok(self.provider.client_id.clone()),
            (ClientIdPolicy::Fixed, Some(value)) if !value.trim().is_empty() => {
                Ok(value.to_string())
            }
            (ClientIdPolicy::Fixed, _) => Ok(self.provider.client_id.clone()),
        }
    }

    pub(crate) async fn post_form_json(
        &self,
        endpoint: &str,
        parameters: &[(String, String)],
    ) -> Result<Value> {
        let response = self.post_form_raw(endpoint, parameters).await?;
        if !(200..300).contains(&response.status) {
            return Err(oauth_error(response.status, &response.body));
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| Error::InvalidResponse("OAuth response is not valid JSON"))
    }

    pub(crate) async fn post_form_raw(
        &self,
        endpoint: &str,
        parameters: &[(String, String)],
    ) -> Result<RawResponse> {
        let body = {
            let mut encoded = url::form_urlencoded::Serializer::new(String::new());
            encoded.extend_pairs(
                parameters
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            );
            encoded.finish()
        };
        let builder = self
            .http
            .post(endpoint)
            .map_err(map_eggfetch_error)?
            .without_auth()
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .timeout(self.options.timeout)
            .max_decoded_body_size(self.options.max_response_body_bytes)
            .without_retry()
            .redirect_policy(RedirectPolicy::new(false, 0));
        let mut response = builder.send().await.map_err(map_eggfetch_error)?;
        let status = response.status().as_u16();
        let body = read_limited_body(&mut response, self.options.max_response_body_bytes).await?;
        Ok(RawResponse { status, body })
    }
}

async fn poll_device_until<F, Fut>(
    device: &DeviceCode,
    cancellation: &CancellationToken,
    caller_timeout: Duration,
    client_id: String,
    issuer: String,
    mut poll: F,
) -> Result<Credentials>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<DevicePoll>>,
{
    if device.expires_in.is_zero() {
        return Err(Error::DeviceExpired);
    }
    let start = tokio::time::Instant::now();
    let caller_deadline = start
        .checked_add(caller_timeout)
        .ok_or(Error::InvalidResponse(
            "device authorization deadline is out of range",
        ))?;
    let server_is_deadline = device.expires_at <= caller_deadline;
    let deadline = device.expires_at.min(caller_deadline);
    let mut interval = device.interval;

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return if server_is_deadline {
                Err(Error::DeviceExpired)
            } else {
                Err(Error::Timeout("device authorization"))
            };
        }
        let remaining = deadline.saturating_duration_since(now);
        let wake = now + interval.min(remaining);
        tokio::select! {
            _ = cancellation.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep_until(wake) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return if server_is_deadline {
                Err(Error::DeviceExpired)
            } else {
                Err(Error::Timeout("device authorization"))
            };
        }
        let poll_future = poll(device.device_code.clone());
        let result = tokio::select! {
            _ = cancellation.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, poll_future) => match result {
                Ok(result) => result?,
                Err(_) => return if server_is_deadline {
                    Err(Error::DeviceExpired)
                } else {
                    Err(Error::Timeout("device authorization"))
                },
            }
        };
        match result {
            DevicePoll::Pending => {}
            DevicePoll::SlowDown => interval = interval.saturating_add(Duration::from_secs(5)),
            DevicePoll::Denied => return Err(Error::ProviderDenied("access_denied".to_string())),
            DevicePoll::Expired => return Err(Error::DeviceExpired),
            DevicePoll::Complete(tokens) => {
                return Ok(Credentials::from_tokens(
                    tokens,
                    client_id,
                    issuer,
                    Credentials::now_unix(),
                ));
            }
        }
    }
}

/// Opaque, one-use authorization transaction. Sensitive fields are private and
/// redacted from `Debug`; it is consumed by callback completion.
pub struct PendingAuthorization {
    pub(crate) authorization_url: String,
    pub(crate) redirect_uri: String,
    pub(crate) code_verifier: String,
    pub(crate) state: String,
    pub(crate) nonce: String,
    pub(crate) client_id: String,
    pub(crate) registration: bool,
}

impl PendingAuthorization {
    /// URL to present to the user in an external browser or another UI.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("authorization_url", &"<redacted URL>")
            .field("redirect_uri", &self.redirect_uri)
            .field("code_verifier", &"<redacted>")
            .field("state", &"<redacted>")
            .field("nonce", &"<redacted>")
            .field("client_id", &"<redacted>")
            .field("registration", &self.registration)
            .finish()
    }
}

pub(crate) struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

async fn read_limited_body(
    response: &mut eggfetch_core::Response,
    maximum: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > maximum as u64)
    {
        return Err(Error::ResponseTooLarge);
    }
    let bytes = response.bytes().await.map_err(map_eggfetch_error)?;
    if bytes.len() > maximum {
        return Err(Error::ResponseTooLarge);
    }
    Ok(bytes.to_vec())
}

pub(crate) fn oauth_error(status: u16, body: &[u8]) -> Error {
    if let Some(code) = oauth_error_code(body) {
        Error::OAuth { status, code }
    } else {
        Error::HttpStatus { status }
    }
}

fn oauth_error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let code = value.get("error")?.as_str()?;
    crate::error::safe_oauth_error_code(code).map(str::to_string)
}

fn map_eggfetch_error(error: eggfetch_core::error::Error) -> Error {
    use eggfetch_core::error::Error as FetchError;
    match error {
        FetchError::DecodedBodyTooLarge => Error::ResponseTooLarge,
        FetchError::Timeout { .. } | FetchError::TransportIoTimeout { .. } => {
            Error::Timeout("HTTP request")
        }
        _ => Error::Transport,
    }
}

fn validate_nonempty(value: &str, label: &'static str) -> Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Error::InvalidConfiguration(match label {
            "authorization code" => {
                "authorization code must not be empty or contain control characters"
            }
            "PKCE verifier" => "PKCE verifier must not be empty or contain control characters",
            "refresh token" => "refresh token must not be empty or contain control characters",
            "access token" => "access token must not be empty or contain control characters",
            "device code" => "device code must not be empty or contain control characters",
            _ => "token value must not be empty or contain control characters",
        }));
    }
    Ok(())
}

pub(crate) fn validate_redirect_uri(redirect_uri: &str, expected_path: &str) -> Result<url::Url> {
    let redirect = url::Url::parse(redirect_uri)
        .map_err(|_| Error::InvalidConfiguration("redirect URI is invalid"))?;
    let loopback_http = match redirect.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if !matches!(redirect.scheme(), "https" | "http")
        || (redirect.scheme() == "http" && !loopback_http)
        || !redirect.username().is_empty()
        || redirect.password().is_some()
        || redirect.query().is_some()
        || redirect.fragment().is_some()
        || redirect.path() != expected_path
    {
        return Err(Error::InvalidConfiguration(
            "redirect URI must use HTTPS or loopback HTTP and the exact configured callback path",
        ));
    }
    Ok(redirect)
}

fn first_string(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str).map(str::to_owned))
}

pub(crate) fn require_trusted_verification_url(url: &str, trusted_hosts: &[String]) -> Result<()> {
    if trusted_hosts.is_empty()
        || url
            .bytes()
            .any(|byte| byte == b'\\' || byte.is_ascii_whitespace() || byte < 0x20)
    {
        return Err(Error::UntrustedVerificationUrl { host: None });
    }
    let parsed = match url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return Err(Error::UntrustedVerificationUrl { host: None }),
    };
    let host = parsed
        .host_str()
        .map(|host| host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase());
    let trusted = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && host.as_ref().is_some_and(|host| {
            trusted_hosts
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(host))
        });
    if trusted {
        Ok(())
    } else {
        let host = host.filter(|host| host.len() <= 253 && host.is_ascii());
        Err(Error::UntrustedVerificationUrl { host })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeouts_are_finite_and_response_limit_is_bounded() {
        let options = ClientOptions::default();
        assert!(options.timeout.connect.is_some());
        assert!(options.timeout.read.is_some());
        assert!(options.timeout.total.is_some());
        assert!(options.max_response_body_bytes > 0);
    }

    #[test]
    fn redirect_uri_path_is_exact_and_http_requires_loopback() {
        assert!(validate_redirect_uri("http://127.0.0.1:1234/callback", "/callback").is_ok());
        assert!(validate_redirect_uri("https://example.test/callback", "/callback").is_ok());
        assert!(validate_redirect_uri("http://example.test/callback", "/callback").is_err());
        assert!(validate_redirect_uri("http://127.0.0.1:1234/other", "/callback").is_err());
    }

    #[test]
    fn oauth_errors_keep_codes_but_never_descriptions_or_body() {
        let error = oauth_error(
            400,
            br#"{"error":"invalid_scope","error_description":"secret detail"}"#,
        );
        assert_eq!(error.oauth_code(), Some("invalid_scope"));
        let display = error.to_string();
        assert!(!display.contains("secret detail"));
        assert!(!format!("{error:?}").contains("secret detail"));

        let echoed_secret = oauth_error(400, br#"{"error":"refresh-secret-123"}"#);
        assert!(matches!(echoed_secret, Error::HttpStatus { status: 400 }));
        assert!(!echoed_secret.to_string().contains("refresh-secret-123"));
        assert!(!format!("{echoed_secret:?}").contains("refresh-secret-123"));
    }

    #[tokio::test]
    async fn refresh_credentials_rejects_a_different_issuer_before_network_access() {
        let client = OAuthClient::new(
            eggfetch_core::Client::new(),
            crate::provider::OAuthProviderConfig::xai(),
        );
        let credentials = Credentials {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            client_id: crate::provider::XAI_CLIENT_ID.into(),
            issuer: crate::provider::OPENAI_ISSUER.into(),
            ..Credentials::default()
        };

        assert!(matches!(
            client.refresh_credentials(&credentials).await,
            Err(Error::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn trusted_verification_url_requires_exact_https_host_and_rejects_parser_delimiters() {
        let hosts = vec!["accounts.x.ai".to_string(), "auth.x.ai".to_string()];
        assert!(
            require_trusted_verification_url("https://accounts.x.ai/verify?user_code=ABCD", &hosts)
                .is_ok()
        );
        assert!(
            require_trusted_verification_url("https://accounts.x.ai.evil.test/", &hosts).is_err()
        );
        assert!(
            require_trusted_verification_url("https://accounts.x.ai\\@evil.test/", &hosts).is_err()
        );
        assert!(require_trusted_verification_url("http://accounts.x.ai/", &hosts).is_err());
    }

    fn test_device(expires: u64, interval: u64) -> DeviceCode {
        let expires_in = Duration::from_secs(expires);
        DeviceCode {
            device_code: "device-secret".into(),
            user_code: "user-secret".into(),
            verification_uri: "https://accounts.x.ai/device".into(),
            verification_uri_complete: None,
            expires_in,
            expires_at: tokio::time::Instant::now() + expires_in,
            interval: Duration::from_secs(interval),
        }
    }

    #[test]
    fn device_code_debug_redacts_device_codes_and_verification_urls() {
        let mut device = test_device(30, 5);
        device.verification_uri = "https://accounts.x.ai/device?verification-secret=hidden".into();
        device.verification_uri_complete =
            Some("https://accounts.x.ai/device?user_code=hidden-code".into());

        let debug = format!("{device:?}");
        for secret in [
            "device-secret",
            "user-secret",
            "verification-secret",
            "hidden-code",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    fn test_tokens() -> TokenSet {
        TokenSet {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            id_token: None,
            token_type: "Bearer".into(),
            scope: None,
            expires_at: Some(10_000),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn device_slow_down_increases_all_later_intervals_by_five_seconds() {
        use std::sync::{Arc, Mutex};

        let observations = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = observations.clone();
        let counter = calls.clone();
        let result = poll_device_until(
            &test_device(30, 1),
            &CancellationToken::new(),
            Duration::from_secs(30),
            "client".into(),
            "https://auth.x.ai".into(),
            move |_| {
                let observed = observed.clone();
                let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    observed.lock().unwrap().push(tokio::time::Instant::now());
                    Ok(match index {
                        0 => DevicePoll::SlowDown,
                        1 => DevicePoll::Pending,
                        _ => DevicePoll::Complete(test_tokens()),
                    })
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(result.access_token(), "access");
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 3);
        assert_eq!(
            observations[1].duration_since(observations[0]),
            Duration::from_secs(6)
        );
        assert_eq!(
            observations[2].duration_since(observations[1]),
            Duration::from_secs(6)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn device_expiry_is_measured_from_code_issuance_not_polling_start() {
        let device = test_device(2, 1);
        tokio::time::advance(Duration::from_secs(3)).await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let error = poll_device_until(
            &device,
            &CancellationToken::new(),
            Duration::from_secs(10),
            "client".into(),
            "https://auth.x.ai".into(),
            move |_| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(DevicePoll::Pending)
                }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::DeviceExpired));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn device_flow_honors_short_expiry_and_caller_deadline() {
        let cancellation = CancellationToken::new();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let error = poll_device_until(
            &test_device(2, 5),
            &cancellation,
            Duration::from_secs(20),
            "client".into(),
            "https://auth.x.ai".into(),
            move |_| {
                let count = count.clone();
                async move {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(DevicePoll::Pending)
                }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::DeviceExpired));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let error = poll_device_until(
            &test_device(30, 5),
            &cancellation,
            Duration::from_secs(2),
            "client".into(),
            "https://auth.x.ai".into(),
            |_| async { Ok(DevicePoll::Pending) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Timeout("device authorization")));
    }

    #[tokio::test(start_paused = true)]
    async fn device_flow_supports_cancellation_denial_and_server_expiry() {
        let cancellation = CancellationToken::new();
        let cancel_signal = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_signal.cancel();
        });
        let error = poll_device_until(
            &test_device(30, 10),
            &cancellation,
            Duration::from_secs(20),
            "client".into(),
            "https://auth.x.ai".into(),
            |_| async { Ok(DevicePoll::Pending) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Cancelled));

        let error = poll_device_until(
            &test_device(30, 1),
            &CancellationToken::new(),
            Duration::from_secs(20),
            "client".into(),
            "https://auth.x.ai".into(),
            |_| async { Ok(DevicePoll::Denied) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::ProviderDenied(_)));

        let error = poll_device_until(
            &test_device(30, 1),
            &CancellationToken::new(),
            Duration::from_secs(20),
            "client".into(),
            "https://auth.x.ai".into(),
            |_| async { Ok(DevicePoll::Expired) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::DeviceExpired));
    }
}
