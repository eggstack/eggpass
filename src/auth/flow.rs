//! The two ways of logging in.
//!
//! * [`login_loopback`] runs a browser-based authorization code flow on a
//!   **random ephemeral** loopback port. The port is deliberately unrelated to
//!   the API port, so logging in works while the daemon is running.
//! * [`login_device`] uses the RFC 8628 device-code grant and needs no
//!   browser on this machine at all.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use super::client::{AuthError, DevicePoll, OAuthClient, TokenSet, UserInfo};
use super::pkce;
use super::store::StoredAuth;
use crate::timeutil;

/// Hosts we are willing to send a user to for device-code verification.
const TRUSTED_VERIFICATION_HOSTS: &[&str] = &["accounts.x.ai", "auth.x.ai", "x.ai"];

/// Errors raised while running a login flow.
#[derive(Debug)]
pub enum FlowError {
    /// The OAuth endpoints refused or failed.
    Auth(AuthError),
    /// The user did not finish within the time limit.
    Timeout,
    /// The callback carried a `state` we did not issue.
    StateMismatch,
    /// The callback carried an error from the provider.
    Provider {
        error: String,
        description: Option<String>,
    },
    /// The callback did not carry an authorization code.
    NoCode,
    /// The provider asked us to visit a host we do not trust.
    UntrustedUrl(String),
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
            FlowError::UntrustedUrl(url) => {
                write!(
                    f,
                    "refusing to open `{url}`: it is not an x.ai verification page"
                )
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

/// Run the browser-based authorization code flow.
pub fn login_loopback(client: &OAuthClient) -> Result<LoginOutcome, FlowError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| FlowError::Io(format!("could not bind a loopback port: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| FlowError::Io(format!("could not read the loopback address: {err}")))?
        .port();

    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let verifier = pkce::random_verifier();
    let challenge = pkce::challenge_for(&verifier);
    let state = pkce::random_state();
    let nonce = pkce::random_nonce();

    let url = client.authorize_url(&redirect_uri, &challenge, &state, &nonce);

    println!("Opening your browser to complete the login.");
    println!("If it does not open, visit this URL yourself:\n\n  {url}\n");
    let _ = webbrowser::open(&url);

    let code = wait_for_callback(listener, &state)?;

    let tokens = client.exchange_code(&code, &redirect_uri, &verifier)?;
    Ok(finish(client, tokens))
}

/// Run the device-code flow.
pub fn login_device(client: &OAuthClient) -> Result<LoginOutcome, FlowError> {
    let device = client.request_device_code()?;

    if !is_trusted_verification_url(&device.verification_uri) {
        return Err(FlowError::UntrustedUrl(device.verification_uri));
    }
    if let Some(complete) = &device.verification_uri_complete {
        if !is_trusted_verification_url(complete) {
            return Err(FlowError::UntrustedUrl(complete.clone()));
        }
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
                return Ok(finish(client, tokens));
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
fn finish(client: &OAuthClient, tokens: TokenSet) -> LoginOutcome {
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

    let auth = StoredAuth {
        access_token: tokens.access_token.clone(),
        refresh_token: tokens.refresh_token.clone(),
        id_token: tokens.id_token.clone(),
        token_type: if tokens.token_type.is_empty() {
            "Bearer".to_string()
        } else {
            tokens.token_type.clone()
        },
        scope: tokens.scope.clone(),
        client_id: client.client_id().to_string(),
        issuer: client.issuer().to_string(),
        expires_at: tokens.expires_at,
        obtained_at: now,
        user_id: account.as_ref().and_then(|info| info.user_id.clone()),
        email: account.as_ref().and_then(|info| info.email.clone()),
    };

    LoginOutcome { auth }
}

/// Accept connections until the callback arrives or the deadline passes.
fn wait_for_callback(listener: TcpListener, state: &str) -> Result<String, FlowError> {
    listener.set_nonblocking(true).map_err(|err| {
        FlowError::Io(format!("could not configure the callback listener: {err}"))
    })?;

    let deadline = Instant::now() + Duration::from_secs(super::CALLBACK_TIMEOUT_SECS);

    loop {
        if Instant::now() >= deadline {
            return Err(FlowError::Timeout);
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let request = read_request(&mut stream)?;
                let Some(target) = request.split_whitespace().nth(1) else {
                    respond(&mut stream, 400, "Bad Request", "malformed request")?;
                    continue;
                };
                let (path, query) = split_target(target);

                if path != "/callback" {
                    respond(&mut stream, 404, "Not Found", "not found")?;
                    continue;
                }

                let params = parse_query(&query);

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

                let returned_state = params.get("state");
                if returned_state.map(String::as_str) != Some(state) {
                    respond(
                        &mut stream,
                        400,
                        "Login failed",
                        "The login could not be verified. You can close this window.",
                    )?;
                    return Err(FlowError::StateMismatch);
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
                return Ok(code.clone());
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

/// Whether a URL points at a page we are willing to send the user to.
pub fn is_trusted_verification_url(url: &str) -> bool {
    let Some(after_scheme) = url.split_once("://") else {
        return false;
    };
    let (scheme, rest) = after_scheme;
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Strip any userinfo and port.
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default();

    TRUSTED_VERIFICATION_HOSTS.contains(&host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_hosts_are_accepted() {
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai/oauth2/device"
        ));
        assert!(is_trusted_verification_url(
            "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"
        ));
        assert!(is_trusted_verification_url(
            "https://auth.x.ai/oauth2/device"
        ));
    }

    #[test]
    fn other_hosts_are_refused() {
        assert!(!is_trusted_verification_url(
            "https://evil.example.com/device"
        ));
        assert!(!is_trusted_verification_url(
            "https://accounts.x.ai.evil.com/device"
        ));
        assert!(!is_trusted_verification_url("http://accounts.x.ai/device"));
        assert!(!is_trusted_verification_url("accounts.x.ai"));
        assert!(!is_trusted_verification_url("javascript:alert(1)"));
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
}
