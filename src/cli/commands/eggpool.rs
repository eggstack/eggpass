//! `eggpass eggpool` setup and revocation.

use std::path::PathBuf;

use crate::cli::args::{EggpoolAction, EggpoolControl};
use crate::cli::{CommandError, Context};
use crate::config::Config;
use crate::daemon::{self, DaemonState};

/// Configure or revoke the EggPool connection.
pub fn eggpool(context: &Context, action: EggpoolAction) -> Result<(), CommandError> {
    match action {
        EggpoolAction::Setup {
            control,
            server_name,
            ca,
            label,
        } => setup(context, control, server_name, ca, label),
        EggpoolAction::Revoke => revoke(context),
    }
}

fn setup(
    context: &Context,
    control: EggpoolControl,
    server_name: String,
    ca: PathBuf,
    label: Option<String>,
) -> Result<(), CommandError> {
    let daemon_state = daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );
    let (live_host, live_port, was_running) = match daemon_state {
        DaemonState::Running { host, port, .. } => (host, port, true),
        DaemonState::Stale { .. } | DaemonState::Stopped => {
            (context.config.host.clone(), context.config.port, false)
        }
    };

    let ca = std::fs::canonicalize(&ca).map_err(|err| {
        CommandError::new(format!(
            "could not resolve EggPool CA file {}: {err}",
            ca.display()
        ))
    })?;
    let control = match control.host.contains(':') {
        true => format!("[{}]:{}", control.host, control.port),
        false => format!("{}:{}", control.host, control.port),
    };
    let label = label.unwrap_or_else(|| "eggpass".to_string());
    let ticket = crate::eggpool::create_ticket(
        &context.paths,
        &context.config,
        crate::eggpool::TicketSetup {
            control: &control,
            server_name: &server_name,
            ca_path: &ca,
            label: &label,
            live_host: &live_host,
            live_port,
        },
    )
    .map_err(CommandError::new)?;

    if was_running {
        let refreshed = refreshed_context(context)?;
        if let Err(err) = crate::cli::commands::run::restart(&refreshed) {
            return Err(CommandError::new(format!(
                "EggPool was configured, but the daemon could not restart; the ticket was not printed. Rerun setup to issue a fresh ticket ({err})"
            )));
        }
    }

    // The ticket is a bearer credential; stdout is the only place it is shown.
    println!("{}", ticket.as_str());
    Ok(())
}

fn revoke(context: &Context) -> Result<(), CommandError> {
    let was_running = matches!(
        daemon::state(
            &context.paths.pid_file(),
            &context.config.host,
            context.config.port,
        ),
        DaemonState::Running { .. }
    );
    // Stop first so revocation cannot leave an authenticated reverse client
    // alive if cleanup or the later optional restart fails.
    if was_running {
        crate::cli::commands::run::stop(context)?;
    }
    crate::eggpool::revoke(&context.paths, &context.config).map_err(CommandError::new)?;

    if was_running {
        let refreshed = refreshed_context(context)?;
        crate::cli::commands::run::start_after_revoke(&refreshed)?;
    }
    println!("EggPool connection revoked.");
    Ok(())
}

fn refreshed_context(context: &Context) -> Result<Context, CommandError> {
    let config = Config::load(context.paths.config_file())
        .map_err(|err| CommandError::new(format!("could not reload the updated config: {err}")))?;
    Ok(Context {
        paths: context.paths.clone(),
        config,
        cli: context.cli.clone(),
    })
}
