# eggpass-auth

Async OAuth authentication primitives for applications using Eggfetch. The crate is independently versioned from the Eggpass CLI and accepts an Eggfetch 0.3 `Client` owned by the caller.

## Boundary

The library does not create a Tokio runtime, launch a browser, print to a terminal, bind a callback listener, read stdin, or assume a credential file path. The host application owns the runtime, user interface, callback transport, and persistence. The async methods are awaited on the application's runtime.

## Browser authorization

```rust,no_run
use eggfetch_core::Client;
use eggpass_auth::{OAuthClient, OAuthProviderConfig};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let auth = OAuthClient::new(Client::new(), OAuthProviderConfig::xai());
let pending = auth.begin_authorization("http://127.0.0.1:43123/callback", None)?;
let url_for_external_browser = pending.authorization_url();
// Present the URL, receive the callback in the host application, then pass its
// full URL to complete_authorization. The crate does not create the listener.
let _ = url_for_external_browser;
# Ok(())
# }
```

`begin_authorization` creates PKCE S256, state, nonce, and the authorization URL. `complete_authorization` consumes the opaque pending transaction and rejects callback origin/path mismatches, missing or mismatched state, and an ID-token nonce mismatch. The ID-token nonce claim is checked; this crate does not validate ID-token signatures. Use the provider's user-info endpoint or a separate OIDC verifier when signature/issuer/audience validation is required.

## Device authorization

`request_device_code().await` returns a `DeviceCode` containing a verification URI and user code. Before returning it, the library requires HTTPS and an exact provider-configured host match for both regular and pre-filled URIs. The caller presents these values. `poll_device_authorization(&device, &cancel, timeout).await` sleeps before the first poll, honors server expiry (including short lifetimes), applies the RFC 8628 five-second increment after each `slow_down`, and supports cancellation and a caller timeout.

## Profiles

- `OAuthProviderConfig::xai()` uses the public x.ai client ID, full scopes, `referrer=eggpass`, device authorization, and the exact x.ai verification-host allowlist.
- `OAuthProviderConfig::xai_compatibility_fallback()` returns the `grok-build` referrer and the reference client's shorter scope set for a new user authorization after a client-configuration refusal. Token exchange and refresh are never retried automatically.
- `OAuthProviderConfig::openai()` uses the `dynamic_agent_client` registration entrypoint, OpenAI resource parameters, and registration-only `agent_name_hint=eggpass`. OpenAI's issued ID is returned in `Credentials::client_id()` and should be persisted/reused. `openai_with_host_id(...)` adds a caller-owned stable `ext_agent_host_id`.
- `OAuthProviderConfig::custom(...)` starts a builder for custom endpoints and scopes. Endpoints require HTTPS, except localhost/loopback HTTP for local tests. Add verification hosts before using device flow.

Custom parameters should contain OAuth protocol parameters, not secrets. Profile and pending-authorization `Debug` output redacts IDs, parameter values, URLs, verifier, state, and nonce.

## Explicit token operations

The low-level operations `exchange_authorization_code`, `refresh_token`, `revoke`, and `userinfo` are async and accept explicit caller-owned token values. `refresh_token` returns the provider's token set without retrying. When an issued refresh response omits a new refresh token, preserve the previous one when saving credentials. Prefer `refresh_credentials` or `TokenManager` for this merge behavior.

## Storage and refresh manager

Implement `CredentialStore` for the application's database or secret store. Its `save` operation should atomically replace a credential set. `TokenManager` performs proactive refresh with a five-minute default skew, compares the failed 401 token before refreshing, coalesces refreshes in one manager/process, and retains an old refresh token when rotation is omitted. It does not provide a cross-process lock. The crate has no built-in filesystem adapter.

## HTTP safety defaults

Each request explicitly sets finite Eggfetch phase and total timeouts (pool/connect/write 10–15 seconds, read 30 seconds, total 60 seconds by default) and caps decoded response bodies at 1 MiB. Tune them with `ClientOptions`. Requests disable redirects, automatic retries, and caller-configured default authentication; the user-info call sets its own bearer token. OAuth errors expose status and a sanitized error code, not arbitrary response bodies. Timeout and transport diagnostics are deliberately generic to avoid leaking request details.

## Validation

Run the downstream-style example and deterministic local OAuth stub tests with:

```sh
cargo test -p eggpass-auth --locked
cargo check -p eggpass-auth --example downstream --locked
cargo package -p eggpass-auth --locked --allow-dirty
```

Provider-live requests are not part of deterministic CI.
