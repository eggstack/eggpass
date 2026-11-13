//! The user-editable configuration file.
//!
//! The format is TOML and lives at `~/.config/grokproxy/config.toml`. A missing
//! file is not an error — the defaults are used. A malformed file *is* an error:
//! we never silently reset a file the user hand-edited.
//!
//! Unknown keys are preserved when we rewrite the file, and comments survive
//! because `grokproxy port` performs a surgical text edit rather than a
//! parse-and-reserialise round trip.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Default listen port.
pub const DEFAULT_PORT: u16 = 8484;

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
    /// The `/v1` prefix is part of the path we build, so the client is given
    /// the origin and appends `/v1/...` itself.
    pub fn base_url(self) -> &'static str {
        match self {
            Upstream::Api => "https://api.x.ai/v1",
            Upstream::Build => "https://cli-chat-proxy.grok.com/v1",
        }
    }

    /// The origin, without the `/v1` prefix.
    pub fn origin(self) -> &'static str {
        match self {
            Upstream::Api => "https://api.x.ai",
            Upstream::Build => "https://cli-chat-proxy.grok.com",
        }
    }

    /// A short human-readable name for `grokproxy status`.
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

/// The OAuth settings, all of which can be overridden in the config file.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// The public client id.
    pub client_id: String,
    /// The OAuth issuer.
    pub issuer: String,
    /// The scopes to request.
    pub scopes: Vec<String>,
    /// The `referrer` parameter sent to x.ai.
    pub referrer: String,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        OAuthConfig {
            client_id: crate::auth::DEFAULT_CLIENT_ID.to_string(),
            issuer: crate::auth::DEFAULT_ISSUER.to_string(),
            scopes: crate::auth::DEFAULT_SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            referrer: crate::auth::DEFAULT_REFERRER.to_string(),
        }
    }
}

/// The effective configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port the server listens on.
    pub port: u16,
    /// Which upstream serves requests.
    pub upstream: Upstream,
    /// Key clients must present. Empty means "accept anything".
    pub api_key: String,
    /// Log verbosity.
    pub log_level: LogLevel,
    /// Allow binding a privileged port (< 1024).
    pub allow_privileged_port: bool,
    /// The OAuth settings.
    pub oauth: OAuthConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: DEFAULT_PORT,
            upstream: Upstream::Api,
            api_key: String::new(),
            log_level: LogLevel::Info,
            allow_privileged_port: false,
            oauth: OAuthConfig::default(),
        }
    }
}

/// The template written when no configuration file exists yet.
pub const DEFAULT_CONFIG_TOML: &str = r#"# grokproxy configuration
#
# This file is plain TOML and is safe to edit by hand.

# TCP port the local API listens on. Takes effect after `grokproxy restart`.
port = 8484

# Upstream that serves model requests.
#   "api"   = https://api.x.ai/v1                    (full catalog, default)
#   "build" = https://cli-chat-proxy.grok.com/v1     (Grok Build proxy)
upstream = "api"

# Key clients must send as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
# Leave empty to accept any key — the server only ever binds to 127.0.0.1.
api_key = ""

# Log verbosity: error | warn | info | debug
log_level = "info"

# Set to true to allow binding a privileged port below 1024.
allow_privileged_port = false

# The OAuth settings. Change these only if xAI rotates them; the values below
# are the ones the official Grok CLI uses.
[oauth]
client_id = "b1a00492-073a-47ea-816f-4c329264a828"
issuer = "https://auth.x.ai"
scopes = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write"
referrer = "grokproxy"
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
    port: Option<i64>,
    upstream: Option<String>,
    api_key: Option<String>,
    log_level: Option<String>,
    allow_privileged_port: Option<bool>,
    oauth: Option<RawOAuth>,
}

/// The raw shape of the `[oauth]` table.
#[derive(Debug, Deserialize, Default)]
struct RawOAuth {
    client_id: Option<String>,
    issuer: Option<String>,
    scopes: Option<String>,
    referrer: Option<String>,
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
        if let Some(port) = raw.port {
            config.port = validate_port(port, path)?;
        }
        if let Some(upstream) = raw.upstream {
            config.upstream =
                Upstream::parse(&upstream).map_err(|message| ConfigError::Invalid {
                    path: path.to_path_buf(),
                    message,
                })?;
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
        if let Some(oauth) = raw.oauth {
            if let Some(client_id) = oauth.client_id {
                config.oauth.client_id = client_id;
            }
            if let Some(issuer) = oauth.issuer {
                config.oauth.issuer = issuer;
            }
            if let Some(scopes) = oauth.scopes {
                let scopes: Vec<String> = scopes.split_whitespace().map(str::to_string).collect();
                if !scopes.is_empty() {
                    config.oauth.scopes = scopes;
                }
            }
            if let Some(referrer) = oauth.referrer {
                config.oauth.referrer = referrer;
            }
        }
        Ok(config)
    }

    /// The configured scopes, borrowed.
    pub fn scope_references(&self) -> Vec<&str> {
        self.oauth
            .scopes
            .iter()
            .map(|scope| scope.as_str())
            .collect()
    }

    /// The OpenAI-compatible base URL clients should point at.
    pub fn api_base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
}

/// Create the configuration file with defaults if it does not exist yet.
///
/// Returns `true` when the file was created.
pub fn ensure_config_file(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    write_file(path, DEFAULT_CONFIG_TOML)?;
    Ok(true)
}

/// Change the `port` key in the configuration file, preserving everything else.
///
/// The edit is textual so that comments, key order and unknown keys survive.
pub fn set_port(path: &Path, port: u16) -> Result<(), ConfigError> {
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

    let updated = replace_port(&original, port);

    // The edit must still produce a valid configuration.
    Config::parse(&updated, path)?;

    write_file(path, &updated)
}

/// Replace the top-level `port` value in TOML text, appending the key if needed.
///
/// Only a top-level `port` is touched — a `port` inside a table belongs to
/// that table. When the key is missing it is inserted *before* the first table
/// header, because a top-level key after a table header would belong to it.
fn replace_port(text: &str, port: u16) -> String {
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
        if trimmed[..eq].trim() != "port" {
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
        line.replace_range(value_start..value_end, &port.to_string());
        return join_lines(&lines, text.ends_with('\n'));
    }

    let insert_at = first_table.unwrap_or(lines.len());
    lines.insert(insert_at, format!("port = {port}"));
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
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
    }
    std::fs::write(path, text).map_err(|source| ConfigError::Io {
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
    fn the_default_template_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        ensure_config_file(&path).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.upstream, Upstream::Api);
        assert_eq!(config.oauth.client_id, crate::auth::DEFAULT_CLIENT_ID);
        assert_eq!(config.oauth.issuer, crate::auth::DEFAULT_ISSUER);
        assert_eq!(config.oauth.referrer, crate::auth::DEFAULT_REFERRER);
        assert_eq!(config.oauth.scopes.len(), crate::auth::DEFAULT_SCOPES.len());
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
        assert_eq!(config.oauth.client_id, "other");
        assert_eq!(config.oauth.issuer, "https://example.com");
        assert_eq!(config.oauth.scopes, vec!["openid", "api:access"]);
        assert_eq!(config.oauth.referrer, "grok-build");
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
}
