//! `eggpass status`.

use crate::auth::store;
use crate::cli::Context;
use crate::daemon::{self, DaemonState};
use crate::provider::ProviderId;
use crate::timeutil;
use crate::upstream::Method;

/// Report the account state and the daemon state.
///
/// Both sections are always printed, so the output is stable enough to grep.
pub fn status(context: &Context) -> Result<(), crate::cli::CommandError> {
    print_accounts(context);
    println!();
    print_server(context);
    Ok(())
}

/// Print the account section, one entry per provider.
fn print_accounts(context: &Context) {
    println!("Accounts:");

    for provider in ProviderId::all() {
        let label = format!("  {:<8}", provider.display_name());
        let path = context.paths.credential_file(provider.as_str());

        let auth = match store::load(&path) {
            Ok(auth) => auth,
            Err(err) => {
                println!("{label}error reading the credential file: {err}");
                continue;
            }
        };

        let Some(auth) = auth else {
            println!("{label}logged out");
            continue;
        };

        println!("{label}logged in");
        println!(
            "    email:    {}",
            auth.email.clone().unwrap_or_else(|| "-".to_string())
        );
        println!(
            "    user id:  {}",
            auth.user_id.clone().unwrap_or_else(|| "-".to_string())
        );

        if provider == ProviderId::OpenAi {
            println!(
                "    plan:     {}",
                if auth.plan_enabled {
                    "ChatGPT plan usage enabled"
                } else {
                    "ChatGPT plan usage not granted"
                }
            );
        }

        let expires_at = auth.effective_expires_at();
        let remaining = expires_at - timeutil::now_unix();
        if remaining <= 0 {
            if auth.refresh_token.is_some() {
                println!("    token:    expired — it will be refreshed on the next request");
            } else {
                println!("    token:    expired — run `eggpass login`");
            }
        } else {
            println!(
                "    token:    valid, expires in {}",
                timeutil::humanize_duration(remaining)
            );
        }
    }
}

/// Print the server section.
fn print_server(context: &Context) {
    let state = daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );

    match state {
        DaemonState::Running { pid, host, port } => {
            println!("Server:   running");
            println!("  pid:       {pid}");
            println!(
                "  url:       http://{}:{port}/v1",
                daemon::probe_host(&host)
            );
            println!("  host:      {host}");
            println!("  port:      {port}");
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

/// Count the models the upstreams offer.
///
/// This needs the network and a valid token, so it is best-effort: a failure
/// must not break `status`.
fn model_count(context: &Context) -> String {
    let mut total = 0usize;
    let mut counted = false;

    for provider in ProviderId::all() {
        let path = context.paths.credential_file(provider.as_str());
        let Ok(Some(auth)) = store::load(&path) else {
            continue;
        };
        if !auth.is_fresh(timeutil::now_unix(), 0) {
            continue;
        }

        let spec = crate::provider::spec_for(provider, &context.config);
        let client = crate::upstream::UpstreamClient::new(&spec);
        let Ok(mut response) =
            client.send(Method::Get, "/v1/models", &[], None, &auth.access_token)
        else {
            continue;
        };
        if response.status != 200 {
            continue;
        }
        let Ok(bytes) = response.read_to_vec() else {
            continue;
        };

        let ids = crate::provider::catalogue_ids(spec.catalogue, &bytes);
        total += ids.len();
        counted = true;
    }

    if !counted {
        return "unavailable".to_string();
    }
    format!("{total} available")
}
