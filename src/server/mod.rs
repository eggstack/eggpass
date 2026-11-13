//! The local HTTP server.
//!
//! The server binds to `127.0.0.1` and nothing else. It speaks the
//! OpenAI-compatible subset that clients actually use, forwards everything to
//! the configured upstream with the Grok OAuth token, and never lets a
//! client's own credentials reach x.ai.

pub mod chat;
pub mod health;
pub mod http;
pub mod models;
pub mod router;
pub mod sse;

#[cfg(test)]
mod tests;

pub use chat::forwarded_headers;
pub use http::{HttpServer, Request, Response};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::auth::refresh::TokenManager;
use crate::upstream::UpstreamClient;

/// How long in-flight requests are given to finish during shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Everything a request handler needs.
#[derive(Debug)]
pub struct ServerContext {
    /// The key clients must present. Empty means "accept anything".
    pub api_key: String,
    /// The upstream requests are forwarded to.
    pub upstream: UpstreamClient,
    /// Hands out access tokens, refreshing them when needed.
    pub tokens: TokenManager,
}

/// Errors raised while starting the server.
#[derive(Debug)]
pub enum ServerError {
    /// The port could not be bound.
    Bind { port: u16, source: std::io::Error },
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Bind { port, source } => {
                write!(f, "could not bind 127.0.0.1:{port}: {source}")
            }
        }
    }
}

impl std::error::Error for ServerError {}

/// The running server.
pub struct Server {
    http: Arc<HttpServer>,
    shutdown: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
}

impl Server {
    /// Bind the server to the loopback interface.
    pub fn bind(port: u16) -> Result<Arc<Server>, ServerError> {
        let http =
            HttpServer::bind(&format!("127.0.0.1:{port}")).map_err(|source| ServerError::Bind {
                port,
                source: std::io::Error::other(source.to_string()),
            })?;

        Ok(Arc::new(Server {
            http: Arc::new(http),
            shutdown: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }))
    }

    /// The address the server is listening on.
    pub fn address(&self) -> std::net::SocketAddr {
        self.http
            .local_addr()
            .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// Serve requests until the server is asked to stop.
    pub fn serve(self: Arc<Self>, context: Arc<ServerContext>) {
        // A SIGTERM asks for a graceful shutdown: stop accepting, let the
        // in-flight requests finish, then exit.
        let flag = self.shutdown.clone();
        let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, flag.clone());
        let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, flag.clone());

        let handler = {
            let server = Arc::clone(&self);
            let context = Arc::clone(&context);
            Arc::new(move |request: Request| {
                server.in_flight.fetch_add(1, Ordering::SeqCst);
                let response = handle_request(request, &context);
                server.in_flight.fetch_sub(1, Ordering::SeqCst);
                response
            }) as Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>
        };

        Arc::clone(&self.http).serve(self.shutdown.clone(), handler);

        // Let in-flight requests finish, but not forever.
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while self.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Route a request and produce a response.
fn handle_request(request: Request, context: &ServerContext) -> Response {
    let path = request.path().to_string();
    let route = router::route(&path);

    // The health probe is never authenticated.
    if matches!(route, router::Route::Health) {
        return health::handle();
    }

    if !is_authorized(context, &request) {
        return openai_error(401, "missing or invalid API key", "invalid_request_error");
    }

    match (request.method.as_str(), route) {
        ("GET", router::Route::Models) => models::list(&request, context),
        ("GET", router::Route::Model(id)) => models::single(&request, context, &id),
        ("POST", router::Route::ChatCompletions) | ("POST", router::Route::Responses) => {
            chat::complete(&request, context)
        }
        _ => openai_error(
            404,
            &format!("unknown endpoint `{path}`"),
            "invalid_request_error",
        ),
    }
}

/// Whether the request carries the configured API key.
///
/// The key may arrive as `Authorization: Bearer`, as `X-Api-Key`, or as a
/// `key` query parameter. When no key is configured, everything is allowed —
/// the server only ever binds to loopback.
pub fn is_authorized(context: &ServerContext, request: &Request) -> bool {
    if context.api_key.is_empty() {
        return true;
    }

    for (name, value) in &request.headers {
        let lowered = name.to_ascii_lowercase();
        let value = value.trim();
        match lowered.as_str() {
            "authorization" => {
                let token = value
                    .strip_prefix("Bearer ")
                    .or_else(|| value.strip_prefix("bearer "));
                if let Some(token) = token
                    && constant_time_eq(token.trim(), &context.api_key)
                {
                    return true;
                }
            }
            "x-api-key" => {
                if constant_time_eq(value, &context.api_key) {
                    return true;
                }
            }
            _ => {}
        }
    }

    for pair in request.query().split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name.eq_ignore_ascii_case("key") && constant_time_eq(value, &context.api_key) {
            return true;
        }
    }

    false
}

/// Compare two secrets without leaking their length through timing.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// An error in the OpenAI error shape.
pub fn openai_error(status: u16, message: &str, kind: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": kind,
            "code": serde_json::Value::Null,
        }
    });
    Response::json(status, body.to_string())
}

#[cfg(test)]
mod authorisation_tests {
    use super::*;

    fn context_with_key(key: &str) -> ServerContext {
        ServerContext {
            api_key: key.to_string(),
            upstream: UpstreamClient::new(crate::config::Upstream::Api),
            tokens: TokenManager::new(
                crate::paths::Paths::resolve(None).unwrap(),
                crate::auth::OAuthClient::new(
                    crate::auth::DEFAULT_CLIENT_ID,
                    crate::auth::DEFAULT_ISSUER,
                    crate::auth::DEFAULT_SCOPES,
                    crate::auth::DEFAULT_REFERRER,
                ),
            ),
        }
    }

    fn request_with(headers: Vec<(&str, &str)>, query: &str) -> Request {
        Request {
            method: "GET".to_string(),
            target: format!("/v1/models?{query}"),
            version: "HTTP/1.1".to_string(),
            headers: headers
                .into_iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    #[test]
    fn an_empty_key_accepts_everything() {
        let context = context_with_key("");
        assert!(is_authorized(&context, &request_with(vec![], "")));
    }

    #[test]
    fn the_key_is_accepted_in_any_of_the_three_places() {
        let context = context_with_key("secret");
        assert!(is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer secret")], "")
        ));
        assert!(is_authorized(
            &context,
            &request_with(vec![("X-Api-Key", "secret")], "")
        ));
        assert!(is_authorized(&context, &request_with(vec![], "key=secret")));
    }

    #[test]
    fn a_wrong_key_is_rejected() {
        let context = context_with_key("secret");
        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer wrong")], "")
        ));
        assert!(!is_authorized(&context, &request_with(vec![], "")));
    }
}
