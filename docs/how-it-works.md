# How it works

## The request path

```
client ──OpenAI API──► eggpass ──OAuth bearer──► api.x.ai / api.openai
```

1. The client sends an OpenAI-shaped request to `127.0.0.1:8484`.
2. `eggpass` checks the client key, if one is required.
3. The model name decides the provider.
4. The request is translated if the provider needs it.
5. The client's credentials are stripped, and the provider's token is put in
   their place.
6. The answer is translated back and streamed to the client.

## The pieces

- **Login** uses OAuth 2.1 authorization code + PKCE. The callback uses a
  random ephemeral loopback port, so you can log in while the daemon is
  running. The device-code flow is available for headless machines.
- **x.ai** is reached at `https://auth.x.ai` with the public Grok CLI client
  id. The Grok Build proxy (`upstream = "build"`) is available as an
  alternative.
- **ChatGPT** is reached through *Sign in with ChatGPT for Open Source* at
  `https://auth.openai.com`. The first sign-in registers the installation and
  receives its own client id; later sign-ins reuse it. The route is the public
  `https://api.openai.com/v1` one — the internal Codex backend is not used.
- **Requests are routed by model name.** The longest matching prefix in
  `routing.prefixes` wins; an unmatched model goes to
  `routing.default_provider`.
- **Chat Completions are translated for ChatGPT.** Sign in with ChatGPT speaks
  the Responses API, so a Chat Completions request is converted on the way in
  and the answer converted back on the way out. x.ai needs no translation.
  The conversion renames the two fields the Responses API spells differently —
  `reasoning_effort` becomes `reasoning.effort` and `max_tokens` becomes
  `max_output_tokens` — because the Responses API rejects the Chat Completions
  spellings outright.
- **Your client's credentials never leave the machine** — they are stripped
  before the request is forwarded, and the provider's token is put in their
  place. The provider-owned headers are stripped too, so a client cannot make
  the proxy claim to be a different build.
- **A client key is required to run.** `eggpass key add` mints a key, shows it
  exactly once, and stores a salted hash of it — never the key itself. The
  server re-reads the store whenever it changes, so a key can be added or
  revoked without a restart. `eggpass run`, `restart` and `croncheck` refuse
  to start without one, and a running server refuses every request if its
  last key is revoked — except `GET /health` and `GET /v1/last-inference`,
  which answer without a key because they carry nothing but a version string
  and a status code. Loopback was never the boundary: a web page can reach
  `127.0.0.1`, so "no key" meant "open to any page the user visits".
- **The authority is checked even with a key.** On a loopback bind the `Host`
  header must be `127.0.0.1`, `localhost`, `[::1]`, a `*.localhost` name, or
  the bound address. A request carrying anything else — the shape a DNS
  rebinding attack produces, or a proxy that forgets to rewrite `Host` — gets
  a `421 Misdirected Request` that names the address it should have used.
- **Tokens refresh themselves** five minutes before they expire. A refresh
  that returns no new refresh token keeps the old one, so a rotating provider
  cannot lock you out. Each provider has its own credential file and its own
  refresh lock.
- **Streaming is not buffered.** Each chunk is flushed as it arrives, so the
  first token shows up when the model produces it. The upstream's rate-limit
  headers reach the client on the streaming path too.
- **Updates are verified.** `eggpass update` reads the release manifest,
  checks the SHA-256 of what it downloaded, makes the candidate prove its own
  version, and only then replaces the binary — under a lock, with a rollback.

## The model catalogue

`GET /v1/models` is a round trip to every provider you are signed in to, and
the answer is their union. Nothing is compiled in.

That has two consequences worth knowing:

- A model that no provider advertises is a `404`, not a hopeful `200`.
- The catalogue changes when the providers change. There is nothing to update
  here when xAI or OpenAI ship a new model.

Each entry carries what the provider says about the model, not just its name:
the thinking levels it offers, its context window, whether it takes images.
That is what lets a client tell the user which thinking levels are on offer
without a hardcoded list of its own. See
[Thinking levels](#thinking-levels) below.

The two providers publish that information differently — ChatGPT as
`supported_reasoning_levels`, x.ai as `capabilities.reasoning_effort` — and
clients read it under yet other names. So each entry answers all of them:

| Field | Meaning |
| --- | --- |
| `id`, `object`, `owned_by` | The OpenAI shape. |
| `name`, `display_name` | The model's display name. |
| `reasoning` | Whether the model offers thinking levels. |
| `capabilities.reasoning.effort` | The thinking levels, as a plain list. |
| `default_reasoning_level` | The level the model uses by default. |
| `limit.context`, `context_length`, `context_window` | The context window. |
| `supported_reasoning_levels` | ChatGPT's own description of its levels. |

The provider's own fields are carried through untouched and the aliases are
added beside them, so nothing is lost. A field a provider says nothing about
is left out rather than guessed at: the x.ai models that publish no
`capabilities` block carry no reasoning flag at all.

## Thinking levels

A thinking level is the amount of reasoning a model does before it answers.
Both providers name it, and each in its own way.

- **x.ai** takes `reasoning_effort` on `/v1/chat/completions` and answers with
  the thinking in `reasoning_content`.
- **ChatGPT** speaks the Responses API, which takes `reasoning.effort` and
  answers with a reasoning *item*.

Both publish the levels they offer in their catalogue, under different names,
and the catalogue answers both.

`eggpass` makes the two look the same to a client:

- A Chat Completions request that sets `reasoning_effort` has it renamed to
  `reasoning.effort` for ChatGPT, and the upstream's reasoning summary is asked
  for as well. The Responses API rejects the Chat Completions spelling, so
  without the rename the level would be silently dropped and the model would
  use its own default instead.
- The answer's thinking comes back in `reasoning_content`, the field x.ai
  uses, whether it arrived as a summary or as streamed deltas.

x.ai always returns its thinking. The ChatGPT route decides for itself whether
to write a summary, so the field is there when there is thinking to show and
absent when there is not — the level is applied either way.

The levels each model accepts are the upstream's business, and the catalogue
says which they are under `supported_reasoning_levels`. A level the model does
not offer is refused by the upstream, and that refusal comes back to the client
as the upstream's own error.

## Tool calls

A model that can use tools asks for one, the client runs it, and the result goes
back into the conversation. The two providers spell that differently.

- **x.ai** speaks Chat Completions natively, so a tool call is already the shape
  a client expects: `message.tool_calls`.
- **ChatGPT** speaks the Responses API, where a tool call is an output *item*
  of type `function_call` — announced with its name and id, then filled in as
  its arguments arrive.

`eggpass` translates the second into the first, so a client sees the same thing
whichever provider answered:

- The upstream's `function_call` becomes `message.tool_calls`, carrying the
  `call_id` the client matches the tool's answer against, and the finish reason
  becomes `tool_calls`.
- While streaming, the call is announced with its id and name, and its arguments
  arrive as they do.
- When the client sends the model's own tool call back with the tool's answer,
  it is carried into the conversation as a `function_call` item, so the upstream
  can place the answer next to the call it belongs to.

That last step matters more than it looks. Without it the tool's answer arrives
with nothing to attach to, and the conversation quietly loses the tool's result
— the model answers as though it had never been told.

## Errors

An error from a provider keeps its status, its type and its code. A request for
a model the provider does not serve comes back as a `400` with the code
`model_not_found`, not as a blanket `502 server error` — the difference
between "this cannot work" and "try again".

## The auth rules

The rules are decided by the **live host**, not by the config file:

| Bound to | No key configured | A key is configured |
| --- | --- | --- |
| Loopback (`127.0.0.1`) | Open to anything on the machine | The key is required |
| Anywhere else | **Every request is refused** | The key is required |

So `eggpass` can never be an open proxy for the network. On loopback, "open"
means open *to this machine only*, which is why it is the default.

"Refused" means every endpoint that does any work. Two routes are exempt on
every bind, because they carry nothing worth protecting: `GET /health` returns
the version string that `status` and `croncheck` probe, and
`GET /v1/last-inference` returns the status of the last inference to each
provider so that `eggpass usage` can report it. Neither accepts input, neither
touches a credential, and neither says anything about a prompt or an answer.
The authority guard still runs on both, so a rebound request is refused with
`421`.

A server bound outside loopback with no key at all logs an error at startup
and refuses every request, rather than quietly serving the world.

## Design notes

- One binary, one config file, no database, no cloud, no admin API.
- Loopback only. Your provider credentials never leave the machine.
- No async runtime: blocking sockets, one thread per connection.
- The HTTP server is part of this project rather than a dependency, because
  the obvious choice (`tiny_http`) buffers responses in a one-kilobyte
  `BufWriter`, which would hold back the first token of every streamed
  answer.
- The updater is the eggstack family's `eggup` crates rather than a bespoke
  one, so `eggpass` behaves like every other tool in the family. That raised
  the MSRV from 1.85 to 1.89.
- The provider picker runs in raw mode through `nix`, matching the family's
  own terminal selector. `crossterm` was tried first and reversed: it
  rendered the menu as a staircase.
- **The daemon keeps no history of requests.** The only thing it remembers
  between requests is the HTTP status of the last inference to each provider,
  and it holds that in memory — no file, no log, no timestamps. It is there
  so `eggpass usage` can say something about a provider that publishes no
  quota, and it is gone when the daemon stops. A record that outlived the
  process would be a history of what the user asked the model.

## Changing where it listens

`eggpass port` and `eggpass host` change the address the server listens on
and restart it in one step:

```sh
eggpass port 9000       # move to port 9000 and restart
eggpass host 0.0.0.0    # listen on every interface and restart
```

Both commands only restart when the value actually differs from what is
being served, and both refuse to change anything if the new address is
unusable — a taken port prints `port <port> is taken` and leaves the server
alone. If the restart fails, the old configuration is restored and the server
comes back up where it was.

`host` defaults to loopback, and that is the intended shape. To reach the
proxy from another machine, forward the port over SSH rather than widening
the bind — it keeps the proxy on the interface it was designed for:

```sh
ssh -L 8484:127.0.0.1:8484 box
```

`eggpass host 0.0.0.0` works — this is a tool for people who expect to be able
to widen it — and it still requires a key, and warns when it allows one. Two
things change when you widen it: the authority guard is skipped, because the
names a LAN address is reachable by are not knowable from the config, and the
key becomes the only boundary. The daemon itself only ever speaks plain HTTP,
so a tunnel is also what keeps the traffic off the wire.

## The daemon

The daemon is self-contained. It is found by its PID file at
`~/.config/eggpass/eggpass.pid`, which records the pid, the host it bound and
the port it serves. There is no launchd unit and no systemd unit, and there
is no service manager to configure.

`eggpass croncheck` is the supported way to keep it alive: it starts the
daemon only if it is not already running, and prints nothing either way.
