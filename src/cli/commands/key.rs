//! `eggpass key`.
//!
//! `eggpass` issues the keys its clients use. A key is minted by
//! `eggpass key add`, shown exactly once, and stored as a salted hash; the
//! key itself is never written down. `eggpass key list` shows the numbers
//! and the visible part of each key, and `eggpass key revoke <number>` takes
//! one away.
//!
//! A key takes effect as soon as it is issued: the server re-reads the store
//! whenever it changes, so nothing has to be restarted.

use crate::cli::args::KeyAction;
use crate::cli::{CommandError, Context};
use crate::keys::KeyStore;
use crate::timeutil;

/// Add, list and revoke the keys clients use.
pub fn key(context: &Context, action: KeyAction) -> Result<(), CommandError> {
    let store = KeyStore::open(context.paths.key_store_file())
        .map_err(|err| CommandError::new(err.to_string()))?;

    match action {
        KeyAction::Add { label } => add(&store, label),
        KeyAction::List => list(context, &store),
        KeyAction::Revoke { id, all } => revoke(&store, id, all),
    }
}

/// Mint a key and show it.
fn add(store: &KeyStore, label: Option<String>) -> Result<(), CommandError> {
    let (id, plain) = store
        .add(label)
        .map_err(|err| CommandError::new(err.to_string()))?;

    println!("Created key #{id}.");
    println!();
    println!("    {plain}");
    println!();
    println!("This is the only time the key is shown — only its hash is stored.");
    println!("Send it as:");
    println!("    Authorization: Bearer {plain}");

    Ok(())
}

/// Show the keys that exist.
fn list(context: &Context, store: &KeyStore) -> Result<(), CommandError> {
    let entries = store.entries();

    if entries.is_empty() {
        println!("No keys yet.");
        println!("Create one with `eggpass key add`.");
        return Ok(());
    }

    println!("  #  key                  label         created");
    for entry in &entries {
        println!(
            "  {:<2} {:<20} {:<14} {}",
            entry.id,
            entry.prefix,
            entry.label_or_placeholder(),
            timeutil::format_rfc3339(entry.created_at)
        );
    }

    println!();
    if !context.config.api_key.is_empty() {
        println!(
            "The configuration file also sets an `api_key`, which is accepted alongside these keys."
        );
    }
    println!("Revoke one with `eggpass key revoke <number>`.");

    Ok(())
}

/// Take a key away.
fn revoke(store: &KeyStore, id: Option<u32>, all: bool) -> Result<(), CommandError> {
    if all {
        let removed = store
            .revoke_all()
            .map_err(|err| CommandError::new(err.to_string()))?;
        if removed == 0 {
            println!("There were no keys to revoke.");
            return Ok(());
        }
        println!("Revoked {removed} key(s).");
        warn_if_open(store);
        return Ok(());
    }

    let Some(id) = id else {
        return Err(CommandError::new(
            "`eggpass key revoke` needs the number of the key to revoke",
        ));
    };

    let entry = store
        .revoke(id)
        .map_err(|err| CommandError::new(err.to_string()))?;

    println!("Revoked key #{} ({}).", entry.id, entry.prefix);
    warn_if_open(store);
    Ok(())
}

/// Say so when the last key has gone and the endpoint is closed.
fn warn_if_open(store: &KeyStore) {
    if !store.is_empty() {
        return;
    }

    println!(
        "That was the last key. Every request is now refused; \
         run `eggpass key add` to issue another."
    );
}
