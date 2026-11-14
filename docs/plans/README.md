# Plans

All planning, research and specification documents for `eggpass` live here.
Nothing in this directory ships with the binary.

| Document | What it covers | Status |
| --- | --- | --- |
| [`research.md`](research.md) | x.ai OAuth surface, the two-host upstream comparison, model catalog, a file-by-file review of the reference implementation, daemon design, config layout, and the open-risk register. Includes the **locked decisions**. | Current — 2026-11-13 |
| [`cli-spec.md`](cli-spec.md) | The command-by-command contract for `login`, `logout`, `status`, `run`, `stop`, `restart`, `port`, `host`, `key`, `edit`, `update` and `croncheck`: behaviour, exit codes, side effects, file paths and config schema. | Current — 2026-11-13 |
| [`implementation-plan.md`](implementation-plan.md) | Phased build order, module layout, dependency set, per-phase acceptance criteria, test strategy and the risk-to-phase mapping. Ends with **what shipped**, including the deviations from the plan and why. | Current — 2026-11-13 |
| [`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md) | Follow-up research: how the model catalogue is refreshed today, what adding a ChatGPT/Codex OAuth path would take, whether `eggserve` should replace the handrolled HTTP server, and what plan/usage limits x.ai and OpenAI actually expose. | Current — 2026-11-13 |
| [`plan-eggpass.md`](plan-eggpass.md) | The plan for turning `eggpass` into a combined Grok + ChatGPT proxy: the streaming-concurrency findings, whether to drop the static model catalogue, the provider/routing/credential design, and the phase breakdown. Built on **Sign in with ChatGPT for Open Source**, not the Codex CLI's OAuth client. Also carries the rename map, the interactive provider picker, the `port`/`host` address commands, the eggpack/eggup packaging ecosystem, and the eggpool relationship. | Current — 2026-11-13 |
| [`shipped-2026-11-13.md`](shipped-2026-11-13.md) | The record of what the eggpass implementation actually built: every phase, every deviation from the plan, the live verification evidence, and the two things that still need a human. | Current — 2026-11-13 |
| [`security-pass-2026-11-14.md`](security-pass-2026-11-14.md) | The auth review: the five findings (unkeyed loopback reachable from a browser, the chunked-body abort, pre-auth body buffering, the `0644` config, the silent key-store fail-open), the evidence for each, the corrective design, and the decisions taken. | Implemented — 2026-11-14 |

## Reading order

1. [`shipped-2026-11-13.md`](shipped-2026-11-13.md) — what exists today.
   Start here if you want the current state; the other documents explain how
   it got that way.
2. [`research.md`](research.md) — why the design is what it is. Start at
   [Locked decisions](research.md#locked-decisions-2026-11-13) if you only want
   the conclusions.
3. [`cli-spec.md`](cli-spec.md) — what the tool does.
4. [`implementation-plan.md`](implementation-plan.md) — how it gets built, in
   what order, and how each step is verified.
5. [`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md)
   — the follow-up questions: catalogue refresh, a Codex auth path, HTTP
   server choice, and plan limits.
6. [`plan-eggpass.md`](plan-eggpass.md)
   — the design and phasing for the combined Grok + ChatGPT proxy, the rename
   map, the interactive provider picker, and the eggpack/eggup ecosystem.

## Key decisions

| Decision | Choice |
| --- | --- |
| Runtime | Rust — MSRV 1.89, edition 2024, stable-only, no async runtime |
| Upstream | `https://api.x.ai` (full model catalog), `cli-chat-proxy.grok.com` as fallback |
| Login | Browser loopback (PKCE) by default, `--device` for headless (x.ai only) |
| OAuth callback port | Random ephemeral loopback, independent of the API port |
| Config | TOML at `~/.config/eggpass/config.toml` |
| Credentials | One file per provider, `~/.config/eggpass/credentials/<provider>.json`, mode `0600` |
| Daemon | Self-contained, PID-file based — no launchd, no systemd |
| Bind address | `host` + `port`, both applied and restarted by their own commands; loopback by default |
| ChatGPT auth | Sign in with ChatGPT for Open Source — dynamic registration, no client secret, public `https://api.openai.com/v1` |
| ChatGPT callback port | Random ephemeral loopback, path `/auth/callback`, host `127.0.0.1` — SIWC permits a variable port |
| Provider selection | Interactive picker — `j`/`k` or `↑`/`↓` to move, `Enter` selects, `q`/`Esc` cancels; numbered-list fallback when stdin is not a TTY |
| Client keys | Issued by `eggpass key add` — shown once, stored as a salted hash, numbered, revoked by number; a key takes effect on the next request |
| Re-login | Replaces the stored credential atomically; there is no `--force` flag and no "already logged in" refusal |
| Logout | Revokes at the provider, then deletes the local credential; best-effort revocation, never a hard failure |
| Picker implementation | `nix`'s termios, following the family's own terminal selector — originally `crossterm`, reversed when the crossterm picker was found to render the menu as a staircase |
| Project name | `eggpass` — the rename map is Part 6.2 of [`plan-eggpass.md`](plan-eggpass.md) |
| HTTP server | Handrolled, kept — `eggserve` requires tokio and we have no async runtime |
| Model catalogue | Live. `GET /v1/models` is a round trip; an unknown model id is a `404`. No static list. |
| Chat Completions | Translated to/from the Responses API for ChatGPT, forwarded natively for x.ai |
| Packaging / updates | `eggpack` for producer packaging, `eggup` for verified consumer updates — Part 8 of [`plan-eggpass.md`](plan-eggpass.md) |
| Updater reference | `cargo-cleanme`, not eggpool — eggpool's `update` is a Python-to-Rust migration artefact and non-standard for the family |
| eggpool | Kept separate — eggpass is maintained on its own; eggpool ingests its OAuth layer downstream |

## Status

The implementation is complete. `api.x.ai` has been verified to accept the
x.ai OAuth token, and the proxy has been checked end to end against the real
service — see [what shipped](shipped-2026-11-13.md).

The **rename** to `eggpass`, the **interactive provider picker**, the
**`port`/`host` address commands**, the **ChatGPT provider** and the
**eggpack/eggup updater** are all **built**. See
[`shipped-2026-11-13.md`](shipped-2026-11-13.md).

Two things still need a human: a completed ChatGPT sign-in in a browser (the
SIWC flow is implemented but has not been exercised end to end), and a
published `eggpass` release (the updater is implemented but there is nothing
published for it to install).

The [security pass](security-pass-2026-11-14.md) is **built**. It records the
five auth-surface findings from the 2026-11-14 review and the decisions taken:
a client key is required to run, the authority is checked on every loopback
request, the body is read only after the key is, and the file modes and the
key-store warning are fixed. The release posture is still manual review only.
