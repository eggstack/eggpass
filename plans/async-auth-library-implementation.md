# Async Auth Library Implementation Plan

**Status:** Local implementation and validation complete. No public release or publication occurred or is authorized.

**Basis:** [`async-auth-library-requirements.md`](../async-auth-library-requirements.md) and the existing Eggpass auth, provider, and release code cited below.

## Goal

Create a small, independently versioned public Rust crate—provisionally `eggpass-auth`—that lets downstream async applications reuse Eggpass’s OAuth behavior with their own `eggfetch_core::Client`, runtime, UI, and credential storage. Preserve Eggpass as a synchronous CLI/proxy; do not make its server or request path async as part of this work.

## Proposed defaults to approve before implementation

1. **Package boundary:** Add `crates/eggpass-auth` as a separate workspace package, not a library target inside the existing `eggpass` binary package. Give it its own version and crate metadata. Keep the root binary’s version and release identity independent.
2. **Dependency:** Target the researched Eggfetch 0.3 line directly and accept the temporary Eggfetch 0.2/0.3 dependency duplication from the updater unless updating `eggup-eggfetch` is independently justified. Recheck current versions and package availability before coding and again before publication.
3. **Core scope:** Include async authorization-code exchange, refresh, revocation, user-info, browser-authorization transaction creation/completion, device-code creation, and cancellable device polling. Return URLs and device instructions as data. Do not print, launch a browser, read stdin, bind loopback sockets, or create a runtime in the core crate. Consider loopback/browser conveniences only as a later optional adapter.
4. **Providers:** Ship focused built-in x.ai and OpenAI profiles plus a minimal documented custom OAuth profile. Preserve x.ai’s `referrer` behavior and device-flow support, and OpenAI’s registration-only parameters and issued-client-ID reuse. Do not expose the inference-oriented `ProviderSpec`.
5. **Credentials:** Make explicit-token operations and caller-owned storage the default. Add a generic storage seam for a manager only if it can preserve the existing refresh behavior; an Eggpass-compatible file adapter is optional and must not make the core assume `~/.config/eggpass/auth.json` or import Grok CLI paths. Document that in-process refresh coalescing is not a cross-process lock unless the storage contract provides coordination.
6. **Sync Eggpass adapter:** Before extracting or replacing the current sync auth path, prove a narrow host-owned async-to-sync bridge that does not create a runtime in the library or change the proxy/server architecture. If the bridge is unsafe or causes behavior regressions, pause that integration rather than rewrite the server or silently maintain two divergent OAuth implementations.
7. **Release boundary:** Version and publish the auth crate independently. Use an auth-specific release tag/workflow and ensure the existing root `publish-crates.yml` cannot treat an auth release as an Eggpass release. The current root publisher listens to all published releases, so this isolation is a required release-workflow change, not an assumption.

These are recommended planning defaults, not decisions already made by the user. The scope, sync bridge, storage contract, Eggfetch alignment, and versioning choice are go/no-go decisions before implementation begins.

## Stage 0 decisions and execution record

The user asked to work through the plan in full. I am proceeding with the proposed defaults below; this authorizes local implementation and dry-run validation only, not a public release or crates.io publication.

- Package: separate `crates/eggpass-auth` workspace member, independently versioned from the `eggpass` binary. Initial library version: `0.1.0`; auth release tags: `eggpass-auth-v0.1.0` and the same prefix for later versions.
- Eggfetch: use the `eggfetch-core` 0.3 line and accept temporary 0.2/0.3 duplication. Checked 2026-11-17: crates.io lists 0.3.0 as latest; it declares Rust 1.89. `eggup-eggfetch` 0.2.0 still requires `eggfetch-core ^0.2.0`.
- Scope: async token operations, auth-code transaction/callback completion, device flow, both built-in profiles, and custom OAuth configuration. Core creates no runtime and owns no UI, listener, filesystem path, or terminal interaction.
- Storage: caller-owned credentials by default, with a generic async credential-store seam and a manager whose single-flight guarantee is scoped to one manager instance/process. Eggpass’s filesystem adapter stays host-owned.
- Sync bridge: host-owned Tokio runtime calls the async library only at Eggpass’s synchronous auth boundary. The proxy/server remains synchronous. A local-stub bridge test is required before replacing the existing auth path.
- Provider profile parameters: preserve x.ai `referrer`/device flow and OpenAI registration-only `agent_name_hint`, resource parameters, and issued-client-ID reuse; support custom profiles.
- Release boundary: auth-specific tag/workflow; root publishing is limited to root `vX.Y.Z` tags. Validate package workflows dry-run only; do not create or publish a GitHub release.

## Out of scope

- Making Eggpass’s blocking HTTP server, upstream calls, daemon, or SSE path async.
- Replacing unrelated `ureq` uses in `src/upstream.rs`, `src/daemon.rs`, or usage reporting.
- Introducing embedded login pages, UI, browser launching, or terminal interaction into the public core.
- Live OAuth checks as a CI requirement.
- Publishing a crate, creating or publishing a GitHub release, or changing existing release artifacts/tags without a separate, fresh confirmation.

## Staged work

### Stage 0 — Resolve the implementation gates

- Recheck `eggpass-auth` name availability, the current Eggfetch API/MSRV, and whether Eggup’s adapter has moved beyond Eggfetch 0.2.x.
- Choose the five defaults above or record alternatives. In particular, settle the manager/storage API and how synchronous Eggpass calls will drive the async library.
- Prototype only the sync bridge needed to establish feasibility; do not start extracting auth flows until it is demonstrated on the existing CLI path.
- Record the first crate version and tag convention separately from the Eggpass binary version.

**Exit:** Decisions are explicit, the package name is available, dependency/MSRV compatibility is checked, and the sync bridge has a safe direction or is explicitly deferred.

### Stage 1 — Establish the crate and public type boundary

- Add the crate under `crates/eggpass-auth`, with a narrow dependency set, Rust 1.89 MSRV unless dependency revalidation changes it, a crate-local README/examples, and an explicit package include list.
- Add it to the Cargo workspace while preserving the existing root `eggpass` package, binary target, root package version, package include rules, and Eggpack identity.
- Define stable public modules and builders for `OAuthProviderConfig` and the client. Accept/clone a caller-supplied Eggfetch client; expose no Eggfetch response internals or Eggpass server/config modules.
- Define non-exhaustive public errors that carry useful status/OAuth codes but not arbitrary response bodies. Document ownership and `Send`/`Sync` guarantees.
- Ensure token-bearing values, device codes, pending authorization secrets, and stored credentials have redacted `Debug`; do not log secrets.

**Exit:** The crate compiles independently and as a workspace member; a minimal consumer can construct a provider/client without importing Eggpass’s private modules.

### Stage 2 — Port the async OAuth protocol client

- Implement form/query encoding and provider-specific request parameters for authorization, token exchange, refresh, revocation, user-info, device-code creation, and polling.
- Make connect/read/total timeout policy explicit in configuration; do not accidentally inherit Eggfetch defaults. Bound response bodies and avoid blind retries for exchanges or refreshes, especially where refresh-token rotation may occur.
- Normalize status, OAuth, malformed-response, timeout, and network failures into the crate’s own errors. Avoid returning or displaying arbitrary response bodies.
- Preserve the current OAuth request semantics and the special OpenAI issued-client-ID lifecycle.

**Exit:** Deterministic local-stub tests cover encoded parameters, success/error/non-JSON responses, timeout and oversized-body handling, and client-ID/registration behavior.

### Stage 3 — Implement authorization and device-flow transactions

- Implement an authorization transaction that returns an authorization URL and opaque pending state containing PKCE verifier, high-entropy state, nonce, redirect URI, and expected callback path. Completion validates state and the exact callback path before exchanging the code. Keep sensitive transaction fields private and redacted.
- Require PKCE S256; preserve nonce/state validation. A loopback listener, if added later, must bind only to loopback, use a short-lived random port, and close after completion; no embedded user-agent.
- Implement device-code start and async polling with caller-controlled cancellation/deadline. Return the verification URI and user code as values; validate verification hosts before presenting/opening them, and never log device codes.
- Follow RFC 8628 polling semantics: honor the server’s expiry rather than clamping short values to 60 seconds; after `slow_down`, add five seconds to the interval for subsequent polls. Cover `authorization_pending`, denial, expiry, and cancellation.

**Exit:** Local deterministic tests verify PKCE/state/nonce, callback rejection cases, trusted-host checks, polling interval/deadline behavior, and cancellation.

### Stage 4 — Add token lifecycle, storage seam, and Eggpass adapter

- Provide token parsing/refresh independently from filesystem paths. If a manager is included, preserve proactive refresh skew, 401 compare-and-refresh, one refresh per process under concurrency, and merge behavior that retains the existing refresh token when rotation is omitted.
- Define whether storage operations are atomic and whether they coordinate across processes. A file adapter, if selected, must retain restrictive permissions and atomic replacement; do not imply that an in-process mutex is a cross-process lock.
- Complete the host-side Eggpass adapter only after the Stage 0 bridge is proven. Keep CLI prompts, browser launching, stdin handling, path resolution, Grok CLI import, and the synchronous `TokenProvider` interface in Eggpass host modules. Do not change the proxy server’s execution model.
- Resolve the current parity defects before calling the port complete: redact secret-bearing `Debug`, correct `slow_down` to the RFC-required five-second increase, and handle short server expiry values without the current 60-second minimum.

**Exit:** Refresh rotation, concurrent refresh, storage guarantees, and the existing sync CLI integration pass focused regression tests without requiring downstream users to adopt Eggpass paths or UI.

### Stage 5 — Downstream contract, CI, and package validation

- Add a compiling downstream-style example that supplies an Eggfetch client and awaits authorization/refresh using the consumer’s runtime; it must not depend on Eggpass CLI/server modules.
- Run the library against a local OAuth stub using the real Eggfetch client. Keep provider-live tests opt-in and outside deterministic CI.
- Extend CI to format, lint, test, and package the workspace/library. Keep root binary tests and builds. Validate the library on supported platforms, including Windows if the selected storage adapter is cross-platform.
- Check dependency resolution explicitly for both Eggfetch lines and confirm `--locked` works. Run `cargo package --locked` for `eggpass-auth` and the existing root package; inspect the packaged file list for omissions or accidental inclusion.
- Verify all five existing Eggpass release targets before any later Eggpass binary release; library validation does not replace that release gate.

**Exit:** Required protocol/security tests pass, the downstream example compiles, both packages pass locked packaging checks, and existing Eggpass CI/build/release behavior remains intact.

### Stage 6 — Prepare a separate publication path (no publication in this plan)

- Add an auth-specific, dry-run-first verification/publish path with independent version/tag validation and explicit package selection.
- Scope the existing root crates.io workflow to Eggpass release tags, and test that an auth-tagged release neither builds nor publishes the root `eggpass` crate. Keep the existing Eggpack binary release process and its draft-review-before-publish gate intact.
- Update maintainer release documentation with package-specific versioning, tag convention, package verification, and publication effects. Recheck crate-name availability and exact dependency versions before a first publish.
- Stop at a verified dry run. Any public GitHub release publication (which can trigger crates.io publication) requires separate, fresh user confirmation immediately before that action.

**Exit:** Local package dry runs passed and workflow lint/source checks confirm the release boundary. A GitHub workflow dispatch was not run; no public release has been created or published.

### Completion record

- Passed on the local macOS host: `cargo fmt --all -- --check`, strict workspace Clippy, locked workspace tests (329 Eggpass, 30 auth unit, 10 local-stub tests), downstream example check, and locked Rust 1.89 workspace tests.
- Passed local package/build checks: `cargo package --locked --allow-dirty -p eggpass-auth`; root `eggpass` package verification with a local Cargo patch to `crates/eggpass-auth` (the auth crate is intentionally not yet on crates.io); and `cargo build --release --locked`.
- Passed `actionlint` on CI and both publisher workflows, ShellCheck on publisher shell snippets, and `git diff --check`.
- Not run: remote GitHub Actions dispatch, Windows MSVC job, or the five-target Eggpass release build. CI defines Windows 1.89 checks; all five targets remain mandatory before any future Eggpass binary release.
- No crate was published and no GitHub release was created. Publish `eggpass-auth` before any future Eggpass crates.io package release; the publisher workflows remain separately gated.

## Acceptance checklist

- [x] Public crate API accepts a caller-owned Eggfetch client, creates no runtime, owns no UI, and has no Eggpass home-directory assumption.
- [x] Built-in x.ai/OpenAI profiles and custom-provider configuration preserve required provider-specific parameters and issued-client-ID behavior.
- [x] PKCE S256, entropy, state/nonce and exact callback validation are tested; device verification URIs are host-checked.
- [x] Device polling handles pending, `slow_down` (+5 seconds), denial, actual expiry, timeout, and cancellation.
- [x] Timeouts, response-size bounds, error sanitization, retry rules, and secret-safe debug/log behavior are tested/documented.
- [x] Refresh preserves proactive skew, compare-and-refresh, one-process single-flight, and refresh-token rotation semantics; storage atomicity/cross-process guarantees are explicit.
- [x] Local OAuth stub tests use the real Eggfetch client; a downstream-style async consumer compiles without Eggpass application modules.
- [x] Root Eggpass sync behavior and server architecture are unchanged; the sync adapter bridge is proven or deferred explicitly.
- [x] Both packages pass formatting, lint, tests, and locked packaging; CI covers the selected platform/storage support.
- [x] Existing five-target Eggpass release gates remain intact; auth release tags cannot invoke root Eggpass publication; no public publication occurs without fresh confirmation.

## Current code and research references

- Requirements and dependency/version findings: [`async-auth-library-requirements.md`](../async-auth-library-requirements.md), especially sections “Recommendation and boundary,” “Security and protocol acceptance criteria,” and “Package, dependency, tests, and release work.”
- Private binary-only boundary: `src/main.rs:8-21`.
- Current synchronous OAuth client and secret-bearing token/device types: `src/auth/client.rs:51-67,130-180`.
- Browser/device flows, trusted verification URLs, expiry clamp, and current `slow_down` increment: `src/auth/flow.rs:127-289`.
- Refresh coalescing and rotated-token merge behavior: `src/auth/refresh.rs:67-225`.
- Current credential format, restrictive atomic file persistence, and Grok CLI import: `src/auth/store.rs:1-18,44-87,130-210`.
- Provider-specific profile behaviors: `src/provider/xai.rs:75-100`; `src/provider/openai.rs:149-198`.
- Existing root package metadata and explicit package include list: `Cargo.toml:1-28,30-77`.
- Existing root publisher trigger and root-only package/version checks: `.github/workflows/publish-crates.yml:10-19,41-61`.
- Eggpass binary release shape and human publication gate: `docs/releasing.md:22-115`; five-target candidate builds are in `.github/workflows/release.yml`.
- OAuth protocol references and Eggfetch API links are collected in the requirements memo.
