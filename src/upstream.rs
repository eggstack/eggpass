//! The upstream client.
//!
//! Two upstreams sit behind one interface:
//!
//! | `upstream` | Base URL | Extra headers |
//! | --- | --- | --- |
//! | `"api"` (default) | `https://api.x.ai/v1` | none |
//! | `"build"` | `https://cli-chat-proxy.grok.com/v1` | Grok CLI fingerprint |
//!
//! The `build` proxy refuses requests that do not advertise a Grok CLI client
//! version of at least `1.0.13`, so we always send the current one.

use std::io::Read;
use std::time::Duration;

use crate::auth::refresh::TokenError;
use crate::config::Upstream;

/// The Grok Build release whose version we advertise.
pub const GROK_BUILD_CLIENT_VERSION: &str = "1.0.77";

/// The minimum Grok Build client version the build proxy accepts.
pub const GROK_BUILD_MINIMUM_VERSION: &str = "1.0.13";

/// Models we advertise even when the upstream list has not caught up.
pub const KNOWN_MODELS: &[&str] = &[
    "grok-4.7",
    "grok-4.6",
    "grok-4.5",
    "grok-4.3",
    "grok-4.20-0309-reasoning",
    "grok-4.20-0309-non-reasoning",
    "grok-4.20-multi-agent-0309",
    "grok-build-0.1",
];

/// The HTTP methods we forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Errors raised while talking to the upstream.
#[derive(Debug)]
pub enum UpstreamError {
    /// The request never reached the upstream.
    Network(String),
    /// The upstream could not be reached or the response was malformed.
    Protocol(String),
    /// No usable access token is available.
    Token(TokenError),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamError::Network(message) => write!(f, "upstream request failed: {message}"),
            UpstreamError::Protocol(message) => {
                write!(f, "upstream response was malformed: {message}")
            }
            UpstreamError::Token(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

impl From<TokenError> for UpstreamError {
    fn from(err: TokenError) -> UpstreamError {
        UpstreamError::Token(err)
    }
}

/// Something that can hand out an access token.
pub trait TokenProvider {
    /// A token that is good to use right now.
    fn token(&self) -> Result<String, TokenError>;
    /// A fresh token, after the given one was rejected.
    fn refresh(&self, failed_token: &str) -> Result<String, TokenError>;
}

/// A response from the upstream.
pub struct UpstreamResponse {
    /// The HTTP status code.
    pub status: u16,
    /// The response headers.
    pub headers: Vec<(String, String)>,
    /// The response body, as a stream.
    pub body: Box<dyn Read + Send + 'static>,
}

impl UpstreamResponse {
    /// The value of a header, looked up case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Read the whole body into memory.
    pub fn read_to_vec(&mut self) -> std::io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        self.body.read_to_end(&mut buffer)?;
        Ok(buffer)
    }
}

/// Attach the forwarded headers, the bearer token and the upstream's own
/// headers to a request.
fn add_headers<B>(
    mut request: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
    token: &str,
    extra: &[(&'static str, String)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        if let Ok(header_name) = ureq::http::header::HeaderName::from_bytes(name.as_bytes())
            && let Ok(header_value) = ureq::http::header::HeaderValue::from_str(value)
        {
            request = request.header(header_name, header_value);
        }
    }

    request = request.header("Authorization", format!("Bearer {token}"));
    for (name, value) in extra {
        request = request.header(*name, value.clone());
    }
    request
}

/// A client for one of the two upstreams.
#[derive(Debug, Clone)]
pub struct UpstreamClient {
    kind: Upstream,
    base_url: String,
    agent: ureq::Agent,
}

impl UpstreamClient {
    /// Build a client for the given upstream.
    pub fn new(kind: Upstream) -> UpstreamClient {
        UpstreamClient::with_base_url(kind, kind.origin())
    }

    /// Build a client that talks to a specific origin.
    ///
    /// The request path always carries the `/v1` prefix.
    pub fn with_base_url(kind: Upstream, base_url: &str) -> UpstreamClient {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(1_800)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .user_agent(format!("grokproxy/{}", env!("CARGO_PKG_VERSION")))
            .build();

        UpstreamClient {
            kind,
            base_url: base_url.trim_end_matches('/').to_string(),
            agent: ureq::Agent::new_with_config(config),
        }
    }

    /// Send a request to the upstream.
    pub fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(String, String)],
        body: Option<&[u8]>,
        token: &str,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let url = format!("{}{}", self.base_url, path);

        let extra = self.extra_headers();

        let response = match method {
            Method::Get => add_headers(self.agent.get(&url), headers, token, &extra).call(),
            Method::Post => {
                let body = body.unwrap_or(b"");
                add_headers(self.agent.post(&url), headers, token, &extra).send(body)
            }
        }
        .map_err(|err| match err {
            ureq::Error::StatusCode(status) => {
                UpstreamError::Protocol(format!("upstream returned HTTP {status}"))
            }
            other => UpstreamError::Network(other.to_string()),
        })?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();

        let body = response.into_body().into_reader();

        Ok(UpstreamResponse {
            status,
            headers,
            body: Box::new(body),
        })
    }

    /// Headers that are only sent to the Grok Build proxy.
    fn extra_headers(&self) -> Vec<(&'static str, String)> {
        match self.kind {
            Upstream::Api => Vec::new(),
            Upstream::Build => {
                debug_assert!(
                    version_is_at_least(GROK_BUILD_CLIENT_VERSION, GROK_BUILD_MINIMUM_VERSION),
                    "the advertised Grok Build version is below the floor the proxy enforces"
                );
                vec![
                    ("X-XAI-Token-Auth", "xai-grok-cli".to_string()),
                    (
                        "x-grok-client-version",
                        GROK_BUILD_CLIENT_VERSION.to_string(),
                    ),
                    ("x-grok-client-identifier", "grokproxy".to_string()),
                ]
            }
        }
    }
}

/// Send a request, refreshing the token and retrying once if the upstream says
/// the token is no good.
pub fn send_with_auth(
    upstream: &UpstreamClient,
    tokens: &dyn TokenProvider,
    method: Method,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<UpstreamResponse, UpstreamError> {
    let token = tokens.token()?;
    let mut response = upstream.send(method, path, headers, body, &token)?;

    if response.status != 401 {
        return Ok(response);
    }

    // Drain the failed response so the connection can be reused.
    let _ = response.read_to_vec();

    let refreshed = tokens.refresh(&token)?;
    upstream.send(method, path, headers, body, &refreshed)
}

/// Headers that must never be forwarded to the upstream.
///
/// The client's own credentials live here — they must not reach x.ai.
pub const STRIPPED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "host",
    "content-length",
    "accept-encoding",
    "connection",
    "transfer-encoding",
    "upgrade",
    "forwarded",
    "x-real-ip",
    "cdn-loop",
];

/// Header prefixes that must never be forwarded to the upstream.
pub const STRIPPED_HEADER_PREFIXES: &[&str] = &["x-api-key-", "x-forwarded-", "cf-"];

/// Remove the headers that must not reach the upstream.
pub fn sanitize_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| !is_stripped(name))
        .cloned()
        .collect()
}

/// Whether a header must be stripped.
pub fn is_stripped(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    if STRIPPED_HEADERS.contains(&lowered.as_str()) {
        return true;
    }
    STRIPPED_HEADER_PREFIXES
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
}

/// Normalise a request target so the upstream always sees `/v1/...`.
///
/// Accepts both `/chat/completions` and `/v1/chat/completions`, and drops the
/// `key` query parameter, which is ours rather than the upstream's.
pub fn normalize_target(target: &str) -> (String, String) {
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target, ""),
    };

    let path = normalize_path(path);
    let query = strip_key_parameter(query);
    (path, query)
}

/// Normalise a path so it starts with `/v1`.
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    let trimmed = match trimmed.strip_prefix("v1") {
        // Only a real `/v1` prefix is stripped; `/v1beta` is not one.
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest.trim_start_matches('/'),
        _ => trimmed,
    };
    format!("/v1/{trimmed}")
}

/// Remove the `key` parameter from a query string.
pub fn strip_key_parameter(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default();
            !name.eq_ignore_ascii_case("key")
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Join a normalised path and a query string into an upstream path.
pub fn build_path(path: &str, query: &str) -> String {
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    }
}

/// Whether a dotted version number is at least as new as another.
fn version_is_at_least(version: &str, floor: &str) -> bool {
    fn parts(value: &str) -> Vec<u32> {
        value
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect()
    }
    parts(version) >= parts(floor)
}

/// Percent-encode a value for use in a path segment.
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

/// Merge the models we know about into the upstream's list.
///
/// Upstream entries are never dropped; we only add the ones that are missing.
pub fn merge_models(body: &[u8]) -> Vec<u8> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    let Some(data) = value.get("data").and_then(serde_json::Value::as_array) else {
        return body.to_vec();
    };

    let present: Vec<String> = data
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect();

    let mut merged = data.clone();
    for model in KNOWN_MODELS {
        if present.iter().any(|id| id == model) {
            continue;
        }
        merged.push(serde_json::json!({
            "id": model,
            "object": "model",
            "owned_by": "xai",
        }));
    }

    let mut output = value.clone();
    if let Some(object) = output.as_object_mut() {
        object.insert("data".to_string(), serde_json::Value::Array(merged));
    }
    serde_json::to_vec(&output).unwrap_or_else(|_| body.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_advertised_client_version_is_above_the_floor() {
        // The Grok Build proxy answers 426 below 1.0.13.
        assert!(
            version_is_at_least(GROK_BUILD_CLIENT_VERSION, GROK_BUILD_MINIMUM_VERSION),
            "the advertised version {GROK_BUILD_CLIENT_VERSION} is below the floor {GROK_BUILD_MINIMUM_VERSION}"
        );
        assert!(!version_is_at_least("1.0.12", GROK_BUILD_MINIMUM_VERSION));
        assert!(version_is_at_least("1.0.13", GROK_BUILD_MINIMUM_VERSION));
    }

    #[test]
    fn client_credentials_are_stripped() {
        let headers = vec![
            ("Authorization".to_string(), "Bearer client-key".to_string()),
            ("X-Api-Key".to_string(), "client-key".to_string()),
            ("X-Api-Key-Secret".to_string(), "x".to_string()),
            ("Host".to_string(), "127.0.0.1:8484".to_string()),
            ("CF-Ray".to_string(), "abc".to_string()),
            ("X-Forwarded-For".to_string(), "1.2.3.4".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let kept = sanitize_headers(&headers);
        let names: Vec<String> = kept.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"Content-Type".to_string()), "{names:?}");
        for gone in [
            "Authorization",
            "X-Api-Key",
            "X-Api-Key-Secret",
            "Host",
            "CF-Ray",
            "X-Forwarded-For",
        ] {
            assert!(!names.iter().any(|name| name == gone), "{gone} survived");
        }
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(normalize_path("/chat/completions"), "/v1/chat/completions");
        assert_eq!(
            normalize_path("/v1/chat/completions"),
            "/v1/chat/completions"
        );
        assert_eq!(normalize_path("/v1"), "/v1/");
        assert_eq!(normalize_path("/v1beta/models"), "/v1/v1beta/models");
        assert_eq!(normalize_path("/models"), "/v1/models");
    }

    #[test]
    fn the_key_parameter_is_removed() {
        let (path, query) = normalize_target("/v1/chat/completions?key=secret&stream=true");
        assert_eq!(path, "/v1/chat/completions");
        assert_eq!(query, "stream=true");
        assert!(!query.contains("secret"));
    }

    #[test]
    fn the_key_parameter_is_removed_on_its_own() {
        let (_, query) = normalize_target("/v1/models?key=secret");
        assert_eq!(query, "");
    }

    #[test]
    fn models_are_merged_without_dropping_upstream_entries() {
        let body = br#"{"object":"list","data":[{"id":"grok-4.7","object":"model"}]}"#;
        let merged = merge_models(body);
        let value: serde_json::Value = serde_json::from_slice(&merged).unwrap();
        let ids: Vec<&str> = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids[0], "grok-4.7");
        assert!(ids.contains(&"grok-4.6"), "{ids:?}");
        assert_eq!(ids.len(), KNOWN_MODELS.len());
    }

    #[test]
    fn merging_leaves_an_unparsable_body_alone() {
        let body = b"not json";
        assert_eq!(merge_models(body), body);
    }
}
