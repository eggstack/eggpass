# CLI specification

Binary name: `eggpass`

Every command is **idempotent** and safe to run from cron. All commands exit
`0` on success, `1` on usage/operational error, `2` on "not logged in" where
that is meaningful (see per-command notes).

Global flags:

```
--config <path>   override the config file location
--provider <id>   name the provider directly; suppresses the picker
--verbose         verbose logging
--help, -h        show help for a command
--version         print version
```

Environment variables:

```
EGGPASS_HOME       override the config directory
EGGPASS_PROVIDER   same as --provider <id>, lower precedence than the flag
```

`--provider <id>` applies to the commands that talk to a provider (`login`,
`logout`). It is the non-interactive path into every command: with it set, the
picker is never drawn and stdin is never read, so the command behaves the same
whether it is run from a terminal, a pipe or cron. An unknown provider id is
an error, not a silent fallback.

---

## `eggpass login`

Authenticate with a provider over OAuth.

```
eggpass login              # interactive provider picker, then the login flow
eggpass login --provider xai   # pick the provider directly, no picker
eggpass login --device     # x.ai only: device-code flow, for headless / SSH
```

**Behaviour**

1. **Choose a provider.** With no provider named — by neither `--provider` nor
   `EGGPASS_PROVIDER` — present an interactive picker over every provider the
   build knows about:

   ```
   $ eggpass login

     Select a provider to sign in with:

       ❯ x.ai
         ChatGPT

     ↑/↓ or j/k to move · Enter to select · Esc to cancel
   ```

   - `↑`/`↓` **and** `j`/`k` both move the cursor; `Enter` selects; `Esc` and
     `Ctrl-C` cancel.
   - Cancelling exits `0` and changes nothing.
   - When stdin is **not** a terminal (piped input, CI, cron), the picker
     falls back to a numbered list and reads one line:

     ```
     Select a provider to sign in with:
       1) x.ai
       2) ChatGPT
     Provider [1-2]:
     ```

   - `--device` is accepted only for x.ai. Choosing ChatGPT with `--device` is
     an error, because the ChatGPT path has no documented device flow.

2. **Sign in.** The flow is per provider:
   - **x.ai — loopback (default):** bind a **random ephemeral** port on
     `127.0.0.1`, generate PKCE verifier/challenge plus `state` and `nonce`,
     open the browser at the authorize URL, and wait up to **10 minutes** for
     the callback on `http://127.0.0.1:<ephemeral>/callback`.
   - **x.ai — device (`--device`):** request a device code, print the user code
     and the verification URL, and poll every `interval` seconds (default 5)
     until approved, denied, or expired (30 minutes).
   - **ChatGPT — loopback:** bind a random ephemeral port on `127.0.0.1` and
     use the path `/auth/callback` — Sign in with ChatGPT requires the path to
     match exactly, and only the port may vary. On a **first** sign-in the
     client id is `dynamic_agent_client` and OpenAI issues the real client id
     during the callback; that issued id is stored. On a **re-login** the
     saved issued client id is reused, so no second registration is created.
3. Exchange the code for tokens and write them to the provider's credential
   file under `~/.config/eggpass/credentials/` (mode `0600`).
4. Print success, the account identifier, and when the access token expires.

**Re-login replaces.** Signing in to a provider that is already logged in
**replaces** the stored credential. It never fails with "already logged in",
and it never creates a second record for the same provider. The write is
atomic, so an interrupted re-login leaves the previous credential intact.

There is no `--force` flag: replacement is the only behaviour.

The OAuth callback port is **independent of the API port**. Login works while
the daemon is running and does not require the daemon to be stopped.

**Exit codes:** `0` success or cancelled · `1` denied/expired/failed ·
`2` `--device` requested for a provider that has no device flow.

---

## `eggpass logout`

Revoke and remove stored OAuth credentials.

```
eggpass logout
eggpass logout --provider xai   # pick the provider directly, no picker
```

**Behaviour**

1. **Choose a provider.** With no provider named — by neither `--provider` nor
   `EGGPASS_PROVIDER` — present the same picker as `login`, but listing **only
   the providers that are actually logged in**. A provider with no stored
   credential is not shown at all.

   ```
   $ eggpass logout

     Select a provider to sign out of:

       ❯ x.ai

     ↑/↓ or j/k to move · Enter to select · Esc to cancel
   ```

   With no provider logged in anywhere, print "not logged in" and exit `0`
   **without** showing the picker.

2. **Recycle the credential.** Revoke it at the provider before deleting it:
   - **x.ai:** `POST https://auth.x.ai/oauth2/revoke` with the refresh token.
     This is the `revocation_endpoint` published in x.ai's OIDC discovery.
   - **ChatGPT:** `POST` to the `revocation_endpoint` from
     `https://auth.openai.com/.well-known/openid-configuration` with
     `token=<refresh_token>`, `token_type_hint=refresh_token` and the issued
     client id. An empty `200` is success.
3. Delete the provider's credential file.
4. Stop the running daemon? **No.** The daemon stays up and will begin
   returning `401` for that provider until the next login. `logout` prints a
   notice saying so.

Revocation is **best-effort**. If the network call fails, the local credential
is still deleted, and the command says the remote revocation was not
confirmed — the token then lapses on its own, and the user can also disconnect
the app from the provider's own settings page.

**Exit codes:** `0` always (a failed revocation is not an error) · `1` the
credential file could not be removed.

---

## `eggpass status`

Report account state and daemon state.

```
eggpass status
```

**Output** — both sections, always, so the output is stable for scripting:

```
Accounts:
  x.ai      logged in
    email:    david@example.com
    user id:  01J...
    token:    valid, expires in 5h 12m
  ChatGPT   logged in
    email:    david@example.com
    plan:     plus (plan usage enabled)
    token:    valid, expires in 47m

Server:   running
  pid:       51234
  url:       http://127.0.0.1:8484
  upstream:  api.x.ai
  models:    10 available
```

```
Accounts:
  x.ai      logged out
  ChatGPT   logged out

Server:   stopped
```

Each provider appears in a fixed order so the output stays diffable. The
`models` line is only meaningful for the provider serving inference; it is
omitted when no provider is logged in.

**Exit codes:** `0` always. Parseability matters more than status codes here —
`status` is a human-facing command.

---

## `eggpass run`

Start the server as a background daemon.

```
eggpass run
eggpass run --foreground    # run in the foreground instead (debugging)
```

**Behaviour**

1. If a daemon is already running, print its PID and URL and exit `0`
   **without** starting a second one.
2. Otherwise spawn a detached child, write its PID to
   `~/.config/eggpass/eggpass.pid`, redirect stdout/stderr to
   `~/.config/eggpass/eggpass.log`.
3. Wait for the child to bind the port and answer `/health` (up to ~10s).
   If it does not come up, print the last lines of the log and exit `1`.
4. Print the base URL that clients should use:
   `http://127.0.0.1:8484/v1`

**Exit codes:** `0` running · `1` failed to start.

---

## `eggpass stop`

Stop the running daemon.

```
eggpass stop
```

**Behaviour**

1. Read the PID file. If the PID is not alive, remove the stale PID file, print
   "not running", and exit `0`.
2. Send `SIGTERM`, wait up to 10 seconds for a graceful exit.
3. If still alive after the grace period, send `SIGKILL`.
4. Remove the PID file.

**Exit codes:** `0` stopped or was not running · `1` could not be stopped.

---

## `eggpass restart`

Stop, then start.

```
eggpass restart
```

**Behaviour** — `stop` then `run`, in that order. Prints the new PID and URL.
A restart is the point at which a changed `port` takes effect.

**Exit codes:** `0` running · `1` failed to restart.

---

## `eggpass port`

Show or change the listen port.

```
eggpass port            # print the current port
eggpass port 9000       # change it
```

**Behaviour**

- With no argument, print the current port and the URL.
- With an argument, validate it (integer, 1–65535, not a privileged port below
  1024), write it to the config file, and report that it takes effect on the
  next restart.
- If the daemon is running and the port changed, print:
  `port set to 9000 — run 'eggpass restart' to apply (currently serving 8484)`.
- Changing the port **does not** restart the daemon.

**Exit codes:** `0` ok · `1` invalid port.

---

## `eggpass edit`

Open the config file in the user's editor.

```
eggpass edit
```

**Behaviour**

1. Create the config file with defaults if it does not exist.
2. Open it with `$EDITOR`, falling back to `$VISUAL`, then to a sensible
   default (`helix` if installed, else `vim`, else `nano`).
3. When the editor exits, validate the file and report any parse errors
   without overwriting the user's file.

**Exit codes:** `0` ok · `1` editor failed or config invalid.

---

## `eggpass croncheck`

Designed for a `crontab` entry — start the server only if it is not already
running.

```
eggpass croncheck
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
*/5 * * * * /usr/local/bin/eggpass croncheck >> ~/.config/eggpass/cron.log 2>&1
```

---

## Files

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/eggpass/config.toml` | User-editable config | `0644` |
| `~/.config/eggpass/credentials/xai.json` | x.ai OAuth tokens | `0600` (dir `0700`) |
| `~/.config/eggpass/credentials/openai.json` | ChatGPT OAuth tokens, issued client id, host id | `0600` (dir `0700`) |
| `~/.config/eggpass/eggpass.pid` | Daemon PID | `0644` |
| `~/.config/eggpass/eggpass.log` | Daemon + cron log | `0644` |

`~/.grok/auth.json` (written by the official Grok Build CLI) is read as an
**import source** when `~/.config/eggpass/credentials/xai.json` is absent, so a
user who already ran `grok login` can start using the proxy without logging in
again.

## Config file

```toml
# ~/.config/eggpass/config.toml

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
