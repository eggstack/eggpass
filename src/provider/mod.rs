//! The providers `eggpass` can authenticate against and route to.
//!
//! A provider is described by a [`ProviderSpec`]: where its OAuth endpoints
//! live, what it expects in a request body, how its model catalogue is shaped,
//! and which inference paths it serves natively. Everything else in the
//! program — the login flow, the credential store, the router — is written
//! against the spec rather than against a particular provider, so adding a
//! third provider is a matter of writing one more spec.

pub mod openai;
pub mod usage;
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

impl serde::Serialize for ProviderId {
    /// The provider's configuration-file spelling, so a machine-readable
    /// report names a provider the way the rest of the tool does.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
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
    /// How long a request to this provider may take.
    pub timeouts: crate::config::Timeouts,
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
            timeouts: config.timeouts,
        }
    }

    /// What this provider can say about its own allowance.
    pub fn usage(&self) -> usage::ProviderUsage {
        usage::collect(self.spec.id, self)
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

                        serde_json::Value::Object(entry)
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };

    let models: Vec<Value> = models
        .into_iter()
        .map(|mut model| {
            if let Some(entry) = model.as_object_mut() {
                add_aliases(entry);
            }
            model
        })
        .collect();

    let output = serde_json::json!({
        "object": "list",
        "data": models,
    });
    serde_json::to_vec(&output).unwrap_or_else(|_| body.to_vec())
}

/// Add the alias fields a client may read, beside the upstream's own.
///
/// Clients disagree about how a model describes itself. The OpenAI shape
/// names a model and says little else; the OpenRouter shape — which most of
/// the OpenAI-compatible ecosystem has settled on — says what the model can
/// do, under `capabilities` and `limit`.
///
/// The upstream's own fields are carried through untouched, and these are
/// added beside them, derived only from what the upstream states. A field
/// the upstream says nothing about is left out rather than guessed at: the
/// models x.ai publishes without a `capabilities` block carry no reasoning
/// flag, because saying `reasoning: false` would be a claim its own
/// catalogue does not make.
fn add_aliases(entry: &mut serde_json::Map<String, Value>) {
    // The display name, under the OpenRouter spelling.
    let display_name = entry
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(name) = display_name {
        entry
            .entry("name".to_string())
            .or_insert_with(|| Value::String(name));
    }

    // The context window, under both names.
    //
    // ChatGPT publishes it as `context_window` and x.ai as `context_length`,
    // so the merged catalogue gives both providers the same two names for it.
    let context = entry
        .get("context_window")
        .or_else(|| entry.get("context_length"))
        .cloned();
    if let Some(context) = context {
        entry
            .entry("limit".to_string())
            .or_insert_with(|| serde_json::json!({ "context": context }));
        entry.entry("context_length".to_string()).or_insert(context);
    }

    // The default thinking level, under the name ChatGPT uses.
    let default_effort = entry
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("default_reasoning_effort"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(default) = default_effort {
        entry
            .entry("default_reasoning_level".to_string())
            .or_insert_with(|| Value::String(default));
    }

    // The thinking levels, under the OpenRouter spelling.
    let levels = effort_levels(entry);
    if !levels.is_empty() {
        entry.insert("reasoning".to_string(), Value::Bool(true));

        // x.ai publishes a `capabilities` block of its own, so the alias is
        // added to it rather than over it.
        let capabilities = entry
            .entry("capabilities".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(capabilities) = capabilities.as_object_mut() {
            capabilities.insert(
                "reasoning".to_string(),
                serde_json::json!({ "supported": true, "effort": levels }),
            );
        }
    }
}

/// The thinking levels a model offers, under either provider's spelling.
///
/// ChatGPT publishes them as `supported_reasoning_levels`, a list of objects;
/// x.ai publishes them as `capabilities.reasoning_effort`, a list of strings.
fn effort_levels(entry: &serde_json::Map<String, Value>) -> Vec<String> {
    let levels: Vec<String> = entry
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if !levels.is_empty() {
        return levels;
    }

    entry
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("reasoning_effort"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
    fn the_slug_catalogue_also_answers_the_openrouter_shape() {
        // Most of the OpenAI-compatible ecosystem reads a model record the
        // OpenRouter way, so the same facts are offered under those names too.
        let body = br#"{"models":[{
            "slug":"gpt-6.1-sol",
            "display_name":"GPT-6.1 Sol",
            "visibility":"list",
            "supported_reasoning_levels":[{"effort":"low"},{"effort":"high"}],
            "context_window":500000
        }]}"#;

        let normalized = normalize_catalogue(CatalogueKind::SlugArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        let model = &value["data"][0];

        assert_eq!(model["name"], "GPT-6.1 Sol");
        assert_eq!(model["reasoning"], true);
        assert_eq!(model["limit"]["context"], 500000);
        assert_eq!(model["capabilities"]["reasoning"]["supported"], true);
        assert_eq!(model["capabilities"]["reasoning"]["effort"][0], "low");
        assert_eq!(model["capabilities"]["reasoning"]["effort"][1], "high");

        // The upstream's own spelling is still there beside the alias.
        assert_eq!(model["supported_reasoning_levels"][0]["effort"], "low");
        assert_eq!(model["context_window"], 500000);
    }

    #[test]
    fn the_data_catalogue_is_given_the_limit_alias() {
        // x.ai publishes the context window as `context_length`, and publishes
        // no thinking levels for this model, so it gets the limit and nothing
        // else.
        let body = br#"{"data":[{
            "id":"grok-build-0.1",
            "object":"model",
            "owned_by":"xai",
            "context_length":256000
        }]}"#;

        let normalized = normalize_catalogue(CatalogueKind::DataArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        let model = &value["data"][0];

        assert_eq!(model["limit"]["context"], 256000);
        assert_eq!(model["context_length"], 256000);

        // Nothing is claimed that x.ai does not publish.
        assert!(model.get("capabilities").is_none());
        assert!(model.get("reasoning").is_none());
        assert!(model.get("name").is_none());
    }

    #[test]
    fn the_data_catalogue_keeps_xais_own_capabilities() {
        // x.ai publishes its thinking levels inside a `capabilities` block of
        // its own. The alias is added beside them, not over them.
        let body = br#"{"data":[{
            "id":"grok-4.7",
            "object":"model",
            "owned_by":"xai",
            "context_length":500000,
            "capabilities":{
                "reasoning_effort":["low","medium","high","xhigh"],
                "default_reasoning_effort":"high"
            }
        }]}"#;

        let normalized = normalize_catalogue(CatalogueKind::DataArray, body);
        let value: Value = serde_json::from_slice(&normalized).unwrap();
        let model = &value["data"][0];

        // x.ai's own fields are untouched.
        assert_eq!(model["capabilities"]["reasoning_effort"][0], "low");
        assert_eq!(model["capabilities"]["default_reasoning_effort"], "high");
        assert_eq!(model["default_reasoning_level"], "high");

        // And the OpenRouter shape is answered as well.
        assert_eq!(model["reasoning"], true);
        assert_eq!(model["capabilities"]["reasoning"]["supported"], true);
        assert_eq!(model["capabilities"]["reasoning"]["effort"][3], "xhigh");
        assert_eq!(model["limit"]["context"], 500000);
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
