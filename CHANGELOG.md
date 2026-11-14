# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-13

The first release. A local, OpenAI-compatible proxy for Grok and ChatGPT that
authenticates with your own subscription over OAuth.

### Added

- **A local OpenAI-compatible API.** `POST /v1/chat/completions` (streaming
  and non-streaming), `POST /v1/responses`, `GET /v1/models`,
  `GET /v1/models/{id}` and an unauthenticated `GET /health`. Both
  `/v1/chat/completions` and `/chat/completions` work, so a client whose base
  URL already carries the `/v1` needs no change.

- **OAuth login for two providers.** x.ai, through the public Grok Build
  client id, and ChatGPT, through *Sign in with ChatGPT for Open Source*. The
  OAuth callback listens on a random ephemeral loopback port, so you can log
  in while the daemon is running and on a machine whose API port is taken.

- **A device-code flow for x.ai.** The sign-in prints a short code and a URL
  and can be approved from any device, which is what makes it work over SSH
  and on a headless machine. `eggpass login --oauth` uses the browser redirect
  instead.

- **An interactive provider picker.** `eggpass login` and `eggpass logout` open
  a picker rendered through `nix` termios, with a numbered-list fallback when
  stdin is not a terminal. `--provider <id>` skips it entirely.

- **A live model catalogue.** `GET /v1/models` is the union of every provider
  you are signed in to, fetched live. Nothing is compiled in, so a model no
  provider advertises is a `404` rather than a hopeful `200`.

- **Routing by model name.** The longest matching prefix in
  `routing.prefixes` wins, and an unmatched model goes to
  `routing.default_provider`.

- **Chat Completions translation for ChatGPT.** Sign in with ChatGPT speaks
  the Responses API, so a Chat Completions request is converted on the way in
  and the answer converted back on the way out. x.ai needs no translation.

- **Issued client keys.** `eggpass key add` mints a key, shows it exactly
  once, and stores only a salted hash of it. `eggpass key list` shows the
  numbers, `eggpass key revoke <n>` takes one away, and `--all` takes every
  one. A key takes effect on the next request, with no restart.

- **`eggpass port` and `eggpass host`.** Each shows or changes the address
  the server listens on and restarts it in one step. Both refuse to change
  anything if the new address is unusable — a taken port prints
  `port <port> is taken` — and both roll the configuration back if the
  restart fails.

- **`eggpass croncheck`.** Starts the daemon only if it is not already
  running, and prints nothing either way, so it is safe to run from cron.

- **`eggpass update`.** The eggstack family's `eggup` crates perform the
  update: the release manifest is read, the SHA-256 of what was downloaded is
  checked, the candidate must prove its own version, and only then is the
  binary replaced — under a lock, with a rollback.

- **A self-contained daemon.** PID-file based, with no launchd unit and no
  systemd unit, and no service manager to configure.

### Fixed

- **A thinking level now reaches the model.** A Chat Completions request sets
  the level with `reasoning_effort`; the Responses API rejects that spelling
  outright, so it was being dropped and the model used its own default. It is
  now renamed to `reasoning.effort`, and the upstream's reasoning summary is
  asked for so the thinking has somewhere to go.

- **The thinking comes back.** The upstream's reasoning summary is carried in
  `reasoning_content` — the field x.ai's Chat Completions answer uses — so a
  client shows the thinking for both providers, in a streamed answer as well as
  a complete one. The ChatGPT route decides for itself whether to write a
  summary, so the field is absent when there is none to show; the level is
  applied either way.

- **The model catalogue says what a model can do.** The ChatGPT entries were
  reduced to a name, which left a client with nothing to offer the user. They
  now carry the upstream's own data: the thinking levels a model offers, its
  context window, whether it takes images. x.ai's entries already carried
  theirs.

- **The catalogue answers every spelling at once.** The two providers publish
  that information under different names — ChatGPT as
  `supported_reasoning_levels`, x.ai as `capabilities.reasoning_effort` — and
  the clients that read it use yet others. Each entry now also answers
  `reasoning`, `capabilities.reasoning.effort`, `default_reasoning_level`,
  `limit.context` and `name`, beside the provider's own fields. Nothing is
  claimed that a provider does not publish.

- **An upstream error keeps its status, type and code.** A request for a model
  the provider does not serve came back as a `502 server error`, which reads as
  "try again" for something that cannot succeed. It is now the upstream's own
  `400 invalid_request_error` with the code `model_not_found`.

- **A token limit is no longer thrown away.** `max_output_tokens` was on the
  list of fields the Sign in with ChatGPT route rejects; it accepts it. A
  client's `max_tokens` is now carried over as `max_output_tokens` instead of
  being silently dropped.

### Security

- The client's credentials are stripped before a request is forwarded, and the
  provider's token is put in their place. The provider-owned headers are
  stripped too, so a client cannot make the proxy claim to be a different
  build.
- Provider credentials are stored per provider in `~/.config/eggpass/credentials/`,
  mode `0600` inside a `0700` directory, and never leave the machine.
- A server bound outside loopback with no client key **refuses every request**
  rather than serving as an open proxy. Binding a non-loopback address at all
  requires a key.
- Secrets are never logged. Token values are redacted everywhere.

### Notes

- The project was renamed from `grokproxy` to `eggpass`. An installation that
  predates the rename is **copied** into `~/.config/eggpass` — nothing is
  moved and nothing is deleted — and a `migrated-to-eggpass` marker is left
  behind.

[Unreleased]: https://github.com/eggstack/eggpass/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/eggstack/eggpass/releases/tag/v0.1.0
