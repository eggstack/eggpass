//! Logging to the log file and to stderr.
//!
//! Two rules govern this module:
//!
//! 1. Nothing is ever logged that is not explicitly asked for — token values
//!    never reach the log in the first place.
//! 2. As a second line of defence, [`redact`] scrubs anything that *looks* like
//!    a credential out of every message before it is written.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::config::LogLevel;
use crate::timeutil;

/// Log severity, ordered from least to most verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    /// The label written into the log file.
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

struct Logger {
    file: Mutex<Option<File>>,
    level: Level,
    stderr: bool,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Start logging.
///
/// `path` is the log file; `None` disables file logging. `verbose` raises the
/// level to debug regardless of the configured level. `stderr` controls whether
/// messages are mirrored to standard error.
pub fn init(path: Option<&Path>, level: LogLevel, verbose: bool, stderr: bool) {
    let level = if verbose {
        Level::Debug
    } else {
        match level {
            LogLevel::Error => Level::Error,
            LogLevel::Warn => Level::Warn,
            LogLevel::Info => Level::Info,
            LogLevel::Debug => Level::Debug,
        }
    };

    let file = path.and_then(|path| {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new().create(true).append(true).open(path).ok()
    });

    let _ = LOGGER.set(Logger {
        file: Mutex::new(file),
        level,
        stderr,
    });
}

/// Write a message at the given level.
pub fn log(level: Level, message: &str) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    if level > logger.level {
        return;
    }

    let message = redact(message);
    let line = format!(
        "{} {:<5} {}\n",
        timeutil::format_rfc3339(timeutil::now_unix()),
        level.label(),
        message
    );

    if let Ok(mut file) = logger.file.lock()
        && let Some(file) = file.as_mut()
    {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }

    if logger.stderr {
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}

/// Log an error.
pub fn error(message: &str) {
    log(Level::Error, message);
}

/// Log a warning.
pub fn warn(message: &str) {
    log(Level::Warn, message);
}

/// Log an informational message.
pub fn info(message: &str) {
    log(Level::Info, message);
}

/// Log a debug message.
pub fn debug(message: &str) {
    log(Level::Debug, message);
}

/// Scrub credential-shaped values out of a message.
///
/// Three shapes are handled: `Bearer <token>`, `key=value` where the key names
/// a credential, and bare JWTs.
pub fn redact(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while !rest.is_empty() {
        // `Bearer <token>`
        if let Some(position) = find_ci(rest, "bearer ") {
            out.push_str(&rest[..position]);
            out.push_str("Bearer <redacted>");
            let after = &rest[position + "bearer ".len()..];
            let token_len = after
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(after.len());
            rest = &after[token_len..];
            continue;
        }

        // `access_token=…` and friends.
        if let Some((position, key_len)) = find_credential_key(rest) {
            out.push_str(&rest[..position]);
            out.push_str(&rest[position..position + key_len]);
            out.push_str("<redacted>");
            let after = &rest[position + key_len..];
            let value_len = after
                .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
                .unwrap_or(after.len());
            rest = &after[value_len..];
            continue;
        }

        // Bare JWTs.
        if let Some(position) = find_jwt(rest) {
            out.push_str(&rest[..position]);
            out.push_str("<redacted-jwt>");
            rest = &rest[position + jwt_len(rest, position)..];
            continue;
        }

        out.push_str(rest);
        break;
    }

    out
}

/// Find `needle` in `haystack`, ignoring ASCII case.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    haystack.find(&needle)
}

/// Credential-bearing parameter names we scrub.
const CREDENTIAL_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "client_secret",
    "code_verifier",
    "code_challenge",
    "api_key",
    "apikey",
    "password",
    "secret",
    "authorization",
];

/// Find the start of a credential-bearing `key=` pair.
fn find_credential_key(text: &str) -> Option<(usize, usize)> {
    let lowered = text.to_ascii_lowercase();
    for key in CREDENTIAL_KEYS {
        let mut from = 0usize;
        while let Some(offset) = lowered[from..].find(key) {
            let start = from + offset;
            let after = &text[start + key.len()..];
            let trimmed = after.trim_start_matches([' ', '\t']);
            let skip = after.len() - trimmed.len();
            if trimmed.starts_with('=') {
                return Some((start, key.len() + skip + 1));
            }
            from = start + key.len();
        }
    }
    None
}

/// Find the start of a JWT (`eyJ...`.`...`.`...`).
fn find_jwt(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index + 3 <= bytes.len() {
        if &text[index..index + 3] == "eyJ" {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The length of the JWT starting at `position`.
fn jwt_len(text: &str, position: usize) -> usize {
    let bytes = text.as_bytes();
    let mut dots = 0usize;
    let mut index = position;
    while index < bytes.len() {
        let byte = bytes[index];
        let is_token_byte =
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.';
        if !is_token_byte {
            break;
        }
        if byte == b'.' {
            dots += 1;
            if dots > 2 {
                break;
            }
        }
        index += 1;
    }
    // Trim a trailing dot.
    while index > position && bytes[index - 1] == b'.' {
        index -= 1;
    }
    index - position
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_tokens_are_scrubbed() {
        let out = redact("Authorization: Bearer abc.def.ghi");
        assert!(!out.contains("abc.def.ghi"), "{out}");
        assert!(out.contains("Bearer <redacted>"), "{out}");
    }

    #[test]
    fn credential_parameters_are_scrubbed() {
        let out = redact("access_token=abc123&refresh_token=def456");
        assert!(!out.contains("abc123"), "{out}");
        assert!(!out.contains("def456"), "{out}");
    }

    #[test]
    fn bare_jwts_are_scrubbed() {
        let out = redact("token is eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcdefg here");
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "{out}");
        assert!(out.contains("here"), "{out}");
    }

    #[test]
    fn ordinary_text_survives() {
        let text = "GET /v1/chat/completions model=grok-4.7 status=200";
        assert_eq!(redact(text), text);
    }
}
