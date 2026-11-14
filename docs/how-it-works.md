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
- **Client keys are issued by `eggpass`.** `eggpass key add` mints a key,
  shows it exactly once, and stores a salted hash of it — never the key
  itself. The server re-reads the store whenever it changes, so a key can be
  added or revoked without a restart.
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

## Thinking levels

A thinking level is the amount of reasoning a model does before it answers.
Both providers name it, and each in its own way.

- **x.ai** takes `reasoning_effort` on `/v1/chat/completions` and answers with
  the thinking in `reasoning_content`.
- **ChatGPT** speaks the Responses API, which takes `reasoning.effort` and
  answers with a reasoning *item*.

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

`host` defaults to loopback, and that default is the safe one: binding
elsewhere puts the proxy, and through it your paid subscriptions, on the
network. A non-loopback host is refused unless you have a key — one issued by
`eggpass key add`, or the `api_key` in the config file — and warns when it is
allowed.

## The daemon

The daemon is self-contained. It is found by its PID file at
`~/.config/eggpass/eggpass.pid`, which records the pid, the host it bound and
the port it serves. There is no launchd unit and no systemd unit, and there
is no service manager to configure.

`eggpass croncheck` is the supported way to keep it alive: it starts the
daemon only if it is not already running, and prints nothing either way.
