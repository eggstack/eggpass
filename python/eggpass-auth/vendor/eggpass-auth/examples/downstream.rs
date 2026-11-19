use std::error::Error;

use eggfetch_core::Client;
use eggpass_auth::{OAuthClient, OAuthProviderConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // The application owns this runtime and HTTP client. eggpass-auth creates
    // neither and simply returns futures for the application to await.
    let oauth = OAuthClient::new(Client::new(), OAuthProviderConfig::xai());
    let _pending = oauth.begin_authorization("http://127.0.0.1:43123/callback", None)?;

    // Optional opt-in demonstration of an async refresh using a caller-supplied
    // token. The token itself is never printed. This example does not launch a
    // browser or start a callback listener.
    if let Ok(refresh_token) = std::env::var("EGGPASS_AUTH_REFRESH_TOKEN") {
        let refreshed = oauth.refresh_token(&refresh_token, None).await?;
        println!("Refresh succeeded; expiry: {:?}", refreshed.expires_at());
    }
    Ok(())
}
