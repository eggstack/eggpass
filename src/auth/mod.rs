//! OAuth 2.1 support for x.ai.
//!
//! The flow is Authorization Code + PKCE against `https://auth.x.ai`, using
//! the public Grok CLI client id. No client secret is involved: the
//! discovery document advertises `token_endpoint_auth_methods_supported =
//! ["none"]`, so PKCE replaces the secret (RFC 8252).

pub mod client;
pub mod flow;
pub mod pkce;
pub mod refresh;
pub mod store;

pub use client::OAuthClient;
pub use refresh::TokenManager;

/// The public Grok CLI client id.
///
/// This is a public credential — it ships inside the official Grok Build
/// binary. It is overridable through the configuration so that a future
/// change does not require a release.
pub const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// The x.ai OAuth issuer.
pub const DEFAULT_ISSUER: &str = "https://auth.x.ai";

/// The identifier we send as the `referrer` parameter.
///
/// xAI tracks this parameter. `grok-build` is the value the official CLI
/// sends; we try our own first and fall back to it if the login is refused.
pub const DEFAULT_REFERRER: &str = "grokproxy";

/// The fallback `referrer` value, used when xAI rejects [`DEFAULT_REFERRER`].
pub const FALLBACK_REFERRER: &str = "grok-build";

/// The scope set the official Grok CLI requests.
///
/// `offline_access` is what gives us a refresh token, and `api:access` is what
/// lets the token call `api.x.ai`. The conversation and workspace scopes are
/// requested for parity with the official CLI.
pub const DEFAULT_SCOPES: &[&str] = &[
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

/// The shorter scope set used by the reference implementation.
///
/// Used as a fallback when xAI refuses the full set (open risk #4).
pub const FALLBACK_SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
];

/// How long we wait for the OAuth callback before giving up.
pub const CALLBACK_TIMEOUT_SECS: u64 = 600;
