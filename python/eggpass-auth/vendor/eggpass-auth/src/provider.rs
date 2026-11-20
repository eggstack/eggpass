use std::fmt;

use crate::{Error, Result};

/// The x.ai OAuth issuer.
pub const XAI_ISSUER: &str = "https://auth.x.ai";
/// The public x.ai / Grok CLI client identifier.
pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
/// The OpenAI OAuth issuer.
pub const OPENAI_ISSUER: &str = "https://auth.openai.com";
/// The temporary client identifier used to register an OpenAI agent.
pub const OPENAI_ENTRY_CLIENT_ID: &str = "dynamic_agent_client";
/// The OpenAI resource sent during authorization and token requests.
pub const OPENAI_RESOURCE: &str = "https://api.openai.com/v1";

const XAI_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
    "conversations:read",
    "conversations:write",
    "workspaces:read",
    "workspaces:write",
];
const XAI_FALLBACK_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
];
const OPENAI_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "resource.invoke",
    "chatgpt.tokens.use.direct",
];

/// Whether a provider uses a fixed client ID or issues one during registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientIdPolicy {
    /// The configured public client ID is used for every operation.
    Fixed,
    /// The provider returns a client ID during the first authorization; callers
    /// must persist it and pass it back for later login and refresh operations.
    IssuedOnRegistration,
}

/// Auth-specific OAuth endpoints and parameters for one provider.
///
/// This type intentionally excludes inference URLs, request policies, and other
/// application-specific provider metadata. Use [`OAuthProviderConfig::xai`],
/// [`OAuthProviderConfig::openai`], or [`OAuthProviderConfig::custom`] to
/// construct a validated profile.
#[derive(Clone)]
pub struct OAuthProviderConfig {
    pub(crate) display_name: String,
    pub(crate) issuer: String,
    pub(crate) authorization_endpoint: String,
    pub(crate) token_endpoint: String,
    pub(crate) device_authorization_endpoint: Option<String>,
    pub(crate) userinfo_endpoint: Option<String>,
    pub(crate) revocation_endpoint: Option<String>,
    pub(crate) callback_path: String,
    pub(crate) client_id: String,
    pub(crate) client_id_policy: ClientIdPolicy,
    pub(crate) scopes: Vec<String>,
    pub(crate) authorization_parameters: Vec<(String, String)>,
    pub(crate) registration_parameters: Vec<(String, String)>,
    pub(crate) token_parameters: Vec<(String, String)>,
    pub(crate) device_parameters: Vec<(String, String)>,
    pub(crate) trusted_device_hosts: Vec<String>,
    pub(crate) xai_referrer_fallback: Option<String>,
}

impl fmt::Debug for OAuthProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthProviderConfig")
            .field("display_name", &self.display_name)
            .field("issuer", &self.issuer)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field(
                "device_authorization_endpoint",
                &self.device_authorization_endpoint,
            )
            .field("userinfo_endpoint", &self.userinfo_endpoint)
            .field("revocation_endpoint", &self.revocation_endpoint)
            .field("callback_path", &self.callback_path)
            .field("client_id", &"<redacted>")
            .field("client_id_policy", &self.client_id_policy)
            .field("scopes", &self.scopes)
            .field(
                "authorization_parameter_names",
                &parameter_names(&self.authorization_parameters),
            )
            .field(
                "registration_parameter_names",
                &parameter_names(&self.registration_parameters),
            )
            .field(
                "token_parameter_names",
                &parameter_names(&self.token_parameters),
            )
            .field(
                "device_parameter_names",
                &parameter_names(&self.device_parameters),
            )
            .field("trusted_device_hosts", &self.trusted_device_hosts)
            .finish()
    }
}

fn parameter_names(parameters: &[(String, String)]) -> Vec<&str> {
    parameters.iter().map(|(name, _)| name.as_str()).collect()
}

impl OAuthProviderConfig {
    /// The built-in x.ai OAuth profile with the full Grok CLI scope set.
    ///
    /// The profile sends `referrer=eggpass`, as Eggpass does. If x.ai returns a
    /// client-configuration OAuth error, applications may start a new
    /// authorization transaction with [`Self::xai_compatibility_fallback`];
    /// token exchanges and refreshes are never automatically retried.
    pub fn xai() -> Self {
        Self::xai_builder(false)
            .build()
            .expect("built-in x.ai profile is valid")
    }

    /// The x.ai fallback profile used when a provider rejects the primary
    /// referrer or full scope set. It uses `referrer=grok-build` and the shorter
    /// reference-client scope set. Start a new user authorization with it; do
    /// not retry an authorization-code exchange or refresh request.
    pub fn xai_compatibility_fallback() -> Self {
        Self::xai_builder(true)
            .build()
            .expect("built-in x.ai fallback profile is valid")
    }

    fn xai_builder(fallback: bool) -> OAuthProviderConfigBuilder {
        let scopes = if fallback {
            XAI_FALLBACK_SCOPES
        } else {
            XAI_SCOPES
        };
        let referrer = if fallback { "grok-build" } else { "eggpass" };
        OAuthProviderConfigBuilder::new("x.ai", XAI_ISSUER, XAI_CLIENT_ID)
            .authorization_endpoint(format!("{XAI_ISSUER}/oauth2/authorize"))
            .token_endpoint(format!("{XAI_ISSUER}/oauth2/token"))
            .device_authorization_endpoint(format!("{XAI_ISSUER}/oauth2/device/code"))
            .userinfo_endpoint(format!("{XAI_ISSUER}/oauth2/userinfo"))
            .revocation_endpoint(format!("{XAI_ISSUER}/oauth2/revoke"))
            .callback_path("/callback")
            .scopes(scopes.iter().copied())
            .authorization_parameter("referrer", referrer)
            .device_parameter("referrer", referrer)
            .trusted_device_host("accounts.x.ai")
            .trusted_device_host("auth.x.ai")
            .trusted_device_host("x.ai")
            .xai_referrer_fallback(if fallback { "eggpass" } else { "grok-build" })
    }

    /// The built-in Sign in with ChatGPT for Open Source profile.
    ///
    /// OpenAI issues a client ID during the first registration. Save the
    /// callback's issued ID in the returned [`Credentials`] and pass it to the
    /// next authorization/refresh call. The built-in registration hint is
    /// `eggpass`; callers may build a custom profile to use another hint.
    pub fn openai() -> Self {
        Self::openai_builder(None)
            .build()
            .expect("built-in OpenAI profile is valid")
    }

    /// OpenAI's profile with a caller-owned, stable host identifier.
    pub fn openai_with_host_id(host_id: impl Into<String>) -> Result<Self> {
        Self::openai_builder(Some(host_id.into())).build()
    }

    fn openai_builder(host_id: Option<String>) -> OAuthProviderConfigBuilder {
        let mut builder =
            OAuthProviderConfigBuilder::new("OpenAI", OPENAI_ISSUER, OPENAI_ENTRY_CLIENT_ID)
                .authorization_endpoint(format!("{OPENAI_ISSUER}/api/accounts/authorize"))
                .token_endpoint(format!("{OPENAI_ISSUER}/api/accounts/oauth/token"))
                .userinfo_endpoint(format!("{OPENAI_ISSUER}/api/accounts/oauth/userinfo"))
                .revocation_endpoint(format!("{OPENAI_ISSUER}/api/accounts/oauth/revoke"))
                .callback_path("/auth/callback")
                .client_id_policy(ClientIdPolicy::IssuedOnRegistration)
                .scopes(OPENAI_SCOPES.iter().copied())
                .authorization_parameter("resource", OPENAI_RESOURCE)
                .registration_parameter("agent_name_hint", "eggpass")
                .token_parameter("resource", OPENAI_RESOURCE);
        if let Some(host_id) = host_id {
            builder = builder.authorization_parameter("ext_agent_host_id", host_id);
        }
        builder
    }

    /// Start a custom OAuth profile. Set the absolute authorization and token
    /// endpoint URLs and add only the optional endpoints/parameters the
    /// provider supports, then call [`OAuthProviderConfigBuilder::build`].
    pub fn custom(
        display_name: impl Into<String>,
        issuer: impl Into<String>,
        client_id: impl Into<String>,
    ) -> OAuthProviderConfigBuilder {
        OAuthProviderConfigBuilder::new(display_name, issuer, client_id)
    }

    /// Provider display name, safe to show in UI.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// The issuer associated with credentials from this profile.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The configured callback path.
    pub fn callback_path(&self) -> &str {
        &self.callback_path
    }

    /// The configured client ID to use before a provider-issued ID is stored.
    pub fn entry_client_id(&self) -> &str {
        &self.client_id
    }

    /// How this provider assigns client IDs.
    pub fn client_id_policy(&self) -> ClientIdPolicy {
        self.client_id_policy
    }

    /// The scopes requested by this profile.
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// Return a profile with a different client ID, useful for a caller-owned
    /// provider registration or a changed public client ID.
    pub fn with_client_id(mut self, client_id: impl Into<String>) -> Result<Self> {
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            return Err(Error::InvalidConfiguration("client ID must not be empty"));
        }
        self.client_id = client_id;
        Ok(self)
    }

    /// Add or replace a non-secret authorization parameter. Existing values
    /// with the same name are replaced.
    pub fn with_authorization_parameter(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self> {
        insert_parameter(
            &mut self.authorization_parameters,
            name.into(),
            value.into(),
        )?;
        Ok(self)
    }

    /// Add or replace a non-secret registration-only authorization parameter.
    pub fn with_registration_parameter(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self> {
        insert_parameter(&mut self.registration_parameters, name.into(), value.into())?;
        Ok(self)
    }

    /// Add or replace a non-secret parameter sent with every token request.
    pub fn with_token_parameter(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self> {
        insert_parameter(&mut self.token_parameters, name.into(), value.into())?;
        Ok(self)
    }
}

/// Builder for an [`OAuthProviderConfig`].
///
/// Endpoint URLs must be HTTPS, except for loopback HTTP endpoints used by
/// deterministic local tests. Callback paths are compared exactly when a
/// callback is completed.
pub struct OAuthProviderConfigBuilder {
    config: OAuthProviderConfig,
}

impl OAuthProviderConfigBuilder {
    fn new(
        display_name: impl Into<String>,
        issuer: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Self {
        Self {
            config: OAuthProviderConfig {
                display_name: display_name.into(),
                issuer: issuer.into(),
                authorization_endpoint: String::new(),
                token_endpoint: String::new(),
                device_authorization_endpoint: None,
                userinfo_endpoint: None,
                revocation_endpoint: None,
                callback_path: "/callback".to_string(),
                client_id: client_id.into(),
                client_id_policy: ClientIdPolicy::Fixed,
                scopes: Vec::new(),
                authorization_parameters: Vec::new(),
                registration_parameters: Vec::new(),
                token_parameters: Vec::new(),
                device_parameters: Vec::new(),
                trusted_device_hosts: Vec::new(),
                xai_referrer_fallback: None,
            },
        }
    }

    pub fn authorization_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.authorization_endpoint = endpoint.into();
        self
    }

    pub fn token_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.token_endpoint = endpoint.into();
        self
    }

    pub fn device_authorization_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.device_authorization_endpoint = Some(endpoint.into());
        self
    }

    pub fn userinfo_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.userinfo_endpoint = Some(endpoint.into());
        self
    }

    pub fn revocation_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.revocation_endpoint = Some(endpoint.into());
        self
    }

    pub fn callback_path(mut self, path: impl Into<String>) -> Self {
        self.config.callback_path = path.into();
        self
    }

    pub fn client_id_policy(mut self, policy: ClientIdPolicy) -> Self {
        self.config.client_id_policy = policy;
        self
    }

    pub fn scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    pub fn authorization_parameter(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let _ = insert_parameter(
            &mut self.config.authorization_parameters,
            name.into(),
            value.into(),
        );
        self
    }

    pub fn registration_parameter(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let _ = insert_parameter(
            &mut self.config.registration_parameters,
            name.into(),
            value.into(),
        );
        self
    }

    pub fn token_parameter(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let _ = insert_parameter(&mut self.config.token_parameters, name.into(), value.into());
        self
    }

    pub fn device_parameter(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let _ = insert_parameter(
            &mut self.config.device_parameters,
            name.into(),
            value.into(),
        );
        self
    }

    pub fn trusted_device_host(mut self, host: impl Into<String>) -> Self {
        self.config
            .trusted_device_hosts
            .push(host.into().to_ascii_lowercase());
        self
    }

    fn xai_referrer_fallback(mut self, value: impl Into<String>) -> Self {
        self.config.xai_referrer_fallback = Some(value.into());
        self
    }

    /// Validate endpoint URLs, callback path, client ID, and OAuth parameters.
    pub fn build(mut self) -> Result<OAuthProviderConfig> {
        if self.config.display_name.trim().is_empty() {
            return Err(Error::InvalidConfiguration(
                "provider display name must not be empty",
            ));
        }
        if self.config.client_id.trim().is_empty() {
            return Err(Error::InvalidConfiguration("client ID must not be empty"));
        }
        validate_endpoint(&self.config.issuer, "issuer")?;
        validate_endpoint(
            &self.config.authorization_endpoint,
            "authorization endpoint",
        )?;
        validate_endpoint(&self.config.token_endpoint, "token endpoint")?;
        for (endpoint, name) in [
            (
                self.config.device_authorization_endpoint.as_deref(),
                "device endpoint",
            ),
            (
                self.config.userinfo_endpoint.as_deref(),
                "userinfo endpoint",
            ),
            (
                self.config.revocation_endpoint.as_deref(),
                "revocation endpoint",
            ),
        ] {
            if let Some(endpoint) = endpoint {
                validate_endpoint(endpoint, name)?;
            }
        }
        validate_callback_path(&self.config.callback_path)?;
        if self
            .config
            .scopes
            .iter()
            .any(|scope| scope.is_empty() || scope.chars().any(char::is_whitespace))
        {
            return Err(Error::InvalidConfiguration(
                "each OAuth scope must be a non-empty token",
            ));
        }
        validate_parameters(&self.config.authorization_parameters)?;
        validate_parameters(&self.config.registration_parameters)?;
        validate_parameters(&self.config.token_parameters)?;
        validate_parameters(&self.config.device_parameters)?;
        for host in &self.config.trusted_device_hosts {
            if host.is_empty()
                || host.starts_with('.')
                || host.ends_with('.')
                || host.contains('/')
                || host.contains(':')
                || !host.is_ascii()
            {
                return Err(Error::InvalidConfiguration(
                    "trusted device hosts must be plain DNS hostnames",
                ));
            }
        }
        self.config.trusted_device_hosts.sort();
        self.config.trusted_device_hosts.dedup();
        Ok(self.config)
    }
}

fn validate_endpoint(value: &str, label: &'static str) -> Result<()> {
    let url = url::Url::parse(value)
        .map_err(|_| Error::InvalidConfiguration("OAuth endpoint URL is invalid"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::InvalidConfiguration(match label {
            "issuer" => "issuer must be an HTTP(S) origin without credentials, query, or fragment",
            _ => "OAuth endpoints must be HTTP(S) URLs without credentials, query, or fragment",
        }));
    }
    if url.scheme() != "https" && !is_loopback_url(&url) {
        return Err(Error::InvalidConfiguration(
            "plain HTTP is permitted only for loopback OAuth endpoints",
        ));
    }
    Ok(())
}

fn is_loopback_url(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn validate_callback_path(path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('?')
        || path.contains('#')
        || path.contains('\r')
        || path.contains('\n')
    {
        return Err(Error::InvalidConfiguration(
            "callback path must be an absolute path without query or fragment",
        ));
    }
    Ok(())
}

fn validate_parameters(parameters: &[(String, String)]) -> Result<()> {
    for (name, value) in parameters {
        if name.is_empty()
            || name.chars().any(|c| c.is_control())
            || value.chars().any(|c| c.is_control())
        {
            return Err(Error::InvalidConfiguration(
                "OAuth parameter names and values must not contain control characters",
            ));
        }
    }
    Ok(())
}

fn insert_parameter(
    parameters: &mut Vec<(String, String)>,
    name: String,
    value: String,
) -> Result<()> {
    if name.is_empty()
        || name.chars().any(|c| c.is_control())
        || value.chars().any(|c| c.is_control())
    {
        return Err(Error::InvalidConfiguration(
            "OAuth parameter names and values must not contain control characters",
        ));
    }
    if let Some((_, previous)) = parameters
        .iter_mut()
        .find(|(existing, _)| existing == &name)
    {
        *previous = value;
    } else {
        parameters.push((name, value));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_has_referrer_device_flow_and_trusted_hosts() {
        let config = OAuthProviderConfig::xai();
        assert_eq!(config.client_id, XAI_CLIENT_ID);
        assert_eq!(
            config.device_authorization_endpoint.as_deref(),
            Some("https://auth.x.ai/oauth2/device/code")
        );
        assert!(
            config
                .trusted_device_hosts
                .iter()
                .any(|host| host == "accounts.x.ai")
        );
        assert!(
            config
                .authorization_parameters
                .contains(&("referrer".into(), "eggpass".into()))
        );
        let fallback = OAuthProviderConfig::xai_compatibility_fallback();
        assert!(
            fallback
                .authorization_parameters
                .contains(&("referrer".into(), "grok-build".into()))
        );
        assert_eq!(fallback.scopes.len(), 6);
    }

    #[test]
    fn openai_registration_parameters_and_issued_id_policy_are_explicit() {
        let config = OAuthProviderConfig::openai();
        assert_eq!(
            config.client_id_policy,
            ClientIdPolicy::IssuedOnRegistration
        );
        assert!(
            config
                .authorization_parameters
                .contains(&("resource".into(), OPENAI_RESOURCE.into()))
        );
        assert!(
            config
                .token_parameters
                .contains(&("resource".into(), OPENAI_RESOURCE.into()))
        );
        assert!(
            config
                .registration_parameters
                .contains(&("agent_name_hint".into(), "eggpass".into()))
        );
        let with_host = OAuthProviderConfig::openai_with_host_id("urn:uuid:test").unwrap();
        assert!(
            with_host
                .authorization_parameters
                .contains(&("ext_agent_host_id".into(), "urn:uuid:test".into()))
        );
    }

    #[test]
    fn custom_profiles_reject_plain_http_outside_loopback_and_invalid_callback_paths() {
        let insecure = OAuthProviderConfig::custom("example", "https://issuer.example", "client")
            .authorization_endpoint("http://issuer.example/authorize")
            .token_endpoint("https://issuer.example/token")
            .build();
        assert!(matches!(insecure, Err(Error::InvalidConfiguration(_))));

        let invalid_path =
            OAuthProviderConfig::custom("example", "https://issuer.example", "client")
                .authorization_endpoint("https://issuer.example/authorize")
                .token_endpoint("https://issuer.example/token")
                .callback_path("/callback?wrong=1")
                .build();
        assert!(matches!(invalid_path, Err(Error::InvalidConfiguration(_))));
    }

    #[test]
    fn configuration_debug_redacts_client_and_parameter_values() {
        let config = OAuthProviderConfig::xai();
        let debug = format!("{config:?}");
        assert!(!debug.contains(XAI_CLIENT_ID));
        assert!(!debug.contains("eggpass"));
        assert!(debug.contains("referrer"));
    }
}
