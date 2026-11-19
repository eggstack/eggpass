//! Shared machine-readable usage report assembly for the CLI and daemon API.

use serde::{Deserialize, Serialize};

use crate::auth::refresh::TokenManager;
use crate::config::{Timeouts, Upstream};
use crate::provider::usage::{ProviderUsage, UsageReport, xai};
use crate::provider::{ProviderId, usage::codex_app_server::CodexCollection};

/// One provider's answer, with the provider it belongs to.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ProviderEntry {
    pub provider: ProviderId,
    #[serde(flatten)]
    pub outcome: ProviderUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub caption: Option<String>,
    pub last_inference: Option<u16>,
}

/// The machine-readable form shared with eggpass usage --json.
#[derive(Serialize)]
pub(crate) struct UsageReportJson<'a> {
    pub providers: &'a [ProviderEntry],
}

/// A provider collection plus whether a retry is warranted.
#[derive(Debug, Clone)]
pub(crate) struct CollectedEntry {
    pub entry: ProviderEntry,
    pub timed_out: bool,
}

/// Providers which are enabled in the effective configuration, in display order.
pub(crate) fn enabled_provider_ids(config: &crate::config::Config) -> Vec<ProviderId> {
    ProviderId::all()
        .into_iter()
        .filter(|provider| config.providers.get(*provider).enabled)
        .collect()
}

/// Use Codex's report only for the OpenAI usage entry.
pub(crate) fn codex_outcome(
    provider: ProviderId,
    report: Option<UsageReport>,
) -> Option<(ProviderUsage, Option<&'static str>)> {
    if provider != ProviderId::OpenAi {
        return None;
    }
    report.map(|report| {
        (
            ProviderUsage::Reported { report },
            Some(crate::provider::usage::codex_app_server::SOURCE_LABEL),
        )
    })
}

/// Collect one provider using the existing shared token manager.
pub(crate) fn collect_entry(
    provider: ProviderId,
    xai_upstream: Upstream,
    tokens: &TokenManager,
    timeouts: Timeouts,
    codex: Option<CodexCollection>,
    billing_base_url: &str,
) -> CollectedEntry {
    let codex_timed_out = codex.as_ref().is_some_and(|result| result.timed_out);
    let codex_report = codex.and_then(|result| result.report);

    let (outcome, source, timed_out) = if provider == ProviderId::OpenAi {
        if let Some((outcome, source)) = codex_outcome(provider, codex_report) {
            (outcome, source, false)
        } else {
            let outcome = match tokens.current() {
                Ok(None) => ProviderUsage::NotSignedIn,
                Ok(Some(_)) => crate::provider::usage::openai::unavailable(),
                Err(err) => ProviderUsage::Failed {
                    reason: format!("could not read the stored credentials ({err})"),
                },
            };
            (outcome, None, codex_timed_out)
        }
    } else {
        match tokens.current() {
            Ok(None) => (ProviderUsage::NotSignedIn, None, false),
            Ok(Some(_)) => {
                let result = xai::collect_detailed(tokens, timeouts, billing_base_url);
                (result.outcome, None, result.timed_out)
            }
            Err(err) => (
                ProviderUsage::Failed {
                    reason: format!("could not read the stored credentials ({err})"),
                },
                None,
                false,
            ),
        }
    };

    CollectedEntry {
        entry: ProviderEntry {
            provider,
            outcome,
            source: source.map(str::to_owned),
            caption: (provider == ProviderId::Xai).then(|| xai::caption(xai_upstream).to_string()),
            last_inference: None,
        },
        timed_out,
    }
}

/// Apply a fresh daemon inference snapshot without changing the cached report.
pub(crate) fn attach_inferences(
    entries: &[ProviderEntry],
    inferences: &std::collections::BTreeMap<ProviderId, u16>,
) -> Vec<ProviderEntry> {
    entries
        .iter()
        .cloned()
        .map(|mut entry| {
            entry.last_inference = inferences.get(&entry.provider).copied();
            entry
        })
        .collect()
}

/// Keep an API-triggered xAI attempt bounded even when inference requests have long timeouts.
/// The configured timeout remains in force for inference and for the interactive CLI.
pub(crate) fn bounded_timeouts(timeouts: Timeouts) -> Timeouts {
    const MAX_USAGE_REQUEST: u64 = 15;
    const MAX_CONNECT: u64 = 5;
    Timeouts {
        connect_seconds: timeouts.connect_seconds.min(MAX_CONNECT),
        global_seconds: timeouts.global_seconds.min(MAX_USAGE_REQUEST),
        body_seconds: timeouts.body_seconds.min(MAX_USAGE_REQUEST),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_provider_filter_preserves_display_order() {
        let mut config = crate::config::Config::default();
        config.providers.xai.enabled = false;
        assert_eq!(enabled_provider_ids(&config), vec![ProviderId::OpenAi]);
        config.providers.openai.enabled = false;
        assert!(enabled_provider_ids(&config).is_empty());
    }

    #[test]
    fn api_timeouts_are_capped_without_extending_user_limits() {
        let bounded = bounded_timeouts(Timeouts {
            connect_seconds: 30,
            global_seconds: 1_800,
            body_seconds: 1_800,
        });
        assert_eq!(bounded.connect_seconds, 5);
        assert_eq!(bounded.global_seconds, 15);
        assert_eq!(bounded.body_seconds, 15);

        let shorter = bounded_timeouts(Timeouts {
            connect_seconds: 2,
            global_seconds: 8,
            body_seconds: 9,
        });
        assert_eq!(shorter.connect_seconds, 2);
        assert_eq!(shorter.global_seconds, 8);
        assert_eq!(shorter.body_seconds, 9);
    }
}
