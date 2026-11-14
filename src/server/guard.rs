//! The authority guard.
//!
//! A loopback HTTP server is reachable from a web page, and a page can make
//! its own name resolve to `127.0.0.1` — DNS rebinding — at which point the
//! request it sends here is same-origin and it can read the answer. The clue
//! is the `Host` header: a rebound request carries the attacker's name, not
//! one of ours.
//!
//! The client key is the real boundary — `eggpass run` refuses to start
//! without one, so a rebound page cannot authenticate. This guard is the
//! second lock: it keeps a future regression, or a proxy that forwards a
//! foreign `Host`, from turning the server into a same-origin endpoint again.
//!
//! On a non-loopback bind the check is skipped. The names a LAN address is
//! reachable by are not knowable from the configuration, and a key is
//! mandatory there anyway.

use super::{Request, Response};

/// Refuse a request whose `Host` is not one this server answers to.
///
/// Returns `None` when the request is acceptable, or a `421` response when it
/// is not. The body names the expected authority and never echoes the one
/// that arrived: the value is attacker-controlled.
pub fn check(request: &Request, bound_host: &str, port: u16) -> Option<Response> {
    if !crate::config::is_loopback_host(bound_host) {
        return None;
    }

    // HTTP/1.0 clients may omit `Host`, and a browser always sends it. There
    // is nothing to rebind without it.
    let value = request.header("host")?;

    let allowed = authority_host(value).is_some_and(|host| is_allowed(&host, bound_host));
    if allowed {
        return None;
    }

    let expected = url_authority(bound_host, port);
    Some(super::openai_error_with_code(
        421,
        &format!(
            "the request was addressed to a host this server does not serve; use \
             http://{expected}, or forward `Host: {expected}` if a proxy is in front"
        ),
        "invalid_request_error",
        Some("invalid_host"),
    ))
}

/// The host component of a `Host` header, normalised.
///
/// Returns `None` for an authority that cannot be understood — a bare IPv6
/// literal without brackets, a port that is not a number, or an empty value.
fn authority_host(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let host = if let Some(rest) = value.strip_prefix('[') {
        let end = rest.find(']')?;
        let inside = &rest[..end];
        if inside.is_empty() {
            return None;
        }
        let after = &rest[end + 1..];
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        format!("[{inside}]")
    } else {
        match value.matches(':').count() {
            0 => value.to_string(),
            1 => {
                let (host, port) = value.rsplit_once(':')?;
                let port_is_a_number =
                    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit());
                if host.is_empty() || !port_is_a_number {
                    return None;
                }
                host.to_string()
            }
            // An unbracketed IPv6 literal is not a valid authority.
            _ => return None,
        }
    };

    let mut host = host.to_ascii_lowercase();
    if host.ends_with('.') {
        host.pop();
    }
    if host.is_empty() { None } else { Some(host) }
}

/// Whether an authority's host is one a loopback bind answers to.
fn is_allowed(host: &str, bound_host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
        // Browsers force `.localhost` to loopback and an attacker cannot serve
        // a page from it, so the suffix rule cannot be claimed by a rebind.
        || host.ends_with(".localhost")
        || host == bound_host
}

/// The authority to put in the refusal message and in documentation.
fn url_authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_host(value: Option<&str>) -> Request {
        let headers = match value {
            Some(value) => vec![("Host".to_string(), value.to_string())],
            None => Vec::new(),
        };
        Request {
            method: "GET".to_string(),
            target: "/v1/models".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    fn refused(value: &str) -> bool {
        check(&request_with_host(Some(value)), "127.0.0.1", 8484).is_some()
    }

    #[test]
    fn the_loopback_authorities_are_accepted() {
        for value in [
            "127.0.0.1:8484",
            "127.0.0.1",
            "localhost:8484",
            "localhost",
            "LocalHost:8484",
            "127.0.0.1.:8484",
            "[::1]:8484",
            "[::1]",
            "eggpass.localhost:8484",
            " 127.0.0.1:8484 ",
        ] {
            assert!(!refused(value), "{value} was refused");
        }
    }

    #[test]
    fn a_lookalike_authority_is_refused() {
        for value in [
            "evil.example:8484",
            "localhost.attacker.example",
            "127.0.0.1.evil.example",
            "evil-localhost:8484",
            "127.0.0.1:not-a-port",
            "127.0.0.1:",
            "::1",
            "[::1",
            "",
        ] {
            assert!(refused(value), "{value} was accepted");
        }
    }

    #[test]
    fn a_missing_host_is_allowed() {
        let request = request_with_host(None);
        assert!(check(&request, "127.0.0.1", 8484).is_none());
    }

    #[test]
    fn the_bound_literal_is_accepted() {
        let request = request_with_host(Some("127.0.0.2:8484"));
        assert!(check(&request, "127.0.0.2", 8484).is_none());
    }

    #[test]
    fn a_non_loopback_bind_skips_the_check() {
        let request = request_with_host(Some("something.example:8484"));
        assert!(check(&request, "0.0.0.0", 8484).is_none());
        assert!(check(&request, "192.168.1.10", 8484).is_none());
    }

    #[test]
    fn the_refusal_names_the_expected_authority_and_not_the_received_one() {
        let request = request_with_host(Some("evil.example:8484"));
        let response = check(&request, "127.0.0.1", 8484).expect("refused");

        assert_eq!(response.status, 421);
        let body = match response.body {
            crate::server::http::Body::Bytes(bytes) => String::from_utf8(bytes).expect("body"),
            _ => panic!("a refusal is a complete body"),
        };
        assert!(body.contains("http://127.0.0.1:8484"), "{body}");
        assert!(!body.contains("evil.example"), "{body}");
    }

    #[test]
    fn an_ipv6_bind_is_rendered_bracketed() {
        assert_eq!(url_authority("::1", 8484), "[::1]:8484");
        assert_eq!(url_authority("127.0.0.1", 8484), "127.0.0.1:8484");
    }
}
