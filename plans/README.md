# Plans

All planning, research and specification documents for `eggpass` live here.
Nothing in this directory ships with the binary.

| Document | What it covers | Status |
| --- | --- | --- |
| [`research.md`](research.md) | x.ai OAuth surface, the two-host upstream comparison, model catalog, a file-by-file review of the reference implementation, daemon design, config layout, and the open-risk register. Includes the **locked decisions**. | Current — 2026-11-13 |
| [`cli-spec.md`](cli-spec.md) | The command-by-command contract for `login`, `logout`, `status`, `run`, `stop`, `restart`, `port`, `edit` and `croncheck`: behaviour, exit codes, side effects, file paths and config schema. | Current — 2026-11-13 |
| [`implementation-plan.md`](implementation-plan.md) | Phased build order, module layout, dependency set, per-phase acceptance criteria, test strategy and the risk-to-phase mapping. Ends with **what shipped**, including the deviations from the plan and why. | Current — 2026-11-13 |
| [`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md) | Follow-up research: how the model catalogue is refreshed today, what adding a ChatGPT/Codex OAuth path would take, whether `eggserve` should replace the handrolled HTTP server, and what plan/usage limits x.ai and OpenAI actually expose. | Current — 2026-11-13 |
| [`plan-eggpass.md`](plan-eggpass.md) | The plan for turning `eggpass` into a combined Grok + ChatGPT proxy: the streaming-concurrency findings, whether to drop the static model catalogue, the provider/routing/credential design, and the phase breakdown. Built on **Sign in with ChatGPT for Open Source**, not the Codex CLI's OAuth client. Also carries the rename map, the interactive provider picker, the eggpack/eggup packaging ecosystem, and the eggpool scope overlap. | Current — 2026-11-13 |

## Reading order

1. [`research.md`](research.md) — why the design is what it is. Start at
   [Locked decisions](research.md#locked-decisions-2026-11-13) if you only want
   the conclusions.
2. [`cli-spec.md`](cli-spec.md) — what the tool does.
3. [`implementation-plan.md`](implementation-plan.md) — how it gets built, in
   what order, and how each step is verified.
4. [`research-model-refresh-codex-eggserve-limits.md`](research-model-refresh-codex-eggserve-limits.md)
   — the follow-up questions: catalogue refresh, a Codex auth path, HTTP
   server choice, and plan limits.
5. [`plan-eggpass.md`](plan-eggpass.md)
   — the design and phasing for the combined Grok + ChatGPT proxy, the rename
   map, the interactive provider picker, and the eggpack/eggup ecosystem.

## Key decisions

| Decision | Choice |
| --- | --- |
| Runtime | Rust — MSRV 1.85, edition 2024, stable-only, no async runtime |
| Upstream | `https://api.x.ai/v1` (full model catalog), `cli-chat-proxy.grok.com` as fallback |
| Login | Browser loopback (PKCE) by default, `--device` for headless |
| OAuth callback port | Random ephemeral loopback, independent of the API port |
| Config | TOML at `~/.config/eggpass/config.toml` |
| Daemon | Self-contained, PID-file based — no launchd, no systemd |
| ChatGPT auth (planned) | Sign in with ChatGPT for Open Source — dynamic registration, no client secret, public `https://api.openai.com/v1` |
| ChatGPT callback port (planned) | Random ephemeral loopback, path `/auth/callback`, host `127.0.0.1` — SIWC permits a variable port |
| Provider selection (planned) | Interactive picker — `↑`/`↓` or `j`/`k`, `Enter` selects, `Esc` cancels; numbered-list fallback when stdin is not a TTY |
| Re-login (planned) | Replaces the stored credential atomically; there is no `--force` flag and no "already logged in" refusal |
| Logout (planned) | Revokes at the provider, then deletes the local credential; best-effort revocation, never a hard failure |
| Project name (planned) | `eggpass` — the rename map is Part 6.2 of [`plan-eggpass.md`](plan-eggpass.md) |
| HTTP server | Handrolled, kept — `eggserve` requires tokio and we have no async runtime |
| Packaging / updates (planned) | `eggpack` for producer packaging, `eggup` for verified consumer updates — Part 8 of [`plan-eggpass.md`](plan-eggpass.md) |

## Status

The implementation is complete. `api.x.ai` has been verified to accept the
x.ai OAuth token, and the proxy has been checked end to end against the real
service — see [what shipped](implementation-plan.md#what-shipped-2026-11-13).

The **rename** to `eggpass`, the **interactive provider picker**, the
**ChatGPT provider**, and the **eggpack/eggup** packaging pipeline are all
**planned but not built**. See [`plan-eggpass.md`](plan-eggpass.md).

The provider surface is still x.ai only today. Everything else in this
directory describes the target design.
