//! Native media endpoints that are not inference.
//!
//! A provider declares the media paths it serves in its spec. A path is
//! forwarded verbatim, with the upstream's own status, headers, and body, and
//! is enabled only after a live probe has confirmed it.

use serde_json::Value;

use crate::provider::MediaPath;
use crate::upstream::{self, Method, UpstreamError};

use super::{
    Request, Response, ServerContext, chat::ReleasingStream, openai_error, openai_error_with_code,
};

/// Handle a native media path, or leave an unrelated path to the normal 404.
///
/// Only POST candidates are currently supported; another declared method is
/// refused with 405 rather than being forwarded as POST.
pub fn handle(request: &Request, context: &ServerContext) -> Option<Response> {
    let path = upstream::normalize_path(request.path());
    let declared_anywhere = context.providers.values().any(|handle| {
        handle
            .spec
            .media_paths
            .iter()
            .any(|route| upstream::normalize_path(route.path) == path)
    });
    if !declared_anywhere {
        return None;
    }

    let model = serde_json::from_slice::<Value>(&request.body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(model) = model else {
        return Some(openai_error_with_code(
            400,
            "the body must be a JSON object with a model string",
            "invalid_request_error",
            Some("missing_model"),
        ));
    };

    let provider_id = context.provider_for(&model);
    let Some(handle) = context.providers.get(&provider_id) else {
        return Some(openai_error(
            404,
            &format!("no provider is signed in for model {model}"),
            "invalid_request_error",
        ));
    };

    let route = match match_media_path(handle.spec.media_paths, &path, &request.method) {
        MediaRoute::NotFound => return None,
        MediaRoute::MethodNotAllowed(allowed) => {
            return Some(super::method_not_allowed(&request.method, &allowed));
        }
        MediaRoute::Found(route) if route.method == "POST" => route,
        MediaRoute::Found(_route) => {
            // Only POST candidates are currently supported by send_with_auth's
            // request-body forwarding path. Do not accidentally forward another
            // declared method as POST.
            return Some(super::method_not_allowed(&request.method, "POST"));
        }
    };

    let (_, query) = upstream::normalize_target(&request.target);
    let target = upstream::build_path(&path, &query);
    if !context.take_upstream_slot() {
        return Some(openai_error(
            503,
            "too many requests are in flight; try again in a moment",
            "server_error",
        ));
    }

    let mut response = match upstream::send_with_auth(
        &handle.upstream,
        handle.tokens.as_ref(),
        Method::Post,
        &target,
        &upstream::sanitize_headers(&request.headers),
        Some(&request.body),
    ) {
        Ok(response) => response,
        Err(UpstreamError::Token(err)) => {
            context.release_upstream_slot();
            context.record_inference(provider_id, 401);
            return Some(openai_error(
                401,
                &format!("{}: {err}", handle.spec.display_name),
                "invalid_request_error",
            ));
        }
        Err(err) => {
            context.release_upstream_slot();
            context.record_inference(provider_id, 502);
            return Some(openai_error(502, &err.to_string(), "server_error"));
        }
    };

    let status = response.status;
    if !(200..300).contains(&status) {
        let bytes = match response.read_to_vec() {
            Ok(bytes) => bytes,
            Err(err) => {
                context.release_upstream_slot();
                context.record_inference(provider_id, 502);
                return Some(openai_error(
                    502,
                    &format!("could not read the upstream response: {err}"),
                    "server_error",
                ));
            }
        };
        context.release_upstream_slot();
        let response = super::chat::upstream_error(status, &bytes);
        context.record_inference(provider_id, response.status);
        return Some(response);
    }

    let content_type = response.header("Content-Type").map(str::to_string);
    if !valid_content_type(route.response_types, content_type.as_deref()) {
        context.release_upstream_slot();
        context.record_inference(provider_id, 502);
        let arrived = content_type.as_deref().unwrap_or("missing");
        let expected = if route.response_types.is_empty() {
            "a Content-Type header (no type allowlist configured)".to_string()
        } else {
            route.response_types.join(", ")
        };
        return Some(openai_error_with_code(
            502,
            &format!("unexpected upstream Content-Type {arrived}; expected one of: {expected}"),
            "server_error",
            Some("unexpected_media_content_type"),
        ));
    }

    let headers = super::forwarded_headers(&response.headers);
    let body = ReleasingStream::new(response.body, context.upstream_in_flight.clone());
    context.record_inference(provider_id, status);
    Some(Response::stream(status, headers, Box::new(body)))
}

#[derive(Debug, PartialEq, Eq)]
enum MediaRoute<'a> {
    NotFound,
    MethodNotAllowed(String),
    Found(&'a MediaPath),
}

/// Match a normalized path and the exact method declared by the provider.
fn match_media_path<'a>(media_paths: &'a [MediaPath], path: &str, method: &str) -> MediaRoute<'a> {
    let mut allowed = Vec::new();
    for route in media_paths
        .iter()
        .filter(|route| upstream::normalize_path(route.path) == path)
    {
        if route.method == method {
            return MediaRoute::Found(route);
        }
        if !allowed.contains(&route.method) {
            allowed.push(route.method);
        }
    }

    if allowed.is_empty() {
        MediaRoute::NotFound
    } else {
        MediaRoute::MethodNotAllowed(allowed.join(", "))
    }
}

/// Check the upstream's media type, ignoring optional parameters such as charset.
fn valid_content_type(response_types: &[&str], content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return false;
    };
    if response_types.is_empty() {
        return true;
    }

    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    response_types
        .iter()
        .any(|expected| media_type.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEDIA_PATHS: &[MediaPath] = &[MediaPath {
        method: "POST",
        path: "/v1/images/generations",
        response_types: &["application/json"],
    }];

    #[test]
    fn matching_normalizes_paths_and_requires_the_exact_method() {
        assert_eq!(
            match_media_path(MEDIA_PATHS, "/v1/images/generations", "POST"),
            MediaRoute::Found(&MEDIA_PATHS[0]),
        );
        assert_eq!(
            match_media_path(MEDIA_PATHS, "/v1/images/generations", "GET"),
            MediaRoute::MethodNotAllowed("POST".to_string()),
        );
        assert_eq!(
            match_media_path(MEDIA_PATHS, "/v1/images/edits", "POST"),
            MediaRoute::NotFound,
        );
    }

    #[test]
    fn content_type_allowlist_checks_the_type_before_parameters() {
        assert!(valid_content_type(
            &["application/json"],
            Some("application/json; charset=utf-8"),
        ));
        assert!(!valid_content_type(
            &["application/json"],
            Some("image/png"),
        ));
        assert!(!valid_content_type(&["application/json"], None));
        assert!(valid_content_type(&[], Some("image/png")));
        assert!(!valid_content_type(&[], None));
    }
}
