//! The two ways of logging in.
//!
//! * [`login_loopback`] runs a browser-based authorization code flow on a
//!   **random ephemeral** loopback port. The port is deliberately unrelated to
//!   the API port, so logging in works while the daemon is running.
//! * [`login_device`] uses the RFC 8628 device-code grant and needs no
//!   browser on this machine at all. Only some providers have one.
//!
//! Both are written against a [`ProviderSpec`], so the differences between
//! providers — the callback path, the client id, the scopes — live in the
//! spec rather than in branches here.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use url::Url;

use super::client::{AuthError, DevicePoll, OAuthClient, TokenSet, UserInfo};
use super::pkce;
use super::store::StoredAuth;
use crate::provider::ProviderSpec;
use crate::timeutil;

/// Errors raised while running a login flow.
#[derive(Debug)]
pub enum FlowError {
    /// The OAuth endpoints refused or failed.
    Auth(AuthError),
    /// The user did not finish within the time limit.
    Timeout,
    /// The callback carried a `state` we did not issue.
    ///
    /// Retained so the error type does not churn. A foreign callback no
    /// longer produces this: the wait continues until a callback carries the
    /// state we issued.
    #[allow(dead_code)]
    StateMismatch,
    /// The callback carried an error from the provider.
    Provider {
        error: String,
        description: Option<String>,
    },
    /// The callback did not carry an authorization code.
    NoCode,
    /// The provider asked us to visit a host we do not trust.
    ///
    /// `host` is the host a WHATWG parse produced, when the string parsed.
    /// The raw URL is never stored: a hostile device response must not be
    /// written to the terminal.
    UntrustedUrl { host: Option<String> },
    /// The provider does not support the requested flow.
    Unsupported(&'static str),
    /// The provider does not publish a device-code endpoint.
    ///
    /// This is not a failure: the caller falls back to the browser flow, the
    /// same way the official Grok CLI does.
    DeviceUnavailable,
    /// Something went wrong reading from or writing to the socket.
    Io(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::Auth(err) => write!(f, "{err}"),
            FlowError::Timeout => {
                write!(f, "timed out waiting for the browser to complete the login")
            }
            FlowError::StateMismatch => {
                write!(
                    f,
                    "the callback carried a `state` we did not issue; refusing the login"
                )
            }
            FlowError::Provider { error, description } => match description {
                Some(description) => {
                    write!(f, "the provider refused the login: {error} ({description})")
                }
                None => write!(f, "the provider refused the login: {error}"),
            },
            FlowError::NoCode => write!(f, "the callback did not carry an authorization code"),
            FlowError::UntrustedUrl { host: Some(host) } => {
                write!(f, "refusing to open a verification page on {host}")
            }
            FlowError::UntrustedUrl { host: None } => {
                write!(f, "the verification URL could not be parsed")
            }
            FlowError::Unsupported(what) => {
                write!(f, "{what} is not supported for this provider")
            }
            FlowError::DeviceUnavailable => {
                write!(f, "the provider does not publish a device-code endpoint")
            }
            FlowError::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for FlowError {}

impl From<AuthError> for FlowError {
    fn from(err: AuthError) -> FlowError {
        FlowError::Auth(err)
    }
}

/// The outcome of a successful login.
#[derive(Debug)]
pub struct LoginOutcome {
    /// The credentials to store.
    pub auth: StoredAuth,
}

/// What the callback carried.
#[derive(Debug)]
pub struct Callback {
    /// The authorization code.
    pub code: String,
    /// The client id the provider issued, when it issued one.
    pub issued_client_id: Option<String>,
    /// The scope the provider reported, when it reported one.
    pub scope: Option<String>,
}

/// Run the browser-based authorization code flow.
///
/// `existing` is the credential already on disk, if any. It decides whether
/// this is a first registration — which is when the provider issues a client
/// id and the registration-only parameters are sent — or a reauthorization
/// that reuses the id it issued last time.
pub fn login_loopback(
    spec: &ProviderSpec,
    client: &OAuthClient,
    existing: Option<&StoredAuth>,
) -> Result<LoginOutcome, FlowError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| FlowError::Io(format!("could not bind a loopback port: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| FlowError::Io(format!("could not read the loopback address: {err}")))?
        .port();

    // The path has to match the provider's allow-list exactly; only the port
    // may vary.
    let redirect_uri = format!("http://127.0.0.1:{port}{}", spec.callback_path);
    let verifier = pkce::random_verifier();
    let challenge = pkce::challenge_for(&verifier);
    let state = pkce::random_state();
    let nonce = pkce::random_nonce();

    // A provider that issues its own client id needs the registration
    // parameters the first time round, and the saved id afterwards.
    let registering = spec.id == crate::provider::ProviderId::OpenAi
        && existing.is_none_or(|auth| auth.client_id.is_empty());

    // A pasted code can only be exchanged when the client id is fixed.
    let allow_pasted_code = !spec.issues_client_id;

    let mut extra: Vec<(String, String)> = Vec::new();
    if !registering
        && let Some(auth) = existing
        && let Some(id_token) = &auth.id_token
    {
        // The hint identifies the account, so the user does not have to pick
        // it again.
        extra.push(("id_token_hint".to_string(), id_token.clone()));
    }

    let url = client.authorize_url_with(
        &redirect_uri,
        &challenge,
        &state,
        &nonce,
        registering,
        &extra,
    );

    println!("Opening your browser to complete the login.");
    println!("If it does not open, visit this URL yourself:\n\n  {url}\n");
    if allow_pasted_code {
        println!(
            "If the page shows a code to copy instead of redirecting, paste it here and press Enter."
        );
    }
    let _ = webbrowser::open(&url);

    // A provider that shows a code to copy, rather than redirecting to the
    // loopback, can still be finished by pasting the code back here. The
    // official Grok CLI waits for the code from "loopback + stdin"; this is
    // the same idea.
    //
    // It is only possible when the provider does not issue a client id of its
    // own, because a pasted code carries no issued id to exchange.
    let pasted = if allow_pasted_code {
        watch_stdin_for_code()
    } else {
        None
    };

    let callback = wait_for_callback(listener, &state, spec.callback_path, pasted)?;

    // The token exchange has to present the id the provider issued, not the
    // entrypoint we started from.
    let exchange_client_id = callback
        .issued_client_id
        .clone()
        .unwrap_or_else(|| client.client_id().to_string());

    let tokens = client.exchange_code_as(
        &callback.code,
        &redirect_uri,
        &verifier,
        &exchange_client_id,
    )?;

    Ok(finish(
        spec,
        client,
        tokens,
        callback.issued_client_id,
        callback.scope,
        existing,
    ))
}

/// Run the device-code flow.
pub fn login_device(spec: &ProviderSpec, client: &OAuthClient) -> Result<LoginOutcome, FlowError> {
    if !spec.supports_device_flow {
        return Err(FlowError::Unsupported("the device-code flow"));
    }

    let device = match client.request_device_code() {
        Ok(device) => device,
        // xAI does not publish the device-code endpoint to every client. A
        // 404 there means "use the browser instead", not "the login failed".
        Err(AuthError::Http { status: 404, .. }) => return Err(FlowError::DeviceUnavailable),
        Err(err) => return Err(err.into()),
    };

    require_trusted_verification_url(&device.verification_uri, spec.trusted_device_hosts)?;
    if let Some(complete) = &device.verification_uri_complete {
        require_trusted_verification_url(complete, spec.trusted_device_hosts)?;
    }

    let open_url = device
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| device.verification_uri.clone());

    println!(
        "Open this page to approve the login:\n\n  {}\n",
        device.verification_uri
    );
    println!("Your code is: {}\n", device.user_code);
    let _ = webbrowser::open(&open_url);

    let mut interval = device.interval.max(1);
    let deadline = Instant::now() + Duration::from_secs(device.expires_in.max(60) as u64);

    loop {
        if Instant::now() >= deadline {
            return Err(FlowError::Timeout);
        }

        // The official CLI sleeps before the first poll: an immediate poll
        // returns `authorization_pending` and risks a `slow_down` penalty.
        std::thread::sleep(Duration::from_secs(interval as u64));

        match client.poll_device_code(&device.device_code)? {
            DevicePoll::Complete(tokens) => {
                println!("Approved.");
                return Ok(finish(spec, client, tokens, None, None, None));
            }
            DevicePoll::Pending => {}
            DevicePoll::SlowDown => {
                interval += 1;
            }
            DevicePoll::Denied(error) => {
                return Err(FlowError::Provider {
                    error,
                    description: None,
                });
            }
            DevicePoll::Expired => return Err(FlowError::Timeout),
        }
    }
}

/// Turn a token set into stored credentials, fetching the account details.
fn finish(
    spec: &ProviderSpec,
    client: &OAuthClient,
    tokens: TokenSet,
    issued_client_id: Option<String>,
    callback_scope: Option<String>,
    existing: Option<&StoredAuth>,
) -> LoginOutcome {
    let now = timeutil::now_unix();

    let mut account = None;
    if let Ok(info) = client.userinfo(&tokens.access_token) {
        account = Some(info);
    }
    if let Some(id_token) = &tokens.id_token {
        let claims = super::client::claims_from_id_token(id_token);
        let account = account.get_or_insert_with(UserInfo::default);
        if account.email.is_none() {
            account.email = claims.email;
        }
        if account.user_id.is_none() {
            account.user_id = claims.user_id;
        }
    }

    // The scope the provider actually granted wins over the one it reported
    // in the callback, and the callback's scope wins over the one we asked
    // for. The scope decides whether ChatGPT plan usage is enabled, so the
    // most authoritative one we have is the one we keep.
    let scope = tokens
        .scope
        .clone()
        .or(callback_scope)
        .or_else(|| existing.and_then(|auth| auth.scope.clone()));

    let client_id = issued_client_id
        .or_else(|| {
            existing
                .filter(|auth| !auth.client_id.is_empty())
                .map(|auth| auth.client_id.clone())
        })
        .unwrap_or_else(|| client.client_id().to_string());

    let ext_agent_host_id = existing
        .and_then(|auth| auth.ext_agent_host_id.clone())
        .or_else(|| {
            spec.extra_authorize_params
                .iter()
                .find(|(name, _)| name == "ext_agent_host_id")
                .map(|(_, value)| value.clone())
        });

    let mut auth = StoredAuth {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        id_token: tokens.id_token.clone(),
        token_type: if tokens.token_type.is_empty() {
            "Bearer".to_string()
        } else {
            tokens.token_type.clone()
        },
        scope,
        client_id,
        issuer: client.issuer().to_string(),
        expires_at: tokens.expires_at,
        obtained_at: now,
        user_id: account.as_ref().and_then(|info| info.user_id.clone()),
        email: account.as_ref().and_then(|info| info.email.clone()),
        ext_agent_host_id,
        plan_enabled: false,
    };

    // ChatGPT plan usage is only granted when the scope says so.
    auth.plan_enabled = auth.has_scope(crate::provider::openai::PLAN_SCOPE);

    LoginOutcome { auth }
}

/// Accept connections until the callback arrives, a code is pasted, or the
/// deadline passes.
fn wait_for_callback(
    listener: TcpListener,
    state: &str,
    callback_path: &str,
    pasted: Option<mpsc::Receiver<String>>,
) -> Result<Callback, FlowError> {
    listener.set_nonblocking(true).map_err(|err| {
        FlowError::Io(format!("could not configure the callback listener: {err}"))
    })?;

    let deadline = Instant::now() + Duration::from_secs(super::CALLBACK_TIMEOUT_SECS);

    loop {
        if Instant::now() >= deadline {
            return Err(FlowError::Timeout);
        }

        // A pasted code takes the same path as a callback, minus the issued
        // client id and scope, which only the callback carries.
        if let Some(pasted) = &pasted
            && let Ok(line) = pasted.try_recv()
        {
            let candidate = line.trim();
            if looks_like_authorization_code(candidate) {
                println!("Using the code you pasted.");
                return Ok(Callback {
                    code: candidate.to_string(),
                    issued_client_id: None,
                    scope: None,
                });
            }
            println!("That does not look like an authorization code. Ignoring it.");
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let request = read_request(&mut stream)?;
                let Some(target) = request.split_whitespace().nth(1) else {
                    respond(&mut stream, 400, "Bad Request", "malformed request")?;
                    continue;
                };
                let (path, query) = split_target(target);

                if path != callback_path {
                    respond(&mut stream, 404, "Not Found", "not found")?;
                    continue;
                }

                let params = parse_query(&query);

                // A connection is this login's result only when it carries
                // the state we issued. Anything else is a local process
                // poking the listener: answer and keep waiting. The page
                // does not echo the query or the expected state.
                if params.get("state").map(String::as_str) != Some(state) {
                    respond(
                        &mut stream,
                        400,
                        "Login failed",
                        "The login could not be verified. You can close this window.",
                    )?;
                    continue;
                }

                if let Some(error) = params.get("error") {
                    let description = params.get("error_description").cloned();
                    respond(
                        &mut stream,
                        400,
                        "Login failed",
                        "The login was refused. You can close this window.",
                    )?;
                    return Err(FlowError::Provider {
                        error: error.clone(),
                        description,
                    });
                }

                let Some(code) = params.get("code") else {
                    respond(
                        &mut stream,
                        400,
                        "Login failed",
                        "No authorization code was returned. You can close this window.",
                    )?;
                    return Err(FlowError::NoCode);
                };

                respond(
                    &mut stream,
                    200,
                    "Login complete",
                    "You are logged in. You can close this window and return to your terminal.",
                )?;
                return Ok(Callback {
                    code: code.clone(),
                    issued_client_id: params.get("client_id").cloned(),
                    scope: params.get("scope").cloned(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                return Err(FlowError::Io(format!(
                    "could not accept a connection: {err}"
                )));
            }
        }
    }
}

/// Watch stdin for a pasted authorization code.
///
/// Returns `None` when stdin is not a terminal. Reading a pipe would swallow
/// whatever the caller piped in and would block a scripted login forever, so
/// the watcher only exists for a person at a keyboard.
fn watch_stdin_for_code() -> Option<mpsc::Receiver<String>> {
    use std::io::IsTerminal;

    if !std::io::stdin().is_terminal() {
        return None;
    }

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if sender.send(trimmed).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    Some(receiver)
}

/// Whether a pasted line could be an authorization code.
///
/// Authorization codes are opaque, so this only rules out the things that
/// clearly are not one: an empty line, whitespace, or a fragment of prose.
fn looks_like_authorization_code(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() >= 8
        && candidate.len() <= 512
        && !candidate.chars().any(char::is_whitespace)
        && candidate.chars().all(|c| c.is_ascii_graphic())
}

/// Read the request head from a connection.
fn read_request(stream: &mut std::net::TcpStream) -> Result<String, FlowError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|err| FlowError::Io(format!("could not configure the socket: {err}")))?;

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|err| FlowError::Io(format!("could not read the callback: {err}")))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > 16 * 1024 {
            break;
        }
    }

    Ok(String::from_utf8_lossy(&buffer).to_string())
}

/// Split a request target into its path and query string.
fn split_target(target: &str) -> (String, String) {
    match target.split_once('?') {
        Some((path, query)) => (percent_decode(path), query.to_string()),
        None => (percent_decode(target), String::new()),
    }
}

/// Parse a query string into its parameters.
fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut params = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

/// Decode percent escapes and `+` in a query component.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Write a small HTML response.
fn respond(
    stream: &mut std::net::TcpStream,
    status: u16,
    title: &str,
    message: &str,
) -> Result<(), FlowError> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui;margin:4rem auto;max-width:32rem\">\
         <h1>{title}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        reason = reason_phrase(status),
        length = body.len(),
        body = body,
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|err| FlowError::Io(format!("could not answer the callback: {err}")))?;
    let _ = stream.flush();
    Ok(())
}

/// The reason phrase for a status code.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    }
}

/// How a verification URL was judged.
enum UrlTrust {
    Trusted,
    /// `host` is set only when a WHATWG parse produced one. A raw refusal
    /// does not parse, so it has none.
    Refused {
        host: Option<String>,
    },
}

/// Whether a URL points at a page we are willing to send the user to.
///
/// The host is the one a WHATWG parser produces, which is the host the
/// browser will open. A raw backslash, ASCII whitespace, or a byte below
/// `0x20` is refused before that parse: a trusted device URL does not contain
/// them, and a backslash the parser would treat as a delimiter must not be
/// able to aim the page elsewhere. The same refuse also rejects a backslash
/// the parser would land on a trusted host — fail closed.
pub fn is_trusted_verification_url(url: &str, trusted_hosts: &[&str]) -> bool {
    matches!(
        classify_verification_url(url, trusted_hosts),
        UrlTrust::Trusted
    )
}

/// Refuse `url`, or `Ok(())` when it is a page we will open.
///
/// The refusal names the parsed host when there is one, and otherwise says
/// the URL could not be parsed. It does not carry the raw string.
fn require_trusted_verification_url(url: &str, trusted_hosts: &[&str]) -> Result<(), FlowError> {
    if is_trusted_verification_url(url, trusted_hosts) {
        return Ok(());
    }
    let host = match classify_verification_url(url, trusted_hosts) {
        UrlTrust::Refused { host } => host,
        UrlTrust::Trusted => None,
    };
    Err(FlowError::UntrustedUrl { host })
}

fn classify_verification_url(url: &str, trusted_hosts: &[&str]) -> UrlTrust {
    if trusted_hosts.is_empty() {
        return UrlTrust::Refused { host: None };
    }
    if url
        .bytes()
        .any(|byte| byte == b'\\' || byte.is_ascii_whitespace() || byte < 0x20)
    {
        return UrlTrust::Refused { host: None };
    }

    let Ok(parsed) = Url::parse(url) else {
        return UrlTrust::Refused { host: None };
    };
    let host = verification_host(&parsed);
    if parsed.scheme() != "https" {
        return UrlTrust::Refused { host };
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return UrlTrust::Refused { host };
    }
    match &host {
        Some(host) if host_is_trusted(host, trusted_hosts) => UrlTrust::Trusted,
        _ => UrlTrust::Refused { host },
    }
}

/// The host a browser will open, with one trailing dot removed.
fn verification_host(parsed: &Url) -> Option<String> {
    let host = parsed.host_str()?;
    Some(host.strip_suffix('.').unwrap_or(host).to_string())
}

fn host_is_trusted(host: &str, trusted_hosts: &[&str]) -> bool {
    trusted_hosts
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XAI_HOSTS: &[&str] = &["accounts.x.ai", "auth.x.ai", "x.ai"];

    #[test]
    fn trusted_hosts_are_accepted() {
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai/oauth2/device",
            XAI_HOSTS
        ));
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234",
            XAI_HOSTS
        ));
        assert!(is_trusted_verification_url(
            "https://auth.x.ai/oauth2/device",
            XAI_HOSTS
        ));
        assert!(is_trusted_verification_url(
            "https://Accounts.X.AI/oauth2/device",
            XAI_HOSTS
        ));
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai:443/oauth2/device",
            XAI_HOSTS
        ));
    }

    #[test]
    fn other_hosts_are_refused() {
        assert!(!is_trusted_verification_url(
            "https://evil.example.com/device",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url(
            "https://accounts.x.ai.evil.com/device",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url(
            "http://accounts.x.ai/device",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url("accounts.x.ai", XAI_HOSTS));
        assert!(!is_trusted_verification_url(
            "javascript:alert(1)",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url(
            "https://accounts.x.ai/device path",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url(
            "https://accounts.x.ai/\u{1}device",
            XAI_HOSTS
        ));
    }

    #[test]
    fn a_provider_with_no_trusted_hosts_refuses_everything() {
        assert!(!is_trusted_verification_url(
            "https://auth.openai.com/device",
            &[]
        ));
    }

    #[test]
    fn query_strings_are_parsed() {
        let params = parse_query("code=abc&state=xyz&scope=openid%20email");
        assert_eq!(params.get("code").map(String::as_str), Some("abc"));
        assert_eq!(params.get("state").map(String::as_str), Some("xyz"));
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("openid email")
        );
    }

    #[test]
    fn request_targets_are_split() {
        let (path, query) = split_target("/callback?code=abc&state=xyz");
        assert_eq!(path, "/callback");
        assert_eq!(query, "code=abc&state=xyz");
    }

    #[test]
    fn percent_decoding_handles_escapes() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn an_authorization_code_is_recognised_by_its_shape() {
        assert!(looks_like_authorization_code("SplxlOBeZQQYbYS6WxSbIA"));
        assert!(looks_like_authorization_code("abc-123_DEF.456"));
        // Too short, too long, or carrying whitespace are not codes.
        assert!(!looks_like_authorization_code(""));
        assert!(!looks_like_authorization_code("short"));
        assert!(!looks_like_authorization_code("two words here"));
        assert!(!looks_like_authorization_code(&"x".repeat(513)));
    }

    #[test]
    fn a_pasted_code_finishes_the_login_without_a_callback() {
        // x.ai sometimes shows a code to copy instead of redirecting to the
        // loopback. The official CLI waits for the code from "loopback +
        // stdin"; this is the same thing.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send("SplxlOBeZQQYbYS6WxSbIA".to_string()).unwrap();

        let callback = wait_for_callback(listener, "state", "/callback", Some(receiver)).unwrap();
        assert_eq!(callback.code, "SplxlOBeZQQYbYS6WxSbIA");
        assert!(callback.issued_client_id.is_none());
        assert!(callback.scope.is_none());
    }

    #[test]
    fn a_pasted_line_that_is_not_a_code_is_ignored() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send("what was that again".to_string()).unwrap();

        // The first line is not a code, so the wait continues. A real code
        // after it still finishes the login.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = sender.send("SplxlOBeZQQYbYS6WxSbIA".to_string());
        });

        let callback = wait_for_callback(listener, "state", "/callback", Some(receiver)).unwrap();
        assert_eq!(callback.code, "SplxlOBeZQQYbYS6WxSbIA");
    }

    fn exchange_callback(port: u16, target: &str) -> String {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
            .unwrap_or_else(|err| panic!("callback connect to {target} failed: {err}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    fn callback_result(target: &str, state: &str) -> Result<Callback, FlowError> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state_owned = state.to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = wait_for_callback(listener, &state_owned, "/callback", None);
            let _ = sender.send(result);
        });
        std::thread::sleep(Duration::from_millis(50));
        let _ = exchange_callback(port, target);
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the callback wait finished")
    }

    #[test]
    fn a_foreign_callback_does_not_cancel_the_login() {
        let state = "issued-state-9f3c7a1e4b2d";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = state.to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = wait_for_callback(listener, &expected, "/callback", None);
            let _ = sender.send(result);
        });
        std::thread::sleep(Duration::from_millis(50));

        for target in [
            "/callback?error=access_denied",
            "/callback?error=access_denied&state=wrong",
            "/callback",
            "/nope",
        ] {
            let response = exchange_callback(port, target);
            assert!(
                response.starts_with("HTTP/1.1 400") || response.starts_with("HTTP/1.1 404"),
                "{target}: {response}"
            );
            assert!(!response.contains(state), "{target}: {response}");
        }

        let good = exchange_callback(port, &format!("/callback?code=goodcode12&state={state}"));
        assert!(good.starts_with("HTTP/1.1 200"), "{good}");

        let callback = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the login wait finished")
            .expect("a later callback with the issued state finishes the login");
        assert_eq!(callback.code, "goodcode12");
    }

    #[test]
    fn a_matching_state_still_ends_the_login() {
        match callback_result("/callback?error=access_denied&state=the-state", "the-state") {
            Err(FlowError::Provider { error, .. }) => assert_eq!(error, "access_denied"),
            other => panic!("expected a provider refusal, got {other:?}"),
        }

        match callback_result("/callback?state=the-state", "the-state") {
            Err(FlowError::NoCode) => {}
            other => panic!("expected no code, got {other:?}"),
        }

        let callback = callback_result("/callback?code=goodcode12&state=the-state", "the-state")
            .expect("a code with the issued state finishes the login");
        assert_eq!(callback.code, "goodcode12");
    }

    #[test]
    fn an_ignored_callback_does_not_echo_the_state() {
        let state = "issued-state-9f3c7a1e4b2d";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = state.to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = wait_for_callback(listener, &expected, "/callback", None);
            let _ = sender.send(result);
        });
        std::thread::sleep(Duration::from_millis(50));

        // The issued state is in the query, so an echo would show it.
        let response = exchange_callback(port, &format!("/callback?extra={state}&error=nope"));
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!response.contains(state), "{response}");

        let _ = exchange_callback(port, &format!("/callback?code=goodcode12&state={state}"));
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the login wait finished")
            .expect("the issued callback still finishes the login");
    }

    #[test]
    fn a_backslash_host_is_not_trusted() {
        for url in [
            r"https://evil.example\@accounts.x.ai/oauth2/device",
            r"https://evil.example\@auth.x.ai/oauth2/device",
            r"https://evil.example\@x.ai/oauth2/device",
        ] {
            assert!(
                !is_trusted_verification_url(url, XAI_HOSTS),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn a_raw_backslash_is_refused() {
        assert!(!is_trusted_verification_url(
            r"https://accounts.x.ai\@evil.example/oauth2/device",
            XAI_HOSTS
        ));
    }

    #[test]
    fn userinfo_is_refused() {
        assert!(!is_trusted_verification_url(
            "https://user@accounts.x.ai/oauth2/device",
            XAI_HOSTS
        ));
        assert!(!is_trusted_verification_url(
            "https://evil.example%5C@accounts.x.ai/device",
            XAI_HOSTS
        ));
    }

    #[test]
    fn a_trailing_dot_on_a_trusted_host_is_accepted() {
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai./device",
            XAI_HOSTS
        ));
    }

    #[test]
    fn the_untrusted_url_error_does_not_echo_the_url() {
        let raw = r"https://evil.example\@accounts.x.ai/oauth2/device?user_code=SECRET";
        let text = require_trusted_verification_url(raw, XAI_HOSTS)
            .expect_err("a backslash URL is refused")
            .to_string();
        assert!(!text.contains(raw), "{text}");
        assert!(!text.contains("user_code"), "{text}");
        assert!(!text.contains('\\'), "{text}");
        assert!(!text.contains("evil.example"), "{text}");
        assert!(text.contains("could not be parsed"), "{text}");

        let parsed = "https://evil.example/oauth2/device?user_code=SECRET";
        let text = require_trusted_verification_url(parsed, XAI_HOSTS)
            .expect_err("an untrusted host is refused")
            .to_string();
        assert!(text.contains("evil.example"), "{text}");
        assert!(!text.contains(parsed), "{text}");
        assert!(!text.contains("user_code"), "{text}");
        assert!(!text.contains("SECRET"), "{text}");
    }
}
