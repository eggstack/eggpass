//! A hand-rolled argument parser.
//!
//! Nine subcommands do not justify a parser dependency, so this module does
//! the small amount of work itself: global flags may appear before or after the
//! subcommand, `--help` works per command, and anything unrecognised is a
//! usage error.

use std::path::PathBuf;

/// The subcommand the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Authenticate with x.ai over OAuth.
    Login { device: bool, force: bool },
    /// Remove stored credentials.
    Logout,
    /// Report account and daemon state.
    Status,
    /// Start the server as a background daemon.
    Run { foreground: bool },
    /// Stop the running daemon.
    Stop,
    /// Stop, then start.
    Restart,
    /// Show or change the listen port.
    Port { port: Option<u16> },
    /// Open the configuration file in an editor.
    Edit,
    /// Start the daemon only if it is not already running.
    Croncheck,
}

/// A parsed command line.
#[derive(Debug, Clone)]
pub struct Cli {
    /// The subcommand to run.
    pub command: Command,
    /// Override for the configuration file location.
    pub config_path: Option<PathBuf>,
    /// Raise log verbosity.
    pub verbose: bool,
}

/// What the parser decided the user wants.
#[derive(Debug)]
pub enum ParseOutcome {
    /// Run this command.
    Run(Cli),
    /// Print this text and exit successfully.
    Help(String),
    /// Print this version and exit successfully.
    Version(String),
}

/// A command line that could not be understood.
#[derive(Debug)]
pub struct UsageError {
    /// A one-line explanation.
    pub message: String,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UsageError {}

/// The program version.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Parse command line arguments (excluding the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<ParseOutcome, UsageError> {
    let args: Vec<String> = args.into_iter().collect();

    let mut config_path: Option<PathBuf> = None;
    let mut verbose = false;
    let mut subcommand: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut wants_help = false;

    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].clone();
        match arg.as_str() {
            "--help" | "-h" => {
                wants_help = true;
                index += 1;
            }
            "--version" | "-V" => {
                return Ok(ParseOutcome::Version(version()));
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "--config" => {
                let value = args.get(index + 1).ok_or_else(|| UsageError {
                    message: "--config requires a path".to_string(),
                })?;
                config_path = Some(PathBuf::from(value));
                index += 2;
            }
            other if other.starts_with("--config=") => {
                config_path = Some(PathBuf::from(&other["--config=".len()..]));
                index += 1;
            }
            other if other.starts_with('-') && other != "-" => {
                if subcommand.is_some() {
                    // After the subcommand, unknown options belong to the
                    // subcommand and are checked there.
                    rest.push(other.to_string());
                    index += 1;
                    continue;
                }
                return Err(UsageError {
                    message: format!("unknown option `{other}`"),
                });
            }
            other => {
                if subcommand.is_none() {
                    subcommand = Some(other.to_string());
                } else {
                    rest.push(other.to_string());
                }
                index += 1;
            }
        }
    }

    let Some(name) = subcommand else {
        if wants_help {
            return Ok(ParseOutcome::Help(global_help()));
        }
        return Err(UsageError {
            message: "no command given".to_string(),
        });
    };

    if wants_help {
        return Ok(ParseOutcome::Help(command_help(&name)));
    }

    let command = match name.as_str() {
        "login" => {
            let mut device = false;
            let mut force = false;
            for arg in &rest {
                match arg.as_str() {
                    "--device" => device = true,
                    "--force" => force = true,
                    other => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `grokproxy login`"),
                        });
                    }
                }
            }
            Command::Login { device, force }
        }
        "logout" => {
            expect_no_args(&name, &rest)?;
            Command::Logout
        }
        "status" => {
            expect_no_args(&name, &rest)?;
            Command::Status
        }
        "run" => {
            let mut foreground = false;
            for arg in &rest {
                match arg.as_str() {
                    "--foreground" => foreground = true,
                    other => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `grokproxy run`"),
                        });
                    }
                }
            }
            Command::Run { foreground }
        }
        "stop" => {
            expect_no_args(&name, &rest)?;
            Command::Stop
        }
        "restart" => {
            expect_no_args(&name, &rest)?;
            Command::Restart
        }
        "port" => {
            let mut port = None;
            for arg in &rest {
                match arg.as_str() {
                    other if other.starts_with('-') => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `grokproxy port`"),
                        });
                    }
                    other => {
                        if port.is_some() {
                            return Err(UsageError {
                                message: "`grokproxy port` takes at most one port number"
                                    .to_string(),
                            });
                        }
                        port = Some(parse_port(other)?);
                    }
                }
            }
            Command::Port { port }
        }
        "edit" => {
            expect_no_args(&name, &rest)?;
            Command::Edit
        }
        "croncheck" => {
            expect_no_args(&name, &rest)?;
            Command::Croncheck
        }
        other => {
            return Err(UsageError {
                message: format!("unknown command `{other}`"),
            });
        }
    };

    Ok(ParseOutcome::Run(Cli {
        command,
        config_path,
        verbose,
    }))
}

/// Reject any positional argument for a command that takes none.
fn expect_no_args(name: &str, rest: &[String]) -> Result<(), UsageError> {
    if let Some(extra) = rest.first() {
        return Err(UsageError {
            message: format!("`grokproxy {name}` takes no arguments (got `{extra}`)"),
        });
    }
    Ok(())
}

/// Parse a port number given on the command line.
fn parse_port(value: &str) -> Result<u16, UsageError> {
    let port: u32 = value.parse().map_err(|_| UsageError {
        message: format!("invalid port `{value}` (expected a number between 1 and 65535)"),
    })?;
    if port == 0 || port > 65535 {
        return Err(UsageError {
            message: format!("invalid port `{value}` (expected a number between 1 and 65535)"),
        });
    }
    Ok(port as u16)
}

/// The top-level help text.
pub fn global_help() -> String {
    format!(
        "grokproxy {version} — a local, OpenAI-compatible proxy for Grok\n\
         \n\
         USAGE:\n    \
             grokproxy [OPTIONS] <COMMAND>\n\
         \n\
         COMMANDS:\n    \
             login       Authenticate with x.ai over OAuth\n    \
             logout      Remove stored OAuth credentials\n    \
             status      Show account and daemon state\n    \
             run         Start the server as a background daemon\n    \
             stop        Stop the running daemon\n    \
             restart     Stop the daemon, then start it again\n    \
             port        Show or change the listen port\n    \
             edit        Open the config file in your editor\n    \
             croncheck   Start the daemon only if it is not already running\n\
         \n\
         OPTIONS:\n    \
             --config <path>   Use a different config file\n    \
             --verbose         Log debug messages\n    \
             -h, --help        Show this help\n    \
             -V, --version     Show the version\n",
        version = version()
    )
}

/// Help text for a single command.
pub fn command_help(name: &str) -> String {
    match name {
        "login" => "grokproxy login [--device] [--force]\n\n\
            Authenticate with x.ai over OAuth.\n\n\
            --device   Use the device-code flow (for headless machines and SSH)\n\
            --force    Log in again even if credentials already exist\n"
            .to_string(),
        "logout" => "grokproxy logout\n\nRemove the stored OAuth credentials.\n".to_string(),
        "status" => "grokproxy status\n\nShow account state and daemon state.\n".to_string(),
        "run" => "grokproxy run [--foreground]\n\nStart the server as a background daemon.\n\n\
            --foreground   Stay in the foreground instead of detaching\n"
            .to_string(),
        "stop" => "grokproxy stop\n\nStop the running daemon.\n".to_string(),
        "restart" => "grokproxy restart\n\nStop the daemon, then start it again.\n".to_string(),
        "port" => "grokproxy port [PORT]\n\nShow the listen port, or change it. A changed port\n\
            takes effect on the next `grokproxy restart`.\n"
            .to_string(),
        "edit" => "grokproxy edit\n\nOpen the config file in $EDITOR.\n".to_string(),
        "croncheck" => {
            "grokproxy croncheck\n\nStart the daemon only if it is not already running.\n\
            Safe to use from a crontab; prints nothing.\n"
                .to_string()
        }
        other => format!("unknown command `{other}`\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<ParseOutcome, UsageError> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_every_subcommand() {
        assert!(matches!(
            parse_args(&["login"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Login {
                    device: false,
                    force: false
                },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["login", "--device", "--force"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Login {
                    device: true,
                    force: true
                },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["logout"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Logout,
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["status"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Status,
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["run", "--foreground"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Run { foreground: true },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["stop"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Stop,
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["restart"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Restart,
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["port"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Port { port: None },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["port", "9000"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Port { port: Some(9000) },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["edit"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Edit,
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["croncheck"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Croncheck,
                ..
            })
        ));
    }

    #[test]
    fn global_flags_work_before_and_after_the_subcommand() {
        let before = parse_args(&["--config", "/tmp/x.toml", "status"]).unwrap();
        let after = parse_args(&["status", "--config", "/tmp/x.toml"]).unwrap();
        let ParseOutcome::Run(before) = before else {
            panic!("expected a command")
        };
        let ParseOutcome::Run(after) = after else {
            panic!("expected a command")
        };
        assert_eq!(before.config_path, after.config_path);
        assert_eq!(before.config_path, Some(PathBuf::from("/tmp/x.toml")));
    }

    #[test]
    fn unknown_flags_and_commands_are_usage_errors() {
        assert!(parse_args(&["--nope"]).is_err());
        assert!(parse_args(&["frobnicate"]).is_err());
        assert!(parse_args(&["status", "extra"]).is_err());
    }

    #[test]
    fn ports_are_validated() {
        assert!(parse_args(&["port", "70000"]).is_err());
        assert!(parse_args(&["port", "abc"]).is_err());
        assert!(parse_args(&["port", "0"]).is_err());
    }

    #[test]
    fn help_is_available_per_command() {
        let ParseOutcome::Help(text) = parse_args(&["login", "--help"]).unwrap() else {
            panic!("expected help")
        };
        assert!(text.contains("--device"), "{text}");
    }

    #[test]
    fn version_is_available() {
        assert!(matches!(
            parse_args(&["--version"]).unwrap(),
            ParseOutcome::Version(_)
        ));
    }
}
