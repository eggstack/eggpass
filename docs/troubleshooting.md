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

**The model answers but the agent never runs a tool** — the answer comes back
with no `tool_calls` in it, so the client has nothing to run. A client that
drives tools needs the provider's tool calls translated into the Chat
Completions shape; check which provider serves the model with
`eggpass status`. x.ai answers with tool calls as they are, and ChatGPT's are
translated.

**The tool ran but the model ignored the result** — the tool's answer did not
make it back into the conversation with the call it belongs to, so the upstream
saw an answer it could not place. The client must send the model's own tool
call back along with the tool's result, in the order the model produced them.

**A ChatGPT request fails with `subscription_sharing_usage_limit_exceeded`** —
the ChatGPT plan's allowance is used up, or the account is not eligible for
plan usage. The error code comes from OpenAI; `eggpass status` shows whether
plan usage was granted to this installation.

**A request fails with `model_not_found`** — the model is not one the
provider serves. The Sign in with ChatGPT route offers the `gpt-6` and
`gpt-5.6` families; `gpt-5-codex` and `codex-mini-latest` are not among them,
and asking for one is answered with a `400` that says so. `GET /v1/models`
lists what is actually available.

**A client shows no thinking levels for a model** — the levels come from the
provider's catalogue, so the client has to be reading `/v1/models`. Some
clients only read a model's name and show nothing else; check what the client
did with the catalogue before suspecting the proxy. The levels a model does
offer are listed under `supported_reasoning_levels`.

**A thinking level is set but the model does not seem to think harder** — the
level is sent as `reasoning_effort`, which is renamed to `reasoning.effort`
for ChatGPT. Not every model takes every level: `none` and `minimal` are
valid in general but are refused by some models, and the refusal comes back
from OpenAI as an error.

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
