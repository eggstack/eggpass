# Plan: `eggpass` — a multi-provider AI proxy

Date: 2026-11-13.

> **Status: implemented.** This document was written as a plan; the plan has
> been built. Part 10 records what shipped, where it diverged from what is
> written here, and the two things that still need a human. The outcome is
> also recorded in [`shipped-2026-11-13.md`](shipped-2026-11-13.md), and the
> command-by-command contract is in [`cli-spec.md`](cli-spec.md).

`eggpass` is a local, OpenAI-compatible proxy that authenticates against
several AI providers with their own subscription OAuth credentials and serves
them behind one base URL. This document is the umbrella plan: the original
design questions, the multi-provider design, and the later clarifications —
the rename, the interactive provider picker, and the packaging/update
ecosystem.

| Part | Covers |
| --- | --- |
| [1](#part-1--the-blocking-question-answered-with-measurements) | The blocking question, answered with measurements |
| [2](#part-2--dropping-the-static-model-half) | Dropping the static model half |
| [3](#part-3--the-combined-proxy-design) | The multi-provider design — providers, credentials, routing, SIWC auth, inference, the CLI surface and the configuration |
| [4](#part-4--phasing) | Phasing |
| [5](#part-5--risks-and-open-questions) | Risks and open questions |
| [6](#part-6--the-rename) | **The rename** |
| [7](#part-7--the-interactive-provider-picker) | **The interactive provider picker** |
| [8](#part-8--packaging-and-updates-eggpack--eggup) | **Packaging and updates: eggpack / eggup** |
| [9](#part-9--relationship-to-eggpool) | **Relationship to eggpool** |
| [10](#part-10--what-shipped) | **What shipped** |

### Decisions settled

Everything below is decided. It is recorded here so the rest of the document
can be read as settled rather than tentative.

| Question | Decision | Where |
| --- | --- | --- |
| eggpool overlap | **Keep eggpass separate.** It is built and maintained on its own; eggpool ingests its OAuth layer downstream. | [9.3](#93-the-decision--keep-them-separate) |
| MSRV | **Raise 1.85 → 1.89** and adopt `eggup`. No bespoke updater. | [8.5](#85-the-msrv-cost) |
| Picker dependency | **`nix`'s termios**, following the family's own terminal selector; the numbered-list fallback always available. *(Originally `crossterm` — reversed after the crossterm picker was found to render the menu as a staircase.)* | [7.3](#73-how-to-read-keys-without-an-async-runtime) |
| Config-directory migration | **Confirmed** — the directory moves; the old one is left in place with a marker, not deleted. | [6.3](#63-why-the-config-directory-moves) |
| `port` / `host` | **`port` becomes apply-and-restart**, and **`host` is added**, with the same rules for both. | [3.8.1](#381-the-address-commands) |

---

> ## Update — OpenAI now supports this properly
>
> The first draft of this plan assumed we would reuse the Codex CLI's OAuth
> client id (`app_EMoamEEZ73f0CkXaXp7hrann`). **OpenAI does not intend
> third-party applications to do that.** OpenAI now publishes a dedicated
> registration mechanism for open-source applications called
> **Sign in with ChatGPT for Open Source**
> (<https://developers.openai.com/siwc/token-sharing-open-source>).
>
> eggpass is open-source and runs locally, so it is squarely in that path's
> audience — and dynamic registration needs **no client secret and no partner
> API key**, which the Codex-CLI route would have needed.
>
> The update improves the design in five ways:
>
> 1. **R1 is closed.** The path is documented and supported, not
>    reverse-engineered.
> 2. **The inference endpoint is the public API** — `https://api.openai.com/v1`
>    — not the internal `chatgpt.com/backend-api/codex` (WHAM) backend. The
>    two providers now share the same `/v1` path style.
> 3. **A dynamic loopback port is allowed**, so our existing ephemeral-port
>    design works unchanged. (The Codex CLI pins itself to 1455/1457.)
> 4. **No `ChatGPT-Account-Id` header** is needed; the token identifies the
>    account.
> 5. **The model catalogue is live** from the public API, so no compiled-in
>    model list is needed on that side either.
>
> It also adds two hard constraints: `stream: true` is **mandatory** on every
> request, and a specific list of request fields (`temperature`, `top_p`,
> `max_output_tokens`, …) is **rejected** and must be stripped.
>
> Sections 3.4, 3.5, 3.6, 3.7, 3.9, Part 4 and Part 5 have been rewritten
> accordingly; each carries its own inline note.

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
`eggpass` instance, with short requests interleaved.

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
    pub path_style: PathStyle,       // /v1/... (both providers use it)
    pub tokens: TokenManager,        // per-provider credential + refresh
    pub headers: HeaderPolicy,       // extra headers we add
    pub request: RequestPolicy,      // per-provider request sanitising
}
```

> **Update (2026-11-13).** `PathStyle` was introduced for the Codex CLI's
> WHAM backend, whose paths are relative to `/backend-api/codex`. Under SIWC
> the ChatGPT provider uses `https://api.openai.com/v1`, so **both providers
> share the `/v1` prefix** and `PathStyle` collapses to one variant. It is
> kept only because `RequestPolicy` and `HeaderPolicy` now need the same
> per-provider hook.

`HeaderPolicy` is what generalises `UpstreamClient::extra_headers()`
(`src/upstream.rs:215`):

| Provider | Extra headers | Why |
| --- | --- | --- |
| x.ai (`api`) | none | — |
| x.ai (`build`) | `X-XAI-Token-Auth: xai-grok-cli`, `x-grok-client-version: 1.0.77`, `x-grok-client-identifier` | The proxy returns 426 below client version 1.0.13 |
| ChatGPT (SIWC) | none | The public API identifies the account from the token; no `ChatGPT-Account-Id` needed |

`RequestPolicy` is new, and only the ChatGPT provider uses it: strip the
fields the route rejects, inject `store: false` and `stream: true`, and ensure
`instructions` are present (see §3.5).

**A related fix belongs here.** `upstream::STRIPPED_HEADERS`
(`src/upstream.rs:263`) strips the *client's* credentials but not
*provider-owned* headers. A client can currently send its own
`x-xai-token-auth` or `x-grok-client-version` and have them reach the upstream
— which is both a correctness problem (we would add a duplicate of a header we
also set) and a small injection surface. The provider-owned header names must
be added to the stripped list.

### 3.2 Credential storage

Today: one `~/.config/eggpass/auth.json`, mode 0600, in a 0700 directory,
with an import from `~/.grok/auth.json`.

Proposed: one file per provider under a directory.

```
~/.config/eggpass/credentials/xai.json
~/.config/eggpass/credentials/openai.json
```

Each 0600. The existing `auth.json` is imported into `credentials/xai.json` on
first sight, so nothing is lost and the import path from `~/.grok/auth.json`
keeps working.

This is better than one file with two sections because the two credential
shapes genuinely differ:

| | x.ai | ChatGPT (SIWC) |
| --- | --- | --- |
| Access token | opaque, ~6 h | opaque, **1 h** |
| Refresh token | yes, rotates | yes, rotates, **30-day** lifetime |
| `id_token` | not used | **yes** — carries `sub`, `email`, `aud` (= issued client id) |
| Issued client id | fixed, in config | **per user and workspace**, issued at registration |
| Host id | none | `ext_agent_host_id`, stable per host |
| Extra fields | `email`, `user_id`, `team_id`, `tier` | `subject`, `scopes`, `plan_enabled` |

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

Deterministic, zero network, zero latency, and editable with `eggpass edit`
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

### 3.4 Codex authentication — Sign in with ChatGPT

> **Update (2026-11-13).** This section was originally written around reusing
> the Codex CLI's OAuth client. **OpenAI does not intend third-party
> applications to reuse the Codex CLI's `client_id`.** OpenAI now publishes a
> supported path for exactly our case — *Sign in with ChatGPT for Open
> Source* — and this section is rewritten around it. Sources:
> [Overview](https://developers.openai.com/siwc/token-sharing-open-source),
> [Registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
> [Token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference).

eggpass is open-source and runs locally, which puts it squarely in the
audience SIWC was written for: *"ChatGPT plan usage is available to open-source
projects, personal projects that run locally, and selected private apps."*

**Why dynamic registration.** SIWC offers two ways to get a client id:

| Route | Who | What you need |
| --- | --- | --- |
| **Dynamic registration** | open-source and locally hosted apps | nothing but the flow itself — **no client secret, no partner API key** |
| Static client id | selected commercial partners | apply through the interest form; it is a waitlist |

The static route is a commercial waitlist, so **dynamic registration is the
only route available to an open-source tool** — and the better fit besides: no
secret to store, no secret to leak.

**The flow.**

| Step | What happens |
| --- | --- |
| 1 | Persist this host's `ext_agent_host_id` (see below). |
| 2 | Open `https://auth.openai.com/api/accounts/authorize` with `client_id=dynamic_agent_client`, `agent_name_hint`, `ext_agent_host_id`, `scope`, `resource=https://api.openai.com/v1`, and fresh `state`, `nonce` and PKCE (`S256`). |
| 3 | The user signs in, names and authorises the agent, and is redirected to our loopback callback with `code`, `state`, `scope` and — on a **new registration** — the **issued `client_id`**. |
| 4 | Validate `state`, retain the issued `client_id`. |
| 5 | Exchange at `POST https://auth.openai.com/api/accounts/oauth/token`: `grant_type=authorization_code`, the **issued** `client_id`, `code`, `code_verifier`, the same `redirect_uri`, the same `resource`. |

Two details that are easy to get wrong:

- **`dynamic_agent_client` is an entrypoint, not an id to save.** The callback
  returns a real issued client id (`oaiapp_…`). That is what we persist, and
  what every later token request must use. *"If a new-registration callback
  does not include an issued ID, treat registration as incomplete."*
- **Reauthorisation uses the issued id, not `dynamic_agent_client`**, and omits
  `agent_name_hint`. We may send a retained `id_token` as `id_token_hint` and
  a saved email as `login_hint`; with `id_token_hint` the flow skips the
  account selector.

**The host id.** `ext_agent_host_id` is required, stable per host, and opaque
— not an email, not a user id. Two accepted formats:

- `urn:ietf:params:oauth:jwk-thumbprint:…` — derived from a public key per
  RFC 9278. **Recommended.** (OpenAI does not verify possession of the private
  key; it is an identifier only.)
- `urn:uuid:…` — a UUIDv4, generated once per host and persisted.

It is not a credential. It exists so ChatGPT plan usage can be attributed per
host, separately from the issued client id.

**The redirect URI — and this is the good news.** SIWC explicitly permits a
**dynamic port**:

> Use an HTTP loopback callback on `127.0.0.1` from initial registration
> onward, for example `http://127.0.0.1:1455/auth/callback`. … Later sign-ins
> may use another available port, such as `http://127.0.0.1:54321/auth/callback`;
> **only the port may vary.** Keep the scheme, host, and path unchanged:
> `/callback` does not match `/auth/callback`. … Do not substitute with
> `localhost`.

So our existing **random ephemeral port** design works as-is. Three
constraints to honour: the host must be `127.0.0.1` (not `localhost`), the path
must be `/auth/callback`, and the *same* URI must be sent in the
authorization request and in the code exchange.

This is a meaningful improvement on the Codex CLI, whose Hydra allow-list pins
the port to 1455/1457. It also removes the collision risk that would have
made a Codex-path login fight with a running Codex CLI.

**No device-code flow.** SIWC documents only the browser loopback flow — there
is no device-code page in the documentation, and `dynamic_agent_client` is not
mentioned as usable with one. Our x.ai `--device` option therefore has no
direct SIWC equivalent, and headless/SSH use of the ChatGPT path is an open
question (Part 5). The Codex CLI does ship a device flow
(`/api/accounts/deviceauth/usercode`); whether it accepts a dynamically
registered client id is unknown and would need testing.

**Scopes.** `openid profile email offline_access resource.invoke
chatgpt.tokens.use.direct`.

`chatgpt.tokens.use.direct` is the one that matters — it is what grants use of
the user's ChatGPT plan. The docs are explicit that *"a valid ID token alone
does not authorize ChatGPT plan usage"*: we must check the **granted scopes in
the token response** before enabling inference, and if the scope is missing,
keep the sign-in but mark plan usage as disabled.

**Token handling.** The token response carries `access_token`, `refresh_token`,
`id_token`, `token_type`, `expires_in`, `scope` and `earliest_refresh_at`.

- Access tokens last **one hour**.
- Refresh tokens last **30 days** and **rotate on every refresh**.
- The access token's `aud` is `https://api.openai.com/v1`.

Unlike the Codex CLI's WHAM path, there is **no `chatgpt_account_id` claim and
no `ChatGPT-Account-Id` header** on this route — the public API identifies the
account from the token itself. That removes a whole category of plumbing.

Refresh is a form POST to the same token endpoint with
`grant_type=refresh_token`, the **issued** client id, the refresh token, and
`resource=https://api.openai.com/v1`, omitting `scope` to retain the grant.
Same mutex-and-re-read discipline as the x.ai path; the `TokenProvider` trait
is already the right interface.

**Credential record.** OpenAI publishes a suggested shape, close to what we
want:

```json
{
  "email": "user@example.com",
  "issuer": "https://auth.openai.com",
  "subject": "<validated id_token sub>",
  "client_id": "<ISSUED client id>",
  "ext_agent_host_id": "urn:uuid:...",
  "id_token": "...",
  "access_token": "...",
  "refresh_token": "...",
  "token_type": "Bearer",
  "expires_in": 3600,
  "scopes": ["chatgpt.tokens.use.direct", "email", "offline_access",
             "openid", "profile", "resource.invoke"],
  "saved_at": "<UTC timestamp>"
}
```

We would add our own fields (`plan_enabled: bool`, our own `obtained_at`) and
store it 0600, atomically, alongside the x.ai credential.

**Signing out.** Revoke the renewable session first: form POST to the
`revocation_endpoint` from
`https://auth.openai.com/.well-known/openid-configuration` with
`token=<refresh_token>`, `token_type_hint=refresh_token` and the issued client
id. An empty `200` is success. Revoking does **not** delete the registered
client, so the issued client id stays valid for a later sign-in.

**ID-token validation.** OpenAI asks the client to verify the signature
against the published JWKS and to check issuer, audience, expiry and nonce.
That is right for a multi-tenant app. For a single-user local proxy the token
arrives over TLS straight from the token endpoint, and we already read
unverified claims on the x.ai path — but here the `aud` check *is* meaningful
(it must equal our issued client id), so we should at minimum verify `iss`,
`aud` against the issued client id, `exp`, and the `nonce` we sent.

### 3.5 The Codex inference surface

> **Update (2026-11-13).** Rewritten. The SIWC path does **not** use the
> internal `chatgpt.com/backend-api/codex` (WHAM) backend that the Codex CLI
> talks to. It uses the **public** Responses API. This is the single biggest
> improvement in the update.

**The endpoint is the public API.** From OpenAI's own guidance:

> **Supported endpoint:** Use the public Responses API endpoint above for this
> ChatGPT plan usage flow; **do not point it at ChatGPT's `backend-api`
> endpoints.**

| | x.ai | ChatGPT via SIWC |
| --- | --- | --- |
| Base | `https://api.x.ai/v1` | `https://api.openai.com/v1` |
| Path style | `/v1/…` | `/v1/…` — **the same** |
| Inference | `/v1/chat/completions`, `/v1/responses` | `/v1/responses` only |
| Catalogue | `GET /v1/models` → `{data:[{id}]}` | `GET /v1/models` → `{models:[{slug, display_name, visibility}]}` |
| Required headers | — | none beyond `Authorization` |
| Required fields | — | `store: false`, `stream: true` |

Two consequences fall straight out of this table.

**The path style is no longer a problem.** Our `upstream::normalize_path`
(`src/upstream.rs:316`) prefixes `/v1` to everything, and `api.openai.com/v1`
is exactly that. `PathStyle` collapses to a single variant. That removes the
largest piece of complexity the WHAM design would have needed — and it means
the two providers are *more* alike than the Codex CLI's path ever was.

**No `ChatGPT-Account-Id` header.** The public API identifies the account from
the token. `HeaderPolicy` for the OpenAI provider is empty.

**But `stream: true` is mandatory.** From the preview limitations:

> **HTTP requests:** Set `store: false` and `stream: true`.

That is a real constraint on our proxy, and it cuts both ways:

- For a **streaming** client request, we forward the upstream SSE unchanged.
  Easy.
- For a **non-streaming** client request, we must still ask the upstream for a
  stream, then **aggregate it into a single response object**. That is a
  genuine piece of work: consume the event stream, reassemble the output text
  and usage, and emit one JSON body.

**Unsupported request fields must be stripped.** The route rejects a specific
list outright:

> **Unsupported fields:** Omit `background`, `conversation`, `max_output_tokens`,
> `max_tool_calls`, `metadata`, `moderation`, `multi_agent`, `prompt`,
> `prompt_cache_retention`, `safety_identifier`, `temperature`, `top_logprobs`,
> `top_p`, `truncation`, and `user`.

A Chat Completions client sending `temperature: 0.7` or `max_tokens` would be
rejected outright. So the OpenAI provider needs a **request sanitizer** that
removes these fields, injects `store: false` and `stream: true`, and ensures
`instructions` (or developer messages) are present — the docs note that
explicit `{type: "message", role: "system"}` items are rejected.

**Unsupported tools.** Image generation, file search, Code Interpreter, native
computer use, hosted MCP/connectors and Responses `tool_search` are all
rejected on this route. `error.code = subscription_sharing_unsupported_capability`
with an `error.param` naming the offending input is how that surfaces.

**Conversation state.** `previous_response_id` must be omitted; the full
history goes in `input` on every turn.

**`/v1/chat/completions` for a ChatGPT model.** The trade-off looks different
now, because the upstream route is Responses-only *and* streaming-only:

- **(b) Do not translate.** ChatGPT models are served on `/v1/responses` only.
  A `/v1/chat/completions` request naming one gets a clear error naming the
  endpoint to use instead.
- **(c) Translate both ways.** Convert Chat Completions to the Responses
  request shape and the Responses response back — *and* aggregate the
  mandatory upstream stream into a buffered Chat Completions response when
  the client did not ask for a stream.

Option (c) now has to do **three** things rather than two: request
translation, event-stream → buffered response, and event-stream → Chat
Completions SSE. The event vocabularies do not overlap
(`chat.completion.chunk` with `choices[].delta.content` versus
`response.output_text.delta`, `response.output_item.added`,
`response.function_call_arguments.delta`, `response.completed`), so each
needs its own state machine.

**Recommendation: ship (b) first, but build the aggregator in the same phase.**
Note that the aggregator is needed even under (b) — a client asking for a
*non-streaming* `/v1/responses` still needs the upstream stream collapsed into
one body. That aggregator is the unavoidable piece of work and belongs in
Phase 5. The Chat Completions translation layered on top of it is the
optional Phase 6.

### 3.6 Merged `/v1/models`

`/v1/models` should return the union of every logged-in provider's catalogue,
each normalised to the OpenAI shape:

```
GET /v1/models
→ GET {xai}/v1/models              → data[].id
→ GET {openai}/v1/models           → models[].slug  → data[].id
→ merged, deduped, one array
```

> **Update (2026-11-13).** The OpenAI catalogue is now served from the public
> `GET https://api.openai.com/v1/models` with the same access token used for
> inference — no `client_version` query parameter, no WHAM path. Entries carry
> `slug`, `display_name` and `visibility`; OpenAI's own example filters to
> `visibility == "list"` for a model picker while preserving the server's
> ordering, and so should we.

**This also settles the research note's highest-value open question.** The
ChatGPT catalogue is live and served from the public API, so that side does
**not** need a compiled-in model list. (The Codex CLI bundles or caches its
own catalogue; we do not have to.)

Each entry should probably carry a `owned_by` that identifies the provider
(`xai` / `openai`), so a client can tell where a model comes from.

`GET /v1/models/{id}` should resolve against the merged catalogue and return
the provider's own entry — or, per Part 2, simply `404` for an unknown id.

### 3.7 Plan and usage limits

The research in `plans/research-model-refresh-codex-eggserve-limits.md`
established what each provider exposes — but that note predates SIWC, so the
ChatGPT column needs qualifying.

| | x.ai | ChatGPT via SIWC |
| --- | --- | --- |
| Live budget | `x-ratelimit-*` headers on inference responses | **not documented on the public route** |
| Plan type | `tier` claim on the token (`tier: 1`) | **not exposed** by the SIWC route |
| Window details | headers only | not exposed; the signal is the error code |
| In-band signal | `429` + headers | `response.failed` with `subscription_sharing_usage_limit_exceeded` (429) or `subscription_sharing_usage_unavailable` (503) |

**What changed.** The rich `GET /wham/usage` endpoint and the `x-L-*` /
`x-codex-*` headers belong to the **Codex CLI's internal WHAM backend** — the
route SIWC explicitly tells us *not* to use. On the supported public route,
OpenAI instead documents:

- the error codes above, which can arrive **mid-stream** after the response
  has already begun;
- a link to [ChatGPT Settings → Usage](https://chatgpt.com/settings/usage),
  where the user can review app usage and set per-app limits.

OpenAI does not publish a usage/quota API for this route. Whether the WHAM
`/wham/usage` endpoint happens to accept a SIWC token is **unverified** and
worth a quick test — if it works, it gives us the same rich window data the
Codex CLI gets, and `eggpass status` could show the user's real remaining
allowance. If it does not, the honest position is that ChatGPT plan usage is
reported to the user through OpenAI's own settings page and through the
in-band error codes.

**The cheap, high-value change** is unchanged and still applies: forward
upstream headers on the streaming path. Today `src/server/sse.rs:12` builds the
streaming response with exactly three headers and discards everything the
upstream sent — so a streaming client sees no rate-limit information at all,
while a non-streaming client does. That is an inconsistency worth fixing
regardless of Codex, and it is where the x.ai `x-ratelimit-*` budget becomes
visible to users.

**A provider-specific `/v1/usage` endpoint** stays asymmetric: x.ai has headers
plus the CLI-proxy billing call; ChatGPT has nothing documented. If we add it,
it belongs at the provider layer, not the core: `GET /v1/usage` → dispatch to
the provider that can answer, and return a clear "not available" for the one
that cannot.

### 3.8 CLI surface

There are now **ten** commands. The changes are:

| Command | Change |
| --- | --- |
| `login` | `--provider xai\|openai` (default `xai`). `--device` is x.ai-only — SIWC has no documented device flow. |
| `logout` | `--provider xai\|openai` (default `xai`). Revokes the SIWC refresh token before clearing. |
| `status` | Show both providers' account state; show the routing table; show whether ChatGPT plan usage is enabled; show the host and port actually being served. |
| `port` | **Changes meaning** — applies the change and restarts the daemon, rather than only writing the config. See 3.8.1. |
| `host` | **New** — the bind address, with the same apply-and-restart behaviour as `port`. See 3.8.1. |
| `run` / `stop` / `restart` / `croncheck` / `edit` | Unchanged. |

`status` is where the value is clearest — one screen showing which providers
are logged in, their plan/tier, their token lifetime, whether ChatGPT plan
usage is actually granted (`chatgpt.tokens.use.direct` in the granted scopes),
and which models route where.

### 3.8.1 The address commands

`port` and `host` are the two commands that change where the server listens.
They are deliberately identical in shape, because the failure modes are the
same, and the user should not have to learn two different rules.

**The old rule was wrong for the common case.** `port 9000` used to write the
config and print `run 'eggpass restart' to apply`. That is a trap: the obvious
intent of typing a new port is to move the server there, and the old behaviour
silently did not. It also left the config and the live server disagreeing,
which is why `status` had to report two different ports.

**The new rule.** Each command validates, applies, and restarts in one step:

1. **Validate first.** An invalid port or host is refused before anything is
   written or stopped. Nothing changes, and the command says why. The port
   rule is `1..=65535`, with the existing privileged-port rule intact: below
   `1024` needs `allow_privileged_port = true`. The host rule is an IPv4/IPv6
   literal, or `localhost`/`local`/`loopback`, which all mean `127.0.0.1`. A
   name outside that list is refused rather than resolved — a bind address has
   to name a local interface, and resolving one would make the result depend
   on DNS.
2. **Restart only if it would actually change something.** The comparison is
   against the address the daemon is **actually serving**, read from the pid
   file, not against the config file. Asking for the port that is already
   being served does not restart anything — the point of the rule is that a
   no-op change must not bounce live connections.
3. **Check the port is free before restarting.** If the requested port is
   taken, the command prints `port <port> is taken` and changes nothing: no
   config write, no stop, no restart. The user can immediately try another
   port without having to undo anything. The check happens **before** the
   daemon is stopped, so a taken port never causes an outage.
4. **Apply, and be able to undo.** Stop, write the config, start, wait for
   `/health`. If the start fails, the previous config is restored and the
   daemon comes back up on the old address. A failed change must never leave
   the user with a stopped proxy.

**The safety rule on `host`.** The project's posture is loopback-only, and
`host` is the one command that can break it. Binding to anything other than
`127.0.0.1`/`::1` puts the proxy — and through it the user's x.ai and ChatGPT
subscriptions — on the network. So a non-loopback host is **refused unless
`api_key` is configured**; with an `api_key` it is allowed, and the command
warns that the proxy is now reachable from the network. An empty `api_key`
means "accept any key", which on a non-loopback bind is an open proxy on the
user's subscription.

`host` changes only the bind address. It leaves the port exactly as it is.

**What the pid file needs.** "Only restart if the value actually changed"
requires knowing what the daemon is serving, so the pid file gains a third
line: the host it bound. The line is optional when reading, so a pid file
written by an older build still parses and the daemon still starts.

**Config writing stays surgical.** `config::set_port` edits the file in place
so comments and unknown keys survive; `set_host` is its sibling with the same
behaviour. Neither rewrites the file wholesale.

### 3.9 Configuration

```toml
host = "127.0.0.1"   # the bind address; "localhost"/"local"/"loopback" mean 127.0.0.1
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
issuer = "https://auth.openai.com"
scopes = ["openid", "profile", "email", "offline_access",
          "resource.invoke", "chatgpt.tokens.use.direct"]
agent_name_hint = "eggpass"
resource = "https://api.openai.com/v1"
```

> **Update (2026-11-13).** `[oauth.openai]` has **no `client_id` key**. Under
> SIWC the client id is not a constant we ship — it is *issued* by OpenAI
> during the user's first sign-in and stored with their credential. The only
> constant is the entrypoint, `dynamic_agent_client`, which OpenAI documents
> and which we should not hardcode either if it can be overridden.
> `agent_name_hint` is our app's actual name, sent only on initial
> registration.

The `[oauth.*]` tables already exist for x.ai (`[oauth]` with `client_id`,
`issuer`, `scopes`, `referrer`); this generalises them per provider, which
also keeps the OpenAI constants overridable — the same mitigation that was
applied for x.ai's risk register.

Note the existing config surgery: `eggpass port` performs a surgical text
edit so comments and unknown keys survive. Adding tables must not break that,
and `eggpass host` must use the same technique (Part 3.8.1).

---

## Part 4 — Phasing

Each phase is independently shippable and independently testable.

> **Status: all shipped.** Phases 0–6 and 7's header forwarding are built and
> verified; Phase 7's optional `GET /v1/usage` was deliberately not built.
> See [Part 10](#part-10--what-shipped).

### Phase 0 — Harden the current server (no behaviour change for users)

- **H1** Track in-flight connections; join connection threads during the grace
  window. Fixes the SIGTERM truncation.
- **H2** Make the upstream timeouts configurable; raise the default body
  timeout (5 minutes → 30 minutes) and the global timeout accordingly.
- **H3** Client-connection idle timeout and a max-connection count.
- **H4** Max-concurrent-upstream-requests cap, answering `503` beyond it.
- **H5** Strip provider-owned headers (`x-xai-token-auth`,
  `x-grok-client-version`) from client requests.

*Acceptance:* the concurrency probe in Part 1 keeps its result; a stream
survives `SIGTERM` up to the grace window; a client cannot inject
provider-owned headers.

### Phase 1 — Address commands: `port` and `host` (independent)

The `port` command stops being a config edit and becomes an apply-and-restart
command, and `host` joins it. This phase is independent of everything else:
it touches the CLI, the config writer, the daemon lifecycle and the pid file,
and it can ship on its own.

- `port <n>` validates, checks the port is free, writes the config, restarts
  the daemon, and waits for `/health`.
- `host <addr>` does the same for the bind address, and refuses a non-loopback
  address unless `api_key` is configured.
- Both restart **only** when the requested value differs from what the daemon
  is actually serving, which is read from the pid file.
- The pid file gains a third line: the host the daemon bound. The line is
  optional when reading, so an old pid file still parses.
- `config::set_port` grows a `set_host` sibling with the same surgical
  text-edit behaviour, so comments and unknown keys survive.
- A failed restart restores the previous config and brings the daemon back up
  on the old address.

*Acceptance:* `port 9000` on a free port restarts the daemon and `/health`
answers on 9000; `port <taken>` prints `port <port> is taken` and changes
nothing; `port <current>` does not restart; `host 0.0.0.0` without an
`api_key` is refused; `host 127.0.0.1` on an already-loopback daemon does not
restart.

### Phase 2 — Decide the model catalogue (independent)

Either drop `KNOWN_MODELS` and the synthetic `200` (option c), or move the
list into `config.toml` as `known_models` (the configurable middle ground).

*Acceptance:* `/v1/models` reflects the upstream; unknown ids `404`; no
compiled-in model names remain in the source.

### Phase 3 — Generalise the provider layer (no behaviour change yet)

- `Provider`, `PathStyle`, `HeaderPolicy`, `RequestPolicy`.
- Per-provider credential files under `credentials/`, with the `auth.json`
  migration.
- Per-provider `TokenManager` and per-provider refresh mutex.
- `TokenProvider` implementations for both providers.
- Config: `[providers.*]`, `[oauth.*]`, `[routing.*]`.

*Acceptance:* both providers can be logged in; the existing x.ai behaviour is
byte-for-byte unchanged; the test suite still passes with the new
indirection.

### Phase 4 — ChatGPT authentication via Sign in with ChatGPT

> **Update (2026-11-13).** Rewritten. Dynamic registration via SIWC replaces
> the Codex-CLI-client approach. No device-code flow exists on this path.

- Host id: generate and persist `ext_agent_host_id` (`urn:uuid:…` or an RFC
  9278 JWK thumbprint URI).
- Browser loopback on an **ephemeral** port, path `/auth/callback`, host
  `127.0.0.1`.
- Authorize at `https://auth.openai.com/api/accounts/authorize` with
  `client_id=dynamic_agent_client`, `agent_name_hint`, `ext_agent_host_id`,
  `scope`, `resource=https://api.openai.com/v1`, fresh `state`, `nonce`, PKCE
  S256.
- Capture and persist the **issued** `client_id` from the callback.
- Exchange at `https://auth.openai.com/api/accounts/oauth/token` with the
  issued client id.
- Validate `state`, then the ID token's `iss`, `aud` (against the issued
  client id), `exp` and `nonce`.
- Check the **granted scopes** for `chatgpt.tokens.use.direct`; store
  `plan_enabled` accordingly.
- Refresh with rotation (1 h access, 30-day rotating refresh).
- Revoke the refresh token on logout.
- `eggpass login --provider openai`, `logout`, `status`.

*Acceptance:* a real SIWC sign-in completes and stores the issued client id
alongside the tokens; the token refreshes across rotation; `status` reports
the granted scopes and whether plan usage is enabled; a re-login reuses the
issued client id rather than registering again.

### Phase 5 — ChatGPT inference on the public Responses API

> **Update (2026-11-13).** Rewritten for the public API, not WHAM.

- Route `/v1/responses` to `https://api.openai.com/v1/responses`.
- `RequestPolicy`: strip the rejected fields, inject `store: false` and
  `stream: true`, ensure `instructions` are present.
- **Stream aggregator**: because `stream: true` is mandatory, a non-streaming
  client request requires consuming the upstream event stream and collapsing
  it into one JSON body.
- Catalogue adapter for `models`/`slug` → `data`/`id`, with
  `visibility == "list"` filtering for display.
- Merged `/v1/models`.
- Routing by prefix map, with the unknown-model retry to the default
  provider.

*Acceptance:* a real `gpt-*` request succeeds, streamed and buffered; the
merged catalogue lists both providers' models; a Grok request still routes to
x.ai; a rejected field (`temperature`) is stripped rather than erroring.

### Phase 6 — Chat Completions translation for ChatGPT (optional)

Only if there is demand. Chat → Responses request, Responses → Chat
response, including the streaming event mapping — on top of the aggregator
built in Phase 5.

### Phase 7 — Usage and limits

- Forward upstream headers on the streaming path.
- Optionally, `GET /v1/usage` dispatching per provider.

---

## Part 5 — Risks and open questions

### Risks

> **Update (2026-11-13).** The risk register changed substantially once OpenAI
> published Sign in with ChatGPT. The two highest-severity risks are **closed**:
> the ChatGPT path is now a documented, supported OpenAI integration rather
> than a reverse-engineered reuse of the Codex CLI's client.

| # | Risk | Severity | Mitigation |
| --- | --- | --- | --- |
| ~~R1~~ | ~~OpenAI does not support third-party use of the Codex OAuth client.~~ | **Closed** | **Replaced by SIWC.** OpenAI does not intend third-party apps to reuse the Codex CLI's `client_id`; it now provides a dedicated registration path for open-source and locally hosted apps. The plan uses that path. |
| ~~R2~~ | ~~The Codex backend's contract can change without notice — it is an internal API (WHAM).~~ | **Closed** | **No longer applicable.** SIWC points at the *public* `https://api.openai.com/v1` and explicitly says not to use `backend-api`. |
| R3 | The ChatGPT plan allowance is shared with the user's own ChatGPT usage and with every other app using their plan. OpenAI states the five-hour limit is shared across all apps for Plus users, and does not apply for Pro. | Medium | Document it, and link users to [ChatGPT Settings → Usage](https://chatgpt.com/settings/usage) as OpenAI instructs. |
| R4 | Our x.ai path is metered on the **standard API tier** (tier 1), not the SuperGrok consumer pool — it is billed like API usage. | Medium | Document it clearly in the README. It is not a defect, but it is surprising. |
| R5 | **No headless sign-in.** SIWC documents only the browser loopback flow, so `eggpass login --provider openai` needs a browser and a reachable loopback port. Our x.ai `--device` option has no equivalent. | Medium | Accept it for now; investigate whether the undocumented Codex device endpoint accepts a dynamically issued client id. Otherwise a user on a headless box cannot sign in. |
| R6 | **Silent parameter loss.** The route rejects `temperature`, `top_p`, `max_output_tokens` and a dozen other fields. A Chat Completions client that sends `temperature: 0.7` will have it stripped rather than honoured — the request succeeds but behaves differently. | Medium | Strip deliberately, and surface it in the response (a header or a note) rather than silently dropping it. This is a fidelity trade-off, not a bug. |
| R7 | **`stream: true` is mandatory**, so every ChatGPT request is a stream upstream. The aggregator that collapses it for non-streaming clients is required work, and it is where bugs will live. | Medium | Build it in Phase 5 with a proper event-parsing test suite; treat `response.completed` as the only success signal. |
| R8 | SIWC is labelled a **preview**. Field lists, error codes and the registration flow can change. | Medium | Keep the provider's constants in one place and make them config-overridable so a change is a config edit, not a release. |
| R9 | Two providers double the credential-handling and refresh surface. | Medium | The `TokenProvider` seam already exists; the per-provider `TokenManager` is a small delta. |
| R10 | The issued client id is bound to **one user and one workspace**. A user with several ChatGPT accounts needs several registrations, each with its own credential record. | Low | SIWC's "Accounts and sessions" guidance describes exactly this; store one record per registration and allow switching. |
| R11 | Usage-limit errors can arrive **mid-stream**, after the response has begun. A naive client sees a truncated body rather than a clean error. | Low | Propagate the upstream error shape; document `subscription_sharing_usage_limit_exceeded` and `subscription_sharing_usage_unavailable` in the README. |
| R12 | **`host` can expose the proxy to the network.** The project's posture is loopback-only, and a non-loopback bind puts an OpenAI-compatible endpoint — backed by the user's x.ai and ChatGPT subscriptions — on the LAN. With an empty `api_key` ("accept any key") that is an open proxy on someone's paid plan. | **High** | Refuse a non-loopback host unless `api_key` is configured; warn loudly when it is allowed; keep loopback as the default; document the exposure in the README and in the command's own output. |
| R13 | **The auto-restart in `port`/`host` truncates live streams.** Restarting means `SIGTERM`, which today cuts in-flight SSE responses (Part 1) — so a user changing the port mid-answer loses the answer. | Medium | The restart is the user's explicit intent, so truncation is acceptable *if* it is announced. Phase 0's H1 fix makes the grace window real, and the command says it is restarting before it does. A no-op change restarts nothing, which is the common case. |

### Open questions to resolve during implementation

1. **Does the SIWC route accept the `ChatGPT-Account-Id` header, or is the
   token alone sufficient?** The SIWC docs never mention the header, which
   suggests it is unnecessary on the public route — but the Codex CLI always
   sends it, so a cheap test is worth doing.
2. **Does the WHAM `/wham/usage` endpoint accept a SIWC token?** It is the
   Codex CLI's endpoint, not part of SIWC, but if it works it gives us the
   user's real remaining allowance for `eggpass status`. Worth one test.
3. **Is the `build` fallback upstream actually used?** Determines whether
   dropping `KNOWN_MODELS` costs anything.
4. **Does the undocumented Codex device endpoint accept a dynamically issued
   client id?** This is the only route to a headless ChatGPT sign-in.
5. **Do chat-completions clients matter enough to justify Phase 6?** And if a
   Chat Completions request has its `temperature` stripped, should we warn the
   client in-band?
6. **Should the ChatGPT provider be on by default?** With a supported path the
   original argument for opt-in weakens, but the first release should still
   not surprise anyone by reaching for OpenAI. My recommendation remains
   opt-in via `eggpass login --provider openai`.

---

## Part 6 — The rename

> **Update (2026-11-13).** The project was renamed. `eggpass` is the name
> going forward, everywhere: binary, crate, config directory, environment
> variable, documentation and CI.

### 6.1 Why

The tool is no longer a Grok proxy with a chat-CGPT afterthought. It is a
multi-provider proxy whose providers are peers. A name that names one vendor
misdescribes it, and would need renaming again the moment a third provider
appears. `eggpass` also sits naturally alongside the rest of the eggstack
family (`eggpack`, `eggup`, `eggpool`, `eggserve`, …), which matters for
Part 8.

### 6.2 The rename map

This is the one place the old identifiers are written down — a rename
specification that does not name the old identifiers is not actionable.

| Old | New | Where |
| --- | --- | --- |
| `grokproxy` | `eggpass` | crate name, binary name, `Server:` header, user-agent |
| `GROKPROXY` | `EGGPASS` | environment variable (`EGGPASS_HOME`) |
| `~/.config/grokproxy/` | `~/.config/eggpass/` | config dir, PID file, log file |
| `grokproxy.pid` | `eggpass.pid` | PID file name |
| `grokproxy.log` | `eggpass.log` | log file name |
| `grokproxy auth.json` | `eggpass/credentials/xai.json` | credential file (see Part 3.2) |

### 6.3 Why the config directory moves

The config directory name is part of the environment contract: `EGGPASS_HOME`,
`XDG_CONFIG_HOME`, the PID file and the log file all key off it. Keeping the
old directory name under a new binary name would leave the tool looking like
it belongs to another project, and would collide with the `credentials/`
subdirectory layout the multi-provider design needs anyway.

**Migration.** The directory moves. On first run, if
`~/.config/eggpass/` does not exist but `~/.config/grokproxy/` does, `eggpass`
migrates it: create the new directory `0700`, move `config.toml`, move the
credential file to `credentials/xai.json`, move the PID and log files, and
leave the old directory in place with a `migrated-to-eggpass` marker rather
than deleting it. Deleting user data is not something a proxy should do
unprompted. If the old directory is unreadable, `eggpass` says so and starts
with defaults.

The migration is one-time, idempotent, and never overwrites an existing
`~/.config/eggpass/`.

> **Decision (2026-11-13): the migration is confirmed** as specified — the
> directory moves, and the old one is left in place with a marker rather than
> deleted.

### 6.4 How the rename is executed

The rename is mechanical, but it is not one `sed`:

1. `Cargo.toml`: `[package] name = "eggpass"`. The crate is a single binary, so
   the binary name follows automatically.
2. `src/paths.rs`: the resolution order is `$EGGPASS_HOME` →
   `$XDG_CONFIG_HOME/eggpass` → `~/.config/eggpass`, plus the file names
   `eggpass.pid` and `eggpass.log`.
3. `src/cli/args.rs`: the usage text, the help text, and the binary name in
   error messages.
4. `src/main.rs`, `src/cli/commands/*`: the user-visible strings.
5. `src/server/http.rs`: the `Server:` response header.
6. `src/upstream.rs`: the `User-Agent`, currently `grokproxy/<version>`.
7. `src/auth/*`: error strings that name the tool.
8. `scripts/verify-live.sh`: the binary path and the `EGGPASS_HOME` variable.
9. `.github/workflows/ci.yml`: any binary-name references.
10. `README.md` and everything under `plans/`.

**Renaming the crate changes the `Cargo.lock` package name**, so the lock file
is regenerated rather than edited.

**Regression risk is low but not zero**: the tests assert on strings that
include the tool name (the `Server:` header, usage text), so a missed
occurrence shows up as a test failure rather than as a silent inconsistency.
The grep for the old string across the tree must come back empty before the
change is considered done — with one deliberate exception: this rename map.

---

## Part 7 — The interactive provider picker

### 7.1 The requirement

`eggpass login` and `eggpass logout` both present a picker. The user moves
with the arrow keys **or** `j`/`k`, and confirms with `Enter`.

```
$ eggpass login

  Select a provider to sign in with:

  Use j/k or ↑/↓ to navigate, Enter to select, q/Esc to quit

    > x.ai
        Grok models, on your x.ai subscription
      ChatGPT
        ChatGPT models, on your ChatGPT plan

```

`eggpass logout` uses the same picker but lists **only the providers that are
actually logged in**. A provider with no stored credential is not shown.

### 7.2 The non-TTY fallback is not optional

Every command in this tool is documented as cron-safe, and `eggpass login` is
the one command a user might reasonably want to run from a script, a CI job,
or over a pipe. A picker that hangs on a non-terminal stdin would be a trap.

So the picker has two modes:

- **Interactive** — stdin *and* stdout are terminals. Raw-mode key handling,
  arrow keys, `j`/`k`, `Enter`, `q`, `Esc`, `Ctrl-C`.
- **Fallback** — stdin or stdout is a pipe or a file. Print a numbered list
  and read one line:

  ```
  Select a provider to sign in with:
    1) x.ai
    2) ChatGPT
  Provider [1-2]:
  ```

  An empty line takes the first entry. An out-of-range answer re-prompts
  rather than failing, so a typo does not end a scripted login.

  `EGGPASS_PROVIDER=xai` is also accepted as an explicit, non-interactive way
  to name the provider — useful in scripts where even a prompt is unwelcome.

**Every command must also work when the picker is skipped entirely.** A
`--provider <id>` flag selects a provider directly and suppresses the picker.
The picker is the default, not the only way in. That keeps the tool
scriptable, keeps the CLI spec honest, and gives `croncheck` and friends a
path that never touches the terminal.

### 7.3 How to read keys without an async runtime

The project constraint is no async runtime, so the picker must be a blocking
read loop. Three options:

| Option | How | Verdict |
| --- | --- | --- |
| **crossterm** | `crossterm::event::read()` in a loop, with `terminal::enable_raw_mode()` | **Recommended** |
| Hand-rolled | `nix::sys::termios` for raw mode on Unix; `windows-sys` for the Windows console mode | Viable, but two platform paths to maintain |
| Line mode only | Print a numbered list, read a line | Necessary as the fallback, insufficient on its own |

**crossterm** is what the rest of the family uses — `snip-it` (another
eggstack project) builds its fuzzy-search TUI on `ratatui` + `crossterm`.
crossterm's `event::read()` is blocking and its dependency tree does not
require tokio, so the "no async runtime" constraint survives. The cost is
`crossterm` plus its own dependencies (`mio`, `parking_lot`, `signal-hook`,
`bitflags`), which is a real addition to a dependency list we have kept
deliberately small.

**Hand-rolling with `nix`** would add nothing on Unix — we already depend on
`nix` — but Windows would need `windows-sys` for `SetConsoleMode`, and the
`IS_ENHANCED_VK` arrow-key decoding is fiddly. crossterm already solves both,
and solves them on the platforms where they actually bite.

**Recommendation: crossterm.** The dependency is justified by one feature
that is unpleasant to write twice, and it buys the ecosystem consistency the
project wants anyway.

> **Update (2026-11-13) — reversed, and shipped on `nix`.** The crossterm
> picker was built first and **rendered the menu as a staircase**: crossterm's
> `enable_raw_mode` calls `cfmakeraw`, which clears `OPOST`, so a bare `\n`
> steps down a line without returning the cursor to the first column. The
> rewrite follows the family's **own terminal selector** — the one eggpool's
> `connect` command uses — on `nix`'s termios, and the dependency list got
> *smaller* rather than bigger.
>
> What the rewrite added:
>
> - the screen is **cleared and the menu redrawn from the top** on every
>   keystroke, rather than scrolled and patched;
> - every line ends with an explicit `\r\n`, because raw mode turns off the
>   kernel's newline translation;
> - the interactive path needs **both** stdin and stdout to be terminals, so a
>   terminal on stdin with a pipe on stdout never writes cursor-control
>   sequences into the pipe;
> - the cursor **stops at the ends** instead of wrapping;
> - `q` cancels as well as `Esc`, and `Ctrl-C` is an **interruption** rather
>   than a cancellation;
> - control bytes in a label are neutralised;
> - **pseudoterminal tests** drive the selector over a real PTY, so this class
>   of bug cannot come back unnoticed.
>
> The full account is in
> [`shipped-2026-11-13.md`](shipped-2026-11-13.md#the-picker-was-rendered-wrongly).

> **Decision (2026-11-13): use crossterm.** The raw-mode picker ships as
> specified, with the numbered-list fallback always available. The
> "ship the fallback first" option is not taken.

### 7.4 Drawing the picker

The picker needs to redraw in place. The simplest correct approach is to
print the menu, then on each keypress move the cursor up by the number of
lines the menu occupies and reprint. That needs no full-screen alternate
buffer, no diffing, and no terminal-size negotiation — and it leaves the
final menu visible in the scrollback, which is what a user wants after a
login.

If `ratatui` is ever adopted for a richer UI, the picker moves into it. But
for two to five providers, redraw-in-place is the right size of solution.

### 7.5 Failure modes to handle

- **Ctrl-C during the picker** — restore the terminal and exit non-zero. The
  terminal must be restored on *every* exit path, including panic: use a
  guard object whose `Drop` restores the terminal mode.

  > **As shipped.** `Ctrl-C` is reported as an **interruption**, not a
  > cancellation, so it exits `1` with a message while cancelling (`q` or
  > `Esc`) exits `0` and changes nothing. That distinction is the reference
  > implementation's, and it is the right one: cancelling is a choice,
  > interrupting is the user giving up on the whole command.
- **EOF on stdin** in the fallback — treat it as "no selection", exit `0`.
- **Terminal width** — truncate long provider descriptions rather than
  wrapping them, so the redraw arithmetic stays correct.
- **No providers configured** — cannot happen today (x.ai and ChatGPT are
  built in), but if the provider set ever becomes empty the picker must say so
  rather than loop forever.

### 7.6 Where the picker lives in the code

A new `src/cli/prompt.rs` module, deliberately kept out of `src/cli/commands/`
because it is a UI primitive, not a command:

```rust
pub fn select(label: &str, options: &[Option]) -> Result<Option, PromptError>;
```

The command layer supplies the option list; the module owns the terminal
handling. `logout` filters the list before calling it, so the "only show what
you are logged into" rule lives in the command, and the picker stays dumb.

---

## Part 8 — Packaging and updates: eggpack / eggup

> **Update (2026-11-13).** New. The goal is for `eggpass` to use the same
> packaging and update machinery as the rest of the eggstack family, so the
> repos look and behave alike.

### 8.1 What the two tools are

They are a matched pair, and the split matters:

| | eggpack | eggup |
| --- | --- | --- |
| Side | **producer** | **consumer** |
| Job | Turns a versioned release contract into a build/qualify/finalize plan, a deterministic GitHub Actions workflow, and a **draft** GitHub release | Performs verified local installs, updates and rollbacks of release artifacts |
| Publishes? | **Never.** It stages a draft; publication is a separate human action | No — it installs locally |
| Authenticity | **None.** SHA-256 checksums are integrity facts only | **None.** Same posture, explicitly |
| Published to crates.io | `eggpack-manifest` 0.1.1 (schema-v1 evidence) | `eggup-core`, `eggup-acquisition`, `eggup-eggfetch`, `eggup-curl`, `eggup-archive`, `eggup-eggpack`, `eggup-service` — all 0.2.0 |
| MSRV | 1.89 | 1.89 |

The `eggpack-cli` binary itself is **not** published to crates.io — it is built
from the eggstack source (`cargo build -p eggpack-cli`). That matters: the
producer side is a tool we vendor or invoke in CI, while the consumer side is
a normal dependency.

### 8.2 The producer side: eggpack

A release is three pure file-in/file-out commands — no network, no token, no
real build. It needs six input files:

| File | What it holds |
| --- | --- |
| `distribution.toml` | The release contract: targets, artifacts, names |
| `pack-config.toml` | Pack configuration |
| `build-bindings.toml` | How each target is built |
| `qualification-bindings.toml` | How each target is qualified |
| `draft-template.json` | The GitHub draft release template |
| GitHub policy | Release/draft policy |

The flow:

```sh
# 1. resolve intent -> release plan + executable CI graph + draft identity
eggpack ci _resolve-release --contract distribution.toml --pack-config pack-config.toml \
  --build-bindings build-bindings.toml --qualification-bindings qualification-bindings.toml \
  --selected linux-x64 --tag 1.2.6 --source-revision "$REV" --template draft-template.json \
  --source-root . --output-plan out/release-plan.json \
  --output-ci-plan out/release-ci-plan.json --output-github-policy out/draft-policy.json

# 2. render the workflow — deterministic: same inputs, byte-identical output
eggpack ci generate --ci-plan out/release-ci-plan.json \
  --github-policy github-policy.json --output out/release.yml

# 3. prove a checked-in workflow still matches its plan — never writes, exit 1 on drift
eggpack ci check --ci-plan out/release-ci-plan.json \
  --github-policy github-policy.json --workflow out/release.yml
```

**What this gives `eggpass`:**

- A **deterministic** release workflow. The same inputs render byte-identical
  output, so the workflow file is generated, not hand-maintained.
- **Drift detection.** `eggpack ci check` exits non-zero when the checked-in
  workflow no longer matches its plan. That belongs in our existing CI as a
  required gate, exactly like `cargo fmt --check`.
- **A draft-only release.** `_prepare-stage` materialises the finalised
  assets, checksum sidecars, `release-manifest.json`, `install.sh` and
  `install.ps1`. `_stage-github-draft` reconciles that into a GitHub draft
  via `GITHUB_TOKEN`. **Publication stays a human action.**
- **A release manifest** — `release-manifest.json` — which is exactly what
  `eggup-eggpack` consumes on the other side.

`eggpack contract expand --contract distribution.toml --release-id v1.2.6
--target linux-x64 --field asset` lets a *consumer* repository read a producer
fact without re-implementing TOML parsing. `eggpass` is a producer, so this
is mostly for other repos that ship `eggpass` binaries — worth knowing about,
not needed now.

**Targets.** `eggpool` and `eggress` publish Linux x86_64, Linux aarch64 and
macOS arm64, and treat Windows as unqualified. `eggpass` should match:
those three targets, and Windows documented as unsupported rather than
half-supported.

### 8.3 The consumer side: eggup

`eggup` is the library behind `eggpool update` and `eggress update`. Its core
is a five-step transaction, and each step returns the next type so a
committed transaction is only reachable by passing all five:

| Step | Guarantee |
| --- | --- |
| `.prepare()` | Bytes staged in an owner-private directory (`0700`/`0600` on Unix) |
| `.verify_integrity()` | Staged bytes match the declared SHA-256 |
| `.validate(&v)` | The candidate check passed — every member must be `Verified` |
| `.commit(ownership)` | Ownership re-proven and digests re-hashed **under the lock** |

`RolledBack` and `RecoveryRequired` are real outcomes, not errors — the
transaction models failure, rather than pretending it cannot happen.

**Which crates `eggpass` would use:**

| Crate | Use |
| --- | --- |
| `eggup-core` | The transaction mechanics. Sole dependency `sha2`. |
| `eggup-eggfetch` | Native HTTP acquisition, in-process Rustls. Or `eggup-curl` if we prefer an external `curl` and a smaller link footprint — the choice is explicitly a footprint-and-trust decision, and both implement the same `AcquisitionTransport` seam. |
| `eggup-archive` | Bounded, allowlisted extraction of already-verified tar.gz / zip |
| `eggup-eggpack` | Reads an Eggpack `ReleaseManifest` v1 — the bridge between the two halves |
| `eggup-service` | **Optional.** systemd / launchd / cron / Windows SCM registration, "without automatic privilege escalation" |

**`eggup-service` is the interesting one for us.** Our daemon is
deliberately self-contained and PID-file based, with no launchd or systemd
involvement — a deliberate choice recorded in the original plan. `eggup-service`
offers exactly the service managers we decided not to depend on. The
recommendation is to keep our own PID-file daemon as the default and treat
`eggup-service` as an *optional* `eggpass service install` extra, if at all.
It must not become a dependency of the default path.

### 8.4 The install pattern

Both `eggpool` and `eggress` ship the same shape:

- `scripts/install.sh` — a `curl | bash` installer that fetches the
  **qualified GitHub raw binary** for the target, **verifies its SHA-256
  before executing it**, and commits it to `~/.local/bin/<name>`. It never
  invokes a language runtime or a source build.
- `<name> update` — self-update through eggup: the latest catalogued release,
  or one exact historical version.
- `<name> update 0.8.0` — an exact version switch, which doubles as rollback.
- `<name> install-provenance` — shows whether the installation is
  package-manager-managed or standalone, and which is the update authority.

`eggpack`'s `_prepare-stage` emits **both** `install.sh` and `install.ps1`,
so the staging step generates the installers rather than us hand-maintaining
them.

**For `eggpass` this gives us, for free:**

- `eggpass update` — a verified, rollback-capable self-update.
- `eggpass install-provenance` — a clear answer to "who is allowed to update
  this binary?".
- The same install ergonomics as every other eggstack tool, which is the
  point.

### 8.5 The MSRV cost

**eggpack and eggup both require Rust 1.89. Our project is MSRV 1.85.**

Adopting `eggup-*` as a dependency raises our MSRV to 1.89. That is not a
problem in itself — 1.89 is well behind current stable — but it is a real
change to a stated project constraint, and it should be made deliberately
rather than discovered at build time.

If staying on 1.85 matters more than the shared update ecosystem, the fallback
is to implement `eggpass update` directly on `ureq` + `sha2` (both already
dependencies) and skip eggup. That is a few hundred lines, and it would make
`eggpass` the one eggstack tool with a bespoke updater — which is exactly the
uniformity this decision is trying to buy.

> **Decision (2026-11-13): raise the MSRV to 1.89 and adopt eggup.** The
> fallback is not taken. `rust-version = "1.89"` in `Cargo.toml`, the CI MSRV
> job moves to 1.89, and the README's requirements line moves with it. The
> bespoke-updater path is dropped, so eggpass ships the same verified-update
> mechanism as the rest of the family.

This is the one place the project constraint moves rather than being
protected, and it is worth a line in the release notes when it happens.

### 8.6 The `eggpass update` command

> **Update (2026-11-13).** The reference implementation for this command is
> **`cargo-cleanme`**, not eggpool. eggpool's `update` command is non-standard
> for the family because eggpool was originally a Python project distributed
> through pip — its `update` exists to cover the move to Rust, not as the
> house pattern. cargo-cleanme is the standard-shaped consumer, and it is what
> this command follows.

New command, matching the family:

```
eggpass update              # latest catalogued release, same owner
eggpass update 0.2.0       # exact historical-version switch (also the rollback)
eggpass update --check      # report what would happen; change nothing
```

Behaviour:

- Acquire the release manifest (via `eggup-eggpack`), select a candidate,
  verify integrity, validate, and commit under eggup's lock.
- **Never** self-update in place while the daemon is running with the old
  binary. The update replaces the binary on disk; the running daemon keeps
  the old image until it is restarted. `eggpass update` says so.
- The daemon is **not** restarted automatically. Restarting is `eggpass
  restart`, which the user already knows about. An implicit restart during an
  update would cut live streams — see the SIGTERM truncation issue in Part 1.
- Rollback is the same command with the older version.

> **As shipped.** The `install-provenance` subcommand was **not** built: the
> provenance that matters is the one that decides whether a mutation is
> allowed, and `eggpass update --check` prints it as the `owner:` line.
> `cargo install`-managed binaries are refused with the `cargo install`
> command to run instead, and an unprovable installation is refused rather
> than guessed. The full behaviour is in
> [`cli-spec.md`](cli-spec.md#eggpass-update).

### 8.7 What this means for the CI we already have

Our `.github/workflows/ci.yml` stays as the *check* workflow (fmt, clippy,
test, MSRV). eggpack generates a **separate, deterministic release workflow**
(`out/release.yml`) that chains preflight → build → qualify → required gate →
aggregate → stage → draft.

Both are checked in. `eggpack ci check` runs in the check workflow and fails
on drift, so the generated release workflow can never silently diverge from
its plan.

---

## Part 9 — Relationship to eggpool

> **Update (2026-11-13).** New. This was the most consequential discovery in
> the whole review. **It is now settled** — see 9.3: eggpass is built and
> maintained separately, and eggpool ingests its OAuth layer downstream.

### 9.1 What eggpool already is

[`eggstack/eggpool`](https://github.com/eggstack/eggpool) is a **lightweight,
LAN-hosted proxy that aggregates multiple AI provider accounts behind OpenAI
Chat Completions, OpenAI Responses, and Anthropic Messages-compatible
paths.** Its own README describes, among other things:

- "Load-based routing across multiple providers and accounts with dynamic
  model discovery, quota awareness, and health gating"
- "Optional sticky model-router aliases for virtual-model selection"
- `eggpool connect` — "Add a provider account", with an **interactive
  onboarding wizard** (`eggpool onboard`)
- `eggpool serve` / `status` / `stop` / `restart` / `rehash`
- `eggpool models refresh` — "Refresh the model catalog"
- `eggpool update` — verified updates, with exact-version rollback
- `eggpool accounts status`, `eggpool accounts explain --model <id>`
- Model pinning as `model/provider` (e.g. `"gpt-4o/openai"`)
- A SQLite-backed dashboard, multi-user personal tokens, per-user budgets

That is, feature for feature, the multi-provider proxy this plan has been
designing — and it already has the interactive provider onboarding, the model
catalogue merging, the routing table, and the update path.

### 9.2 Why eggpass is still not eggpool

The constraints that shaped this project are deliberately narrower than
eggpool's:

| | eggpool | eggpass |
| --- | --- | --- |
| Audience | LAN-hosted, multi-user, managed | single user, loopback only |
| Storage | SQLite + dashboard + backups | none — one config file, no database |
| Exposure | binds `0.0.0.0:11300` by default, login page, personal tokens | binds `127.0.0.1` only |
| Credentials | primarily API keys from the environment | **subscription OAuth credentials** (x.ai, ChatGPT) |
| OAuth | provider accounts | PKCE loopback, ephemeral port, device code, SIWC dynamic registration |
| Extra surface | admin/dashboard, multi-user budgets, live rehash, model enrichment from OpenRouter/Artificial Analysis/HuggingFace | none — "no database, no cloud, no admin API" |

The distinctive part of eggpass is **subscription OAuth**: signing in with a
ChatGPT Plus account or an X Premium subscription and using that plan's
allowance through an OpenAI-compatible API. eggpool's `connect` is built around
API keys and provider accounts; the OAuth-flavoured, subscription-credential
flow is not what it is for.

So eggpass is the **minimal, single-user, OAuth-first sibling** — not a
duplicate. This was confirmed as the operating model in 9.3: eggpass is
maintained on its own, and eggpool is the place where a multi-user,
LAN-hosted, database-backed version of the idea would live.

### 9.3 The decision — keep them separate

> **Update (2026-11-13).** **Decided: option 1. `eggpass` is built and
> maintained as its own project.** The two are separate binaries, separate
> repos, separate releases, separate configs, separate ports. eggpass does
> not become a feature of eggpool, and eggpool does not absorb eggpass.

The reasoning, in the owner's words: eggpass exists to be a **small,
lightweight OAuth-to-proxy** that puts Grok and ChatGPT behind an
OpenAI-compatible endpoint, for tools that speak OpenAI but do not support
Grok — mcode being the concrete case. It is a single-user, local tool, and it
stays that way.

That use case is genuinely different from eggpool's. eggpool is LAN-hosted,
multi-user and database-backed; eggpass is loopback-only, single-user and
file-backed. The two are not competing implementations of the same thing.

**What this settles.**

- The ChatGPT provider work in Part 4 Phase 5 is **not** redundant. eggpass
  needs it.
- eggpass stays small. The dashboard, the SQLite store, multi-user personal
  tokens, per-user budgets, quota-aware routing across accounts and model
  enrichment are **not** reimplemented here, and eggpass should say so plainly
  rather than growing toward eggpool's shape.
- The feature boundary is drawn around the OAuth layer, not around the proxy.
  eggpass's distinctive part is **subscription OAuth** — signing in with a
  ChatGPT Plus account or an X Premium subscription and spending that plan's
  allowance through an OpenAI-compatible API. eggpool's `connect` flow is
  built around provider accounts and API keys, which is a different problem.

**The downstream direction.** The plan further down the line is to give
eggpool an OAuth capability, and for eggpool to **ingest eggpass's** OAuth
work rather than reimplement it. eggpass remains maintained separately and
released on its own; eggpool consumes it downstream.

That downstream relationship is a *reason* to keep eggpass's OAuth layer
clean and separable, not a reason to build eggpool's features here. Concretely
it means:

- the OAuth provider layer is a distinct module with no eggpool-specific
  assumptions in it;
- the credential storage, refresh and revocation logic is written to be
  reusable, not to be exactly what eggpass happens to need today;
- nothing in eggpass assumes it is the only consumer of that module.

It also means the two projects' release cycles stay independent — eggpass can
ship a ChatGPT fix without an eggpool release, and eggpool can take the OAuth
capability when it is ready to.

**One thing still worth checking**, because it costs an hour and could change
the downstream plan: does eggpool's existing `connect`/`onboard` flow already
handle ChatGPT subscription OAuth in some form? If it does, the downstream
work is smaller than it looks; if it does not, the OAuth layer eggpass
builds is exactly what eggpool will want to ingest. Either answer supports
building eggpass's OAuth layer on its own terms.

### 9.4 Other eggstack projects worth knowing about

| Project | Relevance |
| --- | --- |
| `eggserve` | Evaluated in the research note. A hardened static file server with an async custom-service seam; **not adopted** — it requires tokio. |
| `eggress` | A Rust-native multi-protocol **network** proxy (SOCKS5/HTTP/CONNECT, pproxy-compatible). A different layer from our API server — not a replacement for our HTTP server, but the right tool if `eggpass` ever needs an outbound proxy chain or a SOCKS listener. It has a blocking embed API (`start_blocking()` / `shutdown_blocking()`). |
| `eggup-service` | systemd / launchd / cron / Windows SCM lifecycle adapters. Optional extra, never a default dependency (Part 8.3). |
| `eggfetch` | The native HTTP client behind `eggup-eggfetch`. Relevant if we ever want to replace `ureq`. |
| `eggchaos` | A stream-fault engine for testing chaotic network conditions. Potentially valuable for our SSE paths — a fault-injection harness for the streaming and refresh-retry logic would be a real quality win. |
| `eggreplay` | A semantic interaction recorder/replayer and regression engine. Also potentially valuable for replaying recorded upstream responses in tests. |
| `eggplan` | A repo planning work-order engine. Possibly relevant to how the eggstack family structures planning work — worth a look when formalising our own plan documents. |
| `gregg`, `snip-it` | The family's TUI references. `snip-it` uses `ratatui` + `crossterm`, which is the precedent for our provider picker (Part 7.3). |

---

## Part 10 — What shipped

The plan above has been built. This part records how the build went, so the
plan and the outcome can be read against each other.

The full record — every phase, every deviation, the live verification
evidence and the two outstanding human steps — is in
[`shipped-2026-11-13.md`](shipped-2026-11-13.md). This part is the summary
and the delta.

### Every phase landed

| Phase | Status | Notes |
| --- | --- | --- |
| 0 — Harden the server (H1–H5) | Shipped | H1–H5 all implemented; the limits became real through the new `[server]` and `[timeouts]` config tables. |
| 1 — `port` and `host` | Shipped | Both apply and restart. The shared machinery is `src/cli/commands/address.rs`. |
| 2 — Model catalogue | Shipped | The static list is **gone**. `GET /v1/models` is a live round trip; unknown ids are `404`. |
| 3 — The provider layer | Shipped | `src/provider/{mod,xai,openai}.rs`; per-provider credential files; per-provider refresh mutexes. |
| 4 — ChatGPT SIWC | Shipped | Dynamic registration, the issued client id persisted, `ext_agent_host_id` persisted, revocation on logout. |
| 5 — ChatGPT inference | Shipped | The Responses API on the public endpoint, the stream aggregator, the merged catalogue. |
| 6 — Chat Completions translation | Shipped | `src/server/translate.rs`. |
| 7 — Usage and limits | **Partly shipped** | Upstream headers reach streaming clients. `GET /v1/usage` was **not** implemented — optional in the plan, and the upstream contract could not be verified. |
| 6.2 — The rename | Shipped | The crate, the binary, `EGGPASS_HOME`, the pid file, the log file, and the config directory. |
| 7 — The picker | Shipped | On `nix`'s termios, following the family's own terminal selector, with the numbered-list fallback and pseudoterminal tests. |
| 8 — Packaging and updates | Shipped | The updater's **policy** is ours; the mechanics are `eggup`'s. |
| 9 — Client keys | Shipped | `eggpass key add`, `eggpass key list`, `eggpass key revoke`. `eggpass` issues the keys, shows each one once, and stores only a salted hash. Added after the plan was written. |

### The delta from the plan

| Plan said | Shipped | Why |
| --- | --- | --- |
| MSRV 1.85 | **1.89** | `eggup`'s crate family requires it. |
| A bespoke updater | `eggup`'s published crates | There is no `eggup` crate — the family is `eggup-core`, `eggup-acquisition`, `eggup-eggfetch`, `eggup-curl`, `eggup-archive`, `eggup-eggpack`. Their mechanics are better than ours would have been. |
| `eggpool`'s `update` as the model | **cargo-cleanme** | eggpool's `update` covers a Python-to-Rust move, not the family pattern. cargo-cleanme is the standard-shaped consumer. |
| `eggup-curl` was an option | `eggup-eggfetch` | `CurlConfig` has no User-Agent seam and the adapter clears the child environment, so the release host answers 403. |
| `ExistingAsOwnedVerifier` proves ownership | `LiveDigestVerifier` | The former asserts from configuration; the latter proves it by hashing the destination under `eggup`'s lock and requiring the pre-transaction digest. |
| `eggpass install-provenance` | **Not built** | The provenance that matters is the one that gates the mutation, and `--check` prints it as `owner:`. |
| `GET /v1/usage` | **Not built** | The upstream contract is unverified, and a guessed contract is worse than none. |

### One bug worth recording

The provider base URL included `/v1` while the request path also carried the
`/v1` prefix, so every request went to `/v1/v1/...` and 404'd. The base URL
is now the **origin**; the path carries the prefix. The unit tests did not
catch it because the stub upstream answered any path — only the first live
request did.

### What is left

Two things, both blocked on something outside the code:

1. **A completed ChatGPT sign-in.** The SIWC implementation is complete, but
   exercising it needs a human to sign in with their ChatGPT account in a
   browser and consent to plan usage.
2. **A published release.** `eggpass update` is implemented and its policy is
   tested, but there is no published `eggpass` release for it to install. The
   repository (`eggstack/eggpass`) is a configured default, not a confirmed
   fact — this repo has no git remote.

Both are written up with the exact commands in
[`shipped-2026-11-13.md`](shipped-2026-11-13.md#what-still-needs-a-human).
