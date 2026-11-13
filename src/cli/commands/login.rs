//! `eggpass login` and `eggpass logout`.

use crate::auth::client::AuthError;
use crate::auth::flow::{self, FlowError, LoginOutcome};
use crate::auth::store;
use crate::auth::{FALLBACK_REFERRER, FALLBACK_SCOPES};
use crate::cli::prompt::{self, Choice};
use crate::cli::{CommandError, Context};
use crate::provider::{ProviderId, ProviderRuntime};
use crate::timeutil;

/// Authenticate with a provider over OAuth.
///
/// Signing in to a provider that is already signed in **replaces** the stored
/// credential: there is no "already logged in" refusal, and no `--force`
/// flag. The write is atomic, so an interrupted re-login leaves the previous
/// credential intact.
pub fn login(
    context: &Context,
    device: bool,
    provider: Option<ProviderId>,
) -> Result<(), CommandError> {
    let provider = match provider {
        Some(provider) => provider,
        None => match choose_provider(context, "Select a provider to sign in with:")? {
            Some(provider) => provider,
            // Cancelling changes nothing.
            None => return Ok(()),
        },
    };

    if device && !provider_supports_device(context, provider) {
        return Err(CommandError {
            message: Some(format!(
                "the device-code flow is not supported by {}; sign in with a browser instead",
                provider.display_name()
            )),
            code: 2,
        });
    }

    let runtime = ProviderRuntime::new(provider, &context.config, &context.paths);

    // The host id identifies this installation. It is created on the first
    // sign-in and reused afterwards, so the same registration is kept.
    ensure_host_id(context, provider)?;

    let outcome = login_with_fallback(&runtime, device)?;

    let path = context.paths.credential_file(provider.as_str());
    store::save(&path, &outcome.auth)
        .map_err(|err| CommandError::new(format!("could not write the credentials: {err}")))?;

    println!(
        "Logged in to {} as {}.",
        provider.display_name(),
        outcome.auth.account_label()
    );
    println!("The access token {}.", describe_expiry(&outcome.auth));
    if provider == ProviderId::OpenAi {
        if outcome.auth.plan_enabled {
            println!("ChatGPT plan usage is enabled.");
        } else {
            println!(
                "ChatGPT plan usage was not granted; the token cannot spend the ChatGPT allowance."
            );
        }
    }
    println!("Credentials stored in {}.", path.display());
    Ok(())
}

/// Run the login flow, retrying once with the fallback `referrer` and scopes.
///
/// xAI tracks the `referrer` parameter and may one day refuse an unknown
/// value; `grok-build` is the value the official CLI sends, so it is the
/// safest fallback.
fn login_with_fallback(
    runtime: &ProviderRuntime,
    device: bool,
) -> Result<LoginOutcome, CommandError> {
    let first = attempt(runtime, device);
    match first {
        Ok(outcome) => Ok(outcome),
        Err(err) if is_retryable(&err) && runtime.spec.id == ProviderId::Xai => {
            eprintln!("The login was refused; retrying with the official client settings.");
            let mut spec = runtime.spec.clone();
            spec.extra_authorize_params =
                vec![("referrer".to_string(), FALLBACK_REFERRER.to_string())];
            spec.scopes = FALLBACK_SCOPES
                .iter()
                .map(|scope| scope.to_string())
                .collect();
            let client = crate::auth::OAuthClient::for_provider(&spec, runtime.client.client_id());
            attempt_with(&spec, &client, device).map_err(|err| CommandError::new(err.to_string()))
        }
        Err(err) => Err(CommandError::new(err.to_string())),
    }
}

/// Run one attempt of the chosen flow.
fn attempt(runtime: &ProviderRuntime, device: bool) -> Result<LoginOutcome, FlowError> {
    attempt_with(&runtime.spec, &runtime.client, device)
}

/// Run one attempt with an explicit spec and client.
fn attempt_with(
    spec: &crate::provider::ProviderSpec,
    client: &crate::auth::OAuthClient,
    device: bool,
) -> Result<LoginOutcome, FlowError> {
    if device {
        flow::login_device(spec, client)
    } else {
        flow::login_loopback(spec, client, None)
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

/// Whether the provider can complete a device-code grant.
fn provider_supports_device(context: &Context, provider: ProviderId) -> bool {
    crate::provider::spec_for(provider, &context.config).supports_device_flow
}

/// Make sure the ChatGPT host id exists, and write it to the config.
fn ensure_host_id(context: &Context, provider: ProviderId) -> Result<(), CommandError> {
    if provider != ProviderId::OpenAi {
        return Ok(());
    }
    if context.config.oauth.openai.ext_agent_host_id.is_some() {
        return Ok(());
    }

    let host_id = crate::provider::openai::generate_host_id();
    crate::config::set_host_id(context.paths.config_file(), &host_id)
        .map_err(|err| CommandError::new(format!("could not record the ChatGPT host id: {err}")))?;
    crate::logging::info(&format!("recorded a new ChatGPT host id in {}", host_id));
    Ok(())
}

/// Ask the user which provider to use.
fn choose_provider(context: &Context, label: &str) -> Result<Option<ProviderId>, CommandError> {
    let choices: Vec<Choice> = ProviderId::all()
        .into_iter()
        .filter(|id| context.config.providers.get(*id).enabled)
        .map(|id| {
            let description = match id {
                ProviderId::Xai => "Grok models, on your x.ai subscription",
                ProviderId::OpenAi => "ChatGPT models, on your ChatGPT plan",
            };
            Choice::described(id.as_str(), id.display_name(), description)
        })
        .collect();

    let chosen =
        prompt::select(label, &choices).map_err(|err| CommandError::new(err.to_string()))?;

    Ok(chosen.and_then(|id| ProviderId::parse(&id)))
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

/// Revoke and remove a provider's stored credentials.
pub fn logout(context: &Context, provider: Option<ProviderId>) -> Result<(), CommandError> {
    // Only the providers we are actually signed in to are offered.
    let signed_in: Vec<ProviderId> = ProviderId::all()
        .into_iter()
        .filter(|id| is_signed_in(context, *id))
        .collect();

    if signed_in.is_empty() {
        println!("Not logged in.");
        return Ok(());
    }

    let provider = match provider {
        Some(provider) => provider,
        None => {
            let choices: Vec<Choice> = signed_in
                .iter()
                .map(|id| Choice::new(id.as_str(), id.display_name()))
                .collect();
            match prompt::select("Select a provider to sign out of:", &choices)
                .map_err(|err| CommandError::new(err.to_string()))?
            {
                Some(id) => match ProviderId::parse(&id) {
                    Some(provider) => provider,
                    None => return Ok(()),
                },
                // Cancelling changes nothing.
                None => return Ok(()),
            }
        }
    };

    let path = context.paths.credential_file(provider.as_str());
    let stored = store::load(&path)
        .map_err(|err| CommandError::new(format!("could not read the credentials: {err}")))?;

    let Some(stored) = stored else {
        println!("Not logged in to {}.", provider.display_name());
        return Ok(());
    };

    // Revoke at the provider before deleting the local copy. Revocation is
    // best-effort: a token that could not be revoked simply lapses.
    let revoked = revoke(context, provider, &stored);

    let removed = store::remove(&path)
        .map_err(|err| CommandError::new(format!("could not remove the credentials: {err}")))?;

    if removed {
        println!("Logged out of {}.", provider.display_name());
    } else {
        println!("Not logged in to {}.", provider.display_name());
    }

    if !revoked {
        println!(
            "The token could not be revoked at {}; it will lapse on its own.",
            provider.display_name()
        );
    }

    // The daemon keeps running, but it will start answering 401.
    let state = crate::daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );
    if state.is_running() {
        println!("The server is still running and will answer 401 until you log in again.");
    }

    Ok(())
}

/// Whether a provider has a stored credential.
pub fn is_signed_in(context: &Context, provider: ProviderId) -> bool {
    store::load(&context.paths.credential_file(provider.as_str()))
        .ok()
        .flatten()
        .is_some()
}

/// Try to revoke a token at the provider.
///
/// Returns whether the revocation was confirmed.
fn revoke(context: &Context, provider: ProviderId, stored: &store::StoredAuth) -> bool {
    let spec = crate::provider::spec_for(provider, &context.config);
    let Some(endpoint) = spec.revocation_endpoint() else {
        return false;
    };
    let Some(token) = stored.refresh_token.as_deref() else {
        // There is nothing to revoke but the access token, which the provider
        // will expire on its own.
        return false;
    };

    let client_id = if stored.client_id.is_empty() {
        spec.entry_client_id.clone()
    } else {
        stored.client_id.clone()
    };
    let client = crate::auth::OAuthClient::for_provider(&spec, &client_id);

    match client.revoke(&endpoint, token, &client_id) {
        Ok(()) => true,
        Err(err) => {
            crate::logging::debug(&format!("revocation at {endpoint} failed: {err}"));
            false
        }
    }
}
