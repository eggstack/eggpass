//! The ChatGPT usage collector.
//!
//! ChatGPT has no usable quota endpoint for a Sign in with ChatGPT
//! credential, and that is not a gap in this implementation — it is the
//! documented shape of the thing.
//!
//! The Codex app-server does expose `account/rateLimits/read`, and the
//! response shape is exactly right: `usedPercent` is an integer 0–100,
//! `resetsAt` is unix seconds, `planType` enumerates the plans. But the
//! app-server reads a *Codex-managed* ChatGPT login out of
//! `$CODEX_HOME/auth.json`, and behind that call it reaches a private,
//! undocumented consumer backend, `chatgpt.com/backend-api/wham/usage`, which
//! needs a `chatgpt-account-id` header. A Sign in with ChatGPT credential
//! carries no workspace id, and there is no documented way to read one out of
//! it: the SIWC documentation says plainly that *"Email and `sub` are not
//! workspace identifiers"* — the workspace is bound server-side to the issued
//! client id. Refreshing there would break as well, because Codex refreshes
//! with its own client id and a foreign refresh token does not refresh
//! there.
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

use crate::provider::ProviderRuntime;
use crate::provider::usage::ProviderUsage;

/// The page OpenAI points users at for their own usage.
pub const USAGE_PAGE: &str = "https://chatgpt.com/settings/usage";

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
pub fn collect(_runtime: &ProviderRuntime) -> ProviderUsage {
    ProviderUsage::Unavailable {
        reason: "Sign in with ChatGPT does not expose a quota API".to_string(),
        hint: Some(USAGE_PAGE.to_string()),
        signals: SIGNALS.to_vec(),
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
}
