//! The providers `eggpass` can authenticate against and route to.
//!
//! A provider is described by a [`ProviderSpec`]: where its OAuth endpoints
//! live, what it expects in a request body, how its model catalogue is shaped,
//! and which inference paths it serves natively. Everything else in the
//! program — the login flow, the credential store, the router — is written
//! against the spec rather than against a particular provider, so adding a
//! third provider is a matter of writing one more spec.

pub mod openai;
pub mod xai;

use serde_json::Value;

/// The providers this build knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    /// x.ai, reached with the public Grok CLI client id.
    Xai,
    /// ChatGPT, reached through Sign in with ChatGPT for Open Source.
    OpenAi,
}

impl ProviderId {
    /// The identifier used in configuration and on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Xai => "xai",
            ProviderId::OpenAi => "openai",
        }
    }

    /// The name shown to the user.
    pub fn display_name(self) -> &'static str {
        match self {
            ProviderId::Xai => "x.ai",
            ProviderId::OpenAi => "ChatGPT",
        }
    }

    /// Parse the configuration-file spelling of a provider.
    pub fn parse(value: &str) -> Option<ProviderId> {
        match value {
            "xai" | "x.ai" | "grok" => Some(ProviderId::Xai),
            "openai" | "chatgpt" => Some(ProviderId::OpenAi),
            _ => None,
        }
    }

    /// Every provider, in the order they are shown to the user.
    pub fn all() -> [ProviderId; 2] {
        [ProviderId::Xai, ProviderId::OpenAi]
    }
}

/// How a provider's model catalogue is shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogueKind {
    /// `{"data": [{"id": "..."}]}` — the OpenAI-compatible shape.
    DataArray,
    /// `{"models": [{"slug": "...", "visibility": "list"}]}` — the Sign in with
    /// ChatGPT shape, where `slug` is the model id.
    SlugArray,
}

/// Which inference paths a provider serves natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePaths {
    /// Both `/v1/chat/completions` and `/v1/responses`.
    Both,
    /// Only `/v1/responses`. A Chat Completions request has to be translated
    /// into a Responses request first.
    ResponsesOnly,
}

/// What a provider demands of a request body.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestPolicy {
    /// Fields the provider rejects outright. They are removed rather than
    /// passed through, because passing them through fails the request.
    pub strip_fields: &'static [&'static str],
    /// Whether the provider requires `stream: true`.
    pub force_stream: bool,
    /// Whether the provider requires `store: false`.
    pub force_store_false: bool,
    /// Whether the provider requires the conversation to arrive as an `input`
    /// array rather than a bare string.
    pub require_input_array: bool,
}

/// Everything the program needs to know about one provider.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    /// Which provider this is.
    pub id: ProviderId,
    /// The name shown to the user.
    pub display_name: &'static str,
    /// The OAuth issuer.
    pub issuer: String,
    /// The authorize endpoint, relative to the issuer.
    pub authorize_path: String,
    /// The token endpoint, relative to the issuer.
    pub token_path: String,
    /// The device-authorization endpoint, when the provider has one.
    pub device_path: Option<String>,
    /// The userinfo endpoint, when the provider has one.
    pub userinfo_path: Option<String>,
    /// The revocation endpoint, when the provider has one.
    pub revocation_path: Option<String>,
    /// The path the OAuth callback must use.
    ///
    /// Sign in with ChatGPT requires `/auth/callback` exactly; only the port
    /// may vary.
    pub callback_path: &'static str,
    /// The client id presented when no registration has happened yet.
    pub entry_client_id: String,
    /// The scopes requested.
    pub scopes: Vec<String>,
    /// Extra parameters added to the authorize request.
    pub extra_authorize_params: Vec<(String, String)>,
    /// Parameters added to the authorize request **only** when the provider is
    /// being registered for the first time.
    ///
    /// Sign in with ChatGPT wants the agent name hint on the registration
    /// request and not on later reauthorizations.
    pub registration_params: Vec<(String, String)>,
    /// Extra parameters added to every token request.
    pub extra_token_params: Vec<(String, String)>,
    /// The base URL inference is sent to.
    pub base_url: String,
    /// Headers only this provider needs.
    pub extra_headers: Vec<(&'static str, String)>,
    /// What the provider demands of a request body.
    pub policy: RequestPolicy,
    /// Whether the provider can complete a device-code grant.
    pub supports_device_flow: bool,
    /// Whether the provider issues its own client id and returns it in the
    /// callback.
    ///
    /// When it does, the authorization code can only come from the callback:
    /// a code pasted into the terminal carries no issued client id, so the
    /// token exchange would present the wrong one.
    pub issues_client_id: bool,
    /// The hosts we are willing to send a user to for device-code
    /// verification.
    pub trusted_device_hosts: &'static [&'static str],
    /// How the provider's model catalogue is shaped.
    pub catalogue: CatalogueKind,
    /// Which inference paths the provider serves natively.
    pub native_paths: NativePaths,
}

impl ProviderSpec {
    /// The revocation endpoint, when the provider has one.
    pub fn revocation_endpoint(&self) -> Option<String> {
        self.revocation_path
            .as_ref()
            .map(|path| format!("{}{}", self.issuer, path))
    }
}

/// The spec for a provider, with the configuration already applied.
pub fn spec_for(id: ProviderId, config: &crate::config::Config) -> ProviderSpec {
    match id {
        ProviderId::Xai => xai::spec(config),
        ProviderId::OpenAi => openai::spec(config),
    }
}

/// Everything needed to talk to one provider: its spec, an OAuth client, and
/// the manager that keeps its access token fresh.
pub struct ProviderRuntime {
    /// What the provider is and how it behaves.
    pub spec: ProviderSpec,
    /// The OAuth client.
    pub client: crate::auth::OAuthClient,
    /// The token manager.
    pub tokens: crate::auth::TokenManager,
}

impl ProviderRuntime {
    /// Assemble the runtime for one provider.
    pub fn new(
        id: ProviderId,
        config: &crate::config::Config,
        paths: &crate::paths::Paths,
    ) -> ProviderRuntime {
        let spec = spec_for(id, config);

        // The client id we present is the one the provider issued, when it has
        // issued one, and the entrypoint otherwise.
        let client_id = stored_client_id(paths, id).unwrap_or_else(|| spec.entry_client_id.clone());
        let client = crate::auth::OAuthClient::for_provider(&spec, &client_id);

        let tokens =
            crate::auth::TokenManager::new(paths.credential_file(id.as_str()), client.clone())
                .with_import_source(import_source(id, paths));

        ProviderRuntime {
            spec,
            client,
            tokens,
        }
    }
}

/// The client id in the stored credential, when there is one.
fn stored_client_id(paths: &crate::paths::Paths, id: ProviderId) -> Option<String> {
    let stored = crate::auth::store::load(&paths.credential_file(id.as_str())).ok()??;
    if stored.client_id.is_empty() {
        None
    } else {
        Some(stored.client_id)
    }
}

/// Where a provider's credential may be imported from.
fn import_source(id: ProviderId, paths: &crate::paths::Paths) -> Option<std::path::PathBuf> {
    match id {
        // The official Grok CLI's file is an import source for x.ai.
        ProviderId::Xai => paths.grok_cli_auth_file(),
        ProviderId::OpenAi => None,
    }
}

/// Normalise a provider's model catalogue into the OpenAI list shape.
///
/// The result is `{"object": "list", "data": [{"id": ..., "object": "model"}]}`.
pub fn normalize_catalogue(kind: CatalogueKind, body: &[u8]) -> Vec<u8> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };

    let models: Vec<Value> = match kind {
        CatalogueKind::DataArray => value
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        CatalogueKind::SlugArray => value
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
                    .map(|model| {
                        let slug = model
                            .get("slug")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let mut entry = serde_json::Map::new();
                        entry.insert("id".to_string(), serde_json::json!(slug));
                        entry.insert("object".to_string(), serde_json::json!("model"));
                        entry.insert("owned_by".to_string(), serde_json::json!("openai"));

                        // The upstream's own capability data is carried over, so
                        // a client can tell which thinking levels a model
                        // offers and how large its context is.
                        for field in openai::CATALOGUE_FIELDS {
                            if let Some(value) = model.get(*field) {
                                entry.insert((*field).to_string(), value.clone());
                            }
                        }

                        // x.ai's catalogue calls the context window
                        // `context_length`, so the merged catalogue gives both
                        // providers the same name for it.
                        if let Some(window) = model.get("context_window") {
                            entry.insert("context_length".to_string(), window.clone());
                        }

                        serde_json::Value::Object(entry)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };

    let output = serde_json::json!({
        "object": "list",
        "data": models,
    });
    serde_json::to_vec(&output).unwrap_or_else(|_| body.to_vec())
}

/// The model ids a provider's catalogue advertises.
pub fn catalogue_ids(kind: CatalogueKind, body: &[u8]) -> Vec<String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    match kind {
        CatalogueKind::DataArray => value
            .get("data")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| model.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        CatalogueKind::SlugArray => value
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
                    .filter_map(|model| model.get("slug").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_parse_from_their_config_names() {
        assert_eq!(ProviderId::parse("xai"), Some(ProviderId::Xai));
        assert_eq!(ProviderId::parse("x.ai"), Some(ProviderId::Xai));
        assert_eq!(ProviderId::parse("openai"), Some(ProviderId::OpenAi));
        assert_eq!(ProviderId::parse("chatgpt"), Some(ProviderId::OpenAi));
        assert_eq!(ProviderId::parse("nope"), None);
    }

    #[test]
    fn the_slug_catalogue_is_normalised_and_filtered() {
        let body = br#"{"models":[
            {"slug":"gpt-5","display_name":"GPT-5","visibility":"list"},
            {"slug":"secret","visibility":"hidden"}
        ]}"#;
        let normalized = normalize_catalogue(CatalogueKind::SlugArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        let data = value["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["id"], "gpt-5");
        assert_eq!(data[0]["object"], "model");
    }

    #[test]
    fn the_slug_catalogue_keeps_the_upstreams_capability_data() {
        // The upstream says which thinking levels a model offers and how large
        // its context is. A client reads the catalogue to learn that, so it has
        // to survive the normalisation.
        let body = br#"{"models":[{
            "slug":"gpt-6.1-sol",
            "display_name":"GPT-6.1-Sol",
            "visibility":"list",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
            "default_reasoning_level":"low",
            "supports_reasoning_effort_updates":true,
            "context_window":500000,
            "max_context_window":872000,
            "input_modalities":["text","image"],
            "base_instructions":"You are Codex, a very long internal prompt.",
            "model_messages":{"instructions_template":"another long prompt"}
        }]}"#;

        let normalized = normalize_catalogue(CatalogueKind::SlugArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        let model = &value["data"][0];

        assert_eq!(model["id"], "gpt-6.1-sol");
        assert_eq!(model["object"], "model");
        assert_eq!(model["owned_by"], "openai");
        assert_eq!(model["display_name"], "GPT-6.1-Sol");

        // The thinking levels, which are what a client offers the user.
        assert_eq!(model["supported_reasoning_levels"][0]["effort"], "low");
        assert_eq!(model["supported_reasoning_levels"][1]["effort"], "high");
        assert_eq!(model["default_reasoning_level"], "low");
        assert_eq!(model["supports_reasoning_effort_updates"], true);

        // The limits, under both the upstream's name and x.ai's.
        assert_eq!(model["context_window"], 500000);
        assert_eq!(model["context_length"], 500000);
        assert_eq!(model["max_context_window"], 872000);
        assert_eq!(model["input_modalities"][1], "image");

        // The upstream's own instructions are not part of a catalogue a client
        // can read.
        assert!(model.get("base_instructions").is_none());
        assert!(model.get("model_messages").is_none());
    }

    #[test]
    fn the_data_catalogue_is_normalised() {
        let body = br#"{"data":[{"id":"grok-4.7","object":"model"}]}"#;
        let normalized = normalize_catalogue(CatalogueKind::DataArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        assert_eq!(value["data"][0]["id"], "grok-4.7");
        assert_eq!(value["object"], "list");
    }

    #[test]
    fn an_unparsable_catalogue_is_left_alone() {
        let body = b"not json";
        assert_eq!(normalize_catalogue(CatalogueKind::DataArray, body), body);
    }
}
