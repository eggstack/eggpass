# Research: Grok OAuth → OpenAI-compatible local proxy

Research date: **2026-11-13**. All endpoint and protocol claims below were
either read from source or verified live against x.ai. Live verification is
called out explicitly.

---

## Locked decisions (2026-11-13)

| Decision | Choice | Rationale |
| --- | --- | --- |
| Runtime | **Rust** | Matches the official Grok CLI; strong stdlib for crypto and HTTP; excellent single-binary distribution. |
| Upstream | **`api.x.ai/v1`** | Full model catalog (`grok-4.7`, `grok-4.6`, `grok-4.5`, `grok-4.3`, `grok-4.20-*`). The Grok Build proxy exposes one model and renames models to `-build` variants. |
| Login | **Browser loopback**, `--device` fallback | Best desktop UX; device code covers headless/SSH and needs no port. |
| OAuth callback port | **Random ephemeral loopback**, independent of the API port | Lets `login` run while the daemon is up and survives a `port` change. |
| Config format | **TOML** at `~/.config/eggpass/config.toml` | Same as the official Grok CLI; comment-friendly for `$EDITOR`; no parser dependency beyond one crate. |
| Daemon model | **Self-contained, PID-file based** | Identical behaviour on macOS and Linux; no root, no install step, no platform branching. |
| Async runtime | **None** — blocking, thread-per-connection | Keeps the dependency tree small and the SSE proxying code straightforward. |

### Rust toolchain and dependency plan

- **MSRV `1.85`** (the floor for Edition 2024), declared as `rust-version` in
  `Cargo.toml`. Edition 2024's resolver (`resolver = "3"`) picks dependency
  versions compatible with the MSRV, which is the main lever for long-term
  maintainability. No `rust-toolchain.toml` pin — pinning forces a specific
  download on every contributor and machine.
- **No nightly.** Stable only.

Planned dependencies (all pure-Rust except where noted):

| Crate | Purpose | Why |
| --- | --- | --- |
| `serde`, `serde_json` | JSON | Token + upstream payloads |
| `toml` | Config file | Comments, stable format |
| `ureq` | Outbound HTTP | Blocking, rustls, streaming response reader for SSE |
| `tiny_http` | Inbound HTTP | Blocking, thread-per-connection, supports streamed response bodies |
| `sha2`, `base64` | PKCE | `SHA256` → base64url no-pad |
| `rand` | Randomness | `state`, `nonce`, PKCE verifier |
| `daemonize` | Daemonization | Double-fork, `setsid`, stdio redirect, PID file |
| `signal-hook` | Signals | Graceful `SIGTERM` shutdown |
| `nix` | Process control | `kill(pid, 0)` liveness check |
| `dirs` | Paths | Config directory resolution |
| `webbrowser` | OAuth | Open the browser at the authorize URL |

Deliberately **excluded**: `tokio`/`async`/`axum`/`reqwest` (heavy tree for a
tool that is mostly proxying), `keyring` (native dependency; the official CLI
uses a plain file and so do we), and `clap`'s derive-heavy features (a small
hand-rolled arg parser is enough for nine subcommands).

---

## 1. Executive summary

| Question | Answer |
| --- | --- |
| How do we authenticate? | OAuth 2.1 Authorization Code + PKCE against `https://auth.x.ai`, using the **public Grok CLI client id**. No client secret. |
| Which grant to support? | Loopback browser flow (primary) + RFC 8628 device code (fallback for headless/SSH). Both are supported by x.ai. |
| Which upstream hosts the model? | **`https://api.x.ai/v1`** for chat/models/responses. `https://cli-chat-proxy.grok.com/v1` is the *Grok Build* path — narrower catalog, build-tuned model variants. |
| Does the OAuth token work on `api.x.ai`? | Yes — verified live by a third-party implementation on 2026-07-30, and the `api:access` scope exists specifically for this. |
| What models? | `grok-4.7`, `grok-4.6`, `grok-4.5`, `grok-4.3`, `grok-4.20-*`, `grok-build-0.1` (10 on `api.x.ai` vs 1 on the CLI proxy). |
| Critical gotcha | `cli-chat-proxy.grok.com` returns **HTTP 426** unless the request reports a Grok CLI client version ≥ 1.0.13. |
| Critical gotcha | The `Authorization` header alone is *not* what the Grok Build proxy wants — it also expects `X-XAI-Token-Auth: xai-grok-cli` and `x-grok-client-version`. |

**The single most consequential finding** is the two-host split. The reference
project (`dvcrn/grok-oauth-proxy`) proxies to `cli-chat-proxy.grok.com`, which
means it exposes roughly *one* model and renames models to `-build` variants.
Since the goal here is to expose `grok-4.7` / `grok-4.6`, we should proxy to
`api.x.ai/v1` instead and keep the CLI proxy as an optional fallback.

---

## 2. The x.ai OAuth surface

### 2.1 Discovery document (verified live)

`GET https://auth.x.ai/.well-known/openid-configuration` returns:

```json
{
  "issuer": "https://auth.x.ai",
  "authorization_endpoint": "https://auth.x.ai/oauth2/authorize",
  "device_authorization_endpoint": "https://auth.x.ai/oauth2/device/code",
  "token_endpoint": "https://auth.x.ai/oauth2/token",
  "userinfo_endpoint": "https://auth.x.ai/oauth2/userinfo",
  "revocation_endpoint": "https://auth.x.ai/oauth2/revoke",
  "jwks_uri": "https://auth.x.ai/.well-known/jwks.json",
  "response_types_supported": ["code"],
  "grant_types_supported": [
    "authorization_code",
    "refresh_token",
    "urn:ietf:params:oauth:grant-type:device_code"
  ],
  "code_challenge_methods_supported": ["S256"],
  "token_endpoint_auth_methods_supported": [
    "client_secret_basic", "client_secret_post", "none"
  ],
  "id_token_signing_alg_values_supported": ["ES256"]
}
```

`token_endpoint_auth_methods_supported` includes `"none"` → this is a **public
client**; PKCE replaces the client secret (RFC 8252). We ship no secret.

### 2.2 Client id

The Grok CLI's public client id, hardcoded in the official
`xai-org/grok-build` source (`crates/codegen/xai-grok-login/src/config.rs`):

```
b1a00492-073a-47ea-816f-4c329264a828
```

It is obfuscated with `obfstr` in the Rust source but is a *public* credential
— it is embedded in a binary that ships to every user. We reuse it, exactly as
the reference project and other public proxies do.

### 2.3 Scopes

Two scope sets exist in the official CLI:

```rust
// legacy web-login scopes (accounts.x.ai relay)
["openid", "profile", "email", "offline_access", "api:access"]

// current OAuth2 scopes — the set we want
["openid", "profile", "email", "offline_access",
 "grok-cli:access", "api:access",
 "conversations:read", "conversations:write",
 "workspaces:read", "workspaces:write"]
```

Notes:

- `grok-cli:access` — *"authorizes the token for API proxy requests"* (official comment).
- `api:access` — needed for `api.x.ai` inference. **We need this one.**
- `offline_access` — required to get a `refresh_token`. **Without it, there is
  nothing to refresh and the login dies every few hours.**
- `conversations:*` / `workspaces:*` are only needed for Grok Build's own
  conversation and workspace features. We can request them for parity, or omit
  them to keep the consent screen minimal. **Recommendation: request the full
  official set** — it costs nothing and keeps us compatible if we ever want
  Build-like features. The reference project uses only the shorter set
  `openid profile email offline_access grok-cli:access api:access`.

The full set x.ai advertises as supported (from discovery):
`openid, profile, email, offline_access, grok-cli:access, grok-workspace:serve,
team:read, teams:read, teams:write, org:read, orgs:read, orgs:write,
api:access, grok-plugins:access, conversations:read, conversations:write,
workspaces:read, workspaces:write, api-keys:read, api-keys:write, logs:read,
billing:read, billing:write`.

### 2.4 Grant A — Loopback Authorization Code + PKCE (primary)

This is what the official CLI does for an interactive desktop login.

**Request** (from the official `build_authorize_url`, verified against source):

```
GET https://auth.x.ai/oauth2/authorize
  ?response_type=code
  &client_id=b1a00492-073a-47ea-816f-4c329264a828
  &redirect_uri=http%3A%2F%2F127.0.0.1%3A<PORT>%2Fcallback
  &scope=<space-joined scopes>
  &code_challenge=<BASE64URL(SHA256(verifier))>
  &code_challenge_method=S256
  &state=<random>
  &nonce=<random>
  &referrer=grok-build
```

Details that matter:

- **PKCE verifier**: 32 random bytes, Base64URL *without* padding. Challenge =
  `BASE64URL_NOPAD(SHA256(verifier))`.
- **`state` and `nonce`**: both random. The official CLI uses UUIDv7. `state`
  must be verified on callback; `nonce` must match the `id_token` claim.
- **`referrer`**: the official CLI sends `referrer=grok-build`. The Grok Build
  changelog explicitly contains the fix *"OAuth login URLs no longer contain
  duplicate referrer parameters"*, so this parameter is tracked by xAI. The
  reference project sends `referrer=hermes-agent`; using our own identifier is
  more honest but `grok-build` is the proven-working value. **Recommendation:
  send a stable identifier of our own (e.g. `eggpass`) and fall back to
  `grok-build` if the login is rejected.** This is a low-risk, easily-changed
  constant.
- **Callback port**: the official CLI binds a **random ephemeral port** and
  formats `http://127.0.0.1:{port}/callback`, with a fixed port only in local
  dev. This confirms xAI accepts arbitrary loopback ports for this client
  (RFC 8252 §7.3 port-agnostic loopback redirect). The reference project
  hardcodes port 56121.

  **Recommendation: use a random ephemeral loopback port for the OAuth
  callback, completely separate from the API port.** This means `eggpass
  login` works while the daemon is running on 8484, and works after
  `eggpass port 9000` without a restart dance.

- **Callback timeout**: the official CLI uses **600 seconds (10 minutes)**.
- **Consent screen**: when a *Team* principal is chosen, xAI strips the
  user-only scopes (`openid`, `email`) from the issued token. Not a problem for
  a personal SuperGrok account.

**Token exchange:**

```
POST https://auth.x.ai/oauth2/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code
&code=<code>
&redirect_uri=http://127.0.0.1:<PORT>/callback
&client_id=b1a00492-073a-47ea-816f-4c329264a828
&code_verifier=<verifier>
```

Response contains `access_token`, `refresh_token`, `id_token`, `expires_in`,
and `token_type`.

> Note: the reference project also sends `code_challenge_method=S256` in the
> token request. That is not part of RFC 6749 and is unnecessary — the verifier
> is what matters. Harmless, but we should not copy it.

### 2.5 Grant B — Device code (headless fallback)

Verified live — this endpoint accepts the public client id and returns a
well-formed response:

```
POST https://auth.x.ai/oauth2/device/code
Content-Type: application/x-www-form-urlencoded

client_id=b1a00492-073a-47ea-816f-4c329264a828
&scope=<space-joined scopes>
&referrer=grok-build
```

```json
{
  "device_code": "NQwbWoSoDNbc1WPvSctetXxZztPianyjiKpSDj-uD7Z8pGV95EiLXzV5R2IDWZynCgzqrjjuonVhOVILCtOqaA",
  "user_code": "JF9K-PTY8",
  "verification_uri": "https://accounts.x.ai/oauth2/device",
  "verification_uri_complete": "https://accounts.x.ai/oauth2/device?user_code=JF9K-PTY8",
  "expires_in": 1800,
  "interval": 5
}
```

Polling uses the standard RFC 8628 grant:

```
POST https://auth.x.ai/oauth2/token
grant_type=urn:ietf:params:oauth:grant-type:device_code
&device_code=<device_code>
&client_id=<client_id>
```

The official CLI sleeps *before* the first poll (an immediate poll returns
`authorization_pending` and risks a `slow_down` penalty), honors
`interval`, and increments the interval on `slow_down`.

**Security note:** `verification_uri` points at `accounts.x.ai`. The reference
project validates the returned URL against a trusted-host allowlist before
displaying it. We should do the same — never open a browser to a URL the
provider returned without checking the host.

**Recommendation:** make device code the fallback when there is no browser
(`login --device`), and also the primary path on a headless box. It is strictly
more robust than loopback and requires no port.

### 2.6 Refresh

```
POST https://auth.x.ai/oauth2/token
grant_type=refresh_token
&refresh_token=<refresh_token>
&client_id=<client_id>
```

Refresh-token rotation: the official CLI's `tokensFromJSON` logic keeps the
**previous** refresh token when the response omits a new one, and otherwise
stores the new one. The reference project does the same (it threads
`previousRefreshToken` through `requestTokens`). We must do the same, or a
rotating provider will break us on the second refresh.

### 2.7 Token lifetimes

- The official CLI's hardcoded fallback TTL is **30 days** (`TOKEN_TTL`), used
  when the server does not send `expires_at`.
- The Grok Build README says *"Tokens expire after 7 days."*
- A third-party implementation observed access tokens expiring in **~6 hours**.

These are different things being described (access token vs. refresh token vs.
legacy web-login token). The safe engineering answer is the same in every case:

> **Trust the server's `expires_at` / `expires_in`.** If absent, fall back to a
> conservative TTL. Refresh proactively with a skew (the reference uses 5
> minutes) and persist the new tokens atomically.

### 2.8 Credential storage

The official CLI stores credentials in `~/.grok/auth.json`, keyed by scope:

```json
{
  "https://accounts.x.ai/sign-in": {
    "key": "<access token>",
    "auth_mode": "oidc",
    "create_time": "2026-11-13T05:12:18Z",
    "user_id": "…",
    "email": "…",
    "refresh_token": "…",
    "expires_at": "2026-12-13T05:12:18Z",
    "oidc_issuer": "https://auth.x.ai",
    "oidc_client_id": "b1a00492-…"
  }
}
```

**Interop opportunity:** if the user already ran `grok login` (the official
Grok Build CLI), `~/.grok/auth.json` already exists and can be read directly.
A third-party library does exactly this. We should:

- Prefer our own credential file (`~/.config/eggpass/auth.json`, mode 0600) as
  the source of truth.
- **Also** support reading `~/.grok/auth.json` as a fallback/import source, so
  `eggpass login` is not mandatory if the user already has a Grok login.

Security posture for our own file: directory mode `0700`, file mode `0600`,
write atomically (temp file + `rename`), never log token values.

---

## 3. Upstream inference: which host?

### 3.1 `api.x.ai` — the right default

`https://api.x.ai/v1` is xAI's regular OpenAI-compatible API. A third-party
implementation verified on **2026-07-30** that a Grok OIDC login (from
`~/.grok/auth.json`) is accepted there for:

- `POST /v1/chat/completions`
- `GET  /v1/models`
- `POST /v1/embeddings`
- `POST /v1/responses`

The only endpoint that rejects the OAuth login is `GET /v1/api-key` (which is
about API keys, not inference).

Catalog from that same implementation (`XAI_CHAT_MODELS`, verified 2026-10-06):

| Alias | Model ID |
| --- | --- |
| `latest` / `grok47` | `grok-4.7` |
| `grok46` | `grok-4.6` |
| `grok45` | `grok-4.5` |
| `grok43` | `grok-4.3` |
| `grok420` | `grok-4.20-0309-reasoning` |
| `grok420NonReasoning` | `grok-4.20-0309-non-reasoning` |
| `grok420MultiAgent` | `grok-4.20-multi-agent-0309` (Responses API only) |
| `build` | `grok-build-0.1` |

`/v1/models` on `api.x.ai` lists **ten** models; on the CLI proxy it lists
**one**.

### 3.2 `cli-chat-proxy.grok.com` — the Grok Build path

`https://cli-chat-proxy.grok.com/v1` is what the official Grok Build CLI talks
to. It is narrower and different:

- Served model IDs are **build-tuned**: a request for `grok-4.5` comes back
  served by `grok-4.5-build`.
- Its `/models` lists one model where `api.x.ai` lists ten.
- It returns **HTTP 426** unless the request advertises a Grok CLI client
  version ≥ **1.0.13**.
- The official CLI sends these headers for it:

  ```
  Authorization: Bearer <token>
  X-XAI-Token-Auth: xai-grok-cli
  x-grok-client-version: <VERSION>
  x-userid: <user id>
  x-email: <email>
  x-client-mode: <...>
  ```

- Hosted **search** genuinely requires this proxy — it is the contract Grok
  Build's own search runs on.

Current Grok Build version (from the official changelog, 2026-11-05): **1.0.77**.
The reference project sends `X-Grok-Client-Version: 1.0.46`.

### 3.3 Decision

**Proxy to `api.x.ai/v1`.** Reasons:

1. It exposes the full model catalog, including `grok-4.7` and `grok-4.6`,
   which is the explicit goal.
2. It does not rename models to `-build` variants.
3. It has no client-version floor, so no brittle header to maintain.

Keep `cli-chat-proxy.grok.com` reachable in the config as an optional upstream
(`upstream = "api" | "build"`) for hosted-search parity, but do not make it the
default. The upstream should be a config value rather than a compile-time
constant.

**Headers to send upstream on `api.x.ai`:** just `Authorization: Bearer <token>`.
`api.x.ai` is the public API surface and does not want Grok CLI fingerprinting.
Only add the CLI headers when talking to `cli-chat-proxy.grok.com`.

---

## 4. The reference implementation (`dvcrn/grok-oauth-proxy`)

Go, ~2.5k lines, MIT-ish. Reviewed file by file.

### 4.1 What it gets right

- **Correct PKCE.** Verifier = 32 random bytes, Base64URL no-pad; challenge =
  Base64URL(SHA256(verifier)). ✓
- **Correct `state` handling.** Verifier stored server-side keyed by `state`,
  deleted on use, single-use. ✓
- **Correct refresh semantics.** 5-minute skew, mutex-guarded, re-reads from
  disk inside the lock, preserves the previous refresh token when the response
  omits a new one, and has a `forceRefreshToken` path that only re-refreshes if
  the *same* access token failed (so a 401 from upstream doesn't cause a
  refresh stampede). ✓
- **Atomic-ish credential persistence.** Directory `0700`, file `0600`. ✓
- **Trusted verification URL check** for the device-code flow. ✓
- **Stripping internal headers** on proxy (`Authorization`, `X-Api-Key`,
  hop-by-hop, `Cf-*`, `X-Forwarded-*`) so client credentials never leak upstream. ✓
- **Models endpoint merging** so the proxy can advertise aliases the upstream
  doesn't list. ✓

### 4.2 What to change

| Issue | Detail | Our fix |
| --- | --- | --- |
| **Wrong upstream** | Proxies to `cli-chat-proxy.grok.com` → 1 model, `-build` renames | Proxy to `api.x.ai/v1` |
| **OAuth port collides with API port** | Login server and API server are both `127.0.0.1:56121`; you cannot be logged in and serving at the same time | Separate ephemeral loopback port for OAuth |
| **Narrow scope set** | Omits `conversations:*` / `workspaces:*` | Request the official scope set |
| **Missing CLI headers** | Sends only `Authorization` + `X-Grok-Client-Version` | Fine for `api.x.ai`; add `X-XAI-Token-Auth` when using the build proxy |
| **Hardcoded client version** | `1.0.46`, will rot | Make it a constant we bump, or omit entirely on `api.x.ai` |
| **`referrer` is misleading** | `hermes-agent` / `pi` | Use our own stable identifier |
| **Admin API key required** | `ADMIN_API_KEY` must be set or proxied routes return 500 | Optional; loopback-only binding is the real control. Generate-and-store a key by default, allow `--no-auth` |
| **Cloudflare Workers half** | ~40% of the codebase is for a hosted deployment | Drop it |
| **MCP server** | `/mcp` endpoint + tools | Drop it (not requested) |
| **npm packaging + update checker** | Extra surface, update-check phone-home | Drop it |

### 4.3 What to borrow

- The `AuthTokens` struct and the refresh/force-refresh control flow.
- The `mergeModelsList` approach (we may want to advertise `grok-4.7` even if
  upstream's list lags).
- The `trustedVerificationURL` idea.
- The header-stripping list.

---

## 5. OpenAI-compatible surface

Minimum viable, matching what OpenAI clients actually call:

| Endpoint | Notes |
| --- | --- |
| `POST /v1/chat/completions` | Must stream SSE correctly (`stream: true`) and pass through non-streaming JSON. Tool calls, `reasoning_effort`, `temperature`, `max_tokens`. |
| `GET /v1/models` | Proxy upstream, optionally merge aliases. |
| `POST /v1/responses` | Nice-to-have; `grok-4.7` always returns `reasoning.encrypted_content` on the Responses API. |
| `GET /v1/models/{id}` | Cheap to add. |
| `GET /health` | For `status` / `croncheck` liveness. |

Path handling: the reference strips a leading `/v1/` and re-prepends `/v1/`,
which makes both `/v1/chat/completions` and `/chat/completions` work. Worth
doing — some clients send the base URL with `/v1` already in it and some
without.

Auth: accept the key as `Authorization: Bearer`, `X-API-Key`, or `?key=`.
If no key is configured, accept anything (the server is loopback-only).

### Streaming caveat

The Grok Build changelog contains: *"Chat Completions streams that end without
a finish reason are retried instead of returning partial output."* Upstream
stream handling is not perfectly reliable; a proxy should be ready to buffer
and re-issue rather than leak a truncated stream to the client.

---

## 6. Daemon design

Requirements from the CLI spec: `run` backgrounds the server, `stop` ends it,
`croncheck` starts it only if it is not running, and `port` changes take effect
on restart.

**Recommendation: a self-contained daemon with a PID file — no launchd, no
systemd.**

- Works identically on macOS, Linux and in containers.
- No root, no install step, no platform branching.
- `croncheck` is trivial: PID liveness + a `/health` probe.

Details:

| Concern | Approach |
| --- | --- |
| Detach | `setsid()` / double-fork; child re-parents to init. Redirect stdio to the log file. |
| PID file | `~/.config/eggpass/eggpass.pid` — write PID after bind succeeds. |
| Liveness | PID exists **and** process alive (`kill(pid, 0)`) **and** `/health` responds on the configured port. PID reuse is a real hazard — the health probe disambiguates. |
| Stale PID | If the PID file exists but the process is dead, treat as not-running and clean up. |
| `stop` | `SIGTERM` → wait up to ~10s → `SIGKILL`. Graceful shutdown closes the listener and finishes in-flight requests. |
| Log | `~/.config/eggpass/eggpass.log`, with rotation or size cap. |
| `run` semantics | **Idempotent**: if already running, print the PID and exit 0. Never start a second instance. |
| Port change | Write the new port to config; the running daemon keeps the old port until `restart`. The CLI should say so explicitly. |
| Single-instance guard | Bind failure on the port is itself the guard; also check the PID file before starting. |

The reference project uses a macOS **launchd LaunchAgent** instead
(`install-launchagent.sh`). That is a legitimate alternative — it gives
auto-restart and boot persistence — but it is macOS-only and adds an install
step. A daemon-with-PID-file keeps the tool self-contained; launchd/systemd
can be offered later as an optional `install-service` command.

---

## 7. Config and paths

Use XDG-ish conventions, which macOS and Linux both tolerate:

```
~/.config/eggpass/
├── config.toml      # port, upstream, api key, log level
├── auth.json        # OAuth tokens (0600)
├── eggpass.pid
└── eggpass.log
```

Config format: **TOML** — it is what the official Grok CLI uses
(`~/.grok/config.toml`), it has no parser dependency in Go/Rust, and it
supports comments (which JSON does not, and users will want to annotate a
config file they edit with `$EDITOR`).

Defaults:

```toml
port = 8484
upstream = "api"          # "api" = api.x.ai, "build" = cli-chat-proxy.grok.com
api_key = ""              # empty = accept any key (loopback only)
log_level = "info"
```

`eggpass edit` should open this file in `$EDITOR` (falling back to
`$VISUAL`, then a sensible default). On this machine that means **helix** or
**vim** — both are what the user named.

---

## 8. Risks and open items

> **Update (2026-11-13).** Risk 1 is closed: `api.x.ai` was verified to
> accept the x.ai OAuth token through the finished proxy. The catalogue came
> back with thirteen models including `grok-4.7` and `grok-4.6`, and both a
> plain and a streaming `grok-4.7` completion succeeded. The upstream stays
> `api.x.ai`.

1. **Does `api.x.ai` accept the OAuth token for *our* subscription tier?**
   Verified by a third party on 2026-07-30, and by us on 2026-11-13 through the
   finished proxy. **Closed.**
2. **The public client id could change or be revoked.** It is embedded in a
   shipped binary, so xAI has an interest in keeping it working, but it is not
   contractual. Make it a config value with the current value as default, so it
   can be swapped without a release.
3. **The client-version floor on `cli-chat-proxy` can move.** Only relevant
   if we use the build upstream. Keep the version in one constant.
4. **Token lifetime ambiguity** (6h vs 7d vs 30d). Mitigated by always
   trusting `expires_at` from the server.
5. **`referrer` parameter** — unverified whether xAI validates the value.
   Test with `eggpass`; fall back to `grok-build`.
6. **Scope consent friction.** Requesting the full official scope set may
   show a longer consent screen. If `conversations:*` causes issues, drop to
   the reference project's shorter set — inference does not need them.
7. **Rate limits / fair-use.** A SuperGrok subscription's OAuth token may be
   subject to different limits than an API key. Unknown until tested.

---

## 9. Sources

- Official Grok Build CLI source — <https://github.com/xai-org/grok-build>
  (Rust, periodically synced from the xAI monorepo)
  - `crates/codegen/xai-grok-login/src/config.rs` — issuer, client id, scopes, referrer
  - `crates/codegen/xai-grok-login/src/oidc/protocol.rs` — `build_authorize_url`, PKCE
  - `crates/codegen/xai-grok-login/src/oidc/login.rs` — loopback flow, 600s timeout
  - `crates/codegen/xai-grok-login/src/device_code.rs` — RFC 8628 flow
  - `crates/codegen/xai-grok-login/src/model.rs` — `GrokAuth`, 30-day TTL fallback
  - `crates/codegen/xai-grok-config/src/endpoints.rs` — `cli_chat_proxy_base_url`
  - `crates/codegen/xai-grok-env/src/lib.rs` — `PROD_CLI_CHAT_PROXY_BASE_URL`
  - `crates/codegen/xai-grok-shell/src/remote/model_source/oai.rs` — upstream headers
- xAI OIDC discovery — `https://auth.x.ai/.well-known/openid-configuration` (verified live)
- xAI device-code endpoint — `https://auth.x.ai/oauth2/device/code` (verified live)
- Grok Build changelog — <https://x.ai/changelog/build> (v1.0.77, 2026-11-05)
- Reference proxy — <https://github.com/dvcrn/grok-oauth-proxy>
- Host/capability split and model catalog — `my-ai-api` npm package v0.3.10
  (`dist/providers/xai/capabilities.js`, `dist/providers/xai/config.js`)
- xAI model docs — <https://docs.x.ai/developers/models/grok-4.7>
