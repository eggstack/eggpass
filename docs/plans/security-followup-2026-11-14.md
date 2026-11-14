# Security follow-up — residual auth defects, 2026-11-14

The work order for the second pass over the authentication surface, reviewed
against `b4a55ff` (`security: require a key and check the request authority`).
The first pass is [implemented](security-pass-2026-11-14.md). This one is
implemented. Nothing was published, and a live daemon was not restarted.

| | |
| --- | --- |
| Scope | The client-key gate, both OAuth flows, credential storage, and every path a secret crosses |
| Out of scope | What an agent does once it has a key; hardening a non-loopback bind for the public internet |
| Trigger | Second review of the 2026-11-14 pass, against the implementation rather than the plan |
| Verdict | The key gate holds. Four defects the pass missed or described as fixed. No auth bypass, no credential theft |
| Release posture | Unchanged — manual review only, nothing published |
| Status | **Implemented** |

---

## 1. Summary

The mandatory key holds. A missing key, a wrong bearer, and a wrong `?key=`
still stop in `pre_authorize`, on every host. The `ffffffffffffffff` chunk-size
abort is fixed: the size is rejected before it is used, and the server answers
`413`. Those are not reopened here.

What the pass got wrong is two sentences in its own corrective design
([§5.3](security-pass-2026-11-14.md#53-f-2-and-f-3--decide-authentication-before-reading-the-body)),
which the changelog now repeats:

- The refusal drain is "a fixed 8 KB buffer bounded by `MAX_BODY_BYTES`".
  The content-length drain is. The chunked drain is not. `read_line` retains
  a chunk-size, trailer, or chunk-ending line with no cap, and it runs before
  the size check. A client that keeps sending and never sends `\n` grows that
  line until allocation fails. `panic = "abort"` takes the process with it.
  The idle timeout does not save this: it fires only when a read blocks.

- "Once unauthenticated requests never reach `read_body`, the 8 GB worst case
  requires a valid key." Health is unauthenticated on purpose, and the gate
  lets it through *before* the body is read. `GET /health` and `GET /healthz`
  with `Content-Length: 33554432` still allocate. That is the F-3 case the
  pass said now requires a key.

Two further defects sit in the login flows, not in the proxy gate. A local
process can cancel an in-progress `eggpass login` by hitting the ephemeral
callback listener; the listener treats the first bad query as the provider.
And the device-flow URL check does not agree with the browser it hands the
URL to: `https://evil.example\@accounts.x.ai/...` is accepted, and a WHATWG
parser (the `url` crate, and the system parser `webbrowser` ends up in) takes
the host to be `evil.example`.

None of the four is an auth bypass. A forged callback code is still rejected
at the token exchange. A hostile device URL does not yield a client key. The
fixes below stay inside the decisions the first pass already settled.

---

## 2. What held

Reviewed against `b4a55ff` and not reopened:

- `run`, `restart`, and `croncheck` refuse to start without a key. `stop`
  still works without one. No auto-mint.
- `is_authorized` refuses when nothing is configured, on every host. The
  loopback exception is gone.
- The authority guard runs first, including on `/health`. A foreign `Host` on
  a loopback bind is `421`. A missing `Host` is allowed. `*.localhost` is
  allowed. A non-loopback bind skips the guard. `eggpass host 0.0.0.0` stays
  possible. There is no `Origin` check.
- Both `api_key` and issued keys remain. Issued keys are salted hashes at
  `0600`. A corrupt key store fails open with a warning.
- `ffffffffffffffff` is refused with `413`, and the next request is answered
  (`an_oversized_chunk_is_refused_and_the_server_survives`).
- The callback listener's own `read_request` stops at 16 KiB
  (`src/auth/flow.rs:534`). That reader is not the unbounded line.
- PKCE is S256. A code that was not minted for this verifier fails at the
  token endpoint. Cancelling a login is not account takeover.

`login_loopback` is always called with `existing: None`
(`src/cli/commands/login.rs:142`). The `id_token_hint` branch in
`login_loopback` (`src/auth/flow.rs:148-155`) is dead from the CLI. Do not
wire the stored credential into that call as part of this work. That would
put an `id_token` on the authorize URL, in the line printed to the terminal,
and in the browser. Leave the parameter.

---

## 3. Findings

Each finding is a concrete trigger against `b4a55ff`. None of them was
reproduced by crashing a live daemon. The abort in particular must not be
aimed at a running process; the path is the evidence.

### F-1 — Low — a chunked line with no newline still aborts the daemon

**Trigger.** One TCP connection, `Transfer-Encoding: chunked`, then a
chunk-size line, a trailer line, or the line that should have been the
chunk's closing CRLF, with no `\n`, kept open and kept fed. Two ways in,
neither of which needs a valid key:

- `GET /health` or `GET /healthz`, with an allowed or missing `Host` (the
  guard runs first; a foreign `Host` on loopback is already `421`). The gate
  returns `None` (`src/server/mod.rs:208-212`), so `read_body` is called with
  `keep = true`.
- Any request the gate refuses — wrong key, no key on a non-health route,
  foreign `Host`. After the refusal is written, the drain calls
  `read_body(..., keep = false)` (`src/server/http.rs:422-426`). That path
  still enters `read_chunked_body`.

**Impact.** `read_line` (`src/server/http.rs:338-363`) copies every byte into
a `Vec` until it sees `\n`. `read_chunked_body` calls it for the size line
*before* the size check (`src/server/http.rs:595`, check at `:614`), again
for each trailer (`:602`), and again for the chunk ending (`:627`). The
socket buffer itself stays about one 8 KiB read ahead; the unbounded
allocation is the line. A client that never blocks never trips the idle
timeout (`DEFAULT_IDLE_TIMEOUT` is 120s, set at `:276`, and it fires only
when `read` returns a timeout). Allocation failure aborts the process
(`Cargo.toml` release profile, `panic = "abort"`). Nothing restarts it unless
`croncheck` is on a timer.

This is the same impact as the pass's F-2, through a different input. The
`ffffffffffffffff` fix does not cover it. The pass's claim that an
unauthenticated client never reaches `read_body` is false for both paths
above.

**Evidence.** Code path only. `read_head` is capped at 64 KiB
(`MAX_HEAD_BYTES`). `read_line` has no equivalent check, including for a
remainder that arrived in the same reads as the head — `read_head` can leave
a pending slice already larger than 8 KiB, and `read_line` copies it before
the next `fill`.

### F-2 — Low — `/health` still collects a body, with no key

**Trigger.** Up to `DEFAULT_MAX_CONNECTIONS` (256) requests to `/health` or
`/healthz`, each with `Content-Length: 33554432` and an allowed or missing
`Host`. On a non-loopback bind the guard is skipped, so any `Host` works;
that does not change the finding, and this pass does not start hardening
that bind.

**Impact.** `33554432` is `MAX_BODY_BYTES`, and the check is `length > MAX_BODY_BYTES`
(`src/server/http.rs:564`), so the value is accepted. `read_body` then
allocates `vec![0u8; length]` (`:573`) because health was allowed with
`keep = true`. 256 × 32 MiB is 8 GiB, with no key. The handler throws the
body away (`health::handle` does not read it). The content-length *refusal*
drain is bounded — `keep = false` discards into an 8 KiB buffer, and an
over-32 MiB length returns `TooLarge` before that. The health allow path is
the one that retains the body. This is the pass's F-3, still open.

**Evidence.** Code path only. The probes that actually matter send no body:
`daemon::health_ok` is a `GET /health` (`src/daemon.rs:174-185`), and
`status` and `croncheck` use it. A fix that discards the body does not change
them.

### F-3 — Low — the first bad callback cancels the login

**Trigger.** While `eggpass login` is waiting, any local process connects to
the ephemeral `127.0.0.1:0` listener and requests the callback path
(`/callback` for x.ai, `/auth/callback` for ChatGPT) with an `error`
parameter, a wrong or missing `state`, or no `code`. The wait is
`CALLBACK_TIMEOUT_SECS` (600). The port is random; the window is long enough
to scan loopback.

**Impact.** `wait_for_callback` (`src/auth/flow.rs:412-444`) returns on that
first request. The login ends. The user has to start again. A forged `code`
is not accepted: the token exchange still requires the PKCE verifier, which
never leaves the process. This is not account takeover. It is a local process
— including one running as another user on a shared host — deciding that the
login failed. The same correction the first pass made about loopback applies
here: the listener is not "this user".

No `state` is required to cause it. `state` is 16 random bytes, base64url,
22 characters (`STATE_BYTES` in `src/auth/pkce.rs`). A caller who does not
have it should not be able to end the wait.

**Evidence.** The three `return`s. A wrong path already continues (`404`,
then `continue`). The pasted-code path does not go through the listener and
is unchanged; it exists only when the provider does not issue a client id
(x.ai). OpenAI has no paste path.

### F-4 — Low — the device-URL check does not match the browser

**Trigger.** A device-authorization response whose `verification_uri` or
`verification_uri_complete` is `https://evil.example\@accounts.x.ai/...`
(also `\@auth.x.ai`, `\@x.ai`). x.ai only. OpenAI has no device flow
(`supports_device_flow: false`, `trusted_device_hosts: &[]`).

**Impact.** `is_trusted_verification_url` (`src/auth/flow.rs:640-667`) splits
on `/`, `?`, and `#`, then takes the host after the last `@`. The backslash
is not a delimiter, so the host it keeps is `accounts.x.ai`, which is on the
list. `login_device` then prints `verification_uri` and opens
`verification_uri_complete` if present, otherwise the plain URI
(`src/auth/flow.rs:228-247`). A WHATWG parser treats `\` as `/` in an `https`
URL, so the host the browser navigates to is `evil.example`. Confirmed with
`url` 2.5.8, the copy already in `Cargo.lock`:

| URL | Host `url` produces |
| --- | --- |
| `https://evil.example\@accounts.x.ai/oauth2/device` | `evil.example` |
| `https://evil.example\@auth.x.ai/...` | `evil.example` |
| `https://evil.example\@x.ai/...` | `evil.example` |
| `https://accounts.x.ai\@evil.example/...` | `accounts.x.ai` |
| `https://evil.example%5C@accounts.x.ai/device` | `accounts.x.ai`, username `evil.example%5C` |

The current function accepts the first three and rejects the fourth. The
browser does the opposite with the first three. `webbrowser` 1.2.7 on macOS
uses `NSWorkspace`, not an argv, so the string is not a shell injection; it
is still handed to the system URL parser, which is why the host we check has
to be the host that parser produces.

This is not a client-key bypass. The trigger is a device-authorization
response we are willing to believe — the provider, or whoever can speak for
it. The check exists specifically so that response cannot send the user
somewhere else, and it does not work.

The error path echoes the raw URL (`FlowError::UntrustedUrl`'s `Display`).
A hostile response can also fill the terminal. The fix stops echoing it.

**Evidence.** The function, the `url` 2.5.8 parse above, and the existing
tests in `src/auth/flow.rs`, which never pass a backslash. Trusted hosts are
`accounts.x.ai`, `auth.x.ai`, and `x.ai` (`src/provider/xai.rs:98`). Exact
match already rejects `accounts.x.ai.evil.example`. That part is fine.

### N-1 — Note — the constant-time claim is overstated

Not a defect to fix here.

`constant_time_eq` returns immediately when the lengths differ
(`src/keys.rs:449-452`). `is_authorized` returns on the first `api_key` match
(`src/server/mod.rs:279-287`). Issued keys are hashed first, and `verify`
compares every stored hash without an early return (`src/keys.rs:240-248`).
The pass's sentence that every comparison runs constant-time across the whole
store is stronger than the code. There is no practical loopback timing
exploit in that gap: the early return is a length check on the configured
`api_key`, and the issued-key path already walks the store. Do not pad
comparisons in this pass. Do not describe the code as stronger than it is.

---

## 4. Corrective design

### 4.1 F-1 — cap every chunked line before retaining it

`read_line` gains a byte cap, applied before any copy onto the line and
before the `String` conversion. A pending slice that already has no newline
and would cross the cap is rejected without being copied. That covers a
remainder left behind by `read_head`, not only bytes from a later `fill`.

```rust
const MAX_LINE_BYTES: usize = 8 * 1024;
```

8 KiB is the drain buffer the pass already chose, and it is far above a
legitimate chunk-size line. `ffffffffffffffff` is 16 hex digits. A chunk
extension or a trailer longer than the cap is refused. No client we talk to
sends one. The cap is the bound. The idle timeout stays what it is — a
backstop for a stalled read — and the comment on `read_line` says so.

An overlong line is malformed framing, not a body over 32 MiB, so it is not
`413`.

- On the collect path, write `400` with a fixed OpenAI-shaped message
  (`code: "malformed_body"`), close, and do not run the handler. The process
  must still answer the next connection.
- On the drain path the refusal is already written, and `read_body`'s error
  is already ignored. The cap only has to return. It must not panic, and it
  must not allocate the line it is refusing.

The `ffffffffffffffff` check stays. Its test stays. This is a different
input, and the new test must not send more than the cap plus a small
overshoot. Do not aim an unbounded line at a live daemon.

### 4.2 F-2 — do not retain a body for health

Health stays unauthenticated. `status` and `croncheck` need it, and it still
carries only the version string. What changes is that the gate's "allow" is
no longer also "allocate the body".

`Option<Response>` cannot say that, so the gate result becomes a decision:

```rust
enum GateDecision {
    /// The handler may run. `keep_body` is false for a probe that does not
    /// read the body; the bytes are discarded in the fixed drain buffer.
    Proceed { keep_body: bool },
    /// Write this and do not run the handler.
    Refuse(Response),
}
```

`/health` and `/healthz` — the route, not a string compare copied into
`http.rs` — proceed with `keep_body: false`. Every other allow proceeds with
`keep_body: true`. A refusal is unchanged, including the drain.

`read_request` then calls `read_body(..., keep_body)`. The handler sees an
empty body. `health::handle` already ignores the request, so the JSON is
unchanged. A `GET` with no body, which is what every probe sends, is
unchanged.

A content-length above 32 MiB still returns `TooLarge` before the discard.
A content-length of exactly 32 MiB is discarded in the 8 KiB buffer, not
allocated. Combined with 4.1, a chunked health request cannot grow a line
without bound.

**No new body budget.** 256 connections each discarding 32 MiB still occupy
threads and bandwidth. That is the connection cap doing its job, the same as
the refusal drain. Do not lower `MAX_BODY_BYTES` or `DEFAULT_MAX_CONNECTIONS`
to paper over this. A keyed client can still allocate 32 MiB times the
connection cap. That is the user's own agent, which the pass already accepted
and which this pass leaves alone.

The handler's second `pre_authorize` stays. The discard flag is not an auth
decision. A bug in the flag cannot skip the key check.

### 4.3 F-3 — keep waiting unless `state` matches

A connection is this login's result only when its `state` is the one we
issued. Otherwise answer `400` with a fixed page, and continue the loop until
the deadline, a pasted code, or a callback that does carry the state.

The page does not echo the query, and it does not contain the expected
state. A scanner learns nothing from the body.

| Callback | Result |
| --- | --- |
| Wrong path | `404`, continue (already the case) |
| No `state`, wrong `state`, `error` without our `state`, missing `code` without our `state` | `400`, continue |
| Our `state`, and `error` | `FlowError::Provider`. A real denial carries the state. Do not ignore it |
| Our `state`, and no `code` | `FlowError::NoCode`. Only the holder of the state can cause this |
| Our `state`, and a `code` | Success, as today |

`StateMismatch` is no longer returned for a foreign callback. Keep the
variant. Nothing has to construct it, and removing it churns the error type
for no caller.

Do not shorten the 600s timeout as the fix. The state is the boundary. Do not
add a second secret to the callback path. The state already is that secret.
Do not bind the listener anywhere but `127.0.0.1`.

The pasted-code path is unchanged.

### 4.4 F-4 — check the host the browser will open

Add `url` as a direct dependency, pinned to `2.5.8`, the version already in
`Cargo.lock`. Its MSRV is 1.63, under ours. It is the WHATWG parser. Do not
hand-roll a second one, and do not bump it. A denylist of `\` alone closes
the known bypass and misses the next discrepancy; that is how the hand parser
got here. The parser is the check. A raw-character refuse is the belt, not
the check.

`is_trusted_verification_url`:

1. An empty trusted-host list refuses immediately. OpenAI stays refused
   without a parse.
2. Refuse the raw string if it contains `\`, ASCII whitespace, or a byte
   below `0x20`. x.ai does not send these. This also refuses
   `https://accounts.x.ai\@evil.example/...`, which the WHATWG parser would
   accept (host `accounts.x.ai`). Fail closed.
3. `url::Url::parse`. Failure refuses.
4. Scheme must be `https`.
5. No userinfo. `username` empty and no password. Legitimate device URLs
   have neither. This refuses `https://evil.example%5C@accounts.x.ai/device`
   even though the browser would land on `accounts.x.ai`.
6. Host is `host_str()` with one trailing dot removed, compared exactly,
   ASCII case-insensitive, to the trusted list. The crate already lowercases
   the host. No suffix match. `accounts.x.ai.evil.example` stays refused.
   `accounts.x.ai.` is accepted, matching the one-dot rule the authority
   guard already uses. Ports are not compared; `host_str` does not include
   one.

Both `verification_uri` and `verification_uri_complete` go through this, as
they do today. The print and `webbrowser::open` stay after both checks. A
URL that fails is neither printed nor opened.

`FlowError::UntrustedUrl` stops carrying the raw string. The message names
the parsed host when there is one (`refusing to open a verification page on
evil.example`), and otherwise says the URL could not be parsed. It does not
interpolate the response.

### 4.5 Considered and rejected

- **Authenticate `/health`.** Breaks `status` and `croncheck`, which the pass
  left open on purpose. Discard the body instead.
- **Lower the body cap or the connection cap.** Punishes the keyed agent for
  a hole that is the health allow path.
- **Treat the idle timeout as the line bound.** A client that keeps sending
  never blocks. The timeout never fires.
- **Restart on abort.** croncheck already restarts a dead daemon when the
  user asked it to. The cap is the fix. Do not add a supervisor.
- **Shorten the callback timeout, or add a second callback secret.** The
  state is already unguessable across the window. The bug is that we do not
  require it.
- **Wire `id_token_hint` from the stored credential.** Puts a credential on
  a printed URL. Out of scope. See §2.
- **Pad `constant_time_eq`.** No practical exploit. Not this pass. See N-1.
- **Refuse `\` and skip the parser.** Closes today's bypass only.

---

## 5. Phases

Each phase is independently revertable and ships with its own tests. No
phase publishes, and no phase restarts the user's daemon.

### Phase 1 — the HTTP layer (F-1, F-2)

- `MAX_LINE_BYTES`, the capped `read_line`, `400` on the collect path.
- `GateDecision`, health proceeds with `keep_body: false`.
- Tests in `src/server/http.rs`'s integration module, beside the existing
  gate tests. The oversize-chunk test stays green.

**Acceptance.** A chunk-size line of `MAX_LINE_BYTES + 1` with no newline
does not abort the process; the next `/health` is `200`. A refused request
with the same line still returns its refusal. A `/health` request that sent
a small body is answered `200` with the usual JSON, and the handler saw an
empty body.

### Phase 2 — the callback (F-3)

- `wait_for_callback` ignores a callback that does not carry this login's
  state.
- Tests next to the pasted-code tests in `src/auth/flow.rs`. They can call
  `wait_for_callback`; the test module is inside the parent.

**Acceptance.** `error=access_denied` with no state, a wrong state, and a
missing code do not finish the wait. A later callback with the issued state
and a code does. A matching state with `error` still returns `Provider`. A
matching state with no code still returns `NoCode`. The `400` page does not
contain the expected state.

### Phase 3 — the device URL (F-4)

- Direct `url = "=2.5.8"` dependency. No other crate. No version bump.
- The predicate, the error text, the tests beside the existing trusted-host
  tests.

**Acceptance.** The three `\@` forms are refused. A raw backslash is refused
even when the WHATWG host is trusted. Userinfo is refused. The existing
accepted URLs still pass, including a trailing dot and a different case.
`http://`, a lookalike host, and an empty trusted list still fail. The error
text does not contain the raw URL.

### Phase 4 — close-out

- `CHANGELOG.md` under `[Unreleased]`. Correct the two overclaims rather than
  adding a second sentence beside them:
  - "A malformed chunked body no longer aborts the daemon" is incomplete. An
    unbounded chunk line still aborts. Say that the line is capped, and that
    `ffffffffffffffff` remains `413`.
  - "The request body is read only after authentication" is incomplete.
    `/health` still collected a body with no key. Say that health stays
    unauthenticated and no longer retains a body.
  - `### Fixed` entries for the callback cancel and the device-URL check.
    The URL one names the backslash discrepancy; it does not need a sample
    that a reader can paste at a login.
- `cargo fmt --all -- --check`, `clippy -D warnings`, tests on **1.89** and
  **1.100.0**.
- Update this document's status and the index row in `docs/plans/README.md`.
- Do not `cargo install`, do not restart the live daemon, and do not publish.
  These fixes do not change the keyed proxy path. A live restart is a
  separate ask.

---

## 6. Test plan

| Test | Asserts |
| --- | --- |
| `an_overlong_chunk_line_does_not_abort` | Cap plus a small overshoot, no newline, on a request the gate allows. `400`, process still answers `/health`. Does not send more than that |
| `a_refused_chunk_line_still_returns_the_refusal` | Gate returns `401`, same overlong line, client reads the `401`, server survives |
| `a_short_chunked_body_still_arrives` | A normal chunked body is not rejected by the cap |
| `a_health_request_does_not_keep_its_body` | Client sent a small body; handler saw `body.is_empty()`; response is the usual `200` |
| `an_oversized_chunk_is_refused_and_the_server_survives` | Unchanged. `ffffffffffffffff` is still `413` |
| `a_foreign_callback_does_not_cancel_the_login` | `error` without state, wrong state, missing code: wait continues. A later good callback finishes it |
| `a_matching_state_still_ends_the_login` | `error` + state → `Provider`; no code + state → `NoCode`; code + state → that code |
| `an_ignored_callback_does_not_echo_the_state` | The `400` page does not contain the issued state |
| `a_backslash_host_is_not_trusted` | The three `\@` forms refused |
| `a_raw_backslash_is_refused` | `https://accounts.x.ai\@evil.example/...` refused |
| `userinfo_is_refused` | `https://user@accounts.x.ai/...` and the `%5C@` form |
| `a_trailing_dot_on_a_trusted_host_is_accepted` | `https://accounts.x.ai./device` accepted |
| `the_untrusted_url_error_does_not_echo_the_url` | Display text has the host or a fixed sentence, not the raw string |

The existing `trusted_hosts_are_accepted`, `other_hosts_are_refused`, and
`a_provider_with_no_trusted_hosts_refuses_everything` tests stay. They are
the regression net for the parser swap.

---

## 7. Documentation

| File | Change |
| --- | --- |
| `CHANGELOG.md` | Correct the two `[Unreleased]` overclaims; add the callback and device-URL fixes. Phase 4, not before |
| `docs/plans/README.md` | The index row for this document flips from "Not implemented" to "Implemented" when phase 4 lands |
| `docs/plans/security-pass-2026-11-14.md` | No rewrite. The pointer at the top is the correction. §5.3 stays as the record of what that pass decided |

No user-facing page currently claims the body is never read before auth,
other than the changelog. Do not add a troubleshooting entry for a bug this
pass removes.

---

## 8. Compatibility

Four deliberate changes, all fail-closed, none of them visible to a client
that already works:

1. **An overlong chunked line is `400`, or a close on the drain path, instead
   of an abort.** `ffffffffffffffff` is still `413`. A normal chunked body
   still round-trips.
2. **A body on `/health` or `/healthz` is discarded, not stored.** Probes
   send no body. The response stays `200` with the same JSON, and it still
   needs no key.
3. **A callback that does not carry this login's state no longer cancels the
   login.** A real provider error still does, because the provider's redirect
   carries the state. The pasted-code path is unchanged.
4. **A device URL is opened only when the WHATWG host is exactly a trusted
   host, the scheme is `https`, there is no userinfo, and the raw string
   contains no backslash, whitespace, or control character.** Ordinary
   `https://accounts.x.ai/...` URLs still open, including a different case
   and one trailing dot. A URL containing `\` is refused even if a WHATWG
   parse would land on a trusted host.

Loopback stays the happy path. `eggpass host` is unchanged. Nothing in this
pass refuses a non-loopback bind, checks `Origin`, mints a key, or drops
`api_key`.

---

## 9. Verification

Automatic, and nothing else:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo +1.100.0 clippy --all-targets -- -D warnings
cargo +1.100.0 test --all-targets
```

Run on 2026-11-14, on 1.89 and 1.100.0. All passed. `cargo install` was not
run, and a live daemon was not restarted.

Do not reproduce F-1 against a running daemon. The integration test sends
the cap plus a small overshoot and then probes `/health`. That is the proof.
Do not open 256 connections of 32 MiB to prove F-2. The test sends a small
body and asserts the handler saw none.

No isolated `eggpass login` against the real providers is required. The
callback tests drive `wait_for_callback` directly. The URL tests drive the
predicate directly. Neither opens a browser.

---

## 10. Decisions

| Question | Decision |
| --- | --- |
| How is the chunk line bounded? | 8 KiB, checked before the copy. `400` on the collect path. The drain just stops |
| Does health require a key? | No. It stops retaining a body |
| A body budget, or a lower connection cap? | No. The connection cap already bounds the discard |
| What ends a login wait? | A callback whose `state` is the one we issued, or a pasted code, or the deadline. Nothing else |
| How is a device URL judged? | `url` 2.5.8's host, exact match, `https`, no userinfo, plus a raw refuse of `\`, whitespace, and controls |
| Echo the rejected URL? | No. Name the host, or say the URL could not be parsed |
| `id_token_hint` from the stored credential? | No. The CLI does not pass one, and this pass did not start |
| Constant-time padding? | No. The overclaim is corrected here; the code is left |
| Implement now? | Done. Implemented against this work order. A live daemon was not restarted, and nothing was published |
