//! Path routing.

use crate::upstream::normalize_path;

/// A route the server understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// `POST /v1/chat/completions`
    ChatCompletions,
    /// `POST /v1/responses`
    Responses,
    /// `GET /v1/models`
    Models,
    /// `GET /v1/models/{id}`
    Model(String),
    /// `GET /v1/last-inference`
    LastInference,
    /// `GET /health`
    Health,
    /// Anything else.
    NotFound,
}

/// Work out which route a request path names.
///
/// Both `/chat/completions` and `/v1/chat/completions` reach the same place,
/// because clients disagree about whether the base URL already carries `/v1`.
pub fn route(path: &str) -> Route {
    match path {
        "/health" | "/healthz" => return Route::Health,
        _ => {}
    }

    let normalized = normalize_path(path);
    match normalized.as_str() {
        "/v1/chat/completions" => Route::ChatCompletions,
        "/v1/responses" => Route::Responses,
        "/v1/models" => Route::Models,
        "/v1/last-inference" => Route::LastInference,
        other => match other.strip_prefix("/v1/models/") {
            Some(id) if !id.is_empty() => Route::Model(id.to_string()),
            _ => Route::NotFound,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_known_routes_are_recognised() {
        assert_eq!(route("/v1/chat/completions"), Route::ChatCompletions);
        assert_eq!(route("/chat/completions"), Route::ChatCompletions);
        assert_eq!(route("/v1/responses"), Route::Responses);
        assert_eq!(route("/responses"), Route::Responses);
        assert_eq!(route("/v1/models"), Route::Models);
        assert_eq!(
            route("/v1/models/grok-4.7"),
            Route::Model("grok-4.7".into())
        );
        assert_eq!(route("/health"), Route::Health);
        assert_eq!(route("/healthz"), Route::Health);
        assert_eq!(route("/v1/last-inference"), Route::LastInference);
        assert_eq!(route("/last-inference"), Route::LastInference);
    }

    #[test]
    fn unknown_paths_are_not_found() {
        assert_eq!(route("/v1/embeddings"), Route::NotFound);
        assert_eq!(route("/"), Route::NotFound);
        assert_eq!(route("/v1/models/"), Route::NotFound);
    }
}
