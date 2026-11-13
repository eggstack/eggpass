//! Integration tests: the proxy in front of a stub upstream.
//!
//! Nothing here touches the network. A stub upstream is started on loopback,
//! the proxy is pointed at it, and the tests assert what the upstream saw and
//! what the client got back.
//!
//! The stub is written directly against a socket rather than with an HTTP
//! library, so the tests do not depend on our own server being correct.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::auth::client::OAuthClient;
use crate::auth::refresh::TokenManager;
use crate::auth::store::StoredAuth;
use crate::config::Routing;
use crate::paths::Paths;
use crate::provider::{CatalogueKind, NativePaths, ProviderId, ProviderSpec, RequestPolicy};
use crate::server::{ProviderHandle, Server, ServerContext};
use crate::timeutil;
use crate::upstream::UpstreamClient;

/// A request the stub upstream received.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What the stub upstream should answer.
enum Reply {
    Body {
        status: u16,
        content_type: &'static str,
        body: String,
    },
    Stream {
        status: u16,
        content_type: &'static str,
        chunks: Vec<(Duration, String)>,
    },
}

impl Reply {
    fn json(status: u16, body: impl Into<String>) -> Reply {
        Reply::Body {
            status,
            content_type: "application/json",
            body: body.into(),
        }
    }
}

/// A stub upstream listening on loopback.
struct Stub {
    port: u16,
    recorded: Arc<Mutex<Vec<Recorded>>>,
}

impl Stub {
    fn start(handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub upstream binds");
        let port = listener.local_addr().expect("stub address").port();
        let recorded: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let handler = Arc::new(handler);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let sink = Arc::clone(&sink);
                let handler = Arc::clone(&handler);
                std::thread::spawn(move || {
                    let _ = serve_stub(stream, sink, handler);
                });
            }
        });

        Stub { port, recorded }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<Recorded> {
        self.recorded.lock().expect("stub sink").clone()
    }
}

/// Serve one request on the stub.
fn serve_stub(
    mut stream: std::net::TcpStream,
    sink: Arc<Mutex<Vec<Recorded>>>,
    handler: Arc<dyn Fn(&Recorded) -> Reply + Send + Sync + 'static>,
) -> std::io::Result<()> {
    let mut reader = std::io::BufReader::new(stream.try_clone()?);

    // Read the request head, keeping any body bytes that arrived with it.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..count]);
    };

    let text = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let leftover = buffer[head_end..].to_vec();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    // Read the body.
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        let from_buffer = length.min(leftover.len());
        body[..from_buffer].copy_from_slice(&leftover[..from_buffer]);
        if from_buffer < length {
            reader.read_exact(&mut body[from_buffer..])?;
        }
    }

    let record = Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    };
    let reply = handler(&record);
    sink.lock().expect("stub sink").push(record);

    match reply {
        Reply::Body {
            status,
            content_type,
            body,
        } => {
            let head = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nX-Ratelimit-Remaining: 42\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(body.as_bytes())?;
        }
        Reply::Stream {
            status,
            content_type,
            chunks,
        } => {
            let head = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nX-Ratelimit-Remaining: 42\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes())?;
            stream.flush()?;
            for (delay, text) in chunks {
                std::thread::sleep(delay);
                stream.write_all(format!("{:x}\r\n", text.len()).as_bytes())?;
                stream.write_all(text.as_bytes())?;
                stream.write_all(b"\r\n")?;
                stream.flush()?;
            }
            stream.write_all(b"0\r\n\r\n")?;
        }
    }

    stream.flush()?;
    Ok(())
}

/// The proxy, running in front of a stub upstream.
struct Harness {
    port: u16,
    _directory: tempfile::TempDir,
}

impl Harness {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// The x.ai provider, pointed at the stub upstream.
///
/// The spec is written out here rather than derived from the configuration, so
/// the test exercises the proxy rather than the configuration loader.
fn stub_spec(stub: &Stub) -> ProviderSpec {
    ProviderSpec {
        id: ProviderId::Xai,
        display_name: "x.ai",
        issuer: stub.base_url(),
        authorize_path: "/oauth2/authorize".to_string(),
        token_path: "/oauth2/token".to_string(),
        device_path: Some("/oauth2/device/code".to_string()),
        userinfo_path: Some("/oauth2/userinfo".to_string()),
        revocation_path: Some("/oauth2/revoke".to_string()),
        callback_path: "/callback",
        entry_client_id: "client".to_string(),
        scopes: vec!["openid".to_string(), "api:access".to_string()],
        extra_authorize_params: Vec::new(),
        registration_params: Vec::new(),
        extra_token_params: Vec::new(),
        base_url: stub.base_url(),
        extra_headers: Vec::new(),
        policy: RequestPolicy::default(),
        supports_device_flow: true,
        issues_client_id: false,
        trusted_device_hosts: &["auth.x.ai"],
        catalogue: CatalogueKind::DataArray,
        native_paths: NativePaths::Both,
    }
}

/// Start a proxy in front of a stub upstream.
///
/// The stub upstream is also the OAuth issuer, so a refresh can be exercised
/// without the network.
fn harness(stub: &Stub, api_key: &str, access_token: &str) -> Harness {
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::in_directory(directory.path());
    paths.ensure_dir().expect("config dir");

    let spec = stub_spec(stub);

    let now = timeutil::now_unix();
    let auth = StoredAuth {
        access_token: access_token.to_string(),
        refresh_token: Some("stored-refresh".to_string()),
        id_token: None,
        token_type: "Bearer".to_string(),
        scope: None,
        client_id: "client".to_string(),
        issuer: stub.base_url(),
        expires_at: Some(now + 3_600),
        obtained_at: now,
        user_id: Some("user-1".to_string()),
        email: Some("user@example.com".to_string()),
        ext_agent_host_id: None,
        plan_enabled: false,
    };
    crate::auth::store::save(&paths.credential_file("xai"), &auth)
        .expect("credentials are written");

    let upstream = UpstreamClient::new(&spec);
    let client = OAuthClient::for_provider(&spec, "client");
    let tokens = TokenManager::new(paths.credential_file("xai"), client);

    let mut providers = HashMap::new();
    providers.insert(
        ProviderId::Xai,
        ProviderHandle {
            spec: spec.clone(),
            upstream,
            tokens,
        },
    );

    let context = Arc::new(ServerContext {
        api_key: api_key.to_string(),
        providers,
        default_provider: ProviderId::Xai,
        routing: Routing::default(),
        upstream_in_flight: Arc::new(AtomicUsize::new(0)),
        max_upstream_requests: crate::server::DEFAULT_MAX_UPSTREAM_REQUESTS,
    });

    let server = Server::bind_with_limits("127.0.0.1", 0, crate::config::ServerLimits::default())
        .expect("proxy binds");
    let port = server.address().port();
    let running = Arc::clone(&server);
    std::thread::spawn(move || running.serve(context));

    // Give the accept loop a moment to start.
    std::thread::sleep(Duration::from_millis(50));

    Harness {
        port,
        _directory: directory,
    }
}

/// An HTTP agent that returns error responses instead of failing.
fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build();
    ureq::Agent::new_with_config(config)
}

/// Read a response body as JSON.
fn read_json_body(response: ureq::http::Response<ureq::Body>) -> serde_json::Value {
    let text = response.into_body().read_to_string().expect("body text");
    serde_json::from_str(&text).expect("json body")
}

fn chat_body() -> serde_json::Value {
    json!({"model": "grok-4.7", "messages": [{"role": "user", "content": "hello"}]})
}

#[test]
fn a_chat_completion_round_trips() {
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/chat/completions") {
            Reply::json(
                200,
                r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi there"},"finish_reason":"stop"}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["choices"][0]["message"]["content"], "hi there");
    assert_eq!(body["id"], "chatcmpl-1");
}

#[test]
fn the_clients_credentials_never_reach_the_upstream() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer client-secret")
        .header("X-Api-Key", "client-secret")
        .header("X-Forwarded-For", "10.0.0.1")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let upstream = &requests[0];
    assert_eq!(upstream.method, "POST");
    assert!(upstream.body.contains("grok-4.7"), "{}", upstream.body);

    // Our token replaced the client's, and the client's key is gone.
    assert_eq!(upstream.header("Authorization"), Some("Bearer grok-token"));
    assert_eq!(upstream.header("X-Api-Key"), None);
    assert_eq!(upstream.header("X-Forwarded-For"), None);
}

#[test]
fn a_streaming_response_arrives_incrementally() {
    let stub = Stub::start(|_| Reply::Stream {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![
            (Duration::from_millis(0), "data: {\"i\":1}\n\n".to_string()),
            (
                Duration::from_millis(500),
                "data: {\"i\":2}\n\n".to_string(),
            ),
            (Duration::from_millis(500), "data: [DONE]\n\n".to_string()),
        ],
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(json!({"model": "grok-4.7", "messages": [], "stream": true}).to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "the content type was {content_type:?}"
    );

    let mut reader = response.into_body().into_reader();
    let mut buffer = [0u8; 1024];
    let start = Instant::now();
    let mut first_chunk_at: Option<Duration> = None;
    let mut received = Vec::new();

    loop {
        let count = reader.read(&mut buffer).expect("the stream reads");
        if count == 0 {
            break;
        }
        if first_chunk_at.is_none() {
            first_chunk_at = Some(start.elapsed());
        }
        received.extend_from_slice(&buffer[..count]);
    }
    let total = start.elapsed();
    let first = first_chunk_at.expect("at least one chunk");

    // The first chunk must not have waited for the whole stream.
    assert!(
        first < Duration::from_millis(400),
        "the first chunk took {first:?}, which suggests the response was buffered"
    );
    // And the whole stream must have taken at least as long as its pauses.
    assert!(
        total >= Duration::from_millis(900),
        "the stream took only {total:?}, which suggests it was not streamed"
    );

    let text = String::from_utf8_lossy(&received);
    assert!(text.contains("data: {\"i\":1}"), "{text}");
    assert!(text.contains("data: [DONE]"), "{text}");
}

#[test]
fn an_upstream_401_triggers_one_refresh_and_one_retry() {
    let stub = Stub::start(|request| {
        if request.path == "/oauth2/token" {
            Reply::json(
                200,
                r#"{"access_token":"fresh-token","refresh_token":"fresh-refresh","token_type":"Bearer","expires_in":3600}"#,
            )
        } else if request.path.starts_with("/v1/chat/completions") {
            if request.header("Authorization") == Some("Bearer fresh-token") {
                Reply::json(200, r#"{"id":"chatcmpl-2","choices":[]}"#)
            } else {
                Reply::json(401, r#"{"error":{"message":"expired"}}"#)
            }
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "", "stale-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["id"], "chatcmpl-2");

    let requests = stub.requests();
    let chat_requests = requests
        .iter()
        .filter(|request| request.path.starts_with("/v1/chat/completions"))
        .count();
    let token_requests = requests
        .iter()
        .filter(|request| request.path == "/oauth2/token")
        .count();

    assert_eq!(chat_requests, 2, "the request is retried exactly once");
    assert_eq!(token_requests, 1, "the token is refreshed exactly once");
}

#[test]
fn the_configured_key_is_required() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "secret-key", "grok-token");

    // Without a key.
    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 401);
    let body = read_json_body(response);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(body["error"]["message"].is_string());

    // With the key as a bearer token.
    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer secret-key")
        .send(chat_body().to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);

    // And the upstream never saw the client's key.
    let requests = stub.requests();
    assert_eq!(requests[0].header("X-Api-Key"), None);
    assert_eq!(
        requests[0].header("Authorization"),
        Some("Bearer grok-token")
    );
}

#[test]
fn the_health_endpoint_needs_no_key() {
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/health"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["status"], "ok");
}

#[test]
fn an_unknown_endpoint_answers_404_in_the_openai_shape() {
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .post(harness.url("/v1/embeddings"))
        .header("Content-Type", "application/json")
        .send(json!({}).to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 404);
    let body = read_json_body(response);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(body["error"]["code"].is_null());
}

#[test]
fn the_key_query_parameter_is_not_forwarded() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions?key=client-secret"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    let requests = stub.requests();
    assert!(
        !requests[0].path.contains("client-secret"),
        "the key leaked upstream: {}",
        requests[0].path
    );
}

#[test]
fn the_model_list_comes_from_the_upstream() {
    let stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"object":"list","data":[{"id":"grok-4.7","object":"model"}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .get(harness.url("/v1/models"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|model| model["id"].as_str().expect("model id"))
        .collect();

    // The catalogue is exactly what the provider advertises. There is no
    // compiled-in list of model names to pad it with.
    assert_eq!(ids, vec!["grok-4.7"]);
}

#[test]
fn a_model_the_upstream_does_not_advertise_is_a_404() {
    let stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"object":"list","data":[{"id":"grok-4.7","object":"model"}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .get(harness.url("/v1/models/grok-9.9"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 404);
    let body = read_json_body(response);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("grok-9.9")
    );
}

#[test]
fn a_path_without_the_v1_prefix_also_works() {
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/chat/completions") {
            Reply::json(200, r#"{"id":"chatcmpl-3"}"#)
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .post(harness.url("/chat/completions"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);

    let requests = stub.requests();
    assert!(
        requests[0].path.starts_with("/v1/chat/completions"),
        "the upstream saw {}",
        requests[0].path
    );
}

#[test]
fn keep_alive_connections_serve_more_than_one_request() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");

    let agent = agent();
    for _ in 0..3 {
        let response = agent
            .get(harness.url("/v1/models"))
            .call()
            .expect("the proxy answers");
        assert_eq!(response.status().as_u16(), 200);
    }

    assert_eq!(stub.requests().len(), 3);
}

#[test]
fn the_upstream_headers_reach_a_streaming_client() {
    // A streaming client used to see nothing but the event stream: the
    // upstream's rate-limit headers were thrown away, so the budget was
    // invisible to exactly the clients that were streaming.
    let stub = Stub::start(|_| Reply::Stream {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![(Duration::from_millis(0), "data: [DONE]\n\n".to_string())],
    });
    let harness = harness(&stub, "", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(json!({"model": "grok-4.7", "messages": [], "stream": true}).to_string())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok()),
        Some("42"),
        "the upstream's rate-limit header did not reach the client"
    );
}

#[test]
fn a_client_cannot_inject_the_provider_owned_headers() {
    // The Grok Build headers describe the client the proxy speaks for. A
    // client that set them must not be able to make the proxy claim to be
    // some other build.
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .header("X-XAI-Token-Auth", "spoofed")
        .header("x-grok-client-version", "9.9.9")
        .header("x-grok-client-identifier", "not-eggpass")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    let requests = stub.requests();
    let upstream = requests.first().expect("the upstream saw the request");
    assert_eq!(upstream.header("X-XAI-Token-Auth"), None);
    assert_eq!(upstream.header("x-grok-client-version"), None);
    assert_eq!(upstream.header("x-grok-client-identifier"), None);
}
