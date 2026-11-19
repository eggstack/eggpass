# Reuse the daemon usage cache from the CLI

**Status:** Local implementation and validation are complete. Windows MSVC tests (including the current-user-only usage-key ACL test) and the Windows release build passed in [CI run #44588788169](https://github.com/eggstack/eggpass/actions/runs/44588788169) at commit `34ec6f52a2d42d10fbdef07e3d5ad5ef01f75162`.

## Goal

Change `eggpass usage` to prefer the daemon's existing cached usage report, while retaining today's direct provider collection when the daemon is unavailable. Give the CLI a separate, daemon-managed usage credential that can read only the usage endpoint. Do not change the meaning or scope of regular Eggpass API keys.

This is a CLI/authentication integration plan, not a plan to build another usage cache. The authenticated `GET /v1/usage` route, its in-memory 600-second cache, single-flight refresh, finite retries, response shape, and freshness headers already exist in the current source. The CLI currently collects provider usage directly on each run and only makes a separate best-effort daemon request for last-inference state.

## Research findings and design decision

- The CLI collection is in `src/cli/commands/usage.rs:129-178, 246-284`; its report DTO and enabled-provider selection are in `src/provider/usage/report.rs`. Existing output includes provider results, source/caption, and last-inference status.
- The daemon cache and `GET /v1/usage` response are in `src/server/usage.rs`. The route is registered in `src/server/router.rs`, and request auth/dispatch is centralized in `src/server/mod.rs`.
- All protected routes currently require a configured regular key. The daemon also refuses to start without one (`src/cli/commands/run.rs`). Issued regular keys are salted hashes in `src/keys.rs`; the daemon verifies them through the shared auth path. Preserve these properties.
- Paths are rooted in the resolved Eggpass data directory. A `--config` override changes only the config file path, not the key store or other runtime files (`src/paths.rs`). The existing Unix private writer uses atomic replacement and 0700/0600 modes, but the current permission helper is a no-op on Windows. A new secret cannot rely on that no-op as adequate Windows protection.
- **Recommendation:** use a separate, high-entropy runtime secret and authorize it by exact match only for `GET /v1/usage`. Do not infer its scope from a prefix/suffix, strip a marker, or insert it into the regular key store. A recognizable prefix is unnecessary; if ever used for display, it is only a label and must not affect authorization.
- Keep the secret in a dedicated runtime file under the resolved data directory, not in `config.toml`, the ordinary key store, command output, or logs. Rotate it for each daemon start. The CLI reads it locally and sends it in an Authorization Bearer header; do not put it in the URL/query string.

## Security and compatibility contract

1. **Least privilege:** the usage secret is accepted only for a bodyless `GET` to the usage route. It must fail authentication for inference, model, health/other, and any other route, including when an illustrative marker is removed or altered. Compare the entire token; never strip characters and never dispatch to a usage-key lookup solely because a candidate has a prefix.
2. **Regular keys unchanged:** regular config keys and issued keys retain their current accepted transports and full route scope. A valid regular key continues to work on the usage endpoint and every route it works on today. Do not change regular-key verification, listing, labels, hashing, or revocation behavior.
3. **No elevation of daemon startup:** the usage secret does not count as a configured client key, is not returned by key-list commands, and cannot satisfy `require_key`. Preserve the current fail-closed behavior when there are no regular keys, including for loopback clients.
4. **No loopback exception:** keep the existing authority/Host guard and authenticated-route checks. The usage token is not a reason to exempt localhost or weaken auth.
5. **Credential handling:** generate at least 256 bits of cryptographically secure randomness. Store the complete token only in its dedicated private file and in daemon memory. Use exact constant-time comparison. Do not log it, include it in errors, or expose it through a command, API response, config dump, debug output, or key listing. Treat usage data itself as sensitive even though the token has narrower access.
6. **File security and lifecycle:** add a path accessor such as `Paths::usage_key_file()`, rooted beside other Eggpass runtime files and unaffected by `--config`. Write atomically; create the Unix file as 0600 inside the 0700 data directory. On Windows, establish and test a current-user-only ACL for both directory and file; fail daemon startup closed if user-only protection cannot be established. Do not treat the existing non-Unix permission no-op as protection. Rotate/replace the token before the daemon reports ready. On graceful shutdown, remove the file only if it still contains this daemon's token; a crash may leave a stale file, but the next start must replace it before serving. A stopped daemon's leftover file must not authorize anything.
7. **Header scope:** accept the usage secret only from the Authorization Bearer header used by the CLI. Do not accept it from a query parameter. Keep existing regular-key header/query behavior unchanged.

## CLI behavior

Refactor `eggpass usage` into a daemon-attempt phase followed by the existing direct-collection phase. Keep formatting, JSON stdout, exit status rules, provider selection, and direct collector behavior compatible.

- Resolve daemon state using the existing pid/host/port mechanism. If it is stopped or stale, use the existing direct provider collector without attempting the daemon request.
- If the daemon is running, read the dedicated usage-secret file. If the file is missing or cannot be read, use direct collection; do not print the secret or a file's contents in an error.
- Request `GET /v1/usage` with the secret as a Bearer header. Use bounded connect/overall timeouts appropriate for a local cache endpoint and status-aware HTTP handling. Never place the secret in a URL.
- On HTTP 200, parse the existing CLI-compatible JSON shape into the same report model and use the existing render/JSON/exit-result path. Respect an explicit provider selection by selecting that provider from the response. The daemon's fresh last-inference field should be used rather than making a second request. Preserve the JSON body shape and stdout; if the response has `X-Eggpass-Usage-Stale: true`, explain that cached data is stale on stderr only.
- On HTTP 404, use direct collection for compatibility with a running daemon that does not implement this route.
- If the request has a connection failure, recheck daemon state. Use direct collection only if that recheck confirms the daemon stopped or stale. If it still appears to be running, report the daemon communication failure instead of silently bypassing its cache.
- Do **not** fall back to direct provider calls for HTTP 202, 503, or 504. Those statuses represent an active refresh or cooldown, and fetching providers directly would bypass the daemon's single-flight/retry policy. Surface an actionable CLI error while preserving the HTTP status/retry context where practical.
- Treat HTTP 401/403 and malformed successful responses as errors, not as permission to silently fall back. This makes a stale/mismatched secret visible and prevents fallback from hiding an authentication/configuration defect. Do not retry by stripping a prefix or trying variants of the secret.
- A valid regular API key is not needed by the CLI for the daemon request; the scoped secret is sufficient for this route only. Direct fallback continues to use the existing local provider credential flow, not the scoped secret.

## Implementation stages

### Stage 1 — Add the runtime secret lifecycle

- Add the dedicated path to `Paths`; document that config-file overrides do not relocate it.
- Add a focused secret-file helper or safely extract/reuse the private atomic-file writer without changing regular-key semantics. Use an unpredictable temporary filename opened with create-new semantics; avoid following attacker-planted temporary symlinks. Apply restrictive permissions/ACL before the secret can be read by another process.
- Generate a new URL-safe opaque token from a cryptographic RNG for each daemon launch. Do not add a prefix-based auth class. Atomically write the token, load the same exact bytes into server context, and ensure startup cannot report ready if creation/protection fails.
- Remove the matching secret on graceful shutdown without deleting a different daemon's rotated token. Do not persist or modify a config field.

**Exit:** the daemon has one high-entropy runtime secret, stored only in the dedicated protected file and process memory; rotation/restart and shutdown behavior are defined and tested on supported platforms.

### Stage 2 — Add route-specific authentication

- Keep common authority validation and regular-key checks as they are. Add a separate exact-match usage-secret check in the shared auth path, gated by both the usage route and GET method, and by Authorization Bearer presentation only.
- Ensure the regular-key-present guard still executes: the runtime usage secret must not satisfy startup or `keys_configured` requirements and must not open a server whose regular keys have been removed.
- Ensure other methods/routes do not become authorized via the usage secret. Do not change the unauthenticated health/last-inference exceptions.

**Exit:** existing normal-key behavior is unchanged; only the exact current usage secret can authorize GET usage, and it cannot authorize inference or any other route.

### Stage 3 — Prefer the cached daemon response in `eggpass usage`

- Add a small typed result for the daemon attempt (use cached report, fall back to direct collection, or return an error) so status handling is explicit and testable.
- Reuse the daemon-state and local HTTP patterns, but send the scoped Bearer key and request the existing endpoint. Add bounded transport timeouts and response parsing without logging credentials or provider payloads.
- Feed a successful daemon report through the existing CLI render/JSON and exit-code logic. Keep stdout exactly in the current schema; send stale-cache notice to stderr. Preserve provider override behavior and use last-inference already present in the API report.
- Keep direct collectors untouched as the fallback path. Apply the state-recheck rule on connection failure and never bypass 202/503/504.

**Exit:** a live daemon's complete or stale cached response avoids provider calls; unavailable/unsupported daemon cases retain direct checks; pending/cooldown/auth failures have explicit non-bypass behavior.

### Stage 4 — Deterministic security, fallback, and regression tests

- **Secret files:** test resolved path behavior, config override separation, rotation, atomic replacement, restrictive Unix modes, Windows current-user ACL, failure-closed startup, and matching-token cleanup. Ensure no test output includes token bytes.
- **Authorization:** integration-test the scoped key on GET usage, and rejection on inference/models/other routes and wrong method. Test wrong/modified token and a token with any illustrative prefix removed; assert route scope comes from exact full-token matching, not parsing. Test that regular configured and issued keys still work unchanged. Verify the scoped token is absent from key listing and does not let a no-regular-key daemon start or serve protected routes. Retain authority/Host guard tests for loopback.
- **CLI HTTP behavior:** use a local stub server and injected collector counter. Assert HTTP 200 uses the cached report and makes zero direct provider calls; stale 200 warns only on stderr; 404 uses direct collection; stopped/stale daemon and missing key use direct collection; a connection failure falls back only after a stopped/stale recheck; a still-running connection failure, 401/403, malformed body, 202, 503, and 504 do not call direct collectors. Verify response timeout, headers, key transport, and absence of the secret from URL/log/error output.
- **Output compatibility:** test unchanged text and JSON stdout shape, provider filtering, exit-code semantics, and last-inference data for daemon and direct paths. Ensure stderr notices do not contaminate piped JSON.
- Use fake/local responses only; no provider credentials, live provider requests, real daemon network, or secret-bearing test snapshots are required.

**Exit:** all new behavior is covered without live accounts or providers, including the security boundary and every fallback branch.

### Stage 5 — Documentation and validation

- Document that `eggpass usage` uses the daemon cache when available, that a separate private usage-only runtime key is generated/rotated, that it is not a regular API key, and that regular keys remain unchanged. Document stale/pending/cooldown and direct fallback behavior without exposing implementation tokens or implying that usage data is non-sensitive.
- Update current daemon usage/auth and CLI command documentation; keep API response docs consistent with the already-implemented cache route.
- Run formatting, workspace tests, strict Clippy, and `git diff --check`; add targeted server/auth/CLI tests. The earlier Windows run #25 lacked `src/usage_key.rs`; the current run #44588788169 at `34ec6f52a2d42d10fbdef07e3d5ad5ef01f75162` passed the Windows ACL test and release build. Review only the scoped diff and preserve unrelated worktree changes; do not include unrelated files.

**Exit:** docs match actual behavior; local formatting, workspace tests, Clippy, and diff checks pass. Windows ACL validation is complete: the Windows MSVC tests and release build passed on CI run #44588788169 for commit `34ec6f52a2d42d10fbdef07e3d5ad5ef01f75162`.

## Acceptance checklist

- [x] CLI prefers the daemon's existing cached usage report when the daemon is reachable and the scoped credential is usable.
- [x] Direct provider collection remains the fallback when daemon is stopped/stale, route is unsupported, or scoped key is absent/unreadable; connection errors fall back only after confirming the daemon is stopped/stale.
- [x] HTTP 202/503/504 never trigger direct provider calls; authentication/parse errors are visible and do not trigger a silent bypass.
- [x] The usage-only secret is random, private, rotated per daemon start, and separate from config and regular keys.
- [x] Exact secret grants only GET usage; it cannot authorize inference, another route, or wrong method, even if any illustrative prefix/suffix is removed.
- [x] Regular client-key scope/behavior, key listing/revocation, startup key requirement, and loopback/authority protections are unchanged.
- [x] Windows file/directory access is protected by a current-user-only ACL; inability to protect the secret fails closed.
- [x] CLI text and JSON stdout remain compatible; stale-cache information is written to stderr only.
- [x] Tests cover security and fallback behavior without live provider access; docs describe credential storage, scope, rotation, and fallback.
- [x] Unrelated auth/release work remained excluded from the clean PR branch.

## Completion record

- Prior local macOS validation passed: cargo fmt --all -- --check; cargo test --workspace --all-targets --locked (422 tests); cargo clippy --workspace --all-targets --locked -- -D warnings; git diff --check. The clean PR worktree at `34ec6f52a2d42d10fbdef07e3d5ad5ef01f75162` separately passed `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --all-targets --locked` (391 tests), plus `git diff --check`.
- Earlier Actions run [#25](https://github.com/eggstack/eggpass/actions/runs/44387410491) passed at `a626ef7561e096342a4d83f7d2ca452b9b52b873` but did not include `src/usage_key.rs`, so it was not ACL validation. Current [CI run #44588788169](https://github.com/eggstack/eggpass/actions/runs/44588788169) on draft PR [#5](https://github.com/eggstack/eggpass/pull/5), at `34ec6f52a2d42d10fbdef07e3d5ad5ef01f75162`, passed all jobs: release-workflow drift, Linux formatting/Clippy/tests/release build, and Windows MSVC tests/release build. The Windows log confirms `usage_key::tests::the_directory_and_secret_file_have_current_user_only_dacls ... ok`; the Windows test suite reported 378 passed. The first draft run caught missing CLI deserialization integration and Windows ACL test import/type errors; these were fixed before this successful run.
- Draft PR [#5](https://github.com/eggstack/eggpass/pull/5) contains the usage CLI/cache feature stack and scoped usage credential; unrelated local auth/release work was excluded. No release was published or PR merged.

## Out of scope

- Reimplementing or changing the existing daemon usage cache, its TTL, refresh/retry policy, endpoint schema, or freshness headers.
- Making any endpoint unauthenticated, weakening loopback authentication, or accepting the scoped secret for inference.
- Adding the scoped secret to `keys.json`, `eggpass key list`, normal config, or regular-key verification/storage semantics.
- Replacing direct provider collection as a fallback or performing direct collection when the daemon reports an active refresh/cooldown.
- Provider API/OAuth changes, config rewrite/migration, periodic usage polling, or exposing credentials in command output/logs.

## Relevant source references

- `src/cli/commands/usage.rs:129-178, 246-284, 400-435` — direct collection, output, daemon-state/last-inference probe.
- `src/server/usage.rs:321-357` — existing cache response and freshness behavior.
- `src/server/mod.rs:231-298, 363-435` — request guard, shared authentication, key presentation.
- `src/server/router.rs:5-45` — existing route map.
- `src/cli/commands/run.rs:98-129, 189-295` — regular-key startup guard, providers and cache setup.
- `src/keys.rs:182-205, 236-249, 346-407` — regular key issue/verify and private file writer.
- `src/paths.rs:5-11, 156-195, 198-219` — data-directory resolution, config override semantics, platform permission helper.
- `docs/commands.md:73-79` — current CLI/daemon usage documentation.
