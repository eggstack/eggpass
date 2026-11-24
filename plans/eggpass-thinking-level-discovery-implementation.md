# Eggpass Thinking-Level Discovery — Implementation Plan

**Status:** Implemented locally and validated. Changes are limited to code and documentation; no release, tag mutation, or package publication occurred.
**Research checked:** 2026-11-24 against the current workspace and published API documentation.
**Scope:** Make model reasoning/thinking-level metadata discoverable from Eggpass’s authenticated model catalogue, align the intended OpenRouter-compatible alias with its documented shape, and verify it end to end for Grok and ChatGPT.

## Goal

A client that reads Eggpass’s model catalogue should be able to determine, per model, whether reasoning effort is available, the accepted effort values, and a default effort when the provider publishes one. The response must preserve provider-native metadata, avoid inventing unsupported capabilities, and remain consumable by existing Eggpass clients.

## Findings and current behavior

- The configured base URL is `http://127.0.0.1:8484/v1`; the route for model discovery is `GET /v1/models`, not `GET /v1`. The route is authenticated with an Eggpass client key. Upstream OAuth sign-in for Grok/ChatGPT does not waive that local client-key check. An unauthenticated request returned `401`; a subsequent authenticated, read-only request returned HTTP 200 with 23 models, 15 of which included `reasoning.supported_efforts`. The key was supplied by the user and was neither printed nor written to disk.
- `GET /v1/models` fetches signed-in provider catalogues and normalizes them in `src/server/models.rs`. ChatGPT’s model-list allowlist in `src/provider/openai.rs` retains `supported_reasoning_levels`, `default_reasoning_level`, and related capability fields. Grok’s data-array catalogue retains its model fields, including its `capabilities` block. The normalization code derives common aliases in `src/provider/mod.rs`.
- The current alias labels itself OpenRouter-style but emits top-level `reasoning: true` and puts a list at `capabilities.reasoning.effort`. OpenRouter’s documented model metadata uses a top-level `reasoning` object, with `supported_efforts` and, when known, `default_effort`. A client following that contract will not find the current levels, and the boolean at `reasoning` conflicts with the expected object type.
- OpenAI’s documented Models API does not define a standard field for model reasoning levels. Preserving extensions is useful, but generic OpenAI-compatible clients are not required to read them or display an effort selector. Documentation must distinguish Eggpass’s advertised metadata from client-specific UI support.
- Request handling is already present: Chat Completions `reasoning_effort` is translated to Responses `reasoning.effort` for ChatGPT, while Grok’s Chat Completions request is passed through. This plan preserves that routing behavior and adds no new request syntax.

## Design decisions and constraints

1. Keep `/v1/models` as the discovery endpoint and keep its existing client-key requirement. Do not add a directory/index route at `/v1`; document the correct endpoint and authentication instead.
2. Preserve the upstream-native fields as received. For ChatGPT, keep the supported-level objects and native default fields; for Grok, keep its native `capabilities.reasoning_effort` and `default_reasoning_effort` values. Keep the ChatGPT allowlist bounded; do not expose internal instructions or arbitrary upstream payloads.
3. Replace the erroneous boolean alias with a top-level reasoning object. Its minimum useful contract is `reasoning.supported_efforts: string[]`, using the exact ordered values from the provider, plus `reasoning.default_effort` when an upstream default is known. Map the default from ChatGPT’s `default_reasoning_level` or Grok’s `capabilities.default_reasoning_effort`.
4. Do not infer `default_enabled`, `mandatory`, or any other behavioral flags from the mere presence of effort levels. First establish provider-supported semantics; emit those fields only when supported by source metadata or authoritative behavior. Do not claim full OpenRouter schema conformance if required semantics cannot be represented without guessing.
5. Preserve unknown/new effort labels rather than hardcoding today’s enum. Omit reasoning aliases when the provider reports no levels. Do not silently coerce, sort, or drop values; keep the upstream order unless a documented contract requires otherwise.
6. Avoid losing native metadata on key collisions. Define and test deterministic precedence for the canonical top-level `reasoning` object and any pre-existing provider `reasoning` field. Do not overwrite unrelated fields under `capabilities`.
7. Do not imply that every OpenAI-compatible client will render a control. The compatibility promise is that Eggpass exposes discoverable metadata and accepts the corresponding already-supported request shape; clients must understand the extension to show it.
8. No network inference calls, account changes, token/key creation, credential reads, publishing, release, or tag operations are required. Live verification, if available, is a read-only authenticated catalogue GET; never print the client key or dump credential files.

## Implementation stages

### Stage 0 — Confirm the metadata contract

- Recheck the published OpenRouter model metadata contract and both providers’ current catalogue fixtures. Determine whether the provider responses expose `default_enabled`/`mandatory` semantics; if not, leave them out and document the deliberate partial projection.
- Confirm the source-to-alias mapping: ChatGPT `supported_reasoning_levels[*].effort` and `default_reasoning_level`; Grok `capabilities.reasoning_effort[*]` and `capabilities.default_reasoning_effort`.
- Record the exact response contract in tests and client setup docs before changing normalization.

**Exit:** Every exposed value has an authoritative provider source, and no unsupported default/mandatory behavior is guessed.

### Stage 1 — Correct provider normalization

- Update `effort_levels` and `add_aliases` in `src/provider/mod.rs` to build the top-level reasoning object from either provider format.
- Preserve the provider-native source fields and default values. Keep deriving display/context aliases only from observed facts, as today.
- Replace the current boolean-shape test with assertions for the typed object and exact levels/default. Cover both ChatGPT and Grok, including empty/missing levels, unknown future labels, ordering, malformed entries, and collisions with pre-existing reasoning metadata.
- Keep the allowlist in `src/provider/openai.rs` narrow and add a test that internal prompt/instruction fields remain excluded.

**Exit:** Unit tests prove both provider formats produce the intended catalogue metadata without fabricating or dropping provider facts.

### Stage 2 — Exercise the public catalogue route

- Extend the server test harness with stub upstream catalogues for both provider shapes. Call authenticated `GET /v1/models` and assert the merged output contains each model’s native fields and corrected reasoning object.
- Exercise `GET /v1/models/{id}` as well, to ensure the single-model endpoint returns identical metadata.
- Assert that no-key requests remain unauthorized, disabled/unsigned providers contribute no entries, duplicate model IDs retain deterministic behavior, and catalogue failures do not create false reasoning claims.
- Keep these tests offline and deterministic; do not use the live test server as a unit-test dependency.

**Exit:** The public response path—not only the normalizer—preserves level metadata for both providers and retains existing authentication/merge behavior.

### Stage 3 — Protect request compatibility and document client behavior

- Retain tests proving that an advertised ChatGPT level is mapped from Chat Completions `reasoning_effort` to Responses `reasoning.effort`, and that the value is not lost. Add a stub-forwarding test for Grok to ensure its advertised effort value reaches the upstream unchanged.
- Do not introduce server-side level validation in this change unless it can use the same live/provider metadata safely and handle catalogue refresh/failure consistently. Upstream rejection remains the error for unsupported values otherwise.
- Update `docs/client-setup.md`: show the authenticated `GET /v1/models` path, identify the provider-native fields and the canonical `reasoning.supported_efforts`/`default_effort` metadata, and show `reasoning_effort` as the Chat Completions request parameter. Clarify that client UIs vary and may ignore custom model metadata.
- Keep the documented base URL as `http://127.0.0.1:8484/v1`; no `/v1` root catalogue is added.

**Exit:** Existing request translation stays covered, Grok values are forwarded unmodified in a stub test, and docs explain discovery and client limitations.

### Stage 4 — Verify and review

- Run focused catalogue/translation/server tests, then `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --all-targets --locked`.
- A read-only authenticated `GET http://127.0.0.1:8484/v1/models` was completed against the local Eggpass daemon. It returned HTTP 200 with 23 models, 15 carrying `reasoning.supported_efforts`; the key was not printed or stored. The unauthenticated `401` was not treated as catalogue evidence.
- Keep the offline stub integration tests as the deterministic acceptance check; do not create or expose credentials for future runs.
- Review the final diff for scope, response compatibility, absence of guessed capability claims, and absence of secrets. No release/tag/publish operation is part of this plan.

**Exit:** Focused and full local checks pass, the final public response shape is covered offline, and any live-check limitation is explicit.

## Acceptance criteria

- `GET /v1/models` remains the documented discovery endpoint and still requires an Eggpass client key.
- For each model with advertised levels, the response contains the original provider metadata and a top-level `reasoning` object whose `supported_efforts` exactly matches the upstream values and whose `default_effort` is present only when the provider supplies a default.
- Models without published effort levels do not receive fabricated reasoning support or level metadata.
- `GET /v1/models/{id}` returns the same capability fields as the list route.
- ChatGPT effort translation and Grok pass-through remain covered by offline tests.
- Catalog and request tests require no live account, credentials, or token spend; no secret is printed.
- Client setup docs explain the metadata contract and warn that generic OpenAI-compatible clients may ignore it.
- Formatting, strict Clippy, and all-target tests pass; no release or publishing work is performed.

## Implementation outcome

- Catalogue normalization now emits top-level `reasoning.supported_efforts` in provider order and adds `reasoning.default_effort` only from a published provider default. Provider-native fields remain available; conflicting native top-level reasoning is retained under `reasoning.provider`, and an existing `capabilities.reasoning` field is not overwritten.
- Catalogue merging now uses the stable `ProviderId::all()` order, so duplicate IDs consistently prefer x.ai before ChatGPT, independent of hash-map iteration.
- The offline tests cover both provider shapes through list/detail routes, authentication, unsigned-provider exclusion, duplicate precedence, catalogue failures, malformed/future labels, preserved metadata, ChatGPT translation, and unchanged Grok forwarding.
- Formatting, strict Clippy, and the final all-target test run passed. One earlier parallel run had transient failures in existing Codex subprocess tests; that module passed in isolation and the subsequent all-target run passed (404 tests).
- An authenticated live catalogue request returned HTTP 200 with 23 models, 15 carrying `reasoning.supported_efforts`. The user-provided key was not printed or written to disk; no credential files were read and no key was created.

## Relevant files

- `src/provider/mod.rs` — normalization and common reasoning aliases.
- `src/provider/openai.rs` — ChatGPT capability field allowlist.
- `src/server/models.rs` — merged list and single-model routes.
- `src/server/tests.rs` — public route integration tests.
- `src/server/translate.rs` — Chat Completions-to-Responses effort translation.
- `docs/client-setup.md` — authenticated discovery instructions and client caveats.

## References

- OpenAI Models list reference: https://platform.openai.com/docs/api-reference/models/list
- OpenRouter models catalog: https://www.openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties
- OpenRouter reasoning metadata guidance: https://openrouter.ai/docs/guides/best-practices/reasoning-tokens
