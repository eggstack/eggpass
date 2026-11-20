//! Async OAuth authentication building blocks for Eggfetch applications.
//!
//! This crate accepts a caller-owned [`EggfetchClient`], never creates a Tokio
//! runtime, and does not launch a browser, print to a terminal, open a listener,
//! or read/write credential files. Applications own their runtime, user
//! interaction, callback receiver, and credential storage. Owned API values
//! such as [`OAuthClient`], [`PendingAuthorization`], [`Credentials`],
//! [`TokenSet`], and [`DeviceCode`] are `Send + Sync`. [`CredentialStore`]
//! implementations are also `Send + Sync` and return `Send` futures.
//!
//! # Authorization-code flow
//!
//! ```no_run
//! use eggfetch_core::Client;
//! use eggpass_auth::{OAuthClient, OAuthProviderConfig};
//!
//! # async fn login() -> Result<(), Box<dyn std::error::Error>> {
//! let oauth = OAuthClient::new(Client::new(), OAuthProviderConfig::xai());
//! // The redirect URI must use the provider profile's exact callback path.
//! let pending = oauth.begin_authorization("http://127.0.0.1:43123/callback", None)?;
//! let browser_url = pending.authorization_url();
//! // Present browser_url using the caller's UI, then receive the redirect URL
//! // in a caller-owned loopback listener or another registered callback.
//! let _ = browser_url;
//! # Ok(())
//! # }
//! ```
//!
//! The [`PendingAuthorization`] value keeps the PKCE verifier, state, and nonce
//! private and redacts them in `Debug`. Call [`OAuthClient::complete_authorization`]
//! with the full callback URL to validate origin, exact path, state, and the
//! returned ID-token nonce before credentials are released. Interactive callers
//! that may retry user input can first call
//! [`OAuthClient::validate_authorization_callback`] with a borrowed pending
//! transaction; invalid input then leaves the transaction available.
//!
//! # Device authorization
//!
//! [`OAuthClient::request_device_code`] returns a host-checked verification URI
//! and user code as data. The caller decides how to present them. Polling accepts
//! a [`CancellationToken`] and a caller timeout; the server expiry is honored,
//! and each `slow_down` response adds five seconds to subsequent intervals.
//!
//! # Credential storage and refresh
//!
//! [`Credentials`] is serializable for caller-owned storage and redacts secrets
//! from `Debug`. [`CredentialStore`] and [`TokenManager`] provide an optional
//! async refresh manager. Its single-flight guarantee applies to one manager in
//! one process only. Store implementations should atomically replace credential
//! sets and add cross-process coordination when needed.
//!
//! # Security and transport behavior
//!
//! OAuth requests use explicit finite timeouts and a bounded decoded response
//! size. Redirects and automatic retries are disabled for each request, and
//! caller-configured Eggfetch default authentication is suppressed before the
//! library sets any explicit bearer header. Errors expose status and sanitized
//! OAuth codes but not response bodies, URLs, authorization codes, or tokens.
//! Custom OAuth endpoints must use HTTPS, except for loopback HTTP endpoints
//! used by local tests.

mod authorization;
mod client;
mod error;
mod manager;
mod pkce;
mod provider;
mod token;

pub use client::{ClientOptions, OAuthClient, PendingAuthorization};
pub use eggfetch_core::Client as EggfetchClient;
pub use error::{Error, Result, StorageError};
pub use manager::{CredentialStore, TokenManager};
pub use pkce::{challenge_for, random_verifier};
pub use provider::{
    ClientIdPolicy, OAuthProviderConfig, OAuthProviderConfigBuilder, OPENAI_ENTRY_CLIENT_ID,
    OPENAI_ISSUER, OPENAI_RESOURCE, XAI_CLIENT_ID, XAI_ISSUER,
};
pub use token::{Credentials, DeviceCode, DevicePoll, TokenSet, UserInfo};
pub use tokio_util::sync::CancellationToken;
