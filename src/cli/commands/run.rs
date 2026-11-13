//! `eggpass run`, `stop`, `restart` and `croncheck`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cli::{CommandError, Context};
use crate::daemon::{self, DaemonState, SpawnOutcome};
use crate::provider::{ProviderId, ProviderRuntime};
use crate::server::{ProviderHandle, Server, ServerContext};

/// Start the server as a background daemon.
pub fn run(context: &Context, foreground: bool) -> Result<(), CommandError> {
    match daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    ) {
        DaemonState::Running { pid, host, port } => {
            println!(
                "Already running (pid {pid}) at http://{}:{port}/v1",
                daemon::probe_host(&host)
            );
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
    match daemon::state(&pid_file, &context.config.host, context.config.port) {
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
    let state = daemon::state(&pid_file, &context.config.host, context.config.port);
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
            if daemon::wait_for_health(
                &context.config.host,
                context.config.port,
                daemon::START_TIMEOUT,
            ) {
                let info = daemon::read_info(&context.paths.pid_file());
                if !quiet {
                    if let Some(info) = info {
                        println!("eggpass is running (pid {}).", info.pid);
                    }
                    println!(
                        "Point your client at http://{}:{}/v1",
                        daemon::probe_host(&context.config.host),
                        context.config.port
                    );
                }
                Ok(())
            } else {
                let tail = daemon::log_tail(&context.paths.log_file(), 20);
                crate::logging::error("the daemon did not answer /health in time");
                if !quiet {
                    eprintln!(
                        "eggpass did not start within {} seconds.",
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
    let server = Server::bind_with_limits(
        &context.config.host,
        context.config.port,
        context.config.server,
    )
    .map_err(|err| CommandError::new(err.to_string()))?;

    // Every enabled provider gets a handle. A provider we are not signed in
    // to is still registered: it simply answers 401 when it is used.
    let mut providers: HashMap<ProviderId, ProviderHandle> = HashMap::new();
    for id in ProviderId::all() {
        if !context.config.providers.get(id).enabled {
            continue;
        }
        let runtime = ProviderRuntime::new(id, &context.config, &context.paths);
        let upstream =
            crate::upstream::UpstreamClient::with_timeouts(&runtime.spec, context.config.timeouts);
        providers.insert(
            id,
            ProviderHandle {
                spec: runtime.spec,
                upstream,
                tokens: runtime.tokens,
            },
        );
    }

    let default_provider = context
        .config
        .routing
        .default_provider
        .unwrap_or(ProviderId::Xai);

    // The keys `eggpass` issued. The server reads them afresh whenever the
    // file changes, so `eggpass key add` and `eggpass key revoke` take
    // effect without a restart.
    let keys = Arc::new(
        crate::keys::KeyStore::open(context.paths.key_store_file())
            .map_err(|err| CommandError::new(err.to_string()))?,
    );

    // A server that is reachable from the network and has no key refuses
    // every request rather than serving as an open proxy. Say so loudly: the
    // user probably means to issue a key.
    if !crate::config::is_loopback_host(&context.config.host)
        && context.config.api_key.is_empty()
        && keys.is_empty()
    {
        crate::logging::error(&format!(
            "bound to {} with no API key configured; every request will be refused until `eggpass key add` is run",
            context.config.host
        ));
    }

    let server_context = Arc::new(ServerContext {
        host: context.config.host.clone(),
        api_key: context.config.api_key.clone(),
        keys,
        providers,
        default_provider,
        routing: context.config.routing.clone(),
        upstream_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_upstream_requests: crate::server::DEFAULT_MAX_UPSTREAM_REQUESTS,
    });

    daemon::write_info(
        &context.paths.pid_file(),
        std::process::id() as i32,
        &context.config.host,
        context.config.port,
    )
    .map_err(|err| CommandError::new(err.to_string()))?;

    crate::logging::info(&format!(
        "listening on http://{} ({} provider(s) configured)",
        server.address(),
        server_context.providers.len()
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
