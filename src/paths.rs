//! Filesystem locations used by `eggpass`.
//!
//! Resolution order for the configuration directory:
//!
//! 1. `$EGGPASS_HOME` when set — relocates *every* file (config, credentials,
//!    pid, log). Useful for tests and for running a second instance.
//! 2. `$XDG_CONFIG_HOME/eggpass` when set.
//! 3. `~/.config/eggpass`.
//!
//! The `--config <path>` flag overrides only the configuration *file*; the
//! credential, pid and log files stay in the resolved directory.

use std::path::{Path, PathBuf};

/// Name of the user-editable configuration file.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Name of the daemon pid file.
pub const PID_FILE_NAME: &str = "eggpass.pid";
/// Name of the daemon log file.
pub const LOG_FILE_NAME: &str = "eggpass.log";
/// Name of the directory holding the per-provider credential files.
pub const CREDENTIALS_DIR_NAME: &str = "credentials";
/// Name of the file holding the keys `eggpass` issues to its clients.
pub const KEY_STORE_FILE_NAME: &str = "keys.json";
/// The legacy single-provider credential file, kept for the migration.
pub const LEGACY_AUTH_FILE_NAME: &str = "auth.json";
/// The name this project had before the rename.
pub const LEGACY_DIR_NAME: &str = "grokproxy";
/// The marker left behind in the legacy directory after a migration.
pub const MIGRATION_MARKER_NAME: &str = "migrated-to-eggpass";

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
                "cannot determine the home directory; set $EGGPASS_HOME or $XDG_CONFIG_HOME"
            ),
            PathError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for PathError {}

/// The set of files `eggpass` reads and writes.
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

    /// The configuration directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The configuration file (`config.toml`).
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// The directory holding the per-provider credential files.
    pub fn credentials_dir(&self) -> PathBuf {
        self.dir.join(CREDENTIALS_DIR_NAME)
    }

    /// The credential file for one provider (`credentials/<provider>.json`).
    pub fn credential_file(&self, provider: &str) -> PathBuf {
        self.credentials_dir().join(format!("{provider}.json"))
    }

    /// The file holding the keys `eggpass` issues to its clients.
    pub fn key_store_file(&self) -> PathBuf {
        self.dir.join(KEY_STORE_FILE_NAME)
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

    /// The directory this project used before the rename.
    ///
    /// Only consulted when it sits next to the resolved directory, so a
    /// relocated installation (`$EGGPASS_HOME`) never drags in an unrelated
    /// legacy directory.
    pub fn legacy_dir(&self) -> Option<PathBuf> {
        let legacy = self.dir.parent()?.join(LEGACY_DIR_NAME);
        if legacy.is_dir() { Some(legacy) } else { None }
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
    if let Some(value) = non_empty_env("EGGPASS_HOME") {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = non_empty_env("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(value).join("eggpass"));
    }
    let home = dirs::home_dir().ok_or(PathError::NoHomeDirectory)?;
    Ok(home.join(".config").join("eggpass"))
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
    fn eggpass_home_wins_over_xdg() {
        // SAFETY: single-threaded test with no other threads touching the env.
        unsafe {
            std::env::set_var("EGGPASS_HOME", "/tmp/eggpass-test-home");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/eggpass-test-xdg");
        }
        let paths = Paths::resolve(None).unwrap();
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-home/credentials/xai.json")
        );
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/eggpass-test-home/config.toml")
        );

        unsafe {
            std::env::remove_var("EGGPASS_HOME");
        }
        let paths = Paths::resolve(None).unwrap();
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-xdg/eggpass/credentials/xai.json")
        );

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn config_override_only_moves_the_config_file() {
        unsafe {
            std::env::set_var("EGGPASS_HOME", "/tmp/eggpass-test-home");
        }
        let paths = Paths::resolve(Some(Path::new("/tmp/elsewhere/custom.toml"))).unwrap();
        assert_eq!(paths.config_file(), Path::new("/tmp/elsewhere/custom.toml"));
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-home/credentials/xai.json")
        );
        assert_eq!(
            paths.pid_file(),
            Path::new("/tmp/eggpass-test-home/eggpass.pid")
        );
        unsafe {
            std::env::remove_var("EGGPASS_HOME");
        }
    }

    #[test]
    fn the_legacy_directory_is_only_found_beside_ours() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(LEGACY_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();

        let paths = Paths::in_directory(dir.path().join("eggpass"));
        assert_eq!(paths.legacy_dir(), Some(legacy));

        // A relocated installation has no sibling to migrate from.
        let elsewhere = Paths::in_directory("/tmp/eggpass-somewhere-else");
        assert_eq!(elsewhere.legacy_dir(), None);
    }
}
