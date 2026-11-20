use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::{OAuthClient, PendingAuthorization, validate_redirect_uri};
use crate::provider::ClientIdPolicy;
use crate::token::{Credentials, TokenSet};
use crate::{Error, Result};

pub(crate) fn begin_authorization(
    client: &OAuthClient,
    redirect_uri: &str,
    existing: Option<&Credentials>,
) -> Result<PendingAuthorization> {
    let redirect = validate_redirect_uri(redirect_uri, &client.provider.callback_path)?;
    if let Some(credentials) = existing
        && !credentials.issuer().is_empty()
        && credentials.issuer() != client.provider.issuer
    {
        return Err(Error::InvalidConfiguration(
            "existing credentials belong to a different issuer",
        ));
    }

    let existing_client_id = existing
        .map(Credentials::client_id)
        .filter(|client_id| !client_id.is_empty());
    let registration = client.provider.client_id_policy == ClientIdPolicy::IssuedOnRegistration
        && existing_client_id.is_none();
    let client_id = if client.provider.client_id_policy == ClientIdPolicy::IssuedOnRegistration {
        existing_client_id
            .unwrap_or(&client.provider.client_id)
            .to_string()
    } else {
        client.provider.client_id.clone()
    };
    let code_verifier = random_urlsafe(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let state = random_urlsafe(32);
    let nonce = random_urlsafe(32);

    let scope = client.provider.scopes.join(" ");
    let mut authorization_url = url::Url::parse(&client.provider.authorization_endpoint)
        .map_err(|_| Error::InvalidConfiguration("authorization endpoint URL is invalid"))?;
    {
        let mut query = authorization_url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("scope", &scope);
        query.append_pair("code_challenge", &challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", &state);
        query.append_pair("nonce", &nonce);
        for (name, value) in &client.provider.authorization_parameters {
            query.append_pair(name, value);
        }
        if registration {
            for (name, value) in &client.provider.registration_parameters {
                query.append_pair(name, value);
            }
        }
        if !registration && let Some(id_token) = existing.and_then(Credentials::id_token) {
            query.append_pair("id_token_hint", id_token);
        }
    }

    Ok(PendingAuthorization {
        authorization_url: authorization_url.into(),
        redirect_uri: redirect.to_string(),
        code_verifier,
        state,
        nonce,
        client_id,
        registration,
    })
}

pub(crate) async fn complete_authorization(
    client: &OAuthClient,
    pending: PendingAuthorization,
    callback_url: &str,
) -> Result<Credentials> {
    let callback = url::Url::parse(callback_url).map_err(|_| Error::CallbackMismatch)?;
    let redirect = url::Url::parse(&pending.redirect_uri).map_err(|_| Error::CallbackMismatch)?;
    if callback.path() != client.provider.callback_path
        || !same_origin(&callback, &redirect)
        || callback.path() != redirect.path()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.fragment().is_some()
    {
        return Err(Error::CallbackMismatch);
    }

    let state = unique_query_value(&callback, "state")?.ok_or(Error::StateMismatch)?;
    if state != pending.state {
        return Err(Error::StateMismatch);
    }
    if let Some(error) = unique_query_value(&callback, "error")? {
        return Err(Error::ProviderDenied(sanitize_oauth_code(&error)));
    }
    let code = unique_query_value(&callback, "code")?
        .ok_or(Error::InvalidResponse("authorization callback has no code"))?;
    if code.is_empty() || code.chars().any(char::is_control) {
        return Err(Error::InvalidResponse(
            "authorization callback code is invalid",
        ));
    }

    let callback_client_id = unique_query_value(&callback, "client_id")?;
    let client_id = match client.provider.client_id_policy {
        ClientIdPolicy::Fixed => pending.client_id.clone(),
        ClientIdPolicy::IssuedOnRegistration if pending.registration => callback_client_id
            .filter(|issued| !issued.trim().is_empty())
            .ok_or(Error::InvalidResponse(
                "provider callback has no issued client ID",
            ))?,
        ClientIdPolicy::IssuedOnRegistration => {
            if callback_client_id
                .as_ref()
                .is_some_and(|issued| issued != &pending.client_id)
            {
                return Err(Error::CallbackMismatch);
            }
            pending.client_id.clone()
        }
    };
    let callback_scope = unique_query_value(&callback, "scope")?;

    complete_pending_authorization(client, pending, code, client_id, callback_scope).await
}

pub(crate) async fn complete_authorization_with_code(
    client: &OAuthClient,
    pending: PendingAuthorization,
    code: &str,
) -> Result<Credentials> {
    if client.provider.client_id_policy == ClientIdPolicy::IssuedOnRegistration {
        return Err(Error::Unsupported(
            "pasted authorization codes for issued-client-ID providers",
        ));
    }
    if code.is_empty() || code.chars().any(char::is_control) {
        return Err(Error::InvalidResponse(
            "pasted authorization code is invalid",
        ));
    }
    let client_id = pending.client_id.clone();
    complete_pending_authorization(client, pending, code.to_string(), client_id, None).await
}

async fn complete_pending_authorization(
    client: &OAuthClient,
    pending: PendingAuthorization,
    code: String,
    client_id: String,
    callback_scope: Option<String>,
) -> Result<Credentials> {
    let mut parameters = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), pending.redirect_uri),
        ("client_id".to_string(), client_id.clone()),
        ("code_verifier".to_string(), pending.code_verifier),
    ];
    parameters.extend(client.provider.token_parameters.clone());
    let value = client
        .post_form_json(&client.provider.token_endpoint, &parameters)
        .await?;
    let tokens = TokenSet::from_json(&value, Credentials::now_unix())?;
    if client.provider.scopes.iter().any(|scope| scope == "openid") {
        validate_nonce(tokens.id_token(), &pending.nonce)?;
    } else if let Some(id_token) = tokens.id_token() {
        validate_nonce(Some(id_token), &pending.nonce)?;
    }
    let mut credentials = Credentials::from_tokens(
        tokens,
        client_id,
        client.provider.issuer.clone(),
        Credentials::now_unix(),
    );
    if credentials.scope.is_none() {
        credentials.scope = callback_scope;
    }
    Ok(credentials)
}

fn unique_query_value(url: &url::Url, name: &str) -> Result<Option<String>> {
    let mut values = url.query_pairs().filter(|(key, _)| key == name);
    let value = values.next().map(|(_, value)| value.into_owned());
    if values.next().is_some() {
        return Err(Error::CallbackMismatch);
    }
    Ok(value)
}

fn same_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().is_some_and(|host| {
            right
                .host_str()
                .is_some_and(|other| host.eq_ignore_ascii_case(other))
        })
        && left.port_or_known_default() == right.port_or_known_default()
}

fn validate_nonce(id_token: Option<&str>, expected_nonce: &str) -> Result<()> {
    let Some(id_token) = id_token else {
        return Err(Error::NonceMismatch);
    };
    let Some(payload) = id_token.split('.').nth(1) else {
        return Err(Error::NonceMismatch);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| Error::NonceMismatch)?;
    let claims: Value = serde_json::from_slice(&decoded).map_err(|_| Error::NonceMismatch)?;
    if claims.get("nonce").and_then(Value::as_str) == Some(expected_nonce) {
        Ok(())
    } else {
        Err(Error::NonceMismatch)
    }
}

fn sanitize_oauth_code(value: &str) -> String {
    crate::error::safe_oauth_error_code(value)
        .unwrap_or("unknown_error")
        .to_string()
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0; bytes];
    rand::rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::OAuthClient;
    use crate::provider::OAuthProviderConfig;
    use eggfetch_core::Client;

    fn client() -> OAuthClient {
        OAuthClient::new(Client::new(), OAuthProviderConfig::xai())
    }

    #[test]
    fn authorization_transaction_uses_pkce_state_nonce_and_callback_path() {
        let pending = client()
            .begin_authorization("http://127.0.0.1:34567/callback", None)
            .unwrap();
        let url = url::Url::parse(pending.authorization_url()).unwrap();
        let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            values.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            values.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:34567/callback")
        );
        assert_eq!(
            values.get("state").map(String::as_str),
            Some(pending.state.as_str())
        );
        assert_eq!(
            values.get("nonce").map(String::as_str),
            Some(pending.nonce.as_str())
        );
        assert!(values.contains_key("code_challenge"));
        assert!(pending.authorization_url().contains("referrer=eggpass"));
        let debug = format!("{pending:?}");
        assert!(!debug.contains(&pending.state));
        assert!(!debug.contains(&pending.code_verifier));
        assert!(!debug.contains(&pending.nonce));
    }

    #[test]
    fn openai_registration_hint_is_only_sent_for_first_authorization() {
        let client = OAuthClient::new(Client::new(), OAuthProviderConfig::openai());
        let first = client
            .begin_authorization("http://127.0.0.1:45678/auth/callback", None)
            .unwrap();
        let first_url = url::Url::parse(first.authorization_url()).unwrap();
        let first_values: std::collections::HashMap<_, _> =
            first_url.query_pairs().into_owned().collect();
        assert_eq!(
            first_values.get("client_id").map(String::as_str),
            Some("dynamic_agent_client")
        );
        assert_eq!(
            first_values.get("agent_name_hint").map(String::as_str),
            Some("eggpass")
        );
        assert!(first.registration);
        assert!(first_values.contains_key("resource"));

        let existing = Credentials {
            client_id: "issued-client-id".into(),
            issuer: crate::provider::OPENAI_ISSUER.into(),
            ..Credentials::default()
        };
        let next = client
            .begin_authorization("http://127.0.0.1:45678/auth/callback", Some(&existing))
            .unwrap();
        let next_url = url::Url::parse(next.authorization_url()).unwrap();
        let next_values: std::collections::HashMap<_, _> =
            next_url.query_pairs().into_owned().collect();
        assert_eq!(
            next_values.get("client_id").map(String::as_str),
            Some("issued-client-id")
        );
        assert!(!next.registration);
        assert!(!next_values.contains_key("agent_name_hint"));
    }

    #[tokio::test]
    async fn callback_completion_rejects_wrong_origin_path_state_and_duplicates() {
        let client = client();
        for case in ["origin", "path", "state", "duplicate-state", "missing-code"] {
            let pending = client
                .begin_authorization("http://127.0.0.1:34567/callback", None)
                .unwrap();
            let state = pending.state.clone();
            let (callback, expected_callback_error) = match case {
                "origin" => (
                    format!("http://127.0.0.2:34567/callback?code=c&state={state}"),
                    true,
                ),
                "path" => (
                    format!("http://127.0.0.1:34567/other?code=c&state={state}"),
                    true,
                ),
                "state" => (
                    "http://127.0.0.1:34567/callback?code=c&state=wrong".to_string(),
                    false,
                ),
                "duplicate-state" => (
                    format!("http://127.0.0.1:34567/callback?code=c&state={state}&state={state}"),
                    true,
                ),
                _ => (
                    format!("http://127.0.0.1:34567/callback?state={state}"),
                    false,
                ),
            };
            let error = client
                .complete_authorization(pending, &callback)
                .await
                .unwrap_err();
            if expected_callback_error {
                assert!(
                    matches!(error, Error::CallbackMismatch),
                    "{case}: {error:?}"
                );
            } else if case == "state" {
                assert!(matches!(error, Error::StateMismatch));
            } else {
                assert!(matches!(error, Error::InvalidResponse(_)));
            }
        }
    }

    #[test]
    fn callback_error_codes_never_reflect_arbitrary_provider_values() {
        assert_eq!(sanitize_oauth_code("access_denied"), "access_denied");
        assert_eq!(sanitize_oauth_code("refresh-secret-123"), "unknown_error");
    }

    #[test]
    fn nonce_claim_must_match_exactly() {
        let nonce = "expected-nonce";
        let payload = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&serde_json::json!({ "nonce": nonce })).unwrap());
        let token = format!("header.{payload}.signature");
        assert!(validate_nonce(Some(&token), nonce).is_ok());
        assert!(matches!(
            validate_nonce(Some(&token), "wrong"),
            Err(Error::NonceMismatch)
        ));
        assert!(matches!(
            validate_nonce(None, nonce),
            Err(Error::NonceMismatch)
        ));
    }
}
