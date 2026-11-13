# eggpass

A lightweight, local, OpenAI-compatible proxy for Grok and ChatGPT.

It authenticates with your provider over OAuth, stores the credentials on your
own machine, and serves an OpenAI-compatible API on `127.0.0.1:8484`. Any OpenAI
client — OpenCode, Cline, Continue, the OpenAI SDK, a shell script — can talk
to Grok and ChatGPT models without a provider API key.

```
┌──────────────┐   OpenAI API    ┌────────────┐   OAuth bearer   ┌──────────────┐
│ your client  │ ──────────────► │ eggpass    │ ───────────────► │  api.x.ai    │
│ (OpenCode…)  │  127.0.0.1:8484 │  (daemon)  │                  │  api.openai  │
└──────────────┘                  └────────────┘                  └──────────────┘
```

## Requirements

- Rust 1.89 or newer, to build it
- A Grok subscription account, a ChatGPT plan, or both

## Install

```sh
git clone https://github.com/eggstack/eggpass
cd eggpass
cargo install --path .
```

The result is a single binary, `eggpass`, with no runtime dependencies
beyond a certificate store.

## Quick start

```sh
eggpass login      # a picker lists the providers; approve the login
eggpass run        # starts the server in the background
eggpass status     # shows the accounts and the server
```

Point your client at `http://127.0.0.1:8484/v1`.

The xAI login uses the **device-code flow**: it prints a short code and a URL,
and you approve it from any device — a phone, another machine, or this one.
No browser needs to run here, which is also what makes it work over SSH.
`eggpass login --oauth` uses the browser redirect instead.

Already running the official Grok CLI? Its credentials are picked up
automatically — `eggpass status` will show you as logged in.

### The xAI sign-in page says "Grok Build"

It does, and it will. xAI has no general-purpose public OAuth client for
third-party apps, so `eggpass` signs in with the official Grok Build CLI's
public client id — the same public credential every Grok Build install
carries. xAI's consent page takes the product name from that client id, so
it introduces itself as Grok Build.

What we *do* control is the `referrer` parameter, which xAI documents as the
"OAuth usage attribution" value. We send `eggpass`, so the usage is attributed
to eggpass even though the page says Grok Build. xAI's own CLI sends `user`,
and OpenCode sends `opencode`.

If the page shows a **code to copy** instead of redirecting to your browser's
callback, paste that code into the terminal and press Enter — the login is
waiting for it, exactly as the official CLI does.

## Commands

| Command | What it does |
| --- | --- |
| `eggpass login` | Authenticate with a provider over OAuth. `--device` for headless (x.ai only). Replaces an existing login. |
| `eggpass logout` | Revoke the credential at the provider, then delete it. The daemon keeps running and answers `401` until you log in again. |
| `eggpass status` | Show the account state and the daemon state. |
| `eggpass run` | Start the server as a background daemon. `--foreground` to stay in the terminal. |
| `eggpass stop` | Stop the daemon. |
| `eggpass restart` | Stop the daemon, then start it again. |
| `eggpass port` | Show the listen port, or change it. A change is applied and the server is restarted for you. |
| `eggpass host` | Show the bind address, or change it. Same behaviour as `port` — loopback only unless you set an API key. |
| `eggpass edit` | Open the config file in `$EDITOR`. |
| `eggpass croncheck` | Start the daemon only if it is not already running. Prints nothing. |
| `eggpass update` | Update to the latest released version, or to one exact version. `--check` reports without changing anything. |

Every command is idempotent and safe to script from cron.

### Signing in to more than one provider

`eggpass login` and `eggpass logout` open an interactive picker when no
provider is named:

```
$ eggpass login

  Select a provider to sign in with:

  Use j/k or ↑/↓ to navigate, Enter to select, q/Esc to quit

    > x.ai
        Grok models, on your x.ai subscription
      ChatGPT
        ChatGPT models, on your ChatGPT plan

```

`login` lists every provider the build knows about; `logout` lists only the
ones you are actually signed in to. Signing in to a provider you are already
signed in to simply **replaces** the stored credential — there is no `--force`
flag. When stdin is not a terminal the picker falls back to a numbered list,
and `--provider <id>` skips it entirely.

The picker runs in raw mode through `nix`, matching the family's own terminal
selector: the screen is cleared and the menu redrawn from the top on every
keystroke. `j`/`k` and the arrow keys both move the cursor, and the cursor
stops at the ends rather than wrapping. `q` or `Esc` cancels, and `Ctrl-C`
interrupts the command.

### Keeping it alive with cron

```cron
*/5 * * * * /usr/local/bin/eggpass croncheck >> ~/.config/eggpass/cron.log 2>&1
```

`croncheck` does nothing and exits quietly when the server is already up.

## Configuration

`~/.config/eggpass/config.toml`:

```toml
# The address the local API binds to. Loopback by default.
host = "127.0.0.1"

# TCP port the local API listens on.
port = 8484

# Key clients must send as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
# Leave empty to accept any key — which is only safe while the host is loopback.
api_key = ""

# Log verbosity: error | warn | info | debug
log_level = "info"

# Set to true to allow binding a privileged port below 1024.
allow_privileged_port = false

# Which upstream serves x.ai requests.
#   "api"   = https://api.x.ai                  (full catalog, default)
#   "build" = https://cli-chat-proxy.grok.com   (Grok Build proxy)
[providers.xai]
enabled = true
upstream = "api"

# ChatGPT, reached through Sign in with ChatGPT for Open Source.
[providers.openai]
enabled = true

# Which provider serves a model, and the prefix map used to decide.
# The longest matching prefix wins; an unmatched model goes to the default.
[routing]
default_provider = "xai"

[routing.prefixes]
"grok-" = "xai"
"gpt-" = "openai"
"codex" = "openai"

# The x.ai OAuth settings. Change these only if xAI rotates them; the values
# below are the ones the official Grok CLI uses.
[oauth.xai]
client_id = "b1a00492-073a-47ea-816f-4c329264a828"
issuer = "https://auth.x.ai"
scopes = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write"
referrer = "eggpass"

# The ChatGPT OAuth settings. `ext_agent_host_id` identifies this installation
# and is created on the first sign-in if it is not set.
[oauth.openai]
issuer = "https://auth.openai.com"
scopes = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
agent_name_hint = "eggpass"
resource = "https://api.openai.com/v1"

# The upstream timeouts, in seconds.
[timeouts]
connect_seconds = 30
global_seconds = 1800
body_seconds = 1800

# The local server's limits.
[server]
max_connections = 256
idle_timeout_seconds = 120
write_timeout_seconds = 600

# Where `eggpass update` looks for releases.
[update]
repository = "eggstack/eggpass"
manifest_name = "eggpass-release-manifest.json"
```

Run `eggpass edit` to change it in your editor; the file is validated when
the editor exits. A first run writes this file with the defaults filled in.

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

`GET /v1/models` is the **union** of every provider you are signed in to,
fetched live. There is no compiled-in list of model names, so a model that no
provider advertises is a `404` rather than a hopeful `200`.

## Client setup

### OpenAI SDK

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8484/v1", api_key="not-needed")

stream = client.chat.completions.create(
    model="grok-4.3",
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
      "name": "Grok and ChatGPT (local proxy)",
      "options": { "baseURL": "http://127.0.0.1:8484/v1" },
      "models": {
        "grok-4.3": { "name": "Grok 4.3" },
        "gpt-5": { "name": "GPT-5" }
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
| Model | any model name from `GET /v1/models` |

### curl

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"grok-4.3","messages":[{"role":"user","content":"Hello"}]}'
```

## How it works

- **Login** uses OAuth 2.1 authorization code + PKCE. The callback uses a
  random ephemeral loopback port, so you can log in while the daemon is
  running. The device-code flow is available for headless machines.
- **x.ai** is reached at `https://auth.x.ai` with the public Grok CLI client
  id. The Grok Build proxy (`upstream = "build"`) is available as an
  alternative.
- **ChatGPT** is reached through *Sign in with ChatGPT for Open Source* at
  `https://auth.openai.com`. The first sign-in registers the installation and
  receives its own client id; later sign-ins reuse it. The route is the public
  `https://api.openai.com/v1` one — the internal Codex backend is not used.
- **Requests are routed by model name.** The longest matching prefix in
  `routing.prefixes` wins; an unmatched model goes to `routing.default_provider`.
- **Chat Completions are translated for ChatGPT.** Sign in with ChatGPT speaks
  the Responses API, so a Chat Completions request is converted on the way in
  and the answer converted back on the way out. x.ai needs no translation.
- **Your client's credentials never leave the machine** — they are stripped
  before the request is forwarded, and the provider's token is put in their
  place. The provider-owned headers are stripped too, so a client cannot make
  the proxy claim to be a different build.
- **Tokens refresh themselves** five minutes before they expire. A refresh
  that returns no new refresh token keeps the old one, so a rotating provider
  cannot lock you out. Each provider has its own credential file and its own
  refresh lock.
- **Streaming is not buffered.** Each chunk is flushed as it arrives, so the
  first token shows up when the model produces it. The upstream's rate-limit
  headers reach the client on the streaming path too.
- **Updates are verified.** `eggpass update` reads the release manifest,
  checks the SHA-256 of what it downloaded, makes the candidate prove its own
  version, and only then replaces the binary — under a lock, with a rollback.

## Files

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/eggpass/config.toml` | Your configuration | `0644` |
| `~/.config/eggpass/credentials/xai.json` | x.ai OAuth tokens | `0600` (directory `0700`) |
| `~/.config/eggpass/credentials/openai.json` | ChatGPT OAuth tokens | `0600` |
| `~/.config/eggpass/eggpass.pid` | Daemon pid, the host it bound, the port it serves | `0644` |
| `~/.config/eggpass/eggpass.log` | Daemon log | `0644` |

An installation that predates the rename lived in `~/.config/grokproxy`. The
first run copies that directory's contents into `~/.config/eggpass` — it
**copies**, it does not move, and it deletes nothing — and leaves a
`migrated-to-eggpass` marker behind so the old directory says what happened.

`~/.grok/auth.json`, written by the official Grok CLI, is read as an import
source when `credentials/xai.json` is missing.

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
with `eggpass port 9000`; the command checks the port is free before it
restarts anything, and tells you so if it is not.

**The server says it is running but nothing answers** — the daemon is running
but its port has changed. `eggpass status` prints the port it is actually
serving. `eggpass restart` applies the configured port.

**The browser did not open for the login** — the URL is printed as well; open
it by hand, or use `eggpass login --provider xai --device`.

**Streaming feels slow** — check that the client is not buffering. `curl -N`
shows the raw stream.

**A ChatGPT request fails with `subscription_sharing_usage_limit_exceeded`** —
the ChatGPT plan's allowance is used up, or the account is not eligible for
plan usage. The error code comes from OpenAI; `eggpass status` shows whether
plan usage was granted to this installation.

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
- Loopback only. Your provider credentials never leave the machine.
- No async runtime: blocking sockets, one thread per connection.
- The HTTP server is part of this project rather than a dependency, because
  the obvious choice (`tiny_http`) buffers responses in a one-kilobyte
  `BufWriter`, which would hold back the first token of every streamed
  answer.
- The updater is the eggstack family's `eggup` crates rather than a bespoke
  one, so `eggpass` behaves like every other tool in the family. That raised
  the MSRV from 1.85 to 1.89.

### Changing where it listens

`eggpass port` and `eggpass host` change the address the server listens on and
restart it in one step:

```sh
eggpass port 9000       # move to port 9000 and restart
eggpass host 0.0.0.0    # listen on every interface and restart
```

Both commands only restart when the value actually differs from what is being
served, and both refuse to change anything if the new address is unusable —
a taken port prints `port <port> is taken` and leaves the server alone. If
the restart fails, the old configuration is restored and the server comes back
up where it was.

`host` defaults to loopback, and that default is the safe one: binding
elsewhere puts the proxy, and through it your paid subscriptions, on the
network. A non-loopback host is refused unless you set an `api_key`, and warns
when it is allowed.

### Updating

```sh
eggpass update            # the latest published release
eggpass update 0.2.0      # one exact version — which is also the rollback
eggpass update --check    # report what would happen, change nothing
```

The update is performed by `eggup`, the eggstack family's verified updater: it
fetches the release manifest, checks the SHA-256 of what it downloaded, makes
the candidate report the version it claims to be, and only then replaces the
binary — under a lock, with a rollback if anything goes wrong. The SHA-256
check is an **integrity** check, not an authenticity check: it proves the
download was not corrupted, not that it came from us.

A running server keeps the old binary until it is restarted. `eggpass update`
says so rather than restarting for you, because an implicit restart during an
update would cut live streams.

## Documentation

The reasoning behind the design lives in [`plans/`](plans/):

- [`plans/shipped-2026-11-13.md`](plans/shipped-2026-11-13.md) — what the
  implementation actually built: every phase, every deviation from the plan,
  the live verification evidence, and the two things that still need a human.
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
  picker, and the eggpack/eggup packaging ecosystem, with
  [what shipped](plans/plan-eggpass.md#part-10--what-shipped) at the end.

## License

MIT
