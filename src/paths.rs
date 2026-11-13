//! Filesystem locations used by `grokproxy`.
//!
//! Resolution order for the configuration directory:
//!
//! 1. `$GROKPROXY_HOME` when set — relocates *every* file (config, credentials,
//!    pid, log). Useful for tests and for running a second instance.
//! 2. `$XDG_CONFIG_HOME/grokproxy` when set.
//! 3. `~/.config/grokproxy`.
//!
//! The `--config <path>` flag overrides only the configuration *file*; the
//! credential, pid and log files stay in the resolved directory.

use std::path::{Path, PathBuf};

/// Name of the user-editable configuration file.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Name of the OAuth credential store.
pub const AUTH_FILE_NAME: &str = "auth.json";
/// Name of the daemon pid file.
pub const PID_FILE_NAME: &str = "grokproxy.pid";
/// Name of the daemon log file.
pub const LOG_FILE_NAME: &str = "grokproxy.log";

/// Directory holding the official Grok Build CLI's credential file.
pub const GROK_CLI_DIR: &str = ".grok";
/// Name of the official Grok Build CLI's credential file.
pub const GROK_CLI_AUTH_FILE_NAME: &str = "auth.json";

/// Errors produced while resolving paths.
#[derive(Debug)]
pub enum PathError {
    /// No home directory could be determined.
    NoHomeDirectory,
    /// A path could not be created or accessed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::NoHomeDirectory => write!(
                f,
                "cannot determine the home directory; set $GROKPROXY_HOME or $XDG_CONFIG_HOME"
            ),
            PathError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for PathError {}

/// The set of files `grokproxy` reads and writes.
#[derive(Debug, Clone)]
pub struct Paths {
    dir: PathBuf,
    config_file: PathBuf,
}

impl Paths {
    /// Resolve every path, honouring `--config` for the configuration file only.
    pub fn resolve(config_override: Option<&Path>) -> Result<Self, PathError> {
        let dir = resolve_dir()?;
        let mut paths = Paths::in_directory(dir);
        if let Some(path) = config_override {
            paths.config_file = path.to_path_buf();
        }
        Ok(paths)
    }

    /// Build paths rooted at a specific directory.
    ///
    /// Used by the test suite, and by anyone who wants a second, isolated
    /// installation.
    pub fn in_directory(directory: impl Into<PathBuf>) -> Paths {
        let dir = directory.into();
        let config_file = dir.join(CONFIG_FILE_NAME);
        Paths { dir, config_file }
    }

    /// The configuration file (`config.toml`).
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// The OAuth credential store (`auth.json`).
    pub fn auth_file(&self) -> PathBuf {
        self.dir.join(AUTH_FILE_NAME)
    }

    /// The daemon pid file.
    pub fn pid_file(&self) -> PathBuf {
        self.dir.join(PID_FILE_NAME)
    }

    /// The daemon log file.
    pub fn log_file(&self) -> PathBuf {
        self.dir.join(LOG_FILE_NAME)
    }

    /// The official Grok Build CLI's credential file, used as an import source.
    pub fn grok_cli_auth_file(&self) -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join(GROK_CLI_DIR).join(GROK_CLI_AUTH_FILE_NAME))
    }

    /// Create the configuration directory if needed and force its mode to `0700`.
    pub fn ensure_dir(&self) -> Result<(), PathError> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|source| PathError::Io {
                path: self.dir.clone(),
                source,
            })?;
        }
        restrict_permissions(&self.dir, 0o700).map_err(|source| PathError::Io {
            path: self.dir.clone(),
            source,
        })
    }
}

/// Apply a unix permission mode to a path, ignoring platforms without unix modes.
pub fn restrict_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Determine the configuration directory from the environment.
fn resolve_dir() -> Result<PathBuf, PathError> {
    if let Some(value) = non_empty_env("GROKPROXY_HOME") {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = non_empty_env("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(value).join("grokproxy"));
    }
    let home = dirs::home_dir().ok_or(PathError::NoHomeDirectory)?;
    Ok(home.join(".config").join("grokproxy"))
}

/// Read an environment variable, treating an empty value as unset.
fn non_empty_env(key: &str) -> Option<std::ffi::OsString> {
    match std::env::var_os(key) {
        Some(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grokproxy_home_wins_over_xdg() {
        // SAFETY: single-threaded test with no other threads touching the env.
        unsafe {
            std::env::set_var("GROKPROXY_HOME", "/tmp/grokproxy-test-home");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/grokproxy-test-xdg");
        }
        let paths = Paths::resolve(None).unwrap();
        assert_eq!(
            paths.auth_file(),
            Path::new("/tmp/grokproxy-test-home/auth.json")
        );
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/grokproxy-test-home/config.toml")
        );

        unsafe {
            std::env::remove_var("GROKPROXY_HOME");
        }
        let paths = Paths::resolve(None).unwrap();
        assert_eq!(
            paths.auth_file(),
            Path::new("/tmp/grokproxy-test-xdg/grokproxy/auth.json")
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn config_override_only_moves_the_config_file() {
        unsafe {
            std::env::set_var("GROKPROXY_HOME", "/tmp/grokproxy-test-home");
        }
        let paths = Paths::resolve(Some(Path::new("/tmp/elsewhere/custom.toml"))).unwrap();
        assert_eq!(paths.config_file(), Path::new("/tmp/elsewhere/custom.toml"));
        assert_eq!(
            paths.auth_file(),
            Path::new("/tmp/grokproxy-test-home/auth.json")
        );
        assert_eq!(
            paths.pid_file(),
            Path::new("/tmp/grokproxy-test-home/grokproxy.pid")
        );
        unsafe {
            std::env::remove_var("GROKPROXY_HOME");
        }
    }
}
