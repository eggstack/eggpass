# On-Demand Daemon Usage API — Implementation Plan

**Status:** Implemented; final automated checks and installed-binary smoke test passed.

**Implementation and verification:** Added the authenticated in-memory usage API, enabled-provider reporting shared with the CLI, finite timeout retries, freshness metadata, tests, and endpoint documentation. `cargo fmt --all`, `cargo check --workspace --all-targets --locked`, `cargo test --workspace --all-targets --locked`, strict workspace Clippy, and `git diff --check` passed (368 Eggpass tests, 30 auth-crate tests, and 10 OAuth integration tests). Installed from this checkout with `cargo install --path . --locked --force`; `/Users/davidbowman/.cargo/bin/eggpass --version` reports `0.1.1`. An isolated installed-binary smoke test verified unauthenticated `401`, authenticated `200` with `{"providers":[]}` when both providers are disabled, `Cache-Control`/age/update headers, and reuse of the cached timestamp. The smoke test made no live provider calls.

**Research checked:** 2026-11-18 against the current `eggpass` source and docs.

**Scope:** Add an authenticated daemon endpoint that returns the same machine-readable usage report as `eggpass usage --json`, with both paths collecting only providers enabled in configuration. Collect only when requested, retain one recent report in daemon memory for a 10-minute TTL, and allow only a finite, demand-triggered background retry job after a client wait timeout; no scheduled/background polling or file persistence.

## Goal

Provide Gregg and other authorized clients a stable JSON usage endpoint without letting frequent polling cause frequent provider queries. For a given cache snapshot, requests inside the TTL reuse the same report. Once it is absent or expired, one request triggers a refresh; concurrent misses share that refresh. The cache disappears on daemon shutdown/restart.

The initial endpoint is proposed as `GET /v1/usage`. It must not expose provider credentials, account email, or raw upstream payloads.

## Repository baseline and implementation seams

- `src/cli/commands/usage.rs:127-180` currently builds a report for all `ProviderId`s by default, adds each provider's source/caption and last-inference status, then prints either human-readable text or JSON. The JSON wrapper is `{"providers":[...]}`. The CLI's `ProviderEntry` and `UsageReportJson` types and collection functions are private (`src/cli/commands/usage.rs:211-299`). The selected behavior is to change the CLI and API collection to include only providers enabled in configuration.
- The serialized provider outcomes are already structured and reusable: `ProviderUsage` has `reported`, `not_signed_in`, `unavailable`, and `failed` states, and `UsageReport` carries normalized windows, breakdown, credits, and state (`src/provider/usage/mod.rs:33-48,162-193`). xAI's caption explains which billing pool the numbers represent.
- xAI collection calls the billing endpoint, then best-effort fetches settings for the plan name (`src/provider/usage/xai.rs:71-153,188-206`). Those requests use the configured provider timeouts. The defaults allow up to 30 minutes for the global/body timeout (`src/config.rs:252-274`; `src/upstream.rs:138-148`).
- OpenAI usage first tries the local `codex app-server`, then falls back to the Eggpass Sign in with ChatGPT collector (`src/cli/commands/usage.rs:252-282`). The Codex helper has an eight-second session deadline (`src/provider/usage/codex_app_server.rs:21-38`). The SIWC fallback does not query quota; it returns an explicit unavailable result (`src/provider/usage/openai.rs:45-61`). A daemon using this path needs `codex` available on its `PATH` to return the optional Codex result, and that account may differ from the Eggpass inference account.
- The daemon already keeps last inference statuses in memory (`src/server/mod.rs:89-94`, `src/inference.rs:10-35`) and serves them at `/v1/last-inference` (`src/server/inference.rs:19-27`).
- Routes are added in `src/server/router.rs:5-45`; request routing and authorization are in `src/server/mod.rs:231-335`. Only `/health` and `/v1/last-inference` are exempt from API-key authorization. The authority guard remains active for them as well (`src/server/guard.rs:20-48`). The HTTP server handles connections on separate threads (`src/server/http.rs:197-245`).
- `ServerContext` currently has no usage cache, full config, or paths (`src/server/mod.rs:60-94`). The daemon creates `ProviderHandle`s only for enabled providers (`src/cli/commands/run.rs:198-215`), while the CLI usage command currently iterates all providers. The selected behavior is to change the CLI to use the daemon's enabled-provider scope; when OpenAI/ChatGPT is disabled, do not invoke the optional Codex usage process. A shared collector must not create independent `TokenManager`s for enabled providers: separate refresh mutexes could race while rotating the same credentials.
- The historical shipped plan notes that `GET /v1/usage` was deferred when no reliable usage path existed (`plans/shipped-2026-11-13.md:161-163`). The CLI usage collectors now provide the report path this API can reuse.

## Proposed first-release behavior

1. **Route and methods:** Add `GET /v1/usage`. Existing path normalization means `/usage` also resolves consistently for clients whose base URL already ends in `/v1`. Other methods return the existing `405` response with `Allow: GET`. Treat this as a bodyless GET in the request-body gate (`src/server/mod.rs:240-249`).
2. **Authorization:** Require a valid Eggpass client key. Do not add this route to the unauthenticated probe exceptions. Reject unauthorized requests before consulting the cache or starting collection. Preserve the existing Host/authority guard.
3. **Response compatibility and provider scope:** Reuse the CLI JSON schema: top-level `providers` array, provider outcome/status, optional source, caption, and last-inference field. Change both the CLI and API to collect only providers enabled in configuration; omit disabled providers from the array and do not invoke Codex when OpenAI is disabled. Return HTTP 200 when a report is complete, including provider-level `failed`, `unavailable`, or `not_signed_in` outcomes. A request that reaches its refresh wait deadline before a report is ready returns a timeout response instead.
4. **Cache policy:** Keep one most-recent complete usage snapshot and its completion time in process memory, shared through `ServerContext`. Use `Instant` for expiry and a 600-second TTL from a completed collection; structured provider outcomes such as unavailable or non-timeout failures are still complete results. Do not persist the report or add a periodic polling task. A restart starts cold.
5. **Demand-triggered refresh and client deadline:** A cache miss/expiry starts one refresh job. The initiating request waits at most 30 seconds; if the refresh is still running, return an HTTP timeout to that client while allowing the already-started job to continue. Do not hold the cache mutex while provider calls or process startup run. No client timeout may start a duplicate job.
6. **Finite background retries:** If a provider collection times out, keep the same demand-triggered job alive and retry only that provider, up to three times after the initial attempt, with delays of 5, 15, and 30 seconds. Do not repeat successful provider collections. If retries succeed, atomically store the completed report and start its 600-second TTL then. This is a finite retry sequence, not scheduled/background polling. If the client wait expires, return 504; later requests during the active job return 202 with `Retry-After`. On exhaustion, preserve the last good report as stale and apply a 600-second cooldown; if no good report exists, return 503 with `Retry-After`.
7. **Last inference:** Attach the daemon's current `InferenceState` snapshot when serializing each response. The CLI obtains last-inference information afresh (`src/cli/commands/usage.rs:412-449`); caching the complete assembled JSON would make that field stale for up to ten minutes.
8. **Freshness and HTTP caching:** Keep the JSON body compatible and return `Cache-Control: private, no-store`. Add `X-Eggpass-Usage-Age` as integer seconds and `X-Eggpass-Usage-Updated-At` as an RFC 3339 UTC timestamp. Add `X-Eggpass-Usage-Stale: true` when returning a previously successful snapshot after refresh retries have been exhausted.
9. **Bounded collection:** Use a 30-second client wait deadline independent of the existing provider timeout defaults, which allow up to 30 minutes for an xAI request (`src/config.rs:252-274`; `src/upstream.rs:138-148`). The background retry job must also be finite and must not keep a request handler occupied after the client deadline.

## User-selected decisions

On 2026-11-18, the user selected:

- **Provider scope:** Query only providers enabled in configuration. Change the CLI usage command to match the API; omit disabled providers from the `providers` array. If OpenAI/ChatGPT is disabled, do not invoke the Codex usage process.
- **Client wait deadline and retries:** Give a client at most 30 seconds to wait for a cache-miss/expired-cache refresh. If the response deadline expires, return a timeout to that client but continue the same finite refresh job in the background, with up to three spaced retries after a timeout. Store the result in memory if a retry succeeds. The retry job is demand-triggered and must not become periodic polling.
- **Freshness metadata:** Preserve the CLI-compatible JSON body and add `X-Eggpass-Usage-Age` (integer seconds) and `X-Eggpass-Usage-Updated-At` (RFC 3339 UTC timestamp) response headers. Also set `Cache-Control: private, no-store`.

## Confirmed timeout/retry policy

On 2026-11-18, the user selected the following details:

- One initial attempt plus up to three retries (four attempts maximum), with retry delays of 5, 15, and 30 seconds. Retry only the provider whose collection timed out; do not repeat successful provider collections.
- The request that starts a cold/expired refresh waits up to 30 seconds, then returns HTTP 504 if the report is still being refreshed. The demand-triggered refresh job continues independently of that response.
- A later request while the same job is active returns HTTP 202 with `Retry-After`, rather than holding another request thread open or starting another refresh.
- If retries are exhausted, retain the previous successful report as stale and start an in-memory 600-second retry cooldown. Return the stale report with its age/update headers plus `X-Eggpass-Usage-Stale: true`. If no successful report exists, return HTTP 503 with `Retry-After` during the cooldown.

The 600-second success-cache TTL begins when a new report is stored. The separate cooldown begins after the final failed attempt. Neither a timeout response nor a pending response writes to disk or starts periodic polling.

## Implementation stages

### Stage 0 — Confirm API and cache contract

- Implement the selected `GET /v1/usage` contract: API-key authentication, enabled providers only in both CLI and API, 30-second client wait, finite timeout retries, freshness headers, and a 600-second in-memory success-cache TTL.
- Implement the confirmed retry count/spacing, 202 pending response, stale-snapshot, and 600-second cooldown behavior after retries are exhausted.
- Keep the implementation process-local and single-snapshot; no database, file, cross-process cache, or periodic refresh scheduler.

**Exit:** The route, report provider set, freshness contract, retry lifecycle, failure behavior, and latency bounds are explicit.

### Stage 1 — Extract reusable usage collection/report assembly

- Move the serializable report DTO and provider collection/assembly logic out of the CLI-only private implementation into a shared usage service or provider-usage module.
- Preserve the CLI JSON schema, source labels, xAI caption, provider order, and Codex-first OpenAI behavior for providers enabled in configuration.
- Change the CLI's provider selection to filter by `config.providers.get(id).enabled`; disabled providers are omitted, and a disabled OpenAI provider must not start `codex app-server`.
- Provide daemon collection access to the necessary configuration and provider credentials without duplicating `TokenManager` refresh locks.
- Keep `last_inference` supplied from current daemon state at response time.

**Exit:** CLI tests preserve the existing JSON/text formatting for enabled providers while explicitly covering the new disabled-provider filtering; the shared collector can be called from both CLI and daemon paths without live-account requirements in tests.

### Stage 2 — Add the daemon route and TTL cache

- Add the `Usage` route and `GET` dispatch; make it bodyless and subject to existing authority and client-key checks.
- Add a small cache state to the shared server context: optional last successful snapshot, monotonic completion time, one demand-triggered refresh job, in-progress coordination, and a retry cooldown.
- Implement cache-hit, cold-miss, expiry, single-flight, and 30-second client wait behavior. A timed-out client does not cancel the finite retry job; later requests while it is active receive `202` with `Retry-After`. Keep provider calls outside the cache lock and avoid logging report bodies or credentials.
- Preserve completed provider outcomes in the active refresh candidate and retry only timed-out providers, up to three retries after the initial attempt (5/15/30 seconds). Do not repeat successful provider collections. Commit the complete candidate atomically on success; after exhaustion, retain the last complete report as stale with a 600-second cooldown, or return `503` with `Retry-After` if no good report exists. Set `Retry-After` to the next scheduled retry or the remaining cooldown, as applicable.

- Serialize the shared report in the existing schema and add `Cache-Control: private, no-store`, age/update headers, and a stale marker on stale responses.

**Exit:** Fresh requests are served from memory; a miss starts one finite refresh job; concurrent misses do not duplicate provider calls; the 504/202/stale/503 behavior is implemented; no periodic refresh or disk activity is introduced.

### Stage 3 — Test behavior without live provider calls

- Unit-test the cache/refresh worker with an injected collector and controllable clock: cold miss, fresh hit, expiry, 30-second caller timeout without job cancellation, spaced retries, single-flight across concurrent callers, success timestamp/TTL, retry exhaustion, and empty cache after a new daemon instance.
- Extend server integration tests for path routing, `GET`/405 behavior, bodyless handling, required key, Host guard, JSON shape, freshness headers, 202/504/503 responses and `Retry-After`, and no collection on unauthorized requests.
- Use fake upstream/collector seams to assert the number of billing/settings requests across cold, fresh, timeout, retry, and expired requests. Existing xAI collection uses hard-coded live endpoints, so provide injection for tests instead of calling those endpoints.
- Cover OpenAI's Codex success/fallback behavior using its existing fake-process pattern; assert that disabled OpenAI does not launch Codex and that no email, token, or raw upstream/process payload appears in the API body or logs.
- Verify `last_inference` freshness behavior, provider-level failure responses, CLI filtering to enabled providers, and preservation of the CLI JSON schema/format for enabled providers. Test 504 on the initial client deadline, 202 plus `Retry-After` while the shared job remains active, stale-data headers/cooldown after retry exhaustion, and 503 plus `Retry-After` when no prior report exists.

**Exit:** Cache and route tests are deterministic, parallel expiry tests prove one refresh, and no test depends on a live account or external provider service.

### Stage 4 — Document and review

- Document the endpoint, response examples, key requirement, 10-minute report TTL, 30-second client wait, finite timeout-retry schedule, 202/504/503 behavior, freshness/stale headers, cooldown, restart behavior, and the optional Codex account distinction.
- Update current endpoint and security references in `docs/configuration.md`, `docs/how-it-works.md`, `docs/commands.md`, and any maintained API route list.
- Run formatting, Clippy, workspace tests, relevant server/usage tests, and `git diff --check`; review the scoped diff and preserve unrelated worktree changes.

**Exit:** API docs agree with route/auth behavior; checks pass; no unrelated changes are staged or overwritten.

## Acceptance checklist

- [x] `GET /v1/usage` returns the CLI-compatible `providers` JSON shape for enabled providers only.
- [x] The CLI also collects only providers enabled in configuration; disabled OpenAI never invokes Codex, and disabled providers are omitted from the report.
- [x] Route requires an Eggpass key and keeps the authority guard; unauthorized requests never collect usage.
- [x] A cold/expired request waits at most 30 seconds; on expiry the client receives 504 while the single refresh job continues.
- [x] Timed-out provider collections get at most three retries after the initial attempt, at 5/15/30-second spacing; only the timed-out provider is retried, and success stores the complete report with a 600-second TTL.
- [x] Later requests during an active refresh receive 202 with `Retry-After`; they neither block another server thread nor start duplicate upstream collection.
- [x] After exhausted retries, a previous report is returned as stale with age/update/stale headers during a 600-second cooldown; with no prior report, the API returns 503 with `Retry-After`.
- [x] Response headers expose age and update time; the body remains CLI-compatible and uses no-store HTTP caching.
- [x] Usage data exists only in daemon memory; there is no scheduled or open-ended polling, disk persistence, or forced-refresh endpoint.
- [x] Last-inference values are fresh per response.
- [x] Tests use controlled collectors/stubs; no live account/provider service is required.
- [x] CLI formatting/schema remains compatible for enabled providers; docs describe provider filtering, auth, cache/retry behavior, latency, headers, and Codex account sourcing.

## Out of scope

- Changing provider OAuth, inference, token storage, or the `eggpass usage` command's meaning.
- Periodic or open-ended provider polling, writing usage snapshots to files, or retaining usage history. A finite set of spaced retries is in scope only for a refresh already triggered by an API request.
- Adding a public unauthenticated usage route, force-refresh query parameter, or configurable TTL in the first version.
- Adding OpenAI quota support through undocumented ChatGPT endpoints; the existing SIWC fallback remains unavailable unless Codex app-server supplies a report.
- Inspecting or changing the `eggstack/gregg` repository as part of this daemon-only implementation plan.
