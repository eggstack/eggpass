//! `eggpass usage`.
//!
//! One fresh read per provider, normalised, printed, and done. The command
//! needs no daemon: it reads the credential file and talks to the provider
//! directly, exactly as `status`'s model count does.
//!
//! The output is `status`'s: a label column, aligned, greppable, no colour.
//! It is meant to be piped and diffed.
//!
//! # What the numbers mean
//!
//! A quota number without a statement of which pool it belongs to is a lie
//! waiting to happen, so the x.ai block carries a caption that depends on the
//! configured upstream. On the default `api` upstream, `eggpass`'s inference
//! is metered against the xAI API tier and does **not** draw from the
//! consumer subscription the billing endpoint reports.
//!
//! # What is never printed
//!
//! No token, no `Authorization` header, no raw upstream body. The billing
//! body carries the account's email, its user id and its balances; it goes
//! to the provider and stops there.

use serde::Serialize;

use crate::cli::{CommandError, Context};
use crate::provider::usage::{ProviderUsage, UsageReport};
use crate::provider::{ProviderId, ProviderRuntime};
use crate::timeutil;

/// The width of the label column, so every value starts in the same place.
const LABEL_WIDTH: usize = 12;

/// The width of the product-name column under a window.
const PRODUCT_WIDTH: usize = 16;

/// The number of cells in the bar.
const BAR_CELLS: usize = 20;

/// The filled cell.
const FILLED: char = '█';

/// The empty cell.
const EMPTY: char = '░';

/// How far past a reset time a window counts as stale, in seconds.
const STALE_AFTER: i64 = 3_600;

/// Show how much of the subscription allowance is used.
pub fn usage(
    context: &Context,
    provider: Option<ProviderId>,
    json: bool,
) -> Result<(), CommandError> {
    let providers: Vec<ProviderId> = match provider {
        Some(provider) => vec![provider],
        None => ProviderId::all().to_vec(),
    };

    let entries: Vec<ProviderEntry> = providers
        .into_iter()
        .map(|provider| ProviderEntry {
            provider,
            outcome: collect_one(context, provider),
            // The caption travels with the report, in the JSON as well as in
            // the bars: a machine-readable number without it is as misleading
            // as a number without it on the terminal.
            caption: caption_for(provider, &context.config),
        })
        .collect();

    if json {
        let report = UsageReportJson {
            providers: &entries,
        };
        let text = serde_json::to_string_pretty(&report)
            .unwrap_or_else(|err| format!("{{\"error\":\"{err}\"}}"));
        println!("{text}");
    } else {
        print!("{}", render(&entries));
    }

    // A provider that is not signed in still gets a block, so the output does
    // not change shape — but a run in which nobody is signed in has nothing
    // to report at all.
    let signed_in = entries
        .iter()
        .filter(|entry| !matches!(entry.outcome, ProviderUsage::NotSignedIn))
        .count();
    if signed_in == 0 {
        return Err(CommandError {
            message: Some("not signed in — run `eggpass login`".to_string()),
            code: 2,
        });
    }

    // A partial failure is reported in the output, not in the exit code. Only
    // a run in which every provider failed operationally is a failure.
    if entries
        .iter()
        .all(|entry| matches!(entry.outcome, ProviderUsage::Failed { .. }))
    {
        return Err(CommandError::silent(1));
    }

    Ok(())
}

/// One provider's answer, with the provider it belongs to.
#[derive(Serialize)]
struct ProviderEntry {
    /// The provider.
    provider: ProviderId,
    /// What it had to say.
    #[serde(flatten)]
    outcome: ProviderUsage,
    /// Which pool the numbers belong to, when the provider reported any.
    caption: Option<String>,
}

/// The machine-readable form of the report.
#[derive(Serialize)]
struct UsageReportJson<'a> {
    /// One entry per provider.
    providers: &'a [ProviderEntry],
}

/// The note that says which pool a provider's numbers belong to.
///
/// Only x.ai has one: the billing endpoint reports the consumer
/// subscription, and on the `api` upstream that is not what `eggpass` spends.
fn caption_for(provider: ProviderId, config: &crate::config::Config) -> Option<String> {
    match provider {
        ProviderId::Xai => {
            Some(crate::provider::usage::xai::caption(config.providers.xai.upstream).to_string())
        }
        ProviderId::OpenAi => None,
    }
}

/// Ask one provider what it can say.
///
/// A provider that is not signed in is answered without a network call.
fn collect_one(context: &Context, provider: ProviderId) -> ProviderUsage {
    let runtime = ProviderRuntime::new(provider, &context.config, &context.paths);

    match runtime.tokens.current() {
        Ok(None) => ProviderUsage::NotSignedIn,
        Ok(Some(_)) => runtime.usage(),
        Err(err) => ProviderUsage::Failed {
            reason: format!("could not read the stored credentials ({err})"),
        },
    }
}

/// Render the whole report.
fn render(entries: &[ProviderEntry]) -> String {
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        render_block(&mut out, entry);
    }
    out
}

/// Render one provider's block.
fn render_block(out: &mut String, entry: &ProviderEntry) {
    match &entry.outcome {
        ProviderUsage::Reported { report } => {
            render_reported(out, entry.provider, report, entry.caption.as_deref())
        }
        ProviderUsage::NotSignedIn => {
            out.push_str(entry.provider.display_name());
            out.push('\n');
            out.push_str(&field(
                "Usage",
                &format!(
                    "unavailable — not signed in; run `eggpass login --provider {}`",
                    entry.provider.as_str()
                ),
            ));
            out.push('\n');
        }
        ProviderUsage::Unavailable {
            reason,
            hint,
            signals,
        } => {
            out.push_str(entry.provider.display_name());
            out.push('\n');
            out.push_str(&field("Usage", &format!("unavailable — {reason}")));
            out.push('\n');
            if let Some(hint) = hint {
                out.push_str(&field("Limits", hint));
                out.push('\n');
            }
            if !signals.is_empty() {
                out.push_str(&field("Signals", signals[0]));
                out.push('\n');
                for signal in &signals[1..] {
                    out.push_str(&continuation(signal));
                    out.push('\n');
                }
            }
        }
        ProviderUsage::Failed { reason } => {
            out.push_str(entry.provider.display_name());
            out.push('\n');
            out.push_str(&field("Usage", &format!("unavailable — {reason}")));
            out.push('\n');
        }
    }
}

/// Render a provider that reported usage.
fn render_reported(
    out: &mut String,
    provider: ProviderId,
    report: &UsageReport,
    caption: Option<&str>,
) {
    // The heading names the plan when the provider named one.
    match &report.plan {
        Some(plan) => {
            out.push_str(provider.display_name());
            out.push_str(" · ");
            out.push_str(plan);
        }
        None => out.push_str(provider.display_name()),
    }
    out.push('\n');

    let now = timeutil::now_unix();

    for window in &report.windows {
        // An explicit state is the provider's answer, and it replaces the
        // percentage entirely.
        if let Some(state) = &report.state {
            out.push_str(&field(&window.label.display(), state.display()));
            out.push('\n');
        } else {
            match window.used_percent {
                Some(percent) => {
                    out.push_str(&field(
                        &window.label.display(),
                        &format!("{}  {} used", bar(percent), percent_text(percent)),
                    ));
                }
                // No bar at all: a bar with a question mark in it would be a
                // number we do not have.
                None => out.push_str(&field(&window.label.display(), "unavailable")),
            }
            out.push('\n');
        }

        // A window with no reset time gets no reset line. `Resets
        // unavailable` would be noise.
        if let Some(resets_at) = window.resets_at {
            // The offset is the one in force at the reset instant, so a
            // reset on the far side of a daylight-saving change is right.
            let offset = timeutil::local_offset_seconds(resets_at).unwrap_or(0);
            out.push_str(&field("Resets", &reset_text(resets_at, now, offset)));
            out.push('\n');
        }
    }

    // An explicit state is the provider's answer, and it replaces every
    // percentage in the block: a breakdown of products with percentages
    // beside it would say something the state has already answered.
    if report.state.is_none() {
        for product in &report.breakdown {
            match product.used_percent {
                Some(percent) => {
                    out.push_str(&format!(
                        "  {:<width$}{}  {} used",
                        product.product,
                        bar(percent),
                        percent_text(percent),
                        width = PRODUCT_WIDTH
                    ));
                }
                None => {
                    out.push_str(&format!(
                        "  {:<width$}unavailable",
                        product.product,
                        width = PRODUCT_WIDTH
                    ));
                }
            }
            out.push('\n');
        }
    }

    if let Some(credits) = &report.credits {
        if let Some(prepaid) = credits.prepaid_cents {
            out.push_str(&field("Prepaid", &money(prepaid)));
            out.push('\n');
        }
        if let Some(used) = credits.on_demand_used_cents {
            let text = match credits.on_demand_cap_cents {
                Some(cap) => format!("{} of {}", money(used), money(cap)),
                None => format!("{} used", money(used)),
            };
            out.push_str(&field("On demand", &text));
            out.push('\n');
        } else if let Some(cap) = credits.on_demand_cap_cents {
            out.push_str(&field("On demand", &format!("cap {}", money(cap))));
            out.push('\n');
        }
    }

    // The caption says which pool the number belongs to. It is not optional:
    // on the `api` upstream the number is not what `eggpass` spends.
    if let Some(caption) = caption {
        out.push_str(&format!("  {caption}\n"));
    }
}

/// A labelled line.
fn field(label: &str, value: &str) -> String {
    format!("  {label:<width$}{value}", width = LABEL_WIDTH)
}

/// A continuation line, under the label column.
fn continuation(value: &str) -> String {
    format!("  {:<width$}{value}", "", width = LABEL_WIDTH)
}

/// The bar for a percentage.
///
/// Twenty cells, fixed. A non-zero percentage never renders as an empty bar:
/// at 1 % a bar of all `░` would read as "none used", and the number beside
/// it would contradict that.
///
/// The bar is drawn from the percentage as it is *printed*, so the two agree.
/// A percentage that rounds to zero gets an empty bar and reads `0% used`;
/// one that rounds to something above zero gets at least one cell.
fn bar(percent: f64) -> String {
    let rounded = round_percent(percent);
    let mut filled = (rounded / 100.0 * BAR_CELLS as f64).round() as i64;
    filled = filled.clamp(0, BAR_CELLS as i64);
    if rounded > 0.0 && filled == 0 {
        filled = 1;
    }

    let mut out = String::new();
    for _ in 0..filled {
        out.push(FILLED);
    }
    for _ in filled..BAR_CELLS as i64 {
        out.push(EMPTY);
    }
    out
}

/// The percentage rounded to the one decimal it is printed with.
fn round_percent(percent: f64) -> f64 {
    (percent * 10.0).round() / 10.0
}

/// The percentage as it is printed: at most one decimal, and no trailing
/// `.0`.
fn percent_text(percent: f64) -> String {
    let rounded = round_percent(percent);
    if (rounded - rounded.round()).abs() < f64::EPSILON {
        format!("{}%", rounded.round() as i64)
    } else {
        format!("{rounded:.1}%")
    }
}

/// The reset time, in the local timezone, with its offset.
///
/// The offset is printed because `Nov 18, 09:00` alone is ambiguous: a user
/// in `UTC+2` and a colleague in `UTC-5` read the same string and mean
/// different moments.
///
/// The offset is a parameter so that the rendering can be checked against a
/// fixed zone rather than against whatever the machine happens to be in.
fn reset_text(resets_at: i64, now: i64, offset: i64) -> String {
    let mut text = format!(
        "{} ({})",
        timeutil::format_local_datetime(resets_at, now, offset),
        timeutil::format_utc_offset(offset)
    );

    // A window may roll between the read and the render, so a reset that is a
    // few seconds in the past is normal. An hour is not.
    if resets_at < now - STALE_AFTER {
        text.push_str(" (stale)");
    }

    text
}

/// An amount in cents, as dollars.
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let total = cents.abs();
    format!("{sign}${}.{:02}", total / 100, total % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::usage::{
        Credits, ProductUsage, QuotaState, QuotaWindow, UsageReport, WindowLabel,
    };

    /// A provider that reported usage.
    fn reported(percent: Option<f64>) -> ProviderUsage {
        ProviderUsage::Reported {
            report: UsageReport {
                provider: ProviderId::Xai,
                plan: None,
                windows: vec![QuotaWindow {
                    label: WindowLabel::Weekly,
                    used_percent: percent,
                    resets_at: None,
                    duration_secs: None,
                }],
                breakdown: Vec::new(),
                credits: None,
                state: None,
            },
        }
    }

    /// A provider that reported usage, with the report opened for editing.
    fn reported_report(percent: Option<f64>) -> UsageReport {
        match reported(percent) {
            ProviderUsage::Reported { report } => report,
            other => panic!("expected a report, got {other:?}"),
        }
    }

    fn entry(outcome: ProviderUsage) -> ProviderEntry {
        ProviderEntry {
            provider: ProviderId::Xai,
            outcome,
            caption: None,
        }
    }

    #[test]
    fn the_bar_is_twenty_cells() {
        let bar = bar(40.0);
        assert_eq!(bar.chars().count(), 20);
        assert_eq!(bar.chars().filter(|c| *c == FILLED).count(), 8);
        assert_eq!(bar.chars().filter(|c| *c == EMPTY).count(), 12);
    }

    #[test]
    fn a_tiny_percentage_still_shows_a_cell() {
        let bar = bar(1.0);
        assert_eq!(bar.chars().count(), 20);
        assert_eq!(bar.chars().filter(|c| *c == FILLED).count(), 1);
    }

    #[test]
    fn zero_shows_an_empty_bar() {
        let bar = bar(0.0);
        assert_eq!(bar.chars().count(), 20);
        assert_eq!(bar.chars().filter(|c| *c == FILLED).count(), 0);
    }

    #[test]
    fn a_full_bar_is_twenty_cells() {
        let bar = bar(100.0);
        assert_eq!(bar.chars().count(), 20);
        assert_eq!(bar.chars().filter(|c| *c == FILLED).count(), 20);
    }

    #[test]
    fn the_percentage_text_drops_a_trailing_zero() {
        assert_eq!(percent_text(1.0), "1%");
        assert_eq!(percent_text(12.5), "12.5%");
        assert_eq!(percent_text(40.0), "40%");
    }

    #[test]
    fn an_unavailable_window_has_no_bar() {
        let text = render(&[entry(reported(None))]);
        assert!(text.contains("Weekly      unavailable"), "{text}");
        assert!(!text.contains(FILLED), "{text}");
        assert!(!text.contains(EMPTY), "{text}");
    }

    #[test]
    fn a_state_replaces_the_percentage() {
        let mut report = reported_report(Some(40.0));
        report.state = Some(QuotaState::NotEligible);
        let text = render(&[entry(ProviderUsage::Reported { report })]);
        assert!(text.contains("not eligible for this plan"), "{text}");
        assert!(!text.contains("40%"), "{text}");
    }

    #[test]
    fn a_state_replaces_the_breakdown_too() {
        // The state is the answer. A breakdown of products with percentages
        // beside it would say something the state has already answered.
        let mut report = reported_report(Some(40.0));
        report.state = Some(QuotaState::NotEligible);
        report.breakdown = vec![ProductUsage {
            product: "GrokBuild".to_string(),
            used_percent: Some(50.0),
        }];

        let text = render(&[entry(ProviderUsage::Reported { report })]);
        assert!(text.contains("not eligible for this plan"), "{text}");
        assert!(!text.contains("50%"), "{text}");
        assert!(!text.contains("GrokBuild"), "{text}");
    }

    #[test]
    fn the_reset_time_carries_the_offset() {
        let now = timeutil::parse_rfc3339_to_unix("2026-11-14T12:00:00Z").unwrap();
        let resets_at = timeutil::parse_rfc3339_to_unix("2026-11-18T08:00:00Z").unwrap();

        // One hour ahead of UTC: 08:00 UTC is 09:00 there.
        let text = reset_text(resets_at, now, 3_600);
        assert_eq!(text, "Nov 18, 09:00 (UTC+01:00)");

        // And five hours behind it.
        let text = reset_text(resets_at, now, -5 * 3_600);
        assert_eq!(text, "Nov 18, 03:00 (UTC-05:00)");
    }

    #[test]
    fn a_stale_reset_is_marked() {
        let now = timeutil::parse_rfc3339_to_unix("2026-11-14T12:00:00Z").unwrap();

        let old = now - 7_200;
        assert!(reset_text(old, now, 0).contains("(stale)"));

        let recent = now - 30;
        assert!(!reset_text(recent, now, 0).contains("(stale)"));
    }

    #[test]
    fn the_year_appears_only_when_it_differs() {
        let now = timeutil::parse_rfc3339_to_unix("2026-11-14T12:00:00Z").unwrap();

        let same_year = timeutil::parse_rfc3339_to_unix("2026-12-30T09:00:00Z").unwrap();
        assert_eq!(reset_text(same_year, now, 0), "Dec 30, 09:00 (UTC)");

        let next_year = timeutil::parse_rfc3339_to_unix("2027-01-06T09:00:00Z").unwrap();
        assert_eq!(reset_text(next_year, now, 0), "Jan 06, 2027 09:00 (UTC)");
    }

    #[test]
    fn a_tiny_percentage_does_not_contradict_its_bar() {
        // A percentage that rounds to zero reads `0% used`, so its bar is
        // empty. A filled cell beside `0% used` would say otherwise.
        assert_eq!(percent_text(0.04), "0%");
        assert_eq!(bar(0.04).chars().filter(|c| *c == FILLED).count(), 0);
        assert_eq!(bar(0.04).chars().count(), 20);

        // A percentage that rounds to something above zero gets a cell.
        assert_eq!(percent_text(0.1), "0.1%");
        assert_eq!(bar(0.1).chars().filter(|c| *c == FILLED).count(), 1);
        assert_eq!(bar(0.1).chars().count(), 20);
    }

    #[test]
    fn the_chatgpt_block_names_the_error_codes() {
        let entry = ProviderEntry {
            provider: ProviderId::OpenAi,
            caption: None,
            outcome: ProviderUsage::Unavailable {
                reason: "Sign in with ChatGPT does not expose a quota API".to_string(),
                hint: Some("https://chatgpt.com/settings/usage".to_string()),
                signals: vec![
                    "429 subscription_sharing_usage_limit_exceeded",
                    "503 subscription_sharing_usage_unavailable",
                    "403 subscription_sharing_user_not_eligible",
                ],
            },
        };

        let text = render(&[entry]);
        assert!(text.contains("ChatGPT"), "{text}");
        assert!(
            text.contains("subscription_sharing_usage_limit_exceeded"),
            "{text}"
        );
        assert!(
            text.contains("subscription_sharing_usage_unavailable"),
            "{text}"
        );
        assert!(
            text.contains("subscription_sharing_user_not_eligible"),
            "{text}"
        );
        assert!(
            text.contains("https://chatgpt.com/settings/usage"),
            "{text}"
        );
        assert!(!text.contains('%'), "{text}");
    }

    #[test]
    fn the_caption_changes_with_the_upstream() {
        let mut api = crate::config::Config::default();
        api.providers.xai.upstream = crate::config::Upstream::Api;
        let mut reported_entry = entry(reported(Some(40.0)));
        reported_entry.caption = caption_for(ProviderId::Xai, &api);
        let text = render(&[reported_entry]);
        assert!(
            text.contains("billed separately against the xAI API tier"),
            "{text}"
        );

        let mut build = crate::config::Config::default();
        build.providers.xai.upstream = crate::config::Upstream::Build;
        let mut reported_entry = entry(reported(Some(40.0)));
        reported_entry.caption = caption_for(ProviderId::Xai, &build);
        let text = render(&[reported_entry]);
        assert!(text.contains("draws from"), "{text}");
    }

    #[test]
    fn the_json_report_carries_the_caption() {
        // A machine-readable number without the caption is as misleading as
        // one on the terminal without it.
        let mut reported_entry = entry(reported(Some(40.0)));
        reported_entry.caption = caption_for(ProviderId::Xai, &crate::config::Config::default());
        let entries = vec![reported_entry];

        let report = UsageReportJson {
            providers: &entries,
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(
            text.contains("billed separately against the xAI API tier"),
            "{text}"
        );
    }

    #[test]
    fn the_percentage_reads_as_used() {
        // The number is how much is *used*, never how much is left.
        let text = render(&[entry(reported(Some(40.0)))]);
        assert!(text.contains("40% used"), "{text}");
        assert!(!text.contains("40% left"), "{text}");
    }

    #[test]
    fn the_products_are_shown_under_the_window() {
        let mut report = reported_report(Some(40.0));
        report.breakdown = vec![
            ProductUsage {
                product: "GrokBuild".to_string(),
                used_percent: Some(50.0),
            },
            ProductUsage {
                product: "GrokChat".to_string(),
                used_percent: Some(20.0),
            },
        ];

        let text = render(&[entry(ProviderUsage::Reported { report })]);
        assert!(text.contains("GrokBuild"), "{text}");
        assert!(text.contains("50% used"), "{text}");
        assert!(text.contains("GrokChat"), "{text}");
        assert!(text.contains("20% used"), "{text}");
    }

    #[test]
    fn the_plan_name_joins_the_heading() {
        let mut report = reported_report(Some(40.0));
        report.plan = Some("SuperGrok".to_string());
        let text = render(&[entry(ProviderUsage::Reported { report })]);
        assert!(text.starts_with("x.ai · SuperGrok"), "{text}");
    }

    #[test]
    fn the_prepaid_balance_is_shown_in_dollars() {
        let mut report = reported_report(Some(40.0));
        report.credits = Some(Credits {
            prepaid_cents: Some(0),
            on_demand_used_cents: None,
            on_demand_cap_cents: None,
        });
        let text = render(&[entry(ProviderUsage::Reported { report })]);
        assert!(text.contains("Prepaid     $0.00"), "{text}");
    }

    #[test]
    fn the_labels_line_up() {
        let mut report = reported_report(Some(40.0));
        report.windows[0].resets_at = Some(timeutil::now_unix() + 3_600);
        report.credits = Some(Credits {
            prepaid_cents: Some(100),
            on_demand_used_cents: None,
            on_demand_cap_cents: None,
        });

        let text = render(&[entry(ProviderUsage::Reported { report })]);

        // Every labelled line puts its value in the same column: two spaces of
        // indent, then the label padded to the label width.
        for label in ["Weekly", "Resets", "Prepaid"] {
            let line = text
                .lines()
                .find(|line| line.starts_with(&format!("  {label}")))
                .unwrap_or_else(|| panic!("no line for {label} in {text}"));
            let value = &line[2 + LABEL_WIDTH..];
            assert!(!value.starts_with(' '), "{value:?} in {line}");
        }
    }

    #[test]
    fn the_json_report_names_the_provider_and_the_status() {
        let entries = vec![ProviderEntry {
            provider: ProviderId::OpenAi,
            caption: None,
            outcome: ProviderUsage::NotSignedIn,
        }];

        let report = UsageReportJson {
            providers: &entries,
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.contains("\"provider\":\"openai\""), "{text}");
        assert!(text.contains("\"status\":\"not_signed_in\""), "{text}");
    }
}
