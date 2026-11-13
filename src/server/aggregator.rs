//! Collapsing an event stream into a single response body.
//!
//! Sign in with ChatGPT requires `stream: true` on every request. A client
//! that asked for a non-streaming answer therefore gets an event stream
//! upstream and one JSON body back, which is what this module does.
//!
//! The Responses API says the request succeeded only when a
//! `response.completed` event arrives, so that event is what the aggregator
//! waits for. A `response.failed` event is an error, not an answer.

use std::io::Read;

use serde_json::Value;

/// The outcome of aggregating a stream.
#[derive(Debug)]
pub enum Aggregated {
    /// The completed response object.
    Completed(Value),
    /// The response failed.
    Failed {
        /// The error code, when the provider gave one.
        code: Option<String>,
        /// The error message.
        message: String,
    },
    /// The stream ended without a terminal event.
    Incomplete,
}

/// Collapse a Responses event stream into its final response object.
pub fn aggregate_responses(body: impl Read) -> Result<Aggregated, String> {
    let mut text = String::new();
    let mut reader = std::io::BufReader::new(body);
    let mut buffer = [0u8; 8192];

    let mut outcome = Aggregated::Incomplete;
    let mut saw_event = false;

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("could not read the upstream stream: {err}"))?;
        if read == 0 {
            break;
        }
        text.push_str(&String::from_utf8_lossy(&buffer[..read]));

        // Everything up to the last blank line is complete; the rest is an
        // event that has not arrived in full yet, so it stays for the next
        // read.
        let complete = match text.rfind("\n\n") {
            Some(index) => {
                let complete = text[..index + 2].to_string();
                text = text[index + 2..].to_string();
                complete
            }
            None => String::new(),
        };

        if complete.contains("data:") || complete.contains("event:") {
            saw_event = true;
        }

        for event in split_events(&complete) {
            if let Some(value) = parse_event(&event)
                && let Some(result) = classify(&value)
            {
                outcome = result;
            }
        }
    }

    // The last event may not have been separated by a blank line.
    let rest = std::mem::take(&mut text);
    if !rest.trim().is_empty()
        && let Some(value) = parse_event(&rest)
        && let Some(result) = classify(&value)
    {
        saw_event = true;
        outcome = result;
    }

    if !saw_event {
        // The upstream did not send an event stream at all. It sent a plain
        // body: either a response object, or an error object — a refusal or a
        // rejected request, which arrives as JSON even though a stream was
        // asked for.
        let rest = rest.trim();
        if rest.is_empty() {
            return Ok(Aggregated::Incomplete);
        }
        let value: Value = serde_json::from_str(rest).map_err(|err| {
            format!("the upstream response was neither an event stream nor JSON: {err}")
        })?;
        return Ok(classify_plain(&value));
    }

    Ok(outcome)
}

/// Classify a body that arrived as one JSON object rather than as a stream.
fn classify_plain(value: &Value) -> Aggregated {
    // The Responses object always carries an `error` key, and it is `null` on
    // success. A key that is merely *present* is not a failure.
    let Some(error) = value.get("error").filter(|error| !error.is_null()) else {
        return Aggregated::Completed(value.clone());
    };
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the request failed")
        .to_string();
    Aggregated::Failed { code, message }
}

/// Turn one event into an outcome, when it is a terminal one.
fn classify(event: &Value) -> Option<Aggregated> {
    match event.get("type").and_then(Value::as_str)? {
        "response.completed" => {
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            Some(Aggregated::Completed(response))
        }
        "response.incomplete" => {
            let response = event.get("response").cloned().unwrap_or(Value::Null);
            Some(Aggregated::Completed(response))
        }
        "response.failed" => {
            let error = event
                .get("response")
                .and_then(|response| response.get("error"));
            let code = error
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let message = error
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the request failed")
                .to_string();
            Some(Aggregated::Failed { code, message })
        }
        _ => None,
    }
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
    fn a_completed_stream_collapses_into_the_response() {
        let stream = concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\"}}\n\n",
        );
        let Aggregated::Completed(value) = aggregate_responses(stream.as_bytes()).unwrap() else {
            panic!("expected a completed response")
        };
        assert_eq!(value["id"], "resp_1");
    }

    #[test]
    fn a_failed_stream_is_an_error() {
        let stream = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"no quota\"}}}\n\n";
        let Aggregated::Failed { code, message } = aggregate_responses(stream.as_bytes()).unwrap()
        else {
            panic!("expected a failure")
        };
        assert_eq!(
            code.as_deref(),
            Some("subscription_sharing_usage_limit_exceeded")
        );
        assert_eq!(message, "no quota");
    }

    #[test]
    fn a_stream_without_a_terminal_event_is_incomplete() {
        let stream = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n";
        let result = aggregate_responses(stream.as_bytes()).unwrap();
        assert!(matches!(result, Aggregated::Incomplete));
    }

    #[test]
    fn an_empty_stream_is_incomplete() {
        let result = aggregate_responses(&b""[..]).unwrap();
        assert!(matches!(result, Aggregated::Incomplete));
    }

    #[test]
    fn a_terminal_event_at_the_very_end_is_found() {
        let stream = "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_9\"}}";
        let Aggregated::Completed(value) = aggregate_responses(stream.as_bytes()).unwrap() else {
            panic!("expected a completed response")
        };
        assert_eq!(value["id"], "resp_9");
    }

    #[test]
    fn a_plain_response_body_is_taken_as_the_answer() {
        // The upstream does not always send a stream, even when one was
        // asked for. A plain response object is the answer.
        let body = "{\"id\":\"resp_10\",\"status\":\"completed\"}";
        let Aggregated::Completed(value) = aggregate_responses(body.as_bytes()).unwrap() else {
            panic!("expected a completed response")
        };
        assert_eq!(value["id"], "resp_10");
    }

    #[test]
    fn a_plain_error_body_is_a_failure() {
        let body = "{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"no quota\"}}";
        let Aggregated::Failed { code, message } = aggregate_responses(body.as_bytes()).unwrap()
        else {
            panic!("expected a failure")
        };
        assert_eq!(
            code.as_deref(),
            Some("subscription_sharing_usage_limit_exceeded")
        );
        assert_eq!(message, "no quota");
    }

    #[test]
    fn a_body_that_is_neither_a_stream_nor_json_is_an_error() {
        let err = aggregate_responses(b"<html>nope</html>".as_slice()).unwrap_err();
        assert!(err.contains("neither an event stream nor JSON"), "{err}");
    }
}
