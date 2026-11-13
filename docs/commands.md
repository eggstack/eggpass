# Commands

Every command is idempotent, safe to run from cron, and prints a line you can
read. Nothing is ever asked for twice.

## Global options

| Option | Meaning |
| --- | --- |
| `--config <PATH>` | Use this config file instead of `~/.config/eggpass/config.toml`. |
| `--provider <ID>` | Skip the interactive picker. `xai` or `openai`. Must come **before** the command. |
| `--help`, `-h` | Help for the command. |
| `--version`, `-V` | The version. |

```sh
eggpass --provider xai login      # no picker
eggpass --config ./local.toml run
```

---

## `eggpass login`

Authenticate with a provider over OAuth.

```sh
eggpass login
eggpass login --provider xai
eggpass login --device            # x.ai only
eggpass login --oauth             # browser redirect instead of device code
```

| Option | Meaning |
| --- | --- |
| `--device` | Use the device-code flow. Headless. x.ai only. |
| `--oauth` | Use the browser redirect flow. Mutually exclusive with `--device`. |
| `--provider <ID>` | Sign in to this provider without the picker. |

**Exit codes:** `0` success or cancelled · `1` denied/expired/failed.

### The picker

With no `--provider`, `eggpass login` opens an interactive picker listing the
providers the build knows about:

```
$ eggpass login

  Select a provider to sign in with:

  Use j/k or ↑/↓ to navigate, Enter to select, q/Esc to quit

    > x.ai
        Grok models, on your x.ai subscription
      ChatGPT
        ChatGPT models, on your ChatGPT plan

```

`j`/`k` and the arrow keys both move the cursor, and the cursor stops at the
ends rather than wrapping. `q` or `Esc` cancels, and `Ctrl-C` interrupts.
When stdin is not a terminal the picker falls back to a numbered list.

Signing in to a provider you are already signed in to simply **replaces** the
stored credential. There is no `--force` flag and no "already logged in"
refusal.

`eggpass logout` uses the same picker, listing only the providers you are
actually signed in to.

### The xAI sign-in page says "Grok Build"

It does, and it will. xAI has no general-purpose public OAuth client for
third-party apps, so `eggpass` signs in with the official Grok Build CLI's
public client id — the same public credential every Grok Build install
carries. xAI's consent page takes the product name from that client id, so it
introduces itself as Grok Build.

What we *do* control is the `referrer` parameter, which xAI documents as the
"OAuth usage attribution" value. We send `eggpass`, so the usage is attributed
to eggpass even though the page says Grok Build. xAI's own CLI sends `user`,
and OpenCode sends `opencode`.

If the page shows a **code to copy** instead of redirecting to your browser's
callback, paste that code into the terminal and press Enter — the login is
waiting for it, exactly as the official CLI does.

### The OAuth callback

The callback listens on a **random ephemeral loopback port**, chosen per
login. It has nothing to do with the API port, so you can log in while the
daemon is running, and you can log in on a machine where the API port is
already taken.

---

## `eggpass logout`

Revoke the credential at the provider, then delete the local file.

```sh
eggpass logout
eggpass logout --provider openai
```

**Exit codes:** `0` always — a failed revocation is not an error · `1` the
credential could not be read or removed.

The daemon keeps running and answers `401` until you log in again.

---

## `eggpass status`

Show the account state and the daemon state.

```sh
eggpass status
```

**Exit codes:** `0` always. Parseability matters more than status codes here.

It reports, per provider, whether you are signed in, the account email, the
account id, and — for ChatGPT — whether plan usage was granted. It also
reports the daemon: running or not, the address and port **actually being
served**, and the auth state:

```
auth: open — no key configured
auth: 2 key(s)
auth: the key from the configuration file
auth: 1 key(s), the key from the configuration file
```

---

## `eggpass run`

Start the server as a background daemon.

```sh
eggpass run              # background
eggpass run --foreground # stay in the terminal
```

**Exit codes:** `0` running · `1` failed to start.

The daemon is self-contained: no launchd, no systemd. It is found by its PID
file at `~/.config/eggpass/eggpass.pid`, which records the pid, the host it
bound and the port it serves.

If the daemon is bound outside loopback with no client key configured, it logs
an error at startup — every request will be refused until `eggpass key add` is
run.

---

## `eggpass stop` · `eggpass restart`

```sh
eggpass stop
eggpass restart
```

**Exit codes:** `stop` — `0` stopped or was not running · `1` could not be
stopped. `restart` — `0` running · `1` failed to restart.

---

## `eggpass port` · `eggpass host`

Show or change the address the server listens on.

```sh
eggpass port            # show the port being served
eggpass port 9000       # move to 9000 and restart
eggpass host            # show the bind address
eggpass host 0.0.0.0    # listen on every interface and restart
```

**Exit codes:** `0` ok, including "already serving" · `1` invalid port, port
taken, invalid host, host refused, or the restart failed.

Both commands only restart when the requested value actually differs from
what is *being served*, read from the PID file rather than from the config —
so a config file that was edited by hand does not cause a pointless restart.

Both refuse to change anything if the new address is unusable:

- `eggpass port 9000` when something else is on 9000 prints
  `port 9000 is taken` and leaves the server alone. The port is checked
  **before** anything is stopped.
- `eggpass host 0.0.0.0` without a key refuses, and says so:
  *"refusing to bind 0.0.0.0: issue a key with `eggpass key add` first"*.
  An issued key counts, as does the `api_key` in the config file.

If the restart fails, the old configuration is rolled back and the server
comes back up where it was.

`host` defaults to loopback, and that default is the safe one: binding
elsewhere puts the proxy, and through it your paid subscriptions, on the
network. `local`, `localhost` and `loopback` all mean `127.0.0.1`.

A server bound outside loopback with no key at all **refuses every request**
rather than serving as an open proxy.

---

## `eggpass key`

Issue, list and revoke the keys your clients use. You never bring your own —
`eggpass` mints them.

### `eggpass key add [LABEL]`

```sh
$ eggpass key add laptop

Created key #1.

    sk-eggpass-3nQ7fB2xWvKcR8dTmZaYhLpEuJqXf1s

This is the only time the key is shown — only its hash is stored.
Send it as:
    Authorization: Bearer sk-eggpass-3nQ7fB2xWvKcR8dTmZaYhLpEuJqXf1s
```

The label is optional and is only your own bookkeeping. One key may serve
several clients.

**Exit codes:** `0` created · `1` the store could not be written.

### `eggpass key list`

```
  #  key                  label          created
  1  sk-eggpass-3nQ7fB2x   laptop        2026-11-13T21:30:00Z
  2  sk-eggpass-qR7xZ9mT   phone         2026-11-13T21:44:12Z
```

Only the visible prefix is shown — the rest of the key is not recoverable,
because only a salted hash of it is stored. The command also notes when the
config file's `api_key` is set as well, since that key is honoured too.

### `eggpass key revoke <N>` · `eggpass key revoke --all`

```sh
eggpass key revoke 2
eggpass key revoke --all
```

**Exit codes:** `0` ok · `1` the key number does not exist, or the store could
not be written.

Key numbers are stable: they are never reused and never renumbered, so
`revoke 2` always means the same key even after other keys have been revoked.

Revoking the **last** key says what it did, and which way:

```
That was the last key. The endpoint on 127.0.0.1 now accepts anything.
That was the last key. The endpoint on 0.0.0.0 now refuses every request.
```

A key takes effect on the **next request** — the server re-reads the store
whenever it changes, so nothing has to be restarted.

---

## `eggpass edit`

Open the config file in `$EDITOR`.

```sh
eggpass edit
```

**Exit codes:** `0` ok · `1` the editor failed or the config is invalid.

The file is validated when the editor exits.

---

## `eggpass croncheck`

Start the daemon only if it is not already running.

```sh
eggpass croncheck
```

**Exit codes:** `0` in all normal cases — running or successfully started.

**It prints nothing.** If the server is already up, the command does nothing
and says nothing. That is the point: it is meant to be run from cron without
filling a mail spool.

```cron
*/5 * * * * /usr/local/bin/eggpass croncheck >> ~/.config/eggpass/cron.log 2>&1
```

---

## `eggpass update`

Update to the latest released version, or to one exact version.

```sh
eggpass update            # the latest published release
eggpass update 0.2.0      # one exact version — which is also the rollback
eggpass update --check    # report what would happen, change nothing
```

**Exit codes:** `0` updated, or `--check` completed · `1` the release is
unavailable, the digest did not match, the candidate failed to prove its
version, or this installation may not be updated.

How the verification works is in [updating.md](updating.md).

A running server keeps the old binary until it is restarted. `eggpass update`
says so rather than restarting for you, because an implicit restart during an
update would cut live streams.

### Cargo-managed installations

`eggpass` refuses to replace a binary that Cargo owns. If you installed with
`cargo install`, the updater prints the `cargo install` command to run
instead — `cargo install --path .` for a local checkout, or
`cargo install eggpass --git https://github.com/eggstack/eggpass` for the
repository. Replacing Cargo's copy by hand would leave `cargo install --list`
and `cargo uninstall` lying.
