# Configuration

## The config file

`~/.config/eggpass/config.toml`, written with the defaults on first run.
`eggpass edit` opens it in `$EDITOR`; the file is validated when the editor
exits. `--config <PATH>` points a single command at a different file.

This is the file `eggpass` writes for you:

```toml
# eggpass configuration
#
# This file is plain TOML and is safe to edit by hand.

# The address the local API binds to. Loopback by default.
# Change it with `eggpass host`, not by editing this file.
host = "127.0.0.1"

# TCP port the local API listens on.
# Change it with `eggpass port`, not by editing this file.
port = 8484

# Key clients must send as `Authorization: Bearer <key>` or `X-Api-Key: <key>`.
# Leave empty to accept any key — which is only safe while the host is loopback.
# `eggpass key add` issues keys of its own; this is the older, hand-written way.
api_key = ""

# Log verbosity: error | warn | info | debug
log_level = "info"

# Set to true to allow binding a privileged port below 1024.
allow_privileged_port = false

# The upstream timeouts, in seconds. The defaults are generous because a long
# answer can take minutes to stream.
[timeouts]
connect_seconds = 30
global_seconds = 1800
body_seconds = 1800

# The local server's limits.
[server]
max_connections = 256
idle_timeout_seconds = 120
write_timeout_seconds = 600

# Where `eggpass update` looks for releases. The repository publishes a
# release manifest beside the binaries it names.
[update]
repository = "eggstack/eggpass"
manifest_name = "eggpass-release-manifest.json"

# Which upstream serves x.ai requests.
#   "api"   = https://api.x.ai/v1                    (full catalog, default)
#   "build" = https://cli-chat-proxy.grok.com/v1     (Grok Build proxy)
[providers.xai]
enabled = true
upstream = "api"

# ChatGPT, reached through Sign in with ChatGPT for Open Source.
[providers.openai]
enabled = true

# Which provider serves a model, and the prefix map used to decide.
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
```

### The settings that matter

| Setting | Default | Notes |
| --- | --- | --- |
| `host` | `127.0.0.1` | Change it with `eggpass host`. Loopback unless you say otherwise. |
| `port` | `8484` | Change it with `eggpass port`. |
| `api_key` | `""` | The older, hand-written client key. `eggpass key add` is the newer way. |
| `log_level` | `info` | `error`, `warn`, `info` or `debug`. |
| `allow_privileged_port` | `false` | Required to bind below 1024. |
| `routing.default_provider` | `xai` | Where a model with no matching prefix goes. |
| `providers.*.enabled` | `true` | Turn a provider off without deleting its credential. |

### Routing

Requests are routed by **model name**. The longest matching prefix in
`routing.prefixes` wins; a model that matches nothing goes to
`routing.default_provider`.

So with the default map, `grok-4.3` goes to x.ai, `gpt-5.6-sol` and
`codex-mini-latest` go to ChatGPT, and an unrecognised name goes to x.ai.

---

## The files on disk

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/eggpass/config.toml` | Your configuration | `0644` |
| `~/.config/eggpass/keys.json` | The keys `eggpass` issued — salted hashes only | `0600` |
| `~/.config/eggpass/credentials/xai.json` | x.ai OAuth tokens | `0600` (directory `0700`) |
| `~/.config/eggpass/credentials/openai.json` | ChatGPT OAuth tokens | `0600` |
| `~/.config/eggpass/eggpass.pid` | Daemon pid, the host it bound, the port it serves | `0644` |
| `~/.config/eggpass/eggpass.log` | Daemon log | `0644` |

### The key store

`keys.json` holds one record per issued key: its number, its optional label,
the visible prefix, a random per-key salt, and the SHA-256 of
`salt || key`. **The key itself is never written to disk**, so the file
cannot be read to impersonate a client.

The store has exactly one writer — the `eggpass key` command. The server only
reads it, and re-reads it whenever it changes, which is why a new key works
on the next request with no restart.

### The credential files

One file per provider, so a provider can be signed out without touching the
other. Each holds the access token, the refresh token and the expiry, in
`0600`, inside a `0700` directory.

---

## The rename migration

An installation that predates the rename lived in `~/.config/grokproxy`. The
first run copies that directory's contents into `~/.config/eggpass` — it
**copies**, it does not move, and it deletes nothing — and leaves a
`migrated-to-eggpass` marker behind so the old directory says what happened.

The old directory is left exactly as it was, so you can look at it, and so
nothing is lost if you were relying on it.

---

## The Grok CLI import

`~/.grok/auth.json`, written by the official Grok CLI, is read as an import
source when `credentials/xai.json` is missing. You do not have to do anything
for this to happen — `eggpass status` simply shows you as logged in.

**One thing to know about refresh tokens.** x.ai rotates them: every refresh
returns a new refresh token and revokes the old one. `eggpass` keeps the new
one, so it keeps working — but if the credential was *imported* from
`~/.grok/auth.json`, the file it came from still holds the revoked token, and
the official Grok CLI will need its own login again. Log in with
`eggpass login` rather than relying on the import if you use both tools.
