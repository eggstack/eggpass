# Codex App-Server Usage Integration — Implementation Plan

**Status:** Implementation complete. Formatting, Clippy, and all 394 workspace tests pass on macOS; the fake app-server covers the documented stdio flow without a live Codex account. Human-readable usage shows remaining percentages, provider-supplied window durations, and reset countdowns. A Windows MSVC cross-check was attempted but could not compile the existing `ring` C dependency because this macOS host lacks Windows C headers.

**Scope:** Add an optional, local Codex app-server source for `eggpass usage` only. The existing Sign in with ChatGPT (SIWC) OAuth mechanism remains the sole authentication mechanism for Eggpass inference and is not changed by this work.

**Research checked:** 2026-11-17 against OpenAI's Codex app-server documentation and protocol source, plus the current Eggpass usage path.

## Goal

When a user runs `eggpass usage`, report ChatGPT rate-limit windows from the user's existing Codex installation when the Codex CLI is installed, its app-server can start, and it has a ChatGPT-managed account. Do not require Eggpass ChatGPT OAuth for this usage-only result. If this optional source cannot answer, preserve the existing Eggpass usage behavior and output.

The integration is a read-only consumer of Codex's documented local JSON-RPC account methods. It must not participate in inference, authorization, refresh-token exchange, account switching, login, or logout.

## Recommended behavior

1. In the OpenAI provider's `usage` collection path, try a local Codex app-server query before the existing Eggpass-token check. This ordering is necessary: `src/cli/commands/usage.rs:159-171` currently returns `NotSignedIn` before reaching the provider collector when Eggpass itself has no OpenAI token.
2. If Codex returns a valid ChatGPT rate-limit snapshot, convert it to the existing normalized `UsageReport` and report its source as `Codex app-server`. Make that source visible in both terminal and JSON output; the Codex account may differ from the account used for Eggpass inference. Do not print the Codex account email or other identity details.
3. If Codex is absent, unauthenticated, configured for a non-ChatGPT auth mode, incompatible, times out, exits, or returns an error/malformed response, fall through to the existing collector and exit behavior without turning the optional lookup into a command failure:
   - If Eggpass has its own OpenAI token, retain the existing `Unavailable` result and message from `src/provider/usage/openai.rs:63-72`.
   - If Eggpass has no OpenAI token, retain the existing `NotSignedIn` block and exit semantics from `src/cli/commands/usage.rs:95-106,192-202`.
   - Render provider results through the shared human-readable formatter, which shows remaining percentages and reset countdowns where available.
4. Query only when the OpenAI provider is included in the command. Leave xAI collection, inference, and all non-usage commands unchanged.

## App-server protocol flow

Use the installed `codex` executable directly, without invoking a shell:

```text
codex app-server
```

The official default transport is stdio with newline-delimited JSON-RPC messages. A minimal session is:

1. Spawn the process with piped stdin/stdout; keep stderr separate from protocol output.
2. Send `initialize` with a stable Eggpass `clientInfo` name/title/version and wait for the matching response.
3. Send the `initialized` notification.
4. Send `account/read` with `refreshToken: false`; proceed only when `result.account.type` is `chatgpt`. Do not display the returned email. This is a mode check, not a login request.
5. Send `account/rateLimits/read` and wait for the matching response. No thread or turn is needed.
6. Close stdin and terminate/reap the child when the request finishes.

Use normal account methods without opting into `experimentalApi`. Match responses by JSON-RPC `id`, tolerate/ignore unrelated notifications, and treat JSON-RPC errors as an unavailable optional source. Bound startup/RPC time, stdout line size, and total response size. On timeout or protocol failure, kill/reap the child and continue through Eggpass's existing fallback. Do not let a blocking pipe read hang `eggpass usage` indefinitely; implement a deadline-safe reader/process boundary and test it on supported platforms.

Do not read or parse `$CODEX_HOME/auth.json`, copy Codex credentials, pass Eggpass SIWC tokens into Codex, call `account/login/*` or `account/logout`, or make direct requests to ChatGPT's internal usage endpoints. The app-server itself owns its local Codex authentication and any normal token lifecycle. Avoid exposing raw app-server output, stderr, credentials, or RPC errors in terminal output or logs.

## Mapping rate-limit results

OpenAI documents these fields in `account/rateLimits/read`:

- `rateLimitsByLimitId`: optional map of metered buckets, for example `codex`.
- `rateLimits`: backward-compatible single-bucket view.
- Each bucket can include `primary` and `secondary` windows with `usedPercent`, `windowDurationMins`, and `resetsAt`.
- Buckets may also include `planType` and `rateLimitReachedType`.

Mapping rules:

- Prefer a non-empty `rateLimitsByLimitId` map and do not also emit its duplicate top-level `rateLimits` view. If the map is absent or empty, use `rateLimits`.
- Preserve all returned buckets and distinguish them in output. Human-readable rows use the supplied duration (for example, `5-hour` or `7-day`) instead of a generic primary/secondary label; keep the bucket name when multiple buckets would otherwise be ambiguous.
- Map `usedPercent` to the existing optional percentage, `resetsAt` to Unix seconds, and `windowDurationMins` to seconds with checked conversion. Do not assume a fixed quota period or hard-code a reset schedule.
- Missing or `null` fields mean unavailable, never zero. If no usable quota data is present, fall back to current behavior rather than displaying a zero-usage report.
- Map plan/state fields only where their meaning matches the existing model. Do not collapse distinct workspace-credit or usage-limit states into a misleading generic state; extend the normalized representation only if required to preserve the server's meaning.
- Add separate source metadata to the usage entry rather than reusing `caption`, which describes which pool the reported quota belongs to. Keep the JSON addition optional/additive when the source is not Codex.

The existing `UsageReport`/`QuotaWindow` model already represents optional percentages, reset timestamps, and durations (`src/provider/usage/mod.rs:32-75`). The formatter uses supplied durations as inline window labels, keeps provider labels when duration is missing, shows remaining percentages and future reset countdowns, and leaves the JSON `used_percent` data unchanged. Bucket names are included when multiple Codex buckets need disambiguation; xAI collection semantics remain unchanged.

## `account/usage/read` scope

`account/usage/read` is documented as ChatGPT token-activity summaries and optional daily token buckets (for example lifetime tokens, peak daily tokens, and per-day tokens). It is not quota remaining or a rate-limit window. It is not required to solve the current missing-quota problem, so defer it from the first implementation unless a separate product decision expands `eggpass usage` to include a distinct token-activity section. If added later, keep it separate from quota percentages, preserve nullable/missing values, and update terminal/JSON output and tests explicitly.

## Implementation stages

### Stage 0 — Confirm local process compatibility

- Confirm the supported Codex CLI executable invocation and stdio behavior on the project's supported operating systems; use `codex app-server` with default stdio transport.
- Prototype the initialize/initialized/account-read/rate-limits exchange against a test/fake app-server process. Do not inspect or alter any user's Codex credentials and do not require a live account in CI.
- Resolve process deadlines, child cleanup, executable-not-found behavior, and bounded stdout/stderr handling before integrating the query.
- Use existing `serde_json` and standard process/I/O facilities where practical; add a dependency only if a safe, portable timeout/reader implementation requires it.

**Exit:** A bounded helper can complete the RPC flow, distinguish ChatGPT from unsupported/no account, and cleanly terminate the child on success and failure.

### Stage 1 — Integrate the OpenAI usage fallback

- Add a small app-server usage adapter under the provider usage code. Keep process launching and JSON-RPC concerns out of the inference provider implementation.
- Clarify the existing OpenAI collector's module comments as the SIWC-only fallback; do not leave wording that implies the new app-server source is impossible or unavailable.
- Call it for OpenAI usage before the Eggpass token short-circuit. If it returns no valid report, execute the current token-check/provider-collector logic unchanged.
- Convert bucket windows to the current report model; add optional source labeling to text and JSON output. Clearly identify that the result came from the Codex account, which may not be the Eggpass inference account.
- Do not change OAuth configuration, credentials, token storage, auth commands, `ProviderRuntime` inference behavior, upstream requests, or xAI usage.

**Exit:** A Codex-authenticated ChatGPT account can supply usage when Eggpass OpenAI OAuth is absent; all unavailable cases retain existing output/exit behavior.

### Stage 2 — Test the optional boundary and regressions

Use a fake `codex` executable or injected executable path to test deterministic protocol behavior without network/account access:

- executable missing; spawn failure; early exit; startup/RPC timeout; malformed/oversized JSONL; total-output limit; JSON-RPC error; missing or wrong response ID; notification bursts without dropping responses;
- successful initialize handshake and both account RPCs;
- ChatGPT account, no account, API-key account, and another unsupported mode;
- legacy `rateLimits` only; populated and empty `rateLimitsByLimitId`; primary/secondary windows; multiple buckets; null/missing values; zero percent; reset and duration conversion; unknown plan/state variants;
- no false zero report and no duplicate reporting of `rateLimits` plus the bucket map;
- fallback to the exact current OpenAI response when Codex is missing/unusable, both with and without Eggpass OpenAI credentials;
- Codex usage is reported without Eggpass OpenAI credentials, so `eggpass usage --provider openai` does not incorrectly return the current not-signed-in exit code;
- source labeling in text and JSON; no email, token, raw child output, or credential material in output/logs;
- existing xAI usage tests and OpenAI inference/auth tests remain unchanged and pass.

**Exit:** All new tests use a fake process and no live service. Existing CLI, OAuth, and inference tests pass; `git diff --check`, formatting, Clippy, and workspace tests pass.

## Out of scope

- Replacing, extending, or otherwise changing Eggpass's current SIWC OAuth mechanism for inference.
- Supplying Eggpass tokens or account identifiers to Codex app-server.
- Any Codex login, logout, account-switching, or credential-management UI/flow.
- Direct calls to undocumented ChatGPT backend endpoints or reading Codex credential files.
- Making inference depend on Codex being installed or signed in.
- Adding `account/usage/read` token activity to the initial quota-focused implementation.
- Changes to release workflows, package publication, daemon behavior, or xAI usage.

## Acceptance checklist

- [x] OpenAI `usage` can query the local Codex app-server without requiring Eggpass OpenAI OAuth.
- [x] `codex app-server` is optional; missing, unauthenticated, unsupported, incompatible, malformed, or timed-out Codex falls back to current Eggpass behavior.
- [x] Codex reads are limited to account state and documented usage/rate-limit read methods; no login/logout or direct private HTTP endpoint is used.
- [x] Usage output clearly labels the Codex source and buckets, without exposing account email or secrets.
- [x] `account/usage/read` remains deferred unless separately approved as distinct token-activity output.
- [x] Eggpass inference OAuth, token storage, provider requests, xAI usage, and non-usage commands are unchanged.
- [x] Fake-process tests cover protocol success/failure, fallback behavior, and output safety; full existing Rust checks pass.

## References

Official OpenAI documentation and schema reviewed 2026-11-17:

- [Codex App Server protocol and stdio transport](https://developers.openai.com/codex/app-server.md) — JSONL/JSON-RPC framing, `initialize` then `initialized`, local CLI startup, account methods, and the usage response examples.
- [Codex app-server account protocol source](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/src/protocol/v2/account.rs) — response structures for rate-limit buckets/windows and token usage summary.

Repository context:

- `src/cli/commands/usage.rs:53-118` — provider collection, output, and all-not-signed-in exit behavior.
- `src/cli/commands/usage.rs:159-171` — current Eggpass token check before provider usage collection.
- `src/cli/commands/usage.rs:186-232` — current unavailable/not-signed-in terminal rendering.
- `src/provider/usage/openai.rs:57-72` — existing ChatGPT unavailable result and fallback text.
- `src/provider/usage/mod.rs:32-75,161-192` — normalized report, quota window, and provider result types.
- `Cargo.toml:63-69` — current `serde_json` and Tokio dependencies; do not make runtime dependencies a prerequisite for inference.
