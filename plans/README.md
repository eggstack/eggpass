# Plans

All planning, research and specification documents for `grokproxy` live here.
Nothing in this directory ships with the binary.

| Document | What it covers | Status |
| --- | --- | --- |
| [`research.md`](research.md) | x.ai OAuth surface, the two-host upstream comparison, model catalog, a file-by-file review of the reference implementation, daemon design, config layout, and the open-risk register. Includes the **locked decisions**. | Current — 2026-11-13 |
| [`cli-spec.md`](cli-spec.md) | The command-by-command contract for `login`, `logout`, `status`, `run`, `stop`, `restart`, `port`, `edit` and `croncheck`: behaviour, exit codes, side effects, file paths and config schema. | Current — 2026-11-13 |
| [`implementation-plan.md`](implementation-plan.md) | Phased build order, module layout, dependency set, per-phase acceptance criteria, test strategy and the risk-to-phase mapping. | Current — 2026-11-13 |

## Reading order

1. [`research.md`](research.md) — why the design is what it is. Start at
   [Locked decisions](research.md#locked-decisions-2026-11-13) if you only want
   the conclusions.
2. [`cli-spec.md`](cli-spec.md) — what the tool does.
3. [`implementation-plan.md`](implementation-plan.md) — how it gets built, in
   what order, and how each step is verified.

## Key decisions

| Decision | Choice |
| --- | --- |
| Runtime | Rust — MSRV 1.85, edition 2024, stable-only, no async runtime |
| Upstream | `https://api.x.ai/v1` (full model catalog), `cli-chat-proxy.grok.com` as fallback |
| Login | Browser loopback (PKCE) by default, `--device` for headless |
| OAuth callback port | Random ephemeral loopback, independent of the API port |
| Config | TOML at `~/.config/grokproxy/config.toml` |
| Daemon | Self-contained, PID-file based — no launchd, no systemd |
