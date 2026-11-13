//! `eggpass host`.

use crate::cli::commands::address::{self, Address};
use crate::cli::{CommandError, Context};

/// Show or change the address the server binds to.
///
/// A change is applied immediately: the command restarts the daemon for you.
/// The restart only happens when the requested address differs from the one
/// the daemon is actually serving.
pub fn host(context: &Context, requested: Option<String>) -> Result<(), CommandError> {
    let Some(requested) = requested else {
        return show(context);
    };

    // A name that is not an address is refused rather than resolved: a bind
    // address has to name a local interface.
    let normalized = crate::config::normalize_host(&requested).map_err(CommandError::new)?;

    // Binding anywhere but loopback puts the proxy — and through it the
    // user's subscriptions — on the network. Without a key of any kind that
    // would be an open proxy on someone's paid plan.
    if !crate::config::is_loopback_host(&normalized) && !has_key(context) {
        return Err(CommandError::new(format!(
            "refusing to bind {normalized}: issue a key with `eggpass key add` first, so that the proxy is not open to the network"
        )));
    }

    let result = address::apply(context, Address::Host(normalized.clone()));

    if result.is_ok() && !crate::config::is_loopback_host(&normalized) {
        println!("warning: the proxy is now reachable from the network");
    }

    result
}

/// Print the bind address.
fn show(context: &Context) -> Result<(), CommandError> {
    let live = address::live_address(context);

    println!("{}", context.config.host);
    println!("{}", crate::config::Config::api_base_url(&context.config));

    if live.running && live.host != context.config.host {
        println!("(the running server is still bound to {})", live.host);
    }
    Ok(())
}

/// Whether any key is configured, from either place.
///
/// The key in the configuration file counts, and so does any key `eggpass`
/// issued.
fn has_key(context: &Context) -> bool {
    if !context.config.api_key.is_empty() {
        return true;
    }

    match crate::keys::KeyStore::open(context.paths.key_store_file()) {
        Ok(store) => !store.is_empty(),
        Err(_) => false,
    }
}
