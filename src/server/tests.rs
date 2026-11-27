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
use crate::provider::{
    CatalogueKind, MediaPath, NativePaths, ProviderId, ProviderSpec, RequestPolicy,
};
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
            // An empty content type means "send no Content-Type header",
            // which is what `api.openai.com` does with a streamed response.
            let content_type_line = if content_type.is_empty() {
                String::new()
            } else {
                format!("Content-Type: {content_type}\r\n")
            };
            let head = format!(
                "HTTP/1.1 {status} OK\r\n{content_type_line}X-Ratelimit-Remaining: 42\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
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
    /// The keys the proxy accepts, so a test can issue one.
    keys: Arc<crate::keys::KeyStore>,
    /// The status of the last inference the proxy served.
    inferences: Arc<crate::inference::InferenceState>,
    /// The provider the harness serves.
    provider: ProviderId,
    usage_calls: Arc<AtomicUsize>,
    _directory: tempfile::TempDir,
}

impl Harness {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The status of the last inference the proxy served.
    fn last_inference(&self) -> Option<u16> {
        self.inferences.snapshot().get(&self.provider).copied()
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
        media_paths: &[],
    }
}

/// A ChatGPT-shaped provider: Responses only, and a stream is mandatory.
///
/// This is the shape that matters for the `Content-Type` bug: OpenAI answers a
/// streamed request with **no `Content-Type` header at all**, so the proxy
/// cannot rely on the header to know it is looking at an event stream.
fn chatgpt_stub_spec(stub: &Stub) -> ProviderSpec {
    ProviderSpec {
        id: ProviderId::OpenAi,
        display_name: "ChatGPT",
        issuer: stub.base_url(),
        authorize_path: "/oauth2/authorize".to_string(),
        token_path: "/oauth2/token".to_string(),
        device_path: None,
        userinfo_path: None,
        revocation_path: None,
        callback_path: "/auth/callback",
        entry_client_id: "client".to_string(),
        scopes: vec!["openid".to_string()],
        extra_authorize_params: Vec::new(),
        registration_params: Vec::new(),
        extra_token_params: Vec::new(),
        base_url: stub.base_url(),
        extra_headers: Vec::new(),
        policy: RequestPolicy {
            strip_fields: &["temperature", "top_p"],
            force_stream: true,
            force_store_false: true,
            require_input_array: true,
        },
        supports_device_flow: false,
        issues_client_id: true,
        trusted_device_hosts: &["auth.openai.com"],
        catalogue: CatalogueKind::SlugArray,
        native_paths: NativePaths::ResponsesOnly,
        media_paths: &[],
    }
}

/// Start a proxy in front of a stub upstream.
///
/// The stub upstream is also the OAuth issuer, so a refresh can be exercised
/// without the network.
fn harness(stub: &Stub, api_key: &str, access_token: &str) -> Harness {
    harness_with_spec(stub, api_key, access_token, stub_spec(stub))
}

/// Start a proxy in front of a stub upstream, with an explicit provider spec.
///
/// The spec is written out here rather than derived from the configuration, so
/// the test exercises the proxy rather than the configuration loader.
fn harness_with_spec(
    stub: &Stub,
    api_key: &str,
    access_token: &str,
    spec: ProviderSpec,
) -> Harness {
    harness_with_specs(api_key, vec![(stub, spec, Some(access_token))])
}

/// Start a proxy with several provider fixtures. A missing access token leaves
/// that configured provider unsigned, so catalogue tests can prove it is skipped.
fn harness_with_specs(
    api_key: &str,
    fixtures: Vec<(&Stub, ProviderSpec, Option<&str>)>,
) -> Harness {
    assert!(
        !fixtures.is_empty(),
        "at least one provider fixture is required"
    );
    let directory = tempfile::tempdir().expect("temp dir");
    let paths = Paths::in_directory(directory.path());
    paths.ensure_dir().expect("config dir");

    let mut providers = HashMap::new();
    let mut provider_ids = Vec::new();
    let mut usage_sources: HashMap<ProviderId, (Arc<TokenManager>, String)> = HashMap::new();
    for (stub, spec, access_token) in fixtures {
        if let Some(access_token) = access_token {
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
            crate::auth::store::save(&paths.credential_file(spec.id.as_str()), &auth)
                .expect("credentials are written");
        }

        let upstream = UpstreamClient::new(&spec);
        let client = OAuthClient::for_provider(&spec, "client");
        let tokens = Arc::new(TokenManager::new(
            paths.credential_file(spec.id.as_str()),
            client,
        ));
        provider_ids.push(spec.id);
        usage_sources.insert(spec.id, (Arc::clone(&tokens), stub.base_url()));
        providers.insert(
            spec.id,
            ProviderHandle {
                spec,
                upstream,
                tokens,
            },
        );
    }

    let keys = Arc::new(crate::keys::KeyStore::open(paths.key_store_file()).expect("key store"));
    let server = Server::bind_with_limits("127.0.0.1", 0, crate::config::ServerLimits::default())
        .expect("proxy binds");
    let port = server.address().port();
    let inferences = Arc::new(crate::inference::InferenceState::new());
    let usage_calls = Arc::new(AtomicUsize::new(0));
    let usage_calls_for_collector = Arc::clone(&usage_calls);
    let usage_timeouts = crate::config::Timeouts {
        connect_seconds: 1,
        global_seconds: 1,
        body_seconds: 1,
    };
    let usage_cache = Arc::new(crate::server::usage::UsageCache::new(
        provider_ids.clone(),
        move |provider| {
            usage_calls_for_collector.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (usage_tokens, usage_base_url) = usage_sources
                .get(&provider)
                .expect("usage provider is registered");
            crate::provider::usage::report::collect_entry(
                provider,
                crate::config::Upstream::Api,
                usage_tokens.as_ref(),
                usage_timeouts,
                None,
                usage_base_url,
            )
        },
    ));

    let default_provider = *provider_ids.first().expect("provider fixture");
    let context = Arc::new(ServerContext {
        host: "127.0.0.1".to_string(),
        port,
        api_key: api_key.to_string(),
        usage_key: "server-test-usage-key".to_string(),
        eggpool_enrollment: None,
        keys: Arc::clone(&keys),
        providers,
        default_provider,
        routing: Routing::default(),
        upstream_in_flight: Arc::new(AtomicUsize::new(0)),
        max_upstream_requests: crate::server::DEFAULT_MAX_UPSTREAM_REQUESTS,
        inferences: Arc::clone(&inferences),
        usage_cache,
    });

    let running = Arc::clone(&server);
    std::thread::spawn(move || running.serve(context));
    std::thread::sleep(Duration::from_millis(50));

    Harness {
        port,
        keys,
        inferences,
        provider: default_provider,
        usage_calls,
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["choices"][0]["message"]["content"], "hi there");
    assert_eq!(body["id"], "chatcmpl-1");
}

#[test]
fn a_successful_inference_is_recorded() {
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
    let harness = harness(&stub, "secret-key", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    assert_eq!(harness.last_inference(), Some(200));
}

#[test]
fn a_refused_inference_is_recorded_with_its_status() {
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/chat/completions") {
            Reply::json(
                429,
                r#"{"error":{"type":"rate_limit_error","code":"subscription_sharing_usage_limit_exceeded","message":"the shared allowance is exhausted"}}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "secret-key", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    assert_eq!(harness.last_inference(), Some(429));
}

#[test]
fn a_streamed_inference_is_recorded_when_the_stream_ends() {
    let stub = Stub::start(|_| Reply::Stream {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![(Duration::from_millis(0), "data: [DONE]\n\n".to_string())],
    });
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(json!({"model": "grok-4.7", "messages": [], "stream": true}).to_string())
        .expect("the proxy answers");

    let mut reader = response.into_body().into_reader();
    let mut buffer = [0u8; 1024];
    while reader.read(&mut buffer).expect("the stream reads") > 0 {}

    assert_eq!(harness.last_inference(), Some(200));
}

#[test]
fn the_last_inference_is_served() {
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
    let harness = harness(&stub, "secret-key", "grok-token");

    // Before any inference there is nothing to say.
    let response = agent()
        .get(harness.url("/v1/last-inference"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["providers"], json!({}));

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");

    let response = agent()
        .get(harness.url("/v1/last-inference"))
        .call()
        .expect("the proxy answers");
    let body = read_json_body(response);
    assert_eq!(body["providers"]["xai"], json!(200));
}

#[test]
fn the_last_inference_needs_no_key() {
    // The local command that reads it holds no key material, so the route is
    // open — like `/health`, and no more.
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/v1/last-inference"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
}

#[test]
fn the_clients_credentials_never_reach_the_upstream() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "secret-key", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "stale-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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
fn an_issued_key_is_required_once_one_exists() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");
    let (_, plain) = harness.keys.add(None).expect("a key is issued");

    // Without a key.
    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 401);

    // With the key we issued.
    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .header("Authorization", &format!("Bearer {plain}"))
        .send(chat_body().to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);

    // The upstream never saw the client's key.
    let requests = stub.requests();
    assert_eq!(requests[0].header("X-Api-Key"), None);
    assert_eq!(
        requests[0].header("Authorization"),
        Some("Bearer grok-token")
    );
}

#[test]
fn a_revoked_key_is_refused_without_a_restart() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"ok":true}"#));
    let harness = harness(&stub, "", "grok-token");
    let (_, revoked) = harness.keys.add(None).expect("a key is issued");
    let (_, kept) = harness.keys.add(None).expect("another key is issued");

    let request = |key: &str| {
        agent()
            .post(harness.url("/v1/chat/completions"))
            .header("Content-Type", "application/json")
            .header("Authorization", &format!("Bearer {key}"))
            .send(chat_body().to_string())
            .expect("the proxy answers")
            .status()
            .as_u16()
    };

    assert_eq!(request(&revoked), 200);
    assert_eq!(request(&kept), 200);

    harness.keys.revoke(1).expect("the key is revoked");

    assert_eq!(request(&revoked), 401, "the revoked key is still accepted");
    assert_eq!(request(&kept), 200, "the other key stopped working");
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
fn a_known_endpoint_with_the_wrong_method_answers_405() {
    // `404` would say the endpoint does not exist, and send the user looking
    // for a typo. It exists; it takes a different method.
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    for path in ["/v1/models", "/health", "/v1/last-inference"] {
        let response = agent()
            .post(harness.url(path))
            .header("Authorization", "Bearer secret-key")
            .header("Content-Type", "application/json")
            .send(json!({}).to_string())
            .expect("the proxy answers");

        assert_eq!(response.status().as_u16(), 405, "POST {path}");
        // RFC 9110 requires the header on a 405, so a client can correct
        // itself without being told twice.
        assert_eq!(
            response
                .headers()
                .get("allow")
                .and_then(|value| value.to_str().ok()),
            Some("GET"),
            "POST {path} is missing `Allow: GET`"
        );

        let body = read_json_body(response);
        assert_eq!(body["error"]["type"], "invalid_request_error", "{path}");
        assert_eq!(body["error"]["code"], "method_not_allowed", "{path}");
    }
}

#[test]
fn a_get_on_an_inference_endpoint_answers_405() {
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 405);
    assert_eq!(
        response
            .headers()
            .get("allow")
            .and_then(|value| value.to_str().ok()),
        Some("POST")
    );
}

#[test]
fn an_unknown_endpoint_answers_404_in_the_openai_shape() {
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/embeddings"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions?key=secret-key"))
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/v1/models"))
        .header("Authorization", "Bearer secret-key")
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
fn reasoning_levels_are_visible_on_merged_and_single_model_routes() {
    let xai_stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"data":[
                    {"id":"grok-4.7","object":"model","owned_by":"xai","context_length":500000,"capabilities":{"reasoning_effort":["low","future-level","high"],"default_reasoning_effort":"future-level","tools":true}},
                    {"id":"shared-model","object":"model","owned_by":"xai","capabilities":{"reasoning_effort":["xai-level"]}}
                ]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let chatgpt_stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"models":[
                    {"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","visibility":"list","supported_reasoning_levels":[{"effort":"minimal"},{"effort":"high"},{"effort":"experimental-future"}],"default_reasoning_level":"minimal","context_window":500000,"base_instructions":"private prompt"},
                    {"slug":"shared-model","visibility":"list","supported_reasoning_levels":[{"effort":"openai-level"}],"default_reasoning_level":"openai-level"}
                ]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });

    // Deliberately insert providers in reverse precedence order. The merged
    // catalogue must still use ProviderId::all's stable ordering.
    let harness = harness_with_specs(
        "secret-key",
        vec![
            (
                &chatgpt_stub,
                chatgpt_stub_spec(&chatgpt_stub),
                Some("chatgpt-token"),
            ),
            (&xai_stub, stub_spec(&xai_stub), Some("grok-token")),
        ],
    );
    let http = agent();
    let response = http
        .get(harness.url("/v1/models"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().expect("merged data array");
    let ids: Vec<&str> = data
        .iter()
        .map(|model| model["id"].as_str().expect("model id"))
        .collect();
    assert_eq!(ids, vec!["grok-4.7", "shared-model", "gpt-6.1-sol"]);

    let grok = data.iter().find(|model| model["id"] == "grok-4.7").unwrap();
    assert_eq!(
        grok["reasoning"]["supported_efforts"],
        json!(["low", "future-level", "high"])
    );
    assert_eq!(grok["reasoning"]["default_effort"], "future-level");
    assert_eq!(
        grok["capabilities"]["reasoning_effort"],
        json!(["low", "future-level", "high"])
    );
    assert_eq!(
        grok["capabilities"]["default_reasoning_effort"],
        "future-level"
    );
    assert_eq!(grok["capabilities"]["tools"], true);

    let chatgpt = data
        .iter()
        .find(|model| model["id"] == "gpt-6.1-sol")
        .unwrap();
    assert_eq!(
        chatgpt["reasoning"]["supported_efforts"],
        json!(["minimal", "high", "experimental-future"])
    );
    assert_eq!(chatgpt["reasoning"]["default_effort"], "minimal");
    assert_eq!(
        chatgpt["supported_reasoning_levels"][0]["effort"],
        "minimal"
    );
    assert_eq!(chatgpt["default_reasoning_level"], "minimal");
    assert!(chatgpt.get("base_instructions").is_none());

    // XAI is the stable first provider, so its native record wins a duplicate
    // ID even though the fixtures were inserted in the opposite order.
    let shared = data
        .iter()
        .find(|model| model["id"] == "shared-model")
        .unwrap();
    assert_eq!(shared["owned_by"], "xai");
    assert_eq!(
        shared["reasoning"]["supported_efforts"],
        json!(["xai-level"])
    );

    let single_model = |id: &str| {
        let path = format!("/v1/models/{id}");
        let response = http
            .get(&harness.url(&path))
            .header("Authorization", "Bearer secret-key")
            .call()
            .expect("the single-model route answers");
        assert_eq!(response.status().as_u16(), 200);
        read_json_body(response)
    };
    assert_eq!(single_model("grok-4.7"), *grok);
    assert_eq!(single_model("gpt-6.1-sol"), *chatgpt);
}

#[test]
fn an_unauthenticated_model_catalogue_request_is_rejected_before_upstream_access() {
    let stub = Stub::start(|_| Reply::json(200, r#"{"data":[]}"#));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/v1/models"))
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 401);
    assert!(
        stub.requests().is_empty(),
        "unauthenticated catalogue requests must not reach providers"
    );
}

#[test]
fn an_unsigned_provider_contributes_no_catalogue_entries() {
    let xai_stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(200, r#"{"data":[{"id":"grok-signed","object":"model"}]}"#)
        } else {
            Reply::json(404, "{}")
        }
    });
    let chatgpt_stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"models":[{"slug":"gpt-unsigned","visibility":"list"}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_specs(
        "secret-key",
        vec![
            (&xai_stub, stub_spec(&xai_stub), Some("grok-token")),
            (&chatgpt_stub, chatgpt_stub_spec(&chatgpt_stub), None),
        ],
    );

    let response = agent()
        .get(harness.url("/v1/models"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"][0]["id"], "grok-signed");
    assert!(
        chatgpt_stub.requests().is_empty(),
        "unsigned providers must not be queried"
    );
}

#[test]
fn catalogue_failures_do_not_create_reasoning_claims() {
    let xai_stub = Stub::start(|_| {
        Reply::json(
            503,
            r#"{"data":[{"id":"unavailable-model","capabilities":{"reasoning_effort":["high"]}}]}"#,
        )
    });
    let chatgpt_stub = Stub::start(|request| {
        if request.path == "/v1/models" {
            Reply::json(
                200,
                r#"{"models":[{"slug":"gpt-no-levels","display_name":"GPT","visibility":"list"}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_specs(
        "secret-key",
        vec![
            (&xai_stub, stub_spec(&xai_stub), Some("grok-token")),
            (
                &chatgpt_stub,
                chatgpt_stub_spec(&chatgpt_stub),
                Some("chatgpt-token"),
            ),
        ],
    );

    let response = agent()
        .get(harness.url("/v1/models"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"][0]["id"], "gpt-no-levels");
    assert!(body["data"][0].get("reasoning").is_none());
    assert!(body["data"][0].get("capabilities").is_none());
    assert_eq!(xai_stub.requests().len(), 1);
}

#[test]
fn grok_reasoning_effort_is_forwarded_unchanged() {
    let stub = Stub::start(|request| {
        if request.path == "/v1/chat/completions" {
            Reply::json(200, r#"{"id":"chatcmpl-stub","choices":[]}"#)
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "secret-key", "grok-token");
    let body = json!({
        "model":"grok-4.7",
        "messages":[{"role":"user","content":"hello"}],
        "reasoning_effort":"future-level"
    });

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .expect("the proxy answers");
    assert_eq!(response.status().as_u16(), 200);
    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    let forwarded: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(forwarded["reasoning_effort"], "future-level");
    assert!(forwarded.get("reasoning").is_none());
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .get(harness.url("/v1/models/grok-9.9"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let agent = agent();
    for _ in 0..3 {
        let response = agent
            .get(harness.url("/v1/models"))
            .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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
    let harness = harness(&stub, "secret-key", "grok-token");

    agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
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

#[test]
fn a_streamed_response_without_a_content_type_is_still_a_stream() {
    // `api.openai.com` answers a streamed Responses request with no
    // `Content-Type` header at all. A header-only check sends the stream down
    // the plain-body path and tries to parse `event: response.created` as
    // JSON, which fails with "the response is not JSON".
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/responses") {
            Reply::Stream {
                status: 200,
                // No Content-Type header, exactly as OpenAI sends it.
                content_type: "",
                chunks: vec![
                    (
                        Duration::from_millis(0),
                        concat!(
                            "event: response.created\n",
                            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
                        )
                        .to_string(),
                    ),
                    (
                        Duration::from_millis(0),
                        concat!(
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
                        )
                        .to_string(),
                    ),
                    (
                        Duration::from_millis(0),
                        concat!(
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi there\"}]}]}}\n\n",
                        )
                        .to_string(),
                    ),
                ],
            }
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_spec(
        &stub,
        "secret-key",
        "chatgpt-token",
        chatgpt_stub_spec(&stub),
    );

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(
            json!({"model": "gpt-5.6-sol", "messages": [{"role": "user", "content": "hello"}]})
                .to_string(),
        )
        .expect("the proxy answers");

    let body = read_json_body(response);
    assert_eq!(body["object"], "chat.completion", "{body}");
    assert_eq!(
        body["choices"][0]["message"]["content"], "hi there",
        "{body}"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "stop", "{body}");
}

#[test]
fn a_streamed_response_without_a_content_type_reaches_a_streaming_client() {
    // The same missing header must not break the streaming path either.
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/responses") {
            Reply::Stream {
                status: 200,
                content_type: "",
                chunks: vec![
                    (
                        Duration::from_millis(0),
                        concat!(
                            "event: response.output_text.delta\n",
                            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
                        )
                        .to_string(),
                    ),
                    (
                        Duration::from_millis(0),
                        concat!(
                            "event: response.completed\n",
                            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi there\"}]}]}}\n\n",
                        )
                        .to_string(),
                    ),
                ],
            }
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_spec(
        &stub,
        "secret-key",
        "chatgpt-token",
        chatgpt_stub_spec(&stub),
    );

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(json!({"model": "gpt-5.6-sol", "messages": [{"role": "user", "content": "hello"}], "stream": true}).to_string())
        .expect("the proxy answers");

    assert_eq!(response.status(), 200);
    let text = response.into_body().read_to_string().expect("stream text");
    // The deltas arrive as they do upstream; the completed event closes the
    // stream rather than repeating the whole answer.
    assert!(text.contains("chat.completion.chunk"), "{text}");
    assert!(text.contains(r#""content":"hi""#), "{text}");
    assert!(text.contains(r#""finish_reason":"stop""#), "{text}");
    assert!(text.contains("[DONE]"), "{text}");
}

#[test]
fn usage_route_is_authenticated_cached_and_attaches_fresh_inference_state() {
    let stub = Stub::start(|request| {
        if request.path == crate::provider::usage::xai::BILLING_PATH {
            Reply::json(
                200,
                r#"{"config":{"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-11-13T03:07:08Z","end":"2026-11-20T03:07:08Z"},"creditUsagePercent":42.0,"productUsage":[{"product":"GrokChat","usagePercent":42.0}]}}"#,
            )
        } else if request.path == crate::provider::usage::xai::SETTINGS_PATH {
            Reply::json(200, r#"{"plan":"SuperGrok"}"#)
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness(&stub, "secret-key", "grok-token");

    let unauthorized = agent()
        .get(harness.url("/usage"))
        .call()
        .expect("the proxy answers");
    assert_eq!(unauthorized.status(), 401);
    assert_eq!(
        harness
            .usage_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(
        stub.requests().is_empty(),
        "unauthorized requests must not collect"
    );

    let first = agent()
        .get(harness.url("/usage"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the usage route answers");
    assert_eq!(first.status(), 200);
    let headers = first.headers();
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store")
    );
    assert!(
        headers
            .get("x-eggpass-usage-age")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some(),
        "age header must be integer seconds"
    );
    assert!(
        headers
            .get("x-eggpass-usage-updated-at")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.ends_with('Z')),
        "updated-at must be an RFC 3339 UTC timestamp"
    );
    let first_body = read_json_body(first);
    assert_eq!(first_body["providers"].as_array().unwrap().len(), 1);
    assert_eq!(first_body["providers"][0]["provider"], "xai");
    assert_eq!(first_body["providers"][0]["status"], "reported");
    assert_eq!(
        first_body["providers"][0]["last_inference"],
        serde_json::Value::Null
    );
    assert!(!first_body.to_string().contains("user@example.com"));
    assert!(!first_body.to_string().contains("grok-token"));

    let upstream = stub.requests();
    assert_eq!(
        upstream.len(),
        2,
        "cold collection calls billing and settings once"
    );
    assert!(
        upstream
            .iter()
            .any(|request| request.path == crate::provider::usage::xai::BILLING_PATH)
    );
    assert!(
        upstream
            .iter()
            .any(|request| request.path == crate::provider::usage::xai::SETTINGS_PATH)
    );
    assert_eq!(
        harness
            .usage_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    harness.inferences.record(ProviderId::Xai, 429);
    let cached = agent()
        .get(harness.url("/v1/usage"))
        .header("Authorization", "Bearer secret-key")
        .call()
        .expect("the cached report answers");
    assert_eq!(cached.status(), 200);
    let cached_body = read_json_body(cached);
    assert_eq!(cached_body["providers"][0]["last_inference"], 429);
    assert_eq!(
        harness
            .usage_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        stub.requests().len(),
        2,
        "a fresh cache hit must not query upstream"
    );

    let method = agent()
        .post(harness.url("/v1/usage"))
        .header("Authorization", "Bearer secret-key")
        .send("{}")
        .expect("the route reports its allowed method");
    assert_eq!(method.status(), 405);
    assert_eq!(
        method
            .headers()
            .get("allow")
            .and_then(|value| value.to_str().ok()),
        Some("GET")
    );
    assert_eq!(
        harness
            .usage_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[test]
fn the_daemon_usage_credential_cannot_authorize_other_routes() {
    let stub = Stub::start(|_| Reply::json(404, "{}"));
    let harness = harness(&stub, "regular-client-key", "provider-token");
    let secret = "server-test-usage-key";

    let usage = agent()
        .get(harness.url("/v1/usage"))
        .header("Authorization", &format!("Bearer {secret}"))
        .call()
        .expect("the scoped usage request answers");
    assert_eq!(usage.status(), 200);

    let models = agent()
        .get(harness.url("/v1/models"))
        .header("Authorization", &format!("Bearer {secret}"))
        .call()
        .expect("the proxy rejects out-of-scope credentials");
    assert_eq!(models.status(), 401);

    let inference = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", &format!("Bearer {secret}"))
        .header("Content-Type", "application/json")
        .send(chat_body().to_string())
        .expect("the proxy rejects the scoped credential");
    assert_eq!(inference.status(), 401);
    assert!(
        harness.keys.is_empty(),
        "the runtime secret is not an issued client key"
    );
}

#[test]
fn a_native_responses_request_is_forwarded_untouched() {
    // A Responses-only provider speaks the Responses API natively. A request
    // to `/v1/responses` must be forwarded as it is; translating it as if it
    // were a Chat Completions request loses the input entirely.
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/responses") {
            Reply::Stream {
                status: 200,
                content_type: "",
                chunks: vec![(
                    Duration::from_millis(0),
                    concat!(
                        "event: response.completed\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"status\":\"completed\",\"error\":null,\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"responses works\"}]}]}}\n\n",
                    )
                    .to_string(),
                )],
            }
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_spec(
        &stub,
        "secret-key",
        "chatgpt-token",
        chatgpt_stub_spec(&stub),
    );

    let response = agent()
        .post(harness.url("/v1/responses"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(
            json!({
                "model": "gpt-5.6-sol",
                "input": [{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}]
            })
            .to_string(),
        )
        .expect("the proxy answers");

    assert_eq!(response.status(), 200);
    let text = response.into_body().read_to_string().expect("body text");

    // The upstream saw the input, unchanged.
    let requests = stub.requests();
    let upstream = requests.first().expect("the upstream saw the request");
    let sent: serde_json::Value =
        serde_json::from_str(&upstream.body).expect("the upstream body is json");
    assert_eq!(
        sent["input"][0]["content"][0]["text"], "hello",
        "the native request was mangled: {sent}"
    );

    // And the answer came back in the Responses shape, not a Chat
    // Completions one.
    let answer: serde_json::Value = serde_json::from_str(&text).expect("a json body");
    assert_eq!(answer["object"], "response", "{answer}");
    assert_eq!(answer["status"], "completed", "{answer}");
    assert_eq!(
        answer["output"][0]["content"][0]["text"], "responses works",
        "{answer}"
    );
}

fn media_stub_spec(stub: &Stub) -> ProviderSpec {
    let mut spec = stub_spec(stub);
    spec.media_paths = &[MediaPath {
        method: "POST",
        path: "/v1/images/generations",
        response_types: &["application/json"],
    }];
    spec
}

#[test]
fn media_route_forwards_the_exact_body_and_preserves_content_type() {
    let upstream_body = r#"{"created":1,"data":[{"url":"https://example.invalid/synthetic.png"}]}"#;
    let expected_response_body = upstream_body.to_string();
    let stub = Stub::start(move |_| Reply::json(200, upstream_body));
    let harness = harness_with_spec(&stub, "test-key", "test-token", media_stub_spec(&stub));
    let request_body = br#"{ "model" : "grok-4.7", "prompt" : "synthetic test payload", "n" : 1 }"#;

    let response = agent()
        .post(harness.url("/images/generations?quality=low"))
        .header("Authorization", "Bearer test-key")
        .header("Content-Type", "application/json")
        .send(request_body.as_slice())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let body = response
        .into_body()
        .read_to_string()
        .expect("response body");
    assert_eq!(body, expected_response_body);

    let requests = stub.requests();
    let forwarded = requests.first().expect("the upstream saw the request");
    assert_eq!(forwarded.method, "POST");
    assert_eq!(forwarded.path, "/v1/images/generations?quality=low");
    assert_eq!(forwarded.body.as_bytes(), request_body);
}

#[test]
fn media_path_not_enabled_by_the_spec_keeps_the_unknown_endpoint_404() {
    let stub = Stub::start(|_| Reply::json(200, "{}"));
    let harness = harness(&stub, "test-key", "test-token");
    let response = agent()
        .post(harness.url("/v1/images/generations"))
        .header("Authorization", "Bearer test-key")
        .header("Content-Type", "application/json")
        .send(r#"{"model":"grok-4.7"}"#)
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 404);
    let body = response.into_body().read_to_string().expect("error body");
    assert!(body.contains("unknown endpoint"));
    assert!(stub.requests().is_empty());
}

#[test]
fn media_unrelated_unknown_paths_keep_the_404_without_a_model() {
    let stub = Stub::start(|_| Reply::json(200, "{}"));
    let harness = harness_with_spec(&stub, "test-key", "test-token", media_stub_spec(&stub));

    let post = agent()
        .post(harness.url("/v1/does-not-exist"))
        .header("Authorization", "Bearer test-key")
        .send("")
        .expect("the proxy answers");
    assert_eq!(post.status().as_u16(), 404);

    let get = agent()
        .get(harness.url("/v1/does-not-exist"))
        .header("Authorization", "Bearer test-key")
        .call()
        .expect("the proxy answers");
    assert_eq!(get.status().as_u16(), 404);
    assert!(stub.requests().is_empty());
}

#[test]
fn media_declared_path_without_a_model_answers_missing_model() {
    let stub = Stub::start(|_| Reply::json(200, "{}"));
    let harness = harness_with_spec(&stub, "test-key", "test-token", media_stub_spec(&stub));
    let response = agent()
        .post(harness.url("/v1/images/generations"))
        .header("Authorization", "Bearer test-key")
        .send("{}")
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 400);
    let body = response.into_body().read_to_string().expect("error body");
    let value: serde_json::Value = serde_json::from_str(&body).expect("json error");
    assert_eq!(value["error"]["code"], "missing_model");
    assert!(stub.requests().is_empty());
}

#[test]
fn media_path_with_the_wrong_method_answers_405_and_allow() {
    let stub = Stub::start(|_| Reply::json(200, "{}"));
    let harness = harness_with_spec(&stub, "test-key", "test-token", media_stub_spec(&stub));
    let body = r#"{"model":"grok-4.7"}"#;
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", harness.port))
        .expect("the proxy accepts a connection");
    write!(
        stream,
        "GET /v1/images/generations HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer test-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        harness.port,
        body.len(),
        body,
    )
    .expect("the GET body is sent");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("the response is read");

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    assert!(response.contains("\r\nAllow: POST\r\n"), "{response}");
    assert!(stub.requests().is_empty());
}

#[test]
fn media_response_with_an_unexpected_content_type_answers_502() {
    let stub = Stub::start(|_| Reply::Body {
        status: 200,
        content_type: "image/png",
        body: "synthetic image bytes".to_string(),
    });
    let harness = harness_with_spec(&stub, "test-key", "test-token", media_stub_spec(&stub));
    let response = agent()
        .post(harness.url("/v1/images/generations"))
        .header("Authorization", "Bearer test-key")
        .header("Content-Type", "application/json")
        .send(r#"{"model":"grok-4.7","prompt":"synthetic"}"#)
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 502);
    let body = response.into_body().read_to_string().expect("error body");
    let value: serde_json::Value = serde_json::from_str(&body).expect("json error");
    assert_eq!(value["error"]["code"], "unexpected_media_content_type");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("image/png")
    );
}

#[test]
fn a_chatgpt_chat_request_carries_the_image_part_upstream() {
    // The translated path must deliver what the catalogue advertises: an
    // image-only message becomes an `input_image` part, with the URL and
    // `detail` the client sent, and the message item survives translation.
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/responses") {
            Reply::json(
                200,
                r#"{"id":"resp_image","status":"completed","model":"gpt-5.6-sol","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"seen"}]}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_spec(
        &stub,
        "secret-key",
        "chatgpt-token",
        chatgpt_stub_spec(&stub),
    );

    let request = json!({
        "model": "gpt-5.6-sol",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": {
                    "url": "https://example.invalid/fixture.png",
                    "detail": "high"
                }
            }]
        }]
    });
    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(request.to_string())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(body["choices"][0]["message"]["content"], "seen", "{body}");

    let requests = stub.requests();
    let forwarded = requests.first().expect("the upstream saw the request");
    let sent: serde_json::Value = serde_json::from_str(&forwarded.body).expect("forwarded json");

    // The Chat shape is gone; the image is a Responses part.
    assert!(sent.get("messages").is_none(), "{sent}");
    assert_eq!(
        sent["input"][0]["content"][0]["type"], "input_image",
        "{sent}"
    );
    assert_eq!(
        sent["input"][0]["content"][0]["image_url"], "https://example.invalid/fixture.png",
        "{sent}"
    );
    assert_eq!(sent["input"][0]["content"][0]["detail"], "high", "{sent}");
}

#[test]
fn an_xai_chat_request_is_forwarded_byte_for_byte_with_media_parts() {
    // x.ai speaks both shapes, so its body must reach the upstream exactly as
    // the client sent it — including an image part Chat Completions defines.
    let stub = Stub::start(|_| {
        Reply::json(
            200,
            r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
        )
    });
    let harness = harness(&stub, "secret-key", "grok-token");

    let raw = json!({
        "model": "grok-4.7",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": {
                    "url": "https://example.invalid/fixture.png",
                    "detail": "high"
                }
            }]
        }]
    })
    .to_string();

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(raw.clone())
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let requests = stub.requests();
    let forwarded = requests.first().expect("the upstream saw the request");
    assert_eq!(forwarded.body, raw, "the body was rewritten");
}

#[test]
fn a_refusal_reaches_a_chat_client_as_a_content_part() {
    // The non-streaming path returns a refusal as a part array. A refusal-only
    // answer must not be an empty string.
    let stub = Stub::start(|request| {
        if request.path.starts_with("/v1/responses") {
            Reply::json(
                200,
                r#"{"id":"resp_refusal","status":"completed","model":"gpt-5.6-sol","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"The request is not supported."}]}]}"#,
            )
        } else {
            Reply::json(404, "{}")
        }
    });
    let harness = harness_with_spec(
        &stub,
        "secret-key",
        "chatgpt-token",
        chatgpt_stub_spec(&stub),
    );

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(
            json!({"model": "gpt-5.6-sol", "messages": [{"role": "user", "content": "hello"}]})
                .to_string(),
        )
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 200);
    let body = read_json_body(response);
    assert_eq!(
        body["choices"][0]["message"]["content"][0],
        json!({"type": "refusal", "refusal": "The request is not supported."}),
        "{body}"
    );
}

#[test]
fn a_request_without_a_model_answers_missing_model() {
    let stub = Stub::start(|_| Reply::json(200, "{}"));
    let harness = harness(&stub, "secret-key", "grok-token");

    let response = agent()
        .post(harness.url("/v1/chat/completions"))
        .header("Authorization", "Bearer secret-key")
        .header("Content-Type", "application/json")
        .send(r#"{"messages":[]}"#)
        .expect("the proxy answers");

    assert_eq!(response.status().as_u16(), 400);
    let body = read_json_body(response);
    assert_eq!(body["error"]["code"], "missing_model", "{body}");
    assert!(stub.requests().is_empty(), "the body was forwarded anyway");
}
