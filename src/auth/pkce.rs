//! PKCE verifier and challenge generation (RFC 7636).
//!
//! The verifier is 32 random bytes encoded with base64url *without* padding.
//! The challenge is the base64url encoding of the SHA-256 digest of the
//! verifier's ASCII representation.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use sha2::{Digest, Sha256};

/// Number of random bytes in a code verifier.
pub const VERIFIER_BYTES: usize = 32;

/// Number of random bytes in the `state` and `nonce` parameters.
pub const STATE_BYTES: usize = 16;

/// Fill a buffer with cryptographically secure random bytes.
pub fn random_bytes(length: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; length];
    let mut rng = rand::rng();
    rng.fill_bytes(&mut buffer);
    buffer
}

/// A fresh PKCE code verifier.
pub fn random_verifier() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(VERIFIER_BYTES))
}

/// The S256 challenge for a code verifier.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh `state` parameter.
pub fn random_state() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(STATE_BYTES))
}

/// A fresh `nonce` parameter.
pub fn random_nonce() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes(STATE_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_7636_test_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_base64url_without_padding() {
        let verifier = random_verifier();
        assert_eq!(verifier.len(), 43, "{verifier}");
        assert!(!verifier.contains('='), "{verifier}");
        assert!(!verifier.contains('+'), "{verifier}");
        assert!(!verifier.contains('/'), "{verifier}");
    }

    #[test]
    fn challenges_are_base64url_without_padding() {
        let challenge = challenge_for(&random_verifier());
        assert_eq!(challenge.len(), 43, "{challenge}");
        assert!(!challenge.contains('='), "{challenge}");
    }

    #[test]
    fn values_are_not_repeated() {
        let first = random_verifier();
        let second = random_verifier();
        assert_ne!(first, second);
        assert_ne!(random_state(), random_state());
        assert_ne!(random_nonce(), random_nonce());
    }
}
