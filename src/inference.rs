//! What the last inference to each provider answered with.
//!
//! The daemon keeps one number per provider — the HTTP status the last
//! inference was answered with — and nothing else. There is no file, no log
//! and no history: the state lives in the daemon's memory and is gone when
//! the daemon stops. That is deliberate. A record that outlived the process
//! would be a log, and a log of inferences is a history of what the user
//! asked for.
//!
//! It is here so that `eggpass usage` can say something about a provider
//! that publishes no quota of its own: when the number is unknown, the last
//! answer is the only evidence there is about the account.
//!
//! The status is the one the proxy answered the client with, which is the
//! upstream's own status for an upstream refusal, and `401` or `502` for a
//! refusal or a connection that never reached the provider.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::provider::ProviderId;

/// The status of the last inference to each provider.
#[derive(Debug, Default)]
pub struct InferenceState {
    state: Mutex<BTreeMap<ProviderId, u16>>,
}

impl InferenceState {
    /// An empty state.
    pub fn new() -> InferenceState {
        InferenceState::default()
    }

    /// Record the status the last inference to a provider was answered with.
    pub fn record(&self, provider: ProviderId, status: u16) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.insert(provider, status);
    }

    /// The status of the last inference to every provider that has had one.
    pub fn snapshot(&self) -> BTreeMap<ProviderId, u16> {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

/// How a status reads as the outcome of an inference.
///
/// A 2xx is a success; anything else is a failure, and the status says which.
pub fn outcome_text(status: u16) -> String {
    if (200..300).contains(&status) {
        "succeeded".to_string()
    } else {
        format!("failed ({status})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_is_kept_per_provider() {
        let state = InferenceState::new();
        assert!(state.snapshot().is_empty());

        state.record(ProviderId::OpenAi, 200);
        state.record(ProviderId::Xai, 429);

        let snapshot = state.snapshot();
        assert_eq!(snapshot.get(&ProviderId::OpenAi), Some(&200));
        assert_eq!(snapshot.get(&ProviderId::Xai), Some(&429));
    }

    #[test]
    fn the_last_status_wins() {
        let state = InferenceState::new();
        state.record(ProviderId::OpenAi, 429);
        state.record(ProviderId::OpenAi, 200);
        assert_eq!(state.snapshot().get(&ProviderId::OpenAi), Some(&200));
    }

    #[test]
    fn the_snapshot_carries_every_provider_that_has_had_one() {
        let state = InferenceState::new();
        state.record(ProviderId::OpenAi, 200);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert!(!snapshot.contains_key(&ProviderId::Xai));
    }

    #[test]
    fn a_success_reads_as_a_success() {
        assert_eq!(outcome_text(200), "succeeded");
        assert_eq!(outcome_text(204), "succeeded");
    }

    #[test]
    fn a_refusal_reads_as_a_failure_with_its_status() {
        assert_eq!(outcome_text(429), "failed (429)");
        assert_eq!(outcome_text(401), "failed (401)");
        assert_eq!(outcome_text(502), "failed (502)");
    }
}
