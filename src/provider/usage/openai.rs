//! The Sign in with ChatGPT (SIWC) usage fallback.
//!
//! The optional Codex app-server source is queried separately by
//! [`super::codex_app_server`]. This collector describes only what Eggpass's
//! own SIWC credential can report: the SIWC API does not expose quota windows,
//! so the honest fallback is an unavailable result with the provider's
//! documented signals and usage page.
//!
//! The SIWC documentation enumerates every endpoint it defines, and none of
//! them returns quota. The only quota signals it publishes are error codes,
//! and it says what to do with them:
//!
//! | Code | Status | Meaning |
//! | --- | --- | --- |
//! | `subscription_sharing_usage_limit_exceeded` | 429 | The shared allowance is exhausted |
//! | `subscription_sharing_usage_unavailable` | 503 | The allowance could not be read |
//! | `subscription_sharing_user_not_eligible` | 403 | This account has no shared allowance |
//!
//! > *"Do not assume the entire plan is empty or infer a reset time from this
//! > code alone."* — SIWC documentation
//!
//! That instruction is the design rule for this half of the feature: the
//! honest answer is that the number is unavailable, with the reasons and the
//! codes the provider does publish.

use crate::provider::usage::ProviderUsage;

/// The page OpenAI points users at for their own usage.
pub const USAGE_PAGE: &str = "https://chatgpt.com/settings/usage";

/// Where the quota would have come from, had it been exposed.
///
/// The Sign in with ChatGPT grant carries no quota scope: the token says what
/// the account may do, not how much of the allowance is left.
pub const SOURCE: &str = "not exposed by OAuth";

/// The error codes Sign in with ChatGPT publishes about the shared allowance.
pub const SIGNALS: &[&str] = &[
    "429 subscription_sharing_usage_limit_exceeded",
    "503 subscription_sharing_usage_unavailable",
    "403 subscription_sharing_user_not_eligible",
];

/// What ChatGPT can say about the allowance.
///
/// Nothing, as a number. This is the answer, not a stub: it makes no network
/// call, so it cannot break, and it cannot leak a token.
///
/// The wording says what is true — the *authentication mode* does not expose
/// a quota API. It does not say "ChatGPT has no usage information": the user
/// has usage information, OpenAI just does not publish a way to read it with
/// this credential.
/// The structured fallback when no Codex usage report is available.
pub fn unavailable() -> ProviderUsage {
    ProviderUsage::Unavailable {
        reason: "Sign in with ChatGPT does not expose a quota API".to_string(),
        source: Some(SOURCE.to_string()),
        hint: Some(USAGE_PAGE.to_string()),
        signals: SIGNALS.iter().map(|signal| (*signal).to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signals_name_the_documented_codes() {
        assert_eq!(SIGNALS.len(), 3);
        assert!(SIGNALS[0].contains("subscription_sharing_usage_limit_exceeded"));
        assert!(SIGNALS[1].contains("subscription_sharing_usage_unavailable"));
        assert!(SIGNALS[2].contains("subscription_sharing_user_not_eligible"));
    }

    #[test]
    fn the_usage_page_is_openais_own() {
        assert_eq!(USAGE_PAGE, "https://chatgpt.com/settings/usage");
    }

    #[test]
    fn the_source_says_the_grant_is_what_is_missing() {
        assert_eq!(SOURCE, "not exposed by OAuth");
    }

    #[test]
    fn the_answer_names_the_source_and_the_place_to_look() {
        match unavailable() {
            ProviderUsage::Unavailable {
                reason,
                source,
                hint,
                signals,
            } => {
                assert!(reason.contains("does not expose a quota API"), "{reason}");
                assert_eq!(source.as_deref(), Some(SOURCE));
                assert_eq!(hint.as_deref(), Some(USAGE_PAGE));
                assert_eq!(signals.len(), 3);
            }
            other => panic!("expected an unavailable answer, got {other:?}"),
        }
    }
}
