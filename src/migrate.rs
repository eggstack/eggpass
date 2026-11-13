//! Moving the configuration directory to its new name.
//!
//! The project used to live in `~/.config/grokproxy`. It now lives in
//! `~/.config/eggpass`, because the name is part of the environment contract:
//! `EGGPASS_HOME`, the pid file and the log file all key off it.
//!
//! The migration is one-time, idempotent, and never overwrites an existing
//! `~/.config/eggpass/`. It does **not** delete the old directory: a proxy
//! should not throw away user data unprompted, so the old directory is left
//! in place with a marker in it.

use std::path::PathBuf;

use crate::paths::{CREDENTIALS_DIR_NAME, LEGACY_AUTH_FILE_NAME, MIGRATION_MARKER_NAME, Paths};

/// What the migration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migration {
    /// There was nothing to migrate.
    Nothing,
    /// The configuration directory was moved.
    Migrated {
        /// The directory it came from.
        from: PathBuf,
        /// The files that were moved.
        moved: Vec<String>,
    },
}

impl Migration {
    /// Whether anything was moved.
    pub fn did_migrate(&self) -> bool {
        matches!(self, Migration::Migrated { .. })
    }
}

/// Move the old configuration directory to the new one, if it is safe to do so.
///
/// The migration only runs when the new directory does not exist at all. If it
/// does — even if it is empty — the user's current installation wins, and
/// nothing is touched.
pub fn migrate(paths: &Paths) -> Result<Migration, std::io::Error> {
    let Some(legacy) = paths.legacy_dir() else {
        return Ok(Migration::Nothing);
    };

    // The new directory already exists: leave everything alone.
    if paths.dir().exists() {
        return Ok(Migration::Nothing);
    }

    // The copies below land inside the new directory, so it has to exist first.
    std::fs::create_dir_all(paths.dir())?;
    crate::paths::restrict_permissions(paths.dir(), 0o700)?;

    let mut moved: Vec<String> = Vec::new();

    // The configuration file. A `--config` override names a file the user
    // chose, so the legacy one is left where it is rather than copied over it.
    let legacy_config = legacy.join(crate::paths::CONFIG_FILE_NAME);
    let config_is_default = paths.config_file() == paths.dir().join(crate::paths::CONFIG_FILE_NAME);
    if config_is_default && legacy_config.is_file() {
        std::fs::copy(&legacy_config, paths.config_file())?;
        moved.push(crate::paths::CONFIG_FILE_NAME.to_string());
    }

    // The credential file moves into the new per-provider layout. It stays
    // where it is as well, because the old directory is not deleted.
    let legacy_auth = legacy.join(LEGACY_AUTH_FILE_NAME);
    if legacy_auth.is_file() {
        let credentials = paths.credentials_dir();
        std::fs::create_dir_all(&credentials)?;
        crate::paths::restrict_permissions(&credentials, 0o700)?;
        let target = paths.credential_file("xai");
        if !target.exists() {
            std::fs::copy(&legacy_auth, &target)?;
            crate::paths::restrict_permissions(&target, 0o600)?;
            moved.push(format!("{CREDENTIALS_DIR_NAME}/xai.json"));
        }
    }

    // The pid and log files.
    for (old_name, new_name) in [
        ("grokproxy.pid", crate::paths::PID_FILE_NAME),
        ("grokproxy.log", crate::paths::LOG_FILE_NAME),
    ] {
        let source = legacy.join(old_name);
        if source.is_file() {
            let target = paths.dir().join(new_name);
            if !target.exists() {
                std::fs::copy(&source, &target)?;
                moved.push(new_name.to_string());
            }
        }
    }

    // Leave a marker so the user can see what happened, and so a later run
    // does not try again.
    let marker = legacy.join(MIGRATION_MARKER_NAME);
    if !marker.exists() {
        let _ = std::fs::write(
            &marker,
            "This directory was migrated to the `eggpass` configuration directory.\n\
             The files were copied, not moved, and nothing here was deleted.\n",
        );
    }

    Ok(Migration::Migrated {
        from: legacy,
        moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_happens_without_a_legacy_directory() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_directory(dir.path().join("eggpass"));
        assert_eq!(migrate(&paths).unwrap(), Migration::Nothing);
    }

    #[test]
    fn the_legacy_directory_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("grokproxy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.toml"), "port = 9000\n").unwrap();
        std::fs::write(legacy.join("auth.json"), r#"{"access_token":"x"}"#).unwrap();
        std::fs::write(legacy.join("grokproxy.pid"), "123\n8484\n").unwrap();

        let paths = Paths::in_directory(dir.path().join("eggpass"));
        let result = migrate(&paths).unwrap();
        assert!(result.did_migrate());

        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            "port = 9000\n"
        );
        assert!(paths.credential_file("xai").is_file());
        assert!(paths.pid_file().is_file());

        // The old directory is left alone, with a marker in it.
        assert!(legacy.is_dir());
        assert!(legacy.join(MIGRATION_MARKER_NAME).is_file());
        assert!(legacy.join("auth.json").is_file());
    }

    #[test]
    fn an_existing_new_directory_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("grokproxy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.toml"), "port = 9000\n").unwrap();

        let paths = Paths::in_directory(dir.path().join("eggpass"));
        std::fs::create_dir_all(paths.dir()).unwrap();
        std::fs::write(paths.config_file(), "port = 1234\n").unwrap();

        assert_eq!(migrate(&paths).unwrap(), Migration::Nothing);
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            "port = 1234\n"
        );
    }

    #[test]
    fn the_migration_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("grokproxy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("config.toml"), "port = 9000\n").unwrap();

        let paths = Paths::in_directory(dir.path().join("eggpass"));
        assert!(migrate(&paths).unwrap().did_migrate());

        // The second run finds the new directory and does nothing.
        assert_eq!(migrate(&paths).unwrap(), Migration::Nothing);
    }
}
