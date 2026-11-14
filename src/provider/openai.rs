//! The ChatGPT provider, reached through Sign in with ChatGPT for Open Source.
//!
//! Three things make this provider different from x.ai, and all three are
//! enforced by the spec rather than by special cases scattered through the
//! code:
//!
//! * **The client id is issued.** The first sign-in presents
//!   `dynamic_agent_client`; OpenAI answers the callback with the real client
//!   id, which has to be stored and reused on every later sign-in.
//! * **Streaming is mandatory.** Every request must carry `stream: true` and
//!   `store: false`, and a list of fields is rejected outright.
//! * **The catalogue has its own shape.** The model list arrives as
//!   `{"models": [{"slug": ...}]}` rather than the usual `data` array.
//!
//! The inference endpoint is the **public** `https://api.openai.com/v1`, not
//! ChatGPT's internal backend.

use super::{CatalogueKind, NativePaths, ProviderId, ProviderSpec, RequestPolicy};

/// The registration entrypoint.
///
/// This is not the client id to store: it is what the first authorization
/// request presents, and OpenAI answers with the issued client id.
pub const ENTRY_CLIENT_ID: &str = "dynamic_agent_client";

/// The OpenAI OAuth issuer.
pub const ISSUER: &str = "https://auth.openai.com";

/// The resource the token is minted for.
pub const RESOURCE: &str = "https://api.openai.com/v1";

/// The scope set Sign in with ChatGPT asks for.
///
/// `offline_access` gives us a refresh token, `resource.invoke` and
/// `chatgpt.tokens.use.direct` are what grant the use of the ChatGPT plan.
pub const SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "resource.invoke",
    "chatgpt.tokens.use.direct",
];

/// The scope whose presence means ChatGPT plan usage is enabled.
pub const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";

/// The fields the Sign in with ChatGPT route rejects outright.
///
/// Passing one of these through fails the request, so they are removed from
/// the body instead.
///
/// `max_output_tokens` is **not** in this list: the route accepts it, and a
/// client that sets a token limit means it.
pub const REJECTED_FIELDS: &[&str] = &[
    "background",
    "conversation",
    "max_tool_calls",
    "metadata",
    "moderation",
    "multi_agent",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

/// The upstream model fields that are carried into the public catalogue.
///
/// The upstream's catalogue says a great deal about each model — which
/// thinking levels it offers, how large its context is, whether it takes
/// images — and a client that reads the catalogue is how it learns what a
/// model can do. Throwing that away leaves a client with nothing but a name,
/// which is how a thinking level ends up not being offered at all.
///
/// The list is an allowlist on purpose. The upstream also carries the model's
/// own instructions in `base_instructions` and `model_messages` — tens of
/// kilobytes of internal prompt text that has no business in a catalogue a
/// client can read.
pub const CATALOGUE_FIELDS: &[&str] = &[
    // Identity.
    "display_name",
    "description",
    // Thinking.
    "supported_reasoning_levels",
    "default_reasoning_level",
    "supports_reasoning_effort_updates",
    "supports_reasoning_summaries",
    "supports_reasoning_summary_parameter",
    "multi_agent_reasoning_effort",
    // Limits.
    "context_window",
    "max_context_window",
    "truncation_policy",
    // Verbosity.
    "default_verbosity",
    "support_verbosity",
    // Shape.
    "input_modalities",
    "tool_mode",
    "shell_type",
    "experimental_supported_tools",
    // Availability.
    "supported_in_api",
    "priority",
    "service_tiers",
    "additional_speed_tiers",
];

/// Generate the stable identifier for this host.
///
/// Sign in with ChatGPT wants a host identifier that is stable for the life of
/// the installation and distinct from any user-identifying value. A UUIDv4 in
/// a `urn:uuid:` URI is the supported alternative to a JWK thumbprint.
pub fn generate_host_id() -> String {
    use rand::Rng;

    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    // Version 4, variant 1.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The origin of the API a resource names.
///
/// The inference path always carries the `/v1` prefix, so the base URL has to
/// be the origin. A base URL that ended in `/v1` would send every request to
/// `/v1/v1/responses`.
pub fn api_origin(resource: &str) -> String {
    let trimmed = resource.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
}

/// Build the ChatGPT provider spec from the configuration.
pub fn spec(config: &crate::config::Config) -> ProviderSpec {
    let oauth = &config.oauth.openai;
    let resource = oauth.resource.clone();

    let mut extra_authorize_params = vec![("resource".to_string(), resource.clone())];

    // The host id identifies this installation. It is required on every
    // authorization request, and it is the same for every sign-in on this
    // machine.
    if let Some(host_id) = &oauth.ext_agent_host_id {
        extra_authorize_params.push(("ext_agent_host_id".to_string(), host_id.clone()));
    }

    // The agent name belongs to the registration request only: once the
    // client has been registered, the issued client id is reused and the hint
    // is left out.
    let registration_params = vec![("agent_name_hint".to_string(), oauth.agent_name_hint.clone())];

    ProviderSpec {
        id: ProviderId::OpenAi,
        display_name: "ChatGPT",
        issuer: oauth.issuer.clone(),
        authorize_path: "/api/accounts/authorize".to_string(),
        token_path: "/api/accounts/oauth/token".to_string(),
        device_path: None,
        userinfo_path: Some("/api/accounts/oauth/userinfo".to_string()),
        revocation_path: Some("/api/accounts/oauth/revoke".to_string()),
        callback_path: "/auth/callback",
        entry_client_id: ENTRY_CLIENT_ID.to_string(),
        scopes: oauth.scopes.clone(),
        extra_authorize_params,
        registration_params,
        extra_token_params: vec![("resource".to_string(), resource.clone())],
        base_url: api_origin(&resource),
        extra_headers: Vec::new(),
        policy: RequestPolicy {
            strip_fields: REJECTED_FIELDS,
            force_stream: true,
            force_store_false: true,
            require_input_array: true,
        },
        supports_device_flow: false,
        // Sign in with ChatGPT issues a client id of its own and returns it in
        // the callback, so a pasted code would carry no client id to exchange.
        issues_client_id: true,
        trusted_device_hosts: &[],
        catalogue: CatalogueKind::SlugArray,
        native_paths: NativePaths::ResponsesOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_origin_has_no_version_prefix() {
        // The request path carries the `/v1` prefix itself, so the base URL
        // must not carry it too.
        assert_eq!(
            api_origin("https://api.openai.com/v1"),
            "https://api.openai.com"
        );
        assert_eq!(
            api_origin("https://api.openai.com/v1/"),
            "https://api.openai.com"
        );
        assert_eq!(
            api_origin("https://api.openai.com"),
            "https://api.openai.com"
        );
    }

    #[test]
    fn the_spec_sends_requests_to_the_origin() {
        let config = crate::config::Config::default();
        let spec = spec(&config);
        assert_eq!(spec.base_url, "https://api.openai.com");

        // The path carries the prefix, so the two together name one version.
        let target = crate::upstream::build_path("/v1/responses", "");
        assert_eq!(
            format!("{}{target}", spec.base_url),
            "https://api.openai.com/v1/responses"
        );
    }
}
