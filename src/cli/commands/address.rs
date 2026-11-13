//! The machinery behind `eggpass port` and `eggpass host`.
//!
//! Both commands do the same thing with a different value, so the rules live
//! here and the two commands are thin wrappers:
//!
//! 1. **Validate first.** An invalid value is refused before anything is
//!    written or stopped.
//! 2. **Restart only if it would change something.** The comparison is
//!    against the address the daemon is *actually serving*, read from the pid
//!    file, not against the config file. Asking for the port that is already
//!    being served does not bounce live connections.
//! 3. **Check the new address is usable before restarting.** A taken port
//!    prints `port <port> is taken` and changes nothing at all, so the user
//!    can immediately try another port.
//! 4. **Apply, and be able to undo.** Stop, write the config, start, wait for
//!    `/health`. If the start fails, the previous config is restored and the
//!    daemon comes back up on the old address.

use std::net::TcpListener;

use crate::cli::{CommandError, Context};
use crate::config::Config;

/// Which part of the address a command is changing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// The listen port.
    Port(u16),
    /// The bind address.
    Host(String),
}

/// What the daemon is serving right now.
#[derive(Debug, Clone)]
pub struct Live {
    /// Whether the daemon is running.
    pub running: bool,
    /// The address it bound.
    pub host: String,
    /// The port it is serving.
    pub port: u16,
}

/// The address the daemon is serving, or the configured one when it is not
/// running.
pub fn live_address(context: &Context) -> Live {
    match crate::daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    ) {
        crate::daemon::DaemonState::Running { host, port, .. } => Live {
            running: true,
            host,
            port,
        },
        _ => Live {
            running: false,
            host: context.config.host.clone(),
            port: context.config.port,
        },
    }
}

/// Apply an address change.
pub fn apply(context: &Context, address: Address) -> Result<(), CommandError> {
    let live = live_address(context);

    // What the daemon is serving, for the comparison.
    let (live_host, live_port) = (live.host.clone(), live.port);

    // What the new address will be.
    let (new_host, new_port) = match &address {
        Address::Port(port) => (live_host.clone(), *port),
        Address::Host(host) => (host.clone(), live_port),
    };

    // 2. Only restart when the value actually differs from what is served.
    let changes_serving = match &address {
        Address::Port(port) => *port != live_port,
        Address::Host(host) => host != &live_host,
    };

    // The config is always brought in line with the request, so that a later
    // restart does not undo it.
    let config_differs = match &address {
        Address::Port(port) => *port != context.config.port,
        Address::Host(host) => host != &context.config.host,
    };

    if !changes_serving {
        if config_differs {
            write_config(context, &address)?;
            println!("already serving {}", describe(&address));
            println!("The configuration file was updated to match.");
        } else {
            println!("already serving {}", describe(&address));
        }
        return Ok(());
    }

    // 3. Check the new address is usable, before anything is stopped.
    if let Some(reason) = unusable(&new_host, new_port) {
        match &address {
            Address::Port(port) => {
                println!("port {port} is taken");
            }
            Address::Host(host) => {
                println!("host {host} is not bindable");
            }
        }
        println!("Nothing was changed. {reason}");
        return Err(CommandError::silent(1));
    }

    if !live.running {
        // Nothing to restart: the change takes effect at the next start.
        write_config(context, &address)?;
        println!(
            "{} set to {} — it takes effect at the next start.",
            label(&address),
            describe(&address)
        );
        return Ok(());
    }

    // 4. Apply, with a way back.
    let previous_config = std::fs::read_to_string(context.paths.config_file()).ok();

    println!(
        "restarting on {}…",
        match &address {
            Address::Port(port) => format!("port {port}"),
            Address::Host(host) => host.to_string(),
        }
    );

    crate::cli::commands::run::stop(context)?;

    if let Err(err) = write_config(context, &address) {
        if let Err(restore_err) = restore(context, previous_config) {
            eprintln!("error: could not restore the config file: {restore_err}");
        }
        return Err(err);
    }

    match start(context) {
        Ok(()) => {
            println!("running on {}", base_url(&new_host, new_port));
            Ok(())
        }
        Err(err) => {
            // The change failed: put everything back and bring the old
            // address up again.
            let restored = restore(context, previous_config);
            match restored {
                Ok(()) => {
                    println!("The change failed and was rolled back.");
                    Err(err)
                }
                Err(restore_err) => {
                    eprintln!("error: {restore_err}");
                    Err(err)
                }
            }
        }
    }
}

/// Write the new value into the configuration file.
fn write_config(context: &Context, address: &Address) -> Result<(), CommandError> {
    let result = match address {
        Address::Port(port) => crate::config::set_port(context.paths.config_file(), *port),
        Address::Host(host) => crate::config::set_host(context.paths.config_file(), host),
    };
    result.map_err(|err| CommandError::new(format!("could not update the config file: {err}")))
}

/// Put the old configuration file back.
fn restore(context: &Context, previous: Option<String>) -> Result<(), CommandError> {
    if let Some(text) = previous {
        std::fs::write(context.paths.config_file(), text).map_err(|err| {
            CommandError::new(format!("could not restore the config file: {err}"))
        })?;
    }

    // Bring the daemon back up on the old address.
    if let Ok(config) = Config::load(context.paths.config_file()) {
        let restored = Context {
            paths: context.paths.clone(),
            config,
            cli: context.cli.clone(),
        };
        crate::cli::commands::run::run(&restored, false)?;
    }
    Ok(())
}

/// Start the daemon with the configuration as it now stands.
fn start(context: &Context) -> Result<(), CommandError> {
    let config = Config::load(context.paths.config_file())
        .map_err(|err| CommandError::new(format!("could not read the config file: {err}")))?;
    let refreshed = Context {
        paths: context.paths.clone(),
        config,
        cli: context.cli.clone(),
    };
    crate::cli::commands::run::run(&refreshed, false)
}

/// Why the new address cannot be used, if it cannot.
///
/// The check is a bind: the same thing the server will do.
fn unusable(host: &str, port: u16) -> Option<String> {
    match TcpListener::bind((host, port)) {
        Ok(listener) => {
            // The probe socket is closed here, so there is a small window
            // before the server binds. It is the best that can be done
            // without holding the socket open.
            drop(listener);
            None
        }
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            Some("Something else is already listening there.".to_string())
        }
        Err(err) => Some(format!("It cannot be bound: {err}")),
    }
}

/// The name of the setting being changed.
fn label(address: &Address) -> &'static str {
    match address {
        Address::Port(_) => "port",
        Address::Host(_) => "host",
    }
}

/// A human-readable description of the requested value.
fn describe(address: &Address) -> String {
    match address {
        Address::Port(port) => format!("port {port}"),
        Address::Host(host) => format!("host {host}"),
    }
}

/// The base URL the server answers on.
fn base_url(host: &str, port: u16) -> String {
    format!("http://{}:{port}", crate::daemon::probe_host(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_and_the_description_match_the_setting() {
        assert_eq!(label(&Address::Port(9000)), "port");
        assert_eq!(label(&Address::Host("0.0.0.0".into())), "host");
        assert_eq!(describe(&Address::Port(9000)), "port 9000");
        assert_eq!(describe(&Address::Host("0.0.0.0".into())), "host 0.0.0.0");
    }

    #[test]
    fn a_wildcard_bind_probes_the_loopback_interface() {
        assert_eq!(crate::daemon::probe_host("0.0.0.0"), "127.0.0.1");
        assert_eq!(crate::daemon::probe_host("::"), "127.0.0.1");
        assert_eq!(crate::daemon::probe_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(crate::daemon::probe_host("192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn a_taken_port_is_detected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(unusable("127.0.0.1", port).is_some());
    }

    #[test]
    fn a_free_port_is_usable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(unusable("127.0.0.1", port).is_none());
    }
}
