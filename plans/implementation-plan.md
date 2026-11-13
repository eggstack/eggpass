# Implementation plan

Phased build order for `eggpass`. Each phase lists the files it creates,
what it must do, and how we know it is done. **No phase starts until the
previous one is verified.**

Decisions this plan implements are recorded in
[`research.md`](research.md#locked-decisions-2026-11-13); the command
behaviour it implements is specified in [`cli-spec.md`](cli-spec.md).

---

## Ground rules

- **Rust stable only.** `rust-version = "1.89"`, `edition = "2024"`. No
  `rust-toolchain.toml` pin. No nightly features.
  *(Raised from 1.85 when the updater adopted `eggup`'s crate family, which
  requires 1.89.)*
- **No async runtime.** Blocking, thread-per-connection. This keeps the
  dependency tree small and SSE proxying straightforward.
- **Secrets never logged.** Token values are redacted everywhere, including
  error messages.
- **Every command is idempotent** and safe under `cron`.
- **Write code that a stranger can maintain.** No clever abstractions; prefer
  boring, well-named functions and explicit error types.

---

## Module layout

The layout as it stands after the eggpass work. The original eight-phase
layout is marked; everything else was added by the multi-provider phase.

```
src/
├── main.rs              # arg parsing + dispatch
├── cli/
│   ├── mod.rs           # subcommand dispatch, shared output helpers
│   ├── args.rs          # hand-rolled parser (no clap)
│   ├── prompt.rs        # the interactive provider picker (raw mode via nix)
│   └── commands/
│       ├── mod.rs
│       ├── login.rs     # login / logout
│       ├── status.rs    # status
│       ├── run.rs       # run / stop / restart / croncheck
│       ├── address.rs   # the shared port/host apply-and-restart machinery
│       ├── port.rs      # port
│       ├── host.rs      # host
│       ├── update.rs    # update
│       └── edit.rs      # edit
├── config.rs            # Config struct, TOML load/save, defaults
├── paths.rs             # config dir, credential files, pid file, log file
├── migrate.rs           # the ~/.config/grokproxy → ~/.config/eggpass migration
├── update.rs            # the updater's policy; the mechanics are eggup's
├── provider/
│   ├── mod.rs           # ProviderId, ProviderSpec, RequestPolicy, routing
│   ├── xai.rs           # x.ai's constants and spec
│   └── openai.rs        # ChatGPT's SIWC constants and spec
├── auth/
│   ├── mod.rs
│   ├── pkce.rs          # verifier + S256 challenge
│   ├── client.rs        # authorize URL, token exchange, refresh, revoke
│   ├── flow.rs          # loopback browser flow, device-code flow
│   ├── store.rs         # per-provider credential files (atomic, 0600)
│   └── refresh.rs       # proactive refresh with skew + rotation handling
├── upstream.rs          # the outbound HTTP client and header rules
├── server/
│   ├── mod.rs           # listener, thread-per-connection, shutdown
│   ├── http.rs          # the HTTP/1.1 server
│   ├── router.rs        # path normalisation and matching
│   ├── chat.rs          # /v1/chat/completions, /v1/responses
│   ├── translate.rs     # Chat Completions ↔ Responses, for ChatGPT
│   ├── aggregator.rs    # the Responses SSE stream → one answer
│   ├── models.rs        # /v1/models, /v1/models/{id} — live
│   ├── health.rs        # /health
│   └── sse.rs           # streaming passthrough
├── daemon.rs            # daemonize, PID file, liveness, stop
└── logging.rs           # log file, levels, redaction
```

---

## Dependencies

| Crate | Purpose | Notes |
| --- | --- | --- |
| `serde` + `serde_json` | JSON | `derive` feature |
| `toml` | Config file | Serialize + deserialize |
| `ureq` | Outbound HTTP | rustls, streaming response reader for SSE |
| `sha2` | PKCE challenge | `Sha256` |
| `base64` | base64url | `URL_SAFE_NO_PAD` engine |
| `rand` | Randomness | `state`, `nonce`, PKCE verifier |
| `daemonize` | Daemonization | double-fork, `setsid`, PID file |
| `signal-hook` | Signals | graceful `SIGTERM` |
| `nix` | Process control and the picker | `kill(pid, 0)` liveness, signal sending, and raw-mode termios for the picker (the `term` feature) |
| `dirs` | Paths | config directory resolution |
| `webbrowser` | OAuth | open the authorize URL |
| `eggup-core` | Updater | the transaction, the ownership verifier, the validators |
| `eggup-acquisition` | Updater | fetching the manifest and the artifact |
| `eggup-eggpack` | Updater | projecting a release manifest for a target |
| `eggup-eggfetch` | Updater | the production transport |

Dev-dependencies: `tempfile` (credential-store tests). The stub upstream is
written directly against a socket in `src/server/tests.rs`.

Deliberately excluded: `tokio`/`async`/`axum`/`reqwest`, `keyring`, `clap`,
`tiny_http`, `httptest`, `wiremock`, `crossterm`.

`crossterm` was in the dependency list for the interactive picker, and was
dropped when the picker was rewritten on `nix`'s termios — see
[`shipped-2026-11-13.md`](shipped-2026-11-13.md#the-picker-was-rendered-wrongly).

---

## Phase 1 — Skeleton, config and paths

**Files:** `Cargo.toml`, `src/main.rs`, `src/cli/{mod,args}.rs`,
`src/config.rs`, `src/paths.rs`, `src/logging.rs`

**Work**

1. `cargo init --bin --vcs git` style scaffold, name `eggpass`.
   Set `rust-version = "1.85"`, `edition = "2024"`, `resolver` inherited from
   edition 2024 (v3, MSRV-aware dependency resolution).
2. `paths.rs` resolves, in order:
   - `$EGGPASS_HOME` if set,
   - else `$XDG_CONFIG_HOME/eggpass` if set,
   - else `~/.config/eggpass`.
   Files: `config.toml`, `auth.json`, `eggpass.pid`, `eggpass.log`.
3. `config.rs`:
   ```toml
   port = 8484
   upstream = "api"      # "api" | "build"
   api_key = ""          # empty = accept any key
   log_level = "info"    # error | warn | info | debug
   ```
   Load: missing file → defaults, do not error. Malformed file → hard error
   naming the file and the parse position (never silently reset it).
   Save: preserve unknown keys and comments as far as `toml` allows; at
   minimum never drop keys it does not understand.
4. `args.rs`: hand-rolled parser for `--config`, `--verbose`, `--help`,
   `--version`, and the nine subcommands. Unknown flags → usage error, exit 1.
   A tenth command, `host`, is added by Phase 1 of
   [`plan-eggpass.md`](plan-eggpass.md), along with `--provider` and the
   `EGGPASS_PROVIDER` / `EGGPASS_HOST` / `EGGPASS_PORT` environment
   overrides.
5. `logging.rs`: writes to the log file and stderr; levels per config;
   **redacts** any value that looks like a token.

**Done when**

- `eggpass --version` prints a version.
- `eggpass --help` lists the nine commands.
- A missing config file does not error; a malformed one does, with a clear message.
- `cargo test` passes and `cargo clippy -- -D warnings` is clean.

---

## Phase 2 — OAuth core

**Files:** `src/auth/{mod,pkce,client,flow,store}.rs`

This is the riskiest phase — it is the first thing that talks to x.ai.

**Work**

1. **PKCE** (`pkce.rs`): verifier = 32 random bytes → `URL_SAFE_NO_PAD` base64.
   Challenge = `URL_SAFE_NO_PAD(SHA256(verifier))`. `state` and `nonce` = 16
   random bytes each.
2. **Constants** (`client.rs`) — all in one place, all overridable by config
   so a future client-id change needs no release:

   | Constant | Value |
   | --- | --- |
   | `CLIENT_ID` | `b1a00492-073a-47ea-816f-4c329264a828` |
   | `ISSUER` | `https://auth.x.ai` |
   | `AUTHORIZE_URL` | `https://auth.x.ai/oauth2/authorize` |
   | `TOKEN_URL` | `https://auth.x.ai/oauth2/token` |
   | `DEVICE_CODE_URL` | `https://auth.x.ai/oauth2/device/code` |
   | `USERINFO_URL` | `https://auth.x.ai/oauth2/userinfo` |
   | `SCOPES` | `openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write` |
   | `REFERER` | `eggpass` (fallback `grok-build`) |

3. **Loopback flow** (`flow.rs`):
   - Bind `127.0.0.1:0` (ephemeral port). **Never the API port.**
   - Build the authorize URL exactly as the official CLI does:
     `response_type=code`, `client_id`, `redirect_uri`,
     `scope`, `code_challenge`, `code_challenge_method=S256`, `state`,
     `nonce`, `referrer`.
   - Open the browser at that URL; also print the URL so it can be opened
     manually when no browser is available.
   - Serve `GET /callback`; verify `state` matches before exchanging the code.
     Reject a mismatched `state` and any `error`/`error_description`.
   - Timeout: **600 seconds**, then exit with a clear message.
   - Exchange the code at the token endpoint; persist the tokens.
4. **Device flow** (`flow.rs`):
   - `POST` the device-code request with `client_id`, `scope`, `referrer`.
   - **Validate `verification_uri` against a trusted-host allowlist**
     (`accounts.x.ai`, `auth.x.ai`) before displaying or opening it. Never
     open a provider-supplied URL unchecked.
   - Poll the token endpoint every `interval` seconds (default 5), sleeping
     *before* the first poll. Handle `authorization_pending`,
     `slow_down` (increase interval), `access_denied`, `expired_token`.
   - Honor `expires_in` (30 min) as the hard deadline.
5. **Token exchange** (`client.rs`):
   ```
   POST https://auth.x.ai/oauth2/token
   Content-Type: application/x-www-form-urlencoded
   grant_type=authorization_code &code=… &redirect_uri=… &client_id=… &code_verifier=…
   ```
   Do **not** send `code_challenge_method` in the token request — the verifier
   is sufficient and the parameter is not in RFC 6749.
6. **Store** (`store.rs`): `auth.json` at `~/.config/eggpass/auth.json`,
   directory `0700`, file `0600`, written atomically (temp file in the same
   directory + `rename`). Fields: `access_token`, `refresh_token`,
   `expires_at` (unix seconds), `token_type`, `id_token`, `scope`, `client_id`,
   `issuer`, `obtained_at`.
7. **Import** (`store.rs`): if our `auth.json` is absent, try
   `~/.grok/auth.json` — the official Grok Build CLI's file — and convert a
   usable entry (`key` → `access_token`, `refresh_token`, `expires_at`,
   `user_id`, `email`). This lets a user who already ran `grok login` skip
   our login entirely.

**Done when**

- `eggpass login` completes a real browser login against x.ai and writes
  `auth.json` with mode `0600`.
- `eggpass login --device` completes against x.ai from a headless shell.
- `eggpass logout` removes the file; `eggpass logout` again is a no-op
  that exits `0`.
- A tampered `state` on the callback is rejected.
- A `verification_uri` pointing at an untrusted host is refused.

---

## Phase 3 — Token refresh

**Files:** `src/auth/refresh.rs`, `src/auth/client.rs`

**Work**

1. `refresh_token` grant:
   ```
   grant_type=refresh_token &refresh_token=… &client_id=…
   ```
2. **Rotation handling — the subtle part.** If the response omits
   `refresh_token`, keep the previous one. If it returns a new one, store it.
   Losing this breaks the second refresh.
3. **Proactive refresh**: refresh when the access token expires within
   **5 minutes**. Use the server's `expires_at` when present; otherwise fall
   back to a conservative TTL (30 days, matching the official CLI's
   `TOKEN_TTL`).
4. **Concurrency**: a mutex around refresh, and re-read the store *inside* the
   lock so a refresh performed by another process is not clobbered. If the
   token is already fresh after re-reading, return it without refreshing.
5. **Forced refresh** after an upstream `401`: refresh only if the failing
   access token is still the current one. Otherwise a burst of parallel
   requests triggers a refresh stampede.
6. On refresh failure, return a clear `401` to the client
   ("token expired, run `eggpass login`"), never a generic `500`.

**Done when**

- An expired access token is refreshed transparently before a request goes out.
- A refresh response without `refresh_token` does not break the next refresh.
- A refresh failure surfaces a `401` with an actionable message.

---

## Phase 4 — Upstream client

**Files:** `src/upstream.rs`

**Work**

1. Two upstreams behind one trait:

   | `upstream` | Base URL | Extra headers |
   | --- | --- | --- |
   | `"api"` (default) | `https://api.x.ai/v1` | none |
   | `"build"` | `https://cli-chat-proxy.grok.com/v1` | `X-XAI-Token-Auth: xai-grok-cli`, `x-grok-client-version: <VERSION>` |

   `VERSION` is a single constant (currently `1.0.77`, the current Grok Build
   release) and is only sent on the `build` upstream, which returns `426`
   below `1.0.13`.
2. **Header hygiene** — before forwarding, strip from the incoming request:
   `Authorization`, `X-Api-Key`, `X-Api-Key-*`, `Host`, `Content-Length`,
   `Accept-Encoding`, `Connection`, `Transfer-Encoding`, `Upgrade`,
   `Forwarded`, `X-Forwarded-*`, `Cf-*`, `X-Real-Ip`, `Cdn-Loop`.
   Then set `Authorization: Bearer <grok token>`.
   **The client's key must never reach x.ai.**
3. **Path normalisation**: accept both `/v1/chat/completions` and
   `/chat/completions`; strip a leading `/v1` and re-apply `/v1` so the
   upstream always sees `/v1/...`. Also strip a `key` query parameter before
   forwarding.
4. **Retry-once on `401`**: force-refresh the token and retry once. No second
   retry (avoid loops).
5. **Model list**: `GET /v1/models` upstream, pass through. Optionally merge
   aliases we want to advertise (`grok-4.7` etc.) that upstream does not list,
   without dropping upstream entries.

**Done when**

- `GET /v1/models` returns the upstream catalog.
- A client-supplied `Authorization` header does not appear in the upstream
  request (assert in a test with a stub server).
- A `401` from upstream triggers exactly one refresh + retry.

---

## Phase 5 — HTTP server

**Files:** `src/server/*`

**Work**

1. `tiny_http` server on `127.0.0.1:<port>`. Thread-per-connection.
   **Loopback only** — never `0.0.0.0`.
2. Routes:

   | Route | Behaviour |
   | --- | --- |
   | `POST /v1/chat/completions` | Forward; support streaming and non-streaming |
   | `POST /v1/responses` | Forward |
   | `GET /v1/models` | Forward |
   | `GET /v1/models/{id}` | Forward or synthesise from the list |
   | `GET /health` | Liveness, no auth |
   | anything else | `404` in OpenAI error shape |

3. **Auth**: if `api_key` is configured, require it as `Authorization: Bearer`,
   `X-Api-Key`, or `?key=`. If not configured, accept anything — the server is
   loopback-only. `401` in OpenAI error shape when required and absent.
4. **OpenAI error shape**:
   ```json
   {"error": {"message": "...", "type": "invalid_request_error", "code": null}}
   ```
5. **Streaming** (`sse.rs`): when the client sends `"stream": true`, forward
   the upstream response as `text/event-stream` **without buffering** — copy
   chunks from the upstream reader to the client writer as they arrive and
   flush each time. Set `Cache-Control: no-cache`,
   `Connection: keep-alive`, `X-Accel-Buffering: no`.
6. **Non-streaming**: buffer and pass the JSON through unchanged, preserving
   the upstream status code.
7. **Graceful shutdown**: `SIGTERM` → stop accepting new connections, let
   in-flight requests finish (bounded, ~5s), then exit.
8. Do not follow redirects upstream; pass upstream status codes through
   faithfully.

**Done when**

- A non-streaming chat completion round-trips correctly.
- A streaming chat completion arrives at the client incrementally (verify by
  timing chunk arrival, not just by content).
- `401` without a key when a key is configured.
- `GET /health` answers without auth.
- `SIGTERM` shuts the server down promptly.

---

## Phase 6 — Daemon lifecycle

**Files:** `src/daemon.rs`, `src/cli/commands/run.rs`

**Work**

1. **PID file** at `~/.config/eggpass/eggpass.pid`.
2. **Liveness check** — a daemon is "running" only if **both**:
   - the PID in the file is alive (`kill(pid, 0)`), **and**
   - `GET /health` on the configured port responds.
   The PID check alone is defeated by PID reuse; the health check alone can be
   satisfied by a different process on the same port. Together they are
   reliable.
3. **`run`**: if already running → print PID and URL, exit `0`, do not start a
   second instance. Otherwise `daemonize` (double-fork + `setsid` + stdio
   redirect to the log file), write the PID, then wait up to ~10s for
   `/health`. If it never comes up, print the tail of the log and exit `1`.
4. **`stop`**: read the PID; if not alive, remove the stale PID file, print
   "not running", exit `0`. Otherwise `SIGTERM`, wait up to 10s, then
   `SIGKILL`. Remove the PID file.
5. **`restart`**: `stop` then `run`.
6. **`croncheck`**: if running → do nothing, exit `0`, **write nothing to
   stdout**. If not running → start it, exit `0`. All diagnostics go to the
   log file so a crontab entry stays silent.

**Done when**

- `run` twice does not create two daemons.
- `stop` then `run` cycles cleanly.
- `croncheck` in a crontab produces no output when the server is already up.
- Killing the process by hand leaves a stale PID file that `status` reports
  correctly as "stopped" and `run` cleans up.

---

## Phase 7 — Remaining commands

**Files:** `src/cli/commands/{status,port,host,edit}.rs`

> **Update (2026-11-13).** `port` was built as described below — config-only,
> no implicit restart — and `host` did not exist. Both are **superseded** by
> the design in [Part 3.8.1 of `plan-eggpass.md`](plan-eggpass.md#381-the-address-commands).
> The text below is kept as the record of what was originally specified and
> what shipped first; the superseded behaviour is the "never restart implicitly"
> rule, which the new `port`/`host` replace.

**Work**

1. **`status`** — print both sections always, in the fixed shape from
   `cli-spec.md`: account (email, user id, token validity) and server
   (running/stopped, pid, url, upstream, model count). Exit `0` always.
2. **`port`** — no argument prints the current port and URL. With an
   argument: validate (integer, 1–65535, not privileged), write to config,
   and report that it takes effect on the next restart. **Never restart
   implicitly.** If the daemon is running, print both the new and the
   currently-serving port.
3. **`edit`** — create the config with defaults if missing, open `$EDITOR`
   (fallback `$VISUAL`, then `helix`, then `vim`, then `nano`). After the
   editor exits, validate the file and report parse errors **without**
   overwriting the user's file.

**Done when**

- `status` output is stable across runs and greppable.
- `port 9000` then `port` prints `9000`; the running daemon still serves the
  old port until `restart`.
- `port 70000` is rejected with a clear message and exit `1`.
- `edit` opens the right editor and reports a syntax error without losing it.

> **As shipped.** `port` and `host` now validate, compare against the pid
> file, probe the address, then stop → write → start, with a rollback. The
> "never restart implicitly" rule above is **no longer the behaviour**: it is
> superseded. The live verification is in
> [`shipped-2026-11-13.md`](shipped-2026-11-13.md).

---

## Phase 8 — Packaging, tests, hardening

**Work**

1. **Tests**
   - Unit: PKCE vectors, base64url no-pad, config round-trip, token parsing,
     refresh-rotation logic, header-stripping list, path normalisation.
   - Integration (stub upstream): chat completion pass-through, streaming
     pass-through, `401` → refresh → retry, key enforcement.
   - Never hit the network in a test except the explicitly-marked manual
     verification script below.
2. **Manual verification script** (`scripts/verify-live.sh`) — the only place
   that talks to x.ai for real:
   - device-code login → token stored
   - `GET /v1/models` returns `grok-4.7` and `grok-4.6`  ← **this validates
     open risk #1** (does `api.x.ai` accept our OAuth token?)
   - a chat completion and a streaming chat completion
   - token refresh after expiry
3. **Packaging** — `cargo install --path .`; a `release` profile with
   `strip = true`, `lto = true`, `codegen-units = 1`, `panic = "abort"`.
4. **Hardening**
   - `config.toml` `0600` if it ever holds a key; `auth.json` always `0600`.
   - Never log `access_token`, `refresh_token`, or `id_token`.
   - Redact `Authorization` in request logs.
   - Refuse to start if the configured port is privileged (<1024) unless
     explicitly overridden.
5. **CI** — `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
   on push.
6. **Docs** — final `README.md` with install, usage, client setup examples
   (OpenCode, Cline, Continue, OpenAI SDK), and troubleshooting.

**Done when**

- `cargo test` and `cargo clippy -- -D warnings` pass.
- `scripts/verify-live.sh` passes end to end against the real x.ai.
- The release binary runs from a clean machine with no Rust toolchain present.

---

## Sequencing rationale

OAuth (Phase 2) comes first because it is the only part that requires an
external account and an interactive browser — it is the most likely to
surprise us, and every other phase depends on it. The upstream reachability
test (Phase 4) comes early for the same reason: it resolves open risk #1, and
if `api.x.ai` turns out to reject the OAuth token, the whole upstream choice
changes. The server (Phase 5) and daemon (Phase 6) are the parts most likely
to go smoothly, so they come after the risky unknowns are retired.

---

## Open risks carried into implementation

| # | Risk | Mitigation | Verified in |
| --- | --- | --- | --- |
| 1 | `api.x.ai` may not accept the OAuth token for our subscription tier | `upstream` is a config value; fall back to `"build"` | Phase 8 live script |
| 2 | The public client id could change or be revoked | Config-overridable constant | — |
| 3 | `referrer` value may be validated by x.ai | Try `eggpass`, fall back to `grok-build` | Phase 2 |
| 4 | Full scope set may cause consent friction | Fall back to the shorter set used by the reference | Phase 2 |
| 5 | Token lifetime ambiguity (6h / 7d / 30d) | Always trust the server's `expires_at`; conservative fallback | Phase 3 |
| 6 | Upstream streams can end without a finish reason | Buffer-and-retry at the client boundary, not mid-stream | Phase 5 |
| 7 | Rate limits differ between OAuth and API-key billing | Measure during Phase 8 | Phase 8 |

---

## Definition of done

- All nine commands behave exactly as `cli-spec.md` describes — and the tenth,
  `host`, plus the eleventh, `update`.
- `eggpass login && eggpass run` yields a working OpenAI-compatible
  endpoint serving `grok-4.7` / `grok-4.6`.
- `eggpass croncheck` is safe as a 5-minute crontab entry.
- The dependency list matches the table above with nothing extra.
- `cargo test` green, `clippy` clean, `fmt` clean.
- No token material in logs, in the repository, or in a crash dump.

---

## What shipped (2026-11-13)

All eight phases are implemented, committed and verified. The notes below
record the places where the build did not match this plan, and why.

The multi-provider phase — the rename, the provider layer, the ChatGPT SIWC
path, the picker, the address commands and the updater — is recorded
separately in [`shipped-2026-11-13.md`](shipped-2026-11-13.md).

### Added after the plan: `eggpass` issues the client keys

The plan had one key for clients — the `api_key` in the config file, written by
hand. `eggpass` now issues its own:

```
eggpass key add [LABEL]   mint a key and show it once
eggpass key list           the numbers, the visible parts, the labels
eggpass key revoke <N>     take one away
```

`src/keys.rs` holds the store: a per-key random salt and the SHA-256 of
`salt || key`, never the key itself, in `~/.config/eggpass/keys.json` at mode
`0600`. The numbers are stable and never reused, so `revoke 2` always means
the same key. The store has one writer — this command — and the server re-reads
it whenever it changes, so a key takes effect on the next request with no
restart.

One behaviour changed with it: a server bound outside loopback with no key at
all now refuses every request rather than serving as an open proxy.

### Open risk #1 is resolved
`api.x.ai` **does** accept the x.ai OAuth token. Verified live on
2026-11-13 through the proxy: `GET /v1/models` returned thirteen models
including `grok-4.7` and `grok-4.6`, and a `grok-4.7` chat completion and a
streaming completion both succeeded. The upstream stays `api.x.ai`; the
`"build"` upstream remains available in the config.

The token used for that check was imported from the official Grok CLI's
`~/.grok/auth.json`, which also exercised the import path and the refresh
path: the imported access token was expired, and the proxy refreshed it
transparently on the first request, keeping the refresh token.

### Deviations from the plan

| Plan | What shipped | Why |
| --- | --- | --- |
| `tiny_http` for the inbound server | A small HTTP/1.1 server in `src/server/http.rs` | `tiny_http` wraps the response socket in a hard-coded one-kilobyte `BufWriter` that is only flushed when the response ends. Streaming responses would have been delivered in one-kilobyte bursts and the first token of every answer would have been withheld. The replacement is about three hundred lines, flushes after every chunk, and removes a dependency. |
| `httptest` or `wiremock` for the stub upstream | A stub upstream written directly against a socket, in `src/server/tests.rs` | Fewer dependencies, and it can stream, which `httptest` cannot do without its own buffering. |
| `src/auth/store.rs` holds the credential format | Unchanged | — |
| A separate `src/timeutil.rs` module | Added | The plan had no home for RFC 3339 conversion, which is needed to read `expires_at` out of the official Grok CLI's credential file. |
| The config file has four keys | Five — `allow_privileged_port` was added | Phase 8 requires refusing to bind a privileged port "unless explicitly overridden", which needs a key to override it with. |
| The pid file holds a pid | It holds a pid **and** the port the daemon is serving | Without it, `status` and `port` cannot report the port that is live right now, which differs from the configured port after an `eggpass port` change. A third line, the bound host, is added by Phase 1 of `plan-eggpass.md`, so that `host` can tell what is actually being served. |

### The dependency list, as built

Runtime: `serde`, `serde_json`, `toml`, `ureq`, `sha2`, `base64`, `rand`,
`daemonize`, `signal-hook`, `nix`, `dirs`, `webbrowser`.
Development: `tempfile`.

That is the plan's table minus `tiny_http`, with nothing added.

*As of the multi-provider phase the runtime list also carries the four `eggup`
crates (the updater's mechanics). The full list is in
[`shipped-2026-11-13.md`](shipped-2026-11-13.md).*

### Verification

- `cargo test` — 139 tests, no network.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `scripts/verify-live.sh` — passes end to end against the real x.ai:
  device-code login, model catalogue, single model, chat completion, streaming
  completion, client-credential stripping, token refresh, clean shutdown.
