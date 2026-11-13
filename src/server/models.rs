//! The model catalogue.

use crate::upstream::{self, Method, UpstreamError};

use super::{Request, Response, ServerContext, forwarded_headers, openai_error};

/// Handle `GET /v1/models`.
pub fn list(_request: &Request, context: &ServerContext) -> Response {
    let mut response = match upstream::send_with_auth(
        &context.upstream,
        &context.tokens,
        Method::Get,
        "/v1/models",
        &[],
        None,
    ) {
        Ok(response) => response,
        Err(UpstreamError::Token(err)) => {
            return openai_error(401, &err.to_string(), "invalid_request_error");
        }
        Err(err) => {
            return openai_error(502, &err.to_string(), "server_error");
        }
    };

    if response.status != 200 {
        let bytes = response.read_to_vec().unwrap_or_default();
        return Response::bytes(response.status, forwarded_headers(&response.headers), bytes);
    }

    // Merge in the models we know about, without dropping upstream entries.
    let bytes = response.read_to_vec().unwrap_or_default();
    let merged = upstream::merge_models(&bytes);
    Response::json(200, String::from_utf8_lossy(&merged).to_string())
}

/// Handle `GET /v1/models/{id}`.
pub fn single(_request: &Request, context: &ServerContext, id: &str) -> Response {
    let path = format!("/v1/models/{}", upstream::percent_encode(id));

    let mut response = match upstream::send_with_auth(
        &context.upstream,
        &context.tokens,
        Method::Get,
        &path,
        &[],
        None,
    ) {
        Ok(response) => response,
        Err(UpstreamError::Token(err)) => {
            return openai_error(401, &err.to_string(), "invalid_request_error");
        }
        Err(err) => {
            return openai_error(502, &err.to_string(), "server_error");
        }
    };

    if response.status == 404 {
        // The upstream does not know this model, but it may still be one we
        // advertise. Answer with a minimal model object.
        let body = serde_json::json!({
            "id": id,
            "object": "model",
            "owned_by": "xai",
        });
        return Response::json(200, body.to_string());
    }

    let bytes = response.read_to_vec().unwrap_or_default();
    Response::bytes(response.status, forwarded_headers(&response.headers), bytes)
}
