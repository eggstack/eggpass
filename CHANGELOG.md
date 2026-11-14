# Changelog

Notable changes to eggpass are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `eggpass usage` reads provider usage on demand. xAI usage is available where
  the account exposes it. ChatGPT usage is reported as unavailable because
  Sign in with ChatGPT does not provide a quota API. When the daemon has served
  an inference, the report can also show its latest client-facing HTTP status
  for each provider. This status is held in memory only; prompts and responses
  are not stored as history.
- `GET /v1/last-inference` returns only the latest client-facing status code
  for each provider. It does not require a client key. The authority check
  still applies on loopback.

### Security

- A client key is required to start the server. Protected API routes require a
  valid key. `/health` and `/v1/last-inference` remain unauthenticated.
- On loopback, requests with an unrecognized `Host` header are refused with
  `421 Misdirected Request`.
- The log redactor removes bare `?key=` values without matching names such as
  `monkey=`.

### Fixed

- Streaming preserves multi-byte characters split across socket reads, both
  for translated SSE and responses aggregated for non-streaming clients.
- Known endpoints return `405 Method Not Allowed` with an `Allow` header when
  called with the wrong method. Unknown paths still return `404`.
- Oversized or malformed chunked request lines are rejected without aborting
  the daemon. Authentication is checked before request bodies are retained,
  and health-check bodies are discarded.
- An OAuth callback with the wrong state no longer cancels an active login.
- Device-flow verification URLs are checked using browser-compatible URL
  parsing and exact trusted-host matching.
- Config files are written with mode `0600`. A key store that becomes
  unreadable is reported in the log rather than silently failing open.
- `EGGPASS_HOME` no longer imports credentials from `~/.grok/auth.json`.
- Revoking the last issued key reports that protected requests will be refused
  until a key is added again.

## [0.1.0] - 2026-11-13

First release of the local OpenAI-compatible proxy for Grok and ChatGPT.

### Added

- OpenAI-compatible endpoints for Chat Completions, the Responses API, and
  model discovery. Chat Completions can stream incrementally.
- OAuth sign-in for x.ai and ChatGPT. x.ai supports device-code sign-in for
  headless use; ChatGPT uses Sign in with ChatGPT for Open Source.
- Model-based provider routing, live model catalogues, and Chat Completions
  translation for ChatGPT.
- Client-key creation, listing, and revocation. Issued keys are shown once and
  stored as salted hashes.
- Daemon commands, address changes with rollback, and `croncheck` for scheduled
  startup.
- `eggpass update` for checksum-checked installation of published releases.

### Fixed

- ChatGPT reasoning levels and output-token limits are translated to the
  Responses API fields it accepts.
- ChatGPT tool calls are translated to the Chat Completions format and carried
  back with tool results.
- Provider model errors retain their useful status and error code.

### Security

- Client credentials are removed before requests are forwarded. OAuth tokens
  are sent to their respective providers over HTTPS and are not returned to
  clients or written to logs.
- OAuth credentials and issued-key hashes are stored in restricted files.
- The server refuses to bind outside loopback without a client key.

### Notes

- The project was renamed from `grokproxy` to `eggpass`. Existing data is
  copied to `~/.config/eggpass`; the old directory is left in place.

[Unreleased]: https://github.com/eggstack/eggpass/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/eggstack/eggpass/releases/tag/v0.1.0
