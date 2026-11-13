//! The model catalogue.
//!
//! The catalogue is the **union** of every provider we are signed in to. Each
//! provider's own list is fetched live and normalised into the OpenAI shape,
//! so the catalogue is never stale and a model that exists upstream is never
//! missing here.
//!
//! There is no compiled-in list of model names. A model that no provider
//! advertises is a `404`, not a hopeful `200`.

use serde_json::{Value, json};

use crate::upstream::{self, Method, UpstreamError};

use super::{Request, Response, ServerContext, openai_error};

/// Handle `GET /v1/models`.
pub fn list(_request: &Request, context: &ServerContext) -> Response {
    let merged = merged_catalogue(context);

    match merged {
        Ok(Some(catalogue)) => Response::json(200, catalogue.to_string()),
        // Nothing is signed in, so there is no catalogue to show.
        Ok(None) => Response::json(200, json!({ "object": "list", "data": [] }).to_string()),
        Err(err) => openai_error(502, &err, "server_error"),
    }
}

/// Handle `GET /v1/models/{id}`.
pub fn single(_request: &Request, context: &ServerContext, id: &str) -> Response {
    let catalogue = match merged_catalogue(context) {
        Ok(Some(catalogue)) => catalogue,
        Ok(None) => {
            return openai_error(
                404,
                &format!("unknown model `{id}`"),
                "invalid_request_error",
            );
        }
        Err(err) => return openai_error(502, &err, "server_error"),
    };

    let found = catalogue["data"]
        .as_array()
        .and_then(|models| {
            models
                .iter()
                .find(|model| model.get("id").and_then(Value::as_str) == Some(id))
        })
        .cloned();

    match found {
        Some(model) => Response::json(200, model.to_string()),
        None => openai_error(
            404,
            &format!("unknown model `{id}`"),
            "invalid_request_error",
        ),
    }
}

/// The merged catalogue of every signed-in provider.
///
/// `Ok(None)` means no provider is signed in.
fn merged_catalogue(context: &ServerContext) -> Result<Option<Value>, String> {
    let mut models: Vec<Value> = Vec::new();
    let mut any = false;

    for handle in context.providers.values() {
        // A provider we are not signed in to contributes nothing.
        match handle.tokens.current() {
            Ok(Some(_)) => {}
            _ => continue,
        }

        let mut response = match upstream::send_with_auth(
            &handle.upstream,
            &handle.tokens,
            Method::Get,
            "/v1/models",
            &[],
            None,
        ) {
            Ok(response) => response,
            Err(UpstreamError::Token(_)) => continue,
            Err(err) => return Err(err.to_string()),
        };

        if response.status != 200 {
            continue;
        }
        let Ok(bytes) = response.read_to_vec() else {
            continue;
        };

        let normalised = crate::provider::normalize_catalogue(handle.spec.catalogue, &bytes);
        let Ok(value) = serde_json::from_slice::<Value>(&normalised) else {
            continue;
        };
        let Some(data) = value.get("data").and_then(Value::as_array) else {
            continue;
        };

        any = true;
        for model in data {
            // A model two providers both serve is listed once.
            let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
            if models
                .iter()
                .any(|existing| existing.get("id").and_then(Value::as_str) == Some(id))
            {
                continue;
            }
            models.push(model.clone());
        }
    }

    if !any {
        return Ok(None);
    }

    Ok(Some(json!({ "object": "list", "data": models })))
}
