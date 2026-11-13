# Plan: a combined Grok + ChatGPT proxy, and the server-hardening that goes with it

Date: 2026-11-13. Planning only — no code changed.

This document answers the three questions raised in review, then lays out the
design for turning `grokproxy` into a proxy for both x.ai and ChatGPT.

- [Part 1 — The blocking question](#part-1--the-blocking-question-answered-with-measurements)
- [Part 2 — Dropping the static model half](#part-2--dropping-the-static-model-half)
- [Part 3 — The combined proxy design](#part-3--the-combined-proxy-design)
- [Part 4 — Phasing](#part-4--phasing)
- [Part 5 — Risks and open questions](#part-5--risks-and-open-questions)

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
> grokproxy is open-source and runs locally, so it is squarely in that path's
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

### 3.4 Codex authentication — Sign in with ChatGPT

> **Update (2026-11-13).** This section was originally written around reusing
> the Codex CLI's OAuth client. **OpenAI does not intend third-party
> applications to reuse the Codex CLI's `client_id`.** OpenAI now publishes a
> supported path for exactly our case — *Sign in with ChatGPT for Open
> Source* — and this section is rewritten around it. Sources:
> [Overview](https://developers.openai.com/siwc/token-sharing-open-source),
> [Registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
> [Token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference).

grokproxy is open-source and runs locally, which puts it squarely in the
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
Phase 4. The Chat Completions translation layered on top of it is the
optional Phase 5.

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
Codex CLI gets, and `grokproxy status` could show the user's real remaining
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

The nine commands stay. The changes are:

| Command | Change |
| --- | --- |
| `login` | `--provider xai\|openai` (default `xai`). `--device` is x.ai-only — SIWC has no documented device flow. |
| `logout` | `--provider xai\|openai` (default `xai`). Revokes the SIWC refresh token before clearing. |
| `status` | Show both providers' account state; show the routing table; show whether ChatGPT plan usage is enabled. |
| `run` / `stop` / `restart` / `croncheck` / `port` / `edit` | Unchanged. |

`status` is where the value is clearest — one screen showing which providers
are logged in, their plan/tier, their token lifetime, whether ChatGPT plan
usage is actually granted (`chatgpt.tokens.use.direct` in the granted scopes),
and which models route where.

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
issuer = "https://auth.openai.com"
scopes = ["openid", "profile", "email", "offline_access",
          "resource.invoke", "chatgpt.tokens.use.direct"]
agent_name_hint = "grokproxy"
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
  `x-grok-client-version`) from client requests.

*Acceptance:* the concurrency probe in Part 1 keeps its result; a stream
survives `SIGTERM` up to the grace window; a client cannot inject
provider-owned headers.

### Phase 1 — Decide the model catalogue (independent)

Either drop `KNOWN_MODELS` and the synthetic `200` (option c), or move the
list into `config.toml` as `known_models` (the configurable middle ground).

*Acceptance:* `/v1/models` reflects the upstream; unknown ids `404`; no
compiled-in model names remain in the source.

### Phase 2 — Generalise the provider layer (no behaviour change yet)

- `Provider`, `PathStyle`, `HeaderPolicy`, `RequestPolicy`.
- Per-provider credential files under `credentials/`, with the `auth.json`
  migration.
- Per-provider `TokenManager` and per-provider refresh mutex.
- `TokenProvider` implementations for both providers.
- Config: `[providers.*]`, `[oauth.*]`, `[routing.*]`.

*Acceptance:* both providers can be logged in; the existing x.ai behaviour is
byte-for-byte unchanged; the test suite still passes with the new
indirection.

### Phase 3 — ChatGPT authentication via Sign in with ChatGPT

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
- `grokproxy login --provider openai`, `logout`, `status`.

*Acceptance:* a real SIWC sign-in completes and stores the issued client id
alongside the tokens; the token refreshes across rotation; `status` reports
the granted scopes and whether plan usage is enabled; a re-login reuses the
issued client id rather than registering again.

### Phase 4 — ChatGPT inference on the public Responses API

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

### Phase 5 — Chat Completions translation for ChatGPT (optional)

Only if there is demand. Chat → Responses request, Responses → Chat
response, including the streaming event mapping — on top of the aggregator
built in Phase 4.

### Phase 6 — Usage and limits

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
| R5 | **No headless sign-in.** SIWC documents only the browser loopback flow, so `grokproxy login --provider openai` needs a browser and a reachable loopback port. Our x.ai `--device` option has no equivalent. | Medium | Accept it for now; investigate whether the undocumented Codex device endpoint accepts a dynamically issued client id. Otherwise a user on a headless box cannot sign in. |
| R6 | **Silent parameter loss.** The route rejects `temperature`, `top_p`, `max_output_tokens` and a dozen other fields. A Chat Completions client that sends `temperature: 0.7` will have it stripped rather than honoured — the request succeeds but behaves differently. | Medium | Strip deliberately, and surface it in the response (a header or a note) rather than silently dropping it. This is a fidelity trade-off, not a bug. |
| R7 | **`stream: true` is mandatory**, so every ChatGPT request is a stream upstream. The aggregator that collapses it for non-streaming clients is required work, and it is where bugs will live. | Medium | Build it in Phase 4 with a proper event-parsing test suite; treat `response.completed` as the only success signal. |
| R8 | SIWC is labelled a **preview**. Field lists, error codes and the registration flow can change. | Medium | Keep the provider's constants in one place and make them config-overridable so a change is a config edit, not a release. |
| R9 | Two providers double the credential-handling and refresh surface. | Medium | The `TokenProvider` seam already exists; the per-provider `TokenManager` is a small delta. |
| R10 | The issued client id is bound to **one user and one workspace**. A user with several ChatGPT accounts needs several registrations, each with its own credential record. | Low | SIWC's "Accounts and sessions" guidance describes exactly this; store one record per registration and allow switching. |
| R11 | Usage-limit errors can arrive **mid-stream**, after the response has begun. A naive client sees a truncated body rather than a clean error. | Low | Propagate the upstream error shape; document `subscription_sharing_usage_limit_exceeded` and `subscription_sharing_usage_unavailable` in the README. |

### Open questions to resolve during implementation

1. **Does the SIWC route accept the `ChatGPT-Account-Id` header, or is the
   token alone sufficient?** The SIWC docs never mention the header, which
   suggests it is unnecessary on the public route — but the Codex CLI always
   sends it, so a cheap test is worth doing.
2. **Does the WHAM `/wham/usage` endpoint accept a SIWC token?** It is the
   Codex CLI's endpoint, not part of SIWC, but if it works it gives us the
   user's real remaining allowance for `grokproxy status`. Worth one test.
3. **Is the `build` fallback upstream actually used?** Determines whether
   dropping `KNOWN_MODELS` costs anything.
4. **Does the undocumented Codex device endpoint accept a dynamically issued
   client id?** This is the only route to a headless ChatGPT sign-in.
5. **Do chat-completions clients matter enough to justify Phase 5?** And if a
   Chat Completions request has its `temperature` stripped, should we warn the
   client in-band?
6. **Should the ChatGPT provider be on by default?** With a supported path the
   original argument for opt-in weakens, but the first release should still
   not surprise anyone by reaching for OpenAI. My recommendation remains
   opt-in via `grokproxy login --provider openai`.
