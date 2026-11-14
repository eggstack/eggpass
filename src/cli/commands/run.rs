//! `eggpass run`, `stop`, `restart` and `croncheck`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cli::{CommandError, Context};
use crate::daemon::{self, DaemonState, SpawnOutcome};
use crate::provider::{ProviderId, ProviderRuntime};
use crate::server::{ProviderHandle, Server, ServerContext};

/// Start the server as a background daemon.
pub fn run(context: &Context, foreground: bool) -> Result<(), CommandError> {
    require_key(context)?;

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
    // Check before stopping: a missing key must not leave the endpoint down.
    require_key(context)?;
    stop(context)?;
    run(context, false)
}

/// Start the daemon only if it is not already running.
///
/// Nothing is printed: a crontab entry must not mail the user. Everything
/// that happens goes into the log file.
pub fn croncheck(context: &Context) -> Result<(), CommandError> {
    require_key(context)?;

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

/// Refuse to start without a client key.
///
/// A loopback socket is reachable from a web page, so "no key" is not a
/// boundary. `run` and `croncheck` tell the user to issue one rather than
/// serving an endpoint anything local can use.
fn require_key(context: &Context) -> Result<(), CommandError> {
    ensure_a_key_exists(&context.config.api_key, &context.paths.key_store_file())
}

/// The implementation behind [`require_key`], testable without a context.
fn ensure_a_key_exists(api_key: &str, store: &std::path::Path) -> Result<(), CommandError> {
    if !api_key.is_empty() {
        return Ok(());
    }

    match crate::keys::KeyStore::open(store) {
        Ok(keys) if !keys.is_empty() => Ok(()),
        Ok(_) => Err(no_key()),
        Err(err) => Err(CommandError::new(format!(
            "the key store at {} could not be read ({err}); run `eggpass key add` to issue a key",
            store.display()
        ))),
    }
}

/// The error a user sees when the proxy has no client key.
fn no_key() -> CommandError {
    CommandError::new(
        "refusing to run: no client key is configured; run `eggpass key add` first \
         (or set `api_key` in the config file)",
    )
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

    let server_context = Arc::new(ServerContext {
        host: context.config.host.clone(),
        port: context.config.port,
        api_key: context.config.api_key.clone(),
        keys,
        providers,
        default_provider,
        routing: context.config.routing.clone(),
        upstream_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_upstream_requests: crate::server::DEFAULT_MAX_UPSTREAM_REQUESTS,
        // The status of the last inference to each provider, kept in memory
        // for `eggpass usage` to report.
        inferences: Arc::new(crate::inference::InferenceState::new()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_api_key_needs_no_store() {
        let path = std::path::Path::new("/nonexistent/keys.json");
        assert!(ensure_a_key_exists("secret", path).is_ok());
    }

    #[test]
    fn an_empty_store_is_refused_with_a_pointer_to_the_command() {
        let directory = tempfile::tempdir().expect("temp dir");
        let err = ensure_a_key_exists("", &directory.path().join("keys.json"))
            .expect_err("a server with no key is refused");
        let message = err.message.unwrap_or_default();
        assert!(message.contains("eggpass key add"), "{message}");
        assert!(message.contains("refusing to run"), "{message}");
    }

    #[test]
    fn an_issued_key_is_enough() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("keys.json");
        let store = crate::keys::KeyStore::open(&path).expect("key store");
        store.add(None).expect("key");

        assert!(ensure_a_key_exists("", &path).is_ok());
    }

    #[test]
    fn a_broken_store_is_reported() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("keys.json");
        std::fs::write(&path, "{not json").expect("write");

        let err = ensure_a_key_exists("", &path).expect_err("a broken store is refused");
        let message = err.message.unwrap_or_default();
        assert!(message.contains("could not be read"), "{message}");
    }
}
