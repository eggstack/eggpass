//! A hand-rolled argument parser.
//!
//! Ten subcommands do not justify a parser dependency, so this module does
//! the small amount of work itself: global flags may appear before or after the
//! subcommand, `--help` works per command, and anything unrecognised is a
//! usage error.

use std::path::PathBuf;

use crate::provider::ProviderId;

/// The subcommand the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Authenticate with a provider over OAuth.
    Login {
        /// Use the device-code flow rather than the browser.
        device: bool,
        /// Use the browser flow rather than the device-code flow.
        oauth: bool,
        /// The provider to sign in with, when it was named.
        provider: Option<ProviderId>,
    },
    /// Revoke and remove stored credentials.
    Logout {
        /// The provider to sign out of, when it was named.
        provider: Option<ProviderId>,
    },
    /// Force-refresh a provider's stored credentials.
    Refresh {
        /// The provider to refresh, when it was named.
        provider: Option<ProviderId>,
    },
    /// Report account and daemon state.
    Status,
    /// Report how much of the subscription allowance remains.
    Usage {
        /// The provider to ask, when it was named.
        provider: Option<ProviderId>,
        /// Print the report as JSON.
        json: bool,
    },
    /// Start the server as a background daemon.
    Run {
        /// Stay in the foreground instead of detaching.
        foreground: bool,
    },
    /// Stop the running daemon.
    Stop,
    /// Stop, then start.
    Restart,
    /// Show or change the listen port.
    Port {
        /// The new port, when one was given.
        port: Option<u16>,
    },
    /// Show or change the bind address.
    Host {
        /// The new host, when one was given.
        host: Option<String>,
    },
    /// Open the configuration file in an editor.
    Edit,
    /// Start the daemon only if it is not already running.
    Croncheck,
    /// Update to the latest released version.
    Update {
        /// An exact version to install instead of the latest one.
        version: Option<String>,
        /// Report what would happen without changing anything.
        check: bool,
    },
    /// Issue, list and revoke the keys clients use.
    Key {
        /// What to do with the keys.
        action: KeyAction,
    },
    /// Configure or revoke an EggPool client connection.
    Eggpool {
        /// What to do with the EggPool connection.
        action: EggpoolAction,
    },
}

/// What `eggpass key` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Mint a key and show it once.
    Add {
        /// What the key is for, when the user said.
        label: Option<String>,
    },
    /// Show the keys that exist.
    List,
    /// Take a key away.
    Revoke {
        /// The number of the key to take away.
        id: Option<u32>,
        /// Whether to take every key away.
        all: bool,
    },
}

/// What `eggpass eggpool` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EggpoolAction {
    /// Configure a connection to EggPool.
    Setup {
        /// The EggPool control endpoint.
        control: EggpoolControl,
        /// The TLS server name expected by EggPool.
        server_name: String,
        /// The CA certificate bundle to trust.
        ca: PathBuf,
        /// An optional display label for this connection.
        label: Option<String>,
    },
    /// Revoke the configured EggPool connection.
    Revoke,
}

/// The host and port of an EggPool control endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EggpoolControl {
    /// Hostname or IP address (without IPv6 brackets).
    pub host: String,
    /// TCP port.
    pub port: u16,
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
    let mut provider: Option<ProviderId> = None;
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
            "--provider" => {
                let value = args.get(index + 1).ok_or_else(|| UsageError {
                    message: "--provider requires a provider id".to_string(),
                })?;
                provider = Some(parse_provider(value)?);
                index += 2;
            }
            other if other.starts_with("--provider=") => {
                provider = Some(parse_provider(&other["--provider=".len()..])?);
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
            let mut oauth = false;
            let mut local_provider = provider;
            for arg in &rest {
                match arg.as_str() {
                    "--device" => device = true,
                    "--oauth" => oauth = true,
                    "--provider" => {
                        return Err(UsageError {
                            message: "`--provider` is a global option; put it before the command"
                                .to_string(),
                        });
                    }
                    other if other.starts_with("--provider=") => {
                        local_provider = Some(parse_provider(&other["--provider=".len()..])?);
                    }
                    other => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `eggpass login`"),
                        });
                    }
                }
            }
            if device && oauth {
                return Err(UsageError {
                    message: "`--device` and `--oauth` ask for opposite things; pick one"
                        .to_string(),
                });
            }
            Command::Login {
                device,
                oauth,
                provider: local_provider,
            }
        }
        "logout" => {
            for arg in &rest {
                match arg.as_str() {
                    "--provider" => {
                        return Err(UsageError {
                            message: "`--provider` is a global option; put it before the command"
                                .to_string(),
                        });
                    }
                    other if other.starts_with("--provider=") => {
                        provider = Some(parse_provider(&other["--provider=".len()..])?);
                    }
                    other => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `eggpass logout`"),
                        });
                    }
                }
            }
            Command::Logout { provider }
        }
        "refresh" => {
            expect_no_args(&name, &rest)?;
            Command::Refresh { provider }
        }
        "status" => {
            expect_no_args(&name, &rest)?;
            Command::Status
        }
        "usage" => {
            let mut json = false;
            for arg in &rest {
                if arg == "--json" {
                    json = true;
                    continue;
                }
                if arg.starts_with('-') {
                    return Err(UsageError {
                        message: format!("unknown option `{arg}` for `eggpass usage`"),
                    });
                }
                return Err(UsageError {
                    message: format!("`eggpass usage` takes no arguments (got `{arg}`)"),
                });
            }
            Command::Usage { provider, json }
        }
        "run" => {
            let mut foreground = false;
            for arg in &rest {
                match arg.as_str() {
                    "--foreground" => foreground = true,
                    other => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `eggpass run`"),
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
                            message: format!("unknown option `{other}` for `eggpass port`"),
                        });
                    }
                    other => {
                        if port.is_some() {
                            return Err(UsageError {
                                message: "`eggpass port` takes at most one port number".to_string(),
                            });
                        }
                        port = Some(parse_port(other)?);
                    }
                }
            }
            Command::Port { port }
        }
        "host" => {
            let mut host = None;
            for arg in &rest {
                match arg.as_str() {
                    other if other.starts_with('-') => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `eggpass host`"),
                        });
                    }
                    other => {
                        if host.is_some() {
                            return Err(UsageError {
                                message: "`eggpass host` takes at most one address".to_string(),
                            });
                        }
                        host = Some(other.to_string());
                    }
                }
            }
            Command::Host { host }
        }
        "edit" => {
            expect_no_args(&name, &rest)?;
            Command::Edit
        }
        "croncheck" => {
            expect_no_args(&name, &rest)?;
            Command::Croncheck
        }
        "update" => {
            let mut version = None;
            let mut check = false;
            for arg in &rest {
                match arg.as_str() {
                    "--check" | "--dry-run" => check = true,
                    other if other.starts_with('-') => {
                        return Err(UsageError {
                            message: format!("unknown option `{other}` for `eggpass update`"),
                        });
                    }
                    other => {
                        if version.is_some() {
                            return Err(UsageError {
                                message: "`eggpass update` takes at most one version".to_string(),
                            });
                        }
                        version = Some(other.to_string());
                    }
                }
            }
            Command::Update { version, check }
        }
        "key" => {
            let action = parse_key_action(&rest)?;
            Command::Key { action }
        }
        "eggpool" => {
            if provider.is_some() {
                return Err(UsageError {
                    message: "`eggpass eggpool` discovers providers through `/v1/models` and does not accept `--provider`".to_string(),
                });
            }
            let action = parse_eggpool_action(&rest)?;
            Command::Eggpool { action }
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
            message: format!("`eggpass {name}` takes no arguments (got `{extra}`)"),
        });
    }
    Ok(())
}

/// Parse a provider id given on the command line.
fn parse_provider(value: &str) -> Result<ProviderId, UsageError> {
    ProviderId::parse(value).ok_or_else(|| UsageError {
        message: format!("unknown provider `{value}` (expected \"xai\" or \"openai\")"),
    })
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

/// Parse the arguments of `eggpass key`.
///
/// With no arguments at all the keys are listed, which is what the user most
/// often wants to see.
fn parse_key_action(rest: &[String]) -> Result<KeyAction, UsageError> {
    let Some(first) = rest.first() else {
        return Ok(KeyAction::List);
    };

    match first.as_str() {
        "add" => {
            if let Some(extra) = rest.get(2) {
                return Err(UsageError {
                    message: format!("`eggpass key add` takes at most one label (got `{extra}`)"),
                });
            }
            Ok(KeyAction::Add {
                label: rest.get(1).cloned(),
            })
        }
        "list" => {
            if let Some(extra) = rest.get(1) {
                return Err(UsageError {
                    message: format!("`eggpass key list` takes no arguments (got `{extra}`)"),
                });
            }
            Ok(KeyAction::List)
        }
        "revoke" => {
            let mut id = None;
            let mut all = false;
            for arg in &rest[1..] {
                if arg == "--all" {
                    all = true;
                    continue;
                }
                if arg.starts_with('-') {
                    return Err(UsageError {
                        message: format!("unknown option `{arg}` for `eggpass key revoke`"),
                    });
                }
                if id.is_some() {
                    return Err(UsageError {
                        message: "`eggpass key revoke` takes at most one key number".to_string(),
                    });
                }
                id = Some(parse_key_number(arg)?);
            }
            if id.is_none() && !all {
                return Err(UsageError {
                    message: "`eggpass key revoke` needs the number of a key, or `--all`"
                        .to_string(),
                });
            }
            Ok(KeyAction::Revoke { id, all })
        }
        other => Err(UsageError {
            message: format!(
                "unknown action `{other}` for `eggpass key` (expected \"add\", \"list\" or \"revoke\")"
            ),
        }),
    }
}

/// Parse the arguments of `eggpass eggpool`.
fn parse_eggpool_action(rest: &[String]) -> Result<EggpoolAction, UsageError> {
    let mut revoke = false;
    let mut setup_flag_seen = false;
    let mut control = None;
    let mut server_name = None;
    let mut ca = None;
    let mut label = None;
    let mut index = 0usize;

    while index < rest.len() {
        let arg = rest[index].as_str();
        match arg {
            "--revoke" => {
                if revoke {
                    return Err(UsageError {
                        message: "`--revoke` may only be specified once".to_string(),
                    });
                }
                revoke = true;
                index += 1;
            }
            _ if arg == "--control" || arg.starts_with("--control=") => {
                setup_flag_seen = true;
                if control.is_some() {
                    return Err(UsageError {
                        message: "`--control` may only be specified once".to_string(),
                    });
                }
                let value = eggpool_option_value(rest, &mut index, "--control", arg)?;
                control = Some(parse_eggpool_control(value)?);
            }
            _ if arg == "--server-name" || arg.starts_with("--server-name=") => {
                setup_flag_seen = true;
                if server_name.is_some() {
                    return Err(UsageError {
                        message: "`--server-name` may only be specified once".to_string(),
                    });
                }
                let value = eggpool_option_value(rest, &mut index, "--server-name", arg)?;
                server_name = Some(value.to_string());
            }
            _ if arg == "--ca" || arg.starts_with("--ca=") => {
                setup_flag_seen = true;
                if ca.is_some() {
                    return Err(UsageError {
                        message: "`--ca` may only be specified once".to_string(),
                    });
                }
                let value = eggpool_option_value(rest, &mut index, "--ca", arg)?;
                ca = Some(PathBuf::from(value));
            }
            _ if arg == "--label" || arg.starts_with("--label=") => {
                setup_flag_seen = true;
                if label.is_some() {
                    return Err(UsageError {
                        message: "`--label` may only be specified once".to_string(),
                    });
                }
                let value = eggpool_option_value(rest, &mut index, "--label", arg)?;
                label = Some(value.to_string());
            }
            _ if arg.starts_with('-') => {
                return Err(UsageError {
                    message: format!("unknown option `{arg}` for `eggpass eggpool`"),
                });
            }
            _ => {
                return Err(UsageError {
                    message: format!(
                        "`eggpass eggpool` does not take positional arguments (got `{arg}`)"
                    ),
                });
            }
        }
    }

    if revoke {
        if setup_flag_seen {
            return Err(UsageError {
                message: "`--revoke` cannot be combined with EggPool setup options".to_string(),
            });
        }
        return Ok(EggpoolAction::Revoke);
    }

    let mut missing = Vec::new();
    if control.is_none() {
        missing.push("--control");
    }
    if server_name.is_none() {
        missing.push("--server-name");
    }
    if ca.is_none() {
        missing.push("--ca");
    }
    if !missing.is_empty() {
        return Err(UsageError {
            message: format!("`eggpass eggpool` setup requires {}", missing.join(", ")),
        });
    }

    Ok(EggpoolAction::Setup {
        control: control.expect("required above"),
        server_name: server_name.expect("required above"),
        ca: ca.expect("required above"),
        label,
    })
}

/// Read a required EggPool option value in either `--option VALUE` or `--option=VALUE` form.
fn eggpool_option_value<'a>(
    rest: &'a [String],
    index: &mut usize,
    option: &str,
    arg: &'a str,
) -> Result<&'a str, UsageError> {
    let value = if arg == option {
        let value = rest.get(*index + 1).ok_or_else(|| UsageError {
            message: format!("`{option}` requires a value"),
        })?;
        if value.starts_with("--") {
            return Err(UsageError {
                message: format!("`{option}` requires a value"),
            });
        }
        *index += 2;
        value.as_str()
    } else {
        *index += 1;
        arg.strip_prefix(&format!("{option}=")).unwrap_or_default()
    };
    if value.is_empty() {
        return Err(UsageError {
            message: format!("`{option}` requires a non-empty value"),
        });
    }
    Ok(value)
}

/// Parse and validate an EggPool `HOST:PORT` control endpoint.
fn parse_eggpool_control(value: &str) -> Result<EggpoolControl, UsageError> {
    let invalid = || UsageError {
        message: format!("invalid EggPool control address `{value}` (expected HOST:PORT)"),
    };
    let (host, port) = if value.starts_with('[') {
        let close = value.find(']').ok_or_else(invalid)?;
        let host = &value[1..close];
        let port = value[close + 1..].strip_prefix(':').ok_or_else(invalid)?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(invalid());
        }
        (host, port)
    } else {
        let (host, port) = value.split_once(':').ok_or_else(invalid)?;
        if host.contains(':') {
            return Err(invalid());
        }
        (host, port)
    };
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let port = parse_port(port)?;
    Ok(EggpoolControl {
        host: host.to_string(),
        port,
    })
}

/// Parse the number of a key given on the command line.
fn parse_key_number(value: &str) -> Result<u32, UsageError> {
    let number: u32 = value.parse().map_err(|_| UsageError {
        message: format!("invalid key number `{value}` (expected a number from 1 upwards)"),
    })?;
    if number == 0 {
        return Err(UsageError {
            message: format!("invalid key number `{value}` (expected a number from 1 upwards)"),
        });
    }
    Ok(number)
}

/// The top-level help text.
pub fn global_help() -> String {
    format!(
        "eggpass {version} — a local, OpenAI-compatible proxy for Grok and ChatGPT\n\
         \n\
         USAGE:\n    \
             eggpass [OPTIONS] <COMMAND>\n\
         \n\
         COMMANDS:\n    \
             login       Authenticate with a provider over OAuth\n    \
             logout      Revoke and remove stored credentials\n    \
             refresh     Force-refresh a provider's stored credentials\n    \
             status      Show account and daemon state\n    \
             usage       Show how much of the subscription allowance remains\n    \
             run         Start the server as a background daemon\n    \
             stop        Stop the running daemon\n    \
             restart     Stop the daemon, then start it again\n    \
             port        Show or change the listen port\n    \
             host        Show or change the bind address\n    \
             edit        Open the config file in your editor\n    \
             croncheck   Start the daemon only if it is not already running\n    \
             update      Update to the latest released version\n    \
             key         Issue, list and revoke the keys clients use\n             eggpool     Configure or revoke an EggPool connection\n\
         \n\
         OPTIONS:\n    \
             --config <path>     Use a different config file\n    \
             --provider <id>     Name the provider directly (xai or openai)\n    \
             --verbose           Log debug messages\n    \
             -h, --help          Show this help\n    \
             -V, --version       Show the version\n",
        version = version()
    )
}

/// Help text for a single command.
pub fn command_help(name: &str) -> String {
    match name {
        "login" => "eggpass login [--device | --oauth]\n\n\
            Authenticate with a provider over OAuth. Without --provider a picker\n\
            lists the providers to choose from.\n\n\
            --device   Use the device-code flow (x.ai only, for headless machines)\n\
            --oauth    Use the browser flow (x.ai only, when the device flow is\n\
                       unavailable or you want the browser redirect)\n"
            .to_string(),
        "logout" => "eggpass logout\n\n\
            Revoke and remove the stored credentials. Without --provider a picker\n\
            lists the providers you are signed in to.\n"
            .to_string(),
        "refresh" => "eggpass refresh [--provider ID]\n\nForce-refresh stored credentials without sending an inference request.\nWithout --provider, choose from signed-in providers.\n".to_string(),
        "status" => "eggpass status\n\nShow account state and daemon state.\n".to_string(),
        "usage" => {
            "eggpass usage [--json]\n\n\
            Show how much of the subscription allowance remains. Each provider\n\
            is asked once, on this run, and the answer is printed and the\n\
            command ends. Without --provider every provider you are signed in\n\
            to is asked.\n\n\
             --json   Print the report as JSON instead of the bars\n"
                .to_string()
        }
        "run" => "eggpass run [--foreground]\n\nStart the server as a background daemon.\n\n\
            --foreground   Stay in the foreground instead of detaching\n"
            .to_string(),
        "stop" => "eggpass stop\n\nStop the running daemon.\n".to_string(),
        "restart" => "eggpass restart\n\nStop the daemon, then start it again.\n".to_string(),
        "port" => "eggpass port [PORT]\n\nShow the listen port, or change it. A changed port is\n\
            applied and the server is restarted for you.\n"
            .to_string(),
        "host" => "eggpass host [ADDRESS]\n\nShow the bind address, or change it. A changed address is\n\
            applied and the server is restarted for you. Loopback is the default.\n"
            .to_string(),
        "edit" => "eggpass edit\n\nOpen the config file in $EDITOR.\n".to_string(),
        "croncheck" => {
            "eggpass croncheck\n\nStart the daemon only if it is not already running.\n\
            Safe to use from a crontab; prints nothing.\n"
                .to_string()
        }
        "update" => {
            "eggpass update [VERSION]\n\nUpdate to the latest released version, or to one exact\n            version. A running server keeps the old binary until it is restarted.\n\n\
             \x20           --check   Report what would happen, change nothing\n"
                .to_string()
        }
        "key" => {
            "eggpass key [add | list | revoke]\n\n\
            Issue, list and revoke the keys your clients use. A key is shown\n\
            once, when it is created, and only its hash is stored.\n\n\
            add [LABEL]   Mint a key and show it once\n\
            list          Show the keys, with their numbers and visible parts\n\
            revoke <N>    Revoke the key numbered N\n\
            revoke --all  Revoke every key\n"
                .to_string()
        }
        "eggpool" => "eggpass eggpool [--control HOST:PORT --server-name NAME --ca PATH --label NAME]\n\n\
            eggpass eggpool --revoke\n\n\
            Configure EggPool and print a one-time enrollment ticket;\n\
            the CA bundle stays local, and the EggPass listener must remain on loopback.\n\n\
            --control HOST:PORT   EggPool control endpoint\n\
            --server-name NAME   TLS server name expected by EggPool\n\
            --ca PATH            CA certificate bundle to trust\n\
            --label NAME         Optional connection label\n\
            --revoke             Revoke the configured EggPool connection\n"
            .to_string(),
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
                    oauth: false,
                    provider: None
                },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["login", "--device"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Login {
                    device: true,
                    oauth: false,
                    provider: None
                },
                ..
            })
        ));
        assert!(matches!(
            parse_args(&["logout"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Logout { provider: None },
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
            parse_args(&["host"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Host { host: None },
                ..
            })
        ));
        let ParseOutcome::Run(cli) = parse_args(&["host", "0.0.0.0"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Host {
                host: Some("0.0.0.0".to_string())
            }
        );
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
    fn the_provider_can_be_named_globally() {
        let ParseOutcome::Run(cli) = parse_args(&["--provider", "openai", "login"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Login {
                device: false,
                oauth: false,
                provider: Some(ProviderId::OpenAi)
            }
        );
    }

    #[test]
    fn the_provider_can_be_named_with_an_equals_sign() {
        let ParseOutcome::Run(cli) = parse_args(&["login", "--provider=xai"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Login {
                device: false,
                oauth: false,
                provider: Some(ProviderId::Xai)
            }
        );
    }

    #[test]
    fn refresh_accepts_a_provider_before_or_after_the_command() {
        for args in [
            vec!["--provider", "openai", "refresh"],
            vec!["refresh", "--provider=openai"],
        ] {
            let ParseOutcome::Run(cli) = parse_args(&args).unwrap() else {
                panic!("expected a command")
            };
            assert_eq!(
                cli.command,
                Command::Refresh {
                    provider: Some(ProviderId::OpenAi)
                }
            );
        }
        assert!(parse_args(&["refresh", "extra"]).is_err());
    }

    #[test]
    fn an_unknown_provider_is_a_usage_error() {
        assert!(parse_args(&["--provider", "nope", "login"]).is_err());
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
    fn usage_help_describes_remaining_usage() {
        assert!(
            global_help()
                .contains("usage       Show how much of the subscription allowance remains")
        );
        let usage = command_help("usage");
        assert!(usage.contains("allowance remains"), "{usage}");
        assert!(!usage.contains("allowance is used"), "{usage}");
    }

    #[test]
    fn refresh_has_command_help() {
        let ParseOutcome::Help(text) = parse_args(&["refresh", "--help"]).unwrap() else {
            panic!("expected help")
        };
        assert!(text.contains("Force-refresh stored credentials"), "{text}");
        assert!(text.contains("--provider"), "{text}");
    }

    #[test]
    fn version_is_available() {
        assert!(matches!(
            parse_args(&["--version"]).unwrap(),
            ParseOutcome::Version(_)
        ));
    }

    #[test]
    fn the_key_command_is_parsed() {
        let key_of = |args: &[&str]| -> KeyAction {
            let ParseOutcome::Run(cli) = parse_args(args).unwrap() else {
                panic!("expected a command")
            };
            match cli.command {
                Command::Key { action } => action,
                other => panic!("expected a key command, got {other:?}"),
            }
        };

        assert_eq!(key_of(&["key"]), KeyAction::List);
        assert_eq!(key_of(&["key", "list"]), KeyAction::List);
        assert_eq!(key_of(&["key", "add"]), KeyAction::Add { label: None });
        assert_eq!(
            key_of(&["key", "add", "laptop"]),
            KeyAction::Add {
                label: Some("laptop".to_string())
            }
        );
        assert_eq!(
            key_of(&["key", "revoke", "2"]),
            KeyAction::Revoke {
                id: Some(2),
                all: false
            }
        );
        assert_eq!(
            key_of(&["key", "revoke", "--all"]),
            KeyAction::Revoke {
                id: None,
                all: true
            }
        );
    }

    #[test]
    fn a_mistyped_key_command_is_a_usage_error() {
        assert!(parse_args(&["key", "mint"]).is_err());
        assert!(parse_args(&["key", "revoke"]).is_err());
        assert!(parse_args(&["key", "revoke", "0"]).is_err());
        assert!(parse_args(&["key", "revoke", "abc"]).is_err());
        assert!(parse_args(&["key", "revoke", "1", "2"]).is_err());
        assert!(parse_args(&["key", "revoke", "--nope"]).is_err());
        assert!(parse_args(&["key", "list", "extra"]).is_err());
        assert!(parse_args(&["key", "add", "one", "two"]).is_err());
    }

    #[test]
    fn the_key_command_has_its_own_help() {
        let ParseOutcome::Help(text) = parse_args(&["key", "--help"]).unwrap() else {
            panic!("expected help")
        };
        assert!(text.contains("revoke"), "{text}");
    }

    #[test]
    fn usage_parses() {
        assert!(matches!(
            parse_args(&["usage"]).unwrap(),
            ParseOutcome::Run(Cli {
                command: Command::Usage {
                    provider: None,
                    json: false
                },
                ..
            })
        ));
    }

    #[test]
    fn usage_takes_a_provider_after_the_subcommand() {
        let ParseOutcome::Run(cli) = parse_args(&["usage", "--provider", "xai"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Usage {
                provider: Some(ProviderId::Xai),
                json: false
            }
        );
    }

    #[test]
    fn usage_takes_a_provider_before_the_subcommand() {
        let ParseOutcome::Run(cli) = parse_args(&["--provider", "xai", "usage"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Usage {
                provider: Some(ProviderId::Xai),
                json: false
            }
        );
    }

    #[test]
    fn usage_takes_json() {
        let ParseOutcome::Run(cli) = parse_args(&["usage", "--json"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Usage {
                provider: None,
                json: true
            }
        );
    }

    #[test]
    fn usage_takes_no_arguments() {
        assert!(parse_args(&["usage", "extra"]).is_err());
        assert!(parse_args(&["usage", "--nope"]).is_err());
    }

    #[test]
    fn an_unknown_provider_is_an_error() {
        assert!(parse_args(&["usage", "--provider", "nonesuch"]).is_err());
    }

    #[test]
    fn the_usage_command_has_its_own_help() {
        let ParseOutcome::Help(text) = parse_args(&["usage", "--help"]).unwrap() else {
            panic!("expected help")
        };
        assert!(text.contains("--json"), "{text}");
    }

    #[test]
    fn eggpool_setup_and_revoke_actions_are_parsed() {
        let ParseOutcome::Run(cli) = parse_args(&[
            "eggpool",
            "--control",
            "pool.example:8443",
            "--server-name",
            "eggpool.internal",
            "--ca",
            "/etc/ssl/eggpool.pem",
            "--label",
            "work",
        ])
        .unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Eggpool {
                action: EggpoolAction::Setup {
                    control: EggpoolControl {
                        host: "pool.example".to_string(),
                        port: 8443,
                    },
                    server_name: "eggpool.internal".to_string(),
                    ca: PathBuf::from("/etc/ssl/eggpool.pem"),
                    label: Some("work".to_string()),
                },
            }
        );

        let ParseOutcome::Run(cli) = parse_args(&["eggpool", "--revoke"]).unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Eggpool {
                action: EggpoolAction::Revoke,
            }
        );
    }

    #[test]
    fn eggpool_setup_label_is_optional_and_equals_values_work() {
        let ParseOutcome::Run(cli) = parse_args(&[
            "eggpool",
            "--control=127.0.0.1:7000",
            "--server-name=pool.internal",
            "--ca=/tmp/ca.pem",
        ])
        .unwrap() else {
            panic!("expected a command")
        };
        assert_eq!(
            cli.command,
            Command::Eggpool {
                action: EggpoolAction::Setup {
                    control: EggpoolControl {
                        host: "127.0.0.1".to_string(),
                        port: 7000,
                    },
                    server_name: "pool.internal".to_string(),
                    ca: PathBuf::from("/tmp/ca.pem"),
                    label: None,
                },
            }
        );
    }

    #[test]
    fn invalid_eggpool_option_combinations_are_usage_errors() {
        for args in [
            vec!["eggpool", "--revoke", "--control", "pool:443"],
            vec!["eggpool", "--revoke", "--server-name", "pool"],
            vec!["eggpool", "--revoke", "--ca", "ca.pem"],
            vec!["eggpool", "--revoke", "--label", "pool"],
            vec!["eggpool", "--control", "pool:443", "--server-name", "pool"],
            vec!["eggpool", "--control", "pool:443", "--ca", "ca.pem"],
            vec!["eggpool", "--server-name", "pool", "--ca", "ca.pem"],
            vec![
                "eggpool",
                "--control",
                "pool:0",
                "--server-name",
                "pool",
                "--ca",
                "ca.pem",
            ],
            vec![
                "eggpool",
                "--control",
                "pool:443",
                "--server-name",
                "pool",
                "--ca",
                "ca.pem",
                "--provider",
                "xai",
            ],
        ] {
            assert!(parse_args(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn eggpool_help_describes_both_actions_and_global_help_lists_it() {
        assert!(global_help().contains("eggpool     Configure or revoke an EggPool connection"));
        let ParseOutcome::Help(text) = parse_args(&["eggpool", "--help"]).unwrap() else {
            panic!("expected help")
        };
        assert!(text.contains("--control HOST:PORT"), "{text}");
        assert!(text.contains("--revoke"), "{text}");
        assert!(text.contains("one-time enrollment ticket"), "{text}");
        assert!(!text.contains("/v1/models"), "{text}");
        assert!(!text.contains("--provider"), "{text}");
    }
}
