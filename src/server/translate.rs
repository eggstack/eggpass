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

    // A Chat Completions client names a thinking level with `reasoning_effort`
    // and a token limit with `max_tokens`. The Responses API names them
    // `reasoning.effort` and `max_output_tokens`, and rejects the Chat
    // Completions spellings outright — "Unsupported parameter: reasoning_effort"
    // — so they are renamed here rather than dropped.
    //
    // A client that already speaks the Responses shape sends `reasoning` and
    // `max_output_tokens`, and those are carried over untouched above.
    if !output.contains_key("reasoning")
        && let Some(effort) = object.get("reasoning_effort").and_then(Value::as_str)
    {
        output.insert(
            "reasoning".to_string(),
            // The summary is asked for as well, because the Chat Completions
            // shape has nowhere else to carry the thinking: the upstream only
            // returns the reasoning text when it is asked for. x.ai's Chat
            // Completions answer always carries it in `reasoning_content`, so
            // this is what makes the two providers behave the same way.
            json!({ "effort": effort, "summary": "auto" }),
        );
    }

    if !output.contains_key("max_output_tokens")
        && let Some(limit) = object
            .get("max_completion_tokens")
            .or_else(|| object.get("max_tokens"))
            .and_then(Value::as_u64)
    {
        output.insert("max_output_tokens".to_string(), json!(limit));
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
                    // An assistant message with no text carries nothing, and a
                    // message with empty text is not a message the Responses
                    // API wants. A tool call is carried on its own below.
                    if let Some(text) = content_text(content)
                        && !text.is_empty()
                    {
                        input.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        }));
                    }
                    // The model's own tool call.
                    //
                    // The Responses API expects the call to be in the
                    // conversation before the tool's answer, and it matches the
                    // two by their `call_id`. Leaving the call out leaves the
                    // upstream with a tool answer it cannot place.
                    for call in message
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(item) = convert_tool_call(call) {
                            input.push(item);
                        }
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

/// Convert one Chat Completions tool call into the Responses shape.
///
/// The Chat Completions shape nests the call under `function`; the Responses
/// shape puts it on the item itself. The `call_id` is what ties the call to
/// the tool's answer later in the conversation, so it is carried over.
fn convert_tool_call(call: &Value) -> Option<Value> {
    let function = call.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();

    Some(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

/// Convert a Responses response into a Chat Completions response.
pub fn responses_to_chat_completion(body: &[u8]) -> Result<Vec<u8>, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|err| format!("the response is not JSON: {err}"))?;

    let text = response_text(&value);
    let tool_calls = response_tool_calls(&value);
    let mut reason = finish_reason(&value);
    if !tool_calls.is_empty() {
        // The model asked for a tool rather than for an answer, and that is
        // what the finish reason says.
        reason = "tool_calls";
    }

    let mut choices = vec![json!({
        "index": 0,
        "message": { "role": "assistant", "content": text },
        "finish_reason": reason,
    })];

    // The tool calls, when the model asked for one.
    //
    // The Responses API carries a tool call as an output item; the Chat
    // Completions shape carries it in `message.tool_calls`. Without this the
    // client sees an empty answer where the model asked for a tool, and a
    // client that drives tools has nothing to run.
    if !tool_calls.is_empty() {
        choices[0]["message"]["tool_calls"] = Value::Array(tool_calls);
    }

    // The thinking text, when the upstream sent any.
    //
    // x.ai's Chat Completions answer carries it in `reasoning_content`, so the
    // same field is used here: a client that shows the thinking for one
    // provider shows it for both.
    let reasoning = response_reasoning(&value);
    if !reasoning.is_empty() {
        choices[0]["message"]["reasoning_content"] = json!(reasoning);
    }

    // A failed response carries an error rather than an answer.
    //
    // The Responses object always carries an `error` key, and it is `null` on
    // success. A key that is merely *present* is not a failure.
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
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
    buffer: String,
    pending: Vec<u8>,
    finished: bool,
    id: String,
    model: String,
    created: i64,
    usage: Option<Value>,
    tool_calls: Vec<ToolCall>,
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
            tool_calls: Vec::new(),
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
                encode_event(&chunk)
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
                let finish = if kind == "response.incomplete" {
                    "length"
                } else if self.tool_calls.is_empty() {
                    "stop"
                } else {
                    // The model asked for a tool rather than for an answer.
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
}
