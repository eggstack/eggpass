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
| `--provider <ID>` | Select `xai` or `openai` for `login`, `logout`, or `usage`. |
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

Without `--provider`, `login` offers enabled providers and `logout` offers providers with stored credentials. x.ai uses the device-code flow by default when available; ChatGPT uses a browser flow. `--device` and `--oauth` cannot be combined. Signing in again replaces that provider's stored credentials.

`logout` attempts provider-side revocation, then removes the local credential. Revocation is best-effort, so a failed revocation does not prevent local removal. A running server stays up, but requests to the signed-out provider will fail until you sign in again.

**Exit codes:** `login` returns `0` on success or cancellation, `1` on failure, and `2` if device flow is requested for a provider that does not support it. `logout` returns `0` on success, cancellation, or failed provider-side revocation. Credential read or removal errors return `1`.

## `status`

```sh
eggpass status
```

Shows stored account state and daemon state, including the live address and port when running, plus the configured authentication state. It may contact signed-in providers to count available models. It does not show API keys or OAuth tokens.

**Exit code:** `0` if the command completes.

## `usage`

```sh
eggpass usage                         # query all providers
eggpass usage --provider xai          # query one provider
eggpass usage --json                 # machine-readable report
```

Queries provider usage directly, without needing the daemon. This is a fresh request, not a cached or polled reading. The report describes usage as reported by each provider. On the default xAI `api` upstream, the reported consumer-subscription quota is separate from inference requests, which are billed to the xAI API tier. The report's caption reflects the configured upstream. If quota is unavailable, the report may include the latest inference HTTP status kept by the daemon in memory. That endpoint reports only per-provider client-facing HTTP status codes, not prompts or response bodies.

**Exit codes:** `0` if at least one provider is signed in and not every selected provider failed operationally, `1` if every selected provider failed operationally, `2` if none is signed in.

## `run`, `stop`, and `restart`

```sh
eggpass run                    # start in the background
eggpass run --foreground       # stay in this terminal
eggpass stop
eggpass restart
```

`run` and `restart` require at least one client key, either from `eggpass key add` or from `api_key` in the config file. `run` reports if the daemon is already running. The daemon uses a PID file and log file; it does not require launchd or systemd. `restart` checks for a key before stopping the current daemon.

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
eggpass update 0.2.0        # install an exact version
eggpass update --check      # report the plan without changing files
```

The updater uses the published release manifest and checks the downloaded asset's digest for integrity. The digest does not prove who published it. It does not restart a running daemon, so that process keeps the old binary until restarted. Cargo-managed installations cannot be replaced by this updater. See [updating.md](updating.md) for verification details.

**Exit codes:** `0` when updated or when `--check` completes, `1` if the release is unavailable, verification fails, or this installation cannot be updated.
