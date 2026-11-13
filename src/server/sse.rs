//! Streaming responses.
//!
//! When the upstream answers with `text/event-stream` we forward the body
//! chunk by chunk instead of buffering it, so the client sees tokens as they
//! are produced. The server flushes after every chunk.

use std::io::Read;

use super::Response;

/// Forward a streaming response without buffering it.
pub fn stream(body: Box<dyn Read + Send + 'static>, content_type: &str) -> Response {
    Response::stream(
        200,
        vec![
            ("Content-Type".to_string(), content_type.to_string()),
            ("Cache-Control".to_string(), "no-cache".to_string()),
            ("X-Accel-Buffering".to_string(), "no".to_string()),
        ],
        body,
    )
}
