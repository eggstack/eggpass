# Troubleshooting

## Sign-in and keys

**`not logged in` or credentials cannot refresh:** Run `eggpass login` for
the affected provider. Sign-in replaces that provider's saved credential.

**`refusing to run: no client key is configured`:** Add a key with
`eggpass key add`, or set `api_key` in the config file. The client must send
the key with each protected request.

**Proxy returns `401`:** Check `eggpass status`, then compare the client's key
with `eggpass key list` or the configured `api_key`. If the response says no
client key is configured, add a key. It takes effect on the next request.

**Browser did not open during sign-in:** The login prints the URL. Open it
manually, or use `eggpass login --provider xai --device` for x.ai device-code
sign-in.

**The x.ai sign-in page shows a code to copy:** Paste the code into the
terminal and press Enter.

## Server and network

**`421` or a host authority error:** Use `http://127.0.0.1:8484` as the host.
If a reverse proxy is in front, make sure it sends an accepted `Host` header.
Requests addressed to another host are rejected on loopback binds.

**`address already in use` or `port 9000 is taken`:** Another process is using
the requested port. Choose another port with `eggpass port 9001`. The command
checks the port before restarting the server.

**The server runs but does not answer on the configured port:** Run
`eggpass status` to see the host and port it is actually serving. Use
`eggpass port` or `eggpass host` to apply a configuration change.

**Remote client cannot connect:** The server binds to loopback by default.
Use an SSH tunnel, for example `ssh -L 8484:127.0.0.1:8484 box`, instead of
exposing the API directly.

## Requests and models

**Streaming appears buffered:** Test with `curl -N` to disable curl's output
buffering. Provider response time can also affect when the first chunk arrives.

**A request returns `model_not_found`:** Fetch `GET /v1/models` with your client
key and choose an ID from the response. The catalogues are fetched live, and a
provider may omit models that are unavailable or filtered during normalization.

**A client shows no reasoning levels:** Check whether the model entry in
`GET /v1/models` lists them, and whether the client reads those fields.

**A reasoning level is rejected:** Use a level listed for that model. Providers
can reject levels they do not support.

**`unsupported_content_part`:** The request carries a content part the
translated path cannot express. Use `text`, `image_url`, `input_audio`, or a
file part with `file_id` or `filename` and `file_data`; a non-text part in a
`system` message or a non-text `tool` output is refused by design. A part type
this proxy does not translate can still be sent to `POST /v1/responses`, which
is forwarded verbatim.

**`unsupported_parameter`:** The named Chat Completions field has no Responses
equivalent. `stop`, `seed`, `n` greater than 1, penalty fields with a non-zero
value, `logit_bias`, and `logprobs` are examples. Remove the field, or send a
native Responses request.

**`image_requires_responses` or `audio_requires_responses`:** An image or audio
generation tool was requested over Chat Completions, whose answer shape cannot
carry the generated media. Call `POST /v1/responses` instead.

**`missing_model`:** The body must be a JSON object with a `model` string. A
non-JSON or model-less body is no longer forwarded to the default provider.

**`body_too_large` (HTTP 413):** The body exceeds `server.max_body_bytes`
(32 MiB by default). Inline base64 media counts toward the limit. Raise the
setting within its documented range if the request is legitimate.

**`unexpected_media_content_type`:** A native media endpoint answered with a
content type its provider spec does not list. The response is refused rather
than labelled wrongly; check that the endpoint and the probe result match.

**`unmappable_output_item`:** The upstream returned an item, such as a
generated image or audio, that the Chat Completions shape cannot express. Retry
the request against `POST /v1/responses`.

**An agent does not run a tool:** Check that the response contains
`message.tool_calls` and that the client handles OpenAI Chat Completions tool
calls. For the tool result, the client must send the assistant's tool call and
the matching `tool_call_id` back in the conversation.

## Usage and updates

**ChatGPT usage is unavailable:** Sign in with ChatGPT does not expose a quota
API to eggpass. Use the link shown by `eggpass usage` to view account usage.

**`eggpass update` refuses to replace the binary:** The installation is
Cargo-managed. Run the `cargo install` command printed by the updater instead.

**`eggpass update` says the release is already current:** There is no newer
published release to install.

**The running version is newer than the published release:** The running build
may be a local or unreleased version. The updater will not replace it with an
older published build unless you name that version explicitly.

## More help

| Need | Read or run |
| --- | --- |
| Server and login state | `eggpass status` |
| Daemon log | `~/.config/eggpass/eggpass.log` |
| Issued keys | `eggpass key list` |
| Command details | [commands.md](commands.md) |
| Config settings | [configuration.md](configuration.md) |
| Request path and authentication | [how-it-works.md](how-it-works.md) |
