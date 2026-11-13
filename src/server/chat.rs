//! Chat completions and the Responses API.
//!
//! A request is routed to the provider that serves the model it names, has
//! the provider's request policy applied, and is forwarded upstream. The
//! answer comes back the same way, with two adaptations:
//!
//! * A provider that requires streaming gets `stream: true` whether the
//!   client asked for a stream or not, so a non-streaming client request is
//!   answered by collapsing the upstream event stream into one body.
//! * A provider that only speaks the Responses API gets a Chat Completions
//!   request translated on the way in, and its answer translated back on the
//!   way out.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use serde_json::{Value, json};

use crate::provider::NativePaths;
use crate::upstream::{self, Method, UpstreamError};

use super::{
    Request, Response, ServerContext, aggregator,
    aggregator::Aggregated,
    openai_error, sse,
    translate::{self, ResponsesToChatStream},
};

/// Handle `POST /v1/chat/completions` and `POST /v1/responses`.
pub fn complete(request: &Request, context: &ServerContext) -> Response {
    let model = extract_model(&request.body);

    // Which provider serves this model?
    let provider_id = context.provider_for(&model);
    let Some(handle) = context.providers.get(&provider_id) else {
        return openai_error(
            404,
            &format!("no provider is signed in for model `{model}`"),
            "invalid_request_error",
        );
    };

    let (path, query) = upstream::normalize_target(&request.target);
    let forwarded = upstream::sanitize_headers(&request.headers);

    // Does the provider serve this path natively?
    let native = handle.spec.native_paths == NativePaths::Both;
    let upstream_path = if native {
        path.clone()
    } else {
        "/v1/responses".to_string()
    };

    // Did the client want a stream?
    let wants_stream = body_wants_stream(&request.body);

    // The body, translated when the provider does not speak this shape.
    let body = if native {
        request.body.clone()
    } else {
        match translate::chat_request_to_responses(&request.body) {
            Ok(body) => body,
            Err(err) => {
                return openai_error(400, &err, "invalid_request_error");
            }
        }
    };

    // The provider's own rules about the body.
    let body = apply_policy(&body, &handle.spec.policy);

    let target = upstream::build_path(&upstream_path, &query);

    if !context.take_upstream_slot() {
        return openai_error(
            503,
            "too many requests are in flight; try again in a moment",
            "server_error",
        );
    }

    let mut response = match upstream::send_with_auth(
        &handle.upstream,
        &handle.tokens,
        Method::Post,
        &target,
        &forwarded,
        Some(&body),
    ) {
        Ok(response) => response,
        Err(UpstreamError::Token(err)) => {
            context.release_upstream_slot();
            return openai_error(
                401,
                &format!("{}: {err}", handle.spec.display_name),
                "invalid_request_error",
            );
        }
        Err(err) => {
            context.release_upstream_slot();
            return openai_error(502, &err.to_string(), "server_error");
        }
    };

    let content_type = response
        .header("Content-Type")
        .unwrap_or("application/json")
        .to_string();
    let headers = super::forwarded_headers(&response.headers);

    if content_type.starts_with("text/event-stream") {
        // The upstream is streaming.
        let body = response.body;

        if wants_stream {
            // The client wants the events as they arrive. The slot is held
            // until the stream ends, because a request that is still
            // streaming is still in flight.
            let inner: Box<dyn Read + Send + 'static> = if native {
                body
            } else {
                Box::new(ResponsesToChatStream::new(body))
            };
            let stream = ReleasingStream::new(inner, context.upstream_in_flight.clone());
            return sse::stream(headers, Box::new(stream));
        }

        // The client wants one body: collapse the stream.
        let aggregated = match aggregator::aggregate_responses(body) {
            Ok(aggregated) => aggregated,
            Err(err) => {
                context.release_upstream_slot();
                return openai_error(502, &err, "server_error");
            }
        };
        context.release_upstream_slot();

        return match aggregated {
            Aggregated::Completed(value) => {
                let bytes = serde_json::to_vec(&value).unwrap_or_default();
                let bytes = if native {
                    bytes
                } else {
                    match translate::responses_to_chat_completion(&bytes) {
                        Ok(bytes) => bytes,
                        Err(err) => return openai_error(502, &err, "server_error"),
                    }
                };
                Response::bytes(200, headers, bytes)
            }
            Aggregated::Failed { code, message } => {
                let detail = match &code {
                    Some(code) => format!("{message} ({code})"),
                    None => message,
                };
                let (status, kind) = match code.as_deref() {
                    Some("subscription_sharing_usage_limit_exceeded") => (429, "rate_limit_error"),
                    Some("subscription_sharing_user_not_eligible") => (403, "permission_error"),
                    Some("subscription_sharing_usage_unavailable") => (503, "server_error"),
                    Some("subscription_sharing_invalid_user") => (401, "invalid_request_error"),
                    _ => (502, "server_error"),
                };
                openai_error(status, &detail, kind)
            }
            Aggregated::Incomplete => openai_error(
                502,
                "the upstream stream ended without a completed response",
                "server_error",
            ),
        };
    }

    // A plain body.
    let status = response.status;
    let bytes = match response.read_to_vec() {
        Ok(bytes) => bytes,
        Err(err) => {
            context.release_upstream_slot();
            return openai_error(
                502,
                &format!("could not read the upstream response: {err}"),
                "server_error",
            );
        }
    };
    context.release_upstream_slot();

    let bytes = if native {
        bytes
    } else {
        match translate::responses_to_chat_completion(&bytes) {
            Ok(bytes) => bytes,
            Err(err) => return openai_error(502, &err, "server_error"),
        }
    };

    Response::bytes(status, headers, bytes)
}

/// Apply a provider's rules to a request body.
fn apply_policy(body: &[u8], policy: &crate::provider::RequestPolicy) -> Vec<u8> {
    if policy.strip_fields.is_empty()
        && !policy.force_stream
        && !policy.force_store_false
        && !policy.require_input_array
    {
        return body.to_vec();
    }

    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };

    for field in policy.strip_fields {
        object.remove(*field);
    }
    if policy.force_stream {
        object.insert("stream".to_string(), json!(true));
    }
    if policy.force_store_false {
        object.insert("store".to_string(), json!(false));
    }
    if policy.require_input_array {
        // Sign in with ChatGPT wants the conversation as an `input` array. A
        // client that sent a bare string is talking the Responses shape it
        // knows, and the provider rejects that, so it is converted here.
        if let Some(input) = object.get("input")
            && let Some(text) = input.as_str()
        {
            let message = json!({ "role": "user", "content": text });
            object.insert("input".to_string(), json!([message]));
        }
    }

    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}

/// The model a request names.
fn extract_model(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Whether the client asked for a streamed answer.
fn body_wants_stream(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false)
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

/// A stream that gives an upstream slot back when it ends.
///
/// The slot has to be held for as long as the stream is running: a request
/// that is still streaming is still in flight.
struct ReleasingStream {
    inner: Box<dyn Read + Send + 'static>,
    counter: Option<Arc<AtomicUsize>>,
}

impl ReleasingStream {
    fn new(inner: Box<dyn Read + Send + 'static>, counter: Arc<AtomicUsize>) -> ReleasingStream {
        ReleasingStream {
            inner,
            counter: Some(counter),
        }
    }
}

impl Drop for ReleasingStream {
    fn drop(&mut self) {
        if let Some(counter) = self.counter.take() {
            counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl Read for ReleasingStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_is_read_out_of_the_body() {
        assert_eq!(
            extract_model(br#"{"model":"grok-4.7","messages":[]}"#),
            "grok-4.7"
        );
        assert_eq!(extract_model(b"not json"), "");
        assert_eq!(extract_model(br#"{"messages":[]}"#), "");
    }

    #[test]
    fn the_stream_flag_is_read_out_of_the_body() {
        assert!(body_wants_stream(br#"{"stream":true}"#));
        assert!(!body_wants_stream(br#"{"stream":false}"#));
        assert!(!body_wants_stream(br#"{}"#));
    }

    #[test]
    fn the_rejected_fields_are_stripped_and_the_required_ones_added() {
        let policy = crate::provider::RequestPolicy {
            strip_fields: crate::provider::openai::REJECTED_FIELDS,
            force_stream: true,
            force_store_false: true,
            require_input_array: true,
        };

        let body =
            br#"{"model":"gpt-5","temperature":0.7,"max_output_tokens":10,"metadata":{"a":1}}"#;
        let applied = apply_policy(body, &policy);
        let value: Value = serde_json::from_slice(&applied).unwrap();

        assert_eq!(value["model"], "gpt-5");
        assert!(value.get("temperature").is_none());
        assert!(value.get("max_output_tokens").is_none());
        assert!(value.get("metadata").is_none());
        assert_eq!(value["stream"], json!(true));
        assert_eq!(value["store"], json!(false));
    }

    #[test]
    fn a_permissive_policy_leaves_the_body_alone() {
        let policy = crate::provider::RequestPolicy::default();
        let body = br#"{"model":"grok-4.7","temperature":0.7}"#;
        assert_eq!(apply_policy(body, &policy), body);
    }
}
