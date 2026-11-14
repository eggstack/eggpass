# Security pass — auth hardening, 2026-11-14

The work order for the authentication review of `4f1f381`. Decisions are
settled and the implementation is in place; the phase-5 verification record
follows the code.

A follow-up review of that implementation (`b4a55ff`) found four remaining
defects. They are written up in
[`security-followup-2026-11-14.md`](security-followup-2026-11-14.md), which
is implemented. Two claims in §5.3 — that the drain is bounded by
`MAX_BODY_BYTES`, and that an unauthenticated client never reaches
`read_body` — are corrected there. This document is otherwise the record of
what this pass decided.

| | |
| --- | --- |
| Scope | The client-key gate, the two OAuth flows, credential storage, and every path a secret crosses |
| Out of scope | Agent behaviour; hardening for non-loopback binds beyond requiring a key |
| Trigger | Security review of the auth surface, 2026-11-14 |
| Verdict | No auth bypass, no credential exposure. Five defects to fix, one of them a default-posture problem |
| Release posture | Unchanged — manual review only, nothing published |

---

## 1. Summary

The keyed configuration holds up. A server with an issued key rejects a
missing key, a wrong bearer, and a wrong `?key=` (verified live on the daemon
at `127.0.0.1:8484`), keys are stored as salted hashes at `0600` in a `0700`
directory, and every comparison runs constant-time across the whole store.
Nothing in the audit found a way past a configured key.

What it did find is that **the default configuration is not protected by
loopback against a browser**, and that the documented happy path is the
default configuration. The quick start is three commands, none of which is
`eggpass key add`, and it ends by pointing a client at
`http://127.0.0.1:8484/v1`. A port on loopback is reachable from a web page,
and `Host` is never inspected, so the proxy is a same-origin target for DNS
rebinding.

The decision taken is to stop supporting the unkeyed mode altogether: `run`,
`restart` and `croncheck` refuse to start without a client key, the running
server refuses every request if its last key is revoked, and the quick start
mints one. The authority guard stays as a second lock, because a key is the
only boundary that survives a future regression.

Alongside that are four smaller defects: an unauthenticated abort in the
chunked-body reader, an unauthenticated memory-amplification path, a
plaintext `api_key` in a `0644` config file, and a silent fail-open when the
key store breaks while the daemon is running.

---

## 2. The threat model correction

The project's mental model, stated in the README and in `is_authorized`'s own
doc comment, was:

> While no keys exist the endpoint is open to anything on the machine, which
> is safe for a loopback socket.

(`README.md:91`, `src/server/mod.rs:221`.)

Two things are wrong with that sentence, and they compound.

**Loopback is not "this machine".** Any local process — including one running
as another user on a shared host — can connect to `127.0.0.1:8484`. Without a
key, the boundary is every local process's good behaviour.

**Loopback is not "not the browser".** A web page is a remote party. The
browser will happily send it to `127.0.0.1` on the page's behalf. The
same-origin policy is the only thing that normally stops the page from
*reading* the reply, and DNS rebinding removes it: the attacker's name
resolves to their server for the page load and to `127.0.0.1` afterwards, so
the page and the proxy are one origin. `Host` is the one clue the server has
that this happened, and `handle_request` never looked at it
(`src/server/mod.rs:188`).

A key closes both, because the page neither has it nor can get it. That is why
the primary fix is "a key is required", not "loopback is checked harder".

---

## 3. Findings

Each finding carries its trigger, the impact against 0.1.0 as shipped, and the
evidence collected during the review.

### F-1 — Medium — an unkeyed install is drivable from any web page

**Trigger.** Default install: `api_key = ""`, no issued keys, `host` left at
`127.0.0.1`. The user visits a page that controls its own DNS, served from
`http://evil.example:8484/` so that the rebound address is same-origin. After
the name flips to `127.0.0.1`, a `fetch('/v1/chat/completions', …)` is
same-origin: no preflight, no CORS, `Host: evil.example:8484`, and the
response is readable.

**Impact.** The page spends the signed-in subscription and reads the model's
answers. It cannot read the user's own prompts or history — there is no such
endpoint — so this is account abuse, not data theft. Given that these
providers ban subscription sharing, and that the code already carries the
`subscription_sharing_*` error codes, an attacker with free inference on the
account is a real problem.

**Evidence.**

- With no key configured, `GET /v1/models` carrying
  `Host: evil.example:8487` returned `200 {"data":[],"object":"list"}` on an
  isolated instance. The authority was ignored.
- `POST /v1/chat/completions` with `Content-Type: text/plain`,
  `Origin: http://evil.example` and `Host: evil.example:8487` reached the
  provider client and returned its credential error, not the gate's 401. A
  `text/plain` POST is a CORS *simple request*, so the absent CORS headers and
  the 401 on `OPTIONS` do not stop it.
- `src/server/chat.rs:70` parses the body as JSON without ever reading
  `Content-Type`, so the simple-request shape is accepted.

**Why the default mattered.** `README.md:79` presented `key add` under "Client
keys", after the quick start at `README.md:37`, not as one of its steps. Every
user who followed the quick start sat in the vulnerable configuration.

### F-2 — Low — one unauthenticated packet aborts the daemon

**Trigger.** A single TCP connection with `Transfer-Encoding: chunked`, a
one-byte chunk, then a chunk header of `ffffffffffffffff`.

**Impact.** `body.len() + size` overflows in release builds, the cap check
passes, and `vec![0u8; size]` aborts. With `panic = "abort"` the daemon dies
and nothing restarts it unless `croncheck` is on a timer. It is local-only and
unauthenticated: the key never gets a chance to matter, because the body is
read before the gate (`src/server/http.rs:415` ran before
`src/server/mod.rs:197`).

**Evidence.** Reproduced on an isolated instance:

```
thread '<unnamed>' panicked at src/server/http.rs:536:25: capacity overflow
```

The process was gone on the next probe; `/health` did not answer.

### F-3 — Low — the body is buffered before authentication

**Trigger.** Same ordering as F-2: `MAX_BODY_BYTES` is 32 MB
(`src/server/http.rs:22`) and `max_connections` is 256
(`src/server/http.rs:34`). An unauthenticated local process can hold roughly
8 GB of buffers with 256 in-flight uploads.

**Impact.** Memory exhaustion on a shared host. On a single-user laptop the
attacker is the user's own software, so this is a consequence of F-2's
ordering rather than a separate adversary.

**Evidence.** Code path only: `handle_connection` → `read_request` →
`read_body` → `handler` → auth. The read had no knowledge of the key.

### F-4 — Low — the hand-written `api_key` lives in a `0644` file

**Trigger.** `api_key = "…"` in `config.toml`, the older way of setting a
client key. `write_file` used `std::fs::write` and the process umask
(`src/config.rs:1121`). Any copy of the file, any `--config` path outside the
`0700` directory, and any backup inherits the same mode.

**Impact.** A long-lived plaintext credential readable by other local users or
by anything that indexes the file. The issued keys do not have this problem —
they are hashed and written `0600`. The decision is to keep both mechanisms,
so this file is now a legitimate home for a credential and the mode matters
more, not less.

**Evidence.** On this machine:

```
drwx------  ~/.config/eggpass
-rw-r--r--  ~/.config/eggpass/config.toml
-rw-------  ~/.config/eggpass/keys.json
-rw-------  ~/.config/eggpass/credentials/*.json
```

The `0700` directory currently saved it. The inconsistency is the point:
every other credential file in the project is forced to `0600`.

### F-5 — Low — a broken key store keeps revoked keys live, silently

**Trigger.** `keys.json` becomes unreadable or unparseable *while the daemon is
running* — a hand edit, a partial restore, a disk error. `refresh` keeps the
previously loaded set and records the stamp so the file is not re-read
(`src/keys.rs:266-284`). No log line is written.

**Impact.** A revocation the user believes happened does not take effect until
the next restart. Deleting the file *does* revoke (missing file is an empty
store), so the gap is narrowly the malformed case — but it fails open with no
observability, which is the wrong direction for a key store.

**Evidence.** `read_file` distinguishes `NotFound` from every other error; the
`Err(_)` arm in `refresh` only replaces the stamp.

### N-1 — Note — the redactor does not cover `?key=`

`CREDENTIAL_KEYS` (`src/logging.rs:186`) lists `api_key`, `apikey`,
`authorization` and the token names, but not `key`. Nothing logs a request
target today, so there is no live leak; the query carrier is the one
credential path that can end up in a client's own history, and the safety net
should cover it. Adding the bare word needs a parameter boundary, or
`monkey=` starts redacting.

### N-2 — Note — `EGGPASS_HOME` does not isolate the Grok CLI import

`paths.rs` says `EGGPASS_HOME` "relocates *every* file" (`src/paths.rs:5`),
but `grok_cli_auth_file()` resolves through `dirs::home_dir()`
(`src/paths.rs:127`). A relocated installation still reads `~/.grok/auth.json`.
Found by accident: a review rig on a spare port picked up the real credential
file and attempted one refresh with it (the token was already revoked; nothing
was written). Not a vulnerability — same user — but the doc comment overstates
the isolation, and anyone testing against the OAuth endpoints will hit it.

---

## 4. Prior art

The shape of the fix is settled in the ecosystem.

**Host validation is the rebinding control.** The MCP Streamable HTTP security
guidance, after CVE-2025-66414 / CVE-2025-66416 in the TypeScript and Python
SDKs, now states that servers granting access by network position — loopback,
private network, source-IP trust — must validate `Host` against the names they
serve, on every listener. `Origin` is the CSRF control, not the rebinding
control: a same-origin `GET` after rebinding carries no `Origin` at all, so an
Origin-only check leaves the data read open. For servers authenticated on every
request by a bearer token, the guidance permits accepting any `Origin` but
still requires the `Host` check.

**Exact matching matters.** A substring or suffix test for `localhost` accepts
`localhost.attacker.example`. Vite's `allowedHosts` advisory is the recent
cautionary example; Ollama's CVE-2024-28224 was fixed the same way, by
validating `Host`.

**Chrome's Local Network Access changes the odds, not the requirement.**
Chrome 142 (October 2025) began gating public-origin requests to loopback and
private addresses behind a permission prompt, restricted to secure contexts;
later milestones extended it to WebSockets. PNA's preflight model was
abandoned. A plain-HTTP attacker page can no longer reach `127.0.0.1` silently
in current Chrome, and an HTTPS page needs the user to grant "look for and
connect to any device on your local network". It does not remove the exposure:
the prompt can be granted, other browsers lag, enterprise policy can pre-grant
it, and **loopback-origin pages are not gated at all** — another local web app
can still POST to the proxy. Browser-side controls are a second layer, not the
fix.

**A mandatory key is the only complete control.** Every remaining path above
— the granted prompt, the lagging browser, the local origin — is closed by a
credential the page does not have. That is why the decision is to require one
rather than to rely on the guard.

Sources: [Local Network Access](https://developer.chrome.com/blog/local-network-access/),
[Modern Web Guidance — Local Network Access](https://developer.chrome.com/docs/modern-web-guidance/guides/security/local-network-access),
[MCP origin/host validation discussion](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/3370).

---

## 5. Corrective design

### 5.1 F-1, primary — the server does not run without a key

A key is now a precondition, not a recommendation.

**Command behaviour.**

- `eggpass run` (foreground or daemon) fails with a `CommandError` before it
  binds, forks, or touches the pid file.
- `eggpass restart` checks **before** it stops the old daemon. A missing key
  must not leave the user with a dead endpoint, and the check is cheap.
- `eggpass croncheck` fails the same way. This breaks its "print nothing"
  contract on purpose: a cron job that cannot start the proxy must say why
  rather than fail silently every five minutes.
- `eggpass stop` still works with no key. You must always be able to stop it.

The message names the command:

```
refusing to run: no client key is configured; run `eggpass key add` first
(or set `api_key` in the config file)
```

**Request behaviour.** If the store empties while the daemon runs — the last
key is revoked, or the file is removed — the running server refuses every
request except `/health`, which stays open because `status` and `croncheck`
need it and it carries only a version string. Everything else answers `401`
with a message that says what to do:

```json
{"error":{"message":"no client key is configured on this server; run `eggpass key add`","type":"invalid_request_error","code":"no_api_key"}}
```

`is_authorized` returns false when nothing is configured, on every host. The
loopback exception is deleted, along with `is_loopback_host` from the auth
path; that function is still used to decide whether the guard applies.

**Why not auto-mint.** A key minted by `run` would print once into a forked
daemon's terminal or log, `croncheck` would mint silently on a timer, and a
user who missed the line would have an unrecoverable key and no idea why the
client worked yesterday. Minting stays explicit.

**Why not refuse `croncheck`.** Considered and rejected: cron's whole job is
to keep the daemon alive, and a croncheck that refuses to start is not a
croncheck. The failure is loud instead.

### 5.2 F-1, second lock — the authority guard

A new module, `src/server/guard.rs`, called from `pre_authorize` before
routing, on every request including `/health`.

```rust
pub fn check(request: &Request, bound_host: &str, port: u16) -> Option<Response>
```

**Host.** Parse the `Host` header as an authority: `name`, `name:port`,
`[v6]`, `[v6]:port`. On a loopback bind the host component must match, exactly
and case-insensitively after stripping one trailing dot:

- `127.0.0.1`, `localhost`, `[::1]`;
- anything ending in `.localhost` — a suffix rule, and safe because browsers
  force that TLD to loopback and an attacker cannot serve a page from it;
- the bound literal when it is itself loopback, so `eggpass host 127.0.0.2`
  answers to `127.0.0.2`.

A missing `Host` is allowed: HTTP/1.0 clients may omit it and a browser always
sends it. The port is not compared — it is not a boundary. Anything else is
refused.

On a non-loopback bind the check is skipped. The set of names a LAN address is
reachable by is not knowable from the config, and a key is mandatory there
anyway; a wildcard bind is served only through what the caller knows it as.
This asymmetry is documented in the module and in the docs.

**Status and body.** `421 Misdirected Request` — the exact semantic — with a
body that explains the problem and the fix, never echoing the received
authority:

```json
{"error":{"message":"the request was addressed to a host this server does not serve; use http://127.0.0.1:8484, or forward `Host: 127.0.0.1:8484` if a proxy is in front","type":"invalid_request_error","code":"invalid_host"}}
```

`421 => "Misdirected Request"` is added to `reason_phrase`.

**Origin: dropped.** The review proposed refusing any `Origin` while unkeyed.
With 5.1 there is no unkeyed mode, so the rule would be dead code; and a key
is not ambient (it is not a cookie, so a browser cannot attach it on its own),
which is exactly the case where the MCP guidance allows any `Origin`. A
browser UI holding a key keeps working.

**Considered and rejected.** Checking `Content-Type` on the POST routes would
close the simple-request path independently, but it breaks clients that omit
the header (`curl -d` defaults to `application/x-www-form-urlencoded`) for no
coverage once a key is required. A Unix socket is stronger still and a
direction for later, but it changes every client's base URL.

### 5.3 F-2 and F-3 — decide authentication before reading the body

Both defects are the same ordering in `src/server/http.rs`, so they ship
together.

**Refuse before the body.** `read_request` gains a gate callback evaluated on
the parsed head, before the body is read:

```rust
fn read_request(
    reader: &mut SocketReader,
    writer: &mut TcpStream,
    gate: &dyn Fn(&Request) -> Option<Response>,
) -> std::io::Result<Option<Request>>
```

The head is built into a `Request` with an empty body, the gate runs, and on
`Some(response)` the server writes it with `Connection: close`, returns
`Ok(None)`, and never allocates the body. It does not answer `100 Continue`,
so a client using `Expect: 100-continue` gets the refusal without sending the
body at all.

After writing a refusal for a client that is *not* using `Expect`, the server
drains the body into a fixed 8 KB buffer before closing. Closing on a half-read
body sends a TCP reset that can discard the response before the client reads
it; draining costs no memory and keeps the 401 legible. It is bounded by
`MAX_BODY_BYTES` and the existing idle timeout.

The handler still runs its own check afterwards — the gate is an early
refusal, not the authority — so a bug in the gate cannot open the door. Both
call one function:

```rust
fn pre_authorize(request: &Request, context: &ServerContext) -> Option<Response> {
    // guard → health allow → key required → is_authorized
}
```

**Bound the chunk size.** Replace the addition with a subtraction that cannot
underflow, and answer `413`:

```rust
if size > MAX_BODY_BYTES || body.len() > MAX_BODY_BYTES - size { … }
```

`read_body` learns to collect or discard (`keep: bool`), which is what makes
the drain path allocate only its 8 KB buffer. Oversize bodies answer `413`
with the OpenAI error shape instead of closing the connection silently.

**Body budget: not needed.** Once unauthenticated requests never reach
`read_body`, the 8 GB worst case requires a valid key — the user's own agent.
`MAX_BODY_BYTES` stays at 32 MB and the module comment says why.

### 5.4 F-4 — force the config to `0600`

`write_file` (`src/config.rs:1121`) writes to a temporary and renames, then
applies `restrict_permissions(path, 0o600)` on unix — the same shape `keys.rs`
and `store.rs` already use. `migrate.rs` applies `0600` after copying the
legacy `config.toml`.

For existing installs, `Paths::ensure_dir` (`src/paths.rs:142`) also tightens
the default `config.toml` when it is present. `ensure_dir` already runs on
every command (`src/cli/mod.rs:136`), and it already forces the directory to
`0700`; this is the same move one level down. A `--config` path outside the
directory is left alone — the user chose it.

**Trade-off.** A `0600` config is not readable by a group or a shared service
account. It may contain a credential, so that is the correct default; anyone
who needs otherwise can copy the value out.

### 5.5 F-5 — make the fail-open visible

`KeyStore::refresh` logs one warning when a re-read fails, including the
underlying error (path and parse message only):

```
the key store at <path> could not be read; still serving the keys loaded at startup: <reason>
```

The stamp is already updated only when the file changes, so this is one line
per edit, not one per request. Keep the fail-open behaviour — failing closed
on a transient read error would lock a running daemon out of its own keys —
but pin it with a test so it is a decision rather than an accident.

### 5.6 N-1 — cover `key=` in the redactor

Add `key` to `CREDENTIAL_KEYS` and require a parameter boundary before the
name: start of string, whitespace, `?`, `&`, `,`, or a quote. `?key=…`
redacts; `monkey=…` does not. Two tests.

### 5.7 N-2 — make the import respect the relocation

`grok_cli_auth_file()` returns `None` when `EGGPASS_HOME` is set. A relocated
installation is an isolated one; that is what the variable is for. One test
alongside the existing `paths` tests, and the doc comment becomes true.

---

## 6. Phases

Each phase is independently revertable and ships with its own tests and
documentation.

### Phase 1 — a key is required, and the authority is checked (F-1)

- `run`/`restart`/`croncheck` precondition; `is_authorized` refuses when
  unconfigured; the `no_api_key` 401; `ServerContext.port` for the message.
- `src/server/guard.rs` (new), `pre_authorize`, `421` in `reason_phrase`.
- Tests: every command refuses with the message; a running server with its
  last key revoked answers 401 to everything but `/health`; the authority
  allow-list (allowed, lookalikes, missing, non-loopback skip).
- Docs: README quick start and safety paragraph, `how-it-works`,
  `client-setup`, `troubleshooting`, `commands`, `configuration`.

**Acceptance.** `eggpass run` without a key exits 1 and names `key add`;
`Host: evil.example` against a keyed instance → `421` with the fix hint; the
live mcode path through luna is unaffected.

### Phase 2 — the HTTP layer (F-2, F-3)

- `read_request` gate, `pre_authorize` extraction, collect/discard body,
  checked arithmetic, `413`.
- Tests: gate refusal before the body (`Expect: 100-continue` gets 401, never
  100); `ffffffffffffffff` → 413 and the server still answers; a normal body
  still round-trips.

**Acceptance.** The exact repro from §3 F-2 gets a `413`, the process is still
alive, and the suite passes.

### Phase 3 — file modes and observability (F-4, F-5)

- `write_file`, `ensure_dir`, `migrate.rs`, `refresh` warning.
- Tests: config written in a temp dir is `0600`; a corrupt store keeps the
  previously loaded keys and still verifies them.

### Phase 4 — the two notes (N-1, N-2)

- Redactor key boundary, `EGGPASS_HOME` import suppression, tests.

### Phase 5 — close-out

- `CHANGELOG.md` under `[Unreleased]`: a `### Security` entry for F-1 and
  `### Fixed` entries for F-2/F-3, F-4/F-5.
- Full gate: `cargo fmt --check`, `clippy -D warnings`, tests on **1.89** and
  **1.100.0**, `cargo package --list`, markdown link and anchor check.
- Reinstall and restart the live daemon; drive `gpt-6-luna` through mcode with
  the user's own config to confirm the guard and the key requirement do not
  disturb the working path.
- Update this document's status and the index in `docs/plans/README.md`.

---

## 7. Test plan

| Test | Asserts |
| --- | --- |
| `running_without_a_key_is_refused` | `run` errors, nothing binds, the message names `key add` |
| `croncheck_without_a_key_is_refused` | same, and it is not silent |
| `stop_works_without_a_key` | stopping never needs a credential |
| `an_unconfigured_server_refuses_everything` | 401 with `no_api_key`, `/health` still 200 |
| `revoking_the_last_key_closes_the_endpoint` | the running server refuses without a restart |
| `a_foreign_host_is_refused` | `421`, the body names the expected URL, no echo |
| `the_loopback_authorities_are_accepted` | the four shapes, `.localhost`, trailing dot |
| `a_lookalike_authority_is_refused` | `localhost.attacker.example`, `127.0.0.1.evil.example` |
| `a_missing_host_is_allowed` | HTTP/1.0 survives |
| `a_non_loopback_bind_skips_the_guard` | LAN access unchanged |
| `the_gate_runs_before_the_body` | `Expect: 100-continue` gets `401`, never `100` |
| `an_oversized_chunk_is_refused` | `413`, and the next request is answered |
| `a_corrupt_store_keeps_serving_what_it_had` | fail-open pinned |
| `the_config_file_is_not_world_readable` | `0600` |
| `a_redacted_query_key_is_scrubbed` | `?key=…` gone, `monkey=` intact |
| `eggpass_home_skips_the_grok_import` | `None` |

The raw-socket tests belong in `src/server/http.rs`'s `integration_tests`
module, which already starts a real server; everything else is a unit test
next to the function under test.

---

## 8. Documentation changes

| File | Change |
| --- | --- |
| `README.md` | `eggpass key add` becomes the third quick-start command; replace the "safe for a loopback socket" claim; keep loopback as the happy path and point at SSH forwarding |
| `docs/how-it-works.md` | The key requirement and the authority guard, and why loopback is not a browser boundary |
| `docs/client-setup.md` | The client must send a key now; the `api_key` vs issued-key choice |
| `docs/configuration.md` | `config.toml` is `0600`; `api_key` is the recoverable single-key option, issued keys are per-client |
| `docs/troubleshooting.md` | "refusing to run: no client key"; "421 Misdirected Request"; "a website is using my account" |
| `docs/commands.md` | `run`/`croncheck` fail without a key; `key add` in the setup flow; the 421 behaviour |
| `CHANGELOG.md` | `[Unreleased]` — `### Security` and `### Fixed` |

---

## 9. Compatibility

Five deliberate changes, all documented:

1. **`eggpass run`, `restart` and `croncheck` fail without a key.** The
   headline change. Existing installs must run `eggpass key add` once, or set
   `api_key`. `restart` checks before it stops anything, so a missing key
   never leaves the endpoint down.
2. **A custom `Host` on a loopback bind is refused.** The realistic casualty
   is a local reverse proxy that passes the original `Host` through; the fix
   is `proxy_set_header Host 127.0.0.1:8484`.
3. **`config.toml` becomes `0600`.** Tightened on the next command, not by a
   migration step in the binary.
4. **A malformed chunked body gets `413` instead of killing the daemon.**
5. **A missing `Host` or a `*.localhost` name is accepted**, so nothing that
   works today on loopback stops working.

Loopback stays the happy path. Binding elsewhere still works — `eggpass host`
is unchanged — but the docs say plainly that loopback plus an SSH tunnel is
the intended shape and that a widened bind is a deliberate, riskier choice.

---

## 10. Verification

Automatic:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo +1.100.0 clippy --all-targets -- -D warnings
cargo +1.100.0 test --all-targets
cargo package --list --allow-dirty
```

Manual, on an isolated `EGGPASS_HOME` (port 8487):

```sh
eggpass run --foreground                     # without a key: exit 1, names `key add`
eggpass croncheck                            # same
eggpass key add review                       # prints a key
eggpass run --foreground &                   # starts
curl -s -o /dev/null -w '%{http_code}\n' -H 'Host: evil.example:8487' \
     -H 'Authorization: Bearer <key>' http://127.0.0.1:8487/v1/models   # 421
curl -s -o /dev/null -w '%{http_code}\n' -H 'Host: localhost:8487' \
     -H 'Authorization: Bearer <key>' http://127.0.0.1:8487/v1/models   # 200
```

and the F-2 repro from the appendix, which must answer `413` and leave the
server answering `/health`.

Live:

```sh
cargo install --path . --force
~/.cargo/bin/eggpass restart
mcode exec --model custom_provider:eggpass/gpt-6-luna --permission off \
  --output-format stream-json "run printf ok and report the output"
```

The last one is the regression check that matters: it exercises the keyed
path, the guard, and the tool-call round trip together.

---

## 11. Decisions taken

| Question | Decision |
| --- | --- |
| Status code for a foreign `Host` | `421`, with a body that explains the fix and names the expected URL |
| Is a key mandatory? | Yes — `run`, `restart` and `croncheck` refuse without one, and the running server refuses everything if its keys go away |
| `api_key` vs issued keys | Keep both. `api_key` is the recoverable single-key option; issued keys are per-client, revocable by number, and hashed at rest |
| Guard non-loopback binds | No. Loopback plus SSH forwarding is the happy path; a widened bind keeps working and stays key-protected |
| `*.localhost` | Accepted — browsers force it to loopback, so the suffix rule is safe |
| Missing `Host` | Accepted — HTTP/1.0 compatibility, and no browser omits it |
| `Origin` check | Dropped. With a mandatory key there is no open mode to protect, and the key is not ambient |
| Auto-mint a key on first run | No — it would land in a daemon's log or a cron job, unrecoverable |

---

## Appendix — reproduction notes

All of this was run against `4f1f381`, before the fixes.

**F-1, unkeyed instance.** `EGGPASS_HOME` with `port = 8487`,
`api_key = ""`, then `~/.cargo/bin/eggpass run --foreground`.
`GET /v1/models` with `Host: evil.example:8487` →
`200 {"data":[],"object":"list"}`. The `text/plain` POST reached the provider
client and returned `x.ai: the stored credentials could not be refreshed …`,
not the gate's `missing or invalid API key`.

**F-2, the abort.**

```
POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:8487\r\nTransfer-Encoding: chunked\r\n\r\n
1\r\nA\r\n
ffffffffffffffff\r\n
```

→ `panicked at src/server/http.rs:536:25: capacity overflow`, then the process
is gone.

**F-1, the negative controls on the live keyed daemon.** No key → `401`;
`Authorization: Bearer sk-eggpass-wrong` → `401`; `?key=wrong` → `401`;
`OPTIONS` → `401` with no CORS headers. The keyed posture held.

**F-4, the mode.** `stat -f "%Sp"` on the config directory, the config file,
`keys.json`, `eggpass.log`, and `credentials/*.json`. Only the config and the
log are `0644`; both sit in a `0700` directory.

**Cleanup.** Both review instances and the temporary `EGGPASS_HOME`
directories were removed; the user's config, keys, credentials and
`~/.grok/auth.json` were not modified.
