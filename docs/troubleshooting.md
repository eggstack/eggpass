# Troubleshooting

**`not logged in — run 'eggpass login'`** — the credentials are missing or
expired and cannot be refreshed. Log in again.

**`401` from the proxy** — either you are logged out, or the client is
sending the wrong key. Check `eggpass status`, and check `eggpass key list`:
if it shows a key, the client has to send one of them.

**Every request is refused and the endpoint is bound outside loopback** — a
server bound to a non-loopback address with no key refuses every request
rather than serving as an open proxy. Issue one with `eggpass key add`, or set
`api_key` in the config file.

**`address already in use`** — something else is on the port. Pick another one
with `eggpass port 9000`; the command checks the port is free before it
restarts anything, and tells you so if it is not.

**`port 9000 is taken`** — the same thing, said more directly. Nothing was
changed and the server is still running where it was.

**The server says it is running but nothing answers** — the daemon is running
but its port has changed. `eggpass status` prints the port it is actually
serving. `eggpass restart` applies the configured port.

**The browser did not open for the login** — the URL is printed as well; open
it by hand, or use `eggpass login --provider xai --device`.

**The sign-in page shows a code to copy instead of redirecting** — paste that
code into the terminal and press Enter. The login is waiting for it, exactly
as the official Grok CLI does.

**Streaming feels slow** — check that the client is not buffering. `curl -N`
shows the raw stream.

**A ChatGPT request fails with `subscription_sharing_usage_limit_exceeded`** —
the ChatGPT plan's allowance is used up, or the account is not eligible for
plan usage. The error code comes from OpenAI; `eggpass status` shows whether
plan usage was granted to this installation.

**`eggpass update` refuses to replace the binary** — the installation is
Cargo-managed. Cargo owns that file and its bookkeeping, so replacing it
behind Cargo's back would leave `cargo install --list` and `cargo uninstall`
lying. The command prints the `cargo install` line to run instead.

**`eggpass update` says the release is already current** — the published
release matches the running version. Nothing to do.

**`eggpass update` says the running version is newer than the published one**
— the running build is a local or unreleased build, and the catalogue is not
entitled to replace it.

---

## Where to look next

| Question | Where |
| --- | --- |
| What is the server actually doing? | `eggpass status` |
| What is in the log? | `~/.config/eggpass/eggpass.log` |
| Which keys exist? | `eggpass key list` |
| What does the config say? | `eggpass edit` |
| What exactly does a command do? | [commands.md](commands.md) |
| How does routing work? | [how-it-works.md](how-it-works.md) |
