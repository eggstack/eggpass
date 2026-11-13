# CLI specification

Binary name: `grokproxy`

Every command is **idempotent** and safe to run from cron. All commands exit
`0` on success, `1` on usage/operational error, `2` on "not logged in" where
that is meaningful (see per-command notes).

Global flags:

```
--config <path>   override the config file location
--verbose         verbose logging
--help, -h        show help for a command
--version         print version
```

---

## `grokproxy login`

Authenticate with x.ai over OAuth.

```
grokproxy login              # loopback browser flow (opens the browser)
grokproxy login --device     # device-code flow, for headless / SSH
grokproxy login --force      # discard existing credentials and log in again
```

**Behaviour**

1. If already logged in and `--force` was not passed, print the account
   (email or user id) and the token expiry, and exit `0` without re-authenticating.
2. Otherwise start an OAuth flow:
   - **Loopback (default):** bind a **random ephemeral** port on `127.0.0.1`,
     generate PKCE verifier/challenge plus `state` and `nonce`, open the
     browser at the authorize URL, and wait up to **10 minutes** for the
     callback on `http://127.0.0.1:<ephemeral>/callback`.
   - **Device (`--device`):** request a device code, print the user code and
     the verification URL, and poll every `interval` seconds (default 5) until
     approved, denied, or expired (30 minutes).
3. Exchange the code for tokens and write them to `~/.config/grokproxy/auth.json`
   (mode `0600`).
4. Print success, the account identifier, and when the access token expires.

The OAuth callback port is **independent of the API port**. Login works while
the daemon is running and does not require the daemon to be stopped.

**Exit codes:** `0` success · `2` already logged in without `--force` (not an
error, prints a notice) · `1` denied/expired/failed.

---

## `grokproxy logout`

Remove stored OAuth credentials.

```
grokproxy logout
```

**Behaviour**

- Delete `~/.config/grokproxy/auth.json` (or blank the token fields).
- Stop the running daemon? **No.** The daemon stays up and will begin returning
  `401` until the next login. `logout` prints a notice saying so.
- If no credentials are stored, print "not logged in" and exit `0`.

**Exit codes:** `0` always.

---

## `grokproxy status`

Report account state and daemon state.

```
grokproxy status
```

**Output** — both sections, always, so the output is stable for scripting:

```
Account:  logged in
  email:      david@example.com
  user id:    01J...
  token:      valid, expires in 5h 12m

Server:   running
  pid:       51234
  url:       http://127.0.0.1:8484
  upstream:  api.x.ai
  models:    10 available
```

```
Account:  logged out

Server:   stopped
```

**Exit codes:** `0` always. Parseability matters more than status codes here —
`status` is a human-facing command.

---

## `grokproxy run`

Start the server as a background daemon.

```
grokproxy run
grokproxy run --foreground    # run in the foreground instead (debugging)
```

**Behaviour**

1. If a daemon is already running, print its PID and URL and exit `0`
   **without** starting a second one.
2. Otherwise spawn a detached child, write its PID to
   `~/.config/grokproxy/grokproxy.pid`, redirect stdout/stderr to
   `~/.config/grokproxy/grokproxy.log`.
3. Wait for the child to bind the port and answer `/health` (up to ~10s).
   If it does not come up, print the last lines of the log and exit `1`.
4. Print the base URL that clients should use:
   `http://127.0.0.1:8484/v1`

**Exit codes:** `0` running · `1` failed to start.

---

## `grokproxy stop`

Stop the running daemon.

```
grokproxy stop
```

**Behaviour**

1. Read the PID file. If the PID is not alive, remove the stale PID file, print
   "not running", and exit `0`.
2. Send `SIGTERM`, wait up to 10 seconds for a graceful exit.
3. If still alive after the grace period, send `SIGKILL`.
4. Remove the PID file.

**Exit codes:** `0` stopped or was not running · `1` could not be stopped.

---

## `grokproxy restart`

Stop, then start.

```
grokproxy restart
```

**Behaviour** — `stop` then `run`, in that order. Prints the new PID and URL.
A restart is the point at which a changed `port` takes effect.

**Exit codes:** `0` running · `1` failed to restart.

---

## `grokproxy port`

Show or change the listen port.

```
grokproxy port            # print the current port
grokproxy port 9000       # change it
```

**Behaviour**

- With no argument, print the current port and the URL.
- With an argument, validate it (integer, 1–65535, not a privileged port below
  1024), write it to the config file, and report that it takes effect on the
  next restart.
- If the daemon is running and the port changed, print:
  `port set to 9000 — run 'grokproxy restart' to apply (currently serving 8484)`.
- Changing the port **does not** restart the daemon.

**Exit codes:** `0` ok · `1` invalid port.

---

## `grokproxy edit`

Open the config file in the user's editor.

```
grokproxy edit
```

**Behaviour**

1. Create the config file with defaults if it does not exist.
2. Open it with `$EDITOR`, falling back to `$VISUAL`, then to a sensible
   default (`helix` if installed, else `vim`, else `nano`).
3. When the editor exits, validate the file and report any parse errors
   without overwriting the user's file.

**Exit codes:** `0` ok · `1` editor failed or config invalid.

---

## `grokproxy croncheck`

Designed for a `crontab` entry — start the server only if it is not already
running.

```
grokproxy croncheck
```

**Behaviour**

1. If the daemon is running (PID alive **and** `/health` responds), do nothing
   and exit `0` silently.
2. If it is not running, start it as a daemon exactly as `run` does, then exit
   `0`.
3. All diagnostics go to the log file, not to stdout — a crontab entry must not
   email the user on every run.

**Exit codes:** `0` in all normal cases (running or successfully started) ·
`1` if it needed to start the daemon and the start failed.

**Example crontab entry**

```cron
*/5 * * * * /usr/local/bin/grokproxy croncheck >> ~/.config/grokproxy/cron.log 2>&1
```

---

## Files

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/grokproxy/config.toml` | User-editable config | `0644` |
| `~/.config/grokproxy/auth.json` | OAuth tokens | `0600` (dir `0700`) |
| `~/.config/grokproxy/grokproxy.pid` | Daemon PID | `0644` |
| `~/.config/grokproxy/grokproxy.log` | Daemon + cron log | `0644` |

`~/.grok/auth.json` (written by the official Grok Build CLI) is read as an
**import source** when `~/.config/grokproxy/auth.json` is absent, so a user who
already ran `grok login` can start using the proxy without logging in again.

## Config file

```toml
# ~/.config/grokproxy/config.toml

port = 8484
upstream = "api"      # "api" = api.x.ai  |  "build" = cli-chat-proxy.grok.com
api_key = ""          # empty = accept any key (the server binds loopback only)
log_level = "info"    # error | warn | info | debug
```

## Upstream behaviour

- Base URL: `https://api.x.ai/v1` when `upstream = "api"`.
- Base URL: `https://cli-chat-proxy.grok.com/v1` when `upstream = "build"`,
  in which case the request additionally carries
  `X-XAI-Token-Auth: xai-grok-cli` and `x-grok-client-version: <version>`.
- Incoming `Authorization` / `X-Api-Key` headers from the client are **stripped**
  and replaced with the Grok OAuth token, so client credentials never reach x.ai.

## Exposed endpoints

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/chat/completions` | OpenAI-compatible chat completions (streaming + non-streaming) |
| `POST /v1/responses` | OpenAI-compatible Responses API |
| `GET /v1/models` | Model list |
| `GET /v1/models/{id}` | Single model |
| `GET /health` | Liveness probe (no auth required) |
