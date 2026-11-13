//! The liveness probe.

use super::Response;

/// Handle `GET /health`.
///
/// This endpoint is never authenticated: it is how `grokproxy status` and
/// `grokproxy croncheck` tell whether the daemon is alive.
pub fn handle() -> Response {
    let body = serde_json::json!({
        "status": "ok",
        "service": "grokproxy",
        "version": env!("CARGO_PKG_VERSION"),
    });
    Response::json(200, body.to_string())
}
