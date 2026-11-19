use std::fmt;

/// Result type returned by the library.
pub type Result<T> = std::result::Result<T, Error>;

/// Return only recognized protocol error codes; never reflect arbitrary provider text.
pub(crate) fn safe_oauth_error_code(value: &str) -> Option<&'static str> {
    const SAFE_CODES: &[&str] = &[
        "invalid_request",
        "unauthorized_client",
        "access_denied",
        "unsupported_response_type",
        "invalid_scope",
        "server_error",
        "temporarily_unavailable",
        "invalid_client",
        "invalid_grant",
        "unsupported_grant_type",
        "invalid_token",
        "insufficient_scope",
        "authorization_pending",
        "slow_down",
        "expired_token",
        "authorization_denied",
        "login_required",
        "interaction_required",
        "account_selection_required",
        "consent_required",
        "invalid_request_uri",
        "invalid_request_object",
        "request_not_supported",
        "request_uri_not_supported",
        "registration_not_supported",
    ];
    SAFE_CODES.iter().copied().find(|code| *code == value)
}

/// Errors returned by the OAuth client and credential manager.
///
/// Response bodies, authorization codes, URLs, access tokens, refresh tokens,
/// device codes, and request URLs are deliberately not included in errors.
#[non_exhaustive]
pub enum Error {
    /// Configuration or a response contained an invalid or missing value.
    InvalidResponse(&'static str),
    /// Provider or request configuration failed validation.
    InvalidConfiguration(&'static str),
    /// The HTTP server returned an unexpected status.
    HttpStatus { status: u16 },
    /// The server returned a standard OAuth error code.
    OAuth { status: u16, code: String },
    /// The provider denied an authorization callback.
    ProviderDenied(String),
    /// Callback origin or exact path did not match the pending transaction.
    CallbackMismatch,
    /// Callback state did not match the issued state.
    StateMismatch,
    /// The ID-token nonce did not match the pending authorization.
    NonceMismatch,
    /// The response body exceeded the configured limit.
    ResponseTooLarge,
    /// The HTTP exchange failed. Eggfetch error details are not exposed because
    /// they can contain request URLs or caller configuration.
    Transport,
    /// A configured request or authorization deadline elapsed.
    Timeout(&'static str),
    /// The provider has no endpoint for the requested operation.
    Unsupported(&'static str),
    /// The device authorization expired before approval.
    DeviceExpired,
    /// Device polling was cancelled by the caller.
    Cancelled,
    /// The verification URL did not match the exact configured host allowlist.
    UntrustedVerificationUrl { host: Option<String> },
    /// No credentials were returned by the caller-owned store.
    NotLoggedIn,
    /// Stored credentials have no refresh token.
    NoRefreshToken,
    /// Caller-owned credential storage failed.
    Storage(StorageError),
}

impl Error {
    /// HTTP status associated with an HTTP or OAuth error, when available.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::HttpStatus { status } | Self::OAuth { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// OAuth error code associated with a provider response, when available.
    pub fn oauth_code(&self) -> Option<&str> {
        match self {
            Self::OAuth { code, .. } | Self::ProviderDenied(code) => Some(code),
            _ => None,
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResponse(reason) => {
                f.debug_tuple("InvalidResponse").field(reason).finish()
            }
            Self::InvalidConfiguration(reason) => {
                f.debug_tuple("InvalidConfiguration").field(reason).finish()
            }
            Self::HttpStatus { status } => f
                .debug_struct("HttpStatus")
                .field("status", status)
                .finish(),
            Self::OAuth { status, code } => f
                .debug_struct("OAuth")
                .field("status", status)
                .field("code", code)
                .finish(),
            Self::ProviderDenied(code) => f.debug_tuple("ProviderDenied").field(code).finish(),
            Self::CallbackMismatch => f.write_str("CallbackMismatch"),
            Self::StateMismatch => f.write_str("StateMismatch"),
            Self::NonceMismatch => f.write_str("NonceMismatch"),
            Self::ResponseTooLarge => f.write_str("ResponseTooLarge"),
            Self::Transport => f.write_str("Transport"),
            Self::Timeout(operation) => f.debug_tuple("Timeout").field(operation).finish(),
            Self::Unsupported(operation) => f.debug_tuple("Unsupported").field(operation).finish(),
            Self::DeviceExpired => f.write_str("DeviceExpired"),
            Self::Cancelled => f.write_str("Cancelled"),
            Self::UntrustedVerificationUrl { host } => f
                .debug_struct("UntrustedVerificationUrl")
                .field("host", host)
                .finish(),
            Self::NotLoggedIn => f.write_str("NotLoggedIn"),
            Self::NoRefreshToken => f.write_str("NoRefreshToken"),
            Self::Storage(_) => f.write_str("Storage(<redacted>)"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResponse(reason) => write!(f, "invalid OAuth response: {reason}"),
            Self::InvalidConfiguration(reason) => {
                write!(f, "invalid OAuth configuration: {reason}")
            }
            Self::HttpStatus { status } => write!(f, "HTTP {status} from OAuth endpoint"),
            Self::OAuth { status, code } => {
                write!(f, "OAuth endpoint returned HTTP {status} ({code})")
            }
            Self::ProviderDenied(code) => write!(f, "provider denied authorization ({code})"),
            Self::CallbackMismatch => {
                f.write_str("callback did not match the pending authorization")
            }
            Self::StateMismatch => {
                f.write_str("callback state did not match the pending authorization")
            }
            Self::NonceMismatch => {
                f.write_str("ID-token nonce did not match the pending authorization")
            }
            Self::ResponseTooLarge => {
                f.write_str("OAuth response exceeded the configured body limit")
            }
            Self::Transport => f.write_str("OAuth HTTP request failed"),
            Self::Timeout(operation) => write!(f, "OAuth {operation} timed out"),
            Self::Unsupported(operation) => write!(f, "provider does not support {operation}"),
            Self::DeviceExpired => f.write_str("device authorization expired"),
            Self::Cancelled => f.write_str("device authorization was cancelled"),
            Self::UntrustedVerificationUrl { host: Some(host) } => {
                write!(f, "untrusted device verification host: {host}")
            }
            Self::UntrustedVerificationUrl { host: None } => {
                f.write_str("device verification URL could not be parsed safely")
            }
            Self::NotLoggedIn => f.write_str("no credentials are stored"),
            Self::NoRefreshToken => f.write_str("stored credentials contain no refresh token"),
            Self::Storage(_) => f.write_str("credential storage failed"),
        }
    }
}

impl std::error::Error for Error {}

/// A safe, caller-provided description of a credential-storage failure.
///
/// Do not put credentials or other secrets in this message. `Debug` always
/// redacts it; `Display` returns it so an application may decide whether to
/// present its own storage diagnostics.
pub struct StorageError(String);

impl StorageError {
    /// Construct a storage error using a message that contains no secrets.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Debug for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StorageError(<redacted>)")
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}
