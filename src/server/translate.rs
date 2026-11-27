//! Translating between the Chat Completions and Responses shapes.
//!
//! Sign in with ChatGPT serves the **Responses** API only, and its
//! conversation has to arrive as an `input` array. A client that speaks Chat
//! Completions — which is most of them — therefore needs its request
//! translated on the way in and its answer translated on the way out.
//!
//! x.ai speaks both natively, so nothing is translated for it.

use super::parts::{self, Role};
use serde_json::{Value, json};
use std::fmt;
use std::io::{Read, Result as IoResult};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslateError {
    message: String,
    code: &'static str,
}
impl TranslateError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
    pub fn code(&self) -> &'static str {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

const CARRIED: &[&str] = &[
    "model",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning",
    "max_output_tokens",
    "stream",
    "store",
    "service_tier",
    "modalities",
    "audio",
];

pub fn chat_request_to_responses(body: &[u8]) -> Result<Vec<u8>, TranslateError> {
    let value: Value = serde_json::from_slice(body).map_err(|err| {
        TranslateError::new(
            "unsupported_parameter",
            format!("the request is not JSON: {err}"),
        )
    })?;
    let Some(object) = value.as_object() else {
        return Err(TranslateError::new(
            "unsupported_parameter",
            "the request body is not a JSON object",
        ));
    };
    let mut output = serde_json::Map::new();
    for (field, value) in object {
        if crate::provider::openai::REJECTED_FIELDS.contains(&field.as_str()) {
            // This is provider policy from the live route, not an omission; passing these through makes the upstream reject the whole request.
            continue;
        }
        if CARRIED.contains(&field.as_str()) {
            output.insert(
                field.clone(),
                if field == "tool_choice" {
                    flatten_tool_choice(value)
                } else {
                    value.clone()
                },
            );
            continue;
        }
        if !matches!(
            field.as_str(),
            "messages"
                | "tools"
                | "reasoning_effort"
                | "max_tokens"
                | "max_completion_tokens"
                | "response_format"
                | "n"
                | "stream_options"
                | "stop"
                | "seed"
                | "logit_bias"
                | "prediction"
                | "web_search_options"
                | "function_call"
                | "functions"
                | "logprobs"
                | "presence_penalty"
                | "frequency_penalty"
        ) {
            return Err(unsupported_parameter(
                field,
                "has no known Responses API mapping",
            ));
        }
    }
    if let Some(format) = object.get("response_format") {
        output.insert(
            "text".into(),
            json!({"format":map_response_format(format)?}),
        );
    }
    if let Some(n) = object.get("n").filter(|value| !value.is_null())
        && n.as_u64() != Some(1)
    {
        return Err(unsupported_parameter(
            "n",
            "Responses returns one choice per request",
        ));
    }
    if let Some(options) = object
        .get("stream_options")
        .filter(|value| !value.is_null())
    {
        let Some(options) = options.as_object() else {
            return Err(unsupported_parameter(
                "stream_options",
                "must be an object containing only include_usage",
            ));
        };
        for key in options.keys() {
            if key != "include_usage" {
                return Err(unsupported_parameter(
                    key,
                    "is not supported in stream_options",
                ));
            }
        }
    }
    for field in [
        "stop",
        "seed",
        "logit_bias",
        "prediction",
        "web_search_options",
        "function_call",
        "functions",
    ] {
        if object.contains_key(field) {
            let reason = match field {
                "stop" => "Responses has no stop-sequence equivalent",
                "seed" => "Responses does not accept a deterministic seed",
                "logit_bias" => "Responses has no token-bias equivalent",
                "prediction" => "Responses has no prediction parameter equivalent",
                "web_search_options" => "Responses web-search options are not Chat parameters",
                _ => "use the tools and tool_choice parameters instead",
            };
            return Err(unsupported_parameter(field, reason));
        }
    }
    if let Some(value) = object.get("logprobs").filter(|value| !value.is_null()) {
        match value.as_bool() {
            Some(false) => {}
            Some(true) => {
                return Err(unsupported_parameter(
                    "logprobs",
                    "Responses has no logprobs equivalent",
                ));
            }
            None => return Err(unsupported_parameter("logprobs", "must be a boolean")),
        }
    }
    for field in ["presence_penalty", "frequency_penalty"] {
        if let Some(value) = object.get(field).filter(|value| !value.is_null()) {
            let Some(value) = value.as_f64() else {
                return Err(unsupported_parameter(
                    field,
                    "must be zero because Responses has no penalty equivalent",
                ));
            };
            if value != 0.0 {
                return Err(unsupported_parameter(
                    field,
                    "Responses has no non-zero penalty equivalent",
                ));
            }
        }
    }
    if !output.contains_key("reasoning")
        && let Some(effort) = object.get("reasoning_effort")
    {
        let Some(effort) = effort.as_str() else {
            return Err(unsupported_parameter(
                "reasoning_effort",
                "must be a string",
            ));
        };
        output.insert(
            "reasoning".into(),
            json!({"effort":effort,"summary":"auto"}),
        );
    }
    if !output.contains_key("max_output_tokens")
        && let Some(limit) = object
            .get("max_completion_tokens")
            .or_else(|| object.get("max_tokens"))
    {
        let Some(limit) = limit.as_u64() else {
            return Err(unsupported_parameter(
                "max_tokens",
                "must be a non-negative integer",
            ));
        };
        output.insert("max_output_tokens".into(), json!(limit));
    }
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    if let Some(messages_value) = object.get("messages") {
        let Some(messages) = messages_value.as_array() else {
            return Err(unsupported_parameter("messages", "must be an array"));
        };
        for message in messages {
            if !message.is_object() {
                return Err(unsupported_parameter(
                    "messages",
                    "each message must be an object",
                ));
            }
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user");
            let content = message.get("content");
            match role {
                "system" => {
                    if let Some(text) = parts::content_text(content)? {
                        instructions.push(text);
                    }
                }
                "developer" => {
                    let content = parts::to_responses_parts(Role::Developer, content)?;
                    if !content.is_empty() {
                        input.push(json!({"role":"developer","content":content}));
                    }
                }
                "tool" => {
                    let call_id = message
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let output_text = parts::content_text(content)?.unwrap_or_default();
                    input.push(json!({"type":"function_call_output","call_id":call_id,"output":output_text}));
                }
                "assistant" => {
                    let content = parts::to_responses_parts(Role::Assistant, content)?;
                    let only_empty_text = !content.is_empty()
                        && content.iter().all(|part| {
                            part.get("type").and_then(Value::as_str) == Some("output_text")
                                && part.get("text").and_then(Value::as_str) == Some("")
                        });
                    if !content.is_empty() && !only_empty_text {
                        input.push(json!({"role":"assistant","content":content}));
                    }
                    if let Some(calls) = message.get("tool_calls") {
                        let Some(calls) = calls.as_array() else {
                            return Err(unsupported_parameter("tool_calls", "must be an array"));
                        };
                        for call in calls {
                            input.push(convert_tool_call(call)?);
                        }
                    }
                }
                _ => {
                    let content = parts::to_responses_parts(Role::User, content)?;
                    if !content.is_empty() {
                        input.push(json!({"role":"user","content":content}));
                    }
                }
            }
        }
    }
    if !instructions.is_empty() {
        output.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    output.insert("input".into(), Value::Array(input));
    if let Some(tools) = object.get("tools") {
        let Some(tools) = tools.as_array() else {
            return Err(unsupported_parameter("tools", "must be an array"));
        };
        let converted = tools
            .iter()
            .map(convert_tool)
            .collect::<Result<Vec<_>, _>>()?;
        if !converted.is_empty() {
            output.insert("tools".into(), Value::Array(converted));
        }
    }
    serde_json::to_vec(&Value::Object(output))
        .map_err(|err| TranslateError::new("unsupported_parameter", err.to_string()))
}

fn unsupported_parameter(field: &str, reason: &str) -> TranslateError {
    TranslateError::new(
        "unsupported_parameter",
        format!("unsupported parameter {field}: {reason}"),
    )
}
fn map_response_format(value: &Value) -> Result<Value, TranslateError> {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Err(unsupported_parameter(
            "response_format",
            "requires a string type",
        ));
    };
    match kind {
        "json_object" | "text" => Ok(json!({"type":kind})),
        "json_schema" => {
            let Some(schema) = value.get("json_schema").and_then(Value::as_object) else {
                return Err(unsupported_parameter(
                    "response_format.json_schema",
                    "must be an object",
                ));
            };
            let mut mapped = json!({"type":"json_schema"});
            for key in ["name", "schema", "strict", "description"] {
                if let Some(value) = schema.get(key) {
                    mapped[key] = value.clone();
                }
            }
            Ok(mapped)
        }
        other => Err(unsupported_parameter(
            "response_format.type",
            &format!("unknown type {other}"),
        )),
    }
}
fn flatten_tool_choice(value: &Value) -> Value {
    if value.get("type").and_then(Value::as_str) == Some("function")
        && let Some(name) = value
            .get("function")
            .and_then(|function| function.get("name"))
    {
        return json!({"type":"function","name":name});
    }
    value.clone()
}
fn convert_tool(tool: &Value) -> Result<Value, TranslateError> {
    let Some(kind) = tool.get("type").and_then(Value::as_str) else {
        return Ok(tool.clone());
    };
    if kind == "image_generation" {
        return Err(TranslateError::new(
            "image_requires_responses",
            "image_generation tools require the native Responses API",
        ));
    }
    if kind.starts_with("audio") {
        return Err(TranslateError::new(
            "audio_requires_responses",
            format!("{kind} tools require the native Responses API"),
        ));
    }
    if kind != "function" {
        return Ok(tool.clone());
    }
    let Some(function) = tool.get("function") else {
        return Err(unsupported_parameter(
            "tools.function",
            "function definition is missing",
        ));
    };
    let Some(name) = function.get("name").and_then(Value::as_str) else {
        return Err(unsupported_parameter(
            "tools.function.name",
            "function name is required",
        ));
    };
    let mut converted = json!({"type":"function","name":name});
    if let Some(description) = function.get("description") {
        converted["description"] = description.clone();
    }
    if let Some(parameters) = function.get("parameters") {
        converted["parameters"] = parameters.clone();
    }
    if let Some(strict) = function.get("strict") {
        converted["strict"] = strict.clone();
    }
    Ok(converted)
}

/// Convert one Chat Completions tool call into the Responses shape.
/// The Responses API uses call_id to match a tool result to this call.
fn convert_tool_call(call: &Value) -> Result<Value, TranslateError> {
    let Some(function) = call.get("function") else {
        return Err(unsupported_parameter(
            "tool_calls.function",
            "function call data is missing",
        ));
    };
    let Some(name) = function.get("name").and_then(Value::as_str) else {
        return Err(unsupported_parameter(
            "tool_calls.function.name",
            "function name is required",
        ));
    };
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    Ok(json!({"type":"function_call","call_id":call_id,"name":name,"arguments":arguments}))
}

/// Convert a Responses response into a Chat Completions response.
pub fn responses_to_chat_completion(body: &[u8]) -> Result<Vec<u8>, TranslateError> {
    let value: Value = serde_json::from_slice(body).map_err(|err| {
        TranslateError::new(
            "unmappable_output_item",
            format!("the response is not JSON: {err}"),
        )
    })?;
    let text = response_text(&value);
    let mut content = Value::String(text);
    let mut parts = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            let kind = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let id = item.get("id").and_then(Value::as_str).unwrap_or("unknown");
            if kind == "image_generation_call" || kind.starts_with("audio") {
                return Err(TranslateError::new(
                    "unmappable_output_item",
                    format!(
                        "Responses output item {kind} (id {id}) has no Chat Completions equivalent"
                    ),
                ));
            }
            if kind != "message" {
                continue;
            }
            for part in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match part.get("type").and_then(Value::as_str).unwrap_or("unknown") {
                    "output_text" => parts.push(json!({"type":"text","text":part.get("text").and_then(Value::as_str).unwrap_or("")})),
                    "refusal" => parts.push(json!({"type":"refusal","refusal":part.get("refusal").and_then(Value::as_str).unwrap_or("")})),
                    other => return Err(TranslateError::new("unmappable_output_item", format!("Responses output content part {other} in item {id} has no Chat Completions equivalent"))),
                }
            }
        }
    }
    if parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
    {
        content = Value::Array(parts);
    }
    let tool_calls = response_tool_calls(&value);
    let mut reason = finish_reason(&value);
    if !tool_calls.is_empty() {
        reason = "tool_calls";
    }
    let mut choices = vec![
        json!({"index":0,"message":{"role":"assistant","content":content},"finish_reason":reason}),
    ];
    if !tool_calls.is_empty() {
        choices[0]["message"]["tool_calls"] = Value::Array(tool_calls);
    }
    let reasoning = response_reasoning(&value);
    if !reasoning.is_empty() {
        choices[0]["message"]["reasoning_content"] = json!(reasoning);
    }
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        choices[0]["message"] = json!({"role":"assistant","content":Value::Null});
        choices[0]["finish_reason"] = json!("error");
        let _ = error;
    }
    let mut output = json!({"id":chat_id(value.get("id").and_then(Value::as_str)),"object":"chat.completion","created":now_unix(),"model":value.get("model").and_then(Value::as_str).unwrap_or(""),"choices":choices});
    if let Some(usage) = responses_usage_to_chat(value.get("usage")) {
        output["usage"] = usage;
    }
    serde_json::to_vec(&output)
        .map_err(|err| TranslateError::new("unmappable_output_item", err.to_string()))
}

/// The tool calls in a Responses object.
///
/// The Responses API carries a tool call as an output item of type
/// `function_call`: the tool's name, the arguments it was given, and the
/// `call_id` that ties the call to the tool's answer. The Chat Completions
/// shape asks for the same thing in `message.tool_calls`.
///
/// The `call_id` is the client's handle on the call, so it is preferred over
/// the item's own id.
fn response_tool_calls(value: &Value) -> Vec<Value> {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Vec::new();
    };

    output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| {
            let name = item.get("name").and_then(Value::as_str)?;
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .unwrap_or_default();

            Some(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": arguments },
            }))
        })
        .collect()
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

/// The thinking text in a Responses object.
///
/// The reasoning items carry a `summary` — a list of `summary_text` parts —
/// rather than the reasoning itself. The summary is what the upstream is
/// willing to hand over, so it is what a client is shown.
fn response_reasoning(value: &Value) -> String {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return String::new();
    };

    let mut parts: Vec<String> = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(summary) = item.get("summary").and_then(Value::as_array) else {
            continue;
        };
        for part in summary {
            if part.get("type").and_then(Value::as_str) == Some("summary_text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                parts.push(text.to_string());
            }
        }
    }
    parts.join("\n\n")
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

/// A tool call the model asked for.
///
/// The upstream tells the story of a tool call across several events: the call
/// is announced, its arguments arrive in pieces, and the call is completed.
/// The pieces are tied together by the upstream's item id.
struct ToolCall {
    /// The upstream's item id, which its argument events name.
    item_id: String,
    /// The id the client matches the tool's answer against.
    call_id: String,
    /// The tool's name.
    name: String,
    /// The arguments seen so far.
    arguments: String,
    /// Whether the client has been told about this call.
    announced: bool,
}

/// Rewrites a Responses event stream into Chat Completions chunks.
///
/// The event stream is read a chunk at a time and rewritten on the way
/// through, so the client sees the chunks as they arrive rather than after
/// the whole answer has been received.
pub struct ResponsesToChatStream<R> {
    source: R,
    /// Bytes that have arrived but do not yet form a complete event.
    ///
    /// A buffer of *bytes* rather than a `String`, because a read boundary
    /// falls wherever the socket puts it and a multi-byte character can
    /// straddle one. Decoding per read would replace half a character with
    /// `U+FFFD`; the buffer is decoded only up to a blank line, which is an
    /// ASCII boundary and therefore a whole-character one.
    buffer: Vec<u8>,
    pending: Vec<u8>,
    finished: bool,
    id: String,
    model: String,
    created: i64,
    usage: Option<Value>,
    tool_calls: Vec<ToolCall>,
    emitted_answer: bool,
    unmappable_item: Option<String>,
    refusal_deltas: Vec<String>,
}

impl<R: Read> ResponsesToChatStream<R> {
    /// Wrap an upstream event stream.
    pub fn new(source: R) -> ResponsesToChatStream<R> {
        ResponsesToChatStream {
            source,
            buffer: Vec::new(),
            pending: Vec::new(),
            finished: false,
            id: "chatcmpl-unknown".to_string(),
            model: String::new(),
            created: now_unix(),
            usage: None,
            tool_calls: Vec::new(),
            emitted_answer: false,
            unmappable_item: None,
            refusal_deltas: Vec::new(),
        }
    }

    /// One Chat Completions chunk.
    fn chunk(&self, delta: Value, finish_reason: Value) -> Vec<u8> {
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        });
        encode_event(&chunk)
    }

    /// The tool call with this item id, if there is one.
    fn tool_call_index(&self, item_id: Option<&str>) -> Option<usize> {
        let item_id = item_id?;
        self.tool_calls
            .iter()
            .position(|call| call.item_id == item_id)
    }

    /// Note a tool call, and answer the index it is known by.
    ///
    /// A call that has not been seen before is given the next index; the
    /// Chat Completions shape numbers the calls in the order they arrive.
    fn register_tool_call(&mut self, item_id: &str, call_id: &str, name: &str) -> usize {
        if let Some(index) = self.tool_call_index(Some(item_id)) {
            return index;
        }
        self.tool_calls.push(ToolCall {
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: String::new(),
            announced: false,
        });
        self.tool_calls.len() - 1
    }

    /// Tell the client about a tool call, once.
    ///
    /// The client needs the call's id and name before the arguments, so the
    /// first chunk carries those and the arguments follow in their own chunks.
    fn announce_tool_call(&mut self, index: usize) -> Vec<u8> {
        let Some(call) = self.tool_calls.get(index) else {
            return Vec::new();
        };
        if call.announced {
            return Vec::new();
        }
        let delta = json!({
            "tool_calls": [{
                "index": index,
                "id": call.call_id,
                "type": "function",
                "function": { "name": call.name, "arguments": "" },
            }],
        });
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.announced = true;
        }
        self.emitted_answer = true;
        self.chunk(delta, Value::Null)
    }

    /// Carry the arguments of a tool call across.
    ///
    /// The upstream sends the arguments twice: in pieces, and once whole. The
    /// pieces are sent on as they arrive; the whole copy is only sent when
    /// something is missing, so the client is not given the same arguments
    /// twice.
    fn carry_arguments(&mut self, index: usize, arguments: &str) -> Vec<u8> {
        if arguments.is_empty() {
            return Vec::new();
        }

        let seen = self
            .tool_calls
            .get(index)
            .map(|call| call.arguments.clone())
            .unwrap_or_default();

        // What the client still needs to see.
        let missing = if seen.is_empty() {
            arguments.to_string()
        } else if arguments.starts_with(&seen) {
            arguments[seen.len()..].to_string()
        } else {
            // The upstream changed its mind about the arguments. There is no
            // way to take the first copy back, so the first copy stands.
            String::new()
        };

        if missing.is_empty() {
            return Vec::new();
        }

        if let Some(call) = self.tool_calls.get_mut(index) {
            call.arguments.push_str(&missing);
        }

        let delta = json!({
            "tool_calls": [{
                "index": index,
                "function": { "arguments": missing },
            }],
        });
        self.emitted_answer = true;
        self.chunk(delta, Value::Null)
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

        if kind.starts_with("response.image_generation_call.") {
            let id = event
                .get("item_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            self.unmappable_item = Some(format!(
                "Responses output item image_generation_call (id {id}) has no Chat Completions equivalent"
            ));
            return Vec::new();
        }
        if matches!(
            kind,
            "response.output_item.added" | "response.output_item.done"
        ) && let Some(item) = event.get("item")
        {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
            if item_type == "image_generation_call" || item_type.starts_with("audio") {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("unknown");
                self.unmappable_item = Some(format!(
                    "Responses output item {item_type} (id {id}) has no Chat Completions equivalent"
                ));
                return Vec::new();
            }
        }
        match kind {
            "response.output_item.added" => {
                // A tool call starts. The name and the id come with it; the
                // arguments arrive as their own events.
                let Some(item) = event.get("item") else {
                    return Vec::new();
                };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return Vec::new();
                }
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let index = self.register_tool_call(item_id, call_id, name);
                self.announce_tool_call(index)
            }
            "response.function_call_arguments.delta" => {
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    return Vec::new();
                };
                if delta.is_empty() {
                    return Vec::new();
                }
                let Some(index) =
                    self.tool_call_index(event.get("item_id").and_then(Value::as_str))
                else {
                    return Vec::new();
                };
                if let Some(call) = self.tool_calls.get_mut(index) {
                    call.arguments.push_str(delta);
                }
                let chunk = json!({
                    "tool_calls": [{
                        "index": index,
                        "function": { "arguments": delta },
                    }],
                });
                self.emitted_answer = true;
                self.chunk(chunk, Value::Null)
            }
            "response.function_call_arguments.done" => {
                let Some(arguments) = event.get("arguments").and_then(Value::as_str) else {
                    return Vec::new();
                };
                let Some(index) =
                    self.tool_call_index(event.get("item_id").and_then(Value::as_str))
                else {
                    return Vec::new();
                };
                self.carry_arguments(index, arguments)
            }
            "response.output_item.done" => {
                // The whole of a tool call.
                //
                // The upstream may finish a call without ever announcing it,
                // so the call is registered here as well.
                let Some(item) = event.get("item") else {
                    return Vec::new();
                };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return Vec::new();
                }
                let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let index = self.register_tool_call(item_id, call_id, name);
                let mut out = self.announce_tool_call(index);
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                out.extend_from_slice(&self.carry_arguments(index, arguments));
                out
            }
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
                self.emitted_answer = true;
                encode_event(&chunk)
            }
            "response.refusal.delta" => {
                let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if delta.is_empty() {
                    return Vec::new();
                }
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !self.refusal_deltas.contains(&item_id) {
                    self.refusal_deltas.push(item_id);
                }
                self.emitted_answer = true;
                self.chunk(json!({"refusal":delta}), Value::Null)
            }
            "response.refusal.done" => {
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if self.refusal_deltas.iter().any(|id| id == item_id) {
                    return Vec::new();
                }
                let refusal = event
                    .get("refusal")
                    .or_else(|| event.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if refusal.is_empty() {
                    return Vec::new();
                }
                self.emitted_answer = true;
                self.chunk(json!({"refusal":refusal}), Value::Null)
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                // The thinking text arrives as its own events, ahead of the
                // answer. It goes out in `reasoning_content`, the field x.ai's
                // Chat Completions answer uses for the same thing.
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
                        "delta": { "reasoning_content": delta },
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
                if !self.emitted_answer
                    && let Some(message) = self.unmappable_item.take()
                {
                    let chunk = json!({
                        "id": self.id,
                        "object": "chat.completion.chunk",
                        "created": self.created,
                        "model": self.model,
                        "choices": [{"index":0,"delta":{},"finish_reason":"error"}],
                        "error": {"message":message,"type":"server_error","code":"unmappable_output_item"},
                    });
                    let mut out = encode_event(&chunk);
                    out.extend_from_slice(b"data: [DONE]\n\n");
                    return out;
                }
                let finish = if kind == "response.incomplete" {
                    "length"
                } else if self.tool_calls.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
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
                // Flush whatever is left: it is a final event that never got
                // its trailing blank line. `split_events` yields it as one
                // event, so it is translated exactly once.
                let rest = std::mem::take(&mut self.buffer);
                if !rest.is_empty() {
                    let text = String::from_utf8_lossy(&rest);
                    for event in split_events(&text) {
                        if let Some(value) = parse_event(&event) {
                            let out = self.translate_event(&value);
                            self.pending.extend_from_slice(&out);
                        }
                    }
                }
                if self.pending.is_empty() {
                    return Ok(0);
                }
                continue;
            }

            self.buffer.extend_from_slice(&chunk[..read]);

            // Translate every complete event we have.
            let mut events: Vec<String> = Vec::new();
            while let Some(index) = blank_line(&self.buffer) {
                let event: Vec<u8> = self.buffer.drain(..index + 2).collect();
                events.push(String::from_utf8_lossy(&event).into_owned());
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

/// The offset of the first blank line in a byte buffer.
///
/// A complete server-sent event ends with `\n\n`. Because `\n` cannot appear
/// inside a multi-byte character, a region ending at one is whole UTF-8 and
/// can be decoded without splitting a character.
fn blank_line(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|pair| pair == b"\n\n")
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

    /// A reader that hands back `size` bytes at a time, so the read boundary
    /// falls wherever the test wants it.
    struct Trickle {
        data: Vec<u8>,
        at: usize,
        size: usize,
    }

    impl std::io::Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let left = self.data.len() - self.at;
            let take = left.min(buf.len()).min(self.size);
            buf[..take].copy_from_slice(&self.data[self.at..self.at + take]);
            self.at += take;
            Ok(take)
        }
    }

    /// Run a stream through the translator and return what the client sees.
    fn translate(stream: &str, read_size: usize) -> String {
        let data = stream.as_bytes().to_vec();
        let mut subject = ResponsesToChatStream::new(Trickle {
            data,
            at: 0,
            size: read_size,
        });
        let mut out = Vec::new();
        subject.read_to_end(&mut out).expect("the stream reads");
        String::from_utf8(out).expect("the output is UTF-8")
    }

    #[test]
    fn a_multibyte_character_split_across_a_read_keeps_its_bytes() {
        // One byte per read puts the boundary between the two bytes of every
        // `é`. Decoding each read on its own would replace each half with
        // U+FFFD, which is what this test exists to prevent.
        let stream = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ééé\"}\n\n";

        for read_size in [1, 2, 3, 5, 7, 11] {
            let text = translate(stream, read_size);
            assert!(
                !text.contains('\u{FFFD}'),
                "read size {read_size} corrupted the text: {text}"
            );
            assert!(
                text.contains("ééé"),
                "read size {read_size} lost the text: {text}"
            );
        }
    }

    #[test]
    fn a_final_event_without_a_blank_line_is_translated_once() {
        // The stream ends without its trailing blank line. `split_events`
        // yields the leftover as one event, so it must not be parsed twice.
        let stream = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}";
        let text = translate(stream, 4096);

        assert_eq!(
            text.matches("\"content\":\"hi\"").count(),
            1,
            "the tail was translated more than once: {text}"
        );
    }

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
    fn the_thinking_level_is_renamed_for_the_responses_api() {
        // A Chat Completions client names the thinking level with
        // `reasoning_effort`. The Responses API rejects that spelling
        // outright, so it is renamed rather than dropped.
        let body = br#"{
            "model": "gpt-6.1-sol",
            "messages": [{"role": "user", "content": "hello"}],
            "reasoning_effort": "xhigh"
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["reasoning"]["effort"], "xhigh");
        // The summary is asked for, because the Chat Completions shape has
        // nowhere else to carry the thinking.
        assert_eq!(value["reasoning"]["summary"], "auto");
        // The Chat Completions spelling is never forwarded.
        assert!(value.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_responses_shaped_thinking_level_is_left_alone() {
        // A client that already speaks the Responses shape keeps its own
        // parameters.
        let body = br#"{
            "model": "gpt-6.1-sol",
            "messages": [{"role": "user", "content": "hello"}],
            "reasoning": {"effort": "low", "summary": "detailed"}
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["reasoning"]["effort"], "low");
        assert_eq!(value["reasoning"]["summary"], "detailed");
    }

    #[test]
    fn the_token_limit_is_renamed_for_the_responses_api() {
        let body = br#"{
            "model": "gpt-6.1-sol",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 100
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["max_output_tokens"], 100);
        assert!(value.get("max_tokens").is_none());
    }

    #[test]
    fn the_newer_token_limit_wins() {
        let body = br#"{
            "model": "gpt-6.1-sol",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 100,
            "max_completion_tokens": 250
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["max_output_tokens"], 250);
    }

    #[test]
    fn a_responses_shaped_token_limit_is_carried() {
        // A client that already uses the Responses spelling must keep its
        // limit; the field is Responses-valid, so there is nothing to rename.
        let body = br#"{
            "model": "gpt-6.1-sol",
            "messages": [{"role": "user", "content": "hello"}],
            "max_output_tokens": 300
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["max_output_tokens"], 300);
    }

    #[test]
    fn the_thinking_text_is_carried_in_the_chat_completions_answer() {
        // x.ai's Chat Completions answer carries the thinking in
        // `reasoning_content`, so the same field is used here.
        let body = br#"{
            "id": "resp_1",
            "model": "gpt-6.1-sol",
            "status": "completed",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "Thinking."}]},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}
            ]
        }"#;

        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert_eq!(value["choices"][0]["message"]["content"], "Done.");
        assert_eq!(
            value["choices"][0]["message"]["reasoning_content"],
            "Thinking."
        );
    }

    #[test]
    fn an_answer_without_thinking_carries_no_thinking_field() {
        let body = br#"{
            "id": "resp_1",
            "model": "gpt-6.1-sol",
            "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}]
        }"#;

        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        assert!(
            value["choices"][0]["message"]
                .get("reasoning_content")
                .is_none()
        );
    }

    #[test]
    fn the_thinking_text_is_streamed_as_it_arrives() {
        let stream = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Think\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"}}\n\n",
        );

        let mut translated = ResponsesToChatStream::new(stream.as_bytes());
        let mut out = String::new();
        let mut buffer = [0u8; 4096];
        while let Ok(count) = translated.read(&mut buffer) {
            if count == 0 {
                break;
            }
            out.push_str(&String::from_utf8_lossy(&buffer[..count]));
        }

        assert!(out.contains(r#""reasoning_content":"Think""#), "{out}");
        assert!(out.contains(r#""content":"Done""#), "{out}");
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
            "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}, "strict": true}}]
        }"#;
        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["tools"][0]["type"], "function");
        assert_eq!(value["tools"][0]["name"], "get_weather");
        assert_eq!(value["tools"][0]["strict"], true);
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
    fn a_tool_call_becomes_a_chat_tool_call() {
        // The upstream's own shape for a tool call, taken from a live answer.
        let body = br#"{
            "id": "resp_1",
            "object": "response",
            "status": "completed",
            "model": "gpt-6-luna",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc_00e0bbfd9f990eb1016af7e06f7b5887d1bbb0b1e181f9fe40",
                    "call_id": "call_6738e7ba38504b03a7da80b2893ed7b6",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Paris\"}",
                    "status": "completed"
                }
            ]
        }"#;

        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        let call = &value["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(call["id"], "call_6738e7ba38504b03a7da80b2893ed7b6");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "get_weather");
        assert_eq!(call["function"]["arguments"], "{\"city\":\"Paris\"}");
        assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn an_answer_with_no_tool_call_carries_no_tool_calls() {
        let body = br#"{"id":"resp_1","status":"completed","model":"gpt-5","output":[]}"#;
        let converted = responses_to_chat_completion(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert!(value["choices"][0]["message"].get("tool_calls").is_none());
        assert_eq!(value["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn the_tool_call_is_carried_back_into_the_conversation() {
        // The client sends the model's own tool call back with the tool's
        // answer. The Responses API expects the call to be in the conversation
        // before the answer, matched by their `call_id`.
        let body = br#"{
            "model": "gpt-6-luna",
            "messages": [
                {"role": "user", "content": "What is the weather in Paris?"},
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_6738e7ba38504b03a7da80b2893ed7b6",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_6738e7ba38504b03a7da80b2893ed7b6", "content": "18C"}
            ]
        }"#;

        let converted = chat_request_to_responses(body).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();

        let call = &value["input"][1];
        assert_eq!(call["type"], "function_call");
        assert_eq!(call["call_id"], "call_6738e7ba38504b03a7da80b2893ed7b6");
        assert_eq!(call["name"], "get_weather");
        assert_eq!(call["arguments"], "{\"city\":\"Paris\"}");

        // The tool's answer follows it.
        let output = &value["input"][2];
        assert_eq!(output["type"], "function_call_output");
        assert_eq!(output["call_id"], "call_6738e7ba38504b03a7da80b2893ed7b6");
        assert_eq!(output["output"], "18C");
    }

    #[test]
    fn a_tool_call_is_streamed_as_it_arrives() {
        // The upstream's own events for a tool call, taken from a live answer.
        let events = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-6-luna\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"\",\"status\":\"in_progress\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{\\\"city\\\":\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"\\\"Paris\\\"}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\",\"status\":\"completed\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-6-luna\",\"output\":[{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}]}}\n\n",
        );

        let mut translator = ResponsesToChatStream::new(events.as_bytes());
        let mut out = String::new();
        translator.read_to_string(&mut out).unwrap();

        // The call is announced with its id and name.
        assert!(out.contains("\"id\":\"call_1\""), "{out}");
        assert!(out.contains("\"name\":\"get_weather\""), "{out}");
        assert!(out.contains("\"type\":\"function\""), "{out}");

        // The arguments arrive in pieces, and are not sent twice: the upstream
        // sends them once in pieces and once whole.
        assert!(out.contains("\"arguments\":\"{\\\"city\\\":\""), "{out}");
        assert!(out.contains("\"arguments\":\"\\\"Paris\\\"}\""), "{out}");
        assert_eq!(out.matches("city").count(), 1, "{out}");
        assert_eq!(out.matches("Paris").count(), 1, "{out}");

        assert!(out.contains("\"finish_reason\":\"tool_calls\""), "{out}");
        assert!(out.contains("data: [DONE]"), "{out}");
    }

    #[test]
    fn a_tool_call_that_never_announced_itself_still_arrives() {
        // Some upstreams finish a call without announcing it first.
        let events = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\",\"status\":\"completed\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
        );

        let mut translator = ResponsesToChatStream::new(events.as_bytes());
        let mut out = String::new();
        translator.read_to_string(&mut out).unwrap();

        assert!(out.contains("\"id\":\"call_1\""), "{out}");
        assert!(out.contains("\"name\":\"get_weather\""), "{out}");
        assert!(out.contains("Paris"), "{out}");
        assert!(out.contains("\"finish_reason\":\"tool_calls\""), "{out}");
    }

    #[test]
    fn an_ordinary_answer_still_finishes_with_stop() {
        // A message that also names a tool is not a tool call.
        let events = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"role\":\"assistant\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"model\":\"gpt-5\"}}\n\n",
        );

        let mut translator = ResponsesToChatStream::new(events.as_bytes());
        let mut out = String::new();
        translator.read_to_string(&mut out).unwrap();

        assert!(out.contains("\"finish_reason\":\"stop\""), "{out}");
        assert!(!out.contains("tool_calls"), "{out}");
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

    #[test]
    fn a_successful_response_with_a_null_error_is_an_answer() {
        // The Responses object always carries an `error` key, and it is `null`
        // on success. Treating a present-but-null error as a failure turned
        // every successful ChatGPT answer into `content: null`.
        let body = br#"{
            "id": "resp_1",
            "model": "gpt-5.6-sol",
            "status": "completed",
            "error": null,
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "chatgpt works"}]
            }]
        }"#;

        let bytes = responses_to_chat_completion(body).expect("a chat completion");
        let value: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["object"], "chat.completion", "{value}");
        assert_eq!(
            value["choices"][0]["message"]["content"], "chatgpt works",
            "{value}"
        );
        assert_eq!(value["choices"][0]["finish_reason"], "stop", "{value}");
    }

    #[test]
    fn a_failed_response_is_an_error() {
        let body = br#"{
            "id": "resp_2",
            "status": "failed",
            "error": {"code": "server_error", "message": "it broke"}
        }"#;

        let bytes = responses_to_chat_completion(body).expect("a chat completion");
        let value: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["choices"][0]["finish_reason"], "error", "{value}");
        assert_eq!(
            value["choices"][0]["message"]["content"],
            Value::Null,
            "{value}"
        );
    }

    #[test]
    fn documented_fixture_image_is_preserved_and_image_only_messages_survive() {
        let body = include_str!("../../tests/fixtures/media/chat-completions-image.json");
        let converted = chat_request_to_responses(body.as_bytes()).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(value["input"][0]["content"][0]["type"], "input_image");
        assert_eq!(value["input"][0]["content"][0]["detail"], "high");
        assert_eq!(
            value["input"][0]["content"][0]["image_url"],
            "https://example.invalid/fixture.png"
        );
    }

    #[test]
    fn response_format_is_mapped_to_text_format() {
        for (input, expected) in [
            (json!({"type":"json_object"}), json!({"type":"json_object"})),
            (json!({"type":"text"}), json!({"type":"text"})),
            (
                json!({"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"},"strict":true,"description":"fixture"}}),
                json!({"type":"json_schema","name":"answer","schema":{"type":"object"},"strict":true,"description":"fixture"}),
            ),
        ] {
            let request = json!({"model":"fixture","messages":[],"response_format":input});
            let bytes = chat_request_to_responses(&serde_json::to_vec(&request).unwrap()).unwrap();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["text"]["format"], expected);
        }
    }

    #[test]
    fn an_unknown_response_format_type_is_named() {
        let request = br#"{"model":"fixture","messages":[],"response_format":{"type":"yaml"}}"#;
        let error = chat_request_to_responses(request).unwrap_err();
        assert_eq!(error.code(), "unsupported_parameter");
        assert!(error.message().contains("yaml"));
    }

    #[test]
    fn unsupported_fields_are_named_and_provider_rejected_fields_stay_stripped() {
        for (field, value) in [("n", json!(2)), ("stop", json!(["x"])), ("seed", json!(1))] {
            let request = json!({"model":"fixture","messages":[],(field):value});
            let error =
                chat_request_to_responses(&serde_json::to_vec(&request).unwrap()).unwrap_err();
            assert_eq!(error.code(), "unsupported_parameter");
            assert!(error.message().contains(field), "{}", error.message());
        }
        let request = br#"{"model":"fixture","messages":[],"temperature":0.2,"logprobs":false,"presence_penalty":0}"#;
        let value: Value =
            serde_json::from_slice(&chat_request_to_responses(request).unwrap()).unwrap();
        assert!(value.get("temperature").is_none());
        assert!(value.get("logprobs").is_none());
        for value in [json!(true), json!("yes")] {
            let request = json!({"model":"fixture","messages":[],"logprobs":value});
            let error =
                chat_request_to_responses(&serde_json::to_vec(&request).unwrap()).unwrap_err();
            assert_eq!(error.code(), "unsupported_parameter");
        }
    }

    #[test]
    fn generation_tools_require_the_native_responses_path() {
        for (kind, code) in [
            ("image_generation", "image_requires_responses"),
            ("audio_generation", "audio_requires_responses"),
        ] {
            let request = json!({"model":"fixture","messages":[],"tools":[{"type":kind}]});
            let error =
                chat_request_to_responses(&serde_json::to_vec(&request).unwrap()).unwrap_err();
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn non_function_tools_are_carried_unchanged() {
        let request = br#"{"model":"fixture","messages":[],"tools":[{"type":"web_search","search_context_size":"low"}]}"#;
        let value: Value =
            serde_json::from_slice(&chat_request_to_responses(request).unwrap()).unwrap();
        assert_eq!(
            value["tools"][0],
            json!({"type":"web_search","search_context_size":"low"})
        );
    }

    #[test]
    fn refusal_fixture_is_projected_as_ordered_content_parts() {
        let fixture = include_str!("../../tests/fixtures/media/responses-refusal.json");
        let value: Value =
            serde_json::from_slice(&responses_to_chat_completion(fixture.as_bytes()).unwrap())
                .unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"][0],
            json!({"type":"text","text":"I cannot help with that request."})
        );
        assert_eq!(
            value["choices"][0]["message"]["content"][1],
            json!({"type":"refusal","refusal":"The request is not supported."})
        );
    }

    #[test]
    fn image_generation_output_returns_a_named_translation_error() {
        let fixture = include_str!("../../tests/fixtures/media/responses-image-generation.json");
        let error = responses_to_chat_completion(fixture.as_bytes()).unwrap_err();
        assert_eq!(error.code(), "unmappable_output_item");
        assert!(error.message().contains("image_generation_call"));
        assert!(error.message().contains("ig_fixture_1"));
    }

    #[test]
    fn text_only_output_remains_a_string() {
        let bytes = responses_to_chat_completion(br#"{"id":"resp_1","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]}]}"#).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "hello");
    }

    #[test]
    fn refusal_stream_fixture_emits_refusal_deltas_once() {
        let fixture = include_str!("../../tests/fixtures/media/responses-stream-refusal.txt");
        let output = translate(fixture, 3);
        assert_eq!(
            output.matches(r#""refusal":"I cannot help""#).count(),
            1,
            "{output}"
        );
        assert_eq!(
            output.matches("I cannot help with that request.").count(),
            0,
            "{output}"
        );
        assert!(output.contains("data: [DONE]"), "{output}");
    }

    #[test]
    fn refusal_done_is_emitted_when_no_delta_was_seen() {
        let stream = concat!(
            "data: {\"type\":\"response.refusal.done\",\"item_id\":\"m1\",\"refusal\":\"No.\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );
        let output = translate(stream, 4096);
        assert!(output.contains(r#""refusal":"No.""#), "{output}");
    }

    #[test]
    fn unmappable_stream_with_no_chat_content_emits_named_error() {
        let stream = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"audio\",\"id\":\"audio_1\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
        );
        let output = translate(stream, 4096);
        assert!(output.contains(r#""finish_reason":"error""#), "{output}");
        assert!(
            output.contains(r#""code":"unmappable_output_item""#),
            "{output}"
        );
        assert!(output.contains("audio_1"), "{output}");
        assert!(output.contains("data: [DONE]"), "{output}");
    }

    #[test]
    fn every_advertised_image_model_is_accepted_for_every_provider_spec() {
        let catalogue: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/media/chatgpt-models.json"
        ))
        .unwrap();
        let models = catalogue["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| {
                model["input_modalities"]
                    .as_array()
                    .is_some_and(|modalities| modalities.iter().any(|value| value == "image"))
            });
        for provider in crate::provider::ProviderId::all() {
            let _spec = crate::provider::spec_for(provider, &crate::config::Config::default());
            for model in models.clone() {
                let slug = model["slug"].as_str().unwrap();
                let request = json!({"model":slug,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.invalid/image.png"}}]}]});
                assert!(
                    chat_request_to_responses(&serde_json::to_vec(&request).unwrap()).is_ok(),
                    "provider {} model {slug}",
                    provider.as_str()
                );
            }
        }
    }
}
