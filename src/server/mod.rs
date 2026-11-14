//! The local HTTP server.
//!
//! The server binds to loopback and nothing else, by default. It speaks the
//! OpenAI-compatible subset that clients actually use, forwards every request
//! to the provider that serves the requested model, and never lets a client's
//! own credentials reach the provider.

pub mod aggregator;
pub mod chat;
pub mod guard;
pub mod health;
pub mod http;
pub mod models;
pub mod router;
pub mod sse;
pub mod translate;

#[cfg(test)]
mod tests;

pub use chat::forwarded_headers;
pub use http::{HttpServer, Request, Response};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::auth::refresh::TokenManager;
use crate::config::Routing;
use crate::provider::{ProviderId, ProviderSpec};
use crate::upstream::UpstreamClient;

/// The largest number of simultaneous upstream requests.
pub const DEFAULT_MAX_UPSTREAM_REQUESTS: usize = 64;
/// One provider, ready to serve requests.
pub struct ProviderHandle {
    /// What the provider is and how it behaves.
    pub spec: ProviderSpec,
    /// The client the requests go out through.
    pub upstream: UpstreamClient,
    /// Hands out access tokens, refreshing them when needed.
    pub tokens: TokenManager,
}

/// Everything a request handler needs.
pub struct ServerContext {
    /// The address the server is bound to.
    ///
    /// The authority guard uses it, along with `port`, to name the host the
    /// server answers to.
    pub host: String,
    /// The port the server is listening on.
    pub port: u16,
    /// A key clients may present, taken from the configuration file.
    ///
    /// Empty means there is no such key. The keys `eggpass` issued are
    /// accepted as well.
    pub api_key: String,
    /// The keys `eggpass` issued to its clients.
    pub keys: Arc<crate::keys::KeyStore>,
    /// The providers that are available.
    pub providers: HashMap<ProviderId, ProviderHandle>,
    /// The provider used when no prefix matches the model.
    pub default_provider: ProviderId,
    /// The routing table.
    pub routing: Routing,
    /// The number of upstream requests in flight.
    ///
    /// This is shared, because a streaming request holds its slot until the
    /// stream has finished, which is after the handler has returned.
    pub upstream_in_flight: Arc<AtomicUsize>,
    /// The largest number of simultaneous upstream requests.
    pub max_upstream_requests: usize,
}

impl ServerContext {
    /// The provider that serves a model.
    pub fn provider_for(&self, model: &str) -> ProviderId {
        self.routing.provider_for(model, self.default_provider)
    }

    /// Take an upstream slot, or refuse when there are too many requests.
    pub fn take_upstream_slot(&self) -> bool {
        let mut current = self.upstream_in_flight.load(Ordering::SeqCst);
        loop {
            if current >= self.max_upstream_requests {
                return false;
            }
            match self.upstream_in_flight.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Give an upstream slot back.
    pub fn release_upstream_slot(&self) {
        self.upstream_in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Errors raised while starting the server.
#[derive(Debug)]
pub enum ServerError {
    /// The address could not be bound.
    Bind {
        /// The address that was tried.
        address: String,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Bind { address, source } => {
                write!(f, "could not bind {address}: {source}")
            }
        }
    }
}

impl std::error::Error for ServerError {}

/// The running server.
pub struct Server {
    http: Arc<HttpServer>,
    shutdown: Arc<AtomicBool>,
}

impl Server {
    /// Bind the server to an address, with explicit limits.
    pub fn bind_with_limits(
        host: &str,
        port: u16,
        limits: crate::config::ServerLimits,
    ) -> Result<Arc<Server>, ServerError> {
        let address = format!("{host}:{port}");
        let http = HttpServer::bind(&address)
            .map_err(|source| ServerError::Bind {
                address: address.clone(),
                source,
            })?
            .with_limits(
                limits.max_connections,
                Duration::from_secs(limits.idle_timeout_seconds),
                Duration::from_secs(limits.write_timeout_seconds),
            );

        Ok(Arc::new(Server {
            http: Arc::new(http),
            shutdown: Arc::new(AtomicBool::new(false)),
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

        // The gate sees the request head before the body is read, so an
        // unauthenticated client never gets its body buffered. The handler
        // checks again: the gate is an early refusal, not the authority.
        let gate = {
            let context = Arc::clone(&context);
            Arc::new(move |request: &Request| pre_authorize(request, &context)) as http::Gate
        };

        let handler = {
            let context = Arc::clone(&context);
            Arc::new(move |request: Request| handle_request(request, &context)) as http::Handler
        };

        // The server waits for the in-flight connections itself, so a stream
        // that is halfway out when the signal arrives still finishes.
        Arc::clone(&self.http).serve(self.shutdown.clone(), gate, handler);
    }
}

/// Decide whether a request may proceed, from its head and the context.
///
/// Returns the refusal when it may not. This runs twice: once as the HTTP
/// server's gate, before the body is read, and once at the top of
/// [`handle_request`]. Both call this function, so the two checks cannot
/// drift apart.
fn pre_authorize(request: &Request, context: &ServerContext) -> Option<Response> {
    if let Some(refusal) = guard::check(request, &context.host, context.port) {
        return Some(refusal);
    }

    // The health probe is never authenticated: `status` and `croncheck` use
    // it, and it carries nothing but a version string.
    if matches!(router::route(request.path()), router::Route::Health) {
        return None;
    }

    // A server with no key at all refuses everything. `eggpass run` will not
    // start in this state, but the keys can be revoked while it runs.
    if !keys_configured(context) {
        return Some(openai_error_with_code(
            401,
            "no client key is configured on this server; run `eggpass key add`",
            "invalid_request_error",
            Some("no_api_key"),
        ));
    }

    if !is_authorized(context, request) {
        return Some(openai_error(
            401,
            "missing or invalid API key",
            "invalid_request_error",
        ));
    }

    None
}

/// Route a request and produce a response.
fn handle_request(request: Request, context: &ServerContext) -> Response {
    if let Some(refusal) = pre_authorize(&request, context) {
        return refusal;
    }

    let path = request.path().to_string();
    let route = router::route(&path);

    match (request.method.as_str(), route) {
        ("GET", router::Route::Health) => health::handle(),
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

/// Whether any client key is configured at all.
pub fn keys_configured(context: &ServerContext) -> bool {
    !context.api_key.is_empty() || !context.keys.is_empty()
}

/// Whether the request carries a key we accept.
///
/// A key may arrive as `Authorization: Bearer`, as `X-Api-Key`, or as a `key`
/// query parameter, and may be one of the keys `eggpass` issued or the one
/// written in the configuration file.
///
/// A server with no key configured accepts nothing. The loopback exception
/// that used to live here was not a boundary — a web page can reach loopback,
/// so "open on loopback" meant "open to any page the user visits".
pub fn is_authorized(context: &ServerContext, request: &Request) -> bool {
    if !keys_configured(context) {
        return false;
    }

    for candidate in presented_keys(request) {
        if !context.api_key.is_empty()
            && crate::keys::constant_time_eq(candidate.as_bytes(), context.api_key.as_bytes())
        {
            return true;
        }
        if context.keys.verify(&candidate) {
            return true;
        }
    }

    false
}

/// The keys a request presents.
///
/// A request may present more than one — a header and a query parameter, say
/// — and any one of them being right is enough.
fn presented_keys(request: &Request) -> Vec<String> {
    let mut candidates = Vec::new();

    for (name, value) in &request.headers {
        let lowered = name.to_ascii_lowercase();
        let value = value.trim();
        match lowered.as_str() {
            "authorization" => {
                if let Some(token) = strip_bearer(value) {
                    let token = token.trim();
                    if !token.is_empty() {
                        candidates.push(token.to_string());
                    }
                }
            }
            "x-api-key" if !value.is_empty() => {
                candidates.push(value.to_string());
            }
            _ => {}
        }
    }

    for pair in request.query().split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name.eq_ignore_ascii_case("key") {
            let value = value.trim();
            if !value.is_empty() {
                candidates.push(value.to_string());
            }
        }
    }

    candidates
}

/// The token of a `Bearer` authorization header.
fn strip_bearer(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token)
    } else {
        None
    }
}

/// An error in the OpenAI error shape.
pub fn openai_error(status: u16, message: &str, kind: &str) -> Response {
    openai_error_with_code(status, message, kind, None)
}

/// An error in the OpenAI error shape, carrying the provider's own code.
///
/// The code is what tells a client what actually went wrong — `model_not_found`
/// says one thing and `subscription_sharing_usage_limit_exceeded` says another
/// — so the upstream's code is passed through rather than flattened away.
pub fn openai_error_with_code(
    status: u16,
    message: &str,
    kind: &str,
    code: Option<&str>,
) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": kind,
            "code": code,
        }
    });
    Response::json(status, body.to_string())
}

#[cfg(test)]
mod authorisation_tests {
    use super::*;

    fn empty_store() -> Arc<crate::keys::KeyStore> {
        let directory = tempfile::tempdir().expect("temp dir");
        // The directory is kept alive by the store's own path, and the file is
        // only read, so dropping the directory handle here is harmless.
        let path = directory.keep().join("keys.json");
        Arc::new(crate::keys::KeyStore::open(path).expect("key store"))
    }

    fn context_with_key(key: &str) -> ServerContext {
        ServerContext {
            host: "127.0.0.1".to_string(),
            port: 8484,
            api_key: key.to_string(),
            keys: empty_store(),
            providers: HashMap::new(),
            default_provider: ProviderId::Xai,
            routing: Routing::default(),
            upstream_in_flight: Arc::new(AtomicUsize::new(0)),
            max_upstream_requests: DEFAULT_MAX_UPSTREAM_REQUESTS,
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

    fn health_request() -> Request {
        Request {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn a_server_with_no_key_refuses_everything() {
        let context = context_with_key("");

        assert!(!is_authorized(&context, &request_with(vec![], "")));
        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer anything")], "")
        ));

        let refusal = pre_authorize(&request_with(vec![], ""), &context).expect("refused");
        assert_eq!(refusal.status, 401);
    }

    #[test]
    fn the_health_probe_needs_no_key() {
        let context = context_with_key("");
        assert!(pre_authorize(&health_request(), &context).is_none());
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

    #[test]
    fn an_issued_key_is_accepted() {
        let context = context_with_key("");
        let (_, plain) = context.keys.add(None).expect("key");

        assert!(is_authorized(
            &context,
            &request_with(vec![("Authorization", &format!("Bearer {plain}"))], "")
        ));
        assert!(is_authorized(
            &context,
            &request_with(vec![("X-Api-Key", &plain)], "")
        ));
        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer sk-eggpass-nope")], "")
        ));
    }

    #[test]
    fn a_revoked_key_stops_working() {
        let context = context_with_key("");
        let (_, first) = context.keys.add(None).expect("key");
        let (_, second) = context.keys.add(None).expect("key");

        context.keys.revoke(1).expect("revoke");

        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", &format!("Bearer {first}"))], "")
        ));
        assert!(is_authorized(
            &context,
            &request_with(vec![("Authorization", &format!("Bearer {second}"))], "")
        ));
    }

    #[test]
    fn revoking_the_last_key_closes_the_endpoint() {
        let context = context_with_key("");
        let (_, plain) = context.keys.add(None).expect("key");
        assert!(is_authorized(
            &context,
            &request_with(vec![("Authorization", &format!("Bearer {plain}"))], "")
        ));

        context.keys.revoke(1).expect("revoke");

        // Nothing is configured now, so the running server refuses every
        // request rather than reopening the loopback socket.
        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", &format!("Bearer {plain}"))], "")
        ));
        let refusal = pre_authorize(&request_with(vec![], ""), &context).expect("refused");
        assert_eq!(refusal.status, 401);
    }

    #[test]
    fn a_foreign_host_is_refused_before_the_key_is_considered() {
        let context = context_with_key("secret");
        let request = request_with(vec![("Host", "evil.example:8484")], "");

        let refusal = pre_authorize(&request, &context).expect("refused");
        assert_eq!(refusal.status, 421);
    }

    #[test]
    fn a_loopback_host_is_accepted() {
        let context = context_with_key("secret");
        let request = request_with(
            vec![
                ("Host", "127.0.0.1:8484"),
                ("Authorization", "Bearer secret"),
            ],
            "",
        );

        assert!(pre_authorize(&request, &context).is_none());
    }

    #[test]
    fn a_server_off_loopback_without_a_key_refuses_everything() {
        let mut context = context_with_key("");
        context.host = "0.0.0.0".to_string();

        assert!(!is_authorized(&context, &request_with(vec![], "")));
        assert!(!is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer anything")], "")
        ));
    }

    #[test]
    fn a_server_off_loopback_with_a_key_serves() {
        let mut context = context_with_key("secret");
        context.host = "0.0.0.0".to_string();

        assert!(is_authorized(
            &context,
            &request_with(vec![("Authorization", "Bearer secret")], "")
        ));
        assert!(!is_authorized(&context, &request_with(vec![], "")));
    }

    #[test]
    fn the_upstream_slots_are_bounded() {
        let context = ServerContext {
            host: "127.0.0.1".to_string(),
            port: 8484,
            api_key: String::new(),
            keys: empty_store(),
            providers: HashMap::new(),
            default_provider: ProviderId::Xai,
            routing: Routing::default(),
            upstream_in_flight: Arc::new(AtomicUsize::new(0)),
            max_upstream_requests: 2,
        };

        assert!(context.take_upstream_slot());
        assert!(context.take_upstream_slot());
        assert!(!context.take_upstream_slot());
        context.release_upstream_slot();
        assert!(context.take_upstream_slot());
    }
}
