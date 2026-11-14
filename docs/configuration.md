# Configuration

## Config file and data directory

The default config is `~/.config/eggpass/config.toml`. On first run, eggpass writes a file with the defaults. A missing config uses defaults; commands that load an invalid TOML file or value fail. Use `eggpass edit` to edit and validate it. The `--config <PATH>` option changes only the config file path, not the credential, key, PID, or log locations.

The data directory is selected in this order:

1. `EGGPASS_HOME`, which relocates all eggpass files.
2. `XDG_CONFIG_HOME/eggpass`.
3. `~/.config/eggpass`.

## Core settings

This valid TOML example shows the main settings and defaults. Omitted OAuth settings use built-in provider values.

```toml
host = "127.0.0.1"
port = 8484
api_key = ""
log_level = "info"
allow_privileged_port = false

[timeouts]
connect_seconds = 30
global_seconds = 1800
body_seconds = 1800

[server]
max_connections = 256
idle_timeout_seconds = 120
write_timeout_seconds = 600

[update]
repository = "eggstack/eggpass"
manifest_name = "eggpass-release-manifest.json"

[providers.xai]
enabled = true
upstream = "api"

[providers.openai]
enabled = true

[routing]
default_provider = "xai"

[routing.prefixes]
"grok-" = "xai"
"gpt-" = "openai"
"codex" = "openai"
```

| Setting | Default | Meaning |
| --- | --- | --- |
| `host` | `127.0.0.1` | Listen address. Use `eggpass host` to change it. |
| `port` | `8484` | Listen port. Use `eggpass port` to change it. |
| `api_key` | empty | Optional static client key. It is accepted alongside keys from `eggpass key add`. At least one of these key sources is required to start the server. |
| `log_level` | `info` | `error`, `warn`, `info`, or `debug`. |
| `allow_privileged_port` | `false` | Must be `true` to use a port below 1024. |
| `timeouts.*` | See example | Upstream connection, whole-request, and response-body time limits in seconds. |
| `server.*` | See example | Local connection limit and idle/write timeouts. |
| `providers.*.enabled` | `true` | Disable a provider without deleting its stored credentials. Routing to a disabled provider will not serve requests. |
| `providers.xai.upstream` | `api` | `api` uses `https://api.x.ai/v1`; `build` uses `https://cli-chat-proxy.grok.com/v1`. |
| `routing.default_provider` | `xai` | Provider used when no model prefix matches. |
| `routing.prefixes` | See example | Model-name prefixes mapped to providers. |
| `update.*` | See example | Repository and release-manifest name used by `eggpass update`. |

For a prefix match, the longest matching prefix wins. With the defaults, names beginning with `grok-` route to xAI; names beginning with `gpt-` or `codex` route to ChatGPT. Unmatched names route to xAI. Routing is based on the model name, not the endpoint path.

The advanced `[oauth.xai]` settings are `client_id`, `issuer`, `scopes`, and `referrer`. `[oauth.openai]` supports `issuer`, `scopes`, `agent_name_hint`, `resource`, and `ext_agent_host_id`. `scopes` can be a space-separated string or an array of strings. Leave these at their built-in values unless a provider requires a change. The ChatGPT host ID is generated and recorded on first sign-in if it is absent.

## Keys and network exposure

A client key is required to start the server, including when it listens only on loopback. Use `eggpass key add` to create a numbered, revocable key, or set `api_key` for one static key. The static key is readable in the config. Issued keys are shown once and stored as salted hashes. Clients may send either as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.

The server listens on loopback by default. Binding to a non-loopback address exposes the proxy and the provider accounts it can use to the network. A key is required for that bind. The `GET /health` and `GET /v1/last-inference` probes are unauthenticated: health returns a JSON object with `status`, `service`, and a version string; last-inference returns only per-provider client-facing HTTP status codes. All other API requests require a valid key.

Eggpass runs locally, but inference requests are sent to the provider selected by routing. Prompts and request content are transmitted to xAI or ChatGPT for inference; they do not stay on this machine. `eggpass usage` also contacts providers directly to query usage, and `eggpass status` may contact them to count available models.

Use `eggpass host` and `eggpass port` to change a live server's address. These commands update the config and restart the server when needed. Manual config edits take effect when the server next starts. See [commands.md](commands.md) for the commands and an SSH tunnel example.

## Files

| Path | Contents and handling |
| --- | --- |
| `config.toml` | Settings, possibly including the readable `api_key`. Normally mode `0600`. |
| `keys.json` | Issued-key numbers, labels, visible prefixes, salts, and hashes. It does not contain the full keys. Mode `0600`. |
| `credentials/xai.json` | xAI OAuth credentials, including tokens. Mode `0600`. |
| `credentials/openai.json` | ChatGPT OAuth credentials, including tokens. Mode `0600`. |
| `eggpass.pid` | Daemon process ID and the address and port it serves. |
| `eggpass.log` | Daemon log. |

The data directory and `credentials` directory are restricted to mode `0700`; credential and key files use `0600` on Unix-like systems. Keep any custom config file containing `api_key` private too.

With the default data directory, xAI credentials may be read from `~/.grok/auth.json` when `credentials/xai.json` is absent. If an imported refresh token is rotated, eggpass stores its replacement in its own credential file; the Grok CLI's original file is not updated. Setting `EGGPASS_HOME` disables this import to keep the relocated installation isolated.

Installations from before the rename may have data in `~/.config/grokproxy`. On first run, eggpass copies the old data into the new directory and leaves the original in place.
