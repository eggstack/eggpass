use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::Uri;
use rand::Rng;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::{OAuthClient, PendingAuthorization, validate_redirect_uri};
use crate::provider::{ClientIdPolicy, effective_port, uri_has_userinfo, uri_host};
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
    let authorization_url = {
        let mut query = form_urlencoded::Serializer::new(String::new());
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
        format!(
            "{}?{}",
            client.provider.authorization_endpoint,
            query.finish()
        )
    };

    Ok(PendingAuthorization {
        authorization_url,
        redirect_uri: redirect.to_string(),
        code_verifier,
        state,
        nonce,
        client_id,
        registration,
    })
}

struct ValidatedAuthorizationCallback {
    code: String,
    client_id: String,
    scope: Option<String>,
}

/// Validate a callback without consuming its pending transaction.
///
/// Interactive callers can use this before accepting user-supplied callback
/// input, leaving the transaction available when the input needs to be retried.
pub(crate) fn validate_authorization_callback(
    client: &OAuthClient,
    pending: &PendingAuthorization,
    callback_url: &str,
) -> Result<()> {
    parse_authorization_callback(client, pending, callback_url).map(|_| ())
}

pub(crate) async fn complete_authorization(
    client: &OAuthClient,
    pending: PendingAuthorization,
    callback_url: &str,
) -> Result<Credentials> {
    let callback = parse_authorization_callback(client, &pending, callback_url)?;
    complete_pending_authorization(
        client,
        pending,
        callback.code,
        callback.client_id,
        callback.scope,
    )
    .await
}

fn parse_authorization_callback(
    client: &OAuthClient,
    pending: &PendingAuthorization,
    callback_url: &str,
) -> Result<ValidatedAuthorizationCallback> {
    let callback = callback_url
        .parse::<Uri>()
        .map_err(|_| Error::CallbackMismatch)?;
    let redirect = pending
        .redirect_uri
        .parse::<Uri>()
        .map_err(|_| Error::CallbackMismatch)?;
    if callback.path() != client.provider.callback_path
        || !same_origin(&callback, &redirect)
        || callback.path() != redirect.path()
        || uri_has_userinfo(&callback)
        || callback_url.contains('#')
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
    let scope = unique_query_value(&callback, "scope")?;

    Ok(ValidatedAuthorizationCallback {
        code,
        client_id,
        scope,
    })
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

fn unique_query_value(uri: &Uri, name: &str) -> Result<Option<String>> {
    let query = uri
        .path_and_query()
        .and_then(|path| path.query())
        .unwrap_or_default();
    let mut values = form_urlencoded::parse(query.as_bytes()).filter(|(key, _)| key == name);
    let value = values.next().map(|(_, value)| value.into_owned());
    if values.next().is_some() {
        return Err(Error::CallbackMismatch);
    }
    Ok(value)
}

fn same_origin(left: &Uri, right: &Uri) -> bool {
    let (Some(left_scheme), Some(right_scheme)) = (left.scheme_str(), right.scheme_str()) else {
        return false;
    };
    left_scheme.eq_ignore_ascii_case(right_scheme)
        && uri_host(left)
            .zip(uri_host(right))
            .is_some_and(|(left, right)| left == right)
        && effective_port(left) == effective_port(right)
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

    fn query_values(url: &str) -> std::collections::HashMap<String, String> {
        let uri = url.parse::<Uri>().unwrap();
        let query = uri
            .path_and_query()
            .and_then(|path| path.query())
            .unwrap_or_default();
        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect()
    }

    #[test]
    fn authorization_transaction_uses_pkce_state_nonce_and_callback_path() {
        let pending = client()
            .begin_authorization("http://127.0.0.1:34567/callback", None)
            .unwrap();
        let values = query_values(pending.authorization_url());
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
        let first_values = query_values(first.authorization_url());
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
        let next_values = query_values(next.authorization_url());
        assert_eq!(
            next_values.get("client_id").map(String::as_str),
            Some("issued-client-id")
        );
        assert!(!next.registration);
        assert!(!next_values.contains_key("agent_name_hint"));
    }

    #[test]
    fn callback_uri_normalizes_default_port_host_case_and_form_encoding() {
        let client = client();
        let pending = client
            .begin_authorization("https://Example.test/callback", None)
            .unwrap();
        let callback = format!(
            "https://example.TEST:443/callback?code=code+%2B+value&state={}",
            pending.state
        );

        let parsed = parse_authorization_callback(&client, &pending, &callback).unwrap();
        assert_eq!(parsed.code, "code + value");
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
    fn callback_validation_can_be_retried_without_consuming_the_transaction() {
        let client = OAuthClient::new(Client::new(), OAuthProviderConfig::openai());
        let redirect = "http://127.0.0.1:45678/auth/callback";
        let pending = client.begin_authorization(redirect, None).unwrap();
        let state = pending.state.clone();

        let wrong_state = format!("{redirect}?code=code&state=wrong&client_id=issued-client");
        assert!(matches!(
            client.validate_authorization_callback(&pending, &wrong_state),
            Err(Error::StateMismatch)
        ));

        let missing_issued_id = format!("{redirect}?code=code&state={state}");
        assert!(matches!(
            client.validate_authorization_callback(&pending, &missing_issued_id),
            Err(Error::InvalidResponse(_))
        ));

        let valid =
            format!("{redirect}?code=code&state={state}&client_id=issued-client&scope=openid");
        assert!(
            client
                .validate_authorization_callback(&pending, &valid)
                .is_ok()
        );
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
