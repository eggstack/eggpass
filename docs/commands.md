# Commands

`eggpass` runs the proxy on your machine, but sends inference requests to the provider selected by the model route. Prompts are transmitted to xAI or ChatGPT; they do not stay on the machine. See [configuration.md](configuration.md) for routing and network settings.

## Start here

A client key is required before the server can start. Create one, sign in to a provider, then start the server:

```sh
eggpass key add laptop          # copy the key; it is shown only once
eggpass login --provider xai
eggpass run
```

Point an OpenAI-compatible client at `http://127.0.0.1:8484/v1` and send the key as `Authorization: Bearer <key>` or `X-Api-Key: <key>`. The server listens on loopback by default. See [configuration.md](configuration.md) before binding it to a network interface.

## Global options

| Option | Meaning |
| --- | --- |
| `--config <PATH>` | Use a different TOML config file for this command. Other eggpass files stay in the usual data directory. |
| `--provider <ID>` | Select `xai` or `openai` for `login`, `logout`, `refresh`, or `usage`. |
| `--verbose`, `-v` | Enable debug logging. |
| `--help`, `-h` | Show help. |
| `--version`, `-V` | Show the version. |

Global options can appear before or after the command. For example:

```sh
eggpass --provider xai login
eggpass --config ./local.toml run
```

## `login` and `logout`

```sh
eggpass login                         # choose a provider interactively
eggpass login --provider xai
eggpass login --provider xai --device  # x.ai device-code flow
eggpass login --provider xai --oauth   # browser flow
eggpass logout                         # choose a signed-in provider
eggpass logout --provider openai
```

Without `--provider`, `login` offers enabled providers and `logout` offers providers with stored credentials. x.ai uses the device-code flow by default when available; ChatGPT uses a browser flow. If the browser cannot reach the callback listener (for example, when signing in from another computer), copy the entire callback URL from the browser address bar and paste it into the waiting CLI. ChatGPT requires the full URL, not only its authorization code. `--device` and `--oauth` cannot be combined. Signing in again replaces that provider's stored credentials.

`logout` attempts provider-side revocation, then removes the local credential. Revocation is best-effort, so a failed revocation does not prevent local removal. A running server stays up, but requests to the signed-out provider will fail until you sign in again.

**Exit codes:** `login` returns `0` on success or cancellation, `1` on failure, and `2` if device flow is requested for a provider that does not support it. `logout` returns `0` on success, cancellation, or failed provider-side revocation. Credential read or removal errors return `1`.

## `refresh`

```sh
eggpass refresh                         # choose a signed-in provider
eggpass refresh --provider openai       # select one directly
```

`refresh` forces an OAuth credential refresh immediately, even when the access token is not near expiry. It does not send an inference request or remove the existing credentials. Without `--provider`, the picker lists providers with stored credentials; cancelling does nothing. A successful refresh stores rotated credentials. If the provider rejects the refresh or the stored credentials have no refresh token, run `eggpass login --provider <ID>` to sign in again. The command returns `1` when an explicitly selected provider cannot be refreshed; with no selected provider and no stored credentials, it prints `Not logged in.` and returns `0`.

## `status`

```sh
eggpass status
```

Shows stored account state and daemon state, including the live address and port when running, plus the configured authentication state. It may contact signed-in providers to count available models. It does not show API keys or OAuth tokens.

**Exit code:** `0` if the command completes.

## `usage`

```sh
eggpass usage                         # query all enabled providers
eggpass usage --provider xai          # query one provider
eggpass usage --json                 # machine-readable report
```

`eggpass usage` first requests the running daemon's usage report, reusing its in-memory cached snapshot when available. A stale last-successful snapshot is shown with a warning on stderr. The CLI falls back to direct provider collection when the daemon is stopped or stale, the private runtime usage credential is missing or unreadable, or an older daemon returns `404`; after a connection failure it falls back only if a state recheck confirms the daemon is stopped or stale. Other daemon errors, rejected credentials, and malformed reports are surfaced rather than bypassed. By default it queries only providers enabled in configuration. `--provider` can select one enabled provider; selecting a disabled provider, or running the command when none are enabled, returns an actionable configuration error. Human-readable output shows the percentage and bar for the allowance left, plus each available reset time and countdown; JSON preserves the provider-reported fields. On the default xAI `api` upstream, the reported consumer-subscription quota is separate from inference requests, which are billed to the xAI API tier. The report's caption reflects the configured upstream. If quota is unavailable, the report may include the latest inference HTTP status kept by the daemon in memory. That endpoint reports only per-provider client-facing HTTP status codes, not prompts or response bodies.

**Exit codes:** `0` if at least one provider is signed in and not every selected provider failed operationally, `1` if every selected provider failed operationally, `2` if none is signed in.

### Authenticated daemon endpoint: GET /v1/usage

The daemon also serves `GET /v1/usage` for clients that need a usage report over HTTP. A configured or issued regular client key has its normal full-route access, using `Authorization: Bearer <key>` or `X-Api-Key: <key>`. The CLI's separate runtime usage credential is sent as `Authorization: Bearer <token>` and is accepted only for a bodyless `GET /v1/usage`; it is not listed with regular keys and cannot start the daemon. The response body uses the same `{"providers":[...]}` JSON structure as `eggpass usage --json`; it includes only enabled providers. If every provider is disabled, the endpoint returns a completed empty list. OpenAI/ChatGPT usage is queried only when that provider is enabled; the optional Codex app-server result may describe a different account from Eggpass's inference credential.

A complete report is retained only in daemon memory for 600 seconds from completion. Cache hits do not contact providers. A cold or expired request starts one shared refresh and its initiating client waits at most 30 seconds; if still running, that client receives `504` and the refresh continues. Other requests while it is active receive `202` with `Retry-After`. Timed-out providers alone are retried, for at most three retries after the initial attempt, spaced 5, 15, and 30 seconds apart. xAI usage HTTP requests have bounded 15-second request/body timeouts (and a 5-second connect cap); OAuth refresh requests have a 60-second total deadline, and the optional local Codex session has an 8-second deadline.

On success, responses include `Cache-Control: private, no-store`, `X-Eggpass-Usage-Age` (integer seconds), and `X-Eggpass-Usage-Updated-At` (RFC 3339 UTC). `last_inference` is attached from the daemon's current in-memory state on each response. After all timeout retries fail, the last successful report is returned with `X-Eggpass-Usage-Stale: true` during a 600-second cooldown; if no successful report exists, the endpoint returns `503` with `Retry-After`. A process restart clears the snapshot and cooldown. There is no polling, disk persistence, or force-refresh operation.

## `run`, `stop`, and `restart`

```sh
eggpass run                    # start in the background
eggpass run --foreground       # stay in this terminal
eggpass stop
eggpass restart
```

`run` and `restart` require at least one client key, either from `eggpass key add` or from `api_key` in the config file. `run` reports if the daemon is already running. The daemon uses a PID file and log file; it does not require launchd or systemd. On Unix, `stop` requests a graceful shutdown and force-stops the daemon if it does not exit within 10 seconds. On Windows, `stop` terminates the daemon process directly, so in-flight requests may be interrupted. `restart` checks for a key before stopping the current daemon.

**Exit codes:** `run` and `restart` return `0` when running and `1` on failure. `stop` returns `0` if stopped or already stopped, and `1` if stopping fails.

## `port` and `host`

```sh
eggpass port                 # show configured address and port
eggpass port 9000            # change the port
eggpass host                 # show the bind address
eggpass host 0.0.0.0         # bind all IPv4 interfaces
```

When the daemon is running, a changed value is applied by restarting it. When it is stopped, the new value takes effect at the next start. The commands validate the new address before stopping a running server and roll back the config if restart fails. A port must be from 1 through 65535. Ports below 1024 require `allow_privileged_port = true`. Hosts must be IP addresses, or `localhost`, `local`, or `loopback` for `127.0.0.1`.

Loopback is the safer default. Binding outside loopback exposes the proxy and the provider accounts it can use to the network, so a client key is required. An SSH tunnel is an alternative:

```sh
ssh -L 8484:127.0.0.1:8484 box
```

**Exit codes:** `0` if the value is shown or applied, including when already serving it. `1` for an invalid or unavailable address, a write failure, or a failed restart.

## `eggpool`

```sh
eggpass eggpool --control pool.example:18443 --server-name pool.example --ca ./eggpool-ca.pem --label workstation
eggpass eggpool --revoke
```

Setup writes non-secret connection settings to [eggpool], creates an EggPass API key for the agent, and prints one epagent1. enrollment ticket to stdout. Treat the ticket as a secret: it contains the one-time enrollment credential and the EggPass API key, and is shown only once. If the daemon is already running on loopback, setup restarts it to activate the tunnel; if it is stopped, setup leaves it stopped. The listener must remain loopback-only while EggPool is enabled. eggpass status reports connection state without printing credentials.

--revoke stops a running daemon before removing local enrollment state and the EggPool API key. It restarts only if another EggPass API key remains; otherwise the daemon stays stopped. It does not contact EggPool to revoke a certificate already issued there; that server-side operation must be provided by EggPool.

Exit code: 0 on success; 1 if settings, credentials, the CA file, or daemon restart cannot be handled.

## `key`

```sh
eggpass key add [LABEL]
eggpass key list
eggpass key revoke <N>
eggpass key revoke --all
```

`eggpass key` with no action lists keys. New keys are shown once, then only salted hashes are stored. Keep the displayed key somewhere safe and send it to clients in an authorization header. Labels are optional. Key numbers are stable and are not reused. Adding or revoking a key takes effect on the next request without a restart.

The config file's `api_key`, if set, remains accepted alongside issued keys. Revoking every issued key does not disable that configured key. If there are no issued keys and no configured `api_key`, the running server refuses authenticated API requests with `401`; `GET /health` and `GET /v1/last-inference` remain unauthenticated. The health response is JSON with `status`, `service`, and a `version` string. The last-inference response contains only a map of provider names to their latest client-facing HTTP status codes.

**Exit code:** `0` on success. `1` if the key store cannot be read or written, or a requested key number does not exist.

## `edit`

```sh
eggpass edit
```

Opens the config in `$EDITOR`, then `$VISUAL`, then `helix`, `vim`, or `nano`, falling back to `vi`. The config is validated after the editor exits.

**Exit code:** `0` if the edited config is valid, `1` if the editor fails or validation fails.

## `croncheck`

Start the daemon only if it is not already running. It is intended for a crontab, but it still requires a client key and can fail. Ordinary success is quiet.

```sh
eggpass croncheck
```

```cron
*/5 * * * * /usr/local/bin/eggpass croncheck >> ~/.config/eggpass/cron.log 2>&1
```

**Exit codes:** `0` if already running or started, `1` on failure.

## `update`

```sh
eggpass update              # install the latest release
eggpass update 0.1.0        # install an exact version
eggpass update --check      # report the plan without changing files
```

The updater uses the published release manifest and checks the downloaded asset's digest for integrity. The digest does not prove who published it. It does not restart a running daemon, so that process keeps the old binary until restarted. Cargo-managed installations cannot be replaced by this updater. See [updating.md](updating.md) for verification details.

**Exit codes:** `0` when updated or when `--check` completes, `1` if the release is unavailable, verification fails, or this installation cannot be updated.
