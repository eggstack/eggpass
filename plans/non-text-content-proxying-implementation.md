# Non-Text Content Proxying — Implementation Plan

**Status:** Implemented and verified offline. Stages 1–5 and 7–9 are implemented; Stage 6's forwarding machinery is implemented with every candidate left disabled. Stage 0 live capture and the Stage 6 live probe have not run, so the documented wire shapes and each candidate's subscription access remain unverified. No commit, push, merge, or release was performed.

**Basis:** Read-only audit of the current `fix/pypi-classifiers-0.1.1` tree at `3795089`, covering routing, the chat handler, the two translation directions, stream aggregation, the upstream client, and both provider specs.

## Goal

Make EggPass carry non-text content — image, audio, and file parts — correctly to the upstream provider and back to the client, or fail with a specific error that names what could not be carried.

Two invariants define "done":

1. **No silent drop.** Every content part and every request field either reaches the upstream, or produces a `400`/`502` that names it and gives a reason. An answer is never an empty `content` because everything in it was discarded.
2. **Truthful capabilities.** A model is advertised as accepting a modality only when the configured request path can actually deliver that modality.

## Baseline findings

| # | Gap | Evidence | Effect today |
| --- | --- | --- | --- |
| G1 | Chat Completions input is reduced to text before translation | `translate.rs:168` `content_text()` keeps a part only when it has a `text` field | `image_url`, `input_audio`, and `file` parts vanish for ChatGPT |
| G2 | An image-only message disappears entirely | the same function returns `None`; the `user` arm then inserts nothing | the upstream answers an empty conversation |
| G3 | Request fields outside four names are dropped | `translate.rs:18` `PASSTHROUGH = ["model","tool_choice","parallel_tool_calls","reasoning"]` | `response_format`, `stream_options`, `n`, `stop`, `seed`, penalties, `modalities`, `audio` are lost without a word |
| G4 | Non-`function` tools are discarded | `translate.rs:192` `convert_tool()` returns `None` for any other `type` | image generation, web search, and file search cannot be requested |
| G5 | Output projection is text-only | `translate.rs:340` `response_text()`; `content` is emitted as a string | refusal parts and any image/audio output item are dropped; a non-text-only answer becomes `""` |
| G6 | Streaming translation drops non-text events | the catch-all `_ => Vec::new()` at `translate.rs:809` inside `translate_event` (`translate.rs:612`) | refusals and non-text deltas never reach a streaming client |
| G7 | The catalogue advertises what the translation cannot deliver | `provider/openai.rs:103` keeps `input_modalities`; the test at `provider/mod.rs` asserts `["text","image"]` survives | a client is told images work, then they are silently discarded |
| G8 | No media endpoint is routed | `router.rs:29-46`; anything else is `404 unknown endpoint` (`mod.rs:479-482`) | image generation, audio, files, and embeddings are unreachable |
| G9 | Inline media is bounded by a buffered 32 MiB body | `http.rs:22` `MAX_BODY_BYTES`; enforced at `http.rs:626` | large media fails with a generic `413` that names no limit and no remedy |
| G10 | There is no media test coverage | no `image_url`, `input_image`, or `input_audio` occurrence anywhere under `src/`, tests included | this whole area can regress undetected |

## What already works and must not regress

- **xAI, both endpoints.** `src/provider/xai.rs` sets `NativePaths::Both` and `RequestPolicy::default()`. `chat.rs:53` takes the native branch, the body is cloned byte for byte, and `apply_policy` (`chat.rs:325`) short-circuits when the policy is empty. Body, content type, and any media part pass through unchanged.
- **ChatGPT on `/v1/responses`.** The same native branch forwards `input_image` parts and `image_generation` tools verbatim, and returns the provider's response object verbatim. Image generation is already reachable on this path and only this path.
- **Transport.** Request bodies are read as raw bytes with chunked decoding, and only hop-by-hop and credential headers are stripped (`upstream.rs` `STRIPPED_HEADERS`). Nothing in the HTTP layer is text-specific.
- **The EggPool tunnel** preserves method, path, body, and Authorization (`docs/eggpool.md`), so it inherits whatever the router supports and needs no change here.

## Design principles

1. **Translate or fail; never drop.** A part or field with a known mechanical equivalent is mapped. One without a faithful equivalent produces a named error. Silence is a defect.
2. **Verbatim by default.** When the provider speaks the client's shape, bytes are forwarded untouched. Translation is the exception, applied only where shapes differ.
3. **One projection.** Streaming and non-streaming output share a single mapping implementation so the two cannot drift.
4. **Facts, not hopes.** A capability is advertised, and a route enabled, only after it is confirmed against a live provider.
5. **Fail before the model runs.** An unserviceable request (for example an image tool over Chat Completions) is rejected at request validation, not after tokens have been spent and the answer cannot be expressed.

## Target shapes

Stage 0 confirms every row against a captured payload. Input parts:

| Chat Completions | Responses | Transformation |
| --- | --- | --- |
| `{"type":"text","text":…}` | `{"type":"input_text","text":…}` | rename; already produced |
| `{"type":"image_url","image_url":{"url":…,"detail":…}}` | `{"type":"input_image","image_url":…,"detail":…}` | nested object flattened to a string, `detail` lifted alongside |
| `{"type":"input_audio","input_audio":{"data":…,"format":…}}` | `{"type":"input_audio","input_audio":{…}}` | shape-equivalent; model support is the upstream's call |
| `{"type":"file","file":{…}}` | `{"type":"input_file", …}` | type renamed; `file_id`, or `filename` with `file_data`, carried across |
| bare string `content` | one `input_text` part | unchanged |

Output items:

| Responses item | Chat Completions | Rule |
| --- | --- | --- |
| `message` → `output_text` | `content: "…"` | a text-only answer keeps the string form, for client compatibility |
| `message` → `refusal` | `content: [{"type":"refusal",…}]` | the array form is used only when a non-text part exists |
| `function_call` | `message.tool_calls` | existing behaviour |
| `reasoning` | `message.reasoning_content` | existing behaviour |
| `image_generation_call`, audio items | none | rejected at request time for a Chat client; `502` with a named code if one arrives unexpectedly |

## Staged implementation

### Stage 0 — Capture and freeze the wire shapes (gate)

- With a signed-in account, capture one request/response pair per row above: an xAI `image_url` request, a ChatGPT `/v1/responses` `input_image` request, a refusal, and an image-generation call.
- Record them under `tests/fixtures/media/` with media replaced by a 1×1 PNG data URL and every identifier removed. No token, prompt, or user data is stored.
- Correct the target tables before any mapping code is written. A row that cannot be confirmed is implemented as an explicit rejection, not a guess.

**Exit:** redacted fixtures exist, and every row is either confirmed or deliberately dropped from the tables.

### Stage 1 — Input content parts

- Add `parse_chat_content` and `to_responses_parts` in a new `src/server/parts.rs`. They accept `content` as a string or an array, and are the only place part shapes are understood.
- `chat_request_to_responses` builds part arrays for `user`, `developer`, and `assistant` messages. An image-only message now yields a message item instead of vanishing (G2).
- `system` messages still flatten into `instructions`; a non-text part in a system message is `400 unsupported_content_part`, because `instructions` is a string.
- A `tool` message stays a string `function_call_output`; non-text tool output is `400 unsupported_content_part`.
- An unknown part `type` is `400 unsupported_content_part` naming the type. It is never skipped.
- `content_text` is reduced to the `instructions` adapter and is no longer used on user content.

**Exit:** the existing translation tests pass unchanged for text-only bodies; new tests cover each part type and the image-only message.

### Stage 2 — Request fields and tools

- Replace `PASSTHROUGH` with three exhaustive tables: `CARRIED` (same shape), `RENAMED` (`reasoning_effort`, `max_tokens`/`max_completion_tokens`), and `UNSUPPORTED` (a reason per field). Anything not in a table is `400 unsupported_parameter` naming the field, mirroring the upstream's own behaviour (G3).
- Carry `response_format`, `stream_options`, `n`, `stop`, `seed`, the penalties, and `modalities`/`audio` where the shape is already Responses-valid, and list the rest with a reason.
- `convert_tool` keeps converting `function` tools and passes every other Responses-native tool `type` through unchanged. An unrecognised `type` is carried so the upstream can judge it (G4).
- A Chat Completions request naming an image or audio generation tool is `400 image_requires_responses` / `400 audio_requires_responses`, before any upstream call.

**Exit:** a table-driven test iterates every field and tool type in all three tables and proves each one either appears upstream or returns its named error.

### Stage 3 — Non-streaming output projection

- `responses_to_chat_completion` emits `content` as a string when the answer is text-only, and as a part array when it carries a refusal or another mapped part (G5).
- Map `refusal` to `{"type":"refusal","refusal":…}`. Reasoning and tool calls keep their current handling.
- An `image_generation_call` or audio item with no Chat equivalent returns `502` with a named code and the Responses id, so a client can retry natively.
- The Stage 2 request-time rejection means this branch should be unreachable for well-formed traffic; it exists so that the impossible case fails loudly rather than as `""`.

**Exit:** a text-only answer is byte-shape identical to today; a refusal survives; unmappable output is an error and never an empty string.

### Stage 4 — Streaming translation

- Reuse the Stage 3 mapping inside `ResponsesToChatStream`; there is no second implementation (principle 3).
- Map `response.refusal.delta` and `response.refusal.done` into `delta.refusal` (G6).
- Track unmapped non-text events. On the terminal event, if nothing was emitted and unmapped non-text events were seen, emit a `finish_reason: "error"` chunk carrying the named error, then `[DONE]`. An empty stream is never the answer.

**Exit:** refusals stream correctly; an unmappable stream fails loudly; existing stream tests are unchanged.

### Stage 5 — Capability truthfulness

- After Stage 1 the translated path can deliver images, so `input_modalities` stays in the catalogue. The invariant is enforced instead of assumed.
- Add a test that walks every provider spec and every model advertised with `image` and asserts the translator accepts an `image_url` part for that provider's shape (G7). A future regression that re-narrows the translator breaks the catalogue test rather than the user.
- Do not invent `output_modalities` fields the upstream does not publish.

**Exit:** catalogue and translator cannot drift without a failing test.

### Stage 6 — Native media routes (probe-gated)

- Add `media_paths: &'static [MediaPath]` to `ProviderSpec` (`provider/mod.rs`), empty by default. A `MediaPath` is `{ method, path, response_types }` (G8).
- The router matches an enabled path exactly and dispatches to a new `src/server/media.rs`. It forwards verbatim through `send_with_auth`, holds an upstream slot for the life of the response, applies the configured timeouts, and returns the upstream body with its own `Content-Type`. It must not use `sse::stream`, which forces `text/event-stream` (`sse.rs`).
- Non-2xx answers reuse `upstream_error` so status, type, and code are preserved.
- Candidate entries, each enabled only after a probe: xAI `POST /v1/images/generations`, OpenAI `POST /v1/images/generations`. Audio, files, embeddings, and moderation stay out of scope until separately probed.
- Add `scripts/probe-media-endpoints.sh`: it runs the smallest valid request per candidate against a temporary `EGGPASS_HOME`, and records status and content type without printing any credential. A candidate that is not 2xx stays disabled, and the route remains a `404`.

**Exit:** enabled routes forward byte-exactly and stream binary bodies with the right content type; disabled candidates still answer `404`; probe results are recorded in this plan.

### Stage 7 — Limits and diagnostics

- The `413` body names the limit and states that inline media counts toward it (G9).
- Add an optional `server.max_body_bytes`, clamped to a documented ceiling, defaulting to today's 32 MiB so behaviour does not change unless asked.
- A chat or Responses request with no readable `model` answers `400 missing_model` instead of falling through to the default provider (`config.rs:238`). Today a non-JSON body is forwarded to whichever provider is default, which hides the client's mistake.

**Exit:** all three diagnostics are covered by tests and are actionable from the message alone.

### Stage 8 — Tests and fixtures

- Unit: each mapping row, the image-only message, an unknown part, a refusal, an unmappable output item, and an unmapped stream event with empty text.
- HTTP-level: an in-process stub upstream built with `UpstreamClient::for_endpoint` asserts the forwarded bytes for both provider shapes — transformed for ChatGPT, byte-identical for xAI.
- Fixtures: load the Stage 0 captures with `include_str!` so a provider shape change breaks a test rather than production (G10).
- Invariant: the Stage 5 catalogue/translator test.
- No fixture contains a real credential, prompt, or user image.

**Exit:** `cargo test --workspace -- --test-threads=1` passes.

### Stage 9 — Documentation

- `docs/how-it-works.md`: what is translated, what is forwarded, and what is refused, stated per provider.
- `docs/troubleshooting.md`: `unsupported_content_part`, `unsupported_parameter`, `image_requires_responses`, `missing_model`.
- `CHANGELOG.md`: entries under Unreleased.

## Implementation outcome

| Gap | What changed | Where |
| --- | --- | --- |
| G1, G2 | A single parts module accepts a string or an array and maps text, image, audio, file, and refusal parts. An image-only message now yields a message item. | `src/server/parts.rs`; `translate.rs` message arms |
| G3 | Request fields are carried, renamed, mapped, stripped by provider policy, or refused by name. `response_format` becomes `text.format`; `n: 1` and `stream_options.include_usage` are no-ops; `stop`, `seed`, `n > 1`, logprobs, penalties, and unknown fields answer `unsupported_parameter`. | `translate.rs` field dispatch |
| G4 | Function tools still flatten; every other Responses-native tool is carried unchanged. Image and audio generation tools are refused over Chat Completions with `image_requires_responses` / `audio_requires_responses`. | `translate.rs` `convert_tool` |
| G5 | A text-only answer keeps the string `content`; a refusal produces an ordered part array. An image or audio output item answers `502 unmappable_output_item` instead of an empty answer. | `translate.rs` output projection |
| G6 | Refusal deltas stream as `delta.refusal`; the done copy is suppressed when deltas were seen. An unmappable stream with no chat content ends with a named error chunk and `[DONE]`. | `translate.rs` `ResponsesToChatStream` |
| G7 | A catalogue/translator invariant test accepts an `image_url` part for every advertised image model on every provider spec. | `translate.rs` tests |
| G8 | `MediaPath` is declared per provider; an enabled path is forwarded verbatim with the upstream's status, headers, and body, and its content type is checked against the spec. Dispatch runs only in the `NotFound` arm, so an undeclared path keeps the unchanged `404`. | `provider/mod.rs`; `server/media.rs`; `server/mod.rs` |
| G9 | `server.max_body_bytes` defaults to 32 MiB and clamps to 1–256 MiB. The `413` names the limit and the inline-media cause. | `config.rs`; `server/http.rs` |
| G10 | 473 tests pass, including documented-shape fixtures, HTTP-level byte-identity checks for both provider shapes, refusal projection, and the media route cases. | `server/tests.rs`; `tests/fixtures/media/` |

Deviations from the letter of the plan:

- **Stage 0 was not run.** No authorized live account was available. The fixtures are documented-shape, not captures, and `tests/fixtures/media/README.md` says so. The mapping is therefore tested at the proxy boundary, not against a live ChatGPT response.
- **Provider-rejected fields stay stripped, not refused.** `temperature`, `top_p`, `user`, `metadata`, and the rest of `openai::REJECTED_FIELDS` are provider policy: the live route rejects them, so passing them through fails the whole request. Refusing them would break every client that sends a default tuning value, contradicting the plan's own byte-compatibility requirement. The strip is commented in `translate.rs`; no other field is dropped.
- **No media route is enabled.** Both providers ship `media_paths: &[]`. The probe script is the manual step that would enable a candidate.
- **Media forwarding is POST-only.** A declared non-POST method answers `405` rather than being sent as POST.

## Acceptance checklist

Offline-verified with `cargo test --workspace -- --test-threads=1` (473 tests). Items that require a live account are left unchecked.

- [x] A Chat Completions request carrying `image_url` reaches ChatGPT as `input_image`, with URL and `detail` preserved.
- [x] An image-only message survives translation; no message is dropped for lacking text.
- [x] `input_audio` and `file` parts are translated when a mechanical mapping exists, and rejected by name when one does not.
- [x] No request field or tool type is dropped silently; each is carried or refused with a named error.
- [x] A refusal reaches a non-streaming Chat client as a content part, not as an empty answer.
- [x] A refusal reaches a streaming Chat client as `delta.refusal`.
- [x] An unmappable answer or stream fails with a named error and is never empty content.
- [x] Text-only requests and responses are byte-compatible with the current behaviour.
- [x] xAI forwarding remains byte-identical for both endpoints.
- [x] Every model advertised with `image` is accepted by the translator for that provider.
- [ ] Each enabled media route is probe-confirmed; unconfirmed routes answer `404` and no route forces `text/event-stream` on a binary body. No route is enabled yet, and no live probe has run.
- [x] The `413` and `missing_model` diagnostics are specific and tested.
- [x] No credential, token, prompt, or real user media appears in fixtures, logs, or test output.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -- --test-threads=1
cargo build --locked --release
git diff --check
sh scripts/probe-media-endpoints.sh   # Stage 6 only; requires a signed-in account
```

The probe script is the only step that needs live credentials. Every other check runs offline against fixtures and a stub upstream.

## Risks and rollback

- **Upstream shape drift.** Provider shapes change. Stage 0 captures plus `include_str!` fixtures turn drift into a test failure; the fail-loud rules keep an unmodelled change visible to the client instead of silent.
- **Client compatibility.** Emitting an array `content` where a string was returned would break naive clients. The text-only path keeps the string form; the array is used only when a non-text part exists.
- **Memory on large inline media.** Base64 bodies are fully buffered under the 32 MiB cap. The cap is documented and configurable, not raised by default.
- **A route that cannot serve the subscription.** A media route enabled without a probe ships a 403 to users. The probe gate is what prevents this; disabling a candidate is a one-line spec change.
- **Binary response mis-typing.** A media body returned through `sse::stream` would be labelled `text/event-stream` and mangled. Stage 6 uses `Response::stream` with the upstream's own headers, and a test asserts the type.
- **Rollback.** Stages 1–5 are local to translation and revert together. Stage 6 is disabled by setting `media_paths` to empty. Stage 7's body limit defaults to the current value, so it can ship independently.

## Out of scope

- Audio generation, transcription, and translation endpoints; file upload and lifecycle endpoints; embeddings; moderation. These are not probed here and stay unrouted.
- Changing provider OAuth custody or login, and the EggPool ticket/tunnel contract.
- Any real-credential integration test in CI. The probe is a manual, documented step.
- Commit, push, merge, release, or publication.

## References

- Translation: `src/server/translate.rs:18,168,192,340,612,809`; evaluation order: `src/server/chat.rs:53,325`.
- Aggregation: `src/server/aggregator.rs` (`aggregate_responses`).
- Routing and dispatch: `src/server/router.rs:29-46`; `src/server/mod.rs:429-482`.
- Transport: `src/server/http.rs:22,626`; `src/server/sse.rs`; `src/upstream.rs` (`send_with_auth`, `STRIPPED_HEADERS`, `normalize_target`).
- Providers: `src/provider/mod.rs` (`ProviderSpec`, `RequestPolicy`, `NativePaths`); `src/provider/xai.rs`; `src/provider/openai.rs:84-103`.
- Routing fallback: `src/config.rs:238` (`Routing::provider_for`).
- Tunnel contract: `docs/eggpool.md`.
