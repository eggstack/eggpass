//! Streaming responses.
//!
//! When the upstream answers with `text/event-stream` we forward the body
//! chunk by chunk instead of buffering it, so the client sees tokens as they
//! are produced. The server flushes after every chunk.

use std::io::Read;

use super::Response;

/// Forward a streaming response without buffering it.
pub fn stream(
    mut headers: Vec<(String, String)>,
    body: Box<dyn Read + Send + 'static>,
) -> Response {
    // The content type has to be the event stream, whatever the upstream
    // called it.
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
    headers.push((
        "Content-Type".to_string(),
        "text/event-stream; charset=utf-8".to_string(),
    ));
    headers.push(("Cache-Control".to_string(), "no-cache".to_string()));
    headers.push(("X-Accel-Buffering".to_string(), "no".to_string()));

    Response::stream(200, headers, body)
}
