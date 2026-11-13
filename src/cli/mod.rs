//! Command dispatch and the shared command context.

pub mod args;
pub mod commands;

use crate::config::Config;
use crate::paths::Paths;

pub use args::{Cli, Command};

/// Everything a command needs to do its job.
#[derive(Debug)]
pub struct Context {
    /// Where the files live.
    pub paths: Paths,
    /// The effective configuration.
    pub config: Config,
    /// The parsed command line.
    pub cli: Cli,
}

/// A command that failed.
#[derive(Debug)]
pub struct CommandError {
    /// What to tell the user. `None` means "say nothing".
    pub message: Option<String>,
    /// The exit code.
    pub code: i32,
}

impl CommandError {
    /// A failure with a message and exit code 1.
    pub fn new(message: impl Into<String>) -> CommandError {
        CommandError {
            message: Some(message.into()),
            code: 1,
        }
    }

    /// A failure with no message and a custom exit code.
    pub fn silent(code: i32) -> CommandError {
        CommandError {
            message: None,
            code,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.message {
            Some(message) => write!(f, "{message}"),
            None => Ok(()),
        }
    }
}

/// Run a command and return the process exit code.
pub fn run(cli: Cli) -> i32 {
    let paths = match Paths::resolve(cli.config_path.as_deref()) {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    let config = match Config::load(paths.config_file()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: {err}");
            return 1;
        }
    };

    // `croncheck` runs from a crontab, so it must stay off standard error.
    let quiet = matches!(cli.command, Command::Croncheck);

    // The log file is best-effort: a read-only home directory must not stop
    // the user from running a command.
    crate::logging::init(
        Some(&paths.log_file()),
        config.log_level,
        cli.verbose,
        !quiet,
    );

    // Make sure the configuration directory exists before anything writes to it.
    if let Err(err) = paths.ensure_dir() {
        eprintln!("error: {err}");
        return 1;
    }

    let context = Context { paths, config, cli };

    match dispatch(&context) {
        Ok(()) => 0,
        Err(err) => {
            if let Some(message) = &err.message {
                crate::logging::warn(message);
                eprintln!("error: {message}");
            }
            err.code
        }
    }
}

/// Send the command to its handler.
fn dispatch(context: &Context) -> Result<(), CommandError> {
    match &context.cli.command {
        Command::Login { device, force } => commands::login::login(context, *device, *force),
        Command::Logout => commands::login::logout(context),
        Command::Status => commands::status::status(context),
        Command::Run { foreground } => commands::run::run(context, *foreground),
        Command::Stop => commands::run::stop(context),
        Command::Restart => commands::run::restart(context),
        Command::Port { port } => commands::port::port(context, *port),
        Command::Edit => commands::edit::edit(context),
        Command::Croncheck => commands::run::croncheck(context),
    }
}
