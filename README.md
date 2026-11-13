# eggpass

A lightweight, local, OpenAI-compatible proxy for Grok.

It authenticates with x.ai over OAuth, stores the credentials on your own
machine, and serves an OpenAI-compatible API on `127.0.0.1:8484`. Any OpenAI
client — OpenCode, Cline, Continue, the OpenAI SDK, a shell script — can talk
to Grok models such as `grok-4.7` and `grok-4.6` without an x.ai API key.

> **Status.** The proxy works today against x.ai. The rename to `eggpass`, the
> interactive provider picker, the ChatGPT provider and the eggpack/eggup
> packaging pipeline are designed and written up, but **not yet implemented** —
> see [`plans/plan-eggpass.md`](plans/plan-eggpass.md).

```
┌──────────────┐   OpenAI API    ┌────────────┐   OAuth bearer   ┌──────────────┐
│ your client  │ ──────────────► │ eggpass    │ ───────────────► │  api.x.ai    │
│ (OpenCode…)  │  127.0.0.1:8484 │  (daemon)  │                  │  /v1/chat/…  │
└──────────────┘                  └────────────┘                  └──────────────┘
```

## Requirements

- Rust 1.85 or newer, to build it
- A Grok subscription account

## Install

```sh
git clone https://github.com/you/eggpass
cd eggpass
cargo install --path .
```

The result is a single binary, `eggpass`, with no runtime dependencies
beyond a certificate store.

## Quick start

```sh
eggpass login      # opens your browser; approve the login
eggpass run        # starts the server in the background
eggpass status     # shows the account and the server
```

Point your client at `http://127.0.0.1:8484/v1`.

On a headless machine, or over SSH, use the device-code flow instead:

```sh
eggpass login --device
```

Already running the official Grok CLI? Its credentials are picked up
automatically — `eggpass status` will show you as logged in.

## Commands

| Command | What it does |
| --- | --- |
| `eggpass login` | Authenticate with x.ai over OAuth. `--device` for headless. Replaces an existing login. |
| `eggpass logout` | Revoke the credential at x.ai, then delete it. The daemon keeps running and answers `401` until you log in again. |
| `eggpass status` | Show the account state and the daemon state. |
| `eggpass run` | Start the server as a background daemon. `--foreground` to stay in the terminal. |
| `eggpass stop` | Stop the daemon. |
| `eggpass restart` | Stop the daemon, then start it again. |
| `eggpass port` | Show the listen port, or change it. A change takes effect at the next `restart`. |
| `eggpass edit` | Open the config file in `$EDITOR`. |
| `eggpass croncheck` | Start the daemon only if it is not already running. Prints nothing. |

Every command is idempotent and safe to script from cron.

### Signing in to more than one provider

`eggpass login` and `eggpass logout` open an interactive picker when no
provider is named:

```
$ eggpass login

  Select a provider to sign in with:

    ❯ x.ai
      ChatGPT

  ↑/↓ or j/k to move · Enter to select · Esc to cancel
```

`login` lists every provider the build knows about; `logout` lists only the
ones you are actually signed in to. Signing in to a provider you are already
signed in to simply **replaces** the stored credential — there is no `--force`
flag. When stdin is not a terminal the picker falls back to a numbered list,
and `--provider <id>` skips it entirely.

This is **designed but not yet implemented**; today `login` and `logout`
target x.ai only. See
[`plans/cli-spec.md`](plans/cli-spec.md#eggpass-login) and
[Part 7](plans/plan-eggpass.md#part-7--the-interactive-provider-picker).

### Keeping it alive with cron

```cron
*/5 * * * * /usr/local/bin/eggpass croncheck >> ~/.config/eggpass/cron.log 2>&1
```

`croncheck` does nothing and exits quietly when the server is already up.

## Configuration

`~/.config/eggpass/config.toml`:

```toml
# TCP port the local API listens on. Takes effect after `eggpass restart`.
port = 8484

# Upstream that serves model requests.
#   "api"   = https://api.x.ai/v1                    (full catalog, default)
#   "build" = https://cli-chat-proxy.grok.com/v1     (Grok Build proxy)
upstream = "api"

# Key clients must send as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
# Leave empty to accept any key — the server only ever binds to 127.0.0.1.
api_key = ""

# Log verbosity: error | warn | info | debug
log_level = "info"

# Set to true to allow binding a privileged port below 1024.
allow_privileged_port = false

# The OAuth settings. Change these only if xAI rotates them; the values below
# are the ones the official Grok CLI uses.
[oauth]
client_id = "b1a00492-073a-47ea-816f-4c329264a828"
issuer = "https://auth.x.ai"
scopes = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write"
referrer = "eggpass"
```

Run `eggpass edit` to change it in your editor; the file is validated when
the editor exits.

## Endpoints

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/chat/completions` | Chat completions, streaming and non-streaming |
| `POST /v1/responses` | The OpenAI Responses API |
| `GET /v1/models` | The model catalogue |
| `GET /v1/models/{id}` | One model |
| `GET /health` | Liveness probe, never authenticated |

Both `/v1/chat/completions` and `/chat/completions` work, so it does not
matter whether your client's base URL already carries the `/v1`.

## Client setup

### OpenAI SDK

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8484/v1", api_key="not-needed")

stream = client.chat.completions.create(
    model="grok-4.7",
    messages=[{"role": "user", "content": "Hello"}],
    stream=True,
)
for chunk in stream:
    print(chunk.choices[0].delta.content or "", end="", flush=True)
```

### OpenCode

Add the provider to `~/.config/opencode/opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "eggpass": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Grok (local proxy)",
      "options": { "baseURL": "http://127.0.0.1:8484/v1" },
      "models": {
        "grok-4.7": { "name": "Grok 4.7" },
        "grok-4.6": { "name": "Grok 4.6" }
      }
    }
  }
}
```

### Cline / Continue / anything OpenAI-shaped

| Setting | Value |
| --- | --- |
| API provider | OpenAI Compatible |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | anything, e.g. `sk-local` — unless you set `api_key` in the config |
| Model | `grok-4.7` |

### curl

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"grok-4.7","messages":[{"role":"user","content":"Hello"}]}'
```

## How it works

- **Login** uses OAuth 2.1 authorization code + PKCE against
  `https://auth.x.ai`, with the public Grok CLI client id. The callback uses a
  random ephemeral loopback port, so you can log in while the daemon is
  running. The device-code flow is available for headless machines.
- **The upstream** is `https://api.x.ai/v1`, which serves the full Grok
  catalogue. The Grok Build proxy (`upstream = "build"`) is available as an
  alternative.
- **Your client's credentials never leave the machine** — they are stripped
  before the request is forwarded, and the Grok token is put in their place.
- **Tokens refresh themselves** five minutes before they expire. A refresh
  that returns no new refresh token keeps the old one, so a rotating provider
  cannot lock you out.
- **Streaming is not buffered.** Each chunk is flushed as it arrives, so the
  first token shows up when the model produces it.

## Files

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/eggpass/config.toml` | Your configuration | `0644` |
| `~/.config/eggpass/auth.json` | OAuth tokens | `0600` (directory `0700`) |
| `~/.config/eggpass/eggpass.pid` | Daemon pid and the port it serves | `0644` |
| `~/.config/eggpass/eggpass.log` | Daemon log | `0644` |

When the multi-provider work lands, this becomes
`~/.config/eggpass/credentials/<provider>.json` — one `0600` file per
provider (`xai.json`, `openai.json`) — and the existing `auth.json` is
migrated into `credentials/xai.json` on first sight, so nothing is lost.

`~/.grok/auth.json`, written by the official Grok CLI, is read as an import
source when `auth.json` is missing.

**One thing to know about refresh tokens.** x.ai rotates them: every refresh
returns a new refresh token and revokes the old one. `eggpass` keeps the new
one, so it keeps working — but if the credential was *imported* from
`~/.grok/auth.json`, the file it came from still holds the revoked token, and
the official Grok CLI will need its own login again. Log in with
`eggpass login` rather than relying on the import if you use both tools.

## Troubleshooting

**`not logged in — run 'eggpass login'`** — the credentials are missing or
expired and cannot be refreshed. Log in again.

**`401` from the proxy** — either you are logged out, or the client is
sending the wrong key. Check `eggpass status`, and check `api_key` in the
config.

**`address already in use`** — something else is on the port. Pick another one
with `eggpass port 9000` and then `eggpass restart`.

**The server says it is running but nothing answers** — the daemon is running
but its port has changed. `eggpass status` prints the port it is actually
serving. `eggpass restart` applies the configured port.

**The browser did not open for the login** — the URL is printed as well; open
it by hand, or use `eggpass login --device`.

**Streaming feels slow** — check that the client is not buffering. `curl -N`
shows the raw stream.

## Development

```sh
cargo build                 # debug build
cargo test                  # unit and integration tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo build --release       # stripped, optimised
```

The integration tests run the proxy in front of a stub upstream written
directly against a socket, so they never touch the network.

`scripts/verify-live.sh` is the one script that talks to x.ai for real. It
needs a human to approve the device-code login, and it spends a few tokens.

## Design notes

- One binary, one config file, no database, no cloud, no admin API.
- Loopback only. Your Grok credentials never leave the machine.
- No async runtime: blocking sockets, one thread per connection.
- The HTTP server is part of this project rather than a dependency, because
  the obvious choice (`tiny_http`) buffers responses in a one-kilobyte
  `BufWriter`, which would hold back the first token of every streamed
  answer.

## Documentation

The reasoning behind the design lives in [`plans/`](plans/):

- [`plans/research.md`](plans/research.md) — the x.ai OAuth surface, the
  two-host upstream comparison, the model catalogue, and a review of the
  reference implementation.
- [`plans/cli-spec.md`](plans/cli-spec.md) — the command-by-command contract.
- [`plans/implementation-plan.md`](plans/implementation-plan.md) — the build
  order, the acceptance criteria, and
  [what shipped](plans/implementation-plan.md#what-shipped-2026-11-13),
  including the places where the build departed from the plan.
- [`plans/research-model-refresh-codex-eggserve-limits.md`](plans/research-model-refresh-codex-eggserve-limits.md)
  — catalogue refresh, the ChatGPT auth path, the HTTP server choice, and
  what x.ai and OpenAI actually expose about plan limits.
- [`plans/plan-eggpass.md`](plans/plan-eggpass.md) — the plan for the
  combined Grok + ChatGPT proxy, the rename map, the interactive provider
  picker, and the eggpack/eggup packaging ecosystem.

## License

MIT
