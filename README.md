# grokproxy

A lightweight, local, OpenAI-compatible proxy for Grok.

It authenticates with x.ai over OAuth (PKCE), stores the resulting credentials
on your machine, and serves an OpenAI-compatible API on `127.0.0.1:8484` so any
OpenAI client — OpenCode, Cline, Continue, a shell script, the OpenAI SDK —
can talk to Grok models such as `grok-4.7` and `grok-4.6` without an x.ai API key.

> **Status: planning phase.** No implementation code yet. All planning,
> research and spec documents live in [`plans/`](plans/):
>
> - [`plans/research.md`](plans/research.md) — technical findings this build is
>   based on, including the
>   [locked decisions](plans/research.md#locked-decisions-2026-11-13) (Rust
>   runtime, `api.x.ai` upstream, browser-loopback OAuth)
> - [`plans/cli-spec.md`](plans/cli-spec.md) — the command surface
> - [`plans/implementation-plan.md`](plans/implementation-plan.md) — phased
>   build order, module layout and acceptance criteria

## Requirements

- Rust 1.85+ (stable; MSRV-pinned)
- A Grok subscription account with browser access to `accounts.x.ai`

## Goal

```
┌──────────────┐   OpenAI API    ┌────────────┐   OAuth bearer   ┌──────────────┐
│ your client  │ ──────────────► │ grokproxy  │ ───────────────► │  api.x.ai    │
│ (OpenCode…)  │  127.0.0.1:8484 │  (daemon)  │                  │  /v1/chat/…  │
└──────────────┘                  └────────────┘                  └──────────────┘
```

## Design principles

- **Small.** One binary, one config file, no database, no cloud, no MCP server,
  no admin API surface.
- **Local-only.** Binds to loopback. Your Grok credentials never leave the machine.
- **Long-lived.** Few dependencies, stable config format, no planned breaking changes.
- **Predictable.** Every command is idempotent and safe to script from cron.

## CLI

```
grokproxy login     # authenticate with x.ai over OAuth (browser)
grokproxy logout    # delete stored credentials
grokproxy status    # account state + daemon state
grokproxy run       # start the server as a background daemon
grokproxy stop      # stop the running daemon
grokproxy restart   # stop + start
grokproxy port      # show or change the listen port (default 8484)
grokproxy edit      # open the config file in $EDITOR
grokproxy croncheck # start the daemon only if it is not already running
```

Full semantics in [`plans/cli-spec.md`](plans/cli-spec.md).

## Layout

```
.
├── plans/
│   ├── README.md             # index of planning documents
│   ├── research.md           # OAuth flow, upstream hosts, model catalog, gotchas
│   ├── cli-spec.md           # command-by-command contract
│   └── implementation-plan.md # phased build order and acceptance criteria
└── README.md
```

## License

MIT
