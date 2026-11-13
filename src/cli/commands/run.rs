//! `grokproxy run`, `stop`, `restart` and `croncheck`.

use std::sync::Arc;

use crate::auth::OAuthClient;
use crate::cli::{CommandError, Context};
use crate::daemon::{self, DaemonState, SpawnOutcome};
use crate::server::{Server, ServerContext};
use crate::upstream::UpstreamClient;

/// Start the server as a background daemon.
pub fn run(context: &Context, foreground: bool) -> Result<(), CommandError> {
    match daemon::state(&context.paths.pid_file(), context.config.port) {
        DaemonState::Running { pid, port } => {
            println!("Already running (pid {pid}) at http://127.0.0.1:{port}/v1");
            return Ok(());
        }
        DaemonState::Stale { pid } => {
            daemon::clear(&context.paths.pid_file());
            crate::logging::info(&format!("removed a stale pid file for {pid}"));
        }
        DaemonState::Stopped => {}
    }

    check_port_allowed(context)?;

    if foreground {
        return serve(context);
    }

    start_daemon(context, false)
}

/// Stop the running daemon.
pub fn stop(context: &Context) -> Result<(), CommandError> {
    let pid_file = context.paths.pid_file();
    match daemon::state(&pid_file, context.config.port) {
        DaemonState::Stopped => {
            println!("Not running.");
        }
        DaemonState::Stale { pid } => {
            daemon::clear(&pid_file);
            println!("Not running (removed a stale pid file for {pid}).");
        }
        DaemonState::Running { pid, .. } => {
            let stopped = daemon::stop(pid, daemon::STOP_TIMEOUT)
                .map_err(|err| CommandError::new(err.to_string()))?;
            daemon::clear(&pid_file);
            if stopped {
                println!("Stopped (pid {pid}).");
            } else {
                println!("Not running.");
            }
        }
    }
    Ok(())
}

/// Stop the daemon, then start it again.
pub fn restart(context: &Context) -> Result<(), CommandError> {
    stop(context)?;
    run(context, false)
}

/// Start the daemon only if it is not already running.
///
/// Nothing is printed: a crontab entry must not mail the user. Everything
/// that happens goes into the log file.
pub fn croncheck(context: &Context) -> Result<(), CommandError> {
    let pid_file = context.paths.pid_file();
    let state = daemon::state(&pid_file, context.config.port);
    if state.is_running() {
        crate::logging::debug("croncheck: the daemon is already running");
        return Ok(());
    }

    if let DaemonState::Stale { pid } = state {
        daemon::clear(&pid_file);
        crate::logging::info(&format!("croncheck: removed a stale pid file for {pid}"));
    }

    start_daemon(context, true)
}

/// Fork into the background and wait for the daemon to come up.
fn start_daemon(context: &Context, quiet: bool) -> Result<(), CommandError> {
    check_port_allowed(context)?;

    match daemon::spawn(&context.paths.log_file())
        .map_err(|err| CommandError::new(err.to_string()))?
    {
        SpawnOutcome::Child => {
            // We are the daemon.
            let result = serve(context);
            std::process::exit(match result {
                Ok(()) => 0,
                Err(err) => {
                    crate::logging::error(&err.to_string());
                    1
                }
            });
        }
        SpawnOutcome::Parent => {
            if daemon::wait_for_health(context.config.port, daemon::START_TIMEOUT) {
                let info = daemon::read_info(&context.paths.pid_file());
                if !quiet {
                    if let Some(info) = info {
                        println!("grokproxy is running (pid {}).", info.pid);
                    }
                    println!(
                        "Point your client at http://127.0.0.1:{}/v1",
                        context.config.port
                    );
                }
                Ok(())
            } else {
                let tail = daemon::log_tail(&context.paths.log_file(), 20);
                crate::logging::error("the daemon did not answer /health in time");
                if !quiet {
                    eprintln!(
                        "grokproxy did not start within {} seconds.",
                        daemon::START_TIMEOUT.as_secs()
                    );
                    if !tail.is_empty() {
                        eprintln!("--- last log lines ---\n{tail}");
                    }
                }
                Err(CommandError::new("the daemon failed to start"))
            }
        }
    }
}

/// Run the server in this process.
fn serve(context: &Context) -> Result<(), CommandError> {
    let server =
        Server::bind(context.config.port).map_err(|err| CommandError::new(err.to_string()))?;

    let upstream = UpstreamClient::new(context.config.upstream);
    let tokens = crate::auth::TokenManager::new(
        context.paths.clone(),
        OAuthClient::new(
            &context.config.oauth.client_id,
            &context.config.oauth.issuer,
            &context.config.scope_references(),
            &context.config.oauth.referrer,
        ),
    );

    let server_context = Arc::new(ServerContext {
        api_key: context.config.api_key.clone(),
        upstream,
        tokens,
    });

    daemon::write_info(
        &context.paths.pid_file(),
        std::process::id() as i32,
        context.config.port,
    )
    .map_err(|err| CommandError::new(err.to_string()))?;

    crate::logging::info(&format!(
        "listening on http://{} (upstream {})",
        server.address(),
        context.config.upstream.base_url()
    ));

    server.clone().serve(server_context);

    daemon::clear(&context.paths.pid_file());
    crate::logging::info("shut down");
    Ok(())
}

/// Refuse to run on a privileged port unless the user asked for it.
fn check_port_allowed(context: &Context) -> Result<(), CommandError> {
    if context.config.port < 1024 && !context.config.allow_privileged_port {
        return Err(CommandError::new(format!(
            "port {} is privileged; set `allow_privileged_port = true` in the config file to use it",
            context.config.port
        )));
    }
    Ok(())
}
