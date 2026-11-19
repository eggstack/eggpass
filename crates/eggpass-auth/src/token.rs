use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// OAuth tokens returned by an authorization-code, device, or refresh grant.
///
/// Secret fields are private and `Debug` redacts their values. Use the accessors
/// to pass tokens to the caller's storage or authenticated API client.
#[derive(Clone, Default)]
pub struct TokenSet {
    pub(crate) access_token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) id_token: Option<String>,
    pub(crate) token_type: String,
    pub(crate) scope: Option<String>,
    pub(crate) expires_at: Option<i64>,
}

impl TokenSet {
    pub(crate) fn from_json(value: &Value, now: i64) -> Result<Self> {
        let access_token = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(Error::InvalidResponse("token response has no access_token"))?
            .to_string();
        let refresh_token = optional_string(value, "refresh_token");
        let id_token = optional_string(value, "id_token");
        let token_type = value
            .get("token_type")
            .and_then(Value::as_str)
            .filter(|token_type| !token_type.is_empty())
            .unwrap_or("Bearer")
            .to_string();
        let scope = optional_string(value, "scope");

        let expires_at = if let Some(expiry) = value.get("expires_in") {
            let seconds = expiry.as_i64().ok_or(Error::InvalidResponse(
                "token response expires_in is not an integer",
            ))?;
            if seconds < 0 {
                return Err(Error::InvalidResponse(
                    "token response expires_in is negative",
                ));
            }
            Some(now.checked_add(seconds).ok_or(Error::InvalidResponse(
                "token response expiry is out of range",
            ))?)
        } else {
            match value.get("expires_at") {
                Some(Value::Number(number)) => number.as_i64(),
                Some(Value::String(timestamp)) => parse_rfc3339_to_unix(timestamp),
                _ => None,
            }
        };

        Ok(Self {
            access_token,
            refresh_token,
            id_token,
            token_type,
            scope,
            expires_at,
        })
    }

    /// Bearer access token.
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Refresh token, when the provider returned one.
    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    /// OpenID Connect ID token, when the provider returned one.
    pub fn id_token(&self) -> Option<&str> {
        self.id_token.as_deref()
    }

    /// Token type, normally `Bearer`.
    pub fn token_type(&self) -> &str {
        &self.token_type
    }

    /// Granted scope string, when returned by the provider.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Absolute expiry in Unix seconds, when returned or derived from
    /// `expires_in`.
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }
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

/// Credentials to keep in caller-owned storage.
///
/// The type is serializable for storage adapters, but its `Debug` output always
/// redacts token values. The library does not write it to disk or assume a
/// filesystem location.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    #[serde(default)]
    pub(crate) access_token: String,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) id_token: Option<String>,
    #[serde(default = "default_token_type")]
    pub(crate) token_type: String,
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[serde(default)]
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) issuer: String,
    #[serde(default)]
    pub(crate) expires_at: Option<i64>,
    #[serde(default)]
    pub(crate) obtained_at: i64,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl Credentials {
    pub(crate) fn from_tokens(
        tokens: TokenSet,
        client_id: String,
        issuer: String,
        obtained_at: i64,
    ) -> Self {
        Self {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            id_token: tokens.id_token,
            token_type: if tokens.token_type.is_empty() {
                default_token_type()
            } else {
                tokens.token_type
            },
            scope: tokens.scope,
            client_id,
            issuer,
            expires_at: tokens.expires_at,
            obtained_at,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.access_token.is_empty() {
            return Err(Error::InvalidResponse(
                "stored credentials have no access token",
            ));
        }
        if self.client_id.is_empty() {
            return Err(Error::InvalidResponse(
                "stored credentials have no client ID",
            ));
        }
        Ok(())
    }

    pub(crate) fn merged_refresh(&self, tokens: TokenSet, now: i64) -> Self {
        Self {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token.or_else(|| self.refresh_token.clone()),
            id_token: tokens.id_token.or_else(|| self.id_token.clone()),
            token_type: if tokens.token_type.is_empty() {
                self.token_type.clone()
            } else {
                tokens.token_type
            },
            scope: tokens.scope.or_else(|| self.scope.clone()),
            client_id: self.client_id.clone(),
            issuer: if self.issuer.is_empty() {
                String::new()
            } else {
                self.issuer.clone()
            },
            expires_at: tokens.expires_at,
            obtained_at: now,
        }
    }

    /// Access token for authenticated requests.
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// Refresh token, when available.
    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    /// ID token, when available.
    pub fn id_token(&self) -> Option<&str> {
        self.id_token.as_deref()
    }

    /// Token type, normally `Bearer`.
    pub fn token_type(&self) -> &str {
        &self.token_type
    }

    /// Granted scopes.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// The client ID to reuse for refresh and future authorizations.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Issuer associated with these credentials.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Absolute expiry in Unix seconds, when known.
    pub fn expires_at(&self) -> Option<i64> {
        self.expires_at
    }

    /// Unix time when these credentials were obtained.
    pub fn obtained_at(&self) -> i64 {
        self.obtained_at
    }

    /// Whether the access token remains fresh after the requested refresh skew.
    pub fn is_fresh(&self, now: i64, skew_seconds: i64) -> bool {
        let fallback_expiry = self.obtained_at.saturating_add(30 * 86_400);
        now.saturating_add(skew_seconds.max(0)) < self.expires_at.unwrap_or(fallback_expiry)
    }

    /// Unix seconds since the system epoch. Returns zero if the system clock is
    /// before the Unix epoch.
    pub fn now_unix() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or_default()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("client_id", &"<redacted>")
            .field("issuer", &self.issuer)
            .field("expires_at", &self.expires_at)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

fn parse_rfc3339_to_unix(value: &str) -> Option<i64> {
    let (local, offset_seconds) =
        if let Some(local) = value.strip_suffix('Z').or_else(|| value.strip_suffix('z')) {
            (local, 0)
        } else {
            let offset_index = value
                .char_indices()
                .skip(10)
                .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index))?;
            let (local, offset) = value.split_at(offset_index);
            (local, parse_timezone_offset(offset)?)
        };
    let (date, time) = local.split_once('T').or_else(|| local.split_once('t'))?;
    if date.len() != 10 {
        return None;
    }
    let mut date_parts = date.split('-');
    let year = parse_decimal(date_parts.next()?, 4)?;
    let month = parse_decimal(date_parts.next()?, 2)?;
    let day = parse_decimal(date_parts.next()?, 2)?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }

    let (time, fraction) = match time.split_once('.') {
        Some((time, fraction)) => {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            (time, Some(fraction))
        }
        None => (time, None),
    };
    let _ = fraction;
    if time.len() != 8
        || time.as_bytes().get(2) != Some(&b':')
        || time.as_bytes().get(5) != Some(&b':')
    {
        return None;
    }
    let hour = parse_decimal(&time[0..2], 2)?;
    let minute = parse_decimal(&time[3..5], 2)?;
    let second = parse_decimal(&time[6..8], 2)?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days.checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?
        .checked_sub(offset_seconds)
}

fn parse_timezone_offset(value: &str) -> Option<i64> {
    let (sign, offset) = match value.as_bytes().first()? {
        b'+' => (1, &value[1..]),
        b'-' => (-1, &value[1..]),
        _ => return None,
    };
    if offset.len() != 5 || offset.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let hours = parse_decimal(&offset[0..2], 2)?;
    let minutes = parse_decimal(&offset[3..5], 2)?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_decimal(value: &str, digits: usize) -> Option<i64> {
    if value.len() != digits || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Non-secret account details returned by an OpenID user-info endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserInfo {
    /// Provider account identifier, when available.
    pub user_id: Option<String>,
    /// Account email, when available.
    pub email: Option<String>,
}

/// Data returned by a device authorization endpoint.
#[derive(Clone)]
pub struct DeviceCode {
    pub(crate) device_code: String,
    pub(crate) user_code: String,
    pub(crate) verification_uri: String,
    pub(crate) verification_uri_complete: Option<String>,
    pub(crate) expires_in: std::time::Duration,
    pub(crate) expires_at: tokio::time::Instant,
    pub(crate) interval: std::time::Duration,
}

impl DeviceCode {
    /// Secret device code used only for token polling.
    pub fn device_code(&self) -> &str {
        &self.device_code
    }

    /// Short code the user enters at the verification page.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// Verification page URL.
    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    /// Verification page URL with the user code pre-filled, when provided.
    pub fn verification_uri_complete(&self) -> Option<&str> {
        self.verification_uri_complete.as_deref()
    }

    /// Server-provided authorization lifetime. The library does not increase
    /// short values to a local minimum.
    pub fn expires_in(&self) -> std::time::Duration {
        self.expires_in
    }

    /// Initial server-provided polling interval.
    pub fn interval(&self) -> std::time::Duration {
        self.interval
    }
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCode")
            .field("device_code", &"<redacted>")
            .field("user_code", &"<redacted>")
            .field("verification_uri", &"<redacted URL>")
            .field(
                "verification_uri_complete",
                &self
                    .verification_uri_complete
                    .as_ref()
                    .map(|_| "<redacted URL>"),
            )
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

/// Result of one device-code poll.
#[derive(Debug)]
#[non_exhaustive]
pub enum DevicePoll {
    /// The user has not approved the request yet.
    Pending,
    /// The server asked the client to increase its interval by five seconds.
    SlowDown,
    /// The user denied the request.
    Denied,
    /// The device grant expired.
    Expired,
    /// Authorization succeeded.
    Complete(TokenSet),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_response_parses_rfc3339_expiry_and_numeric_expiry() {
        let rfc3339 = TokenSet::from_json(
            &serde_json::json!({
                "access_token": "access",
                "expires_at": "2026-12-13T05:12:18Z"
            }),
            0,
        )
        .unwrap();
        assert_eq!(rfc3339.expires_at(), Some(1_797_138_738));

        let offset = TokenSet::from_json(
            &serde_json::json!({
                "access_token": "access",
                "expires_at": "1970-01-01T01:00:00+01:00"
            }),
            0,
        )
        .unwrap();
        assert_eq!(offset.expires_at(), Some(0));

        let numeric = TokenSet::from_json(
            &serde_json::json!({"access_token": "access", "expires_at": 1234}),
            0,
        )
        .unwrap();
        assert_eq!(numeric.expires_at(), Some(1234));
    }

    #[test]
    fn credential_freshness_respects_the_requested_refresh_skew() {
        let credentials = Credentials {
            expires_at: Some(1_000),
            obtained_at: 0,
            ..Credentials::default()
        };
        assert!(credentials.is_fresh(699, 300));
        assert!(!credentials.is_fresh(700, 300));
    }

    #[test]
    fn malformed_rfc3339_expiry_is_ignored_without_panicking() {
        let tokens = TokenSet::from_json(
            &serde_json::json!({
                "access_token": "access",
                "expires_at": "2026-99-99T25:61:61Z"
            }),
            0,
        )
        .unwrap();
        assert_eq!(tokens.expires_at(), None);
    }
}
