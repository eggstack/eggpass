[![Crates.io version](https://img.shields.io/crates/v/eggpass.svg)](https://crates.io/crates/eggpass)
[![CI](https://github.com/eggstack/eggpass/actions/workflows/ci.yml/badge.svg)](https://github.com/eggstack/eggpass/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://github.com/eggstack/eggpass/blob/main/LICENSE)

# eggpass

A local, OpenAI-compatible proxy for Grok and ChatGPT. Sign in with your
subscription, then connect OpenAI-compatible clients to one local API.

`eggpass` runs on your machine and sends inference requests directly to x.ai or
OpenAI. It does not relay requests through an eggpass cloud service. You do not
need a separate provider API key.

## Requirements

- Rust 1.89 or newer to install from source with Cargo
- A Grok subscription, a ChatGPT plan, or both

## Install

```sh
cargo install eggpass --git https://github.com/eggstack/eggpass
```

Or install from a local checkout:

```sh
git clone https://github.com/eggstack/eggpass
cd eggpass
cargo install --path .
```

## Quick start

```sh
eggpass login              # choose a provider and sign in
eggpass key add laptop     # copy the key; it is shown once
eggpass run                # start the local server
eggpass status             # check login and server state
```

The x.ai login uses a device code by default. Use `--oauth` for a browser
redirect or `--device` to request the device-code flow explicitly. ChatGPT uses
a browser sign-in. Logging in again replaces that provider's saved credential.

`eggpass run` requires a client key. Issued keys are shown once and stored only
as salted hashes. Save the key for your client, for example:

```sh
export EGGPASS_API_KEY='paste-the-key-here'
```

Replace `MODEL_ID_FROM_V1_MODELS` in the request below with an ID returned by
`GET /v1/models`.

Send a test request:

```sh
curl http://127.0.0.1:8484/v1/chat/completions \
  -H "Authorization: Bearer $EGGPASS_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"MODEL_ID_FROM_V1_MODELS","messages":[{"role":"user","content":"Hello"}]}'
```

Use `eggpass run --foreground` to keep the server in the current terminal.

## Connect a client

| Setting | Value |
| --- | --- |
| Base URL | `http://127.0.0.1:8484/v1` |
| API key | The key from `eggpass key add`, or `api_key` in the config file |
| Model | An ID returned by `GET /v1/models` |

The client must send its key as `Authorization: Bearer <key>` or `X-Api-Key:
<key>`. Setup examples for the OpenAI SDK, OpenCode, Cline, Continue, and curl
are in [docs/client-setup.md](docs/client-setup.md).

To use the proxy from another machine, keep it bound to loopback and forward
the port over SSH:

```sh
ssh -L 8484:127.0.0.1:8484 box
```

## Providers and models

Requests are routed by model name. The longest matching prefix in the
configuration wins; unmatched names go to the default provider.

`GET /v1/models` merges live catalogues from signed-in providers. A provider
that is unavailable or does not return a usable catalogue may contribute no
models. The response is normalized for OpenAI-compatible clients, so some
provider-specific models or fields may not appear. There is no built-in model
list. Check the endpoint for current IDs before configuring a client.

`eggpass` supports Chat Completions and the Responses API. Chat Completions
requests for ChatGPT are translated to and from the Responses API. Details on
routing, streaming, and supported fields are in
[docs/how-it-works.md](docs/how-it-works.md).

## Keys and local access

The server requires at least one client key to start. Create and manage issued
keys with `eggpass key add`, `eggpass key list`, and `eggpass key revoke`.
Alternatively, set `api_key` in the config file. A key change takes effect on
the next request, without a restart.

The server binds to `127.0.0.1` by default. A loopback port can still be
reached by web pages, so the key is required. `GET /health` returns the version
string, and `GET /v1/last-inference` returns only the last client-facing HTTP
status per provider. Those two endpoints do not require a key.

Inference requests, including prompts and responses, are sent to the selected
provider. `eggpass` does not keep prompt or response history. It keeps only the
most recent inference status for each provider in daemon memory; that state is
cleared when the daemon stops.

## Common commands

| Command | Purpose |
| --- | --- |
| `eggpass login` | Sign in to a provider. |
| `eggpass logout` | Revoke and remove a provider credential. |
| `eggpass status` | Show provider login and server state. |
| `eggpass usage` | Read subscription usage where available. |
| `eggpass run` | Start the server. Use `--foreground` to keep it in the terminal. |
| `eggpass stop` / `restart` | Stop or restart the server. |
| `eggpass port` / `host` | Show or change the listen address. Changes restart the server. |
| `eggpass key` | Add, list, or revoke client keys. |
| `eggpass edit` | Edit the config file. |
| `eggpass croncheck` | Start the server if it is not running. |
| `eggpass update` | Install a verified published release. |

See [docs/commands.md](docs/commands.md) for flags, exit codes, and side
effects.

## Configuration

The config file is `~/.config/eggpass/config.toml`. It is created with defaults
on first run and can be opened with `eggpass edit`.

```toml
host = "127.0.0.1"
port = 8484
api_key = ""
```

See [docs/configuration.md](docs/configuration.md) for routing, provider
settings, timeouts, and files on disk.

## Documentation

| Document | Contents |
| --- | --- |
| [Commands](docs/commands.md) | Command syntax, options, and effects |
| [Client setup](docs/client-setup.md) | OpenAI SDK, OpenCode, Cline, Continue, and curl |
| [Configuration](docs/configuration.md) | Config settings and files |
| [How it works](docs/how-it-works.md) | Routing, translation, authentication, and streaming |
| [Updating](docs/updating.md) | Update verification and rollback |
| [Troubleshooting](docs/troubleshooting.md) | Common errors and fixes |
| [Releasing](docs/releasing.md) | Maintainer release procedure |
| [Design notes](docs/plans/) | Historical research and implementation record |

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --release
```

Tests use a local stub upstream and do not access provider services.

## License

MIT. See [LICENSE](LICENSE).
