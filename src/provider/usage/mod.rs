//! What a provider can say about its own allowance.
//!
//! Usage is a provider capability, not a core feature: the two providers are
//! not symmetric and never will be. x.ai exposes the consumer subscription
//! pool over a documented-in-source HTTP endpoint; ChatGPT exposes no quota
//! API for a Sign in with ChatGPT credential at all. So the model lives here,
//! beside the providers, and the command asks the provider layer what it can
//! say.
//!
//! The dispatch is a plain match on [`ProviderId`], mirroring
//! [`crate::provider::spec_for`]. There are two implementations, and the
//! repository already dispatches to providers this way; a trait would be
//! ceremony.
//!
//! # Missing is not zero
//!
//! The single most important rule in this module: **an absent number is not a
//! zero**. The Grok billing endpoint is proto3 JSON, and proto3 omits
//! zero-valued scalars, so a fresh account has no `creditUsagePercent` field
//! at all. A parser that defaulted a missing number to `0.0` would tell a user
//! with a brand-new subscription that they have used 0 % of it, when the
//! truth is "the provider did not say". Every percentage is therefore an
//! [`Option`], and there is no `unwrap_or(0.0)` anywhere on this path.

pub mod openai;
pub mod xai;

use serde::Serialize;

use crate::provider::{ProviderId, ProviderRuntime};

/// What a provider can say about its own allowance.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// The provider the report is about.
    pub provider: ProviderId,
    /// The plan name, when the provider named one.
    pub plan: Option<String>,
    /// The quota windows, in the order they are printed.
    pub windows: Vec<QuotaWindow>,
    /// A per-product breakdown of the same allowance.
    pub breakdown: Vec<ProductUsage>,
    /// Purchased credit, when the provider reports any.
    pub credits: Option<Credits>,
    /// The provider's own statement about eligibility or exhaustion.
    pub state: Option<QuotaState>,
}

impl UsageReport {
    /// Whether the provider said nothing at all.
    ///
    /// A body with no configuration, or a configuration with nothing in it,
    /// is not a report of zero usage — it is no report.
    pub fn says_nothing(&self) -> bool {
        self.windows.is_empty()
            && self.breakdown.is_empty()
            && self.credits.is_none()
            && self.state.is_none()
    }
}

/// One quota window.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaWindow {
    /// The label the window is printed under.
    pub label: WindowLabel,
    /// How much of the window is used, 0–100.
    ///
    /// `None` means the provider did not say. It never means zero.
    pub used_percent: Option<f64>,
    /// When the window resets, in unix seconds.
    pub resets_at: Option<i64>,
    /// How long the window runs, in seconds.
    pub duration_secs: Option<i64>,
}

/// The name of a quota window.
#[derive(Debug, Clone, Serialize)]
// The normalised model names every window kind a provider may report, so a
// second provider does not have to change the model to add one. Only the
// kinds a provider actually reports are built today.
#[allow(dead_code)]
pub enum WindowLabel {
    Primary,
    Secondary,
    Weekly,
    Monthly,
    Daily,
    /// A label the provider used that we do not recognise.
    Other(String),
}

impl WindowLabel {
    /// The label as it is printed.
    pub fn display(&self) -> String {
        match self {
            WindowLabel::Primary => "Primary".to_string(),
            WindowLabel::Secondary => "Secondary".to_string(),
            WindowLabel::Weekly => "Weekly".to_string(),
            WindowLabel::Monthly => "Monthly".to_string(),
            WindowLabel::Daily => "Daily".to_string(),
            // A window whose period the provider did not name still needs
            // something to sit in the label column.
            WindowLabel::Other(raw) if raw.is_empty() => "Usage".to_string(),
            WindowLabel::Other(raw) => strip_period_prefix(raw),
        }
    }
}

/// A product's share of the same allowance.
#[derive(Debug, Clone, Serialize)]
pub struct ProductUsage {
    /// The product id, as the provider spells it.
    pub product: String,
    /// How much of the product's share is used, 0–100.
    ///
    /// `None` means the provider did not say. It never means zero.
    pub used_percent: Option<f64>,
}

/// Purchased credit.
#[derive(Debug, Clone, Serialize)]
pub struct Credits {
    /// A prepaid balance, in cents.
    pub prepaid_cents: Option<i64>,
    /// Amount spent against purchased credit, in cents.
    pub on_demand_used_cents: Option<i64>,
    /// The ceiling on that spending, in cents.
    pub on_demand_cap_cents: Option<i64>,
}

/// The provider's own statement about the allowance.
///
/// An explicit state is the provider's answer, and it takes precedence over
/// a calculated percentage.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
// The mechanism names every state a provider may report. Only the ones the
// providers report today are built; the rest wait for a field that says so.
#[allow(dead_code)]
pub enum QuotaState {
    /// The provider says the allowance is exhausted.
    LimitReached,
    /// The provider says this account is not entitled to the allowance.
    NotEligible,
    /// The provider says ordinary usage is not allowed.
    NotAllowed,
}

impl QuotaState {
    /// The state as it is printed.
    pub fn display(&self) -> &'static str {
        match self {
            QuotaState::LimitReached => "limit reached",
            QuotaState::NotEligible => "not eligible for this plan",
            QuotaState::NotAllowed => "not allowed",
        }
    }
}

/// What one provider had to say.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderUsage {
    /// The provider reported usage.
    Reported {
        /// The report.
        report: UsageReport,
    },
    /// The provider is not signed in on this machine.
    NotSignedIn,
    /// The provider has no supported way to report it.
    Unavailable {
        /// Why it is unavailable, in one line.
        reason: String,
        /// Where the quota would have come from, in one line.
        ///
        /// "Not exposed by OAuth" says something different from the reason:
        /// the reason says the provider has no quota API for this credential,
        /// and the source says the OAuth grant is what it is missing.
        source: Option<String>,
        /// Where the user can see it, when there is such a place.
        hint: Option<String>,
        /// The error codes the provider does publish, when it publishes any.
        signals: Vec<&'static str>,
    },
    /// The attempt failed.
    Failed {
        /// Why it failed, in one line.
        reason: String,
    },
}

/// Errors raised while reading a provider's usage.
#[derive(Debug)]
pub enum UsageError {
    /// A field could not be read.
    Malformed {
        /// The field that was being read, as the provider spells it.
        field: String,
        /// What was wrong with it.
        message: String,
    },
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsageError::Malformed { field, message } => {
                write!(f, "the usage response was malformed: {field} {message}")
            }
        }
    }
}

impl std::error::Error for UsageError {}

/// Ask a provider what it can say about its allowance.
///
/// The caller decides which providers to ask; this only routes the question
/// to the provider that can answer it.
pub fn collect(provider: ProviderId, runtime: &ProviderRuntime) -> ProviderUsage {
    match provider {
        ProviderId::Xai => xai::collect(runtime),
        ProviderId::OpenAi => openai::collect(runtime),
    }
}

/// Strip the proto enum prefix from a period type.
///
/// `USAGE_PERIOD_TYPE_WEEKLY` reads as `WEEKLY`; a type from another
/// vocabulary keeps its own prefix, because a prefix we do not recognise is
/// not one we should pretend to understand.
fn strip_period_prefix(raw: &str) -> String {
    const PREFIX: &str = "USAGE_PERIOD_TYPE_";
    match raw.get(..PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PREFIX) => match raw.get(PREFIX.len()..) {
            Some(rest) if !rest.is_empty() => rest.to_string(),
            _ => raw.to_string(),
        },
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_prefix_is_stripped_for_display() {
        assert_eq!(strip_period_prefix("USAGE_PERIOD_TYPE_WEEKLY"), "WEEKLY");
        assert_eq!(strip_period_prefix("usage_period_type_monthly"), "monthly");
        assert_eq!(strip_period_prefix("SOMETHING_ELSE"), "SOMETHING_ELSE");
        assert_eq!(
            strip_period_prefix("USAGE_PERIOD_TYPE_"),
            "USAGE_PERIOD_TYPE_"
        );
    }

    #[test]
    fn an_empty_report_says_nothing() {
        let report = UsageReport {
            provider: ProviderId::Xai,
            plan: None,
            windows: Vec::new(),
            breakdown: Vec::new(),
            credits: None,
            state: None,
        };
        assert!(report.says_nothing());
    }

    #[test]
    fn a_report_with_anything_in_it_does_not_say_nothing() {
        let report = UsageReport {
            provider: ProviderId::Xai,
            plan: None,
            windows: vec![QuotaWindow {
                label: WindowLabel::Weekly,
                used_percent: Some(1.0),
                resets_at: None,
                duration_secs: None,
            }],
            breakdown: Vec::new(),
            credits: None,
            state: None,
        };
        assert!(!report.says_nothing());
    }

    #[test]
    fn the_states_read_as_sentences() {
        assert_eq!(QuotaState::LimitReached.display(), "limit reached");
        assert_eq!(
            QuotaState::NotEligible.display(),
            "not eligible for this plan"
        );
        assert_eq!(QuotaState::NotAllowed.display(), "not allowed");
    }

    #[test]
    fn a_window_with_no_name_still_has_a_label() {
        assert_eq!(WindowLabel::Other(String::new()).display(), "Usage");
        assert_eq!(WindowLabel::Weekly.display(), "Weekly");
        assert_eq!(
            WindowLabel::Other("USAGE_PERIOD_TYPE_FORTNIGHTLY".to_string()).display(),
            "FORTNIGHTLY"
        );
    }
}
