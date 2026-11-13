//! `grokproxy status`.

use crate::auth::store;
use crate::cli::Context;
use crate::daemon::{self, DaemonState};
use crate::timeutil;
use crate::upstream::{Method, UpstreamClient};

/// Report the account state and the daemon state.
///
/// Both sections are always printed, so the output is stable enough to grep.
pub fn status(context: &Context) -> Result<(), crate::cli::CommandError> {
    print_account(context);
    println!();
    print_server(context);
    Ok(())
}

/// Print the account section.
fn print_account(context: &Context) {
    let grok_cli = context.paths.grok_cli_auth_file();
    let auth = match store::load_or_import(&context.paths.auth_file(), grok_cli.as_deref()) {
        Ok(auth) => auth,
        Err(err) => {
            println!("Account:  logged out");
            println!("  error:      {err}");
            return;
        }
    };

    let Some(auth) = auth else {
        println!("Account:  logged out");
        return;
    };

    println!("Account:  logged in");
    println!(
        "  email:      {}",
        auth.email.clone().unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  user id:    {}",
        auth.user_id.clone().unwrap_or_else(|| "-".to_string())
    );

    let expires_at = auth.effective_expires_at();
    let remaining = expires_at - timeutil::now_unix();
    if remaining <= 0 {
        if auth.refresh_token.is_some() {
            println!("  token:      expired — it will be refreshed on the next request");
        } else {
            println!("  token:      expired — run `grokproxy login`");
        }
    } else {
        println!(
            "  token:      valid, expires in {}",
            timeutil::humanize_duration(remaining)
        );
    }
}

/// Print the server section.
fn print_server(context: &Context) {
    let state = daemon::state(&context.paths.pid_file(), context.config.port);

    match state {
        DaemonState::Running { pid, port } => {
            println!("Server:   running");
            println!("  pid:       {pid}");
            println!("  url:       http://127.0.0.1:{port}/v1");
            println!("  upstream:  {}", context.config.upstream.display_name());
            println!("  models:    {}", model_count(context));
        }
        DaemonState::Stale { pid } => {
            println!("Server:   stopped");
            println!("  pid:       {pid} (stale)");
            println!("  url:       {}", context.config.api_base_url());
        }
        DaemonState::Stopped => {
            println!("Server:   stopped");
            println!("  url:       {}", context.config.api_base_url());
        }
    }
}

/// Count the models the upstream offers.
///
/// This needs the network and a valid token, so it is best-effort: a failure
/// must not break `status`.
fn model_count(context: &Context) -> String {
    let grok_cli = context.paths.grok_cli_auth_file();
    let Ok(Some(auth)) = store::load_or_import(&context.paths.auth_file(), grok_cli.as_deref())
    else {
        return "unavailable (not logged in)".to_string();
    };

    if !auth.is_fresh(timeutil::now_unix(), 0) {
        return "unavailable (the token has expired)".to_string();
    }

    let upstream = UpstreamClient::new(context.config.upstream);
    let Ok(mut response) = upstream.send(Method::Get, "/v1/models", &[], None, &auth.access_token)
    else {
        return "unavailable".to_string();
    };

    if response.status != 200 {
        return "unavailable".to_string();
    }

    let Ok(bytes) = response.read_to_vec() else {
        return "unavailable".to_string();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return "unavailable".to_string();
    };
    let Some(data) = value.get("data").and_then(serde_json::Value::as_array) else {
        return "unavailable".to_string();
    };

    format!("{} available", data.len())
}
