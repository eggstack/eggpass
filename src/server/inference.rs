//! What the last inference to each provider was answered with.
//!
//! The daemon keeps this in memory and serves it here, so that
//! `eggpass usage` can report it. There is no file behind it: the state is
//! gone when the daemon stops, which is the point — it is a fact about this
//! run, not a history of what the user asked for.
//!
//! The route is unauthenticated, like `/health`. The local command that reads
//! it holds no key material — the keys `eggpass` issues are stored as salted
//! hashes, so nothing on this machine can present one — and the answer is an
//! HTTP status code and nothing else. The authority guard still applies, so a
//! rebound request from a web page is refused with `421`, and a cross-origin
//! answer cannot be read without CORS headers.

use serde_json::json;

use super::{Response, ServerContext};

/// Handle `GET /v1/last-inference`.
pub fn handle(context: &ServerContext) -> Response {
    let mut providers = serde_json::Map::new();
    for (provider, status) in context.inferences.snapshot() {
        providers.insert(provider.as_str().to_string(), json!(status));
    }
    let body = json!({ "providers": providers });
    Response::json(200, body.to_string())
}
