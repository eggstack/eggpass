//! `eggpass usage`.
//!
//! The command first reads the running daemon's in-memory report using its
//! private usage-only credential. When the daemon is unavailable or uses an
//! older API, it falls back to direct provider collection.
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

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::cli::{CommandError, Context};
use crate::daemon::{self, DaemonState};
#[cfg(test)]
use crate::provider::usage::report::codex_outcome;
use crate::provider::usage::report::{ProviderEntry, UsageReportJson, enabled_provider_ids};
use crate::provider::usage::{ProviderUsage, UsageReport, WindowLabel};
use crate::provider::{ProviderId, ProviderRuntime};
use crate::timeutil;
use crate::usage_key::UsageKey;
use serde::Deserialize;

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

/// Shows progress while provider usage requests are in flight.
///
/// The spinner is deliberately sent to an interactive stderr only, so piping
/// either output format remains clean. Its worker erases only its own line
/// before returning control to the report output.
struct UsageSpinner {
    running: Option<Arc<AtomicBool>>,
    worker: Option<JoinHandle<()>>,
}

impl UsageSpinner {
    fn start(message: &'static str) -> Self {
        if !io::stderr().is_terminal() {
            return Self {
                running: None,
                worker: None,
            };
        }

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("eggpass-usage-spinner".to_string())
            .spawn(move || {
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

                // Avoid flashing the spinner when a request finishes quickly.
                thread::sleep(Duration::from_millis(120));

                let mut stderr = io::stderr().lock();
                let mut frame = 0;
                let mut displayed = false;
                while thread_running.load(Ordering::Acquire) {
                    if write!(stderr, "\r\x1b[2K{} {message}", FRAMES[frame]).is_err()
                        || stderr.flush().is_err()
                    {
                        break;
                    }
                    displayed = true;
                    frame = (frame + 1) % FRAMES.len();
                    thread::sleep(Duration::from_millis(80));
                }

                if displayed {
                    let _ = write!(stderr, "\r\x1b[2K");
                    let _ = stderr.flush();
                }
            })
            .ok();

        Self {
            running: Some(running),
            worker,
        }
    }
}

impl Drop for UsageSpinner {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            running.store(false, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Show how much of the subscription allowance remains.
pub fn usage(
    context: &Context,
    provider: Option<ProviderId>,
    json: bool,
) -> Result<(), CommandError> {
    let providers = selected_provider_ids(provider, &context.config)?;

    let spinner = UsageSpinner::start("Fetching usage metrics");

    let cached = try_cached_usage(context, &providers);
    let (entries, stale) = report_with_fallback(
        &providers,
        cached,
        || last_inferences(context),
        |provider| {
            let (outcome, source) = collect_one(context, provider);
            ProviderEntry {
                provider,
                outcome,
                // A source identifies which account supplied the snapshot; the caption names the quota pool.
                source,
                caption: caption_for(provider, &context.config),
                last_inference: None,
            }
        },
    )?;

    drop(spinner);
    if stale {
        let _ = write_stale_cache_notice(&mut io::stderr().lock(), true);
    }
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

    usage_exit_result(&entries)
}

/// Resolve the CLI's explicit provider choice against the enabled-provider policy.
fn selected_provider_ids(
    requested: Option<ProviderId>,
    config: &crate::config::Config,
) -> Result<Vec<ProviderId>, CommandError> {
    match requested {
        Some(provider) if config.providers.get(provider).enabled => Ok(vec![provider]),
        Some(provider) => Err(CommandError::new(format!(
            "provider {} is disabled in configuration",
            provider.as_str()
        ))),
        None => {
            let enabled = enabled_provider_ids(config);
            if enabled.is_empty() {
                Err(CommandError::new(
                    "no providers are enabled in configuration",
                ))
            } else {
                Ok(enabled)
            }
        }
    }
}

#[derive(Deserialize)]
struct UsageReportResponse {
    providers: Vec<ProviderEntry>,
}

#[derive(Debug)]
struct CachedUsageReport {
    entries: Vec<ProviderEntry>,
    stale: bool,
}

enum DaemonUsageResponse {
    Cached(CachedUsageReport),
    NotFound,
    Status {
        status: u16,
        retry_after: Option<u64>,
    },
}

enum DaemonFetchError {
    Transport,
    InvalidResponse,
}

const MAX_DAEMON_USAGE_BODY: u64 = 1024 * 1024;

/// Try the daemon's report before collecting directly from providers.
fn try_cached_usage(
    context: &Context,
    providers: &[ProviderId],
) -> Result<Option<CachedUsageReport>, CommandError> {
    let state = daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );
    try_cached_usage_with(
        state,
        &context.paths.usage_key_file(),
        providers,
        fetch_daemon_usage,
        || {
            daemon::state(
                &context.paths.pid_file(),
                &context.config.host,
                context.config.port,
            )
        },
    )
}

/// State machine kept injectable so all fallback branches can be tested locally.
fn try_cached_usage_with<F, S>(
    initial_state: DaemonState,
    key_path: &std::path::Path,
    providers: &[ProviderId],
    mut fetch: F,
    mut recheck_state: S,
) -> Result<Option<CachedUsageReport>, CommandError>
where
    F: FnMut(&str, u16, &str, &[ProviderId]) -> Result<DaemonUsageResponse, DaemonFetchError>,
    S: FnMut() -> DaemonState,
{
    let DaemonState::Running { host, port, .. } = initial_state else {
        return Ok(None);
    };
    let Ok(secret) = UsageKey::read(key_path) else {
        // A missing or unreadable runtime secret cannot authorize the daemon
        // request. Preserve the direct collector as the safe compatibility path.
        return Ok(None);
    };

    match fetch(daemon::probe_host(&host), port, &secret, providers) {
        Ok(DaemonUsageResponse::Cached(report)) => Ok(Some(report)),
        Ok(DaemonUsageResponse::NotFound) => Ok(None),
        Ok(DaemonUsageResponse::Status {
            status,
            retry_after,
        }) => Err(daemon_status_error(status, retry_after)),
        Err(DaemonFetchError::InvalidResponse) => Err(CommandError::new(
            "the daemon returned an invalid usage report; direct provider checks were not run",
        )),
        Err(DaemonFetchError::Transport) => match recheck_state() {
            DaemonState::Stopped | DaemonState::Stale { .. } => Ok(None),
            DaemonState::Running { .. } => Err(CommandError::new(
                "the daemon still appears to be running but its usage endpoint could not be reached; check `eggpass status` or restart the daemon",
            )),
        },
    }
}

fn daemon_status_error(status: u16, retry_after: Option<u64>) -> CommandError {
    let retry = retry_after.map_or_else(String::new, |seconds| {
        format!("; retry after at least {seconds} second(s)")
    });
    if matches!(status, 202 | 503 | 504) {
        return CommandError::new(format!(
            "the daemon has no completed usage report available (HTTP {status}){retry}; direct provider checks were not run"
        ));
    }
    if matches!(status, 401 | 403) {
        return CommandError::new(format!(
            "the daemon rejected the usage-only credential (HTTP {status}); restart the daemon and retry `eggpass usage`"
        ));
    }
    CommandError::new(format!(
        "the daemon usage request failed with HTTP {status}{retry}; direct provider checks were not run"
    ))
}

fn fetch_daemon_usage(
    host: &str,
    port: u16,
    secret: &str,
    providers: &[ProviderId],
) -> Result<DaemonUsageResponse, DaemonFetchError> {
    fetch_daemon_usage_with_timeouts(
        host,
        port,
        secret,
        providers,
        Duration::from_secs(2),
        Duration::from_secs(35),
    )
}

/// Fetch with injectable deadlines so the response timeout is testable locally.
fn fetch_daemon_usage_with_timeouts(
    host: &str,
    port: u16,
    secret: &str,
    providers: &[ProviderId],
    connect_timeout: Duration,
    total_timeout: Duration,
) -> Result<DaemonUsageResponse, DaemonFetchError> {
    let url = format!("http://{host}:{port}/v1/usage");
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .timeout_connect(Some(connect_timeout))
        // The production total deadline includes the server's cold-cache wait.
        .timeout_global(Some(total_timeout))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let authorization = format!("Bearer {secret}");
    let response = agent
        .get(&url)
        .header("Authorization", &authorization)
        .call()
        .map_err(|_| DaemonFetchError::Transport)?;
    let status = response.status().as_u16();
    if status == 404 {
        return Ok(DaemonUsageResponse::NotFound);
    }
    let retry_after = response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    if status != 200 {
        return Ok(DaemonUsageResponse::Status {
            status,
            retry_after,
        });
    }
    let stale = response
        .headers()
        .get("X-Eggpass-Usage-Stale")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let body = response
        .into_body()
        .with_config()
        .limit(MAX_DAEMON_USAGE_BODY + 1)
        .read_to_vec()
        .map_err(|_| DaemonFetchError::Transport)?;
    if body.len() as u64 > MAX_DAEMON_USAGE_BODY {
        return Err(DaemonFetchError::InvalidResponse);
    }
    let response: UsageReportResponse =
        serde_json::from_slice(&body).map_err(|_| DaemonFetchError::InvalidResponse)?;
    let entries = select_cached_entries(response.providers, providers)
        .map_err(|_| DaemonFetchError::InvalidResponse)?;
    Ok(DaemonUsageResponse::Cached(CachedUsageReport {
        entries,
        stale,
    }))
}

fn select_cached_entries(
    entries: Vec<ProviderEntry>,
    selected: &[ProviderId],
) -> Result<Vec<ProviderEntry>, ()> {
    let mut by_provider = BTreeMap::new();
    for entry in entries {
        if let ProviderUsage::Reported { report } = &entry.outcome
            && report.provider != entry.provider
        {
            return Err(());
        }
        if by_provider.insert(entry.provider, entry).is_some() {
            return Err(());
        }
    }
    selected
        .iter()
        .map(|provider| by_provider.remove(provider).ok_or(()))
        .collect()
}

fn report_with_fallback<C>(
    providers: &[ProviderId],
    cached: Result<Option<CachedUsageReport>, CommandError>,
    direct_inferences: impl FnOnce() -> BTreeMap<ProviderId, u16>,
    mut direct_collect: C,
) -> Result<(Vec<ProviderEntry>, bool), CommandError>
where
    C: FnMut(ProviderId) -> ProviderEntry,
{
    if let Some(report) = cached? {
        return Ok((report.entries, report.stale));
    }
    let inferences = direct_inferences();
    let entries = providers
        .iter()
        .copied()
        .map(|provider| {
            let mut entry = direct_collect(provider);
            entry.last_inference = inferences.get(&provider).copied();
            entry
        })
        .collect();
    Ok((entries, false))
}

fn write_stale_cache_notice(writer: &mut impl Write, stale: bool) -> io::Result<()> {
    if stale {
        writeln!(
            writer,
            "warning: daemon usage cache is stale; showing its last successful report"
        )?;
    }
    Ok(())
}
/// Decide the command status after output has been rendered.
fn usage_exit_result(entries: &[ProviderEntry]) -> Result<(), CommandError> {
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

/// Ask one enabled provider for its usage. Codex is optional and queried only
/// for OpenAI, before Eggpass credentials are inspected.
fn collect_one(context: &Context, provider: ProviderId) -> (ProviderUsage, Option<String>) {
    let codex = (provider == ProviderId::OpenAi)
        .then(crate::provider::usage::codex_app_server::collect_detailed);
    collect_one_with_codex_collection(context, provider, codex)
}

/// Compatibility wrapper used by the existing CLI collector tests.
#[cfg(test)]
fn collect_one_with_codex_result(
    context: &Context,
    provider: ProviderId,
    codex_report: Option<UsageReport>,
) -> (ProviderUsage, Option<String>) {
    let codex = (provider == ProviderId::OpenAi).then_some(
        crate::provider::usage::codex_app_server::CodexCollection {
            report: codex_report,
            timed_out: false,
        },
    );
    collect_one_with_codex_collection(context, provider, codex)
}

fn collect_one_with_codex_collection(
    context: &Context,
    provider: ProviderId,
    codex: Option<crate::provider::usage::codex_app_server::CodexCollection>,
) -> (ProviderUsage, Option<String>) {
    let runtime = ProviderRuntime::new(provider, &context.config, &context.paths);
    let collected = crate::provider::usage::report::collect_entry(
        provider,
        context.config.providers.xai.upstream,
        &runtime.tokens,
        context.config.timeouts,
        codex,
        crate::provider::usage::xai::BILLING_BASE_URL,
    );
    (collected.entry.outcome, collected.entry.source)
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
        ProviderUsage::Reported { report } => render_reported(
            out,
            entry.provider,
            report,
            entry.caption.as_deref(),
            entry.source.as_deref(),
        ),
        ProviderUsage::NotSignedIn => {
            out.push_str(entry.provider.display_name());
            out.push('\n');
            out.push_str(&field(
                "Quota",
                &format!(
                    "unavailable — not signed in; run `eggpass login --provider {}`",
                    entry.provider.as_str()
                ),
            ));
            out.push('\n');
        }
        ProviderUsage::Unavailable {
            reason,
            source,
            hint,
            signals,
        } => {
            out.push_str(entry.provider.display_name());
            out.push('\n');
            out.push_str(&field("Quota", &format!("unavailable — {reason}")));
            out.push('\n');
            // The quota cannot be read, so the last inference is the only
            // evidence there is about the account.
            render_inference(out, entry);
            if let Some(source) = source {
                out.push_str(&field("Source", source));
                out.push('\n');
            }
            if let Some(hint) = hint {
                out.push_str(&field("Limits", hint));
                out.push('\n');
            }
            if !signals.is_empty() {
                out.push_str(&field("Signals", &signals[0]));
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
            out.push_str(&field("Quota", &format!("unavailable — {reason}")));
            out.push('\n');
            render_inference(out, entry);
            // The caption belongs here too: the inference that succeeded may
            // be metered against another pool, and this is the block that
            // says so.
            render_caption(out, entry.caption.as_deref());
        }
    }
}

/// Render the caption, if there is one.
///
/// It is set in a fixed column rather than the label column: it is a note
/// about the block, not a field of it.
fn render_caption(out: &mut String, caption: Option<&str>) {
    let Some(caption) = caption else {
        return;
    };
    out.push_str(&format!("  {caption}\n"));
}

/// Render the account/source note when the reported snapshot came from Codex.
fn render_source(out: &mut String, source: Option<&str>) {
    let Some(source) = source else {
        return;
    };
    out.push_str(&field("Source", source));
    out.push('\n');
}

/// Render the row that says what the last inference was answered with.
///
/// It is printed when the quota could not be read, and only then: a block
/// that already carries a percentage has said everything about the account
/// that it can.
fn render_inference(out: &mut String, entry: &ProviderEntry) {
    let Some(status) = entry.last_inference else {
        return;
    };
    out.push_str(&field("Inference", &crate::inference::outcome_text(status)));
    out.push('\n');
}

/// Ask the running daemon what the last inference to each provider was
/// answered with.
///
/// The daemon keeps it in memory and writes nothing, so there is nothing to
/// read when it is not running: the row is then simply not printed. The
/// request is best-effort — a daemon that cannot be reached must not break
/// the report.
fn last_inferences(context: &Context) -> BTreeMap<ProviderId, u16> {
    let state = daemon::state(
        &context.paths.pid_file(),
        &context.config.host,
        context.config.port,
    );
    let DaemonState::Running { host, port, .. } = state else {
        return BTreeMap::new();
    };
    fetch_last_inferences(daemon::probe_host(&host), port)
}

/// Ask a daemon what the last inference to each provider was answered with.
fn fetch_last_inferences(host: &str, port: u16) -> BTreeMap<ProviderId, u16> {
    let url = format!("http://{host}:{port}/v1/last-inference");
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_global(Some(Duration::from_secs(3)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let Ok(response) = agent.get(&url).call() else {
        return BTreeMap::new();
    };
    if response.status().as_u16() != 200 {
        return BTreeMap::new();
    }
    let Ok(text) = response.into_body().read_to_string() else {
        return BTreeMap::new();
    };
    parse_last_inferences(&text)
}

/// Read the daemon's answer.
fn parse_last_inferences(text: &str) -> BTreeMap<ProviderId, u16> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return BTreeMap::new();
    };
    let Some(providers) = value.get("providers").and_then(|value| value.as_object()) else {
        return BTreeMap::new();
    };

    providers
        .iter()
        .filter_map(|(name, status)| {
            let provider = ProviderId::parse(name)?;
            let status = u16::try_from(status.as_u64()?).ok()?;
            Some((provider, status))
        })
        .collect()
}

/// Render a provider that reported usage.
fn render_reported(
    out: &mut String,
    provider: ProviderId,
    report: &UsageReport,
    caption: Option<&str>,
    source: Option<&str>,
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
    let buckets: BTreeSet<&str> = report
        .windows
        .iter()
        .filter_map(|window| bucket_id(&window.label))
        .collect();
    let multiple_buckets = buckets.len() > 1;

    for window in &report.windows {
        // Use the provider's window duration when it is known. This turns
        // Codex's generic Primary/Secondary buckets into useful labels such as
        // 5-hour and 7-day, while preserving named windows like Grok's Weekly
        // when no duration is supplied. Keep bucket names when they are needed
        // to distinguish multiple reported buckets.
        let label = match (
            window.duration_secs.filter(|duration| *duration > 0),
            multiple_buckets.then(|| bucket_id(&window.label)).flatten(),
        ) {
            (Some(duration), Some(bucket)) => {
                format!("{bucket} {}", duration_label(duration))
            }
            (Some(duration), None) => duration_label(duration),
            (None, _) => window.label.display(),
        };

        // An explicit state is the provider's answer, and it replaces the
        // percentage entirely.
        if let Some(state) = &report.state {
            out.push_str(&field(&label, state.display()));
            out.push('\n');
        } else {
            match window.used_percent {
                Some(percent) => {
                    let left = left_percent(percent);
                    out.push_str(&field(
                        &label,
                        &format!("{}  {} left", bar(left), percent_text(left)),
                    ));
                }
                // No bar at all: a bar with a question mark in it would be a
                // number we do not have.
                None => out.push_str(&field(&label, "unavailable")),
            }
            out.push('\n');
        }

        // A window with no reset time gets no reset line. `Resets
        // unavailable` would be noise.
        if let Some(resets_at) = window.resets_at {
            // The offset is the one in force at the reset instant, so a
            // reset on the far side of a daylight-saving change is right.
            let offset = timeutil::local_offset_seconds(resets_at).unwrap_or(0);
            let mut reset = reset_text(resets_at, now, offset);
            if let Some(remaining) = reset_countdown(resets_at, now) {
                reset.push_str(" · in ");
                reset.push_str(&remaining);
            }
            out.push_str(&field("Resets", &reset));
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
                    let left = left_percent(percent);
                    out.push_str(&format!(
                        "  {:<width$}{}  {} left",
                        product.product,
                        bar(left),
                        percent_text(left),
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
    render_caption(out, caption);
    render_source(out, source);
}

/// A labelled line.
fn field(label: &str, value: &str) -> String {
    let separator = if label.chars().count() >= LABEL_WIDTH {
        " "
    } else {
        ""
    };
    format!("  {label:<width$}{separator}{value}", width = LABEL_WIDTH)
}

/// A continuation line, under the label column.
fn continuation(value: &str) -> String {
    format!("  {:<width$}{value}", "", width = LABEL_WIDTH)
}

/// The bar for the percentage of allowance left.
///
/// Twenty cells, fixed. A non-zero percentage never renders as an empty bar:
/// at 1 % left, the bar must show at least one filled cell.
///
/// The bar is drawn from the percentage as it is *printed*, so the two agree.
/// A percentage that rounds to zero gets an empty bar; one that rounds to
/// something above zero gets at least one cell.
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

/// Convert the provider's used percentage into the percentage left.
fn left_percent(used_percent: f64) -> f64 {
    (100.0 - used_percent).clamp(0.0, 100.0)
}

/// A compact label for a quota-window duration.
///
/// Exact days and hours use the adjective form that fits beside the bar
/// (`7-day`, `5-hour`). Unusual durations stay precise without a separate
/// Duration row.
fn duration_label(duration_secs: i64) -> String {
    if duration_secs > 0 && duration_secs % 86_400 == 0 {
        format!("{}-day", duration_secs / 86_400)
    } else if duration_secs > 0 && duration_secs % 3_600 == 0 {
        format!("{}-hour", duration_secs / 3_600)
    } else if duration_secs > 0 && duration_secs % 60 == 0 {
        format!("{}-min", duration_secs / 60)
    } else {
        let hours = duration_secs / 3_600;
        let minutes = duration_secs % 3_600 / 60;
        let seconds = duration_secs % 60;
        match (hours, minutes, seconds) {
            (hours, minutes, _) if hours > 0 && minutes > 0 => {
                format!("{hours}h {minutes}m")
            }
            (hours, _, _) if hours > 0 => format!("{hours}h"),
            (_, minutes, seconds) if minutes > 0 && seconds > 0 => {
                format!("{minutes}m {seconds}s")
            }
            (_, minutes, _) if minutes > 0 => format!("{minutes}m"),
            (_, _, seconds) => format!("{seconds}s"),
        }
    }
}

/// Extract a Codex bucket name from its `bucket Primary/Secondary` label.
fn bucket_id(label: &WindowLabel) -> Option<&str> {
    let WindowLabel::Other(label) = label else {
        return None;
    };
    label
        .strip_suffix(" Primary")
        .or_else(|| label.strip_suffix(" Secondary"))
        .filter(|bucket| !bucket.is_empty())
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

/// Time remaining until a future reset, at a glance precision.
fn reset_countdown(resets_at: i64, now: i64) -> Option<String> {
    let seconds = resets_at.saturating_sub(now);
    if seconds <= 0 {
        return None;
    }

    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;

    Some(if days > 0 {
        if hours > 0 {
            format!("{days}d {hours}hr")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{hours}hr {minutes}m")
        } else {
            format!("{hours}hr")
        }
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "under 1m".to_string()
    })
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
            source: None,
            caption: None,
            last_inference: None,
        }
    }

    /// An entry with the status of the last inference.
    fn entry_with_inference(outcome: ProviderUsage, status: u16) -> ProviderEntry {
        ProviderEntry {
            provider: ProviderId::OpenAi,
            outcome,
            source: None,
            caption: None,
            last_inference: Some(status),
        }
    }

    /// The ChatGPT answer: a provider with no quota API of its own.
    fn chatgpt_unavailable() -> ProviderUsage {
        ProviderUsage::Unavailable {
            reason: "Sign in with ChatGPT does not expose a quota API".to_string(),
            source: Some("not exposed by OAuth".to_string()),
            hint: Some("https://chatgpt.com/settings/usage".to_string()),
            signals: vec![
                "429 subscription_sharing_usage_limit_exceeded".to_string(),
                "503 subscription_sharing_usage_unavailable".to_string(),
                "403 subscription_sharing_user_not_eligible".to_string(),
            ],
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
    fn a_tiny_remaining_percentage_does_not_contradict_its_bar() {
        // When almost all the allowance is used, a remainder that rounds to
        // zero reads `0% left` and has an empty bar.
        let left = left_percent(99.96);
        assert_eq!(percent_text(left), "0%");
        assert_eq!(bar(left).chars().filter(|c| *c == FILLED).count(), 0);
        assert_eq!(bar(left).chars().count(), 20);

        // A remainder that rounds above zero gets a cell.
        let left = left_percent(99.9);
        assert_eq!(percent_text(left), "0.1%");
        assert_eq!(bar(left).chars().filter(|c| *c == FILLED).count(), 1);
        assert_eq!(bar(left).chars().count(), 20);
    }

    #[test]
    fn the_chatgpt_block_names_the_error_codes() {
        let entry = ProviderEntry {
            provider: ProviderId::OpenAi,
            outcome: chatgpt_unavailable(),
            source: None,
            caption: None,
            last_inference: None,
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
    fn the_chatgpt_block_says_what_the_last_inference_did() {
        // The quota cannot be read for ChatGPT, so the inference is the only
        // evidence there is about the account.
        let text = render(&[entry_with_inference(chatgpt_unavailable(), 200)]);

        assert!(text.contains("Quota       unavailable"), "{text}");
        assert!(text.contains("Inference   succeeded"), "{text}");
        assert!(text.contains("Source      not exposed by OAuth"), "{text}");
        assert!(
            text.contains("Limits      https://chatgpt.com/settings/usage"),
            "{text}"
        );
    }

    #[test]
    fn a_refused_inference_reads_with_its_status() {
        let text = render(&[entry_with_inference(chatgpt_unavailable(), 429)]);
        assert!(text.contains("Inference   failed (429)"), "{text}");
    }

    #[test]
    fn a_block_with_no_inference_has_no_row() {
        // The daemon has not served one, so there is nothing to say.
        let text = render(&[entry(chatgpt_unavailable())]);
        assert!(!text.contains("Inference"), "{text}");
    }

    #[test]
    fn a_block_with_a_percentage_does_not_repeat_the_inference() {
        // The quota is known here, so the block says everything about the
        // account that it can.
        let mut reported_entry = entry(reported(Some(40.0)));
        reported_entry.last_inference = Some(200);
        let text = render(&[reported_entry]);
        assert!(!text.contains("Inference"), "{text}");
    }

    #[test]
    fn a_failed_read_still_says_what_the_inference_did() {
        let mut failed = entry_with_inference(
            ProviderUsage::Failed {
                reason: "could not reach the billing endpoint".to_string(),
            },
            200,
        );
        failed.caption = caption_for(ProviderId::Xai, &crate::config::Config::default());
        let text = render(&[failed]);

        assert!(
            text.contains("Quota       unavailable — could not reach the billing endpoint"),
            "{text}"
        );
        assert!(text.contains("Inference   succeeded"), "{text}");
        // The caption keeps the two apart: the inference that succeeded is
        // metered against another pool.
        assert!(
            text.contains("billed separately against the xAI API tier"),
            "{text}"
        );
    }

    #[test]
    fn the_json_report_carries_the_last_inference() {
        let entries = vec![entry_with_inference(chatgpt_unavailable(), 429)];

        let report = UsageReportJson {
            providers: &entries,
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.contains("\"last_inference\":429"), "{text}");
        assert!(
            text.contains("\"source\":\"not exposed by OAuth\""),
            "{text}"
        );
    }

    #[test]
    fn the_daemons_answer_is_read_over_http() {
        // A listener that answers the way the daemon does.
        use std::io::{BufRead, BufReader, Write};

        let body = r#"{"providers":{"openai":429}}"#;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the listener binds");
        let port = listener.local_addr().expect("the address").port();
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the client connects");
            let mut request = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                request
                    .read_line(&mut line)
                    .expect("the request header reads");
                if line == "\r\n" {
                    break;
                }
            }
            request
                .into_inner()
                .write_all(answer.as_bytes())
                .expect("the response writes");
        });

        let read = fetch_last_inferences("127.0.0.1", port);
        assert_eq!(read.get(&ProviderId::OpenAi), Some(&429));
        server.join().expect("the stub server finishes");
    }

    #[test]
    fn a_daemon_that_cannot_be_reached_is_not_an_error() {
        // A port nothing is listening on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("the listener binds");
        let port = listener.local_addr().expect("the address").port();
        drop(listener);

        assert!(fetch_last_inferences("127.0.0.1", port).is_empty());
    }

    #[test]
    fn the_daemons_answer_is_read() {
        let parsed = parse_last_inferences(r#"{"providers":{"openai":429,"xai":200}}"#);
        assert_eq!(parsed.get(&ProviderId::OpenAi), Some(&429));
        assert_eq!(parsed.get(&ProviderId::Xai), Some(&200));
    }

    #[test]
    fn an_unusable_answer_is_read_as_nothing() {
        assert!(parse_last_inferences("").is_empty());
        assert!(parse_last_inferences("not json").is_empty());
        assert!(parse_last_inferences(r#"{"providers":{}}"#).is_empty());
        // A provider we do not know is not one of ours.
        assert!(parse_last_inferences(r#"{"providers":{"nope":200}}"#).is_empty());
        // And a status that is not a status is not one.
        assert!(parse_last_inferences(r#"{"providers":{"openai":"200"}}"#).is_empty());
    }

    #[test]
    fn the_bar_and_percentage_show_remaining_allowance() {
        let text = render(&[entry(reported(Some(40.0)))]);
        assert!(text.contains(&format!("{}  60% left", bar(60.0))), "{text}");
        assert!(!text.contains("40% used"), "{text}");
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
        assert!(text.contains("50% left"), "{text}");
        assert!(text.contains("GrokChat"), "{text}");
        assert!(text.contains("80% left"), "{text}");
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
            outcome: ProviderUsage::NotSignedIn,
            source: None,
            caption: None,
            last_inference: None,
        }];

        let report = UsageReportJson {
            providers: &entries,
        };
        let text = serde_json::to_string(&report).unwrap();
        assert!(text.contains("\"provider\":\"openai\""), "{text}");
        assert!(text.contains("\"status\":\"not_signed_in\""), "{text}");
        assert!(!text.contains("\"source\":null"), "{text}");
    }

    #[test]
    fn usage_selects_only_enabled_providers_and_rejects_disabled_selections() {
        let mut config = crate::config::Config::default();
        config.providers.xai.enabled = false;
        assert_eq!(
            selected_provider_ids(None, &config).unwrap(),
            vec![ProviderId::OpenAi]
        );

        let error = selected_provider_ids(Some(ProviderId::Xai), &config).unwrap_err();
        assert!(
            error
                .message
                .unwrap_or_default()
                .contains("xai is disabled")
        );

        config.providers.openai.enabled = false;
        let error = selected_provider_ids(None, &config).unwrap_err();
        assert!(
            error
                .message
                .unwrap_or_default()
                .contains("no providers are enabled")
        );
    }

    fn context_for(directory: &std::path::Path) -> Context {
        Context {
            paths: crate::paths::Paths::in_directory(directory),
            config: crate::config::Config::default(),
            cli: crate::cli::args::Cli {
                command: crate::cli::args::Command::Usage {
                    provider: Some(ProviderId::OpenAi),
                    json: false,
                },
                config_path: None,
                verbose: false,
            },
        }
    }

    fn codex_report() -> UsageReport {
        UsageReport {
            provider: ProviderId::OpenAi,
            plan: None,
            windows: vec![QuotaWindow {
                label: WindowLabel::Other("codex Primary".to_string()),
                used_percent: Some(25.0),
                resets_at: Some(1_800_000_000),
                duration_secs: Some(18_000),
            }],
            breakdown: Vec::new(),
            credits: None,
            state: None,
        }
    }

    #[test]
    fn a_codex_report_precedes_the_missing_eggpass_credential_check() {
        let directory = tempfile::tempdir().unwrap();
        let context = context_for(directory.path());
        let (outcome, source) =
            collect_one_with_codex_result(&context, ProviderId::OpenAi, Some(codex_report()));
        let entry = ProviderEntry {
            provider: ProviderId::OpenAi,
            outcome,
            source,
            caption: None,
            last_inference: None,
        };

        assert!(matches!(&entry.outcome, ProviderUsage::Reported { .. }));
        assert_eq!(
            entry.source,
            Some(crate::provider::usage::codex_app_server::SOURCE_LABEL.to_string())
        );
        assert!(usage_exit_result(std::slice::from_ref(&entry)).is_ok());
    }

    #[test]
    fn an_unavailable_codex_source_preserves_both_existing_fallbacks() {
        let directory = tempfile::tempdir().unwrap();
        let context = context_for(directory.path());
        let (outcome, source) = collect_one_with_codex_result(&context, ProviderId::OpenAi, None);
        assert!(matches!(&outcome, ProviderUsage::NotSignedIn));
        assert_eq!(source, None);
        let error = usage_exit_result(&[ProviderEntry {
            provider: ProviderId::OpenAi,
            outcome,
            source,
            caption: None,
            last_inference: None,
        }])
        .unwrap_err();
        assert_eq!(error.code, 2);
        assert_eq!(
            error.message.as_deref(),
            Some("not signed in — run `eggpass login`")
        );

        let credential = crate::auth::store::StoredAuth {
            access_token: "test-only-access-token".to_string(),
            refresh_token: None,
            id_token: None,
            token_type: "Bearer".to_string(),
            scope: None,
            client_id: "test-client".to_string(),
            issuer: "https://auth.openai.com".to_string(),
            expires_at: Some(crate::timeutil::now_unix() + 3_600),
            obtained_at: crate::timeutil::now_unix(),
            user_id: None,
            email: None,
            ext_agent_host_id: None,
            plan_enabled: false,
        };
        crate::auth::store::save(
            &context.paths.credential_file(ProviderId::OpenAi.as_str()),
            &credential,
        )
        .unwrap();
        let (outcome, source) = collect_one_with_codex_result(&context, ProviderId::OpenAi, None);
        assert_eq!(source, None);
        assert!(
            matches!(
                outcome,
                ProviderUsage::Unavailable {
                    ref reason,
                    ref source,
                    ref hint,
                    ref signals,
                } if reason == "Sign in with ChatGPT does not expose a quota API"
                    && source.as_deref() == Some("not exposed by OAuth")
                    && hint.as_deref() == Some("https://chatgpt.com/settings/usage")
                    && signals.len() == 3
            ),
            "unexpected fallback: {outcome:?}"
        );
    }

    #[test]
    fn codex_source_and_bucket_are_visible_in_text_and_json_without_identity() {
        let mut entry = ProviderEntry {
            provider: ProviderId::OpenAi,
            outcome: ProviderUsage::Reported {
                report: codex_report(),
            },
            source: Some(crate::provider::usage::codex_app_server::SOURCE_LABEL.to_string()),
            caption: None,
            last_inference: None,
        };
        let text = render(std::slice::from_ref(&entry));
        assert!(text.contains("5-hour"), "{text}");
        assert!(text.contains("75% left"), "{text}");
        assert!(!text.contains("codex Primary"), "{text}");
        assert!(!text.contains("Duration"), "{text}");
        assert!(text.contains("Source      Codex app-server"), "{text}");
        assert!(!text.contains("@"), "{text}");

        let report = UsageReportJson {
            providers: std::slice::from_ref(&entry),
        };
        let value = serde_json::to_value(report).unwrap();
        let encoded = value.to_string();
        assert!(
            encoded.contains("\"source\":\"Codex app-server"),
            "{encoded}"
        );
        assert!(encoded.contains("codex Primary"), "{encoded}");
        assert!(encoded.contains("\"duration_secs\":18000"), "{encoded}");
        assert!(!encoded.contains("@"), "{encoded}");

        entry.source = None;
        let legacy_json = serde_json::to_value(UsageReportJson {
            providers: std::slice::from_ref(&entry),
        })
        .unwrap();
        assert!(legacy_json["providers"][0].get("source").is_none());
    }

    #[test]
    fn codex_usage_is_not_assigned_to_xai() {
        assert!(codex_outcome(ProviderId::Xai, Some(codex_report())).is_none());
    }

    #[test]
    fn durations_become_compact_window_labels() {
        assert_eq!(duration_label(18_000), "5-hour");
        assert_eq!(duration_label(604_800), "7-day");
        assert_eq!(duration_label(300), "5-min");
        assert_eq!(duration_label(5_400), "90-min");
        assert_eq!(duration_label(3_661), "1h 1m");

        let text = render(&[entry(reported(Some(40.0)))]);
        assert!(text.contains("Weekly"), "{text}");
        assert!(!text.contains("Duration"), "{text}");
    }

    #[test]
    fn a_named_weekly_window_is_not_relabelled_or_expanded() {
        let mut report = reported_report(Some(1.0));
        report.windows[0].resets_at =
            Some(timeutil::now_unix() + 3 * 86_400 + 15 * 3_600 + 30 * 60);
        let text = render(&[entry(ProviderUsage::Reported { report })]);

        assert!(text.contains("Weekly"), "{text}");
        assert!(text.contains("99% left"), "{text}");
        assert!(text.contains("in 3d 15hr"), "{text}");
        assert!(!text.contains("5-hour"), "{text}");
        assert!(!text.contains("7-day"), "{text}");
    }

    #[test]
    fn reset_countdowns_are_compact_and_omit_past_resets() {
        let now = timeutil::parse_rfc3339_to_unix("2026-11-14T12:00:00Z").unwrap();
        assert_eq!(
            reset_countdown(now + 3 * 86_400 + 15 * 3_600 + 42 * 60, now),
            Some("3d 15hr".to_string())
        );
        assert_eq!(
            reset_countdown(now + 3 * 3_600 + 21 * 60 + 12, now),
            Some("3hr 21m".to_string())
        );
        assert_eq!(reset_countdown(now + 20, now), Some("under 1m".to_string()));
        assert_eq!(reset_countdown(now, now), None);
        assert_eq!(reset_countdown(now - 30, now), None);
    }

    #[test]
    fn each_provider_window_shows_duration_left_and_its_reset_countdown() {
        let now = timeutil::now_unix();
        let mut report = codex_report();
        report.windows = vec![
            QuotaWindow {
                label: WindowLabel::Other("codex Primary".to_string()),
                used_percent: Some(69.0),
                resets_at: Some(now + 3 * 3_600 + 21 * 60),
                duration_secs: Some(18_000),
            },
            QuotaWindow {
                label: WindowLabel::Other("codex Secondary".to_string()),
                used_percent: Some(95.0),
                resets_at: Some(now + 3 * 86_400 + 15 * 3_600 + 30 * 60),
                duration_secs: Some(604_800),
            },
        ];
        let text = render(&[entry(ProviderUsage::Reported { report })]);

        assert!(text.contains("5-hour"), "{text}");
        assert!(text.contains("31% left"), "{text}");
        assert!(text.contains("7-day"), "{text}");
        assert!(text.contains("5% left"), "{text}");
        assert!(text.contains("in 3hr 21m"), "{text}");
        assert!(text.contains("in 3d 15hr"), "{text}");
        assert!(!text.contains("Primary"), "{text}");
        assert!(!text.contains("Secondary"), "{text}");
        assert!(!text.contains("Duration"), "{text}");
    }

    #[test]
    fn multiple_codex_buckets_remain_distinguishable_with_duration_labels() {
        let mut report = codex_report();
        report.windows.push(QuotaWindow {
            label: WindowLabel::Other("other-model Primary".to_string()),
            used_percent: Some(50.0),
            resets_at: None,
            duration_secs: Some(18_000),
        });
        let text = render(&[entry(ProviderUsage::Reported { report })]);

        assert!(text.contains("codex 5-hour"), "{text}");
        assert!(text.contains("other-model 5-hour"), "{text}");
        assert!(!text.contains("Primary"), "{text}");
    }
}

#[cfg(test)]
mod daemon_cache_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};

    struct HttpStub {
        port: u16,
        request: Receiver<String>,
    }

    fn http_stub(status: u16, body: String, headers: &[(&str, &str)]) -> HttpStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local stub");
        let port = listener.local_addr().expect("local stub address").port();
        let (send, request) = mpsc::channel();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut raw_request = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read request head");
                if line.is_empty() || line == "\r\n" {
                    break;
                }
                raw_request.push_str(&line);
            }
            let _ = send.send(raw_request);
            let mut response =
                format!("HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\n");
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            stream
                .write_all(response.as_bytes())
                .expect("write stub response");
            stream.flush().expect("flush stub response");
        });
        HttpStub { port, request }
    }

    fn running(port: u16) -> DaemonState {
        DaemonState::Running {
            pid: 1,
            host: "127.0.0.1".to_string(),
            port,
        }
    }

    fn sample_entry(provider: ProviderId, last_inference: Option<u16>) -> ProviderEntry {
        ProviderEntry {
            provider,
            outcome: ProviderUsage::Reported {
                report: UsageReport {
                    provider,
                    plan: Some("test plan".to_string()),
                    windows: Vec::new(),
                    breakdown: Vec::new(),
                    credits: None,
                    state: None,
                },
            },
            source: None,
            caption: None,
            last_inference,
        }
    }

    fn report_body(entries: &[ProviderEntry]) -> String {
        serde_json::to_string(&UsageReportJson { providers: entries }).expect("serialize fixture")
    }

    fn usage_key_path(directory: &tempfile::TempDir) -> (PathBuf, UsageKey) {
        let path = directory.path().join("usage.key");
        let key = UsageKey::create(&path).expect("create test usage key");
        (path, key)
    }

    fn fetch_local(
        host: &str,
        port: u16,
        secret: &str,
        selected: &[ProviderId],
    ) -> Result<DaemonUsageResponse, DaemonFetchError> {
        fetch_daemon_usage(host, port, secret, selected)
    }

    #[test]
    fn a_fresh_daemon_report_uses_the_bearer_header_and_skips_direct_collection() {
        let directory = tempfile::tempdir().unwrap();
        let (key_path, key) = usage_key_path(&directory);
        let body = report_body(&[sample_entry(ProviderId::Xai, Some(429))]);
        let stub = http_stub(200, body, &[]);
        let cached = try_cached_usage_with(
            running(stub.port),
            &key_path,
            &[ProviderId::Xai],
            fetch_local,
            || running(stub.port),
        )
        .expect("daemon response parses");

        let raw_request = stub.request.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut lines = raw_request.lines();
        assert_eq!(lines.next(), Some("GET /v1/usage HTTP/1.1"));
        let authorization = lines
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then_some(value.trim())
            })
            .unwrap_or_default();
        let expected = format!("Bearer {}", key.token());
        assert!(crate::keys::constant_time_eq(
            authorization.as_bytes(),
            expected.as_bytes()
        ));
        assert!(
            !raw_request
                .lines()
                .next()
                .unwrap_or_default()
                .contains(key.token())
        );

        let direct_calls = AtomicUsize::new(0);
        let (entries, stale) = report_with_fallback(
            &[ProviderId::Xai],
            Ok(cached),
            || panic!("cache hit must not fetch last-inference separately"),
            |provider| {
                direct_calls.fetch_add(1, Ordering::SeqCst);
                sample_entry(provider, None)
            },
        )
        .expect("cached report selected");
        assert!(!stale);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 0);
        assert_eq!(entries[0].last_inference, Some(429));
    }

    #[test]
    fn a_stale_cached_report_warns_on_stderr_without_changing_json_stdout() {
        let directory = tempfile::tempdir().unwrap();
        let (key_path, _key) = usage_key_path(&directory);
        let body = report_body(&[sample_entry(ProviderId::Xai, Some(503))]);
        let stub = http_stub(200, body, &[("X-Eggpass-Usage-Stale", "true")]);
        let cached = try_cached_usage_with(
            running(stub.port),
            &key_path,
            &[ProviderId::Xai],
            fetch_local,
            || running(stub.port),
        )
        .expect("stale response is valid");
        assert!(stub.request.recv_timeout(Duration::from_secs(2)).is_ok());

        let (entries, stale) = report_with_fallback(
            &[ProviderId::Xai],
            Ok(cached),
            BTreeMap::new,
            sample_entry_with_no_inference,
        )
        .unwrap();
        let mut stderr = Vec::new();
        write_stale_cache_notice(&mut stderr, stale).unwrap();
        let stdout = serde_json::to_string_pretty(&UsageReportJson {
            providers: &entries,
        })
        .unwrap();
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("cache is stale"));
        assert!(!stdout.contains("cache is stale"));
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["providers"][0]["last_inference"], 503);
    }

    fn sample_entry_with_no_inference(provider: ProviderId) -> ProviderEntry {
        sample_entry(provider, None)
    }

    #[test]
    fn route_404_falls_back_but_pending_cooldown_and_auth_errors_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let (key_path, _key) = usage_key_path(&directory);

        let not_found = http_stub(404, "{}".to_string(), &[]);
        let fallback = try_cached_usage_with(
            running(not_found.port),
            &key_path,
            &[ProviderId::Xai],
            fetch_local,
            || running(not_found.port),
        )
        .unwrap();
        assert!(fallback.is_none());
        assert!(
            not_found
                .request
                .recv_timeout(Duration::from_secs(2))
                .is_ok()
        );

        for (status, retry_after) in [
            (202, Some("3")),
            (503, Some("7")),
            (504, Some("11")),
            (401, None),
            (403, None),
        ] {
            let headers: Vec<(&str, &str)> = retry_after
                .into_iter()
                .map(|value| ("Retry-After", value))
                .collect();
            let stub = http_stub(status, "{}".to_string(), &headers);
            let result = try_cached_usage_with(
                running(stub.port),
                &key_path,
                &[ProviderId::Xai],
                fetch_local,
                || running(stub.port),
            );
            let error = result.expect_err("non-404 statuses are surfaced");
            let message = error.message.unwrap_or_default();
            assert!(message.contains(&status.to_string()));
            if let Some(seconds) = retry_after {
                assert!(message.contains(seconds));
            }
            assert!(!message.contains("usage.key"));
            assert!(stub.request.recv_timeout(Duration::from_secs(2)).is_ok());

            let direct_calls = AtomicUsize::new(0);
            let direct_error = report_with_fallback(
                &[ProviderId::Xai],
                Err(CommandError::new(message)),
                || panic!("error must not query the direct fallback"),
                |provider| {
                    direct_calls.fetch_add(1, Ordering::SeqCst);
                    sample_entry(provider, None)
                },
            );
            assert!(direct_error.is_err());
            assert_eq!(direct_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn malformed_success_is_visible_and_never_bypassed() {
        let directory = tempfile::tempdir().unwrap();
        let (key_path, _key) = usage_key_path(&directory);
        let stub = http_stub(200, "provider identity secret".to_string(), &[]);
        let result = try_cached_usage_with(
            running(stub.port),
            &key_path,
            &[ProviderId::Xai],
            fetch_local,
            || running(stub.port),
        );
        let error = result.expect_err("malformed JSON must fail");
        let message = error.message.unwrap_or_default();
        assert!(message.contains("invalid usage report"));
        assert!(!message.contains("provider identity secret"));
        assert!(stub.request.recv_timeout(Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn a_silent_daemon_response_hits_the_configured_deadline() {
        use std::time::Instant;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent local stub");
        let port = listener.local_addr().expect("silent stub address").port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept local request");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).expect("read request head");
                if line.is_empty() || line == "\r\n" {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        });

        let started = Instant::now();
        let result = fetch_daemon_usage_with_timeouts(
            "127.0.0.1",
            port,
            "test-only-secret",
            &[ProviderId::Xai],
            Duration::from_secs(1),
            Duration::from_millis(50),
        );
        assert!(matches!(result, Err(DaemonFetchError::Transport)));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().expect("silent stub finishes");
    }

    #[test]
    fn direct_fallback_occurs_only_for_missing_state_or_stale_connection() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.key");
        let fetch_calls = AtomicUsize::new(0);
        let mut rechecks = 0;
        let absent_key = try_cached_usage_with(
            running(1),
            &missing,
            &[ProviderId::Xai],
            |_, _, _, _| {
                fetch_calls.fetch_add(1, Ordering::SeqCst);
                Err(DaemonFetchError::Transport)
            },
            || {
                rechecks += 1;
                DaemonState::Stopped
            },
        )
        .unwrap();
        assert!(absent_key.is_none());
        assert_eq!(fetch_calls.load(Ordering::SeqCst), 0);
        assert_eq!(rechecks, 0);

        let malformed = directory.path().join("malformed.key");
        std::fs::write(&malformed, "not-a-usage-token").unwrap();
        let unreadable_key = try_cached_usage_with(
            running(1),
            &malformed,
            &[ProviderId::Xai],
            |_, _, _, _| panic!("an invalid key file must not reach the daemon"),
            || panic!("an invalid key file does not need a daemon-state recheck"),
        )
        .unwrap();
        assert!(unreadable_key.is_none());
        let direct_calls = AtomicUsize::new(0);
        let (entries, _) = report_with_fallback(
            &[ProviderId::Xai],
            Ok(unreadable_key),
            BTreeMap::new,
            |provider| {
                direct_calls.fetch_add(1, Ordering::SeqCst);
                sample_entry_with_no_inference(provider)
            },
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 1);

        let (key_path, _key) = usage_key_path(&directory);
        let stale = try_cached_usage_with(
            running(1),
            &key_path,
            &[ProviderId::Xai],
            |_, _, _, _| Err(DaemonFetchError::Transport),
            || DaemonState::Stale { pid: 1 },
        )
        .unwrap();
        assert!(stale.is_none());

        let still_running = try_cached_usage_with(
            running(1),
            &key_path,
            &[ProviderId::Xai],
            |_, _, _, _| Err(DaemonFetchError::Transport),
            || running(1),
        );
        assert!(still_running.is_err());

        for state in [DaemonState::Stopped, DaemonState::Stale { pid: 1 }] {
            let fallback = try_cached_usage_with(
                state,
                &key_path,
                &[ProviderId::Xai],
                |_, _, _, _| panic!("no request when daemon is not running"),
                || DaemonState::Stopped,
            )
            .unwrap();
            let direct_calls = AtomicUsize::new(0);
            let (entries, _) = report_with_fallback(
                &[ProviderId::Xai],
                Ok(fallback),
                BTreeMap::new,
                |provider| {
                    direct_calls.fetch_add(1, Ordering::SeqCst);
                    sample_entry_with_no_inference(provider)
                },
            )
            .unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn provider_selection_is_applied_to_the_cached_report() {
        let entries = vec![
            sample_entry(ProviderId::Xai, Some(200)),
            sample_entry(ProviderId::OpenAi, Some(429)),
        ];
        let selected = select_cached_entries(entries, &[ProviderId::OpenAi]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].provider, ProviderId::OpenAi);
        assert_eq!(selected[0].last_inference, Some(429));
    }
}
