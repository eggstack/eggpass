//! `eggpass update`.
//!
//! The update itself is done by `eggup`, the eggstack family's verified
//! updater: it fetches the release manifest, checks the SHA-256 of what it
//! downloaded, validates it, and only then replaces the running binary. The
//! same mechanism every other eggstack tool uses, so the behaviour is the same
//! everywhere.
//!
//! The SHA-256 check is an **integrity** check, not an authenticity check: it
//! proves the download was not corrupted, not that it came from us. The
//! release manifest is what says which builds exist.
//!
//! A running server keeps the old binary until it is restarted. Restarting is
//! `eggpass restart`, which the user already knows about: an implicit restart
//! during an update would cut live streams.

use crate::cli::{CommandError, Context};

/// Update to the latest released version, or to one exact version.
pub fn update(context: &Context, version: Option<String>, check: bool) -> Result<(), CommandError> {
    let plan = crate::update::update(&context.config.update, version.as_deref(), check)
        .map_err(|err| CommandError::new(err.to_string()))?;

    if check {
        println!(
            "eggpass {} is installed; {} is published.",
            plan.from_version, plan.to_version
        );
        println!("  target: {}", plan.target);
        println!("  asset:  {}", plan.asset);
        println!("  owner:  {}", plan.provenance.code());
        return Ok(());
    }

    println!(
        "Updated eggpass {} to {}.",
        plan.from_version, plan.to_version
    );
    println!("  target: {}", plan.target);
    println!("  asset:  {}", plan.asset);

    // The daemon is deliberately not restarted: a restart cuts live streams,
    // and the user asked for an update, not an outage.
    let running = crate::daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );
    if matches!(running, crate::daemon::DaemonState::Running { .. }) {
        println!("The running server still has the old binary until it is restarted.");
        println!("Run `eggpass restart` when you are ready.");
    }

    Ok(())
}
