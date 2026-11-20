use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use eggfetch_core::Client;
use eggfetch_core::timeout::Timeout;
use eggpass_auth::{
    ClientIdPolicy, ClientOptions, CredentialStore, Credentials, Error, OAuthClient,
    OAuthProviderConfig, StorageError, TokenManager,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

async fn stub_once(status: u16, body: &'static [u8]) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        write_response(&mut stream, status, body).await;
        request
    });
    (format!("http://{address}"), task)
}

async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end;
    loop {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before request headers");
        request.extend_from_slice(&chunk[..read]);
        if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = index + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed before request body");
        request.extend_from_slice(&chunk[..read]);
    }
    request
}

async fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        503 => "Service Unavailable",
        _ => "Test Response",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream.flush().await.unwrap();
}

fn oauth(base: &str, client: Client) -> OAuthClient {
    let config = OAuthProviderConfig::custom("stub", base, "public-client")
        .authorization_endpoint(format!("{base}/authorize"))
        .token_endpoint(format!("{base}/token"))
        .device_authorization_endpoint(format!("{base}/device"))
        .userinfo_endpoint(format!("{base}/userinfo"))
        .revocation_endpoint(format!("{base}/revoke"))
        .callback_path("/callback")
        .scopes(["offline_access"])
        .trusted_device_host("accounts.test")
        .build()
        .unwrap();
    OAuthClient::new(client, config)
}

fn options(total: Duration, body_limit: usize) -> ClientOptions {
    ClientOptions {
        timeout: Timeout {
            pool: Some(Duration::from_secs(1)),
            connect: Some(Duration::from_secs(1)),
            write: Some(Duration::from_secs(1)),
            read: Some(Duration::from_secs(1)),
            total: Some(total),
        },
        max_response_body_bytes: body_limit,
    }
}

#[tokio::test]
async fn real_eggfetch_posts_encoded_authorization_code_without_leaking_bearer_auth() {
    let (base, server) = stub_once(
        200,
        br#"{"access_token":"access-secret","refresh_token":"refresh-secret","token_type":"Bearer","expires_in":3600}"#,
    )
    .await;
    let oauth = oauth(&base, Client::new())
        .with_options(options(Duration::from_secs(2), 4096))
        .unwrap();
    let credentials = oauth
        .exchange_authorization_code(
            "code /+&",
            "http://127.0.0.1:43123/callback",
            "verifier-123456789",
            None,
        )
        .await
        .unwrap();
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /token HTTP/1.1"), "{request}");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded")
    );
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let parameters: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(parameters.get("code").map(String::as_str), Some("code /+&"));
    assert_eq!(
        parameters.get("code_verifier").map(String::as_str),
        Some("verifier-123456789")
    );
    assert_eq!(
        parameters.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert_eq!(credentials.access_token(), "access-secret");
    assert_eq!(credentials.refresh_token(), Some("refresh-secret"));
    assert!(!format!("{credentials:?}").contains("access-secret"));
}

#[tokio::test]
async fn revocation_and_userinfo_use_their_configured_endpoints() {
    let (base, server) = stub_once(200, b"{}").await;
    let revoker = oauth(&base, Client::new());
    revoker.revoke("refresh-secret", None).await.unwrap();
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /revoke HTTP/1.1"));
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        form.get("token").map(String::as_str),
        Some("refresh-secret")
    );
    assert_eq!(
        form.get("token_type_hint").map(String::as_str),
        Some("refresh_token")
    );

    let (base, server) = stub_once(200, br#"{"sub":"account-7","email":"a@example.test"}"#).await;
    let oauth = oauth(&base, Client::new());
    let info = oauth.userinfo("access-secret").await.unwrap();
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("GET /userinfo HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer access-secret")
    );
    assert_eq!(info.user_id.as_deref(), Some("account-7"));
    assert_eq!(info.email.as_deref(), Some("a@example.test"));
}

#[tokio::test]
async fn device_code_request_and_poll_use_real_eggfetch_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let first_request = read_request(&mut first).await;
        write_response(
            &mut first,
            200,
            br#"{"device_code":"device-secret","user_code":"human-code","verification_uri":"https://accounts.test/activate","verification_uri_complete":"https://accounts.test/activate?user_code=human-code","expires_in":15,"interval":1}"#,
        )
        .await;
        drop(first);

        let (mut second, _) = listener.accept().await.unwrap();
        let second_request = read_request(&mut second).await;
        write_response(
            &mut second,
            200,
            br#"{"access_token":"device-access","expires_in":3600}"#,
        )
        .await;
        (first_request, second_request)
    });

    let base = format!("http://{address}");
    let oauth = oauth(&base, Client::new());
    let device = oauth.request_device_code().await.unwrap();
    assert_eq!(device.user_code(), "human-code");
    assert_eq!(device.verification_uri(), "https://accounts.test/activate");
    let debug = format!("{device:?}");
    assert!(!debug.contains("device-secret"));
    assert!(!debug.contains("human-code"));
    let credentials = oauth
        .poll_device_authorization(
            &device,
            &tokio_util::sync::CancellationToken::new(),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(credentials.access_token(), "device-access");

    let (first, second) = server.await.unwrap();
    let first = String::from_utf8(first).unwrap();
    assert!(first.starts_with("POST /device HTTP/1.1"));
    let first_body = first.split("\r\n\r\n").nth(1).unwrap();
    let first_form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(first_body.as_bytes())
            .into_owned()
            .collect();
    assert_eq!(
        first_form.get("client_id").map(String::as_str),
        Some("public-client")
    );

    let second = String::from_utf8(second).unwrap();
    assert!(second.starts_with("POST /token HTTP/1.1"));
    let second_body = second.split("\r\n\r\n").nth(1).unwrap();
    let second_form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(second_body.as_bytes())
            .into_owned()
            .collect();
    assert_eq!(
        second_form.get("device_code").map(String::as_str),
        Some("device-secret")
    );
    assert_eq!(
        second_form.get("grant_type").map(String::as_str),
        Some("urn:ietf:params:oauth:grant-type:device_code")
    );
}

#[tokio::test]
async fn issued_client_id_is_exchanged_and_reused_after_registration() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        write_response(
            &mut stream,
            200,
            br#"{"access_token":"registered-access","expires_in":3600}"#,
        )
        .await;
        request
    });

    let base = format!("http://{address}");
    let profile = OAuthProviderConfig::custom("registered", &base, "entry-client")
        .authorization_endpoint(format!("{base}/authorize"))
        .token_endpoint(format!("{base}/token"))
        .callback_path("/callback")
        .client_id_policy(ClientIdPolicy::IssuedOnRegistration)
        .registration_parameter("agent_name_hint", "consumer")
        .build()
        .unwrap();
    let oauth = OAuthClient::new(Client::new(), profile);
    let redirect = format!("{base}/callback");
    let pending = oauth.begin_authorization(&redirect, None).unwrap();
    let authorization_url = url::Url::parse(pending.authorization_url()).unwrap();
    let state = authorization_url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    let callback =
        format!("{redirect}?code=registration-code&state={state}&client_id=issued-client");
    oauth
        .validate_authorization_callback(&pending, &callback)
        .unwrap();
    let credentials = oauth
        .complete_authorization(pending, &callback)
        .await
        .unwrap();
    assert_eq!(credentials.client_id(), "issued-client");

    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /token HTTP/1.1"));
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        form.get("client_id").map(String::as_str),
        Some("issued-client")
    );

    let next = oauth
        .begin_authorization(&redirect, Some(&credentials))
        .unwrap();
    let next_url = url::Url::parse(next.authorization_url()).unwrap();
    let next_values: std::collections::HashMap<_, _> =
        next_url.query_pairs().into_owned().collect();
    assert_eq!(
        next_values.get("client_id").map(String::as_str),
        Some("issued-client")
    );
    assert!(!next_values.contains_key("agent_name_hint"));
}

#[tokio::test]
async fn non_json_error_does_not_escape_response_body() {
    let (base, server) = stub_once(503, b"proxy diagnostic contains hidden-secret").await;
    let oauth = oauth(&base, Client::new());
    let error = oauth
        .refresh_token("refresh-secret", None)
        .await
        .unwrap_err();
    let request = server.await.unwrap();
    assert!(!request.is_empty());
    assert!(matches!(error, Error::HttpStatus { status: 503 }));
    assert!(!error.to_string().contains("hidden-secret"));
    assert!(!format!("{error:?}").contains("hidden-secret"));
}

#[tokio::test]
async fn oauth_error_code_is_returned_without_description_or_body() {
    let (base, server) = stub_once(
        400,
        br#"{"error":"invalid_client","error_description":"hidden-secret details"}"#,
    )
    .await;
    let oauth = oauth(&base, Client::new());
    let error = oauth
        .refresh_token("refresh-secret", None)
        .await
        .unwrap_err();
    let _ = server.await.unwrap();
    assert!(matches!(error, Error::OAuth { status: 400, .. }));
    assert_eq!(error.oauth_code(), Some("invalid_client"));
    assert!(!error.to_string().contains("hidden-secret"));
    assert!(!format!("{error:?}").contains("hidden-secret"));
}

#[tokio::test]
async fn decoded_body_limit_is_enforced_with_the_real_eggfetch_client() {
    let (base, server) = stub_once(
        200,
        b"{\"access_token\":\"this-response-is-over-the-limit\"}",
    )
    .await;
    let oauth = oauth(&base, Client::new())
        .with_options(options(Duration::from_secs(2), 16))
        .unwrap();
    let error = oauth
        .refresh_token("refresh-secret", None)
        .await
        .unwrap_err();
    let _ = server.await.unwrap();
    assert!(matches!(error, Error::ResponseTooLarge));
}

#[tokio::test]
async fn request_deadline_is_normalized_without_transport_details() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let _ = write_response(&mut stream, 200, br#"{"access_token":"late"}"#).await;
    });
    let oauth = oauth(&format!("http://{address}"), Client::new())
        .with_options(options(Duration::from_millis(50), 4096))
        .unwrap();
    let error = oauth
        .refresh_token("refresh-secret", None)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout("HTTP request")));
    server.await.unwrap();
}

#[tokio::test]
async fn redirects_are_not_followed_even_for_a_caller_owned_client() {
    let (base, server) = stub_once(302, br#"{"next":"/redirected"}"#).await;
    let oauth = oauth(&base, Client::new());
    let error = oauth
        .refresh_token("refresh-secret", None)
        .await
        .unwrap_err();
    let request = String::from_utf8(server.await.unwrap()).unwrap();
    assert!(request.starts_with("POST /token HTTP/1.1"));
    assert!(matches!(error, Error::HttpStatus { status: 302 }));
}

#[derive(Clone, Default)]
struct MemoryStore(Arc<tokio::sync::Mutex<Option<Credentials>>>);

impl CredentialStore for MemoryStore {
    fn load(&self) -> impl Future<Output = Result<Option<Credentials>, StorageError>> + Send {
        let inner = self.0.clone();
        async move { Ok(inner.lock().await.clone()) }
    }

    fn save(
        &self,
        credentials: Credentials,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        let inner = self.0.clone();
        async move {
            *inner.lock().await = Some(credentials);
            Ok(())
        }
    }

    fn clear(&self) -> impl Future<Output = Result<(), StorageError>> + Send {
        let inner = self.0.clone();
        async move {
            *inner.lock().await = None;
            Ok(())
        }
    }
}

#[tokio::test]
async fn manager_coalesces_concurrent_refresh_and_retains_rotated_refresh_token() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let first = read_request(&mut stream).await;
        write_response(
            &mut stream,
            200,
            br#"{"access_token":"fresh-access","expires_in":3600}"#,
        )
        .await;
        let second = tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
        (first, second.is_ok())
    });

    let base = format!("http://{address}");
    let mut profile = OAuthProviderConfig::custom("stub", &base, "public-client")
        .authorization_endpoint(format!("{base}/authorize"))
        .token_endpoint(format!("{base}/token"))
        .callback_path("/callback")
        .scopes(["offline_access"])
        .build()
        .unwrap();
    profile = profile.with_client_id("public-client").unwrap();
    let client = OAuthClient::new(Client::new(), profile);
    let store = MemoryStore::default();
    let stale: Credentials = serde_json::from_str(
        r#"{
            "access_token":"stale-access",
            "refresh_token":"preserved-refresh",
            "id_token":null,
            "token_type":"Bearer",
            "scope":"offline_access",
            "client_id":"public-client",
            "issuer":"PLACEHOLDER_ISSUER",
            "expires_at":0,
            "obtained_at":0
        }"#
        .replace("PLACEHOLDER_ISSUER", &base)
        .as_str(),
    )
    .unwrap();
    store.save(stale).await.unwrap();
    let manager = Arc::new(TokenManager::new(client, store.clone()));

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let manager = manager.clone();
        tasks.spawn(async move { manager.access_token().await });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap(), "fresh-access");
    }

    let (request, saw_second_refresh) = server.await.unwrap();
    assert!(request.starts_with(b"POST /token HTTP/1.1"));
    assert!(
        !saw_second_refresh,
        "manager issued more than one refresh request"
    );
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("request contains the end of headers");
    let body = &request[header_end + 4..];
    let form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(body).into_owned().collect();
    assert_eq!(
        form.get("refresh_token").map(String::as_str),
        Some("preserved-refresh")
    );
    let stored = store.load().await.unwrap().unwrap();
    assert_eq!(stored.access_token(), "fresh-access");
    assert_eq!(stored.refresh_token(), Some("preserved-refresh"));
}
