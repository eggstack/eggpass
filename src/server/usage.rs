//! Authenticated daemon usage route and its process-local refresh cache.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::provider::ProviderId;
use crate::provider::usage::{
    ProviderUsage,
    report::{CollectedEntry, ProviderEntry, UsageReportJson, attach_inferences},
};
use crate::timeutil;

const CACHE_TTL: Duration = Duration::from_secs(600);
const CLIENT_WAIT: Duration = Duration::from_secs(30);
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];
const FAILURE_COOLDOWN: Duration = Duration::from_secs(600);
const CACHE_CONTROL: &str = "private, no-store";

/// Usage collection callback, supplied by the daemon with shared token managers.
type Collector = dyn Fn(ProviderId) -> CollectedEntry + Send + Sync + 'static;

/// Tunable only inside deterministic cache tests; production uses DEFAULT_POLICY.
#[derive(Debug, Clone, Copy)]
struct CachePolicy {
    ttl: Duration,
    client_wait: Duration,
    retry_delays: [Duration; 3],
    cooldown: Duration,
}

const DEFAULT_POLICY: CachePolicy = CachePolicy {
    ttl: CACHE_TTL,
    client_wait: CLIENT_WAIT,
    retry_delays: RETRY_DELAYS,
    cooldown: FAILURE_COOLDOWN,
};

/// A complete cached report. Monotonic time controls expiry; wall time is only
/// used for the public RFC 3339 update header.
#[derive(Clone)]
struct Snapshot {
    entries: Vec<ProviderEntry>,
    completed_at: Instant,
    updated_at_unix: i64,
}

#[derive(Default)]
struct CacheState {
    snapshot: Option<Snapshot>,
    refresh: Option<RefreshState>,
    cooldown_until: Option<Instant>,
}

struct RefreshState {
    next_retry_at: Option<Instant>,
    retry_after_hint: u64,
}

/// What the route should return from the cache coordinator.
enum CacheResponse {
    Complete { snapshot: Snapshot, stale: bool },
    Pending { retry_after: u64 },
    TimedOut { retry_after: u64 },
    Unavailable { retry_after: u64 },
}

/// One process-local usage cache and one finite, request-triggered refresh job.
pub struct UsageCache {
    providers: Vec<ProviderId>,
    collector: Arc<Collector>,
    state: Arc<Mutex<CacheState>>,
    changed: Arc<Condvar>,
    policy: CachePolicy,
}

impl UsageCache {
    /// Create a cold cache over only the providers selected for this daemon.
    pub fn new<F>(providers: Vec<ProviderId>, collector: F) -> UsageCache
    where
        F: Fn(ProviderId) -> CollectedEntry + Send + Sync + 'static,
    {
        UsageCache::with_policy(providers, collector, DEFAULT_POLICY)
    }

    fn with_policy<F>(providers: Vec<ProviderId>, collector: F, policy: CachePolicy) -> UsageCache
    where
        F: Fn(ProviderId) -> CollectedEntry + Send + Sync + 'static,
    {
        UsageCache {
            providers,
            collector: Arc::new(collector),
            state: Arc::new(Mutex::new(CacheState::default())),
            changed: Arc::new(Condvar::new()),
            policy,
        }
    }

    /// Return a fresh snapshot, or trigger/share one finite refresh.
    fn get(&self) -> CacheResponse {
        let now = Instant::now();
        let mut state = lock(&self.state);
        if let Some(response) = response_for_state(&state, now, self.policy) {
            // A request arriving after another one started never waits on its
            // handler thread; it gets 202 and the scheduled retry hint.
            return response;
        }

        state.cooldown_until = None;
        state.refresh = Some(RefreshState {
            next_retry_at: None,
            retry_after_hint: self.policy.retry_delays[0].as_secs().max(1),
        });
        drop(state);

        let providers = self.providers.clone();
        let collector = Arc::clone(&self.collector);
        let worker_state = Arc::clone(&self.state);
        let changed = Arc::clone(&self.changed);
        let policy = self.policy;
        if thread::Builder::new()
            .name("eggpass-usage-refresh".to_string())
            .spawn(move || run_refresh(providers, collector, worker_state, changed, policy))
            .is_err()
        {
            let mut state = lock(&self.state);
            state.refresh = None;
            self.changed.notify_all();
            return CacheResponse::Unavailable { retry_after: 1 };
        }

        self.wait_for_started_refresh()
    }

    fn wait_for_started_refresh(&self) -> CacheResponse {
        let deadline = Instant::now() + self.policy.client_wait;
        let mut state = lock(&self.state);
        loop {
            let now = Instant::now();
            if state.refresh.is_none() {
                if let Some(response) = response_for_state(&state, now, self.policy) {
                    return response;
                }
                return CacheResponse::Unavailable { retry_after: 1 };
            }

            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                return CacheResponse::TimedOut {
                    retry_after: retry_after(&state, now),
                };
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if timeout.timed_out() && state.refresh.is_some() {
                return CacheResponse::TimedOut {
                    retry_after: retry_after(&state, Instant::now()),
                };
            }
        }
    }
}

fn run_refresh(
    providers: Vec<ProviderId>,
    collector: Arc<Collector>,
    state: Arc<Mutex<CacheState>>,
    changed: Arc<Condvar>,
    policy: CachePolicy,
) {
    let mut entries = Vec::with_capacity(providers.len());
    let mut timed_out = Vec::new();
    for provider in providers {
        let result = collect_safely(collector.as_ref(), provider);
        if result.timed_out {
            timed_out.push(provider);
        }
        entries.push(result.entry);
    }

    if timed_out.is_empty() {
        finish_success(entries, &state, &changed);
        return;
    }

    for (retry_index, delay) in policy.retry_delays.iter().copied().enumerate() {
        {
            let mut current = lock(&state);
            if let Some(refresh) = current.refresh.as_mut() {
                refresh.next_retry_at = Some(Instant::now() + delay);
                refresh.retry_after_hint = delay.as_secs().max(1);
            }
            changed.notify_all();
        }
        thread::sleep(delay);
        {
            let mut current = lock(&state);
            if let Some(refresh) = current.refresh.as_mut() {
                refresh.next_retry_at = None;
                refresh.retry_after_hint = policy
                    .retry_delays
                    .get(retry_index + 1)
                    .map(|next| next.as_secs().max(1))
                    .unwrap_or(1);
            }
        }

        let retrying = std::mem::take(&mut timed_out);
        for provider in retrying {
            let result = collect_safely(collector.as_ref(), provider);
            if let Some(entry) = entries.iter_mut().find(|entry| entry.provider == provider) {
                *entry = result.entry;
            }
            if result.timed_out {
                timed_out.push(provider);
            }
        }

        if timed_out.is_empty() {
            finish_success(entries, &state, &changed);
            return;
        }
    }

    let mut current = lock(&state);
    current.refresh = None;
    current.cooldown_until = Some(Instant::now() + policy.cooldown);
    changed.notify_all();
}

fn collect_safely(collector: &Collector, provider: ProviderId) -> CollectedEntry {
    match catch_unwind(AssertUnwindSafe(|| collector(provider))) {
        Ok(result) => result,
        Err(_) => CollectedEntry {
            entry: ProviderEntry {
                provider,
                outcome: ProviderUsage::Failed {
                    reason: "the usage collector failed unexpectedly".to_string(),
                },
                source: None,
                caption: None,
                last_inference: None,
            },
            timed_out: false,
        },
    }
}

fn finish_success(entries: Vec<ProviderEntry>, state: &Mutex<CacheState>, changed: &Condvar) {
    let mut current = lock(state);
    current.snapshot = Some(Snapshot {
        entries,
        completed_at: Instant::now(),
        updated_at_unix: timeutil::now_unix(),
    });
    current.refresh = None;
    current.cooldown_until = None;
    changed.notify_all();
}

fn response_for_state(
    state: &CacheState,
    now: Instant,
    policy: CachePolicy,
) -> Option<CacheResponse> {
    if let Some(snapshot) = &state.snapshot
        && now.saturating_duration_since(snapshot.completed_at) < policy.ttl
    {
        return Some(CacheResponse::Complete {
            snapshot: snapshot.clone(),
            stale: false,
        });
    }

    if let Some(until) = state.cooldown_until.filter(|until| *until > now) {
        let retry_after = seconds_ceil(until.saturating_duration_since(now));
        return Some(match &state.snapshot {
            Some(snapshot) => CacheResponse::Complete {
                snapshot: snapshot.clone(),
                stale: true,
            },
            None => CacheResponse::Unavailable { retry_after },
        });
    }

    state.refresh.as_ref().map(|_| CacheResponse::Pending {
        retry_after: retry_after(state, now),
    })
}

fn retry_after(state: &CacheState, now: Instant) -> u64 {
    state.refresh.as_ref().map_or(1, |refresh| {
        refresh
            .next_retry_at
            .map(|at| seconds_ceil(at.saturating_duration_since(now)))
            .unwrap_or(refresh.retry_after_hint.max(1))
    })
}

fn seconds_ceil(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serve the authenticated usage route after the common request guard passed.
pub(crate) fn handle(context: &super::ServerContext) -> super::Response {
    match context.usage_cache.get() {
        CacheResponse::Complete { snapshot, stale } => {
            let inferences = context.inferences.snapshot();
            let entries = attach_inferences(&snapshot.entries, &inferences);
            let body = match serde_json::to_string(&UsageReportJson {
                providers: &entries,
            }) {
                Ok(body) => body,
                Err(_) => {
                    return api_error(
                        500,
                        "the usage report could not be serialized",
                        "usage_report_serialization_failed",
                        None,
                    );
                }
            };
            let age = Instant::now()
                .saturating_duration_since(snapshot.completed_at)
                .as_secs();
            let mut response = super::Response::json(200, body);
            response.headers.extend([
                ("Cache-Control".to_string(), CACHE_CONTROL.to_string()),
                ("X-Eggpass-Usage-Age".to_string(), age.to_string()),
                (
                    "X-Eggpass-Usage-Updated-At".to_string(),
                    timeutil::format_rfc3339(snapshot.updated_at_unix),
                ),
            ]);
            if stale {
                response
                    .headers
                    .push(("X-Eggpass-Usage-Stale".to_string(), "true".to_string()));
            }
            response
        }
        CacheResponse::Pending { retry_after } => api_error(
            202,
            "a usage report refresh is already in progress",
            "usage_refresh_pending",
            Some(retry_after),
        ),
        CacheResponse::TimedOut { retry_after } => api_error(
            504,
            "the usage report refresh did not finish within 30 seconds",
            "usage_refresh_timeout",
            Some(retry_after),
        ),
        CacheResponse::Unavailable { retry_after } => api_error(
            503,
            "the usage report is temporarily unavailable",
            "usage_refresh_unavailable",
            Some(retry_after),
        ),
    }
}

fn api_error(status: u16, message: &str, code: &str, retry_after: Option<u64>) -> super::Response {
    let mut response = super::openai_error_with_code(status, message, "server_error", Some(code));
    response
        .headers
        .push(("Cache-Control".to_string(), CACHE_CONTROL.to_string()));
    if let Some(seconds) = retry_after {
        response
            .headers
            .push(("Retry-After".to_string(), seconds.max(1).to_string()));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};

    fn policy(client_wait: Duration, retry_delays: [Duration; 3]) -> CachePolicy {
        CachePolicy {
            ttl: Duration::from_secs(600),
            client_wait,
            retry_delays,
            cooldown: Duration::from_millis(100),
        }
    }

    fn entry(provider: ProviderId, timed_out: bool) -> CollectedEntry {
        CollectedEntry {
            entry: ProviderEntry {
                provider,
                outcome: if timed_out {
                    ProviderUsage::Failed {
                        reason: "provider request timed out".to_string(),
                    }
                } else {
                    ProviderUsage::Unavailable {
                        reason: "test fixture outcome".to_string(),
                        source: None,
                        hint: None,
                        signals: Vec::new(),
                    }
                },
                source: None,
                caption: None,
                last_inference: None,
            },
            timed_out,
        }
    }

    fn wait_until_idle(cache: &UsageCache, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&cache.state);
        while state.refresh.is_some() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, result) = cache
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if result.timed_out() && state.refresh.is_some() {
                return false;
            }
        }
        true
    }

    #[test]
    fn production_policy_matches_the_selected_contract() {
        assert_eq!(DEFAULT_POLICY.ttl, Duration::from_secs(600));
        assert_eq!(DEFAULT_POLICY.client_wait, Duration::from_secs(30));
        assert_eq!(
            DEFAULT_POLICY.retry_delays,
            [
                Duration::from_secs(5),
                Duration::from_secs(15),
                Duration::from_secs(30),
            ]
        );
        assert_eq!(DEFAULT_POLICY.cooldown, Duration::from_secs(600));
    }

    #[test]
    fn a_fresh_snapshot_is_reused_and_an_expired_snapshot_refreshes_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let policy = policy(Duration::from_millis(300), [Duration::ZERO; 3]);
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            move |provider| {
                counted.fetch_add(1, Ordering::SeqCst);
                entry(provider, false)
            },
            policy,
        );

        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: false, .. }
        ));
        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: false, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        {
            let mut state = lock(&cache.state);
            state.snapshot.as_mut().unwrap().completed_at =
                Instant::now() - policy.ttl - Duration::from_millis(1);
        }
        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: false, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_client_timeout_does_not_cancel_or_duplicate_the_active_refresh() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let wait_for_release = Arc::clone(&release_rx);
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            move |provider| {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = recv_release(&wait_for_release);
                entry(provider, false)
            },
            policy(Duration::from_millis(10), [Duration::ZERO; 3]),
        );

        assert!(matches!(cache.get(), CacheResponse::TimedOut { .. }));
        assert!(matches!(
            cache.get(),
            CacheResponse::Pending { retry_after: 1.. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release_tx.send(()).unwrap();
        assert!(wait_until_idle(&cache, Duration::from_millis(300)));
        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: false, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn recv_release(receiver: &Arc<Mutex<Receiver<()>>>) -> Result<(), mpsc::RecvTimeoutError> {
        receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .recv_timeout(Duration::from_secs(1))
    }

    #[test]
    fn retries_only_the_timed_out_provider_and_respects_each_delay() {
        let xai_calls = Arc::new(AtomicUsize::new(0));
        let openai_calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let xai_counter = Arc::clone(&xai_calls);
        let openai_counter = Arc::clone(&openai_calls);
        let observed_times = Arc::clone(&observed);
        let delays = [
            Duration::from_millis(2),
            Duration::from_millis(5),
            Duration::from_millis(8),
        ];
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai, ProviderId::OpenAi],
            move |provider| match provider {
                ProviderId::Xai => {
                    observed_times.lock().unwrap().push(Instant::now());
                    let attempt = xai_counter.fetch_add(1, Ordering::SeqCst);
                    entry(provider, attempt < 3)
                }
                ProviderId::OpenAi => {
                    openai_counter.fetch_add(1, Ordering::SeqCst);
                    entry(provider, false)
                }
            },
            policy(Duration::from_millis(300), delays),
        );

        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: false, .. }
        ));
        assert_eq!(xai_calls.load(Ordering::SeqCst), 4);
        assert_eq!(openai_calls.load(Ordering::SeqCst), 1);
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 4);
        for (index, delay) in delays.iter().enumerate() {
            assert!(observed[index + 1].duration_since(observed[index]) >= *delay);
        }
    }

    #[test]
    fn retry_exhaustion_serves_only_the_previous_report_as_stale() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let policy = policy(Duration::from_millis(300), [Duration::ZERO; 3]);
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            move |provider| {
                counted.fetch_add(1, Ordering::SeqCst);
                entry(provider, true)
            },
            policy,
        );
        {
            let mut state = lock(&cache.state);
            state.snapshot = Some(Snapshot {
                entries: vec![entry(ProviderId::Xai, false).entry],
                completed_at: Instant::now() - policy.ttl - Duration::from_secs(1),
                updated_at_unix: 1_800_000_000,
            });
        }

        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: true, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(matches!(
            cache.get(),
            CacheResponse::Complete { stale: true, .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn retry_exhaustion_without_a_good_snapshot_returns_503_during_cooldown() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            move |provider| {
                counted.fetch_add(1, Ordering::SeqCst);
                entry(provider, true)
            },
            policy(Duration::from_millis(300), [Duration::ZERO; 3]),
        );

        assert!(matches!(
            cache.get(),
            CacheResponse::Unavailable { retry_after: 1.. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(matches!(
            cache.get(),
            CacheResponse::Unavailable { retry_after: 1.. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_new_empty_cache_is_cold_and_returns_an_empty_provider_list() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let cache = UsageCache::new(Vec::new(), move |provider| {
            counted.fetch_add(1, Ordering::SeqCst);
            entry(provider, false)
        });
        match cache.get() {
            CacheResponse::Complete {
                snapshot,
                stale: false,
            } => assert!(snapshot.entries.is_empty()),
            _ => panic!("an empty enabled-provider set completes with an empty report"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(lock(&cache.state).snapshot.is_some());
    }

    fn route_context(cache: UsageCache) -> (crate::server::ServerContext, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let keys =
            Arc::new(crate::keys::KeyStore::open(directory.path().join("keys.json")).unwrap());
        let context = crate::server::ServerContext {
            host: "127.0.0.1".to_string(),
            port: 8484,
            api_key: "route-test-key".to_string(),
            keys,
            providers: std::collections::HashMap::new(),
            default_provider: ProviderId::Xai,
            routing: crate::config::Routing::default(),
            upstream_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_upstream_requests: crate::server::DEFAULT_MAX_UPSTREAM_REQUESTS,
            inferences: Arc::new(crate::inference::InferenceState::new()),
            usage_cache: Arc::new(cache),
        };
        (context, directory)
    }

    fn response_header<'a>(response: &'a crate::server::Response, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn the_route_returns_504_then_202_while_one_refresh_continues() {
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let wait_for_release = Arc::clone(&release_rx);
        let cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            move |provider| {
                let _ = recv_release(&wait_for_release);
                entry(provider, false)
            },
            policy(Duration::from_millis(10), [Duration::ZERO; 3]),
        );
        let (context, _directory) = route_context(cache);

        let timed_out = handle(&context);
        assert_eq!(timed_out.status, 504);
        assert_eq!(response_header(&timed_out, "Retry-After"), Some("1"));
        assert_eq!(
            response_header(&timed_out, "Cache-Control"),
            Some(CACHE_CONTROL)
        );

        let pending = handle(&context);
        assert_eq!(pending.status, 202);
        assert!(response_header(&pending, "Retry-After").is_some());
        release_tx.send(()).unwrap();
        assert!(wait_until_idle(
            &context.usage_cache,
            Duration::from_millis(300)
        ));
        assert_eq!(handle(&context).status, 200);
    }

    #[test]
    fn the_route_marks_a_stale_snapshot_and_returns_503_without_one() {
        let policy = policy(Duration::from_millis(300), [Duration::ZERO; 3]);
        let stale_cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            |provider| entry(provider, true),
            policy,
        );
        {
            let mut state = lock(&stale_cache.state);
            state.snapshot = Some(Snapshot {
                entries: vec![entry(ProviderId::Xai, false).entry],
                completed_at: Instant::now() - policy.ttl - Duration::from_secs(1),
                updated_at_unix: 1_800_000_000,
            });
        }
        let (stale_context, _stale_directory) = route_context(stale_cache);
        let stale = handle(&stale_context);
        assert_eq!(stale.status, 200);
        assert_eq!(
            response_header(&stale, "X-Eggpass-Usage-Stale"),
            Some("true")
        );
        assert!(
            response_header(&stale, "X-Eggpass-Usage-Age")
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|age| age >= 600)
        );
        assert_eq!(
            response_header(&stale, "Cache-Control"),
            Some(CACHE_CONTROL)
        );

        let empty_cache = UsageCache::with_policy(
            vec![ProviderId::Xai],
            |provider| entry(provider, true),
            policy,
        );
        let (empty_context, _empty_directory) = route_context(empty_cache);
        let empty = handle(&empty_context);
        assert_eq!(empty.status, 503);
        assert!(response_header(&empty, "Retry-After").is_some());
        assert_eq!(
            response_header(&empty, "Cache-Control"),
            Some(CACHE_CONTROL)
        );
        assert_eq!(handle(&empty_context).status, 503);
    }
}
