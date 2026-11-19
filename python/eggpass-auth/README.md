# `eggpass-auth` for Python

This package exposes the Rust `eggpass-auth` OAuth client to Python `asyncio` through PyO3, `pyo3-async-runtimes`, and Tokio. The Rust protocol implementation remains a separate, Python-free crate. The Python distribution is versioned independently.

## Build and test locally

From this directory, use a virtual environment and install the local package:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install maturin pytest
maturin develop
pytest
```

A regular `pip install .` or a Maturin wheel build compiles the extension. The package uses PyO3's `abi3-py311` stable ABI for GIL-enabled CPython. The GitHub Actions matrix builds Linux x86_64/aarch64 wheels with manylinux2014 (glibc), macOS x86_64/arm64 wheels, and a Windows x86_64 wheel; it tests each wheel on CPython 3.11–3.15. The workflows do not build musllinux wheels. Treat a platform as supported for a release only after the corresponding build and wheel-install tests pass.

The workflows do not build free-threaded CPython 3.15t or 32-bit ARM wheels (including armv6l/armv7l Raspberry Pi wheels). A compatible 64-bit Raspberry Pi OS may be able to use the Linux aarch64 wheel, but that combination is not separately qualified. Building from source requires a working Rust toolchain and the platform's native build prerequisites.

## Authorization-code flow

The host application owns browser launching, the callback listener/UI, and credential persistence. Pass the full callback URL to Rust so it can validate the origin, exact path, state, and OIDC nonce:

```python
from eggpass_auth import OAuthClient, ProviderProfile

oauth = OAuthClient(ProviderProfile.xai())
pending = oauth.begin_authorization("http://127.0.0.1:43123/callback")
print("Open this URL in the host UI:", pending.authorization_url)

async def complete_from_host_callback(callback_url: str):
    # The host passes the full URL received by its loopback listener.
    return await oauth.complete_authorization(pending, callback_url)
```

`PendingAuthorization` is opaque and one-use. Its `repr` never prints the authorization URL or PKCE/state/nonce values. Completion consumes it even if validation or the network request fails. Do not parse the callback into a code/state pair and bypass the full-URL validation.

For pasted-code flows, use `complete_authorization_with_code`. The lower-level `exchange_authorization_code` method is also available when the host intentionally manages PKCE material itself.

## Device flow and cancellation

```python
from eggpass_auth import CancellationToken, OAuthClient, ProviderProfile

async def device_login():
    oauth = OAuthClient(ProviderProfile.xai())
    device = await oauth.request_device_code()
    print(device.user_code, device.verification_uri)
    cancellation = CancellationToken()
    try:
        return await oauth.poll_device_authorization(
            device, cancellation, timeout_seconds=600
        )
    finally:
        cancellation.cancel()
```

Pass a `CancellationToken` when the host needs to stop the Rust polling loop explicitly. Cancelling an `asyncio.Task` should not be treated as a signal to every shared token; call `cancel()` on the token. No Python polling or networking work blocks the event loop.

## Profiles and transport

`ProviderProfile.xai()`, `ProviderProfile.xai_compatibility_fallback()`, and `ProviderProfile.openai()` provide the built-in profiles. Custom profiles are validated by the Rust provider builder:

```python
profile = ProviderProfile.custom("Example", "https://auth.example", "public-client")
profile.authorization_endpoint("https://auth.example/authorize")
profile.token_endpoint("https://auth.example/token")
profile.callback_path("/callback")
profile.scopes(["offline_access"])
profile = profile.build()
```

`OAuthClient` constructs and owns its Rust Eggfetch client. It accepts a finite `timeout_seconds` and a positive `max_response_body_bytes`; a Python `requests`, `httpx`, or `aiohttp` session cannot be injected. V1 does not expose custom proxy, root-certificate, or arbitrary transport configuration. OAuth requests suppress inherited default authentication, disable redirects and retries, and use bounded response bodies.

## Credentials and errors

Token fields are available explicitly when the host needs to persist or use them. `Credentials.to_json()` and `Credentials.from_json()` are secret-bearing storage APIs: protect their output and do not log it. `repr()` for credentials, token sets, pending authorizations, device codes, and clients redacts secrets and sensitive URLs. Exceptions expose only safe protocol facts; OAuth errors provide a recognized `code` and HTTP `status` when available.

Python owns persistence and refresh scheduling in V1. The Rust generic async `CredentialStore` / `TokenManager` API is not exposed: an async Python storage adapter has not been proven safe or atomic. `refresh_token()` returns provider tokens; `refresh_credentials()` merges them while retaining the old refresh token if rotation was omitted. Persist the returned credentials yourself. Rust single-flight management is therefore not claimed for Python callers.

All network operations are awaitable on the caller's running `asyncio` loop. Do not start a nested event loop or call `block_on`. Browser/UI, callback reception, and filesystem paths remain with the host application.

## Packaging scope

The sdist includes a self-contained snapshot of the local `eggpass-auth` crate, so it builds without a published Rust crate. In the monorepo checkout, `vendor/eggpass-auth` is generated from `../../crates/eggpass-auth`; run `python tools/sync_core.py --check` to verify it, or `python tools/sync_core.py --sync` after intentional core changes. Python releases use a separate `eggpass-auth-python-vX.Y.Z` tag and PyPI Trusted Publishing workflow; they do not publish the Rust crate or Eggpass CLI. Publishing remains gated by the protected `pypi` GitHub environment.
