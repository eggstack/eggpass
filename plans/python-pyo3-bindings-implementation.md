# Python Bindings for `eggpass-auth` — Implementation Plan

**Status:** Implemented locally. The macOS arm64 CPython 3.14 wheel and a clean sdist install pass; the separate Linux CI matrix is configured but not yet run. PyPI/crates.io publication remains out of scope.

**Basis:** Current `eggpass-auth` API and the completed PyO3/UniFFI research. Preserve the existing Rust crate and its independent release boundary.

## Implementation outcome

- The separate package lives at `python/eggpass-auth`, imports as `eggpass_auth`, and builds the private `eggpass_auth._native` extension. It uses PyO3 0.30.1, `pyo3-async-runtimes` 0.30.0 with Tokio, Maturin 1.9.5, Rust 1.89, and the `abi3-py39` stable ABI.
- The Python Cargo crate is its own nested workspace. Because the canonical core crate is a member of the repository’s Cargo workspace, the Python package includes a source snapshot at `vendor/eggpass-auth`; `tools/sync_core.py --check` verifies it against `crates/eggpass-auth`. A narrow `.gitignore` exception ensures the snapshot is included in checkout/CI. The sdist packages it and installs without depending on the repository checkout.
- Python `TokenManager` and async storage callbacks are deferred. This implementation did not establish a robust atomic Python-store adapter; V1 exposes explicit credential and refresh operations only.
- Python task cancellation was tested to stop the polling future; it does not set a shared `CancellationToken`. Calling `CancellationToken.cancel()` explicitly produces the Rust cancellation result.
- Local wheel, sdist, integration tests, and type stubs were validated on macOS arm64 / CPython 3.14. The separate workflow configures Ubuntu x86_64 jobs for CPython 3.9 and 3.14, but those GitHub jobs have not run in this implementation session. Windows and other architectures are not qualified.

## Goal

Provide a supported Python package that reuses `eggpass-auth` OAuth protocol behavior from Python applications. Python callers should be able to await authorization, device-flow, token-refresh, and related operations from `asyncio`, while retaining ownership of UI, callback reception, and credential persistence.

## Recommendation and proposed defaults

1. **Binding technology:** Use PyO3 for the Python-native extension, `pyo3-async-runtimes` with its Tokio integration for Rust-future/Python-`asyncio` conversion, and Maturin for the Python package and wheels. The PyO3 v0.30.1 guide still marks its built-in `async fn` bindings as actively developing; use the dedicated runtime bridge unless a later review establishes a stable replacement.
2. **Package boundary:** Keep `crates/eggpass-auth` independent of Python and PyO3. Add a separate Python binding/package project under `python/eggpass-auth`, with a public `eggpass_auth` Python package and private `_native` module. The binding uses its own nested Cargo workspace and remains outside the root Cargo workspace.
3. **Async contract:** Export awaitable Python methods backed by Tokio. Never call `block_on` from Python methods and never create a fresh Tokio runtime per operation. Clone or move owned Rust values into `Send + 'static` futures; do not hold Python/GIL-bound borrows across Rust `.await` points. The Python caller’s event loop remains the public async interface.
4. **HTTP client ownership:** Python cannot pass an arbitrary Python HTTP session to `OAuthClient::new`, which requires a Rust `EggfetchClient`. The binding constructs and owns a Rust Eggfetch client and exposes the finite request timeout and decoded-body cap. V1 does not expose proxy, custom-root, or arbitrary transport settings; it does not imply that Python requests/aiohttp clients can be injected directly.
5. **V1 API:** Expose provider presets, validated custom profiles, `OAuthClient`, opaque pending authorization, credentials/token values, authorization-code operations, device-code operations, refresh/revoke/userinfo, and typed/sanitized errors. Leave browser launching, callback listeners, terminal/UI behavior, and filesystem paths to the Python host. Pass the full callback URL into Rust completion so existing origin/path/state/nonce checks remain authoritative.
6. **Credential storage:** Do not expose the generic Rust `CredentialStore` trait as though a Python class could implement it directly: it is generic/RPITIT-based and not object-safe (`crates/eggpass-auth/src/manager.rs:10-30`). Prototype a concrete binding-side Python-store adapter before promising Python `TokenManager` support. The adapter must safely bridge Python async `load`/`save`/`clear` callbacks into Tokio and preserve atomic whole-credential replacement. If that prototype is not robust, V1 should expose explicit refresh operations and credentials, leaving persistence and refresh orchestration to Python; record `TokenManager` support as a follow-up rather than changing the core trait solely for bindings.
7. **Versioning and release:** Version the Python distribution separately from the Rust crate. The package is `eggpass-auth` version `0.1.0`, targets CPython 3.9+ via `abi3-py39`, and uses PyO3 0.30.1 / `pyo3-async-runtimes` 0.30.0 with Maturin 1.9.5. Registry lookups found no existing `eggpass-auth` package on PyPI or crates.io; no publication workflow or release was added.

## Current API constraints to preserve

- `eggpass-auth` accepts a caller-owned Rust Eggfetch client and creates no runtime; the binding is the host adapter (`crates/eggpass-auth/src/lib.rs:1-9`; `crates/eggpass-auth/src/client.rs:72-80`).
- `PendingAuthorization` holds private PKCE verifier, state, nonce, client ID, and redirect information; it is one-use and its Rust `Debug` redacts sensitive fields (`crates/eggpass-auth/src/client.rs:99-118,544-575`). The Python wrapper must retain it as an opaque object, redact `repr`, and prevent reuse after completion.
- Device polling accepts a `CancellationToken` and caller timeout (`crates/eggpass-auth/src/client.rs:379-397`). Specify and test how Python cancellation signals that token; do not silently claim that cancelling an `asyncio.Task` has the same semantics until verified.
- `Credentials` is serializable for caller-owned storage but contains access, refresh, and ID tokens (`crates/eggpass-auth/src/token.rs:119-145`). Any Python serialization API is secret-bearing; document that callers must protect it. Python `repr`, logs, and exception messages must redact secrets.
- Rust errors intentionally omit response bodies, request URLs, authorization codes, and tokens (`crates/eggpass-auth/src/error.rs:38-42`). Map them to Python exception classes without adding those values back.
- `TokenManager` provides only per-manager/process single-flight, not cross-process coordination (`crates/eggpass-auth/src/manager.rs:33-43`). A Python store/manager adapter must not overstate that guarantee.

## Staged work

### Stage 0 — Prove the binding and resolve package gates

- Recheck current PyO3, `pyo3-async-runtimes`, Maturin, Python, and Rust MSRV compatibility. The current core crate declares Rust 1.89 (`crates/eggpass-auth/Cargo.toml:1-5`); lock compatible versions rather than assuming the latest versions work together.
- Build a minimal extension prototype with one Rust async function returned through `pyo3_async_runtimes::tokio::future_into_py`; verify it is awaited inside a running `asyncio` event loop. Test event-loop reuse and the documented calling pattern; do not drive the returned awaitable with a second/nested event loop.
- Prototype cancellation for device polling, including normal completion, Python task cancellation, and explicit cancellation-token signaling.
- A Python async storage adapter was not proven safe and atomic in this implementation. V1 therefore defers `TokenManager` and exposes explicit credential and refresh operations without changing the core storage trait.
- The wheel uses `abi3-py39`. The sdist uses a self-contained `vendor/eggpass-auth` snapshot checked by `tools/sync_core.py`; a maintainer-only path outside the package is not required. Local macOS arm64 wheel/sdist installs passed; Ubuntu x86_64 CPython 3.9/3.14 checks are configured in CI but remain unrun, and Windows/other architectures remain unqualified.

**Exit:** A minimal importable wheel calls an async Rust stub under `asyncio`; cancellation and event-loop behavior are understood; the store-adapter and sdist-source strategy are explicit.

### Stage 1 — Establish the independent Python package

- Add a separate binding crate and Maturin `pyproject.toml`; configure a mixed package such as `eggpass_auth._native` plus a small Python facade.
- Keep PyO3 and binding-only dependencies out of `crates/eggpass-auth/Cargo.toml` and the normal Rust workspace test path unless Stage 0 demonstrates a safe alternative.
- Add package metadata, license/readme inclusion, a `py.typed` marker and Python stubs or inline typing. Verify Cargo formatting, lint, and existing Rust CI remain unchanged.
- Ensure clean-checkout wheel and sdist builds work in isolation, including the selected source strategy for `eggpass-auth`.

**Exit:** `import eggpass_auth` works after installing the built wheel in a clean environment; the sdist can rebuild without relying on files outside its declared contents.

### Stage 2 — Bind the OAuth API and map lifecycle values

- Wrap the Rust client and provider configuration in Python-facing types. Provide xAI and OpenAI profile constructors, the xAI compatibility profile, and a validated custom-profile builder that exposes only supported non-secret protocol parameters.
- Construct the Rust Eggfetch client inside the wrapper from explicit configuration; expose `ClientOptions`-equivalent request timeout and decoded-body cap. Document any transport settings that are not available in V1.
- Implement each network operation as an awaitable using the Tokio bridge. Keep `begin_authorization` synchronous if it remains purely local; make completion, exchange, refresh, revocation, userinfo, device-code request, and polling awaitable.
- Wrap `PendingAuthorization` as opaque, one-use state; completion consumes it and receives the full callback URL. Do not expose verifier/state/nonce or accept a callback code/state pair that bypasses Rust validation.
- Map `Credentials`, `TokenSet`, `DeviceCode`, `DevicePoll`, and `UserInfo` to typed Python values. Keep secret access explicit and `repr` redacted. Map `eggpass_auth::Error` variants into a Python exception hierarchy preserving only safe status/code facts.
- If Stage 0 approves the store adapter, expose a typed Python async store protocol through a concrete Rust adapter and then expose the core token manager. Otherwise limit V1 to explicit credential and refresh operations.

**Exit:** Python API coverage matches the approved V1 scope; callback validation, refresh-token merge, cancellation, and error redaction match the Rust library’s behavior.

### Stage 3 — Add Python integration and security tests

- Add `pytest` tests that import the built extension and run under `asyncio`; use a local deterministic OAuth stub, not provider-live CI.
- Cover successful and rejected authorization callbacks (wrong origin/path/state, duplicate or missing query values, nonce mismatch), client-ID registration behavior, refresh-token rotation/omission, device pending/slow-down/denial/expiry/cancellation, and sanitized errors.
- Test one-use pending transactions, Python cancellation mapping, multiple operations on the same event loop, event-loop shutdown/recreation, and concurrent refresh behavior within the documented guarantee.
- Assert `repr`, exceptions, and captured logs do not contain access/refresh/ID tokens, authorization codes, device codes, PKCE verifier, state, nonce, or full authorization URLs.
- If storage callbacks are included, test atomic replacement and callback failures, and verify no cross-process guarantee is claimed.

**Exit:** Rust local-stub tests and Python integration tests pass without provider access; existing Rust tests and sync Eggpass paths remain unchanged.

### Stage 4 — Validate wheel/sdist and maintainer workflow

- Build and install wheels for every declared OS/architecture/interpreter target. Use Maturin’s manylinux-compatible Linux build path; test the selected `abi3` or version-specific wheel strategy rather than assuming it eliminates platform-specific wheels.
- Build an sdist and install it in a clean environment; verify the Rust crate source and all build metadata are present or fetched from the selected immutable version source.
- Add a separate Python-package CI job for formatting/linting, Rust checks for the binding crate, Python typing, pytest, wheel build, sdist build, and clean installation. Do not make Python tooling a prerequisite for the existing Rust-only CI unless approved.
- Document API lifecycle, callback ownership, event-loop usage, transport limitations, secure persistence, Python cancellation, supported platforms/Python versions, and the difference between Rust crate and Python package versions.
- Stop at local/CI packaging validation. Do not add or run a PyPI publication workflow in this plan.

**Exit:** The declared wheels and sdist build/install in clean environments; the current Rust workspace and release workflows remain unaffected; no public package is uploaded.

## Out of scope

- Adding PyO3 or Python runtime dependencies to the core `eggpass-auth` crate.
- Making Eggpass’s synchronous CLI, proxy server, or SSE path async.
- Python-owned browser launching, callback listener, UI, terminal prompts, or credential file paths.
- Accepting an arbitrary Python HTTP client as the Rust Eggfetch client.
- Publishing to PyPI, publishing a Rust crate, or changing the existing crates.io release workflows.
- UniFFI, CFFI, or raw C ABI support unless a future multi-language or non-CPython requirement justifies it.

## Acceptance checklist

- [x] The Python binding is a separate package; core `eggpass-auth` remains Python-free.
- [x] Awaitable Rust operations integrate with `asyncio` via the Tokio bridge; no nested `block_on` or per-call runtime is used.
- [x] The Python API owns/configures its Rust Eggfetch client explicitly and documents transport limitations.
- [x] Pending authorization stays opaque and one-use; callback validation remains inside Rust.
- [x] Device-flow cancellation, credentials, errors, and debug/repr behavior preserve the Rust security contract.
- [x] Python storage/`TokenManager` support is explicitly deferred; no object-safe Python trait is implied.
- [x] Local-stub Python tests cover authorization, refresh, device polling/cancellation, security, and event-loop behavior; provider-live calls are absent from CI.
- [ ] Full wheel/sdist platform-matrix validation remains pending: local macOS arm64 passes; Ubuntu x86_64 CPython 3.9/3.14 is configured but unrun; Windows and other architectures are unqualified.
- [x] Existing root Rust workspace checks pass; no PyPI or crates.io publication occurred.

## Research references

Local implementation and conventions:

- Existing Rust API contract and example: `crates/eggpass-auth/README.md`, `crates/eggpass-auth/src/lib.rs`, `crates/eggpass-auth/src/client.rs`, `crates/eggpass-auth/src/token.rs`, `crates/eggpass-auth/src/manager.rs`, `crates/eggpass-auth/src/error.rs`, `crates/eggpass-auth/examples/downstream.rs`.
- Existing implementation plan and crate/workspace release context: `plans/async-auth-library-implementation.md`, `Cargo.toml`, `.github/workflows/ci.yml`.

Current official binding and packaging guidance reviewed 2026-11-17:

- [PyO3 async/await guide](https://pyo3.rs/v0.30.1/async-await) — native async binding status, future bounds, cancellation, interpreter attachment.
- [PyO3 ecosystem async guide](https://pyo3.rs/v0.30.1/ecosystem/async-await) — recommends `pyo3-async-runtimes` for Python/Rust async interoperability.
- [pyo3-async-runtimes](https://github.com/PyO3/pyo3-async-runtimes) — Tokio runtime integration and `future_into_py` pattern.
- [Maturin project layout](https://www.maturin.rs/project_layout), [bindings](https://www.maturin.rs/bindings), and [distribution](https://www.maturin.rs/distribution) — mixed packages, PyO3/UniFFI/CFFI modes, wheels, source distributions, and platform portability.
- [UniFFI async support](https://mozilla.github.io/uniffi-rs/0.32/futures.html) — async Python bindings and cancellation/runtime tradeoffs; retained as an alternative, not the proposed route.
