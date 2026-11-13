//! The x.ai provider.
//!
//! x.ai speaks the plain OpenAI wire format over OAuth 2.1 with PKCE, using
//! the public Grok CLI client id. Nothing has to be rewritten on the way
//! through: the request body is forwarded as it arrived.

use super::{CatalogueKind, NativePaths, ProviderId, ProviderSpec, RequestPolicy};

/// The public Grok CLI client id.
///
/// This is a public credential — it ships inside the official Grok Build
/// binary. It is overridable through the configuration so that a future
/// change does not require a release.
pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// The x.ai OAuth issuer.
pub const ISSUER: &str = "https://auth.x.ai";

/// The Grok Build release whose version we advertise.
pub const GROK_BUILD_CLIENT_VERSION: &str = "1.0.77";

/// The minimum Grok Build client version the build proxy accepts.
pub const GROK_BUILD_MINIMUM_VERSION: &str = "1.0.13";

/// The scope set the official Grok CLI requests.
///
/// `offline_access` is what gives us a refresh token, and `api:access` is what
/// lets the token call `api.x.ai`. The conversation and workspace scopes are
/// requested for parity with the official CLI.
pub const SCOPES: &[&str] = &[
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

/// The shorter scope set used when xAI refuses the full set.
pub const FALLBACK_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
];

/// Build the x.ai provider spec from the configuration.
pub fn spec(config: &crate::config::Config) -> ProviderSpec {
    let oauth = &config.oauth.xai;
    let upstream = config.providers.xai.upstream;

    let mut extra_headers = Vec::new();
    if upstream == crate::config::Upstream::Build {
        debug_assert!(
            crate::upstream::version_is_at_least(
                GROK_BUILD_CLIENT_VERSION,
                GROK_BUILD_MINIMUM_VERSION
            ),
            "the advertised Grok Build version is below the floor the proxy enforces"
        );
        extra_headers.push(("X-XAI-Token-Auth", "xai-grok-cli".to_string()));
        extra_headers.push((
            "x-grok-client-version",
            GROK_BUILD_CLIENT_VERSION.to_string(),
        ));
        extra_headers.push(("x-grok-client-identifier", "eggpass".to_string()));
    }

    ProviderSpec {
        id: ProviderId::Xai,
        display_name: "x.ai",
        issuer: oauth.issuer.clone(),
        authorize_path: "/oauth2/authorize".to_string(),
        token_path: "/oauth2/token".to_string(),
        device_path: Some("/oauth2/device/code".to_string()),
        userinfo_path: Some("/oauth2/userinfo".to_string()),
        revocation_path: Some("/oauth2/revoke".to_string()),
        callback_path: "/callback",
        entry_client_id: oauth.client_id.clone(),
        scopes: oauth.scopes.clone(),
        extra_authorize_params: vec![("referrer".to_string(), oauth.referrer.clone())],
        registration_params: Vec::new(),
        extra_token_params: Vec::new(),
        base_url: upstream.origin().to_string(),
        extra_headers,
        policy: RequestPolicy::default(),
        supports_device_flow: true,
        // x.ai has one fixed public client id, so there is no issued id to
        // wait for. That also means a code pasted into the terminal can be
        // exchanged.
        issues_client_id: false,
        trusted_device_hosts: &["accounts.x.ai", "auth.x.ai", "x.ai"],
        catalogue: CatalogueKind::DataArray,
        native_paths: NativePaths::Both,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_sends_requests_to_the_origin() {
        let config = crate::config::Config::default();
        let spec = spec(&config);

        // The base URL has to be the origin: the path carries the `/v1`
        // prefix itself, and a base URL that also ended in `/v1` would send
        // every request to `/v1/v1/...`.
        assert_eq!(spec.base_url, "https://api.x.ai");
        let target = crate::upstream::build_path("/v1/models", "");
        assert_eq!(
            format!("{}{target}", spec.base_url),
            "https://api.x.ai/v1/models"
        );
    }
}
