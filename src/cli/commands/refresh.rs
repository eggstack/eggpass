//! eggpass refresh.
//!
//! This command deliberately bypasses expiry checks: it is the manual recovery
//! path when a user wants to refresh stored credentials immediately.

use crate::auth::refresh::TokenError;
use crate::cli::prompt::{self, Choice};
use crate::cli::{CommandError, Context};
use crate::provider::{ProviderId, ProviderRuntime};

/// Force-refresh one signed-in provider's stored credentials.
pub fn refresh(context: &Context, provider: Option<ProviderId>) -> Result<(), CommandError> {
    let signed_in: Vec<ProviderId> = ProviderId::all()
        .into_iter()
        .filter(|id| super::login::is_signed_in(context, *id))
        .collect();

    if provider.is_none() && signed_in.is_empty() {
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
            match prompt::select("Select a provider to refresh:", &choices)
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

    let runtime = ProviderRuntime::new(provider, &context.config, &context.paths);
    runtime.tokens.refresh_now().map_err(|error| {
        let provider_name = provider.display_name();
        let login_command = format!("eggpass login --provider {}", provider.as_str());
        let message = match error {
            // Keep CLI failures free of response bodies and credential values.
            TokenError::NotLoggedIn => {
                format!("not logged in to {provider_name}; run {login_command}")
            }
            TokenError::NoRefreshToken => format!(
                "the stored {provider_name} credentials have no refresh token; run {login_command}"
            ),
            TokenError::Store(_) => {
                format!("could not read or write the stored {provider_name} credentials")
            }
            TokenError::Lock(_) => {
                format!("could not acquire the {provider_name} credential refresh lock")
            }
            TokenError::Auth(_) => {
                format!(
                    "could not refresh the stored {provider_name} credentials; run {login_command}"
                )
            }
        };
        CommandError::new(message)
    })?;

    println!("Credentials refreshed for {}.", provider.display_name());
    Ok(())
}
