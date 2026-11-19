//! Filesystem locations used by `eggpass`.
//!
//! Resolution order for the configuration directory:
//!
//! 1. `$EGGPASS_HOME` when set — relocates *every* file (config, credentials,
//!    usage key, pid, log). Useful for tests and for running a second instance.
//! 2. `$XDG_CONFIG_HOME/eggpass` when set.
//! 3. `~/.config/eggpass`.
//!
//! The `--config <path>` flag overrides only the configuration *file*; the
//! credential, usage-key, pid and log files stay in the resolved directory.

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
/// Name of the daemon-managed CLI-only usage credential file.
pub const USAGE_KEY_FILE_NAME: &str = "usage.key";
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
        let eggpass_home = non_empty_env("EGGPASS_HOME");
        let xdg_config_home = non_empty_env("XDG_CONFIG_HOME");
        let home = if eggpass_home.is_none() && xdg_config_home.is_none() {
            dirs::home_dir()
        } else {
            None
        };
        resolve_paths(
            config_override,
            eggpass_home.as_deref(),
            xdg_config_home.as_deref(),
            home.as_deref(),
        )
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

    /// The daemon-managed usage-only credential (not moved by `--config`).
    pub fn usage_key_file(&self) -> PathBuf {
        self.dir.join(USAGE_KEY_FILE_NAME)
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
        // A relocated installation is an isolated one: `EGGPASS_HOME` must
        // not reach back into the user's home for another tool's credentials.
        let eggpass_home_is_set = non_empty_env("EGGPASS_HOME").is_some();
        let home = if eggpass_home_is_set {
            None
        } else {
            dirs::home_dir()
        };
        grok_cli_auth_file_for(eggpass_home_is_set, home)
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
        })?;

        // The configuration file may carry a hand-written `api_key`, so it is
        // as sensitive as the credential files beside it. A `--config` path
        // outside this directory belongs to the user and is left alone.
        if self.config_file.parent() == Some(self.dir.as_path()) && self.config_file.exists() {
            restrict_permissions(&self.config_file, 0o600).map_err(|source| PathError::Io {
                path: self.config_file.clone(),
                source,
            })?;
        }

        Ok(())
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

/// Resolve all paths from explicit environment values and an optional home directory.
fn resolve_paths(
    config_override: Option<&Path>,
    eggpass_home: Option<&std::ffi::OsStr>,
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Result<Paths, PathError> {
    let dir = if let Some(value) = eggpass_home.filter(|value| !value.is_empty()) {
        PathBuf::from(value)
    } else if let Some(value) = xdg_config_home.filter(|value| !value.is_empty()) {
        PathBuf::from(value).join("eggpass")
    } else {
        home.ok_or(PathError::NoHomeDirectory)?
            .join(".config")
            .join("eggpass")
    };

    let mut paths = Paths::in_directory(dir);
    if let Some(path) = config_override {
        paths.config_file = path.to_path_buf();
    }
    Ok(paths)
}

/// Resolve the official Grok CLI credential path unless the installation is relocated.
fn grok_cli_auth_file_for(eggpass_home_is_set: bool, home: Option<PathBuf>) -> Option<PathBuf> {
    if eggpass_home_is_set {
        return None;
    }
    home.map(|home| home.join(GROK_CLI_DIR).join(GROK_CLI_AUTH_FILE_NAME))
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
        use std::ffi::OsStr;

        let paths = resolve_paths(
            None,
            Some(OsStr::new("/tmp/eggpass-test-home")),
            Some(OsStr::new("/tmp/eggpass-test-xdg")),
            None,
        )
        .unwrap();
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-home/credentials/xai.json")
        );
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/eggpass-test-home/config.toml")
        );

        let paths =
            resolve_paths(None, None, Some(OsStr::new("/tmp/eggpass-test-xdg")), None).unwrap();
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-xdg/eggpass/credentials/xai.json")
        );

        let paths =
            resolve_paths(None, None, None, Some(Path::new("/tmp/eggpass-test-user"))).unwrap();
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-user/.config/eggpass/credentials/xai.json")
        );
    }

    #[test]
    fn config_override_only_moves_the_config_file() {
        use std::ffi::OsStr;

        let paths = resolve_paths(
            Some(Path::new("/tmp/elsewhere/custom.toml")),
            Some(OsStr::new("/tmp/eggpass-test-home")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(paths.config_file(), Path::new("/tmp/elsewhere/custom.toml"));
        assert_eq!(
            paths.credential_file("xai"),
            Path::new("/tmp/eggpass-test-home/credentials/xai.json")
        );
        assert_eq!(
            paths.pid_file(),
            Path::new("/tmp/eggpass-test-home/eggpass.pid")
        );
        assert_eq!(
            paths.usage_key_file(),
            Path::new("/tmp/eggpass-test-home/usage.key")
        );
    }

    #[test]
    fn ensure_dir_tightens_an_existing_config_file() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(directory.path().join("eggpass"));
        paths.ensure_dir().unwrap();
        std::fs::write(paths.config_file(), "port = 8484\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }

        paths.ensure_dir().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.config_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        }
    }

    #[test]
    fn eggpass_home_skips_the_grok_cli_import() {
        assert_eq!(
            grok_cli_auth_file_for(true, Some(PathBuf::from("/tmp/eggpass-test-home"))),
            None
        );
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
