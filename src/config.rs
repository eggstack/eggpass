//! The user-editable configuration file.
//!
//! The format is TOML and lives at `~/.config/eggpass/config.toml`. A missing
//! file is not an error — the defaults are used. A malformed file *is* an error:
//! we never silently reset a file the user hand-edited.
//!
//! Unknown keys are preserved when we rewrite the file, and comments survive
//! because `eggpass port` and `eggpass host` perform a surgical text edit
//! rather than a parse-and-reserialise round trip.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::provider::ProviderId;

/// Default listen port.
pub const DEFAULT_PORT: u16 = 8484;

/// Default bind address.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// The upstream that serves model requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// `https://api.x.ai/v1` — the full x.ai catalog.
    Api,
    /// `https://cli-chat-proxy.grok.com/v1` — the Grok Build proxy.
    Build,
}

impl Upstream {
    /// The base URL requests are sent to.
    ///
    /// The origin, without the `/v1` prefix.
    ///
    /// The request path always carries the `/v1` prefix, so the base URL has
    /// to be the origin. A base URL that ended in `/v1` would send every
    /// request to `/v1/v1/...`.
    pub fn origin(self) -> &'static str {
        match self {
            Upstream::Api => "https://api.x.ai",
            Upstream::Build => "https://cli-chat-proxy.grok.com",
        }
    }

    /// A short human-readable name for `eggpass status`.
    pub fn display_name(self) -> &'static str {
        match self {
            Upstream::Api => "api.x.ai",
            Upstream::Build => "cli-chat-proxy.grok.com",
        }
    }

    /// Parse the configuration-file spelling of an upstream.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "api" => Ok(Upstream::Api),
            "build" => Ok(Upstream::Build),
            other => Err(format!(
                "unknown upstream {other:?} (expected \"api\" or \"build\")"
            )),
        }
    }
}

/// Verbosity of the daemon log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    /// Parse the configuration-file spelling of a level.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "error" => Ok(LogLevel::Error),
            "warn" => Ok(LogLevel::Warn),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            other => Err(format!(
                "unknown log level {other:?} (expected \"error\", \"warn\", \"info\" or \"debug\")"
            )),
        }
    }
}

/// Normalise a bind address.
///
/// `local`, `localhost` and `loopback` all mean `127.0.0.1`. Anything else has
/// to be an IP literal: a bind address names a local interface, and resolving
/// a name would make the result depend on DNS.
pub fn normalize_host(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "local" | "localhost" | "loopback" => Ok(DEFAULT_HOST.to_string()),
        other => other
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.to_string())
            .map_err(|_| {
                format!(
                    "unknown host {value:?} (expected an IP address, or \"localhost\", \"local\" or \"loopback\")"
                )
            }),
    }
}

/// Whether a bind address is reachable from outside this machine.
pub fn is_loopback_host(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// The x.ai OAuth settings.
#[derive(Debug, Clone)]
pub struct XaiOAuth {
    /// The public client id.
    pub client_id: String,
    /// The OAuth issuer.
    pub issuer: String,
    /// The scopes to request.
    pub scopes: Vec<String>,
    /// The `referrer` parameter sent to x.ai.
    pub referrer: String,
}

impl Default for XaiOAuth {
    fn default() -> Self {
        XaiOAuth {
            client_id: crate::provider::xai::CLIENT_ID.to_string(),
            issuer: crate::provider::xai::ISSUER.to_string(),
            scopes: crate::provider::xai::SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            referrer: crate::auth::DEFAULT_REFERRER.to_string(),
        }
    }
}

/// The ChatGPT OAuth settings.
#[derive(Debug, Clone)]
pub struct OpenAiOAuth {
    /// The OAuth issuer.
    pub issuer: String,
    /// The scopes to request.
    pub scopes: Vec<String>,
    /// The name shown to the user on the consent screen.
    pub agent_name_hint: String,
    /// The resource the token is minted for.
    pub resource: String,
    /// The stable identifier for this installation.
    pub ext_agent_host_id: Option<String>,
}

impl Default for OpenAiOAuth {
    fn default() -> Self {
        OpenAiOAuth {
            issuer: crate::provider::openai::ISSUER.to_string(),
            scopes: crate::provider::openai::SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            agent_name_hint: "eggpass".to_string(),
            resource: crate::provider::openai::RESOURCE.to_string(),
            ext_agent_host_id: None,
        }
    }
}

/// The OAuth settings, per provider.
#[derive(Debug, Clone, Default)]
pub struct OAuth {
    /// x.ai.
    pub xai: XaiOAuth,
    /// ChatGPT.
    pub openai: OpenAiOAuth,
}

/// The settings for one provider.
#[derive(Debug, Clone)]
pub struct ProviderSettings {
    /// Whether the provider is used at all.
    pub enabled: bool,
    /// Which upstream serves this provider's requests.
    pub upstream: Upstream,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        ProviderSettings {
            enabled: true,
            upstream: Upstream::Api,
        }
    }
}

/// The per-provider settings.
#[derive(Debug, Clone, Default)]
pub struct Providers {
    /// x.ai.
    pub xai: ProviderSettings,
    /// ChatGPT.
    pub openai: ProviderSettings,
}

impl Providers {
    /// The settings for one provider.
    pub fn get(&self, id: ProviderId) -> &ProviderSettings {
        match id {
            ProviderId::Xai => &self.xai,
            ProviderId::OpenAi => &self.openai,
        }
    }
}

/// How a model name is mapped onto a provider.
#[derive(Debug, Clone, Default)]
pub struct Routing {
    /// The provider used when no prefix matches.
    pub default_provider: Option<ProviderId>,
    /// The prefix map, longest prefix first.
    pub prefixes: Vec<(String, ProviderId)>,
}

impl Routing {
    /// The provider that serves a model.
    ///
    /// The longest matching prefix wins. A model no prefix matches goes to the
    /// default provider.
    pub fn provider_for(&self, model: &str, fallback: ProviderId) -> ProviderId {
        let mut best: Option<(usize, ProviderId)> = None;
        for (prefix, provider) in &self.prefixes {
            if model.starts_with(prefix.as_str())
                && best.is_none_or(|(length, _)| prefix.len() > length)
            {
                best = Some((prefix.len(), *provider));
            }
        }
        best.map(|(_, provider)| provider)
            .unwrap_or_else(|| self.default_provider.unwrap_or(fallback))
    }
}

/// The upstream timeouts, in seconds.
///
/// The defaults are generous on purpose: a long ChatGPT answer can take
/// minutes to stream, and cutting a stream off mid-sentence is worse than
/// waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// How long establishing a connection may take.
    pub connect_seconds: u64,
    /// The ceiling on a whole request, in seconds.
    pub global_seconds: u64,
    /// The ceiling on reading the response body, in seconds.
    pub body_seconds: u64,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect_seconds: 30,
            global_seconds: 1_800,
            body_seconds: 1_800,
        }
    }
}

/// The limits the local HTTP server works within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    /// The largest number of simultaneous connections.
    pub max_connections: usize,
    /// How long a kept-alive connection may sit idle, in seconds.
    pub idle_timeout_seconds: u64,
    /// How long a single write may stall, in seconds.
    pub write_timeout_seconds: u64,
}

impl Default for ServerLimits {
    fn default() -> ServerLimits {
        ServerLimits {
            max_connections: crate::server::http::DEFAULT_MAX_CONNECTIONS,
            idle_timeout_seconds: crate::server::http::DEFAULT_IDLE_TIMEOUT.as_secs(),
            write_timeout_seconds: crate::server::http::DEFAULT_WRITE_TIMEOUT.as_secs(),
        }
    }
}

/// Where the releases are published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateSettings {
    /// The GitHub repository, as `owner/name`.
    pub repository: String,
    /// The release manifest's file name inside a release.
    pub manifest_name: String,
}

impl Default for UpdateSettings {
    fn default() -> UpdateSettings {
        UpdateSettings {
            repository: "eggstack/eggpass".to_string(),
            manifest_name: "eggpass-release-manifest.json".to_string(),
        }
    }
}

/// The effective configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The address the server binds to.
    pub host: String,
    /// TCP port the server listens on.
    pub port: u16,
    /// Which upstream serves requests. Kept for the single-provider layout;
    /// `providers.xai.upstream` is the authority when it is set.
    pub upstream: Upstream,
    /// Key clients must present. Empty means "accept anything".
    pub api_key: String,
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Allow binding a privileged port (< 1024).
    pub allow_privileged_port: bool,
    /// The per-provider settings.
    pub providers: Providers,
    /// How models are routed.
    pub routing: Routing,
    /// The OAuth settings.
    pub oauth: OAuth,
    /// The upstream timeouts.
    pub timeouts: Timeouts,
    /// The local server's limits.
    pub server: ServerLimits,
    /// Where the releases are published.
    pub update: UpdateSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            upstream: Upstream::Api,
            api_key: String::new(),
            log_level: LogLevel::Info,
            allow_privileged_port: false,
            providers: Providers::default(),
            routing: Routing::default(),
            oauth: OAuth {
                xai: XaiOAuth::default(),
                openai: OpenAiOAuth::default(),
            },
            timeouts: Timeouts::default(),
            server: ServerLimits::default(),
            update: UpdateSettings::default(),
        }
    }
}

/// The template written when no configuration file exists yet.
pub const DEFAULT_CONFIG_TOML: &str = r#"# eggpass configuration
#
# This file is plain TOML and is safe to edit by hand.

# The address the local API binds to. Loopback by default.
# Change it with `eggpass host`, not by editing this file.
host = "127.0.0.1"

# TCP port the local API listens on.
# Change it with `eggpass port`, not by editing this file.
port = 8484

# A key clients may send as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
# One key is required for the proxy to run. `eggpass key add` issues keys of
# its own — numbered, revocable, and hashed at rest; this setting is the older
# way, a single key you can read back.
api_key = ""

# Log verbosity: error | warn | info | debug
log_level = "info"

# Set to true to allow binding a privileged port below 1024.
allow_privileged_port = false

# The upstream timeouts, in seconds. The defaults are generous because a long
# answer can take minutes to stream.
[timeouts]
connect_seconds = 30
global_seconds = 1800
body_seconds = 1800

# The local server's limits.
[server]
max_connections = 256
idle_timeout_seconds = 120
write_timeout_seconds = 600

# Where `eggpass update` looks for releases. The repository publishes a
# release manifest beside the binaries it names.
[update]
repository = "eggstack/eggpass"
manifest_name = "eggpass-release-manifest.json"

# Which upstream serves x.ai requests.
#   "api"   = https://api.x.ai/v1                    (full catalog, default)
#   "build" = https://cli-chat-proxy.grok.com/v1     (Grok Build proxy)
[providers.xai]
enabled = true
upstream = "api"

# ChatGPT, reached through Sign in with ChatGPT for Open Source.
[providers.openai]
enabled = true

# Which provider serves a model, and the prefix map used to decide.
[routing]
default_provider = "xai"

[routing.prefixes]
"grok-" = "xai"
"gpt-" = "openai"
"codex" = "openai"

# The x.ai OAuth settings. Change these only if xAI rotates them; the values
# below are the ones the official Grok CLI uses.
[oauth.xai]
client_id = "b1a00492-073a-47ea-816f-4c329264a828"
issuer = "https://auth.x.ai"
scopes = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write"
referrer = "eggpass"

# The ChatGPT OAuth settings. `ext_agent_host_id` identifies this installation
# and is created on the first sign-in if it is not set.
[oauth.openai]
issuer = "https://auth.openai.com"
scopes = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
agent_name_hint = "eggpass"
resource = "https://api.openai.com/v1"
"#;

/// Errors produced while reading or writing the configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read or written.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not valid TOML, or holds a value we cannot use.
    Parse {
        path: PathBuf,
        line: Option<usize>,
        column: Option<usize>,
        message: String,
    },
    /// A value in the file is out of range.
    Invalid { path: PathBuf, message: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            ConfigError::Parse {
                path,
                line,
                column,
                message,
            } => {
                write!(f, "{}: ", path.display())?;
                match (line, column) {
                    (Some(line), Some(column)) => {
                        write!(f, "line {line}, column {column}: {message}")
                    }
                    (Some(line), None) => write!(f, "line {line}: {message}"),
                    _ => write!(f, "{message}"),
                }
            }
            ConfigError::Invalid { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The raw shape of the file, before validation.
#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    host: Option<String>,
    port: Option<i64>,
    upstream: Option<String>,
    api_key: Option<String>,
    log_level: Option<String>,
    allow_privileged_port: Option<bool>,
    providers: Option<RawProviders>,
    routing: Option<RawRouting>,
    oauth: Option<RawOAuth>,
    timeouts: Option<RawTimeouts>,
    server: Option<RawServer>,
    update: Option<RawUpdate>,
}

/// The raw shape of the `[update]` table.
#[derive(Debug, Deserialize, Default)]
struct RawUpdate {
    repository: Option<String>,
    manifest_name: Option<String>,
}

/// The raw shape of the `[timeouts]` table.
#[derive(Debug, Deserialize, Default)]
struct RawTimeouts {
    connect_seconds: Option<u64>,
    global_seconds: Option<u64>,
    body_seconds: Option<u64>,
}

/// The raw shape of the `[server]` table.
#[derive(Debug, Deserialize, Default)]
struct RawServer {
    max_connections: Option<usize>,
    idle_timeout_seconds: Option<u64>,
    write_timeout_seconds: Option<u64>,
}

/// The raw shape of the `[providers]` tables.
#[derive(Debug, Deserialize, Default)]
struct RawProviders {
    xai: Option<RawProvider>,
    openai: Option<RawProvider>,
}

/// The raw shape of one `[providers.*]` table.
#[derive(Debug, Deserialize, Default)]
struct RawProvider {
    enabled: Option<bool>,
    upstream: Option<String>,
}

/// The raw shape of the `[routing]` table.
#[derive(Debug, Deserialize, Default)]
struct RawRouting {
    default_provider: Option<String>,
    prefixes: Option<BTreeMap<String, String>>,
}

/// The raw shape of the `[oauth]` table.
#[derive(Debug, Deserialize, Default)]
struct RawOAuth {
    // The legacy flat form, which described x.ai.
    client_id: Option<String>,
    issuer: Option<String>,
    scopes: Option<ScopesValue>,
    referrer: Option<String>,
    // The per-provider form.
    xai: Option<RawXaiOAuth>,
    openai: Option<RawOpenAiOAuth>,
}

/// The raw shape of the `[oauth.xai]` table.
#[derive(Debug, Deserialize, Default)]
struct RawXaiOAuth {
    client_id: Option<String>,
    issuer: Option<String>,
    scopes: Option<ScopesValue>,
    referrer: Option<String>,
}

/// The raw shape of the `[oauth.openai]` table.
#[derive(Debug, Deserialize, Default)]
struct RawOpenAiOAuth {
    issuer: Option<String>,
    scopes: Option<ScopesValue>,
    agent_name_hint: Option<String>,
    resource: Option<String>,
    ext_agent_host_id: Option<String>,
}

/// Scopes, written either as a space-separated string or as a list.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScopesValue {
    /// `scopes = "openid email"`
    One(String),
    /// `scopes = ["openid", "email"]`
    Many(Vec<String>),
}

impl ScopesValue {
    /// The scopes, as a list.
    fn into_vec(self) -> Vec<String> {
        match self {
            ScopesValue::One(text) => text.split_whitespace().map(str::to_string).collect(),
            ScopesValue::Many(list) => list,
        }
    }
}

impl Config {
    /// Load the configuration, falling back to defaults when the file is absent.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Self::parse(&text, path)
    }

    /// Parse configuration text, reporting the offending position on failure.
    pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(raw) => raw,
            Err(err) => {
                let (line, column) = match err.span() {
                    Some(span) => {
                        let (line, column) = line_and_column(text, span.start);
                        (Some(line), Some(column))
                    }
                    None => (None, None),
                };
                return Err(ConfigError::Parse {
                    path: path.to_path_buf(),
                    line,
                    column,
                    message: err.message().to_string(),
                });
            }
        };

        let mut config = Config::default();

        if let Some(host) = raw.host {
            config.host = normalize_host(&host).map_err(|message| ConfigError::Invalid {
                path: path.to_path_buf(),
                message,
            })?;
        }
        if let Some(port) = raw.port {
            config.port = validate_port(port, path)?;
        }
        if let Some(upstream) = raw.upstream {
            config.upstream =
                Upstream::parse(&upstream).map_err(|message| ConfigError::Invalid {
                    path: path.to_path_buf(),
                    message,
                })?;
            // The top-level `upstream` is the legacy spelling of the x.ai
            // provider's upstream.
            config.providers.xai.upstream = config.upstream;
        }
        if let Some(api_key) = raw.api_key {
            config.api_key = api_key;
        }
        if let Some(log_level) = raw.log_level {
            config.log_level =
                LogLevel::parse(&log_level).map_err(|message| ConfigError::Invalid {
                    path: path.to_path_buf(),
                    message,
                })?;
        }
        if let Some(allow) = raw.allow_privileged_port {
            config.allow_privileged_port = allow;
        }

        if let Some(timeouts) = raw.timeouts {
            if let Some(seconds) = timeouts.connect_seconds {
                config.timeouts.connect_seconds = seconds;
            }
            if let Some(seconds) = timeouts.global_seconds {
                config.timeouts.global_seconds = seconds;
            }
            if let Some(seconds) = timeouts.body_seconds {
                config.timeouts.body_seconds = seconds;
            }
        }

        if let Some(server) = raw.server {
            if let Some(max) = server.max_connections {
                config.server.max_connections = max.max(1);
            }
            if let Some(seconds) = server.idle_timeout_seconds {
                config.server.idle_timeout_seconds = seconds;
            }
            if let Some(seconds) = server.write_timeout_seconds {
                config.server.write_timeout_seconds = seconds;
            }
        }

        if let Some(update) = raw.update {
            if let Some(repository) = update.repository {
                let repository = repository.trim().trim_matches('/').to_string();
                if !repository.is_empty() {
                    config.update.repository = repository;
                }
            }
            if let Some(name) = update.manifest_name {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    config.update.manifest_name = name;
                }
            }
        }

        if let Some(providers) = raw.providers {
            if let Some(xai) = providers.xai {
                if let Some(enabled) = xai.enabled {
                    config.providers.xai.enabled = enabled;
                }
                if let Some(upstream) = xai.upstream {
                    config.providers.xai.upstream =
                        Upstream::parse(&upstream).map_err(|message| ConfigError::Invalid {
                            path: path.to_path_buf(),
                            message,
                        })?;
                }
            }
            if let Some(openai) = providers.openai
                && let Some(enabled) = openai.enabled
            {
                config.providers.openai.enabled = enabled;
            }
        }

        if let Some(routing) = raw.routing {
            if let Some(default) = routing.default_provider {
                config.routing.default_provider = Some(ProviderId::parse(&default).ok_or_else(
                    || ConfigError::Invalid {
                        path: path.to_path_buf(),
                        message: format!(
                            "unknown provider {default:?} in `routing.default_provider`"
                        ),
                    },
                )?);
            }
            if let Some(prefixes) = routing.prefixes {
                let mut mapped: Vec<(String, ProviderId)> = Vec::new();
                for (prefix, provider) in prefixes {
                    let id = ProviderId::parse(&provider).ok_or_else(|| ConfigError::Invalid {
                        path: path.to_path_buf(),
                        message: format!("unknown provider {provider:?} for prefix {prefix:?}"),
                    })?;
                    mapped.push((prefix, id));
                }
                // The longest prefix wins, so the order has to be settled.
                mapped.sort_by_key(|left| std::cmp::Reverse(left.0.len()));
                config.routing.prefixes = mapped;
            }
        }

        if let Some(oauth) = raw.oauth {
            // The legacy flat `[oauth]` table described x.ai.
            if let Some(client_id) = oauth.client_id {
                config.oauth.xai.client_id = client_id;
            }
            if let Some(issuer) = oauth.issuer {
                config.oauth.xai.issuer = issuer;
            }
            if let Some(scopes) = oauth.scopes {
                let scopes = scopes.into_vec();
                if !scopes.is_empty() {
                    config.oauth.xai.scopes = scopes;
                }
            }
            if let Some(referrer) = oauth.referrer {
                config.oauth.xai.referrer = referrer;
            }

            if let Some(xai) = oauth.xai {
                if let Some(client_id) = xai.client_id {
                    config.oauth.xai.client_id = client_id;
                }
                if let Some(issuer) = xai.issuer {
                    config.oauth.xai.issuer = issuer;
                }
                if let Some(scopes) = xai.scopes {
                    let scopes = scopes.into_vec();
                    if !scopes.is_empty() {
                        config.oauth.xai.scopes = scopes;
                    }
                }
                if let Some(referrer) = xai.referrer {
                    config.oauth.xai.referrer = referrer;
                }
            }

            if let Some(openai) = oauth.openai {
                if let Some(issuer) = openai.issuer {
                    config.oauth.openai.issuer = issuer;
                }
                if let Some(scopes) = openai.scopes {
                    let scopes = scopes.into_vec();
                    if !scopes.is_empty() {
                        config.oauth.openai.scopes = scopes;
                    }
                }
                if let Some(hint) = openai.agent_name_hint {
                    config.oauth.openai.agent_name_hint = hint;
                }
                if let Some(resource) = openai.resource {
                    config.oauth.openai.resource = resource;
                }
                if let Some(host_id) = openai.ext_agent_host_id {
                    config.oauth.openai.ext_agent_host_id = Some(host_id);
                }
            }
        }

        Ok(config)
    }

    /// The OpenAI-compatible base URL clients should point at.
    pub fn api_base_url(&self) -> String {
        format!("http://{}:{}/v1", self.host, self.port)
    }
}

/// Create the configuration file with defaults if it does not exist yet.
///
/// Returns `true` when the file was created.
pub fn ensure_config_file(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    write_file(path, DEFAULT_CONFIG_TOML)?;
    Ok(true)
}

/// Change the `port` key in the configuration file, preserving everything else.
///
/// The edit is textual so that comments, key order and unknown keys survive.
pub fn set_port(path: &Path, port: u16) -> Result<(), ConfigError> {
    set_top_level_value(path, "port", &port.to_string())
}

/// Change the `host` key in the configuration file, preserving everything else.
pub fn set_host(path: &Path, host: &str) -> Result<(), ConfigError> {
    set_top_level_value(path, "host", &format!("\"{host}\""))
}

/// Record the ChatGPT host id in the configuration file.
///
/// The key lives inside the `[oauth.openai]` table, so this is a table-aware
/// edit rather than a top-level one.
pub fn set_host_id(path: &Path, host_id: &str) -> Result<(), ConfigError> {
    ensure_config_file(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let updated = replace_in_table(&original, "oauth.openai", "ext_agent_host_id", host_id);

    // The edit must still produce a valid configuration.
    Config::parse(&updated, path)?;

    write_file(path, &updated)
}

/// Replace a key inside a named table, appending it to the table if needed.
///
/// The table is named by its dotted path, so `oauth.openai` means the
/// `[oauth.openai]` header.
fn replace_in_table(text: &str, table: &str, key: &str, value: &str) -> String {
    let header = format!("[{table}]");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();

    let mut in_table = false;
    let mut table_start: Option<usize> = None;
    let mut table_end: usize = lines.len();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            if in_table {
                table_end = index;
                break;
            }
            if trimmed.trim() == header {
                in_table = true;
                table_start = Some(index);
            }
        }
    }

    let Some(start) = table_start else {
        // The table is not there yet: add it at the end of the file.
        let mut out = text.trim_end().to_string();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("\n{header}\n{key} = \"{value}\"\n"));
        return out;
    };

    for index in (start + 1)..table_end {
        let line = &lines[index];
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            break;
        }
        let Some(eq) = find_assignment(trimmed) else {
            continue;
        };
        if trimmed[..eq].trim() != key {
            continue;
        }

        let eq_absolute = line.len() - trimmed.len() + eq;
        let after_eq = &line[eq_absolute + 1..];
        let padding = after_eq.len() - after_eq.trim_start().len();
        let value_start = eq_absolute + 1 + padding;
        let value_text = &line[value_start..];
        let value_length = comment_start(value_text).unwrap_or(value_text.len());
        let mut value_end = value_start + value_length;
        while value_end > value_start && line.as_bytes()[value_end - 1].is_ascii_whitespace() {
            value_end -= 1;
        }
        lines[index].replace_range(value_start..value_end, &format!("\"{value}\""));
        return join_lines(&lines, text.ends_with('\n'));
    }

    // The key is not in the table yet: add it at the end of the table.
    lines.insert(table_end, format!("{key} = \"{value}\""));
    join_lines(&lines, text.ends_with('\n'))
}

/// Change one top-level key in the configuration file, preserving everything else.
fn set_top_level_value(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    // Start from the documented template when there is no file yet, so the
    // result is a complete, commented configuration.
    ensure_config_file(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    // Refuse to touch a file we cannot understand.
    if !original.trim().is_empty() {
        toml::from_str::<toml::Table>(&original).map_err(|err| {
            let (line, column) = match err.span() {
                Some(span) => {
                    let (line, column) = line_and_column(&original, span.start);
                    (Some(line), Some(column))
                }
                None => (None, None),
            };
            ConfigError::Parse {
                path: path.to_path_buf(),
                line,
                column,
                message: err.message().to_string(),
            }
        })?;
    }

    let updated = replace_top_level(&original, key, value);

    // The edit must still produce a valid configuration.
    Config::parse(&updated, path)?;

    write_file(path, &updated)
}

/// Replace a top-level value in TOML text, appending the key if needed.
///
/// Only a top-level key is touched — a key inside a table belongs to that
/// table. When the key is missing it is inserted *before* the first table
/// header, because a top-level key after a table header would belong to it.
fn replace_top_level(text: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut in_table = false;
    let mut first_table: Option<usize> = None;

    for (index, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
            if first_table.is_none() {
                first_table = Some(index);
            }
            continue;
        }
        if in_table {
            continue;
        }
        let Some(eq) = find_assignment(trimmed) else {
            continue;
        };
        if trimmed[..eq].trim() != key {
            continue;
        }

        // Replace the value only, leaving the spacing around `=` alone.
        let eq_absolute = line.len() - trimmed.len() + eq;
        let after_eq = &line[eq_absolute + 1..];
        let padding = after_eq.len() - after_eq.trim_start().len();
        let value_start = eq_absolute + 1 + padding;
        let value_text = &line[value_start..];
        let value_length = comment_start(value_text).unwrap_or(value_text.len());
        let mut value_end = value_start + value_length;
        while value_end > value_start && line.as_bytes()[value_end - 1].is_ascii_whitespace() {
            value_end -= 1;
        }
        line.replace_range(value_start..value_end, value);
        return join_lines(&lines, text.ends_with('\n'));
    }

    let insert_at = first_table.unwrap_or(lines.len());
    lines.insert(insert_at, format!("{key} = {value}"));
    join_lines(&lines, text.ends_with('\n'))
}

/// Join lines back together, restoring the trailing newline if there was one.
fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut out = lines.join("\n");
    if !out.is_empty() && (trailing_newline || !out.ends_with('\n')) {
        out.push('\n');
    }
    out
}

/// Find the byte offset of the `=` in a `key = value` line.
fn find_assignment(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut quote = b'"';
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if *byte == quote {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                in_string = true;
                quote = *byte;
            }
            b'#' => return None,
            b'=' => return Some(index),
            _ => {}
        }
    }
    None
}

/// Find the offset of a trailing `#` comment inside a value.
fn comment_start(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut in_string = false;
    let mut quote = b'"';
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if *byte == quote {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                in_string = true;
                quote = *byte;
            }
            b'#' => return Some(index),
            _ => {}
        }
    }
    None
}

/// Validate a port number coming from the configuration file.
fn validate_port(port: i64, path: &Path) -> Result<u16, ConfigError> {
    if !(1..=65535).contains(&port) {
        return Err(ConfigError::Invalid {
            path: path.to_path_buf(),
            message: format!("port {port} is out of range (expected 1-65535)"),
        });
    }
    Ok(port as u16)
}

/// Write text to a path, creating the parent directory when needed.
fn write_file(path: &Path, text: &str) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, text).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    // The file may carry a hand-written `api_key`, so it is as sensitive as
    // the credential files beside it.
    crate::paths::restrict_permissions(path, 0o600).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Translate a byte offset into a 1-based line and column.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let mut line = 1usize;
    let mut column = 1usize;
    for (index, ch) in text.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("absent.toml")).unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.upstream, Upstream::Api);
        assert!(config.api_key.is_empty());
        assert_eq!(config.log_level, LogLevel::Info);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 9000\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.port, 9000);
        assert_eq!(config.upstream, Upstream::Api);
    }

    #[test]
    fn malformed_file_is_a_hard_error_with_a_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 8484\nthis is not toml\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("config.toml"), "{text}");
        assert!(text.contains("line 2"), "{text}");
    }

    #[test]
    fn out_of_range_port_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 70000\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("out of range"), "{err}");
    }

    #[test]
    fn unknown_upstream_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "upstream = \"nope\"\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("unknown upstream"), "{err}");
    }

    #[test]
    fn set_port_preserves_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# my config\nport = 8484\nupstream = \"api\"  # keep me\nfuture_key = 12\n",
        )
        .unwrap();
        set_port(&path, 9100).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"), "{text}");
        assert!(text.contains("port = 9100"), "{text}");
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("future_key = 12"), "{text}");
    }

    #[test]
    fn set_port_appends_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "upstream = \"api\"\n").unwrap();
        set_port(&path, 9100).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("port = 9100"), "{text}");
        assert!(text.contains("upstream = \"api\""), "{text}");
    }

    #[test]
    fn a_written_configuration_is_not_world_readable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        write_file(&path, "port = 8484\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    #[test]
    fn set_host_preserves_comments_and_writes_a_quoted_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# top\nhost = \"127.0.0.1\"  # loopback\nport = 8484\n",
        )
        .unwrap();
        set_host(&path, "0.0.0.0").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top"), "{text}");
        assert!(text.contains("host = \"0.0.0.0\""), "{text}");
        assert!(text.contains("# loopback"), "{text}");
        assert!(text.contains("port = 8484"), "{text}");
    }

    #[test]
    fn the_default_template_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        ensure_config_file(&path).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.upstream, Upstream::Api);
        assert_eq!(config.oauth.xai.client_id, crate::provider::xai::CLIENT_ID);
        assert_eq!(config.oauth.xai.issuer, crate::provider::xai::ISSUER);
        assert_eq!(config.oauth.xai.referrer, crate::auth::DEFAULT_REFERRER);
        assert_eq!(
            config.oauth.xai.scopes.len(),
            crate::provider::xai::SCOPES.len()
        );
        assert_eq!(config.oauth.openai.issuer, crate::provider::openai::ISSUER);
        assert_eq!(
            config.oauth.openai.resource,
            crate::provider::openai::RESOURCE
        );
    }

    #[test]
    fn the_oauth_settings_can_be_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[oauth]\nclient_id = \"other\"\nissuer = \"https://example.com\"\nscopes = \"openid api:access\"\nreferrer = \"grok-build\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.oauth.xai.client_id, "other");
        assert_eq!(config.oauth.xai.issuer, "https://example.com");
        assert_eq!(config.oauth.xai.scopes, vec!["openid", "api:access"]);
        assert_eq!(config.oauth.xai.referrer, "grok-build");
    }

    #[test]
    fn the_per_provider_oauth_tables_win_over_the_legacy_flat_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[oauth]\nclient_id = \"legacy\"\n\n[oauth.xai]\nclient_id = \"specific\"\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.oauth.xai.client_id, "specific");
    }

    #[test]
    fn scopes_may_be_written_as_a_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[oauth.openai]\nscopes = [\"openid\", \"email\"]\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.oauth.openai.scopes, vec!["openid", "email"]);
    }

    #[test]
    fn set_port_ignores_a_port_inside_a_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[server]\nport = 1234\n").unwrap();
        set_port(&path, 9100).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[server]"), "{text}");
        assert!(text.contains("port = 1234"), "{text}");
        assert!(text.contains("port = 9100"), "{text}");
    }

    #[test]
    fn hosts_are_normalised_and_validated() {
        assert_eq!(normalize_host("local").unwrap(), "127.0.0.1");
        assert_eq!(normalize_host("localhost").unwrap(), "127.0.0.1");
        assert_eq!(normalize_host("loopback").unwrap(), "127.0.0.1");
        assert_eq!(normalize_host("0.0.0.0").unwrap(), "0.0.0.0");
        assert_eq!(normalize_host("::1").unwrap(), "::1");
        assert!(normalize_host("example.com").is_err());
        assert!(normalize_host("").is_err());
    }

    #[test]
    fn loopback_is_recognised() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.53"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.1.10"));
    }

    #[test]
    fn the_longest_prefix_wins() {
        let routing = Routing {
            default_provider: Some(ProviderId::Xai),
            prefixes: vec![
                ("gpt-".to_string(), ProviderId::OpenAi),
                ("gpt-5".to_string(), ProviderId::OpenAi),
                ("grok-".to_string(), ProviderId::Xai),
            ],
        };
        assert_eq!(
            routing.provider_for("gpt-5-turbo", ProviderId::Xai),
            ProviderId::OpenAi
        );
        assert_eq!(
            routing.provider_for("gpt-4o", ProviderId::Xai),
            ProviderId::OpenAi
        );
        assert_eq!(
            routing.provider_for("grok-4.7", ProviderId::OpenAi),
            ProviderId::Xai
        );
        assert_eq!(
            routing.provider_for("mystery", ProviderId::OpenAi),
            ProviderId::Xai
        );
    }
}
