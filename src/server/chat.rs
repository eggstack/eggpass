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
    openai_error, openai_error_with_code, sse,
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

    // Does the provider speak the shape the client used?
    //
    // A Responses-only provider speaks the Responses API natively, so a
    // request to `/v1/responses` is forwarded as it is — only a Chat
    // Completions request needs translating. Deciding this on the provider
    // alone would translate a native Responses request into nonsense.
    let native = match handle.spec.native_paths {
        NativePaths::Both => true,
        NativePaths::ResponsesOnly => path == "/v1/responses",
    };
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
            context.record_inference(provider_id, 401);
            return openai_error(
                401,
                &format!("{}: {err}", handle.spec.display_name),
                "invalid_request_error",
            );
        }
        Err(err) => {
            context.release_upstream_slot();
            context.record_inference(provider_id, 502);
            return openai_error(502, &err.to_string(), "server_error");
        }
    };

    let status = response.status;
    let content_type = response
        .header("Content-Type")
        .unwrap_or("application/json")
        .to_string();
    let headers = super::forwarded_headers(&response.headers);

    // An error answer is never an event stream.
    //
    // A provider that forces streaming asks for `stream: true`, but the
    // upstream still answers a rejected request with a plain JSON body. That
    // body has to be read as an error rather than parsed as a stream, or the
    // client is told "server error" when the upstream actually said, say,
    // "model not found" with a 400.
    if status >= 400 {
        context.release_upstream_slot();
        let bytes = match response.read_to_vec() {
            Ok(bytes) => bytes,
            Err(err) => {
                context.record_inference(provider_id, 502);
                return openai_error(
                    502,
                    &format!("could not read the upstream response: {err}"),
                    "server_error",
                );
            }
        };
        let response = upstream_error(status, &bytes);
        context.record_inference(provider_id, response.status);
        return response;
    }

    // Whether this is an event stream.
    //
    // The `Content-Type` header is not enough on its own: `api.openai.com`
    // answers a streamed Responses request with **no `Content-Type` header
    // at all**, so a header-only check sends the stream down the plain-body
    // path and tries to parse `event: response.created` as JSON.
    //
    // A provider that forces `stream: true` was asked for an event stream, so
    // the answer is one — whatever the header says. The header is still
    // honoured for providers that stream without being asked to.
    let is_event_stream =
        content_type.starts_with("text/event-stream") || handle.spec.policy.force_stream;

    if is_event_stream {
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
            // The upstream answered, so the last inference is a success. A
            // stream that breaks afterwards is the client's problem: the code
            // it was given is the code that was sent.
            context.record_inference(provider_id, status);
            return sse::stream(headers, Box::new(stream));
        }

        // The client wants one body: collapse the stream.
        let aggregated = match aggregator::aggregate_responses(body) {
            Ok(aggregated) => aggregated,
            Err(err) => {
                context.release_upstream_slot();
                context.record_inference(provider_id, 502);
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
                        Err(err) => {
                            context.record_inference(provider_id, 502);
                            return openai_error(502, &err, "server_error");
                        }
                    }
                };
                context.record_inference(provider_id, 200);
                Response::bytes(200, headers, bytes)
            }
            Aggregated::Failed { code, message } => {
                let (status, kind) = match code.as_deref() {
                    Some("subscription_sharing_usage_limit_exceeded") => (429, "rate_limit_error"),
                    Some("subscription_sharing_user_not_eligible") => (403, "permission_error"),
                    Some("subscription_sharing_usage_unavailable") => (503, "server_error"),
                    Some("subscription_sharing_invalid_user") => (401, "invalid_request_error"),
                    _ => (502, "server_error"),
                };
                // The upstream answered 200 and reported the failure in the
                // stream. The proxy answers the client with the status the
                // code stands for, and that is the code that is remembered.
                context.record_inference(provider_id, status);
                openai_error(status, &message, kind)
            }
            Aggregated::Incomplete => {
                context.record_inference(provider_id, 502);
                openai_error(
                    502,
                    "the upstream stream ended without a completed response",
                    "server_error",
                )
            }
        };
    }

    // A plain body.
    let bytes = match response.read_to_vec() {
        Ok(bytes) => bytes,
        Err(err) => {
            context.release_upstream_slot();
            context.record_inference(provider_id, 502);
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
            Err(err) => {
                context.record_inference(provider_id, 502);
                return openai_error(502, &err, "server_error");
            }
        }
    };

    // The status recorded is the one the client was answered with, so a
    // translation failure is recorded as the failure it is.
    context.record_inference(provider_id, status);
    Response::bytes(status, headers, bytes)
}

/// The upstream's error, in the OpenAI error shape.
///
/// The upstream's status, type and code are kept. A client that asked for a
/// model the provider does not serve has to be told exactly that — a blanket
/// "server error" hides both the reason and the status, and turns a request
/// that cannot succeed into something that looks worth retrying.
fn upstream_error(status: u16, body: &[u8]) -> Response {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));

    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            // The upstream answers a rejected parameter with `detail` and no
            // `error` object at all.
            parsed
                .as_ref()
                .and_then(|value| value.get("detail"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("the upstream answered with HTTP {status}"));

    let kind = match error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
    {
        Some("invalid_request_error") => "invalid_request_error",
        Some("rate_limit_error") => "rate_limit_error",
        Some("authentication_error") => "authentication_error",
        Some("permission_error") => "permission_error",
        Some("server_error") => "server_error",
        _ => match status {
            401 => "authentication_error",
            403 => "permission_error",
            429 => "rate_limit_error",
            400..=499 => "invalid_request_error",
            _ => "server_error",
        },
    };

    let status = match status {
        400..=599 => status,
        _ => 502,
    };

    openai_error_with_code(status, &message, kind, code.as_deref())
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
    use crate::server::http::Body;

    /// The bytes of a response body.
    fn body_bytes(response: &Response) -> &[u8] {
        match &response.body {
            Body::Bytes(bytes) => bytes,
            Body::Stream(_) => panic!("expected a complete body"),
        }
    }

    #[test]
    fn an_upstream_error_keeps_its_status_type_and_code() {
        // The upstream's answer for a model it does not serve: a 400 with a
        // `model_not_found` code. Flattening that into a 502 "server error"
        // tells the client to retry something that cannot succeed.
        let body = br#"{
            "detail": "The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT account.",
            "error": {
                "type": "invalid_request_error",
                "code": "model_not_found",
                "message": "The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT account."
            }
        }"#;

        let response = upstream_error(400, body);
        assert_eq!(response.status, 400);
        let value: Value = serde_json::from_slice(body_bytes(&response)).unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["code"], "model_not_found");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not supported when using Codex")
        );
    }

    #[test]
    fn an_upstream_error_without_an_error_object_uses_its_detail() {
        // The upstream answers a rejected parameter with `detail` and nothing
        // else.
        let body = br#"{"detail":"Unsupported parameter: background"}"#;

        let response = upstream_error(400, body);
        assert_eq!(response.status, 400);
        let value: Value = serde_json::from_slice(body_bytes(&response)).unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"],
            "Unsupported parameter: background"
        );
    }

    #[test]
    fn an_upstream_error_keeps_a_rate_limit() {
        let body =
            br#"{"error":{"type":"rate_limit_error","code":"tokens","message":"slow down"}}"#;

        let response = upstream_error(429, body);
        assert_eq!(response.status, 429);
        let value: Value = serde_json::from_slice(body_bytes(&response)).unwrap();
        assert_eq!(value["error"]["type"], "rate_limit_error");
        assert_eq!(value["error"]["code"], "tokens");
    }

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
        assert!(value.get("metadata").is_none());
        assert_eq!(value["stream"], json!(true));
        assert_eq!(value["store"], json!(false));

        // The route accepts a token limit, so a client that sets one means it.
        assert_eq!(value["max_output_tokens"], 10);
    }

    #[test]
    fn the_rejected_fields_are_the_ones_the_route_rejects() {
        // Every field in the list is one the Sign in with ChatGPT route
        // answers with "Unsupported parameter". A field that is not rejected
        // must not be in the list, or a client's request is quietly changed.
        for field in crate::provider::openai::REJECTED_FIELDS {
            assert!(
                !matches!(*field, "max_output_tokens"),
                "{field} is accepted by the route"
            );
        }
    }

    #[test]
    fn a_permissive_policy_leaves_the_body_alone() {
        let policy = crate::provider::RequestPolicy::default();
        let body = br#"{"model":"grok-4.7","temperature":0.7}"#;
        assert_eq!(apply_policy(body, &policy), body);
    }
}
