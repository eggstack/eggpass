# grokproxy

A lightweight, local, OpenAI-compatible proxy for Grok.

It authenticates with x.ai over OAuth (PKCE), stores the resulting credentials
on your machine, and serves an OpenAI-compatible API on `127.0.0.1:8484` so any
OpenAI client — OpenCode, Cline, Continue, a shell script, the OpenAI SDK —
can talk to Grok models such as `grok-4.7` and `grok-4.6` without an x.ai API key.

> **Status: research phase.** No implementation code yet. See
> [`docs/research.md`](docs/research.md) for the technical findings this build
> is based on, and [`docs/cli-spec.md`](docs/cli-spec.md) for the command
> surface.

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

Full semantics in [`docs/cli-spec.md`](docs/cli-spec.md).

## Layout

```
.
├── docs/
│   ├── research.md   # OAuth flow, upstream hosts, model catalog, gotchas
│   └── cli-spec.md   # command-by-command contract
└── README.md
```

## License

MIT
