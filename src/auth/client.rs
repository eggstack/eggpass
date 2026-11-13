//! The x.ai OAuth endpoints.
//!
//! Every constant lives here so that a change to x.ai's endpoints or client id
//! is a one-line edit. The URLs are derived from the issuer, so overriding the
//! issuer is enough to point the whole flow somewhere else.

use std::time::Duration;

use serde_json::Value;

use crate::timeutil;

/// Errors raised while talking to the OAuth endpoints.
#[derive(Debug)]
pub enum AuthError {
    /// The request never reached the server.
    Network(String),
    /// The server answered with an unexpected status code.
    Http { status: u16, body: String },
    /// The server answered with an OAuth error object.
    OAuth {
        error: String,
        description: Option<String>,
    },
    /// The response could not be understood.
    InvalidResponse(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(message) => write!(f, "network error: {message}"),
            AuthError::Http { status, body } => {
                write!(f, "HTTP {status} from the OAuth endpoint")?;
                if !body.is_empty() {
                    write!(f, ": {body}")?;
                }
                Ok(())
            }
            AuthError::OAuth { error, description } => match description {
                Some(description) => write!(f, "{error}: {description}"),
                None => write!(f, "{error}"),
            },
            AuthError::InvalidResponse(message) => write!(f, "unexpected response: {message}"),
        }
    }
}

impl std::error::Error for AuthError {}

/// A set of tokens returned by the token endpoint.
#[derive(Debug, Clone, Default)]
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

impl TokenSet {
    /// Read a token endpoint response.
    pub fn from_json(value: &Value, now: i64) -> Result<TokenSet, AuthError> {
        let access_token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AuthError::InvalidResponse("no access_token in the response".into()))?
            .to_string();

        let refresh_token = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);

        let id_token = value
            .get("id_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);

        let token_type = value
            .get("token_type")
            .and_then(Value::as_str)
            .unwrap_or("Bearer")
            .to_string();

        let scope = value
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string);

        let expires_in = value.get("expires_in").and_then(Value::as_i64);

        let expires_at = match value.get("expires_at") {
            Some(Value::Number(number)) => number.as_i64(),
            Some(Value::String(text)) => timeutil::parse_rfc3339_to_unix(text),
            _ => None,
        }
        .or_else(|| expires_in.map(|seconds| now + seconds));

        Ok(TokenSet {
            access_token,
            refresh_token,
            id_token,
            token_type,
            scope,
            expires_at,
        })
    }
}

/// Account details, as far as we need them for `eggpass status`.
#[derive(Debug, Clone, Default)]
pub struct UserInfo {
    /// The provider's identifier for the account.
    pub user_id: Option<String>,
    /// The account's email address.
    pub email: Option<String>,
}

/// A device-code grant in flight.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    /// The secret code used when polling the token endpoint.
    pub device_code: String,
    /// The short code the user types into the verification page.
    pub user_code: String,
    /// The page the user must visit.
    pub verification_uri: String,
    /// The same page with the user code pre-filled.
    pub verification_uri_complete: Option<String>,
    /// How long the code stays valid, in seconds.
    pub expires_in: i64,
    /// How often we should poll, in seconds.
    pub interval: i64,
}

/// The outcome of one device-code poll.
#[derive(Debug)]
pub enum DevicePoll {
    /// The user has not approved the request yet.
    Pending,
    /// We are polling too fast; slow down.
    SlowDown,
    /// The user refused the request.
    Denied(String),
    /// The device code expired before the user acted.
    Expired,
    /// The user approved the request.
    Complete(TokenSet),
}

/// A client for one provider's OAuth endpoints.
///
/// The endpoints, scopes and extra parameters all come from a
/// [`ProviderSpec`](crate::provider::ProviderSpec), so this client works for
/// x.ai and for Sign in with ChatGPT without a branch on the provider.
#[derive(Debug, Clone)]
pub struct OAuthClient {
    client_id: String,
    issuer: String,
    scopes: Vec<String>,
    referrer: Option<String>,
    authorize_path: String,
    token_path: String,
    device_path: Option<String>,
    userinfo_path: Option<String>,
    extra_authorize: Vec<(String, String)>,
    registration_params: Vec<(String, String)>,
    extra_token: Vec<(String, String)>,
    agent: ureq::Agent,
}

impl OAuthClient {
    /// Build a client for a provider.
    ///
    /// `client_id` is the id to present: the provider's fixed public id, or
    /// the id it issued on an earlier registration.
    pub fn for_provider(spec: &crate::provider::ProviderSpec, client_id: &str) -> OAuthClient {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(180)))
            .user_agent(format!("eggpass/{}", env!("CARGO_PKG_VERSION")))
            .build();

        let referrer = spec
            .extra_authorize_params
            .iter()
            .find(|(name, _)| name == "referrer")
            .map(|(_, value)| value.clone());

        OAuthClient {
            client_id: client_id.to_string(),
            issuer: spec.issuer.trim_end_matches('/').to_string(),
            scopes: spec.scopes.clone(),
            referrer,
            authorize_path: spec.authorize_path.clone(),
            token_path: spec.token_path.clone(),
            device_path: spec.device_path.clone(),
            userinfo_path: spec.userinfo_path.clone(),
            extra_authorize: spec
                .extra_authorize_params
                .iter()
                .filter(|(name, _)| name != "referrer")
                .cloned()
                .collect(),
            registration_params: spec.registration_params.clone(),
            extra_token: spec.extra_token_params.clone(),
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// The client id in use.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The issuer in use.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The scopes in use, space separated.
    pub fn scope_string(&self) -> String {
        self.scopes.join(" ")
    }

    /// The authorization endpoint.
    pub fn authorize_endpoint(&self) -> String {
        format!("{}{}", self.issuer, self.authorize_path)
    }

    /// The token endpoint.
    pub fn token_endpoint(&self) -> String {
        format!("{}{}", self.issuer, self.token_path)
    }

    /// The device authorization endpoint.
    pub fn device_endpoint(&self) -> Option<String> {
        self.device_path
            .as_ref()
            .map(|path| format!("{}{}", self.issuer, path))
    }

    /// The userinfo endpoint.
    pub fn userinfo_endpoint(&self) -> Option<String> {
        self.userinfo_path
            .as_ref()
            .map(|path| format!("{}{}", self.issuer, path))
    }
    /// Build the URL the user's browser is sent to, with extra parameters.
    pub fn authorize_url_with(
        &self,
        redirect_uri: &str,
        code_challenge: &str,
        state: &str,
        nonce: &str,
        registration: bool,
        extra: &[(String, String)],
    ) -> String {
        let scope = self.scope_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", redirect_uri),
            ("scope", &scope),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("nonce", nonce),
        ];
        if let Some(referrer) = &self.referrer {
            params.push(("referrer", referrer));
        }
        if registration {
            for (name, value) in &self.registration_params {
                params.push((name.as_str(), value.as_str()));
            }
        }
        for (name, value) in &self.extra_authorize {
            params.push((name.as_str(), value.as_str()));
        }
        for (name, value) in extra {
            params.push((name.as_str(), value.as_str()));
        }
        format!("{}?{}", self.authorize_endpoint(), encode_query(&params))
    }
    pub fn exchange_code_as(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
        client_id: &str,
    ) -> Result<TokenSet, AuthError> {
        let mut params: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ];
        for (name, value) in &self.extra_token {
            params.push((name.as_str(), value.as_str()));
        }
        let value = self.post_form(&self.token_endpoint(), &params)?;
        TokenSet::from_json(&value, timeutil::now_unix())
    }

    /// Ask for a device code.
    pub fn request_device_code(&self) -> Result<DeviceCode, AuthError> {
        let Some(endpoint) = self.device_endpoint() else {
            return Err(AuthError::InvalidResponse(
                "this provider has no device-code flow".into(),
            ));
        };
        let scope = self.scope_string();
        let mut params: Vec<(&str, &str)> = vec![("client_id", &self.client_id), ("scope", &scope)];
        if let Some(referrer) = &self.referrer {
            params.push(("referrer", referrer));
        }
        let value = self.post_form(&endpoint, &params)?;

        let device_code = value
            .get("device_code")
            .and_then(Value::as_str)
            .ok_or_else(|| AuthError::InvalidResponse("no device_code in the response".into()))?
            .to_string();
        let user_code = value
            .get("user_code")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let verification_uri = value
            .get("verification_uri")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AuthError::InvalidResponse("no verification_uri in the response".into())
            })?
            .to_string();
        let verification_uri_complete = value
            .get("verification_uri_complete")
            .and_then(Value::as_str)
            .map(str::to_string);
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(1800);
        let interval = value
            .get("interval")
            .and_then(Value::as_i64)
            .unwrap_or(5)
            .max(1);

        Ok(DeviceCode {
            device_code,
            user_code,
            verification_uri,
            verification_uri_complete,
            expires_in,
            interval,
        })
    }

    /// Poll the token endpoint for a device-code grant.
    pub fn poll_device_code(&self, device_code: &str) -> Result<DevicePoll, AuthError> {
        let params: Vec<(&str, &str)> = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", &self.client_id),
        ];

        let response = self.post_form_raw(&self.token_endpoint(), &params)?;
        match response.status {
            200..=299 => {
                let value: Value = serde_json::from_str(&response.body).map_err(|err| {
                    AuthError::InvalidResponse(format!("could not parse the token response: {err}"))
                })?;
                Ok(DevicePoll::Complete(TokenSet::from_json(
                    &value,
                    timeutil::now_unix(),
                )?))
            }
            _ => {
                let error = serde_json::from_str::<Value>(&response.body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| response.body.clone());

                match error.as_str() {
                    "authorization_pending" => Ok(DevicePoll::Pending),
                    "slow_down" => Ok(DevicePoll::SlowDown),
                    "access_denied" => Ok(DevicePoll::Denied(error)),
                    "expired_token" => Ok(DevicePoll::Expired),
                    _ => Err(AuthError::OAuth {
                        error,
                        description: serde_json::from_str::<Value>(&response.body).ok().and_then(
                            |value| {
                                value
                                    .get("error_description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            },
                        ),
                    }),
                }
            }
        }
    }
    pub fn refresh_as(&self, refresh_token: &str, client_id: &str) -> Result<TokenSet, AuthError> {
        let mut params: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ];
        for (name, value) in &self.extra_token {
            params.push((name.as_str(), value.as_str()));
        }
        let value = self.post_form(&self.token_endpoint(), &params)?;
        TokenSet::from_json(&value, timeutil::now_unix())
    }

    /// Revoke a token at the provider.
    ///
    /// Revocation is best-effort: the caller deletes the local credential
    /// either way, and a token that could not be revoked simply lapses.
    pub fn revoke(&self, endpoint: &str, token: &str, client_id: &str) -> Result<(), AuthError> {
        let params: Vec<(&str, &str)> = vec![
            ("token", token),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ];
        let body = encode_query(&params);
        let response = self
            .agent
            .post(endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(|err| AuthError::Network(err.to_string()))?;

        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let mut response_body = response.into_body();
        let bytes = response_body
            .read_to_vec()
            .map_err(|err| AuthError::Network(err.to_string()))?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        Err(AuthError::Http { status, body: text })
    }

    /// Fetch the account details behind an access token.
    pub fn userinfo(&self, access_token: &str) -> Result<UserInfo, AuthError> {
        let Some(endpoint) = self.userinfo_endpoint() else {
            return Err(AuthError::InvalidResponse(
                "this provider has no userinfo endpoint".into(),
            ));
        };
        let response = self
            .agent
            .get(&endpoint)
            .header("Authorization", format!("Bearer {access_token}"))
            .call();

        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) => {
                return Err(AuthError::Http {
                    status,
                    body: String::new(),
                });
            }
            Err(err) => return Err(AuthError::Network(err.to_string())),
        };

        let status = response.status().as_u16();
        let mut body = response.into_body();
        let bytes = body
            .read_to_vec()
            .map_err(|err| AuthError::Network(err.to_string()))?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        if !(200..300).contains(&status) {
            return Err(AuthError::Http { status, body: text });
        }

        let value: Value = serde_json::from_str(&text)
            .map_err(|err| AuthError::InvalidResponse(err.to_string()))?;

        Ok(UserInfo {
            user_id: first_string(
                &value,
                &["sub", "user_id", "userId", "preferred_username", "username"],
            ),
            email: first_string(&value, &["email", "email_address", "emailAddress"]),
        })
    }

    /// Post a form-encoded request and parse the JSON response.
    fn post_form(&self, url: &str, params: &[(&str, &str)]) -> Result<Value, AuthError> {
        let response = self.post_form_raw(url, params)?;
        if !(200..300).contains(&response.status) {
            return Err(response.into_error());
        }
        serde_json::from_str(&response.body)
            .map_err(|err| AuthError::InvalidResponse(err.to_string()))
    }
    /// Post a form-encoded request, returning the raw status and body.
    ///
    /// The body is returned as it came, including OAuth error objects, so
    /// that the device-code poll can tell `authorization_pending` apart from
    /// a real failure.
    fn post_form_raw(&self, url: &str, params: &[(&str, &str)]) -> Result<RawResponse, AuthError> {
        let body = encode_query(params);
        let response = self
            .agent
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(|err| AuthError::Network(err.to_string()))?;

        let status = response.status().as_u16();
        let mut response_body = response.into_body();
        let bytes = response_body
            .read_to_vec()
            .map_err(|err| AuthError::Network(err.to_string()))?;
        let text = String::from_utf8_lossy(&bytes).to_string();

        Ok(RawResponse { status, body: text })
    }
}

/// A raw HTTP response from the token endpoint.
struct RawResponse {
    status: u16,
    body: String,
}

impl RawResponse {
    /// Turn a failed response into an error.
    ///
    /// An OAuth error object says more than a bare status code, so it is
    /// preferred when the body carries one.
    fn into_error(self) -> AuthError {
        if let Ok(value) = serde_json::from_str::<Value>(&self.body)
            && let Some(error) = value.get("error").and_then(Value::as_str)
        {
            return AuthError::OAuth {
                error: error.to_string(),
                description: value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
        }

        let body: String = self.body.chars().take(400).collect();
        AuthError::Http {
            status: self.status,
            body,
        }
    }
}

/// Read the first present string field from a JSON object.
fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str)
            && !text.is_empty()
        {
            return Some(text.to_string());
        }
    }
    None
}

/// Percent-encode a query string or form body.
pub fn encode_query(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode a single value, leaving only the unreserved characters alone.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Extract the `email` and `sub` claims from an OpenID Connect identity token.
///
/// The signature is *not* checked: these claims are only used to show the
/// account to the user, and the token came straight from the token endpoint.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An x.ai client pointing at a stub issuer, for the tests that need one.
    fn xai_client(client_id: &str) -> OAuthClient {
        let mut config = crate::config::Config::default();
        config.oauth.xai.issuer = "https://auth.x.ai".to_string();
        config.oauth.xai.scopes = vec!["openid".to_string(), "api:access".to_string()];
        let spec = crate::provider::spec_for(crate::provider::ProviderId::Xai, &config);
        OAuthClient::for_provider(&spec, client_id)
    }

    /// A client pointing at a local stub server.
    fn stub_client(issuer: &str) -> OAuthClient {
        let mut config = crate::config::Config::default();
        config.oauth.xai.issuer = issuer.to_string();
        config.oauth.xai.scopes = vec!["openid".to_string()];
        let spec = crate::provider::spec_for(crate::provider::ProviderId::Xai, &config);
        OAuthClient::for_provider(&spec, "client")
    }

    #[test]
    fn token_sets_are_read_from_the_response() {
        let value = json!({
            "access_token": "abc",
            "refresh_token": "def",
            "id_token": "ghi",
            "token_type": "Bearer",
            "expires_in": 3600
        });
        let tokens = TokenSet::from_json(&value, 1000).unwrap();
        assert_eq!(tokens.access_token, "abc");
        assert_eq!(tokens.refresh_token.as_deref(), Some("def"));
        assert_eq!(tokens.expires_at, Some(4600));
    }

    #[test]
    fn a_missing_access_token_is_an_error() {
        let value = json!({"token_type": "Bearer"});
        assert!(TokenSet::from_json(&value, 0).is_err());
    }

    #[test]
    fn an_expires_at_string_is_understood() {
        let value = json!({
            "access_token": "abc",
            "expires_at": "2026-12-13T05:12:18Z"
        });
        let tokens = TokenSet::from_json(&value, 0).unwrap();
        assert_eq!(tokens.expires_at, Some(1_797_138_738));
    }

    #[test]
    fn the_authorize_url_has_the_expected_shape() {
        let client = xai_client("client-id");
        let url = client.authorize_url_with(
            "http://127.0.0.1:1234/callback",
            "challenge",
            "state",
            "nonce",
            false,
            &[],
        );
        assert!(
            url.starts_with("https://auth.x.ai/oauth2/authorize?"),
            "{url}"
        );
        assert!(url.contains("response_type=code"), "{url}");
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert!(url.contains("scope=openid%20api%3Aaccess"), "{url}");
        assert!(url.contains("referrer=eggpass"), "{url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Fcallback"),
            "{url}"
        );
    }

    #[test]
    fn the_sign_in_with_chatgpt_url_carries_the_registration_parameters() {
        let config = crate::config::Config::default();
        let spec = crate::provider::spec_for(crate::provider::ProviderId::OpenAi, &config);
        let client = OAuthClient::for_provider(&spec, crate::provider::openai::ENTRY_CLIENT_ID);

        // A first registration carries the agent name hint.
        let url = client.authorize_url_with(
            "http://127.0.0.1:54321/auth/callback",
            "challenge",
            "state",
            "nonce",
            true,
            &[],
        );
        assert!(
            url.starts_with("https://auth.openai.com/api/accounts/authorize?"),
            "{url}"
        );
        assert!(url.contains("client_id=dynamic_agent_client"), "{url}");
        assert!(
            url.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"),
            "{url}"
        );
        assert!(url.contains("agent_name_hint=eggpass"), "{url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A54321%2Fauth%2Fcallback"),
            "{url}"
        );
    }

    #[test]
    fn the_registration_parameters_are_left_out_of_a_reauthorization() {
        let config = crate::config::Config::default();
        let spec = crate::provider::spec_for(crate::provider::ProviderId::OpenAi, &config);
        let client = OAuthClient::for_provider(&spec, "oaiapp_issued");

        let url = client.authorize_url_with(
            "http://127.0.0.1:54321/auth/callback",
            "challenge",
            "state",
            "nonce",
            false,
            &[],
        );
        assert!(url.contains("client_id=oaiapp_issued"), "{url}");
        assert!(!url.contains("agent_name_hint"), "{url}");
    }

    #[test]
    fn id_token_claims_are_read() {
        // {"alg":"none"} . {"sub":"user-1","email":"a@b.c"} .
        let id_token = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyLTEiLCJlbWFpbCI6ImFAYi5jIn0.";
        let claims = claims_from_id_token(id_token);
        assert_eq!(claims.user_id.as_deref(), Some("user-1"));
        assert_eq!(claims.email.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn a_pending_device_authorization_is_not_an_error() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // A token endpoint that always says the user has not approved yet.
        let listener = TcpListener::bind("127.0.0.1:0").expect("the stub binds");
        let port = listener.local_addr().expect("the stub address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = [0u8; 1024];
                let _ = stream.read(&mut head);
                let body = r#"{"error":"authorization_pending","error_description":"User has not yet authorized"}"#;
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let client = stub_client(&format!("http://127.0.0.1:{port}"));
        let poll = client
            .poll_device_code("device-code")
            .expect("the poll answers");
        assert!(
            matches!(poll, DevicePoll::Pending),
            "a pending approval must not be an error"
        );
    }

    #[test]
    fn percent_encoding_leaves_unreserved_characters_alone() {
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("a:b/c"), "a%3Ab%2Fc");
    }
}
