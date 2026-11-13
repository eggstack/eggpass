//! Chat completions and the Responses API.
//!
//! Both endpoints are thin: the request goes upstream with the Grok token,
//! and the answer comes back untouched. Streaming responses are passed
//! through chunk by chunk.

use crate::upstream::{self, Method, UpstreamError};

use super::{Request, Response, ServerContext, openai_error, sse};

/// Handle `POST /v1/chat/completions` and `POST /v1/responses`.
pub fn complete(request: &Request, context: &ServerContext) -> Response {
    let (path, query) = upstream::normalize_target(&request.target);
    let forwarded = upstream::sanitize_headers(&request.headers);
    let target = upstream::build_path(&path, &query);

    let mut response = match upstream::send_with_auth(
        &context.upstream,
        &context.tokens,
        Method::Post,
        &target,
        &forwarded,
        Some(&request.body),
    ) {
        Ok(response) => response,
        Err(UpstreamError::Token(err)) => {
            return openai_error(401, &err.to_string(), "invalid_request_error");
        }
        Err(err) => {
            return openai_error(502, &err.to_string(), "server_error");
        }
    };

    let content_type = response
        .header("Content-Type")
        .unwrap_or("application/json")
        .to_string();

    if content_type.starts_with("text/event-stream") {
        return sse::stream(response.body, &content_type);
    }

    let bytes = match response.read_to_vec() {
        Ok(bytes) => bytes,
        Err(err) => {
            return openai_error(
                502,
                &format!("could not read the upstream response: {err}"),
                "server_error",
            );
        }
    };

    Response::bytes(response.status, forwarded_headers(&response.headers), bytes)
}

/// Copy the upstream's headers, minus the ones that describe the hop between
/// us and the upstream.
pub fn forwarded_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    const SKIPPED: &[&str] = &[
        "transfer-encoding",
        "connection",
        "content-length",
        "keep-alive",
        "upgrade",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
    ];

    headers
        .iter()
        .filter(|(name, _)| !SKIPPED.contains(&name.to_ascii_lowercase().as_str()))
        .cloned()
        .collect()
}
