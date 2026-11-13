//! `eggpass` — a lightweight, local, OpenAI-compatible proxy for Grok and
//! ChatGPT.
//!
//! The binary is a single command-line tool: authenticate with a provider over
//! OAuth, then serve an OpenAI-compatible API on loopback that any client can
//! talk to.

mod auth;
mod cli;
mod config;
mod daemon;
mod keys;
mod logging;
mod migrate;
mod paths;
mod provider;
mod server;
mod timeutil;
mod update;
mod upstream;

use cli::args::ParseOutcome;

fn main() {
    restore_default_sigpipe();

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match cli::args::parse(arguments) {
        Ok(ParseOutcome::Help(text)) => {
            print!("{text}");
        }
        Ok(ParseOutcome::Version(version)) => {
            println!("eggpass {version}");
        }
        Ok(ParseOutcome::Run(parsed)) => {
            std::process::exit(cli::run(parsed));
        }
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("Run `eggpass --help` for usage.");
            std::process::exit(1);
        }
    }
}

/// Restore the default `SIGPIPE` behaviour.
///
/// Rust ignores `SIGPIPE`, so writing to a closed pipe raises a panic instead
/// of ending the process. Most command line tools end quietly instead, which
/// is what `eggpass status | head -1` should do.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // SAFETY: setting a signal disposition to its default is safe at any time.
    unsafe {
        nix::libc::signal(nix::libc::SIGPIPE, nix::libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}
