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
EGGPASS_HOST       the address to bind, overriding the config's host
EGGPASS_PORT       the port to listen on, overriding the config's port
```

`EGGPASS_HOST` and `EGGPASS_PORT` are read by the **daemon** when it starts,
not by the `host` and `port` commands — those commands change the config file,
and the config file is what survives a restart. They exist so a supervised
daemon can be pointed at a different address without editing the config.

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
  host:      127.0.0.1
  port:      8484
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
A restart is the point at which a hand-edited `port` or `host` in the config
file takes effect. The `port` and `host` commands do not need one — they
restart the daemon themselves.

**Exit codes:** `0` running · `1` failed to restart.

---

## `eggpass port`

Show or change the listen port. A change is applied **immediately** — the
command restarts the daemon for you.

```
eggpass port            # print the current port
eggpass port 9000       # change it and restart
```

**Behaviour**

1. **No argument** — print the configured port, the port actually being served
   (they differ only if the config was edited by hand), and the base URL.
2. **With an argument** — validate it, then apply:
   - **Valid** means an integer in `1..=65535`. A port below `1024` is refused
     unless `allow_privileged_port = true` in the config, matching the rule
     the server already applies at bind time.
   - An invalid port is refused **before anything is written or restarted**.
     Nothing changes and nothing is stopped.
3. **Only restart when the requested port differs from the port being served.**
   - If the daemon is running and already serving that port, it is **not**
     restarted. The config file is corrected to the requested value, so a
     later manual restart does not undo the change.
   - If the daemon is not running, the config is written and the message says
     the new port takes effect at the next start. There is nothing to restart.
4. **Check the port is free before restarting.** If the requested port is
   already taken, print `port <port> is taken` and change nothing — no config
   write, no stop, no restart — so the user can immediately try another port.
5. **Apply** — stop the daemon, write the config, start the daemon, wait for
   `/health` to answer (up to ~10s), then print the new URL and PID.
   If the start fails, the previous config is restored and the daemon is
   restarted on the old port, so a failed change never leaves the user with a
   stopped proxy.

**Output**

```
$ eggpass port 9000
port 9000 is taken
```

```
$ eggpass port 9000
restarting on port 9000…
running on http://127.0.0.1:9000 (pid 51234)
```

```
$ eggpass port 8484
already serving port 8484
```

**Exit codes:** `0` ok, including "already serving" · `1` invalid port, port
taken, or the restart failed.

---

## `eggpass host`

Show or change the address the server binds to. A change is applied
**immediately** — the command restarts the daemon for you.

```
eggpass host              # print the current bind address
eggpass host 0.0.0.0      # bind every interface
```

**Behaviour**

1. **No argument** — print the configured host and the host actually being
   served.
2. **With an argument** — validate it, then apply. Valid values are:
   - an IPv4 or IPv6 literal — `127.0.0.1`, `::1`, `0.0.0.0`, `::`;
   - `localhost`, `local` or `loopback`, which all mean `127.0.0.1`.

   Anything else is refused **before anything is written or restarted**. A name
   outside that list is refused rather than resolved: a bind address has to
   name a local interface, and resolving one would make the result depend on
   DNS.
3. **Only restart when the requested host differs from the host being served** —
   the same rule as `port`.
4. **The port is not touched.** `host` changes only the bind address; the
   configured port is left exactly as it is and the restarted daemon keeps
   serving the same port.
5. **A non-loopback host needs authentication.** Binding anywhere other than
   `127.0.0.1`/`::1` puts the proxy — and through it the user's x.ai and
   ChatGPT subscriptions — on the network. So:
   - if `api_key` is empty, the change is refused, naming the setting that has
     to be configured first;
   - if `api_key` is set, the change is allowed and the command warns that the
     proxy is now reachable from the network.
6. **Check the address is bindable before restarting.** If `host:port` cannot
   be bound, print `host <host> is not bindable` and change nothing.
7. **Apply** — stop, write the config, start, wait for `/health`, print the new
   URL. On failure, restore the previous config and restart on the old
   address.

**Output**

```
$ eggpass host 0.0.0.0
warning: the proxy is now reachable from the network
running on http://0.0.0.0:8484 (pid 51234)
```

**Exit codes:** `0` ok, including "already serving" · `1` invalid host, host
not bindable, no `api_key` configured for a non-loopback host, or the restart
failed.

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
| `~/.config/eggpass/eggpass.pid` | Daemon PID, the host it bound and the port it is serving | `0644` |
| `~/.config/eggpass/eggpass.log` | Daemon + cron log | `0644` |

The pid file records **what the daemon is actually serving**, not what the
config file says. `status`, `port` and `host` all compare against it, which is
how "only restart if the value actually changed" is decided. The host line is
optional when reading, so a pid file written by an older build still parses.

`~/.grok/auth.json` (written by the official Grok Build CLI) is read as an
**import source** when `~/.config/eggpass/credentials/xai.json` is absent, so a
user who already ran `grok login` can start using the proxy without logging in
again.

## Config file

```toml
# ~/.config/eggpass/config.toml

host = "127.0.0.1"  # the address to bind; "localhost"/"local"/"loopback" mean 127.0.0.1
port = 8484
upstream = "api"      # "api" = api.x.ai  |  "build" = cli-chat-proxy.grok.com
api_key = ""          # empty = accept any key (safe only while the host is loopback)
log_level = "info"    # error | warn | info | debug
```

`host` and `port` are the two addresses the server listens on, and they are
changed with the `host` and `port` commands rather than by hand-editing this
file — both commands validate, apply and restart in one step.

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
