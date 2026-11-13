//! `eggpass port`.

use crate::cli::commands::address::{self, Address};
use crate::cli::{CommandError, Context};

/// Show or change the listen port.
///
/// A change is applied immediately: the command restarts the daemon for you.
/// The restart only happens when the requested port differs from the one the
/// daemon is actually serving.
pub fn port(context: &Context, requested: Option<u16>) -> Result<(), CommandError> {
    let Some(requested) = requested else {
        return show(context);
    };

    // A port below 1024 is privileged and needs the same explicit opt-in the
    // server itself applies.
    if requested < 1024 && !context.config.allow_privileged_port {
        return Err(CommandError::new(format!(
            "port {requested} is privileged; set `allow_privileged_port = true` in the config file to use it"
        )));
    }

    address::apply(context, Address::Port(requested))
}

/// Print the port.
fn show(context: &Context) -> Result<(), CommandError> {
    let live = address::live_address(context);

    println!("{}", context.config.port);
    println!("{}", context.config.api_base_url());

    // Mention the port the daemon is actually serving, when it differs.
    if live.running && live.port != context.config.port {
        println!(
            "(the running server is still serving on port {})",
            live.port
        );
    }
    Ok(())
}
