# `eggpass usage` — subscription usage reporting, 2026-11-14

The work order for a read-only command that asks each provider how much of the
user's allowance is used, and prints it as percentage bars, quota windows and
reset times. It was **implemented on 2026-11-14**; §16 records what the
implementation did, and what it deliberately did not do. Nothing was
published, and a live daemon was not restarted.

| | |
| --- | --- |
| Scope | A new `eggpass usage` command: one fresh read per provider, normalised, printed, and done |
| Out of scope | A `GET /v1/usage` endpoint on the proxy; background polling or caching; ChatGPT quota retrieval; the streaming-header fix; anything that changes the auth model |
| Trigger | A feature request: show current subscription utilisation on demand |
| Verdict | Grok is implementable against a verified endpoint. ChatGPT is not, and the honest answer for it is "unavailable" |
| Release posture | Unchanged — manual review only, nothing published |
| Status | **Implemented — 2026-11-14** |

---

## 1. Summary

The request is for an `eggpass usage` command that reads the current quota of
each signed-in subscription and prints it. The shape is right: a one-shot
read, no polling, no daemon involvement.

The two halves of the request are not equally possible, and the difference is
not a matter of effort.

**Grok is doable.** x.ai's CLI proxy exposes the consumer subscription pool at
`GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`, with the same
OAuth token `eggpass` already stores. This repo verified it first-hand on
2026-11-13 — see
[`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md)
§4.1 — and the official client that calls it is **public source**
(`xai-org/grok-build`), so the request and the response shape are both
readable rather than guessed. The plan name ("SuperGrok") comes from a
sibling endpoint, `GET /v1/settings`.

**Codex is not.** The brief is right about the mechanism — the Codex
app-server does expose `account/rateLimits/read` — and right about the
conclusion: *Sign in with ChatGPT* tokens have no documented quota API. The
research strengthens the conclusion. The app-server's own implementation calls
a **private, undocumented ChatGPT consumer backend**,
`GET https://chatgpt.com/backend-api/wham/usage`, which needs a
`chatgpt-account-id` header. `eggpass` does not have one, cannot get one
(the SIWC docs state plainly that *"Email and `sub` are not workspace
identifiers"* — the workspace is bound server-side to the issued client id),
and the SIWC token's scopes are not the ones that path expects. The SIWC
documentation enumerates every endpoint it defines, and none of them returns
quota.

So the ChatGPT side of this feature is **one line of output that says
"unavailable"**, with the error codes OpenAI does document and a link to
OpenAI's own usage page. That is the honest answer, and it is the one the
brief already asks for.

One thing the brief does not raise, and which this plan treats as a first-class
requirement: **the number `eggpass usage` shows for Grok is not the budget
`eggpass` spends.** `eggpass` sends inference to `api.x.ai` by default, and
that path is metered against the **standard API tier**, not against the
SuperGrok consumer weekly pool the billing endpoint reports. A user who runs
`eggpass usage`, sees "40% used", and then watches their API bill is being
misled. The command has to say which pool it is showing, and the caption has
to change with the configured upstream. §[6.1](#61-the-caption-that-has-to-be-there).

---

## 2. What is already known

Three independent sources agree on the Grok endpoint. They are quoted here
because the plan's parsing rules are derived from them, not from the wire.

### 2.1 Grok — the CLI-proxy billing endpoint

**Verified first-hand in this repository, 2026-11-13.** The call, the
request headers, and the response:

```http
GET https://cli-chat-proxy.grok.com/v1/billing?format=credits HTTP/1.1
Host: cli-chat-proxy.grok.com
Authorization: Bearer <ACCESS_TOKEN>
X-XAI-Token-Auth: xai-grok-cli
Accept: application/json
```

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

**Verified from the official client source, 2026-11-14.** `xai-org/grok-build`
is a public repository. The billing call lives in
`crates/codegen/xai-grok-shell/src/extensions/billing.rs`, the proxy base URL
in `crates/codegen/xai-grok-config/src/endpoints.rs`
(`PROD_CLI_CHAT_PROXY_BASE_URL = https://cli-chat-proxy.grok.com/v1`), the
token header in `crates/codegen/xai-grok-login/src/config.rs`
(`token_header = xai-grok-cli`), and the plan display name in
`crates/codegen/xai-grok-config/src/remote_settings.rs`. The official client
also sends `x-userid` (the OIDC `sub`, or the team `principal_id`) and
`x-grok-client-version`.

**Verified by unauthenticated probe, 2026-11-14.** The endpoint answers `401`
with `{"error":"Invalid or expired credentials (auth_kind=none, …)"}` and
checks auth *before* it looks at `format`.

### 2.2 Grok — what the official client sends, and what it does not

| Header | Sent by the official client | Required by `/v1/billing`? |
| --- | --- | --- |
| `Authorization: Bearer <token>` | yes | **yes** |
| `X-XAI-Token-Auth: xai-grok-cli` | yes | **yes** — the proxy's auth middleware names it in its error text |
| `Accept: application/json` | yes | no, but correct |
| `x-userid: <user id>` | yes | no — independent community clients succeed without it |
| `x-grok-client-version: <version>` | yes | no on this route, but the proxy has a client-version gate on its request family, so send it |
| `x-grok-client-identifier` | **no** on the billing call | no — it is injected for proxy-routed *inference*, not for billing |
| `x-authenticateresponse` | **no** on the billing call | no |

The distinction matters: `eggpass`'s own provider spec attaches
`x-grok-client-identifier` to Build-upstream **inference** requests
(`src/provider/xai.rs:72`). The billing request is a different request and
must not inherit that.

### 2.3 Grok — the response fields

Wire names are camelCase. The encoding is proto3 JSON, which **omits
zero-valued scalars** — this is the single most important parsing fact in
this document.

| Field | Type | Encoding | Notes |
| --- | --- | --- | --- |
| `config` | object \| **null** | — | `null` is valid; it means "no config" |
| `config.creditUsagePercent` | f64 | **0–100, the amount *used*** | **Omitted at 0 %.** Absence means *unknown*, not *zero* |
| `config.currentPeriod.type` | string | proto enum name | Observed `USAGE_PERIOD_TYPE_WEEKLY`; the monthly spelling is inferred, not observed |
| `config.currentPeriod.start` / `.end` | string | RFC 3339 with offset | Microsecond precision observed; `Z` and `+00:00` both occur |
| `config.productUsage[]` | array | `{product, usagePercent}` | Product ids vary (`GrokBuild`, `GrokChat`, `GrokImagine`, `PRODUCT_GROK_BUILD`); entries may omit `usagePercent`; the official CLI does not parse this at all |
| `config.prepaidBalance` | object | `{"val": <USD cents>}` | `{}` means **zero** — `val` is omitted when it is 0 |
| `config.onDemandUsed` | object | `{"val": <USD cents>}` | same shape; `{}` means zero |
| `config.onDemandCap` | object | `{"val": <USD cents>}` | same shape |
| `config.isUnifiedBillingUser` | bool | — | An eligibility signal |
| `config.billingPeriodStart` / `.end` | string | RFC 3339 | Duplicates `currentPeriod`; use as the fallback when `currentPeriod` is absent |
| `config.monthlyLimit`, `config.used`, `config.history[]` | — | — | The **legacy** shape, returned when `format=credits` is *not* passed. Deprecated. Not parsed in phase 1 |
| `subscriptionTier` | string | — | Top level. The CLI overwrites this from `RemoteSettings`, so the wire value is not authoritative |

### 2.4 Grok — where the plan name comes from

`creditUsagePercent` says nothing about which plan the account is on. The
display name does, and it comes from a different endpoint:

```http
GET https://cli-chat-proxy.grok.com/v1/settings
```

→ `subscription_tier_display`, one of `"SuperGrok"`, `"SuperGrok Heavy"`,
`"SuperGrok Lite"`, `"X Premium"`, `"X Premium+"`, `"X Basic"`, `"Free"`,
`"API Key"`. `GET /v1/user?include=subscription` returns the lowercase
`subscription_tier` (`"free"`, `"premium"`, `"supergrok"`, …`) and is the
alternative.

This is a **second request**, and it is enrichment, not the feature. If it
fails, the usage still prints; only the plan label is missing.

### 2.5 ChatGPT — what the app-server offers, and why it does not help

`account/rateLimits/read` is real, and the brief describes it accurately.
It was added in `openai/codex` PR #5302 (2025-10-20) and has only grown
since, additively. It is on the **stable** surface — no experimental gate.

The handshake is:

```json
{"method":"initialize","id":0,"params":{"clientInfo":{"name":"eggpass","title":"Eggpass","version":"0.1.0"},"capabilities":{}}}
{"method":"initialized"}
{"method":"account/rateLimits/read","id":1}
```

`initialize` must be first; `initialized` is a notification carrying no
payload; `account/rateLimits/read` takes no required params.

The response shape is right too — `usedPercent` is an **integer 0–100**,
`resetsAt` is **Unix seconds**, and `planType` enumerates
`free, go, plus, pro, prolite, promax, team, business, enterprise, edu, …`.

**And it does not help, for four independent reasons.**

1. **The app-server reads a different credential.** It loads
   `$CODEX_HOME/auth.json` — a *Codex-managed* ChatGPT login. `eggpass` stores
   a *Sign-in-with-ChatGPT* credential. They are different token classes.
2. **The workspace id is missing and unobtainable.** The app-server's backend
   client sends `chatgpt-account-id`, and `account/login/start` with
   `type:"chatgptAuthTokens"` requires `chatgptAccountId` as a non-optional
   string. The SIWC docs state: *"Email and `sub` are not workspace
   identifiers."* The workspace is bound server-side to the issued client id,
   which `eggpass` does have — but there is no documented way to read the
   workspace id *out* of it. `StoredAuth` has no field for it.
3. **The backend is private.** `GET https://chatgpt.com/backend-api/wham/usage`
   appears in no OpenAI documentation; it exists only inside the Codex source.
   Automating an internal consumer backend is the posture the SIWC docs steer
   away from — their guidance is to deep-link `chatgpt.com/settings/usage`.
4. **Refresh would break.** Codex refreshes with *its own* client id. A
   foreign SIWC refresh token does not refresh there.

The SIWC documentation enumerates every endpoint it defines. There is no
quota endpoint. The only quota signals it publishes are error codes, and it
says what to do with them:

| Code | Status | Meaning |
| --- | --- | --- |
| `subscription_sharing_usage_limit_exceeded` | 429 | The shared allowance is exhausted |
| `subscription_sharing_usage_unavailable` | 503 | The allowance could not be read |
| `subscription_sharing_user_not_eligible` | 403 | This account has no shared allowance |

> *"Do not assume the entire plan is empty or infer a reset time from this code
> alone."* — SIWC documentation

That instruction is the design rule for the ChatGPT half of this feature.

---

## 3. Corrections to the brief

The brief is right about the shape of the feature and about the conclusion for
ChatGPT. Six details in it are wrong. Three of them — the balance shape, the
zero-versus-missing rule, and the two Codex shapes — would have produced a
**wrong number** rather than a missing one, which is the worse failure: the
user is told something false instead of being told nothing.

| # | The brief says | Actually | Consequence |
| --- | --- | --- | --- |
| C-1 | `currentPeriod.type` is "Weekly or monthly" | The proto enum name: `USAGE_PERIOD_TYPE_WEEKLY` | Matching on `"weekly"` would never match. Match on the **suffix**, case-insensitively |
| C-2 | `config.prepaidBalance` is a balance | `{"val": <USD cents>}`, and `{}` means zero | Printing `{"val":0}` or treating the object as a number is wrong. Absent `val` means **zero**, not unknown |
| C-3 | `config.onDemandUsed` is optional purchased-credit information | It is `{"val": <USD cents>}` and is present on a real account | Parse it, as cents |
| C-4 | Missing fields must display `unavailable`, never an inferred zero | Correct — and the mechanism is proto3 omission | `creditUsagePercent` **is omitted at 0 %**. A naive parser that defaults a missing number to `0.0` would report "0% used" for an unknown value |
| C-5 | The Grok plan name is "SuperGrok" | It is not in the billing payload at all | It comes from `GET /v1/settings` → `subscription_tier_display`. Until that call is made, the plan name is unavailable |
| C-6 | The Codex response has `primary.usedPercent`, `secondary.usedPercent`, `windowDurationMins`, `resetsAt` | Correct for the **app-server** shape — but it is snake_case (`used_percent`, `limit_window_seconds`, `reset_at`) in the WHAM HTTP payload the app-server calls | Two different shapes with the same meaning. Do not mix them. This plan implements neither |

Two further deviations from the brief's illustration, both deliberate:

- **Provider names.** The illustration says "Grok" and "Codex". `eggpass`
  already calls these providers "x.ai" and "ChatGPT" in `status`, `login` and
  `logout`, and it does not use Codex at all. `usage` uses the same names, so
  one provider has one name across the whole tool.
- **The "Codex · ChatGPT Plus" block is unreachable.** It depends on the
  app-server path that §2.5 rules out. The illustration is aspirational.

---

## 4. The command

### 4.1 Surface

```
eggpass usage
eggpass usage --provider xai
eggpass --provider openai usage
eggpass usage --json
```

- **`--provider <id>`** is already a global flag (`src/cli/args.rs:165`), and
  today only `login` and `logout` consume it. `usage` accepts it the same way:
  before the subcommand, and after it. With it, only that provider is asked;
  without it, every provider that has a credential is asked, in the fixed
  order `ProviderId::all()` gives.
- **`--json`** prints the normalised report as JSON instead of the bars. The
  model in §[5.2](#52-the-normalised-model) is `Serialize`, so this is close
  to free, and a usage command is the kind of thing that ends up in a script.
- **No arguments, no subcommands, no flags that change what is measured.**
  `usage` is one read.

### 4.2 Output

The house style is `status`'s: a label column, aligned, greppable, no colour.
The bar sits where the value goes.

```
$ eggpass usage

x.ai
  Weekly      ████████░░░░░░░░░░░░  40% used
  Resets      Nov 18, 09:00 (UTC+01:00)
  Prepaid     $0.00

ChatGPT
  Usage       unavailable — Sign in with ChatGPT does not expose a quota API
  Limits      https://chatgpt.com/settings/usage
  Signals     429 subscription_sharing_usage_limit_exceeded
              503 subscription_sharing_usage_unavailable
              403 subscription_sharing_user_not_eligible
```

Rules:

- **One block per provider**, in `ProviderId::all()` order. A blank line
  between blocks.
- **The provider heading** is `ProviderId::display_name()`, plus ` · ` and
  the plan name when the provider named one.
- **The label column is 12 characters wide**, so every value starts in the
  same place. `  {label:<12}{value}`.
- **No colour.** The output is meant to be piped and diffed.
- **A provider that is not signed in still gets a block**, so the output does
  not change shape:

```
ChatGPT
  Usage       unavailable — not signed in; run `eggpass login --provider openai`
```

- **A provider that failed gets a block**, and the failure is stated, never
  swallowed:

```
x.ai
  Usage       unavailable — the provider answered HTTP 404
```

### 4.3 The bar

- **Twenty cells**, fixed. `█` (U+2588) filled, `░` (U+2591) empty.
- `filled = round(percent / 100 × 20)`, clamped to `0…20`.
- **A non-zero percentage never renders as an empty bar.** When `percent > 0`
  and `filled == 0`, `filled` is set to `1`. At 1 % a bar of all `░` would
  read as "none used", and the number on the right would contradict it.
- **The bar is omitted entirely** when the percentage is unavailable. There
  is no bar with a question mark in it: the line reads
  `Weekly      unavailable`, and nothing else.
- **The percentage text** carries at most one decimal, with a trailing `.0`
  dropped: `40%`, `12.5%`, `1%`.

### 4.4 Reset times, in the user's timezone

The brief requires local time. `eggpass` has no local-time code today —
`src/timeutil.rs` says so outright: *"eggpass deliberately avoids a date/time
crate: the only things it needs are 'now' in unix seconds, converting an RFC
3339 timestamp … into unix seconds, and printing a log timestamp."*

That reason no longer covers the whole job, but the conclusion still mostly
holds. Adding `chrono` or `time` for one offset lookup is a poor trade.

**Decision: `libc`, already in the dependency tree.** `libc 0.2.191` is
already in `Cargo.lock` (via `nix`, `dirs` and `ureq`), so promoting it to a
direct dependency adds **no new transitive crate and no version churn**.
`nix`'s own `time` feature was checked and does **not** expose `localtime_r`
— it is `clock_gettime`/`clock_nanosleep` only — so `nix` cannot do this.

`libc::localtime_r` is the reentrant, thread-safe, DST-aware way to get the
local offset at a given instant, and it honours `TZ`. The plan:

```rust
#[cfg(unix)]
pub fn local_offset_seconds(unix: i64) -> Option<i64>;   // via libc::localtime_r, tm_gmtoff
#[cfg(not(unix))]
pub fn local_offset_seconds(unix: i64) -> Option<i64>;   // None → fall back to UTC
```

The offset then feeds the existing `timeutil::civil_from_days`, so no new date
arithmetic is needed. `timeutil` gains:

- `format_local_datetime(unix) -> String` → `Nov 18, 09:00`
- `format_local_offset(unix) -> String` → `UTC+01:00`, or `UTC` at zero
- a twelve-entry month-abbreviation table

**The offset is printed next to every reset time.** `Nov 18, 09:00` alone is
ambiguous — a user in `UTC+2` and a colleague in `UTC-5` read the same string
and mean different moments. `Nov 18, 09:00 (UTC+01:00)` does not.

**The year is printed only when it differs from the current year.** A weekly
reset on 30 December lands in January, and `Jan 06, 09:00` would be
misleading. The rule is "same year as now → no year; different year → print
it", and it is one comparison in one place.

**DST is handled by `localtime_r`,** which looks the offset up *at the reset
instant* rather than at the current one. A reset at 02:30 on a spring-forward
morning resolves against the offset in force at that instant. This is the
reason the plan does not propose a hand-rolled tzfile parser: DST is where
hand-rolled timezone code goes wrong, and the payoff here is one offset.

### 4.5 "Unavailable" is not zero

Three different absences, three different displays. This is the rule the brief
states and the rule proto3 forces.

| Situation | Display |
| --- | --- |
| The provider named the field | The number, and a bar |
| The provider omitted the field (proto3 zero-omission, or an older account) | `unavailable` — **never `0%`** |
| The provider has no supported way to report it at all | `unavailable — <reason>` |

The distinction between "absent" and "zero" is load-bearing for
`creditUsagePercent`: **proto3 omits zero-valued scalars**, so a brand-new
account reads as *no field at all*. A parser that defaults a missing number
to `0.0` would tell a user with a fresh subscription that they have used 0 %
of it — when the truth is "the provider did not say". The model therefore
carries `Option<f64>`, never a bare `f64`, and there is no `unwrap_or(0.0)`
anywhere on that path.

The same distinction applies to `prepaidBalance`, in the other direction:
`{"val": 0}` serialises as `{}` because `val` is zero. So:

- `prepaidBalance` **absent** → unknown → `unavailable`.
- `prepaidBalance` **present**, `val` absent → **zero** → `$0.00`.

That asymmetry is the proto3 rule applied twice, and it is worth a comment in
the code so a future reader does not "fix" it.

### 4.6 Explicit state beats a calculated percentage

The brief: *"Any explicit backend exhaustion or eligibility state should take
precedence over a calculated percentage."*

The model carries an optional state, and when it is present the percentage is
not shown at all — the state is the answer.

| State | Source | Display |
| --- | --- | --- |
| `LimitReached` | the provider says the allowance is exhausted | `limit reached` |
| `NotEligible` | `config.isUnifiedBillingUser == false` | `not eligible for this plan` |
| `NotAllowed` | `ordinaryUsageAllowed == false` (Codex shape) | `not allowed` |

For Grok today, `isUnifiedBillingUser` is the only one that appears in the
verified payload. `LimitReached` has no Grok source yet — the mechanism is
built and stays empty until an explicit field provides it. **A percentage of
100 % is not a state.** It is a percentage that happens to be 100, and it is
shown as one. Inferring "limit reached" from "100 %" would be an inference,
and the rule above is about *explicit* states.

### 4.7 Exit codes

`usage` is a human-facing command, like `status`. A partial failure is
reported in the output, not in the exit code.

| Code | When |
| --- | --- |
| `0` | At least one provider produced a block — including one that said "unavailable" or "not signed in" |
| `1` | The arguments were wrong, **or** every provider failed operationally |
| `2` | No provider is signed in on this machine |

`2` matches the existing convention: `src/cli/commands/login.rs:39` already
uses it for "not logged in".

### 4.8 What `usage` must not do

- **No daemon.** `usage` reads the credential file and talks to the provider
  directly, exactly as `status`'s model count does. It must work when the
  proxy is stopped.
- **No polling, no caching, no background refresh.** One invocation, one read
  per provider, then exit.
- **No writing.** `usage` never writes the credential file, the key store, the
  config, or a cache file. It may *trigger* a token refresh — that is the
  `TokenManager`'s job and it writes the credential file — but it adds no
  state of its own.
- **No secret in the output.** No token, no `Authorization` header, no raw
  upstream body. The upstream body in particular can carry account details
  (`email`, `user_id`), and it goes to the terminal and into scrollback.

---

## 5. Architecture

### 5.1 A provider capability, not a core feature

This repository already reached the right conclusion about this shape, in
[`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md)
§4.3:

> A design question for any plan-limits feature: should eggpass expose the
> limits as *response headers* …, or as a *first-class endpoint*? … That
> asymmetry is a reason to keep this a *provider capability* rather than a
> core feature.

The two providers are not symmetric and never will be. Grok has a JSON
endpoint on a host we already talk to; ChatGPT has nothing. So usage lives in
the provider layer, and the CLI asks the provider layer what it can say.

`src/provider/usage/` is a new module beside `src/provider/xai.rs` and
`src/provider/openai.rs`:

```
src/provider/usage/mod.rs      the normalised model, and the dispatch
src/provider/usage/xai.rs      the Grok collector: the call, and the parsing
src/provider/usage/openai.rs   the ChatGPT collector: the "unavailable" answer
```

The dispatch is a plain match on `ProviderId`, mirroring
`provider::spec_for(id, config)` (`src/provider/mod.rs:163`). No trait is
needed: there are two implementations, and the repo already dispatches to
providers this way. A trait would be ceremony.

### 5.2 The normalised model

```rust
/// What a provider can say about its own allowance.
#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// The provider the report is about.
    pub provider: ProviderId,
    /// The plan name, when the provider named one.
    pub plan: Option<String>,
    /// The quota windows, in the order they are printed.
    pub windows: Vec<QuotaWindow>,
    /// A per-product breakdown of the same allowance.
    pub breakdown: Vec<ProductUsage>,
    /// Purchased credit, when the provider reports any.
    pub credits: Option<Credits>,
    /// The provider's own statement about eligibility or exhaustion.
    pub state: Option<QuotaState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaWindow {
    /// The label the window is printed under.
    pub label: WindowLabel,
    /// How much of the window is used, 0–100.
    ///
    /// `None` means the provider did not say. It never means zero.
    pub used_percent: Option<f64>,
    /// When the window resets, in unix seconds.
    pub resets_at: Option<i64>,
    /// How long the window runs, in seconds.
    pub duration_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub enum WindowLabel {
    Primary,
    Secondary,
    Weekly,
    Monthly,
    Daily,
    /// A label the provider used that we do not recognise.
    Other(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductUsage {
    pub product: String,
    pub used_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Credits {
    /// A prepaid balance, in cents.
    pub prepaid_cents: Option<i64>,
    /// Amount spent against purchased credit, in cents.
    pub on_demand_used_cents: Option<i64>,
    /// The ceiling on that spending, in cents.
    pub on_demand_cap_cents: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum QuotaState {
    /// The provider says the allowance is exhausted.
    LimitReached,
    /// The provider says this account is not entitled to the allowance.
    NotEligible,
    /// The provider says ordinary usage is not allowed.
    NotAllowed,
}
```

And the per-provider outcome, which is what the CLI actually switches on:

```rust
#[derive(Debug, Clone)]
pub enum ProviderUsage {
    /// The provider reported usage.
    Reported(UsageReport),
    /// The provider is not signed in on this machine.
    NotSignedIn,
    /// The provider has no supported way to report it.
    Unavailable {
        /// Why it is unavailable, in one line.
        reason: String,
        /// Where the user can see it, when there is such a place.
        hint: Option<String>,
        /// The error codes the provider does publish, when it publishes any.
        signals: Vec<&'static str>,
    },
    /// The attempt failed.
    Failed {
        /// Why it failed, in one line.
        reason: String,
    },
}
```

`Unavailable` and `Failed` are separate variants because they mean different
things to a user: one is a standing property of the provider, the other is
something that went wrong today. `status` treats a failed model count the same
way — it says `unavailable` and moves on.

### 5.3 The Grok collector

**The endpoint is not the configured upstream.** `cli-chat-proxy.grok.com` is
the `build` upstream's host (`src/config.rs:44`), but the billing endpoint
lives there **whatever the configured upstream is**. On the default `api`
upstream the usage read still goes to the CLI proxy.

That has a consequence the implementation must not get wrong:
`ProviderSpec::extra_headers` only carries `X-XAI-Token-Auth` when the
upstream is `build` (`src/provider/xai.rs:59-73`). Reusing the spec's client
would send the usage request **without** the one header the endpoint needs.
So the collector builds its own client.

That needs one small addition to `src/upstream.rs`:

```rust
impl UpstreamClient {
    /// Build a client for an endpoint that is not the provider's inference base.
    pub fn for_endpoint(
        base_url: &str,
        extra: &[(&'static str, String)],
        timeouts: crate::config::Timeouts,
    ) -> UpstreamClient
}
```

with `with_timeouts` refactored to delegate to it. Nothing else changes.

**The request.**

| Part | Value |
| --- | --- |
| Method | `GET` |
| URL | `https://cli-chat-proxy.grok.com/v1/billing?format=credits` |
| `Authorization` | `Bearer <access_token>` |
| `X-XAI-Token-Auth` | `xai-grok-cli` |
| `Accept` | `application/json` |
| `x-grok-client-version` | `crate::provider::xai::GROK_BUILD_CLIENT_VERSION` |
| `x-userid` | the stored `user_id`, when there is one |

`format=credits` is not optional. Without it the endpoint returns the
**legacy** shape (`monthlyLimit`, `used`, `history[]`) with no percentage at
all. The official client parses both; this plan asks for the new one and does
not implement the legacy one. See §[8](#8-what-is-not-implemented-and-why) for why.

`x-grok-client-version` is sent because the proxy has a client-version gate on
its request family and the header is harmless when the gate does not apply.
`x-userid` is sent when the stored credential has a `user_id`, because the
official client sends it and it costs nothing; it is not required.

**Deliberately absent:** `x-grok-client-identifier` and
`x-authenticateresponse`. The official billing request omits both. (The
spec's `x-grok-client-identifier: eggpass` belongs to inference.)

**The call.** `crate::upstream::send_with_auth` already does the right thing:
it takes a fresh token from the `TokenManager`, and on a `401` it refreshes
once and retries. No new retry logic is needed.

**The response handling.**

| Status | Outcome |
| --- | --- |
| `200` | parse the body |
| `401` after the refresh-and-retry | `Failed` — "the provider rejected the token" |
| `404` | `Failed` — "the endpoint is not available (HTTP 404)". The endpoint moved; this is the version-sensitivity case |
| `429` | `Failed` — "the provider is rate-limiting this account" |
| anything else | `Failed` — "the provider answered HTTP <status>" |

**The status code is reported. The body never is.** The `401` body echoes
request metadata (`auth_kind`, `x_xai_token_auth`, `upstream`, `reason`) —
useful for diagnosis, but it is upstream text and it does not belong in a
terminal or a log.

**The parsing.** A pure function, so it can be tested with no network at all:

```rust
pub fn parse_billing(body: &[u8]) -> Result<UsageReport, UsageError>;
```

| Field in | Field out | Rule |
| --- | --- | --- |
| `config.creditUsagePercent` | `windows[0].used_percent` | `Option<f64>`. Absent → `None`. Reject a value outside `0…100` as malformed rather than clamping it |
| `config.currentPeriod.type` | `windows[0].label` | Strip the `USAGE_PERIOD_TYPE_` prefix, case-insensitively; `WEEKLY` → `Weekly`, `MONTHLY` → `Monthly`, `DAILY` → `Daily`; anything else → `Other(raw)` |
| `config.currentPeriod.end` | `windows[0].resets_at` | RFC 3339 → unix seconds, via the existing `timeutil::parse_rfc3339_to_unix` |
| `config.currentPeriod.start` | — | The window's start. Shown only if a `--verbose` run wants it; not printed by default |
| `config.billingPeriodEnd` | `windows[0].resets_at` | The **fallback** when `currentPeriod` is absent |
| `config.prepaidBalance` | `credits.prepaid_cents` | Present with no `val` → **zero**. Absent → `None` |
| `config.onDemandUsed` | `credits.on_demand_used_cents` | Same rule |
| `config.onDemandCap` | `credits.on_demand_cap_cents` | Same rule |
| `config.productUsage[]` | `breakdown` | Each entry's `usagePercent` is independently optional |
| `config.isUnifiedBillingUser` | `state` | `false` → `NotEligible` |
| `config` is `null` or absent | — | The report has no windows → the provider said nothing → `Unavailable` |

**The plan name** is a second, best-effort call to `GET /v1/settings`, read as
`subscription_tier_display`. If that call fails, or the field is absent, the
plan is `None` and the heading is just the provider name. It is never inferred
from anything else.

### 5.4 The ChatGPT collector

```rust
pub fn collect(_runtime: &ProviderRuntime) -> ProviderUsage {
    ProviderUsage::Unavailable {
        reason: "Sign in with ChatGPT does not expose a quota API".to_string(),
        hint: Some("https://chatgpt.com/settings/usage".to_string()),
        signals: vec![
            "429 subscription_sharing_usage_limit_exceeded",
            "503 subscription_sharing_usage_unavailable",
            "403 subscription_sharing_user_not_eligible",
        ],
    }
}
```

That is the whole implementation. It is not a stub — it is the answer, and
§2.5 is the evidence for it. It does not make a network call, so it cannot
break, and it cannot leak a token.

The wording matters. It says what is true: the *authentication mode* does not
expose a quota API. It does not say "ChatGPT has no usage information" — the
user has usage information, OpenAI just does not publish a way to read it with
this credential.

### 5.5 The token

`ProviderRuntime::new(id, &config, &paths)` already holds everything needed:
the spec, the OAuth client, and a `TokenManager`. `TokenManager::access_token()`
(`src/auth/refresh.rs:122`) refreshes proactively when the token is within
five minutes of expiry, and `send_with_auth` refreshes again on a `401`.

So the Grok collector uses `runtime.tokens.access_token()` and the existing
`send_with_auth` path. **No new credential handling, and nothing new is
written to disk** beyond what a refresh already writes.

The x.ai token that `eggpass` stores is the one that works: the scopes
`grok-cli:access` and `api:access` are in the frozen scope set the official
client requests, and **there is no billing scope** — the billing endpoint
authorises on the ordinary token.

### 5.6 Logging and redaction

- The token never reaches the log: `Authorization` is added inside
  `UpstreamClient::send` and is not a loggable value. `logging::redact`
  scrubs `Bearer <…>` as a second line of defence.
- **The upstream body is never logged.** It can carry `email`, `user_id` and
  balance information. A parse failure logs the *field path* that was
  expected and the status code — `billing response: `config.creditUsagePercent`
  is not a number` — and never the body.
- The command's own output is the only place the numbers appear.

---

## 6. The details that are easy to get wrong

### 6.1 The caption that has to be there

`eggpass` sends inference to `api.x.ai` by default. That path is metered
against the **standard xAI API tier** — the token's `tier` claim — and is
billed like API usage. It does **not** draw from the SuperGrok consumer
weekly pool. The repo verified this on 2026-11-13: after two `api.x.ai`
completions the consumer pool still read `creditUsagePercent: 1.0`.

So the number `usage` shows for Grok is the user's **consumer subscription**,
and — on the default upstream — it is not what `eggpass`'s own traffic costs.

The output therefore carries a caption, and the caption depends on the
configured upstream (`config.providers.xai.upstream`):

| Upstream | Caption |
| --- | --- |
| `build` | `This is the pool `eggpass` draws from.` |
| `api` (the default) | `This is your Grok subscription. `eggpass` requests are billed separately against the xAI API tier.` |

A user who reads "40% used" and then sees an API bill has been misled by the
tool. The caption costs two lines and prevents that.

### 6.2 The window that is missing

`currentPeriod` may be absent, and `billingPeriodEnd` may be absent too. The
window then has **no label and no reset time** — but it may still have a
percentage. The line is then labelled by the provider's own period type if
there is one, and the reset line is omitted entirely rather than printed
with a placeholder. `Resets      unavailable` is noise; omitting the line is
honest.

### 6.3 A percentage outside `0…100`

The model says 0–100. A value outside that range means the provider changed
its meaning, and the honest response is to **not display a number** rather
than clamp it. `100.0` is in range and is displayed as `100% used`. `100.5` is
malformed: the window shows `unavailable` and the log says the field was out
of range. Clamping would invent a number the provider did not give.

### 6.4 `creditUsagePercent` is the *used* percentage

Confirmed against the official CLI's own panel. `40` means 40 % **used**, not
40 % remaining. The label in the output is `40% used`, never `40% left`.

### 6.5 `productUsage` is not the plan

`productUsage[]` breaks the pool down by **product** (`GrokBuild`,
`GrokChat`, `GrokImagine`). It is not the plan, and it is not the quota
window. It is rendered as an indented sub-list under the main window, and
only when it is present and non-empty:

```
x.ai
  Weekly      ████████░░░░░░░░░░░░  40% used
  Resets      Nov 18, 09:00 (UTC+01:00)
  GrokBuild       ██████████░░░░░░░░░░  50% used
  GrokChat        ████░░░░░░░░░░░░░░░░░  20% used
```

The product ids are not stable — the official fixture uses
`PRODUCT_GROK_BUILD` and live captures use `GrokBuild` — so they are printed
as the provider spelled them and never mapped onto a fixed vocabulary.

### 6.6 The reset time is in the future

A reset timestamp in the past means the provider's window has rolled and the
figure is stale. That is worth one line, and it is worth being careful about:
the window may roll between the read and the render, so a reset that is a few
seconds in the past is normal, not an error. The rule is: **a reset more than
an hour in the past** gets `(stale)` next to it. Anything closer is left
alone.

### 6.7 Adjacent work, deliberately not in this plan

Two things are close to this feature and are **not** it. Both are worth doing;
neither belongs in this work order.

- **Forwarding the upstream rate-limit headers on the streaming path.** Today
  `src/server/sse.rs:12` builds a streaming response with exactly three
  headers and discards everything the upstream sent, so a streaming client
  sees no budget information while a non-streaming client does. That is a real
  inconsistency, and it is a small fix. It is a change to the **proxy's
  responses**, not a new command, and it carries its own review.
- **A `GET /v1/usage` endpoint on the proxy.** That would have to answer for
  whichever provider is serving inference, out of a daemon that may not be
  running, for a client that wants machine-readable output. It is a different
  feature with a different shape — and the same asymmetry applies to it: Grok
  could answer it, ChatGPT could not.

---

## 7. Module layout and the file-by-file change list

| File | Change |
| --- | --- |
| `src/provider/usage/mod.rs` | **New.** The model (`UsageReport`, `QuotaWindow`, `WindowLabel`, `ProductUsage`, `Credits`, `QuotaState`, `ProviderUsage`), the `UsageError`, and the dispatch `pub fn collect(provider, runtime) -> ProviderUsage` |
| `src/provider/usage/xai.rs` | **New.** `BILLING_URL`, `BILLING_PATH`, `SETTINGS_PATH`, the header set, `collect`, and the pure `parse_billing` / `parse_settings` |
| `src/provider/usage/openai.rs` | **New.** The `Unavailable` answer and the SIWC error codes |
| `src/provider/mod.rs` | `pub mod usage;`, and `ProviderRuntime::usage()` delegating to the dispatch |
| `src/upstream.rs` | `UpstreamClient::for_endpoint(base_url, extra, timeouts)`; `with_timeouts` delegates to it |
| `src/timeutil.rs` | `local_offset_seconds`, `format_local_datetime`, `format_local_offset`, the month table; the module doc comment is corrected — it still avoids a *date/time crate*, but it now reads the local offset through `libc` |
| `src/cli/args.rs` | `Command::Usage { provider: Option<ProviderId>, json: bool }`; the `"usage"` arm; `global_help()`; `command_help("usage")` |
| `src/cli/commands/usage.rs` | **New.** The command: dispatch per provider, render the blocks, the caption, the exit code |
| `src/cli/commands/mod.rs` | `pub mod usage;` |
| `src/cli/mod.rs` | The dispatch arm |
| `Cargo.toml` | `libc = "0.2.191"` — already in `Cargo.lock`, so no new transitive crate and no version churn |
| `Cargo.lock` | `libc` becomes a direct dependency of `eggpass`. The version does not change |
| `docs/plans/cli-spec.md` | A new `## eggpass usage` section, in the style of the others |
| `docs/plans/README.md` | This document, in the index |
| `CHANGELOG.md` | An `### Added` entry under `[Unreleased]` |
| `README.md` | A row in the commands table |

### 7.1 Where the code goes, and why

- **`src/provider/usage/` and not `src/usage/`.** The module doc of
  `src/provider/mod.rs` says it outright: *"Everything provider-specific
  lives in `crate::provider`."* Usage is entirely provider-specific.
- **`src/cli/commands/usage.rs` and not `src/usage_cli.rs`.** It is a
  command, and the commands live in `src/cli/commands/`.
- **No trait.** Two implementations, and `provider::spec_for` is already a
  plain match.

### 7.2 The command's shape, in the style of the others

`src/cli/commands/status.rs` is the closest model: it iterates
`ProviderId::all()`, prints a block per provider, and treats a failure as
something to report rather than to propagate. `usage` follows it, with the
difference that `usage` makes a network call per provider and `status` makes
one per provider too (the model count). Neither requires the daemon.

---

## 8. What is not implemented, and why

| Not implemented | Why |
| --- | --- |
| **The legacy billing shape** (`monthlyLimit`, `used`, `history[]`) | It is only returned when `format=credits` is *absent*, and this plan always sends it. If the endpoint ever stops honouring the parameter, the honest result is `unavailable`, not a number computed from a deprecated shape — and the research explicitly warns against inferring exhaustion from a `0/0` pair |
| **The Codex app-server path** | §2.5. It needs a credential `eggpass` does not have, a token class it does not hold, and a private backend |
| **The WHAM endpoint** (`chatgpt.com/backend-api/wham/usage`) | Private and undocumented. The SIWC docs steer builders to deep-link the settings page instead |
| **A `GET /v1/usage` endpoint on the proxy** | A different feature with a different shape: it would need to answer for whichever provider is serving inference, from a daemon that may not be running. §[6.7](#67-adjacent-work-deliberately-not-in-this-plan) |
| **Forwarding `x-L-*` headers on the streaming path** | A real defect, and a cheap fix — but it is not this feature. §6.7 |
| **Caching the last reading** | The brief says a fresh read per invocation. A cached reading is a number that might be wrong, and this command exists to be right |
| **Colour** | The output is meant to be piped, diffed and read in a log |
| **Adapting the bar to the terminal width** | A fixed 20 cells keeps the output diffable, and keeps the tests simple |

---

## 9. Test plan

No test touches the network. The parsing is a pure function over bytes and
the rendering is a pure function over the model, so both are tested directly.

### 9.1 Parsing — `src/provider/usage/xai.rs`

| Test | Asserts |
| --- | --- |
| `the_verified_payload_is_parsed` | The 2026-11-13 body from §2.1 parses to a weekly window, 1 % used, a reset on 2026-11-20, a zero prepaid balance |
| `a_missing_percentage_is_unknown_not_zero` | A body with no `creditUsagePercent` yields `used_percent: None` — **not** `Some(0.0)` |
| `a_zero_percentage_is_the_same_as_a_missing_one` | Both yield `None`. This is the proto3 omission, and it is the test that protects the rule |
| `the_period_type_is_read_from_the_enum_name` | `USAGE_PERIOD_TYPE_WEEKLY` → `Weekly` |
| `an_unknown_period_type_is_kept_verbatim` | `USAGE_PERIOD_TYPE_FORTNIGHTLY` → `Other("USAGE_PERIOD_TYPE_FORTNIGHTLY")`, never a guess |
| `the_period_type_matches_on_a_suffix` | `USAGE_PERIOD_TYPE_MONTHLY` → `Monthly`, and a differently-prefixed `SOMETHING_WEEKLY` → `Weekly` |
| `a_null_config_is_unavailable` | `{"config":null}` → no windows, not a panic |
| `an_empty_prepaid_balance_is_zero` | `"prepaidBalance":{}` → `Some(0)` cents |
| `an_absent_prepaid_balance_is_unknown` | No `prepaidBalance` at all → `None` |
| `an_out_of_range_percentage_is_rejected` | `creditUsagePercent: 100.5` → `None`, and the error names the field |
| `a_product_without_a_percentage_is_kept` | `{"product":"GrokChat"}` → an entry with `used_percent: None` |
| `a_reset_time_is_parsed` | `2026-11-20T03:07:08Z` → the right unix seconds |
| `an_offset_timestamp_is_parsed` | `…T03:07:08.930537+00:00` → the same seconds |
| `a_non_json_body_is_an_error` | `"<html>"` → an error naming the parse failure, with no body in the message |

### 9.2 The model and the rendering — `src/cli/commands/usage.rs`

| Test | Asserts |
| --- | --- |
| `the_bar_is_twenty_cells` | 40 % → 8 filled, 12 empty |
| `a_tiny_percentage_still_shows_a_cell` | 1 % → 1 filled, not 0 |
| `zero_shows_an_empty_bar` | 0 % → 0 filled |
| `a_full_bar_is_twenty_cells` | 100 % → 20 filled |
| `the_percentage_text_drops_a_trailing_zero` | `1.0` → `1%`, `12.5` → `12.5%`, `40.0` → `40%` |
| `an_unavailable_window_has_no_bar` | The rendered line is `Weekly      unavailable` and contains no block characters |
| `a_state_replaces_the_percentage` | `NotEligible` renders `not eligible for this plan` and no percentage |
| `the_reset_time_carries_the_offset` | `Nov 18, 09:00 (UTC+01:00)` |
| `the_year_appears_only_when_it_differs` | A reset in the current year has no year; one in another year does |
| `the_caption_changes_with_the_upstream` | `build` → "draws from"; `api` → "billed separately against the xAI API tier" |
| `a_stale_reset_is_marked` | A reset more than an hour in the past renders `(stale)`; one 30 seconds in the past does not |
| `the_chatgpt_block_names_the_error_codes` | The three SIWC codes appear, and no percentage does |

### 9.3 Time — `src/timeutil.rs`

| Test | Asserts |
| --- | --- |
| `the_offset_is_zero_at_utc` | With `TZ=UTC`, `local_offset_seconds(0) == Some(0)` |
| `a_known_offset_is_applied` | With `TZ=UTC+02:00`, a summer instant is `+7200` |
| `dst_is_resolved_at_the_instant` | Two instants either side of a DST boundary, in a DST zone, get different offsets |
| `the_local_time_is_the_utc_time_plus_the_offset` | A known epoch renders as the expected wall-clock time |
| `an_impossible_local_time_falls_back_to_utc` | On a non-unix target, the offset is `None` and the rendering is UTC, marked as such |

The DST test sets `TZ` explicitly. `TZ` is process-global, so the tests that
set it must not run concurrently — `serial_test` is not a dependency, so the
alternative is to put the offset lookup behind a function that takes the zone
as an argument and test *that* directly, keeping only one process-global
`TZ` test. The plan prefers the second: `local_offset_seconds_in(zone, unix)`
is the tested primitive, and `local_offset_seconds(unix)` is the thin wrapper
that reads `TZ`.

### 9.4 The command line — `src/cli/args.rs`

| Test | Asserts |
| --- | --- |
| `usage_parses` | `eggpass usage` → `Command::Usage` |
| `usage_takes_a_provider_after_the_subcommand` | `eggpass usage --provider xai` → `Some(Xai)` |
| `usage_takes_a_provider_before_the_subcommand` | `eggpass --provider xai usage` → `Some(Xai)` |
| `usage_takes_json` | `eggpass usage --json` → `json: true` |
| `usage_takes_no_arguments` | `eggpass usage extra` → a usage error |
| `an_unknown_provider_is_an_error` | `eggpass usage --provider nonesuch` → a usage error |

### 9.5 The gate

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo +1.100.0 clippy --all-targets -- -D warnings
cargo +1.100.0 test --all-targets
```

---

## 10. Live verification, once, by hand

These are one-off checks against the live endpoints with the user's own
token, run by a developer during implementation. They are **not** automated
tests, and they must not be run against the live daemon.

1. **The percentage is 0–100 and is the *used* amount.** Already confirmed
   against the official CLI's panel, but re-check while the account has a
   non-trivial number.
2. **Whether `creditUsagePercent` is omitted at 0 %.** Proto3 says it should
   be. If it is *not* omitted, the `None`-vs-`Some(0.0)` distinction still
   holds and nothing changes.
3. **`GET /v1/settings` returns `subscription_tier_display`** with the
   ordinary x.ai token, and which plan it names.
4. **`x-userid` is accepted** when the stored credential has a `user_id`.
5. **The 401 body**, once, on a deliberately expired token, to confirm the
   error shape and that the message does not contain a secret.
6. **The legacy shape** — one call *without* `format=credits` — to confirm
   that the new shape is what we get when we send it, and to see what the
   fallback would look like if the parameter is ever dropped.

---

## 11. Risks

| # | Risk | Weight | Mitigation |
| --- | --- | --- | --- |
| R-1 | **The Grok endpoint is version-sensitive.** It is an internal CLI proxy, not a documented public API. The proxy has a client-version gate, and the `format` parameter has already changed once (legacy → `credits`), which broke third-party collectors | **High** | Parse leniently, treat every field as optional, and report `unavailable` rather than a wrong number. Send the client version header. The endpoint has no documented successor, so there is nowhere else to go |
| R-2 | **The number shown is the wrong budget.** On the default upstream, `eggpass`'s inference is billed against the API tier, not drawn from the pool the endpoint reports | **High** | The caption, §6.1. Tested |
| R-3 | **A missing percentage read as zero.** Proto3 omits zero-valued scalars, so a fresh account has no `creditUsagePercent` at all | **High** | `Option<f64>` end to end. A test asserts a missing percentage is `None` |
| R-4 | **The provider name is wrong.** The plan label comes from a second endpoint that may fail | Low | The plan name is optional enrichment. A failure omits the label; the usage still prints |
| R-5 | **A timezone bug.** A hand-rolled offset would be wrong across DST | Low | `localtime_r`, which resolves the offset at the instant. DST is tested |
| R-6 | **The command leaks an account detail into a log or a scrollback.** The billing body carries `email`, `user_id` and balances | Medium | The body is never logged. The output carries percentages, windows, resets and balances — the user's own data, on the user's own terminal |
| R-7 | **A ChatGPT user expects numbers.** The command will say "unavailable" for ChatGPT, and the reason has to be clear enough that it does not read as a bug | Medium | The wording in §5.4 names the authentication mode, links OpenAI's own usage page, and lists the error codes that do apply. The README says so too |
| R-8 | **`libc` is a new direct dependency** | Low | It is already in `Cargo.lock` at a pinned version. No new transitive crate, no version churn. The alternative — a hand-rolled tzfile parser — is worse |
| R-9 | **The usage read is not free.** It is one extra HTTP request per invocation | Low | One request per provider per invocation, with the existing timeouts. No polling, so the cost is bounded by how often the user runs the command |

---

## 12. Acceptance criteria

1. **`eggpass usage` prints a block for every signed-in provider**, in the
   fixed provider order, with no network call for a provider that is not
   signed in.
2. **The Grok block shows the percentage, the window label and the reset
   time**, with the bar, and the reset time in the local timezone with its
   offset.
3. **A missing Grok percentage shows `unavailable`**, never `0%`.
4. **An explicit state replaces the percentage** when the provider supplies
   one.
5. **The ChatGPT block says `unavailable`**, names the authentication mode,
   links `chatgpt.com/settings/usage`, and lists the three SIWC error codes.
6. **The caption names the pool**, and says that `eggpass` requests are
   billed separately on the `api` upstream.
7. **A provider that fails is reported**, with the status code, and the
   command still exits `0` when another provider produced a block.
8. **`eggpass usage` works with the daemon stopped**, and writes nothing.
9. **Every test in §9 passes**, and the gate in §9.5 is clean on Rust 1.89
   and 1.100.0.

---

## 13. Decisions

| Question | Decision |
| --- | --- |
| Is this a core feature or a provider capability? | A provider capability. `src/provider/usage/` |
| A trait, or a match? | A match, mirroring `provider::spec_for` |
| Which Grok endpoint? | `GET https://cli-chat-proxy.grok.com/v1/billing?format=credits`, always, whatever the configured upstream |
| Which headers? | `Authorization`, `X-XAI-Token-Auth`, `Accept`, plus `x-grok-client-version` and `x-userid` when known. **Not** `x-grok-client-identifier` |
| Where does the plan name come from? | `GET /v1/settings` → `subscription_tier_display`. Best-effort enrichment |
| The legacy billing shape? | Not implemented. A response without a percentage is `unavailable` |
| A missing percentage? | `None` → `unavailable`. Never `0%` |
| `prepaidBalance: {}`? | Zero. `prepaidBalance` absent → unknown. The proto3 rule, applied twice |
| A percentage outside 0–100? | Rejected as malformed. Not clamped |
| The ChatGPT side? | `Unavailable`, with the reason, the settings link and the three SIWC error codes |
| The Codex app-server? | Not used. It needs a workspace id `eggpass` cannot obtain, and a token class it does not hold |
| The WHAM endpoint? | Not used. Private and undocumented |
| Local time? | `libc::localtime_r` — already in `Cargo.lock`. `nix`'s `time` feature was checked and does not expose it |
| The reset format? | `Nov 18, 09:00 (UTC+01:00)`. The year only when it differs from now |
| The bar? | 20 cells, `█`/`░`, fixed width. A non-zero percentage always shows at least one cell |
| Colour? | None |
| A `--json` flag? | Yes. The model is `Serialize`, so it is nearly free and the command will be scripted |
| `--provider`? | Reuses the existing global flag. `usage` accepts it before and after the subcommand |
| The provider's display name? | `ProviderId::display_name()` — "x.ai" and "ChatGPT", the same names `status` uses. **Not** the brief's "Grok" and "Codex" |
| The caption? | Required. It changes with the configured upstream |
| Caching? | None. A fresh read every invocation |
| A `GET /v1/usage` endpoint on the proxy? | Not in this plan. §6.7 |
| Exit codes? | `0` when a block was produced, `1` on a usage error or when every provider failed, `2` when nobody is signed in |
| Release posture? | Unchanged. Manual review only, nothing published, no live daemon restarted |

---

## 14. What a reviewer should check first

1. **§4.5 — the missing-vs-zero rule.** It is the rule most likely to be
   broken by a later change, and it is the one that would tell a user
   something false rather than nothing.
2. **§6.1 — the caption.** A quota number without a statement of which pool
   it belongs to is the failure mode this feature could easily have.
3. **§5.4 and §2.5 — the ChatGPT answer.** If a reviewer believes the
   app-server path is workable with a SIWC token, §2.5 is the evidence to
   argue against it.
4. **§8 — the legacy shape.** The decision not to implement it is
   deliberate, and it is the one that would keep the parser small.

---

## 15. Related documents

- [`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md)
  — §4.1 has the first-hand Grok billing capture this plan is built on; §4.2
  has the ChatGPT side; §4.3 has the "provider capability, not a core
  feature" argument.
- [`plan-eggpass.md`](plan-eggpass.md) — §3.7 is the earlier statement of the
  same ChatGPT position, and open question 2 there ("Does the WHAM
  `/wham/usage` endpoint accept a SIWC token?") is the question this plan
  answers **no** to, on the evidence in §2.5.
- [`cli-spec.md`](cli-spec.md) — the command-by-command contract this plan
  extends.
- [`security-pass-2026-11-14.md`](security-pass-2026-11-14.md) and
  [`security-followup-2026-11-14.md`](security-followup-2026-11-14.md) — the
  auth model this plan does not touch.

---

## 16. What the implementation did

Implemented on 2026-11-14, against this document. The change list in §7 was
followed as written, with four deviations, all of them in the same direction:
less surface, more honesty.

### 16.1 The change list, as built

| File | What it holds now |
| --- | --- |
| `src/provider/usage/mod.rs` | **New.** The model, the `UsageError`, and the dispatch `pub fn collect(provider, runtime) -> ProviderUsage` |
| `src/provider/usage/xai.rs` | **New.** The billing and settings constants, the header set, `collect`, and the pure `parse_billing` / `parse_settings` |
| `src/provider/usage/openai.rs` | **New.** The `Unavailable` answer and the SIWC error codes |
| `src/provider/mod.rs` | `pub mod usage;`, `ProviderRuntime::usage()`, and `ProviderId`'s `Serialize` — a provider serialises as its configuration spelling (`xai`, `openai`) |
| `src/upstream.rs` | `UpstreamClient::for_endpoint(base_url, extra, timeouts)`; `with_timeouts` delegates to it |
| `src/timeutil.rs` | `local_offset_seconds`, `format_local_datetime`, `format_utc_offset`, the month table; the module doc now says why it reads the offset through `libc` |
| `src/cli/args.rs` | `Command::Usage { provider, json }`, the `"usage"` arm, `global_help()`, `command_help("usage")` |
| `src/cli/commands/usage.rs` | **New.** The command, the renderer and the exit code |
| `src/cli/commands/mod.rs` | `pub mod usage;` |
| `src/cli/mod.rs` | The dispatch arm |
| `Cargo.toml` / `Cargo.lock` | `libc = "0.2.191"` as a direct dependency; the version did not change, and no new transitive crate came with it |
| `docs/plans/cli-spec.md` | The `## eggpass usage` section |
| `CHANGELOG.md` | An `### Added` entry, and a `### Fixed` entry for the redactor panic below |
| `README.md` | A row in the commands table |
| `docs/plans/README.md` | The index row, the reading order, the status paragraph and the key-decisions row |

### 16.2 The deviations

1. **`format_local_datetime` takes three arguments.** §4.4 sketched
   `format_local_datetime(unix)`. The year rule — printed only when it differs
   from the current year — needs the current year, and the rendering has to be
   checkable against a fixed zone rather than against whatever the machine
   happens to be in, so the function takes `(unix, now, offset)`. The rule
   itself is unchanged. `format_local_offset` became `format_utc_offset`,
   which formats an offset rather than looking one up.

2. **`local_offset_seconds_in` is a test-only helper.** §9.3 preferred it as
   "the tested primitive", and it is: it takes the zone as an argument, so the
   daylight-saving test does not depend on the machine's own timezone. It sets
   `TZ`, reads the offset and restores `TZ`, and it is compiled only under
   `cfg(test)` — nothing in the shipped binary moves the environment.

3. **The offset is read through `localtime`, not `localtime_r`.** On glibc,
   `localtime_r` parses the zone the first time it is called in the process
   and reuses that parse afterwards; only `localtime` re-reads `TZ` every
   time. Since the test sets `TZ` at runtime, `localtime` is the one that
   answers the question being asked. It answers into a static buffer, so the
   lookup is serialised by a lock and the answer is copied out.

4. **A field-level problem is logged, not returned.** §9.1 wanted the
   out-of-range percentage to yield `None` *and* an error that names the
   field. Both are true, in different places: the parser drops the number and
   logs `billing response: config.creditUsagePercent is outside the range 0
   to 100`, and the helper that read the field returns an `UsageError` naming
   it. The body itself is never in the message.

5. **`ProviderUsage` is `Serialize`.** §5.2 showed the *report* as
   `Serialize` and the *outcome* as not. The outcome has to be serialisable
   for `--json` to say anything about a provider that is not signed in or
   that failed, so it is too. The report is nested under `report` rather than
   flattened, so the provider is named once and not twice.

6. **The caption is in the JSON too.** §6.1 says the caption is not
   optional, and a machine-readable number without it is as misleading as one
   on the terminal without it. The JSON carries it as a `caption` field.

7. **The timezone tests are gated to the platforms that have a timezone.**
   `libc::localtime_r` does not exist on Windows, so a named zone cannot be
   asked for there and the seven tests that ask for one are `#[cfg(unix)]`.
   §9.3's `an_impossible_local_time_falls_back_to_utc` runs everywhere: on a
   platform with no zone, the offset is `None` and the rendering is UTC.

### 16.3 What the review changed

An independent review of the implementation against this document found eight
things. Three were worth fixing, and the fixes are in the tree:

- **The daylight-saving tests would have failed on the project's own CI.**
  CI runs on `ubuntu-latest`, which is glibc, and glibc's `localtime_r`
  caches the zone it parsed the first time it is called in the process. The
  tests set `TZ` at runtime, so on glibc every one of them would have
  answered with whichever zone was parsed first. The lookup now goes through
  `localtime`, which re-reads `TZ` every time. This would have been a broken
  build on `main`, not a subtle wrong answer.
- **A percentage that rounds to zero was drawn as a non-empty bar.** At 0.04 %
  the bar showed a filled cell and the text beside it read `0% used`. The bar
  is now drawn from the percentage as it is printed, so the two agree.
- **An explicit state did not suppress the product breakdown.** §4.6 says the
  percentage is not shown at all when a state is present. The breakdown is
  now suppressed with it.

The rest were tightened rather than changed: the reset-time tests now assert
the literal epoch second instead of the parser's own answer to the same
question, the reset-time rendering is checked against a fixed offset rather
than against the machine's timezone, and the year rule is exercised through
the renderer as well as through the formatter.

### 16.4 One defect found on the way

The redactor in `src/logging.rs` walked a message a byte at a time and sliced
the string at those offsets, which is not always a character boundary. A
message carrying an em dash — `not signed in — run `eggpass login`` — aborted
the process on the way to the log. The comparison is on bytes now, and there
is a regression test.

### 16.5 Live verification

Run once, by hand, against the live endpoints with the account's own token —
never against the daemon, and never as an automated test.

| Check | Result |
| --- | --- |
| The billing endpoint answers with the current shape when `format=credits` is sent | **Confirmed.** The window, the percentage, the product breakdown and the prepaid balance all parsed |
| `GET /v1/settings` returns `subscription_tier_display` | **Confirmed.** The heading read `x.ai · SuperGrok` |
| `x-userid` is accepted when the stored credential has a user id | **Confirmed.** The request carried it and succeeded |
| The percentage is 0–100 and is the amount *used* | **Confirmed.** The live reading rendered `20% used`, and the label reads `used`, never `left` |
| Whether `creditUsagePercent` is omitted at 0 % | **Not checked.** The live account is not at 0 %. The rule does not depend on the answer: an absent field and a zero field both read `unavailable` |
| The `401` body, on a deliberately expired token | **Not checked.** The body is never printed by design, so the check would only confirm a string nobody sees |
| The legacy shape, one call without `format=credits` | **Not checked.** It would mean putting the token in a shell command and copying the raw upstream body to disk, which is exactly what this work order forbids |
