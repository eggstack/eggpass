[![Crates.io version](https://img.shields.io/crates/v/eggpass.svg)](https://crates.io/crates/eggpass)
[![CI](https://github.com/eggstack/eggpass/actions/workflows/ci.yml/badge.svg)](https://github.com/eggstack/eggpass/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/eggstack/eggpass/blob/main/LICENSE)

# eggpass

A lightweight, local, OpenAI-compatible proxy for Grok and ChatGPT.

Sign in with your own subscription over OAuth, and every OpenAI client on your
machine — OpenCode, Cline, Continue, the OpenAI SDK, a shell script — can talk
to Grok and ChatGPT models. No provider API key, no cloud, nothing leaves the
machine.

```
┌──────────────┐   OpenAI API    ┌────────────┐   OAuth bearer   ┌──────────────┐
│ your client  │ ──────────────► │ eggpass    │ ───────────────► │  api.x.ai    │
│ (OpenCode…)  │  127.0.0.1:8484 │  (daemon)  │                  │  api.openai  │
└──────────────┘                  └────────────┘                  └──────────────┘
```

## Requirements

- Rust **1.89** or newer, to build it
- A Grok subscription account, a ChatGPT plan, or both

## Install

```sh
cargo install eggpass --git https://github.com/eggstack/eggpass
```

Or from a clone:

```sh
git clone https://github.com/eggstack/eggpass
cd eggpass
cargo install --path .
```

One binary, `eggpass`. No runtime dependencies beyond a certificate store.

## Get started

### Quick start

Three commands:

```sh
eggpass login      # pick a provider, approve the sign-in
eggpass run        # start the server in the background
eggpass status     # see who is signed in and what is running
```

Then point a client at `http://127.0.0.1:8484/v1`:

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"grok-4.3","messages":[{"role":"user","content":"Hello"}]}'
```

`eggpass run --foreground` keeps the server in the terminal instead.

### Signing in

`eggpass login` opens a picker listing the providers you can sign in with.
Signing in to a provider you are already signed in to replaces the stored
credential — there is no `--force` flag.

The **x.ai** sign-in uses the device-code flow: it prints a short code and a
URL, and you approve it from any device — a phone, another machine, or this
one. No browser needs to run on the machine, which is also what makes it work
over SSH. `eggpass login --oauth` uses the browser redirect instead.

The **ChatGPT** sign-in uses *Sign in with ChatGPT for Open Source*.

Already running the official Grok CLI? Its credentials are picked up
automatically — `eggpass status` will show you as logged in.

> The xAI consent page says "Grok Build", and it will. xAI has no
> general-purpose public OAuth client for third-party apps, so `eggpass` signs
> in with the official Grok Build CLI's public client id. See
> [docs/commands.md](docs/commands.md#login) for what that does and does not
> control.

### Client keys

`eggpass` issues the keys your clients use. You never bring your own — it
mints them, shows them once, and stores only a salted hash:

```sh
eggpass key add laptop          # → sk-eggpass-… shown once
eggpass key list                # what exists
eggpass key revoke 2            # take one away
```

A new key works on the **next request** — no restart. While no keys exist the
endpoint is open to anything on the machine, which is safe for a loopback
socket. Issue one and a key is required from then on.

### Pointing a client at it

| Setting | Value |
| --- | --- |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | anything — unless you issued one, then that key |
| Model | any name from `GET /v1/models` |

Ready-made setups for the OpenAI SDK, OpenCode, Cline, Continue and curl are
in [docs/client-setup.md](docs/client-setup.md).

## Commands

| Command | What it does |
| --- | --- |
| `eggpass login` | Authenticate with a provider over OAuth. `--device` for headless (x.ai only). |
| `eggpass logout` | Revoke the credential at the provider, then delete it. |
| `eggpass status` | Show the account state and the daemon state. |
| `eggpass run` | Start the server as a background daemon. `--foreground` to stay in the terminal. |
| `eggpass stop` | Stop the daemon. |
| `eggpass restart` | Stop the daemon, then start it again. |
| `eggpass port` | Show the listen port, or change it. A change is applied and the server restarted. |
| `eggpass host` | Show the bind address, or change it. Same behaviour as `port`. |
| `eggpass key` | Issue, list and revoke the keys your clients use. |
| `eggpass edit` | Open the config file in `$EDITOR`. |
| `eggpass croncheck` | Start the daemon only if it is not already running. Prints nothing. |
| `eggpass update` | Update to the latest released version, or to one exact version. `--check` reports without changing anything. |

Every command is idempotent and safe to script from cron.

## Endpoints

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/chat/completions` | Chat completions, streaming and non-streaming |
| `POST /v1/responses` | The OpenAI Responses API |
| `GET /v1/models` | The model catalogue |
| `GET /v1/models/{id}` | One model |
| `GET /health` | Liveness probe, never authenticated |

Both `/v1/chat/completions` and `/chat/completions` work, so it does not matter
whether your client's base URL already carries the `/v1`.

`GET /v1/models` is the **union** of every provider you are signed in to,
fetched live. There is no compiled-in list of model names, so a model no
provider advertises is a `404` rather than a hopeful `200`.

## Configuration

`~/.config/eggpass/config.toml`, written with the defaults on first run.
`eggpass edit` opens it in `$EDITOR`.

```toml
host = "127.0.0.1"   # the address the API binds to
port = 8484          # the port it listens on
api_key = ""         # optional hand-written client key
```

The full reference — routing, OAuth settings, timeouts, the file layout — is
in [docs/configuration.md](docs/configuration.md).

## Documentation

| Document | What it covers |
| --- | --- |
| [docs/commands.md](docs/commands.md) | Every command, its flags, its exit codes, its side effects. |
| [docs/client-setup.md](docs/client-setup.md) | OpenAI SDK, OpenCode, Cline, Continue, curl. |
| [docs/configuration.md](docs/configuration.md) | The config file, the files on disk, the migration. |
| [docs/how-it-works.md](docs/how-it-works.md) | Routing, translation, token refresh, key handling. |
| [docs/updating.md](docs/updating.md) | How `eggpass update` verifies and replaces itself. |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Symptoms, causes, fixes. |
| [docs/releasing.md](docs/releasing.md) | How a release is cut and signed off. Maintainers. |
| [docs/plans/](docs/plans/) | The research, the plans and what actually shipped. |

## Development

```sh
cargo test                  # unit and integration tests, no network
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo build --release
```

The integration tests run the proxy in front of a stub upstream written
directly against a socket, so they never touch the network.

`scripts/verify-live.sh` is the one script that talks to x.ai for real. It
needs a human to approve the device-code login, and it spends a few tokens.

## License

MIT — see [LICENSE](LICENSE).
