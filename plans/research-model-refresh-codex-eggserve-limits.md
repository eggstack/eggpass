# Research: model-list refresh, a ChatGPT/Codex auth path, HTTP server choice, and plan limits

Date: 2026-11-13. Research only — no code changed.

**How the findings were obtained.** The x.ai numbers below were verified
first-hand against the live API with the working OAuth credential
(`api.x.ai`, `cli-chat-proxy.grok.com`, `grok.com`). The Codex numbers were
read out of the Codex CLI's own source at `openai/codex` `main`. Third-party
claims are labelled as such and separated from what was verified.

---

## 1. How the model list is refreshed today

**Short answer: it is never cached. Every `GET /v1/models` is a live
round-trip to x.ai.**

There are exactly two pieces of model data, and they behave very differently.

| Piece | Where it lives | How fresh is it |
| --- | --- | --- |
| The upstream catalogue | `api.x.ai/v1/models` | Fetched live, per request |
| Our extra entries | `KNOWN_MODELS`, a hardcoded `const &[&str]` in `src/upstream.rs:26` | Frozen at compile time |

`src/server/models.rs:8` handles `GET /v1/models` by calling
`upstream::send_with_auth(...)` against `/v1/models` on every single request,
then merging the result with `KNOWN_MODELS` via `upstream::merge_models`
(`src/upstream.rs:378`). Upstream entries are never dropped; `KNOWN_MODELS`
only adds the eight ids that are missing:

```
grok-4.7, grok-4.6, grok-4.5, grok-4.3,
grok-4.20-0309-reasoning, grok-4.20-0309-non-reasoning,
grok-4.20-multi-agent-0309, grok-build-0.1
```

There is no cache anywhere in the crate — `grep` for `cache`/`ttl` across
`src/` returns only `Cache-Control: no-cache` on the SSE path. So "how often
do we refresh the model list" has two answers:

- **The live half: on every request.** No TTL, no ETag, no `If-None-Match`.
- **The static half: on every rebuild.** `KNOWN_MODELS` only changes when the
  binary is recompiled. A new model that x.ai ships will appear in the live
  catalogue immediately; a model x.ai *drops* will keep showing up in our list
  forever, because we never remove entries.

### Two consequences worth knowing

**A. `GET /v1/models/{id}` answers 200 for every id, including invented ones.**
`src/server/models.rs:58` intercepts an upstream 404 and returns a synthetic
`{"id": ..., "object": "model", "owned_by": "xai"}`. So
`GET /v1/models/totally-made-up` returns 200. The API therefore cannot answer
"does this model exist", and a client that probes the catalogue to validate a
model name gets a false positive.

**B. `status` makes its own live call.** `model_count` in
`src/cli/commands/status.rs:90` issues its own `GET /v1/models` upstream, but
only when the stored token is still fresh (`auth.is_fresh(now, 0)`); otherwise
it prints `unavailable (the token has expired)`.

### Is the live fetch on `/v1/models` expensive?

**No — verified first-hand.** `GET https://api.x.ai/v1/models` returns **no
`x-ratelimit-*` headers at all**, and does not visibly consume the request
budget. So the per-request catalogue fetch is cheap. That weakens the usual
argument for adding a TTL cache: the cost argument is weak, but the
*correctness* argument (never dropping a model, always reflecting a live
catalogue) is what we are currently buying, and it is real.

If a cache is ever wanted, the TTL would want to be short-ish and the
`KNOWN_MODELS` union would want to change from "additive forever" to
"additive, but let upstream removals win after a grace period".

---

## 2. Adding a ChatGPT/Codex OAuth path

The shape is genuinely parallel to the x.ai path — same PKCE, same
authorization-code grant, same refresh-with-rotation. But there are two
important structural differences, and one of them changes the design.

### 2.1 The constants (read out of `openai/codex` source)

| Constant | Value | Source |
| --- | --- | --- |
| Issuer | `https://auth.openai.com` | `codex-rs/login/src/server.rs:78` (`DEFAULT_ISSUER`) |
| Authorize endpoint | `{issuer}/oauth/authorize` | `server.rs:609` |
| Token endpoint | `{issuer}/oauth/token` | `server.rs:722` |
| Client id | `app_EMoamEEZ73f0CkXaXp7hrann` | Codex CLI's public client id (used by OpenCode and several third-party proxies) |
| Scope | `openid profile email offline_access api.connectors.read api.connectors.invoke` | `server.rs:613` |
| Redirect URI | `http://127.0.0.1:{port}/auth/callback` | `server.rs:197` |
| Allowed callback ports | **1455**, fallback **1457** | `server.rs:79-81` — "Keep in sync with the Codex CLI Hydra redirect URI allow-list" |
| Inference base URL | `https://chatgpt.com/backend-api/codex` | `codex-rs/model-provider-info/src/lib.rs:87` |

### 2.2 The browser flow has a **fixed** callback port — unlike x.ai

This is the single biggest difference from our current implementation.

Our x.ai flow binds a **random ephemeral port** and sends `redirect_uri` with
that port. The ChatGPT client registration does **not** allow that: OpenAI's
Hydra allow-list contains exactly two ports, and the Codex CLI works around a
collision by trying the second one:

```rust
const DEFAULT_PORT: u16 = 1455;
// Keep in sync with the Codex CLI Hydra redirect URI allow-list.
const FALLBACK_PORT: u16 = 1457;
```

`bind_server` (`server.rs:641`) tries `127.0.0.1:1455` first and falls back to
`127.0.0.1:1457`. So a ChatGPT browser login is **not** free-port — it will
collide with a real Codex CLI session, and the design has to accept that.

Note also that Codex's *generic* provider-OAuth path
(`codex-rs/login/src/oauth_session_callback.rs:38`) does bind
`127.0.0.1:0` when no port is configured — so ephemeral ports are possible in
general, just not with the ChatGPT client registration.

### 2.3 There is a **device-code flow** that needs no local port at all

`codex-rs/login/src/device_code_auth.rs` implements a device flow that is a
better fit for a headless proxy than the browser flow:

| Step | Request | Response |
| --- | --- | --- |
| 1 | `POST {issuer}/api/accounts/deviceauth/usercode` body `{"client_id": "..."}` | `{device_auth_id, user_code, interval}` |
| 2 | user opens `{issuer}/codex/device` and enters `user_code` | — |
| 3 | poll `POST {issuer}/api/accounts/deviceauth/token` body `{"device_auth_id", "user_code"}` | on success `{authorization_code, code_challenge, code_verifier}` |
| 4 | exchange the code at `{issuer}/oauth/token` with `redirect_uri = {issuer}/deviceauth/callback` | tokens |

Two details that make this pleasant for us:

- **The redirect URI is remote** (`https://auth.openai.com/deviceauth/callback`).
  There is no local listener, no port, no collision with Codex CLI.
- **The server generates the PKCE pair** — `code_verifier` and
  `code_challenge` come back in step 3. We do not generate them ourselves.

The polling semantics: `403` and `404` mean "keep polling"; anything else is a
hard failure; 15-minute overall timeout. The device code is presented to the
user as a URL plus a one-time code.

### 2.4 Token shape

The token response carries `access_token`, `refresh_token` and `id_token`.
The `id_token` is a JWT whose payload holds:

- `email`
- `chatgpt_account_id` — read from the `https://api.openai.com/auth` claim
  (`login_server.rs:866`)
- `chatgpt_plan_type` — the plan, e.g. `plus`, `pro`, `team`

`chatgpt_account_id` must be sent as the **`ChatGPT-Account-Id`** request
header on every backend call (`codex-rs/backend-client/src/client.rs:388-391`).
Our proxy would need to carry that alongside the bearer token.

There is also a third-party source of tokens: Codex exchanges the `id_token`
for an API-key-style access token via a token-exchange grant
(`login_server.rs:1036-1058`):

```
grant_type=urn:ietf:params:oauth:grant-type:token-exchange
requested_token=openai-api-key
subject_token=<id_token>
subject_token_type=urn:ietf:params:oauth:token-type:id_token
```

That is the path that would turn a ChatGPT subscription into a normal
`api.openai.com` API key. In the Codex CLI it is best-effort — the `.ok()` on
the result means a failure is silently ignored.

### 2.5 The inference surface is **not** the same as Grok's

This is where the "similar path" idea gets interesting, because the WHAM
backend is not a drop-in for `api.x.ai`:

| | x.ai (`api.x.ai`) | ChatGPT backend (`chatgpt.com/backend-api/codex`) |
| --- | --- | --- |
| Inference API | `/v1/chat/completions`, `/v1/responses` | `/responses` (Responses API) |
| Model list path | `/v1/models` | `/models?client_version=…` |
| Model list schema | `{ "data": [ { "id": … } ] }` | `{ "models": [ { "slug": … } ] }` |
| Required extras | — | `store=false`, `instructions` present, content type `input_text` |
| Extra header | — | `ChatGPT-Account-Id` |
| Statelessness | — | Full conversation history resent every turn |

The models-schema difference is the part that would bite us: our `/v1/models`
returns `data`/`id` because that is the OpenAI-compatible shape, and a Codex
backend returns `models`/`slug`. Serving a Codex-backed catalogue through our
existing endpoint means a **translation layer**, not a passthrough.

The Responses-API differences mean that exposing ChatGPT through
`/v1/chat/completions` requires translating chat completions into the
Responses request shape and back. That is a real project, not a config change.

### 2.6 What adding it would actually involve

Roughly, in dependency order:

1. A second credential store entry (or a second file), since ChatGPT tokens
   and x.ai tokens are different issuers with different lifetimes.
2. A second OAuth flow implementation. The PKCE and state machinery in
   `src/auth/pkce.rs` and `src/auth/flow.rs` is reusable; the x.ai
   device-code flow is the closest template, but OpenAI's device endpoints
   and PKCE-from-server semantics differ from x.ai's.
3. A second `TokenProvider` implementation — the `TokenProvider` trait in
   `src/upstream.rs:76` is exactly the right seam, and `send_with_auth`
   already does the one-refresh-and-retry-on-401 dance generically.
4. An upstream kind for the ChatGPT backend (alongside `api` and `build`),
   with the `ChatGPT-Account-Id` header attached — which means the current
   `extra_headers()` mechanism, which is keyed on `Upstream::Api`/`Upstream::Build`,
   would need to carry the account id, not just a static string.
5. Model-list translation (`models`/`slug` → `data`/`id`).
6. A request/response translation layer if `/v1/chat/completions` is to work.

### 2.7 The risk, stated plainly

The Codex CLI's OAuth client id is OpenAI's own. The `7shi/codex-oauth`
README is blunt about it:

> The API behavior documented here was reverse-engineered from the Codex CLI
> and OpenCode source code and is not officially documented by OpenAI. As of
> the time this repository was created, OpenAI does not explicitly prohibit
> third-party OAuth usage with these credentials, but this policy may change
> at any time. **USE AT YOUR OWN RISK.**

Using the ChatGPT subscription backend through a third-party proxy is not
officially supported by OpenAI. The x.ai path we built today is materially
safer: x.ai published the OAuth constants, published the client id, and
publishes the endpoints. OpenAI publishes the endpoints but not the
third-party-use permission.

---

## 3. `eggserve` versus the handrolled HTTP server

### What `eggserve` actually is

`eggserve` is a real, maintained crate family from the `eggstack` org
(`github.com/eggstack/eggserve`). It is **a hardened static file server** —
a "secure alternative to `python -m http.server`" — with a reusable runtime
underneath. The crate family is:

| Crate | Version | Role |
| --- | --- | --- |
| `eggserve-server` | 0.5.0 | The H1 runtime and transport boundary |
| `eggserve-primitives` | 0.2.3 | Transport-neutral request/response/policy values |
| `eggserve-core` | 0.5.0 | Composition layer; owns H2/TLS/PROXY/listener |
| `eggserve-static` | 0.5.1 | Hardened static-file service |
| `eggserve-h3` | 0.5.0 | Experimental HTTP/3 and QUIC adapter |
| `eggserve-bin` | 0.2.3 | The CLI binary |

It is a young project: created 2026-08-18, latest release 2026-10-22, about
9,175 downloads on `eggserve-server`. It is genuinely well-engineered — the
repo has ~290 numbered "plans", a threat model, a qualification record per
protocol, conformance suites, and a downstream-embedding contract.

### The decisive fact: it requires Tokio

`eggserve-server` 0.5.0's dependency list, from the crates.io API:

```
hyper ^1            (features: http1, server)
hyper-util ^0.1     (features: tokio, server, http1)
tokio ^1            (features: macros, net, time, io-util, fs, sync, rt)
bytes, futures-util, http-body, http-body-util, httpdate
eggserve-primitives ^0.2.3
```

The service contract is `call(Request) -> Response` over an
`AsyncRead + AsyncWrite` stream — `architecture/eggserve-server.md` describes
it as "the sole H1 authority over any `AsyncRead + AsyncWrite` stream". The
Rust API is async end to end. There is **no blocking or thread-per-connection
Rust surface**; the synchronous `ThreadingHTTPServer` facade is Python-only.

### What it would cost us

Our project constraint is explicit: **no async runtime**, blocking,
thread-per-connection. Adopting `eggserve-server` would mean:

- Adding `tokio` + `hyper` + `hyper-util` + `http-body` + `http-body-util` +
  `futures-util` + `bytes` + `httpdate` — roughly eight new crates, and the
  async runtime we deliberately avoided, into a binary whose CLI, auth flow,
  daemon lifecycle and refresh logic are all written synchronously.
- Porting the router, the SSE passthrough, the key check and the graceful
  SIGTERM shutdown to async.
- `request_body_policy()` defaults to `Reject` in eggserve — bodies are
  rejected by default, so we would have to opt into Buffer/Stream explicitly.
- Our `Request`/`Response` types are ours; eggserve has its own canonical
  primitives, with `http`/`tower` interop only behind opt-in features.

### What it would buy us

Honestly, quite a lot of good engineering:

- A hardened, conformance-tested H1 parser instead of ours.
- Proper trailer, interim-response and tunnel semantics we do not implement.
- Per-profile resource limits (max connections, max headers, header timeout,
  body ceiling, keep-alive idle timeout) that eggserve enforces centrally.
- Structured JSON-lines operational logging.
- An explicit `ResponsePolicy`/`DatePolicy` for response metadata hygiene.

### The verdict

**Keep the handrolled server.** The reasoning:

1. **We are not a static file server.** eggserve's primary product is exactly
   what we do not need; the custom-service seam is the secondary path, added
   recently (Plans 243–286) and comparatively young.
2. **It would break our central architectural constraint.** The entire binary
   would move onto Tokio to gain a parser we already have working, tested, and
   live-verified — including SSE, which is precisely where `tiny_http` failed
   us and where eggserve's `ResponseBody::Stream` would help.
3. **Our server is small and proven.** `src/server/http.rs` is ~300 lines,
   covered by the integration suite in `src/server/tests.rs`, and verified
   against the real x.ai API for streaming, keep-alive, credential stripping
   and graceful shutdown.
4. **The real risks eggserve mitigates are not our risks.** We bind loopback
   only, accept credentials from a single local user, and forward to a fixed
   set of upstreams. The adversarial-parser surface that eggserve hardens
   against is a much smaller part of our threat model than it is for a
   public-facing static file server.

**The one thing worth stealing** is the *idea* of centrally enforced
resource limits — `max header count`, `max header bytes`, `max body bytes`,
`header timeout`, `max requests per connection`. If we ever want to harden
the parser, adding those bounds to `src/server/http.rs` is a much smaller
change than a runtime migration. That is a follow-up worth considering on its
own merits.

**Reconsider if** the requirement changes to: serve untrusted networks, need
HTTP/2 or HTTP/3, need WebSocket upgrade/tunnel semantics, or need to expose
grokproxy as a library to other Rust projects that already use eggserve.

---

## 4. Do Grok and Codex expose plan limits through these paths?

**Yes for both, but they are very different in quality, and only one of them
is documented.**

### 4.1 x.ai / Grok — verified first-hand

There are **two distinct limit systems**, and they do not mix.

**(a) API tier limits — what our current path actually uses.**

`api.x.ai` returns rate-limit headers on inference responses. Verified live
against `api.x.ai/v1/chat/completions` with our OAuth token:

```
x-ratelimit-limit-tokens: 53000000
x-ratelimit-remaining-tokens: 53000000
x-ratelimit-limit-requests: 8300
x-ratelimit-remaining-requests: 8300
```

`GET /v1/models` returns **no** rate-limit headers. `POST /v1/tokenize-text`
returns the **requests** window only (`limit-requests: 8300`,
`remaining-requests: 8300`) and, per third-party research, does so without
running inference — useful for reading the budget without spending it.

Our token's JWT payload (unverified decode) carries:

```
tier: 1
principal_type: User
principal_id: 8bffe1af-...
team_id: f521c953-...
scope: openid profile email offline_access grok-cli:access api:access …
iss: https://auth.x.ai
```

`x-ratelimit-limit-tokens: 53,000,000` is exactly the documented **Tier 1**
TPM for `grok-4.7` in x.ai's published rate-limit table (Tier 0: 50M,
Tier 1: 53M, Tier 2: 60M). And `tier: 1` is the token claim. So the OAuth
credential is graded on the **standard API team tier system**, not on a
consumer subscription allowance.

That matches x.ai's own pricing page: *"API usage is billed separately at
standard model rates and doesn't draw from your plan's usage pool."* The
response body confirms metered billing — our 1-token completion carried
`"cost_in_usd_ticks": 6990400`.

**(b) The SuperGrok consumer weekly pool — visible, but on the CLI proxy.**

`GET https://cli-chat-proxy.grok.com/v1/billing?format=credits` with
`Authorization: Bearer <token>`, `x-xai-token-auth: xai-grok-cli` and
`Accept: application/json` returns **200** (verified first-hand):

```json
{"config":{
  "currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY",
                   "start":"2026-11-13T03:07:08Z","end":"2026-11-20T03:07:08Z"},
  "creditUsagePercent":1.0,
  "productUsage":[{"product":"Cursor","usagePercent":1.0}],
  "isUnifiedBillingUser":true,
  "prepaidBalance":{"val":0},
  "topUpMethod":"TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
  "billingPeriodStart":"2026-11-13T03:07:08Z",
  "billingPeriodEnd":"2026-11-20T03:07:08Z"}}
```

This is the shared weekly pool that x.ai's FAQ describes ("one shared weekly
usage pool … resets every week"). It is reachable with the same OAuth token.

`POST https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig`
with `Content-Type: application/grpc-web+proto` also returns **200** (verified
first-hand) and reports the same weekly pool.

`GET https://grok.com/rest/rate-limits` with the OAuth token returns
**501 / gRPC code 12 (UNIMPLEMENTED)** — confirmed first-hand, and consistent
with third-party reports that this surface rejects OAuth tokens.

**The important observation:** after two `api.x.ai` chat completions, the
consumer pool still read `creditUsagePercent: 1.0` with `productUsage`
listing only `Cursor`. Our direct-API inference **is not itemised into the
consumer weekly pool** — it is metered against the API tier budget instead.
That is consistent with x.ai's documented separation between plan pool and
API billing, and it means our current path is metered, not free.

**Summary for Grok:** there is **no** subscription-quota endpoint on
`api.x.ai`. Third-party research reports that `/v1/usage`, `/v1/credits`,
`/v1/billing`, `/v1/limits` and `/v1/quota` all 404 there. What exists is:
the `x-ratelimit-*` headers (API tier budget), the `tier` claim on the token,
`GET /v1/me` (identity — verified first-hand, returns `user_id`, `team_id`,
`zdr_status`, `team_blocked`; note it does **not** contain a plan tier field),
and the CLI-proxy billing endpoint (consumer weekly pool).

### 4.2 ChatGPT / Codex — rich, structured, and mostly documented

**Endpoint.** `GET https://chatgpt.com/backend-api/wham/usage` with the
bearer token and `ChatGPT-Account-Id` header. (The Codex CLI's
`PathStyle::from_base_url` picks `/wham/…` when the base URL contains
`/backend-api`, and `/api/codex/…` otherwise — `backend_client.rs:173`.)
`/backend-api/codex/usage` also works and carries a slightly richer payload
(`email`, `rate_limit_reset_credits.applicable_available_count`,
`rate_limit_upsell`).

**Payload shape** (Codex CLI's `RateLimitStatusPayload`):

```jsonc
{
  "plan_type": "plus",              // free, go, plus, pro, prolite, promax,
                                    // team, business, education, enterprise, …
  "rate_limit": {
    "allowed": true,
    "limit_reached": false,
    "primary_window":   { "used_percent": 19, "limit_window_seconds": 18000,
                          "reset_after_seconds": 123, "reset_at": 1789290347 },
    "secondary_window": { "used_percent": 4,  "limit_window_seconds": 604800,
                          "reset_after_seconds": 456, "reset_at": 1789805400 }
  },
  "credits": { "has_credits": true, "unlimited": false, "balance": "12.50" },
  "spend_control": { "reached": false,
                     "individual_limit": { "used": "…", "remaining_percent": 80,
                                           "reset_at": 1789805400 } },
  "additional_rate_limits": [ { "limit_name": "codex_other", "metered_feature": "…",
                                 "rate_limit": { … } } ],
  "rate_limit_reset_credits": { "available_count": 2 },
  "account_id": "…", "user_id": "…", "rate_limit_upsell": { … }
}
```

**Also available:**

- `GET /wham/rate-limit-reset-credits` — banked reset credits.
- `POST /wham/rate-limit-reset-credits/consume` — redeem one.
- Analytics: `/wham/usage/daily-token-usage-breakdown`,
  `/wham/usage/credit-usage-events`,
  `/wham/analytics/daily-workspace-usage-counts`, and
  `POST /wham/usage/thread_usage/query` for per-thread credit costs.
- `GET /wham/profiles/me` — token usage profile.

**And the same data arrives as response headers** on every model request —
which matters a lot for a proxy, because headers need no extra request:

```
x-L-primary-used-percent, x-L-secondary-used-percent
x-L-primary-window-minutes, x-L-secondary-window-minutes
x-L-primary-reset-at, x-L-secondary-reset-at
x-L-limit-name
x-codex-credits-has-credits, x-codex-credits-unlimited, x-codex-credits-balance
x-codex-rate-limit-reached-type, x-codex-promo-message
```

One subtlety: `plan_type` is **not** in the HTTP headers. Codex only sets it
from WebSocket `codex.rate_limits` events; the HTTP header parser writes it
as null. So the endpoint is the only place to get the plan type.

### 4.3 What this means for grokproxy today

**The good news: we already forward upstream headers.** `forwarded_headers`
in `src/server/chat.rs:59` copies everything except hop-by-hop headers, so
`x-ratelimit-*` (Grok) and `x-L-*` / `x-codex-*` (Codex) would reach the
client automatically on non-streaming responses.

**Two gaps, both real:**

1. **The streaming path drops all upstream headers.**
   `src/server/sse.rs:12` constructs the response with exactly three headers
   (`Content-Type`, `Cache-Control`, `X-Accel-Buffering`). The upstream
   response headers — including every rate-limit header — are discarded. So
   a client streaming a completion sees no budget information at all, even
   though the non-streaming path would have shown it.

2. **The `/v1/models` success path drops upstream headers too.**
   `src/server/models.rs:34` returns `Response::json(200, …)` with no upstream
   headers. Only the non-200 branch forwards them. This one is harmless
   (verified: `/v1/models` carries no rate-limit headers anyway).

**A design question for any plan-limits feature:** should grokproxy expose
the limits as *response headers* (pass-through, zero effort, works today for
non-streaming), or as a *first-class endpoint* (e.g. `GET /v1/usage`)? A
`/v1/usage` endpoint would need a provider-specific implementation — Codex has
a clean JSON endpoint; Grok has no equivalent on `api.x.ai` and would have to
be assembled from the headers plus the CLI-proxy billing call. That asymmetry
is a reason to keep this a *provider capability* rather than a core feature.

---

## 5. Recommendations

1. **Model list: leave it uncached.** Verified that `GET /v1/models` is free of
   rate-limit consumption, so the per-request fetch costs us nothing and
   always reflects the live catalogue. The static `KNOWN_MODELS` union is the
   real wart — it never forgets. If we want to fix anything here, make
   `KNOWN_MODELS` a *grace-period* union rather than a permanent one, and fix
   the `GET /v1/models/{id}` behaviour so unknown ids return 404 instead of a
   synthetic 200.

2. **HTTP server: keep the handrolled one.** eggserve is well-built but is a
   static-file server with a young async custom-service seam, and adopting it
   means adopting Tokio across the whole binary — against an explicit project
   constraint, in exchange for a parser we already have. If we want eggserve's
   benefits without its runtime, the cheap subset is centrally enforced
   resource limits (max headers, max header bytes, max body bytes, header
   timeout) in `src/server/http.rs`.

3. **ChatGPT/Codex path: feasible and genuinely parallel, but a bigger job
   than "add a provider".** The auth plumbing is the easy part and our
   `TokenProvider` trait is the right seam. The real work is on the inference
   side — the Codex backend speaks the Responses API, its model catalogue uses
   `models`/`slug` rather than `data`/`id`, it needs `ChatGPT-Account-Id`, and
   it wants `store=false` plus `instructions`. And it must not be forgotten
   that this path is undocumented by OpenAI and explicitly at-your-own-risk.

   If we do build it, the **device-code flow** is the right entry point:
   it needs no local port (so no collision with a running Codex CLI), the
   redirect URI is remote, and the server hands us the PKCE pair.

4. **Plan limits: surface what already exists, cheaply.** Grok's API-tier
   budget is already in the `x-ratelimit-*` headers we forward; Codex's is in
   `x-L-*`. The highest-value change is small: **forward upstream headers on
   the streaming path too**, so streaming clients can see their budget. A
   first-class `/v1/usage` endpoint is a bigger, provider-asymmetric feature
   and should be designed per-provider rather than bolted onto the core.

5. **Worth recording in the README:** our current path is metered on the
   standard xAI API tier system (tier 1 for this account), *not* drawn from
   the SuperGrok consumer weekly pool. Anyone using grokproxy should know
   that the Grok subscription allowance and grokproxy's API usage are
   billed separately.
