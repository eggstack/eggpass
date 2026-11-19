# How it works

`eggpass` runs an OpenAI-compatible HTTP API on your machine. It authenticates
to x.ai or OpenAI with your subscription credentials, then forwards inference
requests to that provider.

```text
client --client key--> eggpass --provider OAuth token--> x.ai or OpenAI
```

Prompts and responses leave your machine and are sent to the selected provider.
`eggpass` does not operate a cloud relay, store prompt or response history, or
log request bodies.

## Request flow

1. The client sends a request to `127.0.0.1:8484` with an `eggpass` client key.
2. `eggpass` checks the key and routes by model name. The longest matching
   prefix in `routing.prefixes` wins. Otherwise it uses
   `routing.default_provider`.
3. The proxy refreshes the provider's OAuth token if needed, removes client
   credentials, and forwards the request.
4. Chat Completions requests for ChatGPT are translated to the Responses API.
   x.ai Chat Completions requests are forwarded in their native format.
5. The provider response is returned to the client. Streaming responses are
   forwarded incrementally.

The Responses API is also available at `POST /v1/responses`.

## Sign-in and credentials

Sign-in uses OAuth Authorization Code with PKCE. x.ai supports a device-code
flow for headless machines. ChatGPT uses Sign in with ChatGPT for Open Source.
Each provider has a separate credential file with restricted file permissions.
Tokens are sent only to their respective provider and are not returned to
clients.

Client keys are separate from provider credentials. `eggpass key add` shows a
key once and stores only a salted hash. The key is checked locally on each
request. Add or revoke issued keys without restarting the server. The CLI's
usage request uses a separate private runtime Bearer credential, generated for
each daemon start and accepted only for a bodyless `GET /v1/usage`. It is not
stored in config or the regular key store, cannot authorize inference or other
routes, and cannot satisfy the regular-key startup requirement.

## Model routing and discovery

`GET /v1/models` fetches catalogues from signed-in providers and combines their
usable entries. Provider availability and normalization can affect which
models or fields appear. There is no compiled-in model list. Check the endpoint
for current model IDs.

The model name determines the provider. The default prefix map sends `grok-`
models to x.ai and `gpt-` and `codex` models to ChatGPT. Change the map in
`config.toml` if needed. See [configuration.md](configuration.md#core-settings).

## Authentication and local access

A client key is required to start the server. Configure an issued key or set
`api_key` in `config.toml`. The server requires a valid key on protected routes.
If the last issued key is revoked while the server is running, protected
requests are refused until a key is added.

`GET /health` and `GET /v1/last-inference` do not require a client key. Health
returns the version string. Last-inference returns only the last client-facing
HTTP status for each provider. Both endpoints still use the authority check on
loopback binds.

The server binds to loopback by default. A loopback port can be reached by web
pages, so the client key is required. To connect from another machine, use an
SSH tunnel. If you bind to a non-loopback address, a client key is still
required and the API uses plain HTTP. See [commands.md](commands.md) for the `host` command and remote access guidance.

## Streaming, errors, and usage

Streaming responses are forwarded as chunks arrive. The proxy does not buffer
the full answer before sending it to the client. Provider errors keep their
status codes and error details where supported.

The `eggpass usage` command first uses the running daemon's cached report and queries only providers enabled in configuration. It uses direct provider collection when the daemon is stopped or stale, the runtime usage credential file is missing or unreadable, or an older daemon returns `404`; a connection failure falls back only after a stopped/stale recheck. Authentication failures, malformed reports, and refresh/cooldown statuses are surfaced without bypassing the daemon. Stale cache is still displayed with a stderr warning. For ChatGPT, the OAuth flow has no quota endpoint, so usage is unavailable rather than estimated; when OpenAI is enabled, the optional local Codex app-server may provide a report from a separate account.

The authenticated `GET /v1/usage` endpoint returns the CLI-compatible JSON report. It caches one complete snapshot in memory for 10 minutes, starts at most one demand-triggered refresh, and never polls or writes usage data to disk. Its initiating request waits up to 30 seconds; if that deadline expires it gets `504` while the finite refresh continues. Concurrent requests get `202` with `Retry-After`. A refresh retries only timed-out providers, up to three times at 5/15/30-second intervals. Each xAI billing/settings request is capped at 15 seconds; OAuth refresh has a 60-second total deadline and Codex has an 8-second session deadline. Success adds age and update-time headers; after retry exhaustion, a previous successful snapshot is marked stale during a 10-minute cooldown, or `503` is returned if no good snapshot exists. A daemon restart clears the cache. The latest inference status is read afresh for every report response.

## Updates

`eggpass update` checks the release manifest and downloaded binary before
replacing the executable. It does not restart a running server. Read
[updating.md](updating.md) for the integrity limits and manual restart steps.
