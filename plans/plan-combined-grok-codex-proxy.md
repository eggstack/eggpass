# Plan: a combined Grok + Codex proxy, and the server-hardening that goes with it

Date: 2026-11-13. Planning only — no code changed.

This document answers the three questions raised in review, then lays out the
design for turning `grokproxy` into a proxy for both x.ai and ChatGPT/Codex.

- [Part 1 — The blocking question](#part-1--the-blocking-question-answered-with-measurements)
- [Part 2 — Dropping the static model half](#part-2--dropping-the-static-model-half)
- [Part 3 — The combined proxy design](#part-3--the-combined-proxy-design)
- [Part 4 — Phasing](#part-4--phasing)
- [Part 5 — Risks and open questions](#part-5--risks-and-open-questions)

---

## Part 1 — The blocking question, answered with measurements

### The question

> Being sync/blocking, my intuition is that a single long streaming IO from a
> model could block other requests and reduce the throughput pretty
> significantly.

### The short answer

**It does not, and the reason is structural rather than lucky.** The blocking
model we use — one OS thread per client connection — is precisely the model
that has no head-of-line blocking *between connections*. A streaming response
blocks exactly one thread: the one serving its own connection.

The intuition is a natural reflex from the *one-thread-per-connection-with-a-
pool-of-workers* design (PHP, WSGI, gunicorn without threads). Our design has
no worker pool, so there is nothing for a long request to starve.

### The structural argument

`src/server/http.rs:136` — `HttpServer::serve`:

```rust
for stream in self.listener.incoming() {
    ...
    std::thread::spawn(move || {
        let _ = handle_connection(stream, handler);
    });
}
```

One thread per accepted connection, on top of the accept loop, which is on the
main thread and never blocks.

`src/server/http.rs:172` — `handle_connection` runs the whole keep-alive
request loop for one client connection, on that connection's thread.

`src/server/http.rs:486` — `write_response` is where the streaming happens:

```rust
Body::Stream(mut body) => {
    ...
    loop {
        let count = body.read(&mut buffer)?;   // blocking read on the upstream socket
        ...
        writer.flush()?;                        // flush after every chunk
    }
}
```

That `body.read()` is a blocking read of the upstream socket. It blocks the
**connection's own thread** for the lifetime of the stream. No other client
connection shares that thread, so no other request is delayed.

### Why the shared upstream client is *not* the bottleneck either

This is the part that could have been a problem, and it is worth being precise
about, because `ServerContext` (`src/server/mod.rs:33`) is `Arc`-shared and
holds exactly **one** `ureq::Agent` — one connection pool for every request the
proxy makes.

I read the ureq 3.5.2 source to check whether that pool could starve:

```rust
// src/pool.rs:265
struct Pool {
    lru: VecDeque<Connection>,          // ← idle connections only
    max_idle_connections: usize,
    max_idle_connections_per_host: usize,
    max_idle_age: Duration,
    ...
}
```

and

```rust
// src/pool.rs:30
pub fn connect(&self, details, max_idle_age, use_pool) -> Result<Connection, Error> {
    let mut pool = self.pool.lock().unwrap();
    ...
    if use_pool {
        if let Some(conn) = pool.get(&key, max_idle_age, details.now) {
            return Ok(conn);
        }
    }
    drop(pool);                            // ← lock released before connecting
    let transport = self.run_connector(details)?;   // new TCP connection, no lock held
    ...
}
```

Three facts follow:

1. **The pool bounds *idle* connections, not active ones.** Defaults are
   `max_idle_connections: 10` and `max_idle_connections_per_host: 3`
   (`src/config.rs:1088`). A connection that is currently streaming is not in
   the LRU queue, so it neither occupies a slot nor starves anybody.
2. **There is no cap on concurrent in-flight upstream connections.** When the
   pool has no idle match, ureq opens a new TCP connection.
3. **The pool mutex is held for microseconds** — it is dropped before the
   connector runs, so even fifty threads racing to open connections do not
   serialise.

One related detail: **ureq 3.5.2 has no HTTP/2 support** (no `h2` feature in
its `Cargo.toml`), so every upstream request is its own HTTP/1.1 TCP
connection. `api.x.ai` negotiates h2 with curl, but we never ask for it. Not a
problem — it just means connection count equals concurrent request count.

### The refresh mutex is not the bottleneck either

`TokenManager` (`src/auth/refresh.rs:69`) holds a `Mutex<()>` around the
refresh network call only (`src/auth/refresh.rs:118`), and re-reads the
credential store inside the lock so a burst of parallel 401s produces exactly
one refresh. The lock is **not** held during a request or during a stream.

### The measurement

Because structural arguments can be wrong, I measured it against the live
x.ai API using the shipped binary — six concurrent SSE streams through one
`grokproxy` instance, with short requests interleaved.

```
=== BASELINE: 4 sequential short requests, no streams in flight ===
  short#1 1.227s
  short#2 1.217s
  short#3 3.941s
  short#4 6.004s          ← median 2.58s

=== 4 short requests WHILE 6 streams are in flight ===
  short#1 0.960s
  short#2 5.028s
  short#3 1.109s
  short#4 2.079s
=== 4 more, streams still running ===
  short#5 1.570s
  short#6 1.363s
  short#7 5.183s
  short#8 1.171s          ← median 1.47s

=== stream completion times ===
  27.76s  28.30s  28.80s  28.86s  29.28s  30.26s
=== chunk counts ===
  940  906  954  936  916  1011  data chunks
```

Two things to read out of this:

- **The streams ran fully in parallel.** If a long stream had blocked the
  others, the six would have serialised to roughly 28 s × 6 ≈ 170 s. They
  finished within 2.5 s of each other, at ~30 s wall clock — **full 6×
  concurrency**, with ~940 chunks each flushed individually.
- **Median latency while streaming was *lower* than the idle baseline**
  (1.47 s vs 2.58 s). The 5-second outliers appear in both sets, so they are
  x.ai's own response-time variance, not proxy contention.

**Conclusion: a long streaming response does not reduce throughput for other
requests. The concern is unfounded for this design.**

### But the exercise did surface four real limits

The question was worth asking even though the answer is reassuring, because
looking at the concurrency model from that angle exposed genuine gaps.

#### L1. The in-flight counter does not cover the streaming phase — this is a bug

`src/server/mod.rs:102`:

```rust
Arc::new(move |request: Request| {
    server.in_flight.fetch_add(1, Ordering::SeqCst);
    let response = handle_request(request, &context);
    server.in_flight.fetch_sub(1, Ordering::SeqCst);   // ← decremented here
    response
})
```

The counter is decremented when the *handler* returns — but the handler only
returns a `Response` whose body is a `Box<dyn Read>` handle. The body is
actually read and written later, inside `write_response` on the connection
thread. So for a streaming request, `in_flight` is already back to zero while
the stream is still running.

Worse: the connection threads are **detached** (`std::thread::spawn` with the
`JoinHandle` discarded in `src/server/http.rs:164`), so they are never joined.
When the main thread finishes the five-second grace loop it simply exits, and
the process takes the connection threads down with it.

**Net effect: `SIGTERM` during a streaming response truncates that stream
immediately**, despite the `SHUTDOWN_GRACE` of five seconds
(`src/server/mod.rs:29`). The grace window exists to let in-flight work
finish, but streaming work is invisible to it.

#### L2. `timeout_recv_body` is a *total* body timeout, and ours is 5 minutes

ureq's own documentation for `timeout_per_read` says:

> The timeout restarts for each read. Phase, per-call and global timeouts still
> apply, **including the total body timeout set by `timeout_recv_body`**.
> — `ureq 3.5.2`, `src/config.rs:780`

We set `timeout_recv_body(Some(Duration::from_secs(300)))` in
`src/upstream.rs:155`. So **any response whose body takes more than five
minutes in total is killed mid-flight**, with a timeout error rather than a
useful message. For a fast chat model that is generous; for
`grok-4.20-reasoning`, or `gpt-5.5-codex` at `xhigh` effort, it is a real
ceiling. `timeout_global` is 1800 s (30 min), so the tighter of the two is the
five-minute body cap.

This matters more once Codex is added, because high-effort reasoning turns are
exactly the long-tail case.

#### L3. Thread-per-connection is unbounded, and idle keep-alive connections never time out

Every client TCP connection becomes one OS thread (Rust's default stack is
2 MiB of virtual address space). Nothing caps the count. And `read_head`
(`src/server/http.rs:327`) blocks indefinitely waiting for the next request on
a keep-alive connection — so a client that opens a connection and walks away
holds its thread forever.

For a single-user loopback proxy this is a non-issue. It becomes a problem if
the proxy is ever exposed beyond the machine, or if a client library opens a
connection per request without closing it.

#### L4. Nothing caps concurrent upstream requests

No limit on how many simultaneous streams may be opened. Each opens its own
TCP connection to x.ai. A runaway client could open hundreds of concurrent
streams, which is unfriendly to x.ai and burns the tier-1 request budget
(8,300 per window) quickly.

### What to do about it

None of these argue for changing the concurrency model. They argue for four
small, targeted changes that are worth doing regardless of whether Codex is
added:

| # | Change | Why |
| --- | --- | --- |
| H1 | Track in-flight **connections**, not handler calls; join the connection threads during the grace window | Fixes the truncation bug (L1) |
| H2 | Make the upstream timeouts configurable; raise the body timeout well above 5 minutes | Fixes the long-stream ceiling (L2) |
| H3 | Add a client-connection idle timeout and a max-connection count | Bounds the thread cost (L3) |
| H4 | Add a max-concurrent-upstream-requests cap, answering `503` beyond it | Bounds upstream pressure (L4) |

None of these requires Tokio. All are a few dozen lines in the files we
already own.

---

## Part 2 — Dropping the static model half

### The proposal

Drop `KNOWN_MODELS` and serve the upstream catalogue verbatim. Two variants:

- **(b) Live catalogue only.** `/v1/models` returns exactly what the upstream
  returns. `KNOWN_MODELS` and `merge_models` are deleted.
- **(c) Live catalogue only, and honest 404s.** As (b), and
  `GET /v1/models/{unknown}` returns the upstream's `404` instead of our
  synthetic `200`.

### The case for dropping it

- **The live catalogue is good.** Verified: thirteen models from `api.x.ai`,
  including `grok-4.7` and `grok-4.6`.
- **It is free.** `GET /v1/models` returns no `x-ratelimit-*` headers and does
  not visibly consume the request budget, so a per-request live fetch costs
  nothing.
- **The static list is the thing that actually needs maintenance.** It is a
  hand-written list of model names that has to be updated whenever x.ai ships
  something, and — because the merge is purely additive — it **never forgets**.
  A model x.ai retired keeps being advertised forever. That is the exact
  failure mode people complain about in a proxy's model list.
- **The synthetic `200` is a lie.** `GET /v1/models/totally-made-up` currently
  returns `200`. Once the static list is gone, that behaviour has no purpose
  and should go too.

### The cost — and it is real

**The `build` upstream's catalogue lists exactly one model** where `api.x.ai`
lists ten (recorded in `plans/research.md:371`). With the static list gone:

- On the default `api` upstream: no meaningful loss.
- On the `build` fallback upstream: `/v1/models` would advertise **one** model
  (the build-tuned variant). The fallback becomes nearly unusable as an
  OpenAI-compatible surface for any client that discovers models from the
  catalogue.

That is the whole argument for keeping the static half: it is what makes the
`build` fallback advertise a usable set.

### Recommendation

**Drop it (option c), with one caveat.**

The `build` upstream is a *fallback*, chosen by editing the config. A user who
selects it can equally well be told "this upstream publishes a narrow
catalogue" — and the alternative is a hand-maintained list of model names that
is wrong the moment x.ai renames anything, which is exactly the maintenance
burden we are trying to remove.

But the decision hinges on one question worth answering first:

> **Is the `build` fallback actually used?**

If the answer is no — if `api.x.ai` has been stable and good, which the
research and the live verification both suggest — then `build` is dead weight
and dropping the static list costs nothing at all. If it *is* used, then the
`build` fallback needs its own catalogue story, and the honest one is to
document that `build` advertises a narrow catalogue rather than to paper over
it with a stale list.

**Alternative worth considering:** keep the static list but make it
*configurable* rather than compiled in — a `known_models = [...]` array in
`config.toml`, empty by default. Then the maintenance burden is a
user-editable file rather than a source edit and a rebuild, and a user on the
`build` upstream can list what they want. This gets most of the benefit of
both options at the cost of one more config key.

---

## Part 3 — The combined proxy design

### 3.1 The provider model

Today the binary hardwires one issuer (x.ai), one credential file, and one
upstream base URL. The change is to generalise along the axis that is already
half-present.

The existing `TokenProvider` trait (`src/upstream.rs:76`) is exactly the right
seam — it was designed for this:

```rust
pub trait TokenProvider {
    fn token(&self) -> Result<String, TokenError>;
    fn refresh(&self, failed_token: &str) -> Result<String, TokenError>;
}
```

Introduce a `Provider` that bundles everything that varies:

```rust
pub struct Provider {
    pub id: ProviderId,              // Xai | OpenAi
    pub base: String,                // origin, no /v1
    pub path_style: PathStyle,       // /v1/... vs /backend-api/codex/...
    pub tokens: TokenManager,        // per-provider credential + refresh
    pub headers: HeaderPolicy,       // extra headers we add
}
```

`HeaderPolicy` is what generalises `UpstreamClient::extra_headers()`
(`src/upstream.rs:215`):

| Provider | Extra headers | Why |
| --- | --- | --- |
| x.ai (`api`) | none | — |
| x.ai (`build`) | `X-XAI-Token-Auth: xai-grok-cli`, `x-grok-client-version: 1.0.77`, `x-grok-client-identifier` | The proxy returns 426 below client version 1.0.13 |
| ChatGPT/Codex | `ChatGPT-Account-Id: <id>` | Required by the backend; taken from the `id_token` claim |

**A related fix belongs here.** `upstream::STRIPPED_HEADERS`
(`src/upstream.rs:263`) strips the *client's* credentials but not
*provider-owned* headers. A client can currently send its own
`x-xai-token-auth`, `x-grok-client-version` or `chatgpt-account-id` and have
them reach the upstream — which is both a correctness problem (we would add a
duplicate of a header we also set) and a small injection surface. The
provider-owned header names must be added to the stripped list.

### 3.2 Credential storage

Today: one `~/.config/grokproxy/auth.json`, mode 0600, in a 0700 directory,
with an import from `~/.grok/auth.json`.

Proposed: one file per provider under a directory.

```
~/.config/grokproxy/credentials/xai.json
~/.config/grokproxy/credentials/openai.json
```

Each 0600. The existing `auth.json` is imported into `credentials/xai.json` on
first sight, so nothing is lost and the import path from `~/.grok/auth.json`
keeps working.

This is better than one file with two sections because the two credential
shapes genuinely differ:

| | x.ai | ChatGPT/Codex |
| --- | --- | --- |
| Access token | opaque | opaque |
| Refresh token | yes, rotates | yes, rotates |
| `id_token` | not used | **yes** — carries `chatgpt_account_id`, `plan_type`, `email` |
| Extra fields | `email`, `user_id`, `team_id`, `tier` | `account_id`, `plan_type` |

A single JSON file with two heterogeneous sections would need optional-field
plumbing in every accessor. Two files keep each provider's schema honest.

**The refresh mutex must become per-provider.** A global mutex would mean a
Codex refresh blocked an x.ai refresh — semantically wrong once there are two
independent issuers, even if the blocking is brief.

### 3.3 Routing: how a request reaches the right provider

Model namespaces do not collide (`grok-*` vs `gpt-*`/`codex*`/`o*`), so no
content negotiation is needed. Two viable strategies:

**Option A — a prefix map in config.**

```toml
[routing]
default_provider = "xai"

[routing.prefixes]
"grok-"  = "xai"
"gpt-"   = "openai"
"codex"  = "openai"
"o1"     = "openai"
"o3"     = "openai"
"o4"     = "openai"
```

Deterministic, zero network, zero latency, and editable with `grokproxy edit`
rather than a rebuild. The cost: when OpenAI ships a new model family, the map
needs a new entry. Mitigated by a generous default set and the fact that
unmatched models fall through to `default_provider`.

**Option B — route by catalogue membership.**

Fetch each provider's catalogue at start-up, build `HashMap<model, Provider>`,
refresh on a TTL or on an unknown-model response. Self-updating, but it adds a
network dependency at start-up, a background refresher, and a class of
stale-mapping bugs. The server would also need to start successfully when the
network is down.

**Recommendation: Option A**, with Option B's *catalogue* still being used for
**advertisement** (`/v1/models` merges every logged-in provider's catalogue) —
which is where it is actually valuable and where it costs nothing extra.

A useful safety valve: if the routed provider returns `404 unknown model`, the
proxy can retry once against the default provider before giving up. That
catches the "new model family, stale prefix map" case without any background
machinery.

### 3.4 Codex authentication

Two flows, both implemented in the Codex CLI's own source.

**Device code — the primary flow.** No local port, no collision with a running
Codex CLI, and the server hands us the PKCE pair.

| Step | Request | Response |
| --- | --- | --- |
| 1 | `POST https://auth.openai.com/api/accounts/deviceauth/usercode`, body `{"client_id": "app_EMoamEEZ73f0CkXaXp7hrann"}` | `{device_auth_id, user_code, interval}` |
| 2 | the user opens `https://auth.openai.com/codex/device` and types `user_code` | — |
| 3 | poll `POST https://auth.openai.com/api/accounts/deviceauth/token`, body `{"device_auth_id", "user_code"}` | `{authorization_code, code_challenge, code_verifier}` |
| 4 | exchange the code at `POST https://auth.openai.com/oauth/token` with `redirect_uri = https://auth.openai.com/deviceauth/callback` | tokens |

Polling semantics from `codex-rs/login/src/device_code_auth.rs`: `403` and
`404` mean keep polling; anything else is a hard failure; 15-minute overall
timeout.

**Browser loopback — the secondary flow.** Registered redirect URIs are
`http://127.0.0.1:1455/auth/callback` with `1457` as the documented fallback
(`codex-rs/login/src/server.rs:79-81` — *"Keep in sync with the Codex CLI
Hydra redirect URI allow-list"*). The port is **not** free, so this flow
collides with a running Codex CLI. Worth supporting, but the device flow
should be the default for the same reason `--device` is our x.ai fallback:
it is the one that always works.

**Scope:** `openid profile email offline_access api.connectors.read
api.connectors.invoke` (`server.rs:613`).

**Token handling.** The response carries `access_token`, `refresh_token` and
`id_token`. From the `id_token` JWT payload we extract:

- `chatgpt_account_id` → sent as the `ChatGPT-Account-Id` header
- `chatgpt_plan_type` → shown by `grokproxy status`
- `email` → shown by `grokproxy status`

Refresh: standard grant, with rotation, using the same 5-minute skew and the
same mutex-and-re-read discipline as the x.ai path. The `TokenProvider` trait
is already the right interface.

**Also worth capturing:** the Codex CLI exchanges the `id_token` for an
API-key-style access token via an OAuth token-exchange grant
(`server.rs:1036-1058`, `requested_token=openai-api-key`). That would turn a
ChatGPT subscription into a normal `api.openai.com` API key — a different and
broader product direction. Out of scope for this phase; worth noting as an
option.

### 3.5 The Codex inference surface

This is where the work actually is.

| | x.ai | ChatGPT/Codex |
| --- | --- | --- |
| Base | `https://api.x.ai/v1` | `https://chatgpt.com/backend-api/codex` |
| Path style | `/v1/chat/completions`, `/v1/responses` | `/responses` only |
| Wire API | Chat Completions *and* Responses | Responses only |
| Catalogue | `GET /v1/models` → `{data:[{id}]}` | `GET /models?client_version=…` → `{models:[{slug}]}` |
| Required request fields | — | `store: false`, `instructions` present, content type `input_text` |
| Required headers | — | `ChatGPT-Account-Id` |
| Conversation state | server-side ok | **stateless** — full history resent every turn |

**Path mapping.** Our `upstream::normalize_path` (`src/upstream.rs:316`)
prefixes `/v1` to everything. That is wrong for the Codex backend, whose paths
are relative to `/backend-api/codex`. This is exactly why `PathStyle` belongs
on the `Provider`: one provider prefixes `/v1`, the other does not.

**Catalogue translation.** The Codex catalogue returns `models`/`slug`; our
`/v1/models` must return `data`/`id` to stay OpenAI-compatible. So the
provider needs a catalogue adapter, not just a passthrough.

**`/v1/chat/completions` for a Codex model.** This is the expensive decision.
Options:

- **(b) Do not translate.** Codex models are served on `/v1/responses` only.
  A `/v1/chat/completions` request naming a Codex model gets a clear error
  naming the endpoint to use instead.
- **(c) Translate both ways.** Convert chat completions to the Responses
  request shape and the Responses response back to chat completions, for both
  buffered and streamed output.

The cost of (c), honestly:

- *Request*: `messages[]` → `input[]` items with `content: [{type:
  "input_text", text}]`; the system message → `instructions`; `max_tokens` →
  `max_output_tokens`; tools change from `{type:"function", function:{…}}` to
  `{type:"function", name, parameters}`; `store: false` injected.
- *Buffered response*: `output[]` → find the `message` item → concatenate its
  `content[].text` → build `choices[0].message.content`; map `usage`.
- *Streaming response*: the two event vocabularies do not overlap. Chat
  Completions emits `chat.completion.chunk` with `choices[].delta.content`;
  Responses emits `response.output_text.delta`,
  `response.output_item.added`, `response.function_call_arguments.delta`,
  `response.completed`, and more. A full event-mapping state machine is
  needed, including the final `[DONE]` sentinel and `usage`.

That is several hundred lines plus tests, and it is the part most likely to
carry subtle bugs.

**Recommendation: ship (b) first.** Our proxy already exposes `/v1/responses`
and it already works against x.ai, so the Responses surface exists and Codex
slots into it with no new translation layer at all. Many current OpenAI-compatible
clients support the Responses API natively. Add (c) later only if there is
demand, and if so, as its own phase with its own test matrix.

### 3.6 Merged `/v1/models`

`/v1/models` should return the union of every logged-in provider's catalogue,
each normalised to the OpenAI shape:

```
GET /v1/models
→ GET {xai}/v1/models          → data[].id
→ GET {codex}/models?client_version=… → models[].slug  → data[].id
→ merged, deduped, one array
```

Each entry should probably carry a `owned_by` that identifies the provider
(`xai` / `openai`), so a client can tell where a model comes from.

`GET /v1/models/{id}` should resolve against the merged catalogue and return
the provider's own entry — or, per Part 2, simply `404` for an unknown id.

### 3.7 Plan and usage limits

The research in `plans/research-model-refresh-codex-eggserve-limits.md`
established what each provider exposes:

| | x.ai | ChatGPT/Codex |
| --- | --- | --- |
| Live budget | `x-ratelimit-*` headers on inference responses | `x-L-*` / `x-codex-*` headers on inference responses |
| Plan type | `tier` claim on the token (`tier: 1`) | `plan_type` in `GET /wham/usage` — **not** in the headers |
| Window details | headers only | `GET /wham/usage` → primary/secondary windows, credits, spend control |

**The cheap, high-value change:** forward upstream headers on the streaming
path. Today `src/server/sse.rs:12` builds the streaming response with exactly
three headers and discards everything the upstream sent — so a streaming
client sees no rate-limit information at all, while a non-streaming client
does. That is an inconsistency worth fixing regardless of Codex.

**A provider-specific `/v1/usage` endpoint** is more work and asymmetric
(Codex has a clean JSON endpoint; x.ai has only headers plus the CLI-proxy
billing call). If we add it, it belongs at the provider layer, not the core:
`GET /v1/usage` → dispatch to the provider that can answer.

### 3.8 CLI surface

The nine commands stay. The changes are:

| Command | Change |
| --- | --- |
| `login` | `--provider xai\|openai` (default `xai`). `--device` for both. |
| `logout` | `--provider xai\|openai` (default `xai`). |
| `status` | Show both providers' account state; show the routing table. |
| `run` / `stop` / `restart` / `croncheck` / `port` / `edit` | Unchanged. |

`status` is where the value is clearest — one screen showing which providers
are logged in, their plan/tier, their token lifetime, and which models route
where.

### 3.9 Configuration

```toml
port = 8484
api_key = ""

[providers.xai]
upstream = "api"                 # "api" | "build"

[providers.openai]
enabled = true

[routing]
default_provider = "xai"

[routing.prefixes]
"grok-" = "xai"
"gpt-"  = "openai"
"codex" = "openai"

[oauth.xai]
client_id = "..."
issuer = "https://auth.x.ai"
scopes = [...]

[oauth.openai]
client_id = "app_EMoamEEZ73f0CkXaXp7hrann"
issuer = "https://auth.openai.com"
scopes = [...]
```

The `[oauth.*]` tables already exist for x.ai (`[oauth]` with `client_id`,
`issuer`, `scopes`, `referrer`); this generalises them per provider, which
also keeps the OpenAI constants overridable — the same mitigation that was
applied for x.ai's risk register.

Note the existing config surgery: `grokproxy port` performs a surgical text
edit so comments and unknown keys survive. Adding tables must not break that.

---

## Part 4 — Phasing

Each phase is independently shippable and independently testable.

### Phase 0 — Harden the current server (no behaviour change for users)

- **H1** Track in-flight connections; join connection threads during the grace
  window. Fixes the SIGTERM truncation.
- **H2** Make the upstream timeouts configurable; raise the default body
  timeout (5 minutes → 30 minutes) and the global timeout accordingly.
- **H3** Client-connection idle timeout and a max-connection count.
- **H4** Max-concurrent-upstream-requests cap, answering `503` beyond it.
- **H5** Strip provider-owned headers (`x-xai-token-auth`,
  `x-grok-client-version`, `chatgpt-account-id`, `originator`) from client
  requests.

*Acceptance:* the concurrency probe in Part 1 keeps its result; a stream
survives `SIGTERM` up to the grace window; a client cannot inject
provider-owned headers.

### Phase 1 — Decide the model catalogue (independent)

Either drop `KNOWN_MODELS` and the synthetic `200` (option c), or move the
list into `config.toml` as `known_models` (the configurable middle ground).

*Acceptance:* `/v1/models` reflects the upstream; unknown ids `404`; no
compiled-in model names remain in the source.

### Phase 2 — Generalise the provider layer (no behaviour change yet)

- `Provider`, `PathStyle`, `HeaderPolicy`.
- Per-provider credential files under `credentials/`, with the `auth.json`
  migration.
- Per-provider `TokenManager` and per-provider refresh mutex.
- `TokenProvider` implementations for both providers.
- Config: `[providers.*]`, `[oauth.*]`, `[routing.*]`.

*Acceptance:* both providers can be logged in; the existing x.ai behaviour is
byte-for-byte unchanged; the test suite still passes with the new
indirection.

### Phase 3 — Codex authentication

- Device-code flow (primary) and browser loopback at 1455/1457 (secondary).
- `id_token` decoding for `chatgpt_account_id`, `plan_type`, `email`.
- Refresh with rotation.
- `grokproxy login --provider openai`, `logout`, `status`.

*Acceptance:* a real device-code login completes and stores credentials;
the token refreshes across rotation; `status` shows the plan.

### Phase 4 — Codex inference on the Responses API

- Route `/v1/responses` to the Codex backend with `ChatGPT-Account-Id` and
  `store: false`.
- Catalogue adapter for `models`/`slug` → `data`/`id`.
- Merged `/v1/models`.
- Routing by prefix map, with the unknown-model retry to the default
  provider.

*Acceptance:* a real `gpt-5.*-codex` request succeeds, buffered and streamed;
the merged catalogue lists both providers' models; a Grok request still routes
to x.ai.

### Phase 5 — Chat Completions translation for Codex (optional)

Only if there is demand. Chat → Responses request, Responses → Chat
response, including the streaming event mapping.

### Phase 6 — Usage and limits

- Forward upstream headers on the streaming path.
- Optionally, `GET /v1/usage` dispatching per provider.

---

## Part 5 — Risks and open questions

### Risks

| # | Risk | Severity | Mitigation |
| --- | --- | --- | --- |
| R1 | **OpenAI does not support third-party use of the Codex OAuth client.** The client id `app_EMoamEEZ73f0CkXaXp7hrann` is OpenAI's own; the behaviour is reverse-engineered and explicitly "at your own risk". | **High** | Make Codex opt-in (`enabled = true` in config, off by default). Document it prominently. The x.ai path stays fully supported and unaffected. |
| R2 | The Codex backend's contract can change without notice — it is an internal API (WHAM). | High | Keep the Codex provider's constants in one place; make the endpoints config-overridable so they can be fixed without a release. |
| R3 | The ChatGPT subscription's usage allowance is a shared pool with the user's own ChatGPT usage; proxy traffic consumes it. | Medium | Document it. Surface the `/wham/usage` window in `status` so the user can see it. |
| R4 | Our x.ai path is metered on the **standard API tier** (tier 1), not the SuperGrok consumer pool — it is billed like API usage. | Medium | Document it clearly in the README. It is not a defect, but it is surprising. |
| R5 | The device-code flow may be disabled server-side for a given client id ("device code login is not enabled for this Codex server"). | Medium | Ship the browser loopback flow as well; the Codex CLI's own error message for this case is explicit. |
| R6 | Two providers double the credential-handling and refresh surface. | Medium | The `TokenProvider` seam already exists; the per-provider `TokenManager` is a small delta. |
| R7 | The Codex backend needs `store: false` and `instructions`; clients that omit them get errors that look like upstream bugs. | Low | Inject `store: false` and a default `instructions` in the provider layer, and normalise content types. |

### Open questions to resolve during implementation

1. **Does the Codex backend serve a live `/models`?** Third-party sources say
   `GET /backend-api/wham/models?client_version=…` works, but the Codex CLI
   also ships a compiled-in model catalogue. If the live endpoint does not
   work, the Codex catalogue has to be a static list — which is precisely the
   maintenance burden Part 2 is trying to remove. **This is the single
   highest-value unknown and should be verified first.**
2. **What headers does the Codex backend require on a model request?** The
   `ChatGPT-Account-Id` header is confirmed from the Codex source. The CLI
   additionally sends `originator` and `version`; whether the backend requires
   them is unverified.
3. **Is the `build` fallback upstream actually used?** Determines whether
   dropping `KNOWN_MODELS` costs anything.
4. **Do chat-completions clients matter enough to justify Phase 5?**
5. **Should Codex be on by default?** R1 argues for opt-in; usability argues
   for on. My recommendation is off by default, with `grokproxy login
   --provider openai` as the explicit opt-in — the same shape as the x.ai
   device flow.
