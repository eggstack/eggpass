//! Translating between the Chat Completions and Responses shapes.
//!
//! Sign in with ChatGPT serves the **Responses** API only, and its
//! conversation has to arrive as an `input` array. A client that speaks Chat
//! Completions — which is most of them — therefore needs its request
//! translated on the way in and its answer translated on the way out.
//!
//! x.ai speaks both natively, so nothing is translated for it.

use std::io::{Read, Result as IoResult};

use serde_json::{Value, json};

/// The request fields that carry over from a Chat Completions request.
///
/// Everything else either does not exist in the Responses API or is rejected
/// outright by the Sign in with ChatGPT route.
const PASSTHROUGH: &[&str] = &["model", "tool_choice", "parallel_tool_calls", "reasoning"];

/// Convert a Chat Completions request into a Responses request.
///
/// System messages become `instructions`, because the Sign in with ChatGPT
/// route rejects an explicit `{type: "message", role: "system"}` item.
pub fn chat_request_to_responses(body: &[u8]) -> Result<Vec<u8>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|err| format!("the request is not JSON: {err}"))?;
    let Some(object) = value.as_object() else {
        return Err("the request body is not a JSON object".to_string());
    };

    let mut output = serde_json::Map::new();

    for field in PASSTHROUGH {
        if let Some(value) = object.get(*field) {
            output.insert((*field).to_string(), value.clone());
        }
    }

    // The instructions come from the system messages.
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content");

            match role {
                "system" => {
                    if let Some(text) = content_text(content) {
                        instructions.push(text);
                    }
                }
                "developer" => {
                    if let Some(text) = content_text(content) {
                        input.push(json!({
                            "role": "developer",
                            "content": [{"type": "input_text", "text": text}],
                        }));
                    }
                }
                "tool" => {
                    // A tool result is a function-call output in the Responses
                    // shape.
                    let call_id = message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let output = content_text(content).unwrap_or_default();
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
                "assistant" => {
                    if let Some(text) = content_text(content) {
                        input.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        }));
                    }
                }
                _ => {
                    if let Some(text) = content_text(content) {
                        input.push(json!({
                            "role": "user",
                            "content": [{"type": "input_text", "text": text}],
                        }));
                    }
                }
            }
        }
    }

    if !instructions.is_empty() {
        output.insert("instructions".to_string(), json!(instructions.join("\n\n")));
    }
    output.insert("input".to_string(), Value::Array(input));

    // The tools have a flatter shape in the Responses API.
    if let Some(tools) = object.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools.iter().filter_map(convert_tool).collect();
        if !converted.is_empty() {
            output.insert("tools".to_string(), Value::Array(converted));
        }
    }

    serde_json::to_vec(&Value::Object(output)).map_err(|err| err.to_string())
}

/// The text of a message's content, whatever shape it arrives in.
fn content_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text: Vec<String> = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| part.as_str().map(str::to_string))
                })
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(text.join("\n"))
            }
        }
        _ => None,
    }
}

/// Convert one Chat Completions tool definition into the Responses shape.
fn convert_tool(tool: &Value) -> Option<Value> {
    let kind = tool.get("type").and_then(Value::as_str)?;
    if kind != "function" {
        // Only function tools have a mechanical translation.
        return None;
    }
    let function = tool.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;

    let mut converted = json!({ "type": "function", "name": name });
    if let Some(description) = function.get("description").and_then(Value::as_str) {
        converted["description"] = json!(description);
    }
    if let Some(parameters) = function.get("parameters") {
        converted["parameters"] = parameters.clone();
    }
    Some(converted)
}

/// Convert a Responses response into a Chat Completions response.
pub fn responses_to_chat_completion(body: &[u8]) -> Result<Vec<u8>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|err| format!("the response is not JSON: {err}"))?;

    let text = response_text(&value);
    let finish_reason = finish_reason(&value);

    let mut choices = vec![json!({
        "index": 0,
        "message": { "role": "assistant", "content": text },
        "finish_reason": finish_reason,
    })];

    // A failed response carries an error rather than an answer.
    if let Some(error) = value.get("error") {
        choices[0]["message"] = json!({
            "role": "assistant",
            "content": Value::Null,
        });
        choices[0]["finish_reason"] = json!("error");
        let _ = error;
    }

    let mut output = json!({
        "id": chat_id(value.get("id").and_then(Value::as_str)),
        "object": "chat.completion",
        "created": now_unix(),
        "model": value.get("model").and_then(Value::as_str).unwrap_or(""),
        "choices": choices,
    });

    if let Some(usage) = responses_usage_to_chat(value.get("usage")) {
        output["usage"] = usage;
    }

    serde_json::to_vec(&output).map_err(|err| err.to_string())
}

/// The assistant text in a Responses object.
pub fn response_text(value: &Value) -> String {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return String::new();
    };

    let mut parts: Vec<String> = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                parts.push(text.to_string());
            }
        }
    }
    parts.join("")
}

/// The finish reason of a Responses object.
fn finish_reason(value: &Value) -> &'static str {
    match value.get("status").and_then(Value::as_str) {
        Some("incomplete") => "length",
        Some("failed") | Some("cancelled") => "error",
        _ => "stop",
    }
}

/// The usage of a Responses object, in the Chat Completions shape.
fn responses_usage_to_chat(usage: Option<&Value>) -> Option<Value> {
    let usage = usage?;
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input + output);

    Some(json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": total,
    }))
}

/// Turn a Responses id into a Chat Completions id.
fn chat_id(id: Option<&str>) -> String {
    match id {
        Some(id) => format!("chatcmpl-{}", id.trim_start_matches("resp_")),
        None => "chatcmpl-unknown".to_string(),
    }
}

/// The current time, in seconds since the unix epoch.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Rewrites a Responses event stream into Chat Completions chunks.
///
/// The event stream is read a chunk at a time and rewritten on the way
/// through, so the client sees the chunks as they arrive rather than after
/// the whole answer has been received.
pub struct ResponsesToChatStream<R> {
    source: R,
    buffer: String,
    pending: Vec<u8>,
    finished: bool,
    id: String,
    model: String,
    created: i64,
    usage: Option<Value>,
}

impl<R: Read> ResponsesToChatStream<R> {
    /// Wrap an upstream event stream.
    pub fn new(source: R) -> ResponsesToChatStream<R> {
        ResponsesToChatStream {
            source,
            buffer: String::new(),
            pending: Vec::new(),
            finished: false,
            id: "chatcmpl-unknown".to_string(),
            model: String::new(),
            created: now_unix(),
            usage: None,
        }
    }

    /// Translate one Responses event into the chunks it produces.
    fn translate_event(&mut self, event: &Value) -> Vec<u8> {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");

        // The first event carries the response, and with it the id and model.
        if let Some(response) = event.get("response") {
            if let Some(id) = response.get("id").and_then(Value::as_str) {
                self.id = chat_id(Some(id));
            }
            if let Some(model) = response.get("model").and_then(Value::as_str) {
                self.model = model.to_string();
            }
        }

        match kind {
            "response.output_text.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if delta.is_empty() {
                    return Vec::new();
                }
                let chunk = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": delta },
                        "finish_reason": Value::Null,
                    }],
                });
                encode_event(&chunk)
            }
            "response.completed" | "response.incomplete" => {
                let response = event.get("response");
                if let Some(response) = response {
                    if let Some(id) = response.get("id").and_then(Value::as_str) {
                        self.id = chat_id(Some(id));
                    }
                    if let Some(model) = response.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                    self.usage = responses_usage_to_chat(response.get("usage"));
                }
                let finish = if kind == "response.incomplete" {
                    "length"
                } else {
                    "stop"
                };
                let mut chunk = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish,
                    }],
                });
                if let Some(usage) = &self.usage {
                    chunk["usage"] = usage.clone();
                }
                let mut out = encode_event(&chunk);
                out.extend_from_slice(b"data: [DONE]\n\n");
                out
            }
            "response.failed" => {
                let message = event
                    .get("response")
                    .and_then(|response| response.get("error"))
                    .map(|error| {
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the request failed")
                            .to_string()
                    })
                    .unwrap_or_else(|| "the request failed".to_string());
                let chunk = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "error",
                    }],
                    "error": { "message": message, "type": "server_error" },
                });
                let mut out = encode_event(&chunk);
                out.extend_from_slice(b"data: [DONE]\n\n");
                out
            }
            // Everything else is bookkeeping the client does not need.
            _ => Vec::new(),
        }
    }
}

impl<R: Read> Read for ResponsesToChatStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        loop {
            if !self.pending.is_empty() {
                let count = self.pending.len().min(buf.len());
                buf[..count].copy_from_slice(&self.pending[..count]);
                self.pending.drain(..count);
                return Ok(count);
            }

            if self.finished {
                return Ok(0);
            }

            // Pull more of the upstream stream.
            let mut chunk = [0u8; 4096];
            let read = self.source.read(&mut chunk)?;
            if read == 0 {
                self.finished = true;
                // Flush whatever is left, in case it is a final event with no
                // trailing blank line.
                let rest = std::mem::take(&mut self.buffer);
                for event in split_events(&rest) {
                    if let Some(value) = parse_event(&event) {
                        let out = self.translate_event(&value);
                        self.pending.extend_from_slice(&out);
                    }
                }
                // A final event that never got its blank line.
                if !rest.trim().is_empty()
                    && let Some(value) = parse_event(&rest)
                {
                    let out = self.translate_event(&value);
                    self.pending.extend_from_slice(&out);
                }
                if self.pending.is_empty() {
                    return Ok(0);
                }
                continue;
            }

            self.buffer
                .push_str(&String::from_utf8_lossy(&chunk[..read]));

            // Translate every complete event we have.
            let mut events: Vec<String> = Vec::new();
            while let Some(index) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..index + 2).collect();
                events.push(event);
            }

            for event in &events {
                if let Some(value) = parse_event(event) {
                    let out = self.translate_event(&value);
                    self.pending.extend_from_slice(&out);
                }
            }

            if self.pending.is_empty() {
                continue;
            }
        }
    }
}

/// Encode one event as a server-sent event.
fn encode_event(value: &Value) -> Vec<u8> {
    format!("data: {value}\n\n").into_bytes()
}

/// Split a buffer into complete server-sent events.
fn split_events(buffer: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut rest = buffer;
    while let Some(index) = rest.find("\n\n") {
        let event: String = rest[..index + 2].to_string();
        events.push(event);
        rest = &rest[index + 2..];
    }
    if !rest.trim().is_empty() {
        events.push(rest.to_string());
    }
    events
}

/// Read the JSON payload of a server-sent event.
fn parse_event(event: &str) -> Option<Value> {
    let payload: String = event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    serde_json::from_str(payload).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_request_becomes_a_responses_request() {
        let body = br#"{
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ],
            "temperature": 0.7,
            "max_tokens": 100
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["model"], "gpt-5");
        assert_eq!(value["instructions"], "be brief");
        assert_eq!(value["input"][0]["role"], "user");
        assert_eq!(value["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(value["input"][0]["content"][0]["text"], "hello");
        assert_eq!(value["input"][1]["role"], "assistant");
        assert_eq!(value["input"][1]["content"][0]["type"], "output_text");

        // The rejected fields are not carried over.
        assert!(value.get("temperature").is_none());
        assert!(value.get("max_tokens").is_none());
    }

    #[test]
    fn a_tool_message_becomes_a_function_call_output() {
        let body = br#"{
            "model": "gpt-5",
            "messages": [
                {"role": "tool", "tool_call_id": "call_1", "content": "42"}
            ]
        }"#;
        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["input"][0]["type"], "function_call_output");
        assert_eq!(value["input"][0]["call_id"], "call_1");
        assert_eq!(value["input"][0]["output"], "42");
    }

    #[test]
    fn a_chat_tool_is_flattened() {
        let body = br#"{
            "model": "gpt-5",
            "messages": [],
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}]
        }"#;
        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["tools"][0]["type"], "function");
        assert_eq!(value["tools"][0]["name"], "get_weather");
    }

    #[test]
    fn a_responses_response_becomes_a_chat_completion() {
        let body = br#"{
            "id": "resp_abc",
            "object": "response",
            "status": "completed",
            "model": "gpt-5",
            "output": [
                {"type": "reasoning", "summary": []},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        }"#;

        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["object"], "chat.completion");
        assert_eq!(value["id"], "chatcmpl-abc");
        assert_eq!(value["choices"][0]["message"]["content"], "hello");
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
        assert_eq!(value["usage"]["prompt_tokens"], 10);
        assert_eq!(value["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn an_incomplete_response_finishes_with_length() {
        let body = br#"{"id":"resp_1","status":"incomplete","model":"gpt-5","output":[]}"#;
        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn the_event_stream_becomes_chat_chunks() {
        let events = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"he\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"llo\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n",
        );

        let mut translator = ResponsesToChatStream::new(events.as_bytes());
        let mut out = String::new();
        translator.read_to_string(&mut out).unwrap();

        // Two content chunks and a final one with the finish reason.
        assert_eq!(out.matches("chat.completion.chunk").count(), 3, "{out}");
        assert!(out.contains("\"content\":\"he\""), "{out}");
        assert!(out.contains("\"content\":\"llo\""), "{out}");
        assert!(out.contains("\"finish_reason\":\"stop\""), "{out}");
        assert!(out.contains("data: [DONE]"), "{out}");
    }

    #[test]
    fn a_failed_event_becomes_an_error_chunk() {
        let events = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"nope\"}}}\n\n",
        );

        let mut translator = ResponsesToChatStream::new(events.as_bytes());
        let mut out = String::new();
        translator.read_to_string(&mut out).unwrap();

        assert!(out.contains("\"finish_reason\":\"error\""), "{out}");
        assert!(out.contains("nope"), "{out}");
    }
}
