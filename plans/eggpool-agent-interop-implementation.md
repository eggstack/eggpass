# EggPass–EggPool Agent Ticket Interoperability — Implementation Plan

**Status:** Implementation complete. The owner of both EggPass and EggPool confirmed the producer v1 contract and authorized changes on the existing EggPool consumer branch. Validation passed. Commit and push are authorized; no merge or release was performed.

**Basis:** EggPass Issue #11, current EggPass producer code and documentation, and the EggPool provider-agent feature branch inspected during the research review.

## Confirmed implementation decisions

- Use EggPass's documented version-1 producer contract without producer-side format changes.
- Apply EggPool consumer changes on feat/provider-oauth-user-accounts in the existing sibling worktree.
- Persist a one-way digest of successfully consumed enrollment tickets so a restart cannot reopen replay; never persist the raw ticket/enrollment credential.

## Goal

Make EggPass-generated EggPool enrollment tickets usable by EggPool, complete the one-time enrollment-to-mTLS flow, and keep enrollment, transport, and inference credentials separate. Preserve existing account ownership, authorization, revocation, and secret-redaction protections.

This plan is stored in the EggPass repository because that is the current workspace. Most compatibility changes belong in the EggPool consumer repository; those edits were separately authorized on the existing consumer branch before implementation.

## Baseline and review findings

- EggPass currently emits a version-1 ticket in the epagent1. envelope. Its JSON contains v, control, server_name, ca_sha256, enroll, expires_at, agent, eggpass, and api_key. The ticket lifetime is 600 seconds. The CA pin is SHA-256 over the first CA certificate's DER, encoded as URL-safe unpadded Base64. See src/eggpool.rs:32-35,97-108,177,220-230,860-883 and docs/eggpool.md:9-50.
- The relevant consumer is the sibling worktree /Users/davidbowman/projects/gorouter/.worktrees/feat-provider-oauth-user-accounts, branch feat/provider-oauth-user-accounts, inspected at commit ce027903. Its ticket decoder expects version and ca_pin, denies unknown fields, caps lifetime at 120 seconds, and hashes the full configured PEM as hex. Its reverse server uses the account name as the initial authentication username. See the consumer worktree's rust/src/agents/ticket.rs:21-24,77-90,154-162,249-259 and rust/src/agents/tunnel.rs:572-635.
- The consumer currently assigns one environment-variable name to both api_key_env and transport_secret_env, and persists the ticket enrollment credential as the account secret. See rust/src/server/agents.rs:302-318,422-428 in the consumer worktree.
- EggPass documents a certificate/key callback, subsequent mTLS reconnect, removal of the one-time password, and Host rewriting for loopback forwarding. I did not find this complete flow in the inspected consumer branch. See docs/eggpool.md:29-50; the consumer enrollment handler rejects configurations requiring client certificates at rust/src/server/agents.rs:167-174.
- The consumer fixture is synthetic and matches the consumer schema; it uses a short lifetime and hashes PEM text, so it does not establish interoperability with the current EggPass producer. See rust/tests/common/agent_ticket.rs:99-107 in the consumer worktree. No tests were run during this review.
- The consumer's historical closure record predates the current producer and reports that no live producer existed then; it also documents adopting the enrollment value as a single transport credential. This is historical evidence, not evidence of current interoperability. See plans/closure/provider-agents/002-status.md:23-37,117-120 in the consumer worktree.

## Contract decisions required before implementation

Use the current EggPass producer and its documented enrollment exchange as the recommended compatibility target, unless both project owners explicitly agree to a new versioned contract. Record the decision in a shared, versioned contract before coding.

1. Envelope and fields: Confirm version-1 prefix/encoding and exact JSON field names, including how the consumer handles producer metadata and the API key. Keep strict parsing, but recognize all fields the producer emits.
2. Lifetime: Agree that a 600-second producer ticket is valid and define the maximum accepted lifetime and clock-boundary behavior. Align every decoder and replay reservation limit; the consumer registry currently caps reservations at 120 seconds (rust/src/agents/enrollment.rs:16-17,59-116).
3. CA pin: Agree on SHA-256 of the first PEM certificate's DER, encoded URL-safe without padding.
4. Initial authentication: Agree on the fixed initial username and the one-time credential's permitted use. EggPass currently specifies username enroll and the ticket's enrollment credential as password (docs/eggpool.md:29-36).
5. Credential roles: Keep the enrollment credential separate from the inference API key. The API key is used for ordinary EggPass API calls; the enrollment credential is one-time bootstrap authorization (docs/eggpool.md:18-23,39-44).
6. Enrollment lifecycle: Confirm certificate issuance, callback authorization/body, persistence and consumption rules, mTLS reconnect, revocation, and loopback Host rewriting (docs/eggpool.md:29-50).
7. Replay after restart: Choose whether consumed-ticket state must survive a consumer restart. The current registry is in-memory, so restart can reopen a replay window (plans/closure/provider-agents/002-status.md:336-339 in the consumer worktree).

**Gate:** Do not start implementation until the field contract, authentication sequence, credential ownership, and certificate bootstrap/reconnect behavior are agreed by both sides.

## Staged implementation

### Stage 1 — Consumer ticket compatibility

- Update EggPool's decoder to accept the agreed producer field names and all required version-1 fields while retaining bounded input, strict validation, secret redaction, and safe error behavior.
- Enforce the agreed lifetime policy, including expiry boundary tests. Align the replay-registry reservation limit with the accepted ticket lifetime.
- Parse the configured CA bundle and verify the agreed first-certificate DER digest and encoding; do not hash the PEM text as a substitute.
- Add a producer-shaped fixture generated from the same contract and ensure it includes the producer's extra fields. Keep malformed, unknown-version, unknown-field, and oversized-input tests.

**Exit:** An unmodified current EggPass ticket passes consumer decoding and validation; deliberately malformed or out-of-policy tickets fail without exposing credentials.

### Stage 2 — Bootstrap authentication and one-time callback

- Implement the agreed initial reverse-auth username and one-time credential handling; it must not depend on the selected account display name.
- Generate or obtain the per-agent client certificate/key, then send the documented callback request through the reverse tunnel with the required path, bearer authorization, and JSON body.
- Validate certificate/key pairing and size limits. Consume the enrollment credential only after successful validation and persistence; ensure failed attempts do not create a partially enrolled account.
- Keep the forwarding destination fixed to the configured EggPass loopback listener. Implement the documented Host rewrite while preserving method, path, body, and authorization header.

**Exit:** A successful callback enrolls a certificate with EggPass; invalid, expired, replayed, or incomplete bootstrap attempts fail safely.

### Stage 3 — mTLS, secrets, and account lifecycle

- Reconnect the agent using mTLS after successful enrollment and remove the one-time password from active reverse-client configuration.
- Store the inference API key separately from any bootstrap/transport credential. Use the inference key for ordinary EggPass API requests; do not substitute the enrollment credential.
- Preserve owner binding, authorization, revocation, atomic publication, and secret-redaction protections already present in the consumer branch.
- Implement persistent consumed-ticket tracking if that is the agreed restart policy; otherwise document and test the explicitly accepted restart behavior.

**Exit:** The normal API path operates independently of bootstrap credentials, mTLS is required after enrollment, and revocation/cleanup removes the correct per-agent state.

### Stage 4 — Cross-project interoperability and security validation

- Add an integration test that uses a ticket produced by EggPass's actual ticket-generation path and exercises EggPool enrollment, callback delivery, mTLS reconnect, and a normal authenticated API request.
- Avoid relying solely on a hand-authored consumer-schema fixture. Keep a versioned shared fixture or test harness so producer and consumer changes detect drift.
- Run relevant test suites in both repositories, formatting/lint checks, and the focused end-to-end test. Review logs and failure paths for ticket, enrollment, API-key, private-key, and OAuth-token disclosure.

**Exit:** The producer-generated ticket completes the full workflow; all negative tests fail closed; results are recorded separately from historical closure evidence.

## Acceptance checklist

- [x] Contract is agreed and versioned by EggPass and EggPool maintainers before implementation.
- [x] Current producer ticket fields and envelope decode without producer-side changes, or a jointly approved contract migration is completed.
- [x] Exactly the agreed ticket lifetime is accepted; expired and excessive lifetimes are rejected; replay reservation TTL is consistent.
- [x] CA-pin verification uses the agreed certificate input and encoding, including a multi-certificate bundle test.
- [x] Initial reverse authentication uses the agreed fixed protocol identity and one-time credential, independent of account display name.
- [x] Enrollment callback validates the exact path, authorization, payload, certificate/key pair, and size boundaries; failed persistence does not consume the ticket.
- [x] Successful enrollment stores the issued certificate/key and reconnects with mTLS; the one-time password is no longer used after bootstrap.
- [x] The API key is stored and used independently from enrollment/transport secrets.
- [x] Host rewriting and fixed-loopback routing preserve request semantics and cannot be redirected by caller-supplied Host/target values.
- [x] Replay behavior is tested for concurrent enrollment, repeated use, expiry, and the agreed restart policy.
- [x] Existing ownership, authorization, revocation, cleanup, and secret-redaction tests continue to pass.
- [x] A cross-project test uses a ticket from the real EggPass producer path and proves the complete enrollment-to-mTLS-to-API flow.
- [x] No credentials or private key material appear in logs, diagnostics, test output, or fixtures.

**Validation evidence:** EggPass producer tests: 5 passed. EggPool full serial workspace suite passed, as did the focused routing, upstream, enrollment, migration, and producer-backed interoperability tests. Formatting, strict Clippy, locked release build, no-default-feature check/Clippy, and `git diff --check` passed. The interoperability runner exercised the real EggPass CLI ticket through callback, mTLS reconnect, and an authenticated API request.

## Out of scope

- Changing provider OAuth custody/login plans; those are separate from Issue #11's reverse-agent enrollment contract.
- Public release, publishing, deployment, or merge.
- Treating the historical consumer closure or its synthetic fixture as proof of current producer interoperability.

## References

- Issue: https://github.com/eggstack/eggpass/issues/11
- EggPass producer: src/eggpool.rs:32-35,97-108,177,220-230,705-736,860-883; contract: docs/eggpool.md:9-50.
- EggPool consumer branch feat/provider-oauth-user-accounts at ce027903: rust/src/agents/ticket.rs; rust/src/agents/enrollment.rs; rust/src/agents/tunnel.rs; rust/src/server/agents.rs; rust/tests/common/agent_ticket.rs; historical evidence: plans/closure/provider-agents/002-status.md.
