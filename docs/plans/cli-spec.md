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
`logout`, `usage`). It is the non-interactive path into every command: with it
set, the picker is never drawn and stdin is never read, so the command behaves
the same whether it is run from a terminal, a pipe or cron. An unknown
provider id is an error, not a silent fallback.

---

## `eggpass login`

Authenticate with a provider over OAuth.

```
eggpass login              # interactive provider picker, then the login flow
eggpass login --provider xai   # pick the provider directly, no picker
eggpass login --device     # x.ai only: device-code flow, the default
eggpass login --oauth      # x.ai only: browser loopback flow
```

**Behaviour**

0. **Which flow.** The **device-code flow is the default** wherever the
   provider has one, and the browser loopback flow is the fallback.

   This is not a preference. xAI's browser flow for the Grok Build client id
   lands on a page that says *"Copy the code below into Grok Build to finish
   signing in"*, which is confusing — and xAI does not publish the
   device-code endpoint to every client. So:

   - the device flow is tried first, because it works from any device, needs
     no browser on this machine and no loopback port, and never shows that
     page;
   - if the device endpoint answers **404**, the browser flow is used
     automatically, with a note saying so.

   The official Grok CLI does exactly this — *"auth: device flow unavailable
   (404), falling back to loopback login"* — and OpenCode registers the
   device method for xAI and nothing else.

   `--device` asks for the device flow explicitly, `--oauth` for the browser
   flow, and the two together are a usage error. ChatGPT has no device flow,
   so it always signs in with the browser.

1. **Choose a provider.** With no provider named — by neither `--provider` nor
   `EGGPASS_PROVIDER` — present an interactive picker over every provider the
   build knows about:

   ```
   $ eggpass login

     Select a provider to sign in with:

     Use j/k or ↑/↓ to navigate, Enter to select, q/Esc to quit

       > x.ai
           Grok models, on your x.ai subscription
         ChatGPT
           ChatGPT models, on your ChatGPT plan

   ```

   - `↑`/`↓` **and** `j`/`k` both move the cursor. The cursor **stops at the
     ends** of the list rather than wrapping around.
   - `Enter` selects; `q` and `Esc` cancel.
   - `Ctrl-C` **interrupts** the command: the terminal is restored and the
     command exits non-zero. That is deliberately not the same as cancelling,
     which is a choice and exits `0`.
   - The screen is **cleared and the menu redrawn from the top** on every
     keystroke, so the menu never scrolls a second copy of itself.
   - Cancelling exits `0` and changes nothing.
   - The interactive picker needs **both** stdin and stdout to be terminals. A
     terminal on stdin with a pipe on stdout would put cursor-control
     sequences into the pipe, so that combination uses the fallback too.
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
   - **The code may also be pasted back.** xAI sometimes shows a code to copy
     on the consent page instead of redirecting to the loopback — its page
     says *"Copy the code below into Grok Build to finish signing in"*. The
     official Grok CLI waits for the code from **"loopback + stdin"**, and so
     does this: while the loopback listener is open, a code pasted into the
     terminal is picked up and exchanged. A pasted line that does not look
     like an authorization code is ignored and the wait continues.
     Only a **terminal** stdin is watched — reading a pipe would swallow the
     caller's input and would block a scripted login forever.
     This is available for x.ai, whose client id is fixed. It is **not**
     available for ChatGPT, which issues a client id of its own and returns it
     in the callback: a pasted code carries no issued id to exchange.
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
`2` `--device` requested for a provider that has no device flow, or
`--device` and `--oauth` given together.

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

     Use j/k or ↑/↓ to navigate, Enter to select, q/Esc to quit

       > x.ai

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
  auth:      2 key(s)
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
omitted when no provider is logged in. The `auth` line says how the endpoint
is protected: `open — no key configured`, `N key(s)`, or `the key from the
configuration file`.

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
   - if there is no key at all — neither one issued by `eggpass key add` nor an
     `api_key` in the config — the change is refused, naming the command that
     has to be run first;
   - if there is a key, the change is allowed and the command warns that the
     proxy is now reachable from the network.

   A server that is already bound outside loopback and then loses its last key
   refuses every request rather than serving as an open proxy.
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
not bindable, no key configured for a non-loopback host, or the restart
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

## `eggpass update`

Update to the latest published release, or to one exact release.

```
eggpass update              # update to the latest release
eggpass update 0.2.0       # update to exactly this version
eggpass update --check      # report what would happen; change nothing
```

**Behaviour**

The update itself is done by `eggup`, the eggstack family's verified updater.
`eggpass` supplies the *policy* — which release to install, what the target
triple is, and what the installed binary must prove about itself — and `eggup`
supplies the *mechanics*: fetch, verify, validate, replace. The same mechanism
every other eggstack tool uses.

1. **Resolve the current executable** and classify how it is installed:
   - **self-managed** — a binary the user installed, which `eggup` may replace.
     The transaction proves ownership by hashing the destination and requiring
     it to match the digest observed before the transaction began; the hash is
     re-read under `eggup`'s lock.
   - **cargo-managed** — installed by `cargo install`. The command **refuses**
     and prints the `cargo install` command to run instead. Replacing a
     Cargo-managed binary behind Cargo's back would desynchronise the two.
   - **unprovable** — neither. The command refuses rather than guess.
2. **Refuse forbidden provenance before asking the release host anything.**
   When local facts alone make the mutation impossible, no network call can
   change the answer.
3. **Fetch the release manifest** from
   `https://github.com/<repository>/releases/latest/download/<manifest_name>`,
   or `.../releases/download/v<version>/<manifest_name>` for an exact version.
   The manifest names the published artifacts and their sizes and SHA-256
   digests.
4. **Project the manifest** for this host's canonical Rust target triple
   (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
   `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`).
   Aliases such as `linux-x64` are **not** accepted — the triple in the
   manifest has to be the exact one.
5. **Never downgrade.** A build newer than the published release is a local or
   unreleased build, and the catalogue is not entitled to replace it. If the
   installed version already equals the published one, the command says so and
   changes nothing.
6. **Download and verify.** The artifact's size and SHA-256 must match the
   manifest. The digest is an **integrity** check, not an authenticity check:
   it proves the download was not corrupted, not that it came from us. `eggpack`
   publishes no signature and claims none.
7. **Validate the candidate.** The downloaded binary is run with `--version`
   and its stdout compared, exactly, against `eggpass <version>\n`. A binary
   that does not print exactly that is not installed.
8. **Commit.** Only after validation does the transaction replace the
   installed binary. If the transaction fails, the receipt's disposition
   decides what happens: rolled back, or recovery required — in which case the
   previous binary is still on disk and its path is printed.
9. **Never restart the daemon.** A running server keeps the old binary until
   it is restarted, and a restart cuts live streams. The command says so and
   points at `eggpass restart`.

**Output**

```
$ eggpass update --check
eggpass 0.1.0 is installed; 0.2.0 is published.
  target: aarch64-apple-darwin
  asset:  eggpass-aarch64-apple-darwin
  owner:  self-managed
```

```
$ eggpass update
Updated eggpass 0.1.0 to 0.2.0.
  target: aarch64-apple-darwin
  asset:  eggpass-aarch64-apple-darwin
The running server still has the old binary until it is restarted.
Run `eggpass restart` when you are ready.
```

**Exit codes:** `0` updated, or `--check` completed · `1` the release is
absent, the version is already current, a downgrade was requested, the
provenance forbids the mutation, the digest did not match, the candidate did
not validate, or the transaction failed.

---

## `eggpass key`

Issue, list and revoke the keys that clients use to reach the endpoint.

```
eggpass key              # list the keys
eggpass key list         # the same
eggpass key add          # mint a key and show it once
eggpass key add laptop   # mint a key and give it a label
eggpass key revoke 2     # revoke the key numbered 2
eggpass key revoke --all # revoke every key
```

**Behaviour**

1. **`eggpass` issues the key.** The user never brings their own: `add` mints a
   key from the system CSPRNG and shows it **exactly once**. There is no
   `key set` — the point of the command is that the key is `eggpass`'s to
   make.
2. **Only the hash is stored.** The store holds a per-key random salt and the
   SHA-256 of `salt || key`, plus the visible prefix, the optional label and
   the creation time. The key itself is never written to disk, so the file
   cannot be read to impersonate a client. The file is `0600` in a `0700`
   directory, and is written atomically.
3. **Keys are numbered, and the number is stable.** The first key is `#1`, the
   next `#2`, and so on. A number is never reused and never renumbered when
   another key is revoked, so `revoke 2` always means the same key. The number
   is what `revoke` takes, and it is what `list` prints.
4. **A key is not tied to one client.** One key may be used by several clients.
   The optional label is for the user's own bookkeeping — `laptop`, `phone` —
   and `list` shows it.
5. **`list` shows only the visible part** — the first 20 characters, which is
   the fixed `sk-eggpass-` marker plus a few random characters. That is enough
   to tell two keys apart in a list and not enough to use one.
6. **A key takes effect on the next request.** The server re-reads the store
   whenever the file changes underneath it, so `add` and `revoke` need no
   restart. The store has exactly one writer — this command — so the file is
   never contended.
7. **Authentication is required when there is a key.** While the store is empty
   and the config's `api_key` is empty, the endpoint accepts anything, which is
   the safe default for a loopback socket. The moment a key exists, a key is
   required. Revoking the last one puts the endpoint back to open, and the
   command says so.
8. **The config's `api_key` still works.** It is the older, hand-written way
   to set one key, and it is accepted alongside the issued ones. It is not
   removed, because a configuration file that suddenly stops parsing is worse
   than a second way in.

**Output**

```
$ eggpass key add laptop
Created key #1.

    sk-eggpass-3nQ7fB2xWvKcR8dTmZaYhLpEuJqXf1s

This is the only time the key is shown — only its hash is stored.
Send it as:
    Authorization: Bearer sk-eggpass-3nQ7fB2xWvKcR8dTmZaYhLpEuJqXf1s
```

```
$ eggpass key list
  #  key                  label          created
  1  sk-eggpass-3nQ7fB2x   laptop        2026-11-13T21:30:00Z
  2  sk-eggpass-qR7xZ9mT   phone         2026-11-13T21:44:12Z

Revoke one with `eggpass key revoke <number>`.
```

```
$ eggpass key revoke 2
Revoked key #2 (sk-eggpass-qR7xZ9mT).
```

**Exit codes:** `0` ok · `1` the key number does not exist, the store could not
be read or written, or the arguments were wrong.

---

## `eggpass usage`

Report how much of the subscription allowance is used.

```
eggpass usage                  # every provider you are signed in to
eggpass usage --provider xai   # one provider
eggpass usage --json           # the report as JSON
```

**Behaviour**

1. **One fresh read per provider.** Each provider is asked once, on this run,
   and the command ends. There is no polling, no cache, and no background
   refresh: a cached reading is a number that might be wrong, and this command
   exists to be right.
2. **No daemon is needed.** The credential file is read and the provider is
   asked directly, exactly as `status`'s model count does. The command works
   with the proxy stopped.
3. **No state of its own.** `usage` writes no cache, no config and no key
   store. It may *trigger* a token refresh — that is the `TokenManager`'s job,
   and it writes the credential file — but it adds nothing of its own.
4. **A provider that is not signed in still gets a block**, so the output does
   not change shape, and no network call is made for it.
5. **A provider that fails still gets a block**, and the failure is stated
   rather than swallowed. The upstream body is never printed: it carries the
   account's email, its user id and its balances.

**Output** — one block per provider, in the fixed provider order, a blank line
between blocks, no colour:

```
x.ai · SuperGrok
  Weekly      ████░░░░░░░░░░░░░░░░  20% used
  Resets      Nov 20, 03:07 (UTC)
  Api             ██░░░░░░░░░░░░░░░░░░  10% used
  Prepaid     $0.00
  This is your Grok subscription. `eggpass` requests are billed separately against the xAI API tier.

ChatGPT
  Usage       unavailable — Sign in with ChatGPT does not expose a quota API
  Limits      https://chatgpt.com/settings/usage
  Signals     429 subscription_sharing_usage_limit_exceeded
              503 subscription_sharing_usage_unavailable
              403 subscription_sharing_user_not_eligible
```

**The rules the output follows**

- **The label column is twelve characters wide**, so every value starts in the
  same place.
- **The bar is twenty cells**, `█` filled and `░` empty. A non-zero
  percentage always shows at least one cell, and the bar is omitted entirely
  when the percentage is unavailable.
- **The number is the amount *used*.** It reads `20% used`, never `20% left`.
- **Missing is not zero.** The Grok billing endpoint is proto3 JSON and omits
  zero-valued scalars, so an absent percentage means the provider did not
  say. It reads `unavailable`, never `0% used`.
- **An explicit state replaces the percentage.** When the provider says the
  account is not eligible, that is the answer and no number is shown.
- **The reset time is local**, with its offset, because `Nov 18, 09:00` alone
  is ambiguous. The year appears only when it differs from the current year.
  A reset more than an hour in the past is marked `(stale)`.
- **The caption is not optional.** On the default `api` upstream, `eggpass`'s
  inference is metered against the xAI API tier and does **not** draw from
  the consumer subscription the billing endpoint reports. The caption says
  which pool the number belongs to, and it changes with
  `providers.xai.upstream`.
- **ChatGPT is unavailable, and says why.** Sign in with ChatGPT exposes no
  quota API, so the block names the authentication mode, links OpenAI's own
  usage page, and lists the three error codes OpenAI does publish. It never
  infers a number.

**Exit codes:** `0` at least one provider produced a block · `1` the arguments
were wrong, or every provider failed operationally · `2` no provider is signed
in on this machine.

---

## Files

| Path | Purpose | Mode |
| --- | --- | --- |
| `~/.config/eggpass/config.toml` | User-editable config | `0644` |
| `~/.config/eggpass/keys.json` | The keys `eggpass` issued — salted hashes, never the keys | `0600` |
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

### Migration from the old name

The config directory used to be `~/.config/grokproxy`. On startup `eggpass`
**copies** the old directory's `config.toml`, `credentials/`, `eggpass.pid` and
`eggpass.log` into `~/.config/eggpass`, and leaves a `migrated-to-eggpass`
marker in the old directory.

The migration **copies rather than moves**, and never deletes anything the
user owns. The old directory is left in place — it is the user's data, and
deleting it is their call, not the tool's. The migration never overwrites an
existing `~/.config/eggpass`, and it is idempotent: running it twice changes
nothing the second time.

## Config file

```toml
# ~/.config/eggpass/config.toml

host = "127.0.0.1"  # the address to bind; "localhost"/"local"/"loopback" mean 127.0.0.1
port = 8484
api_key = ""          # empty = accept any key (safe only while the host is loopback);
                      # `eggpass key add` issues keys of its own, and those are
                      # accepted alongside this one
log_level = "info"    # error | warn | info | debug
allow_privileged_port = false

[timeouts]
connect_seconds = 30    # how long establishing a connection may take
global_seconds = 1800   # the ceiling on a whole request
body_seconds = 1800     # the ceiling on reading the response body

[server]
max_connections = 256       # simultaneous connections; beyond it the server answers 503
idle_timeout_seconds = 120  # how long a kept-alive connection may sit idle
write_timeout_seconds = 600 # how long a single write may stall

[update]
repository = "eggstack/eggpass"
manifest_name = "eggpass-release-manifest.json"

[providers.xai]
enabled = true
upstream = "api"      # "api" = api.x.ai  |  "build" = cli-chat-proxy.grok.com

[providers.openai]
enabled = true

[routing]
default_provider = "xai"

[routing.prefixes]
"grok-" = "xai"
"gpt-"  = "openai"
"codex" = "openai"

[oauth.xai]
client_id = "b1a00492-073a-47ea-816f-4c329264a828"
issuer = "https://auth.x.ai"
scopes = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write workspaces:read workspaces:write"
referrer = "eggpass"

[oauth.openai]
issuer = "https://auth.openai.com"
scopes = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"
agent_name_hint = "eggpass"
resource = "https://api.openai.com/v1"
```

`host` and `port` are the two addresses the server listens on, and they are
changed with the `host` and `port` commands rather than by hand-editing this
file — both commands validate, apply and restart in one step. Editing them by
hand works too; the change takes effect at the next `eggpass restart`.

The top-level `upstream` key is the legacy spelling of
`providers.xai.upstream` and is still read, but the per-provider table wins.

The flat `[oauth]` table is the legacy spelling of `[oauth.xai]` and is still
read; the per-provider tables win.

`scopes` may be written either as a space-separated string or as a list of
strings.

**Unknown keys are preserved** when `eggpass port` and `eggpass host` rewrite
the file. Both perform a surgical text edit on the one key they change rather
than a parse-and-reserialise round trip, so comments and keys the tool does
not understand survive.

## Upstream behaviour

- Base URL: `https://api.x.ai` when `providers.xai.upstream = "api"`.
- Base URL: `https://cli-chat-proxy.grok.com` when
  `providers.xai.upstream = "build"`, in which case the request additionally
  carries `X-XAI-Token-Auth: xai-grok-cli` and
  `x-grok-client-version: <version>`.
- The base URL is the **origin**, never the origin plus `/v1`: the request path
  carries the `/v1` prefix itself, so a base URL ending in `/v1` would send
  every request to `/v1/v1/...`.
- Incoming `Authorization` / `X-Api-Key` headers from the client are **stripped**
  and replaced with the provider's OAuth token, so client credentials never
  reach the provider.

## Exposed endpoints

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/chat/completions` | OpenAI-compatible chat completions (streaming + non-streaming) |
| `POST /v1/responses` | OpenAI-compatible Responses API |
| `GET /v1/models` | The live model catalogue, merged across signed-in providers |
| `GET /v1/models/{id}` | Single model |
| `GET /health` | Liveness probe (no auth required) |

`GET /v1/models` is a **live round trip**. There is no static catalogue: the
union of the signed-in providers' catalogues is what is advertised, and a
model id no provider advertises is a `404`.

**Authentication.** Every endpoint except `/health` needs a key when one is
configured. The key may arrive as `Authorization: Bearer <key>`, as
`X-Api-Key: <key>`, or as a `key` query parameter, and may be one of the keys
`eggpass key add` issued or the `api_key` in the config file. A request
without an accepted key is a `401` in the OpenAI error shape. The client's key
is stripped before the request is forwarded upstream.
