//! Chat Completions content parts, in the Responses shape.
//!
//! This module is the only place a content part's shape is understood.

use super::translate::TranslateError;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Developer,
    Assistant,
    System,
}

/// Accept a string or part array so content is not flattened before routing.
/// Unsupported content shapes return `unsupported_content_part`.
pub fn parse_chat_content(content: Option<&Value>) -> Result<Vec<Value>, TranslateError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    match content {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(vec![json!({"type":"text", "text":text})]),
        Value::Array(parts) => parts.iter().map(parse_part).collect(),
        other => Err(TranslateError::new(
            "unsupported_content_part",
            format!(
                "content must be a string or array, got {}",
                value_kind(other)
            ),
        )),
    }
}

fn parse_part(part: &Value) -> Result<Value, TranslateError> {
    if let Some(text) = part.as_str() {
        return Ok(json!({"type":"text", "text":text}));
    }
    let Some(object) = part.as_object() else {
        return Err(unsupported_part("typeless"));
    };
    if object.get("type").and_then(Value::as_str).is_none() {
        return Err(unsupported_part("typeless"));
    }
    Ok(part.clone())
}

/// Preserve each supported part in the role-specific Responses shape.
/// Parts without a faithful mapping return `unsupported_content_part`.
pub fn to_responses_parts(
    role: Role,
    content: Option<&Value>,
) -> Result<Vec<Value>, TranslateError> {
    let text_type = if role == Role::Assistant {
        "output_text"
    } else {
        "input_text"
    };
    parse_chat_content(content)?
        .iter()
        .map(|part| {
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("typeless");
            match kind {
                "text" => {
                    let text = part
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| unsupported_part("text (missing string text)"))?;
                    Ok(json!({"type":text_type,"text":text}))
                }
                "image_url" => {
                    let image = part
                        .get("image_url")
                        .ok_or_else(|| unsupported_part("image_url (missing image_url)"))?;
                    let (url, detail) = match image {
                        Value::String(url) => (url.clone(), part.get("detail").cloned()),
                        Value::Object(image) => {
                            let url = image
                                .get("url")
                                .and_then(Value::as_str)
                                .ok_or_else(|| unsupported_part("image_url (missing string url)"))?
                                .to_string();
                            let detail =
                                part.get("detail").or_else(|| image.get("detail")).cloned();
                            (url, detail)
                        }
                        _ => return Err(unsupported_part("image_url (invalid shape)")),
                    };
                    let mut converted = json!({"type":"input_image", "image_url":url});
                    if let Some(detail) = detail {
                        converted["detail"] = detail;
                    }
                    Ok(converted)
                }
                "input_image" | "input_audio" | "input_file" => Ok(part.clone()),
                "file" => {
                    let file = part
                        .get("file")
                        .and_then(Value::as_object)
                        .ok_or_else(|| unsupported_part("file (missing file object)"))?;
                    let mut converted = json!({"type":"input_file"});
                    if let Some(file_id) = file.get("file_id") {
                        converted["file_id"] = file_id.clone();
                    } else if let (Some(filename), Some(file_data)) =
                        (file.get("filename"), file.get("file_data"))
                    {
                        converted["filename"] = filename.clone();
                        converted["file_data"] = file_data.clone();
                    } else {
                        return Err(unsupported_part(
                            "file (requires file_id or filename and file_data)",
                        ));
                    }
                    Ok(converted)
                }
                "refusal" if role == Role::Assistant => {
                    let refusal = part
                        .get("refusal")
                        .and_then(Value::as_str)
                        .ok_or_else(|| unsupported_part("refusal (missing string refusal)"))?;
                    Ok(json!({"type":"refusal", "refusal":refusal}))
                }
                other => Err(unsupported_part(other)),
            }
        })
        .collect()
}

/// Flatten text-only system or tool content where Responses needs a string.
/// Any non-text part returns `unsupported_content_part`.
pub fn content_text(content: Option<&Value>) -> Result<Option<String>, TranslateError> {
    let parts = to_responses_parts(Role::System, content)?;
    let mut text = Vec::with_capacity(parts.len());
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("input_text") {
            let kind = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("typeless");
            return Err(unsupported_part(kind));
        }
        text.push(
            part.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| unsupported_part("text (missing string text)"))?
                .to_string(),
        );
    }
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text.join("\n")))
    }
}

fn unsupported_part(kind: &str) -> TranslateError {
    TranslateError::new(
        "unsupported_content_part",
        format!("unsupported content part type {kind}"),
    )
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::Object(_) => "object",
        Value::Array(_) => "array",
        Value::Null => "null",
        Value::String(_) => "string",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn string_content_becomes_a_text_part() {
        assert_eq!(
            to_responses_parts(Role::User, Some(&json!("hello"))).unwrap(),
            vec![json!({"type":"input_text","text":"hello"})]
        );
    }
    #[test]
    fn image_only_content_is_preserved() {
        let content = json!([{"type":"image_url","image_url":{"url":"https://example.invalid/image.png","detail":"high"}}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&content)).unwrap(),
            vec![
                json!({"type":"input_image","image_url":"https://example.invalid/image.png","detail":"high"})
            ]
        );
    }
    #[test]
    fn file_and_audio_parts_are_preserved() {
        let file = json!([{"type":"file","file":{"filename":"doc.pdf","file_data":"data:application/pdf;base64,AA=="}}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&file)).unwrap()[0],
            json!({"type":"input_file","filename":"doc.pdf","file_data":"data:application/pdf;base64,AA=="})
        );
        let audio = json!([{"type":"input_audio","input_audio":{"data":"AA==","format":"wav"}}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&audio)).unwrap()[0],
            audio[0]
        );
    }
    #[test]
    fn unknown_types_are_named() {
        let error = to_responses_parts(Role::User, Some(&json!([{"type":"video"}]))).unwrap_err();
        assert_eq!(error.code(), "unsupported_content_part");
        assert!(error.message().contains("video"));
    }
    #[test]
    fn image_string_detail_and_responses_image_are_carried() {
        let from_chat = json!([{"type":"image_url","image_url":"https://example.invalid/img.png","detail":"low"}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&from_chat)).unwrap()[0],
            json!({"type":"input_image","image_url":"https://example.invalid/img.png","detail":"low"})
        );
        let native = json!([{"type":"input_image","image_url":"https://example.invalid/native.png","detail":"auto"}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&native)).unwrap(),
            native.as_array().unwrap().clone()
        );
    }

    #[test]
    fn file_id_assistant_refusal_and_responses_file_are_mapped() {
        let file = json!([{"type":"file","file":{"file_id":"file_fixture"}}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&file)).unwrap()[0],
            json!({"type":"input_file","file_id":"file_fixture"})
        );
        let input_file = json!([{"type":"input_file","file_id":"file_fixture"}]);
        assert_eq!(
            to_responses_parts(Role::User, Some(&input_file)).unwrap(),
            input_file.as_array().unwrap().clone()
        );
        let refusal = json!([{"type":"refusal","refusal":"No."}]);
        assert_eq!(
            to_responses_parts(Role::Assistant, Some(&refusal)).unwrap()[0],
            refusal[0]
        );
        assert_eq!(
            to_responses_parts(Role::Developer, Some(&json!("directive"))).unwrap()[0]["type"],
            "input_text"
        );
        assert_eq!(
            to_responses_parts(Role::Assistant, Some(&json!("reply"))).unwrap()[0]["type"],
            "output_text"
        );
    }

    #[test]
    fn text_content_can_be_flattened_but_images_cannot() {
        let text = json!(["first", {"type":"text","text":"second"}]);
        assert_eq!(
            content_text(Some(&text)).unwrap().as_deref(),
            Some("first\nsecond")
        );
        let image = json!([{"type":"input_image"}]);
        assert_eq!(
            content_text(Some(&image)).unwrap_err().code(),
            "unsupported_content_part"
        );
    }
}
