//! OAuth 2.1 support for the providers `eggpass` talks to.
//!
//! The flow is Authorization Code + PKCE. For x.ai the client id is the
//! public Grok CLI one and the discovery document advertises
//! `token_endpoint_auth_methods_supported = ["none"]`, so PKCE replaces the
//! secret (RFC 8252). For ChatGPT the client id is *issued* by OpenAI during
//! registration, and the same rule applies — there is no client secret.
//!
//! Everything provider-specific lives in [`crate::provider`]; this module
//! holds the machinery that is the same for every provider.

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
pub const DEFAULT_CLIENT_ID: &str = crate::provider::xai::CLIENT_ID;

/// The x.ai OAuth issuer.
pub const DEFAULT_ISSUER: &str = crate::provider::xai::ISSUER;

/// The identifier we send as the `referrer` parameter.
///
/// xAI tracks this parameter. `grok-build` is the value the official CLI
/// sends; we try our own first and fall back to it if the login is refused.
pub const DEFAULT_REFERRER: &str = "eggpass";

/// The fallback `referrer` value, used when xAI rejects [`DEFAULT_REFERRER`].
pub const FALLBACK_REFERRER: &str = "grok-build";

/// The shorter scope set used by the reference implementation.
///
/// Used as a fallback when xAI refuses the full set.
pub const FALLBACK_SCOPES: &[&str] = crate::provider::xai::FALLBACK_SCOPES;

/// How long we wait for the OAuth callback before giving up.
pub const CALLBACK_TIMEOUT_SECS: u64 = 600;
