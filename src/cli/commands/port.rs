//! `grokproxy port`.

use crate::cli::{CommandError, Context};
use crate::config;
use crate::daemon::{self, DaemonState};

/// Show or change the listen port.
///
/// Changing the port never restarts the daemon: the new port takes effect at
/// the next `grokproxy restart`.
pub fn port(context: &Context, requested: Option<u16>) -> Result<(), CommandError> {
    let Some(requested) = requested else {
        println!("{}", context.config.port);
        println!("{}", context.config.api_base_url());
        report_live_port(context);
        return Ok(());
    };

    if requested < 1024 && !context.config.allow_privileged_port {
        return Err(CommandError::new(format!(
            "port {requested} is privileged; set `allow_privileged_port = true` in the config file to use it"
        )));
    }

    config::set_port(context.paths.config_file(), requested)
        .map_err(|err| CommandError::new(err.to_string()))?;

    let live = match daemon::state(&context.paths.pid_file(), context.config.port) {
        DaemonState::Running { port, .. } => Some(port),
        _ => None,
    };

    match live {
        Some(live) if live != requested => {
            println!(
                "port set to {requested} — run 'grokproxy restart' to apply (currently serving {live})"
            );
        }
        _ => {
            println!("port set to {requested} — run 'grokproxy restart' to apply");
        }
    }

    Ok(())
}

/// Mention the port the daemon is actually serving, when it differs.
fn report_live_port(context: &Context) {
    if let DaemonState::Running { port, .. } =
        daemon::state(&context.paths.pid_file(), context.config.port)
        && port != context.config.port
    {
        println!("(the running server is still serving on port {port})");
    }
}
