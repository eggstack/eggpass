//! `eggpass edit`.

use std::process::Command;

use crate::cli::{CommandError, Context};
use crate::config;

/// Open the configuration file in the user's editor.
///
/// The editor is `$EDITOR`, then `$VISUAL`, then the first of `helix`,
/// `vim` and `nano` that is installed. When the editor exits the file is
/// validated; a syntax error is reported without touching the file.
pub fn edit(context: &Context) -> Result<(), CommandError> {
    let path = context.paths.config_file().to_path_buf();
    config::ensure_config_file(&path).map_err(|err| CommandError::new(err.to_string()))?;

    let editor = pick_editor();

    let status = Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|err| CommandError::new(format!("could not run `{editor}`: {err}")))?;

    if !status.success() {
        return Err(CommandError::new(format!(
            "`{editor}` exited with {status}; the configuration was left unchanged"
        )));
    }

    match config::Config::load(&path) {
        Ok(_) => {
            println!("The configuration is valid.");
            Ok(())
        }
        Err(err) => Err(CommandError::new(format!(
            "the configuration is not valid, and was left unchanged: {err}"
        ))),
    }
}

/// Choose the editor to use.
fn pick_editor() -> String {
    for variable in ["EDITOR", "VISUAL"] {
        if let Ok(value) = std::env::var(variable) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    for candidate in ["helix", "vim", "nano"] {
        if is_installed(candidate) {
            return candidate.to_string();
        }
    }

    "vi".to_string()
}

/// Whether a program is on the `PATH`.
fn is_installed(program: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .filter(|directory| !directory.is_empty())
        .any(|directory| {
            let candidate = std::path::Path::new(directory).join(program);
            candidate.is_file()
        })
}
