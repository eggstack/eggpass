//! `grokproxy login` and `grokproxy logout`.

use crate::auth::client::{AuthError, OAuthClient};
use crate::auth::flow::{self, FlowError, LoginOutcome};
use crate::auth::store;
use crate::auth::{FALLBACK_REFERRER, FALLBACK_SCOPES};
use crate::cli::{CommandError, Context};
use crate::timeutil;

/// Authenticate with x.ai over OAuth.
pub fn login(context: &Context, device: bool, force: bool) -> Result<(), CommandError> {
    let auth_file = context.paths.auth_file();

    if !force {
        let grok_cli = context.paths.grok_cli_auth_file();
        let existing = store::load_or_import(&auth_file, grok_cli.as_deref())
            .map_err(|err| CommandError::new(format!("could not read the credentials: {err}")))?;
        if let Some(existing) = existing {
            println!("Already logged in as {}.", existing.account_label());
            println!("The access token {}.", describe_expiry(&existing));
            println!("Run `grokproxy login --force` to log in again.");
            return Err(CommandError::silent(2));
        }
    }

    let outcome = login_with_fallback(context, device)?;

    store::save(&auth_file, &outcome.auth)
        .map_err(|err| CommandError::new(format!("could not write the credentials: {err}")))?;

    println!("Logged in as {}.", outcome.auth.account_label());
    println!("The access token {}.", describe_expiry(&outcome.auth));
    println!("Credentials stored in {}.", auth_file.display());
    Ok(())
}

/// Run the login flow, retrying once with the fallback `referrer` and scopes.
///
/// xAI tracks the `referrer` parameter and may one day refuse an unknown
/// value; `grok-build` is the value the official CLI sends, so it is the
/// safest fallback.
fn login_with_fallback(context: &Context, device: bool) -> Result<LoginOutcome, CommandError> {
    let first = build_client(
        &context.config.oauth.client_id,
        &context.config.oauth.issuer,
        &context.config.oauth.referrer,
        &context.config.oauth.scopes,
    );
    match attempt(&first, device) {
        Ok(outcome) => Ok(outcome),
        Err(err) if is_retryable(&err) => {
            eprintln!("The login was refused; retrying with the official client settings.");
            let second = build_client(
                &context.config.oauth.client_id,
                &context.config.oauth.issuer,
                FALLBACK_REFERRER,
                &FALLBACK_SCOPES
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect::<Vec<_>>(),
            );
            attempt(&second, device).map_err(|err| CommandError::new(err.to_string()))
        }
        Err(err) => Err(CommandError::new(err.to_string())),
    }
}

/// Build the OAuth client.
fn build_client(client_id: &str, issuer: &str, referrer: &str, scopes: &[String]) -> OAuthClient {
    let scopes: Vec<&str> = scopes.iter().map(|scope| scope.as_str()).collect();
    OAuthClient::new(client_id, issuer, &scopes, referrer)
}

/// Run one attempt of the chosen flow.
fn attempt(client: &OAuthClient, device: bool) -> Result<LoginOutcome, FlowError> {
    if device {
        flow::login_device(client)
    } else {
        flow::login_loopback(client)
    }
}

/// Whether a failure is worth retrying with the fallback settings.
///
/// Only errors that blame the client settings qualify. A refused login, an
/// expired device code or a pending approval must not be retried: the user
/// has to act, and a second attempt would only confuse them.
fn is_retryable(err: &FlowError) -> bool {
    match err {
        FlowError::Auth(AuthError::OAuth { error, .. }) => matches!(
            error.as_str(),
            "invalid_client" | "unauthorized_client" | "invalid_scope" | "invalid_request"
        ),
        _ => false,
    }
}

/// Describe how long the access token is good for.
pub fn describe_expiry(auth: &store::StoredAuth) -> String {
    let expires_at = auth.effective_expires_at();
    let remaining = expires_at - timeutil::now_unix();
    if remaining <= 0 {
        return "has expired".to_string();
    }
    format!("is valid for {}", timeutil::humanize_duration(remaining))
}

/// Remove the stored credentials.
pub fn logout(context: &Context) -> Result<(), CommandError> {
    let auth_file = context.paths.auth_file();
    let removed = store::remove(&auth_file)
        .map_err(|err| CommandError::new(format!("could not remove the credentials: {err}")))?;

    if removed {
        println!("Logged out.");
    } else {
        println!("Not logged in.");
    }

    // The daemon keeps running, but it will start answering 401.
    let state = crate::daemon::state(&context.paths.pid_file(), context.config.port);
    if state.is_running() {
        println!("The server is still running and will answer 401 until you log in again.");
    }

    Ok(())
}
