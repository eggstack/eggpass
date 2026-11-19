//! Host-owned bridge from synchronous Eggpass auth boundaries to `eggpass-auth`.
//!
//! The OAuth library never creates a runtime. Eggpass owns this runtime and
//! enters it from a dedicated scoped thread so synchronous callers do not call
//! `Runtime::block_on` from inside another Tokio runtime.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Runtime};

static RUNTIME: OnceLock<std::result::Result<Runtime, ()>> = OnceLock::new();

#[derive(Debug)]
pub(crate) enum BridgeError {
    RuntimeUnavailable,
    WorkerPanicked,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeUnavailable => f.write_str("could not initialize the OAuth runtime"),
            Self::WorkerPanicked => f.write_str("the OAuth runtime worker failed"),
        }
    }
}

impl std::error::Error for BridgeError {}

fn runtime() -> Result<&'static Runtime, BridgeError> {
    RUNTIME
        .get_or_init(|| {
            Builder::new_multi_thread()
                .enable_all()
                .thread_name("eggpass-oauth")
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| BridgeError::RuntimeUnavailable)
}

/// Run one library future on Eggpass's host-owned runtime and return to the
/// synchronous caller. The future may borrow from the calling stack; scoped
/// threads keep that borrow valid until the operation completes.
pub(crate) fn block_on<F>(future: F) -> Result<F::Output, BridgeError>
where
    F: Future + Send,
    F::Output: Send,
{
    let runtime = runtime()?;
    std::thread::scope(|scope| {
        scope
            .spawn(move || runtime.handle().block_on(future))
            .join()
            .map_err(|_| BridgeError::WorkerPanicked)
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use eggpass_auth::{EggfetchClient as Client, OAuthClient, OAuthProviderConfig};

    use super::block_on;

    #[test]
    fn host_bridge_runs_eggfetch_against_a_local_oauth_stub() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds");
        let address = listener.local_addr().expect("stub address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("client connects");
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = stream.read(&mut chunk).expect("request reads");
                assert_ne!(read, 0, "client closed before request headers");
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response_body = br#"{"access_token":"bridge-access","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("response headers write");
            stream
                .write_all(response_body)
                .expect("response body write");
            request
        });

        let base = format!("http://{address}");
        let provider = OAuthProviderConfig::custom("stub", &base, "client")
            .authorization_endpoint(format!("{base}/authorize"))
            .token_endpoint(format!("{base}/token"))
            .build()
            .expect("local profile validates");
        let client = OAuthClient::new(Client::new(), provider);
        let credentials = block_on(client.refresh_token("refresh-secret", None))
            .expect("host bridge runs the async request")
            .expect("stub returns credentials");
        let request = server.join().expect("stub finishes");

        assert!(request.starts_with(b"POST /token HTTP/1.1"));
        assert_eq!(credentials.access_token(), "bridge-access");
    }
}
