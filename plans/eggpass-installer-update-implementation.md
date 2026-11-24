# Eggpass Installer and Installation-Aware Updates — Implementation Plan

**Status:** Implementation authorized by a follow-up request. This document remains the design record; its plan does not authorize a public release, tag mutation, or package publication.
**Direction:** Port/adapt Gregg’s mature installer closely. Do not create a new installer design from scratch.
**Primary source snapshot:** `eggstack/gregg` commit `fabcaf21bc60f719a5de5b9420490ffed52400e4` (current `main` when researched on 2026-11-23).

## Goal

Provide a supported, prebuilt-binary installation path for Eggpass on the five existing release targets, then make `eggpass update` preserve the installation channel: Cargo-managed installs are updated by Cargo; installer/direct installs use Eggpass’s existing verified self-update transaction.

Preserve the current Eggpack release contract and `eggup` ownership, integrity, locking, rollback, and candidate-validation machinery. Reuse Gregg’s installer implementation and tests for the generic bootstrap mechanics rather than duplicating those mechanics in a fresh script.

## Pre-implementation Eggpass baseline

- At the start of this implementation, the README documented `cargo install eggpass` and Eggpass had no `install.sh` or `install.ps1`.
- The starting Eggpack contract released five native binaries, each with a SHA-256 sidecar, plus `release-manifest.json`; the manifest carries the exact asset size and digest used by Eggpass’s updater.
- `.github/workflows/release.yml` is generated from `release/eggpack/` inputs and CI runs `eggpack ci check`; the implementation must keep using that generator and drift gate.
- At baseline, `src/update.rs` positively detected Cargo ownership and refused mutating self-updates for any provenance other than `SelfManaged`. The existing `eggup` transaction remains the direct-binary update path.
- At baseline, Cargo provenance retained only the detected root and version. The `.crates.toml` key includes `<name> <version> (<source>)`; the implementation now retains that source and delegates only the recognized default crates.io source. Path, Git, alternate-registry, malformed, and unknown-source installs remain fail-closed with source-appropriate guidance.
- At plan time, the public version identities are independent: Eggpass CLI `v0.1.3` / crate `0.1.3`, Rust `eggpass-auth` `eggpass-auth-v0.1.1` / crate `0.1.1`, and Python `eggpass-auth-python-v0.1.2` / package `0.1.2`. This work changes only CLI install/update behavior; do not bump, retag, or publish any of these packages as part of implementation.
- `docs/releasing.md` defines `vX.Y.Z` as the CLI tag and states that publishing the reviewed GitHub draft triggers crates.io publication. Therefore the public GitHub stable release is the binary installer’s version authority; crates.io publication may lag and is only used for an exact-version Cargo fallback/update after that version is available.

## Resolved Eggpack staging seam

The Eggpack revision already pinned in `release/eggpack/github-policy.json` (`242654ab29e59ebbd3beca0a0ff276f4df4df873`) supports the required product-wrapper path. Its `InstallerPresentationV1` `product_wrappers` mode copies two explicitly named repository files as the public `install.sh` and `install.ps1`, while staging Eggpack-generated exact-version bootstrap installers under separate configured names. The checked-in `GitHubStagingInputsV1` has an optional `installer_presentation` path; the renderer passes `--installer-presentation` and the checked-out repository root to `_prepare-stage`. This is the supported extension point: no Eggpack generator/schema change or Eggpack pin bump is presently required.

Use a new `release/eggpack/installer-presentation.json` with this shape, then point `staging.inputs.installer_presentation` in `release/eggpack/github-policy.json` at it:

```json
{
  "schema_version": 1,
  "mode": {
    "product_wrappers": {
      "posix_source": "packaging/install.sh",
      "powershell_source": "packaging/install.ps1",
      "generated_posix_name": "install-exact.sh",
      "generated_powershell_name": "install-exact.ps1"
    }
  }
}
```

Eggpack treats wrapper files as bounded, regular, non-symlink source files copied as bytes; it does not execute them. The four installer assets are auxiliary staging assets, not target binaries or members of `release-manifest.json`. Keep `release/eggpack/install-policy.toml` as the schema-v1 bootstrap policy for the existing direct targets; do not repurpose it for installer presentation.

## Design constraints

### 1. Port Gregg’s installer as a cohesive unit

Use the Gregg installer family at the pinned revision as the implementation source: the release entrypoints and their helper scripts under `packaging/`, the installer rerun tests, and the release-time validation in `.github/workflows/release-binaries.yml`. First inventory the exact entrypoints, helper boundaries, test seams, and license headers at that revision; keep the source revision recorded in the implementation PR.

Preserve the proven structure and behavior, including argument parsing, stable-version validation, host detection, staging/cleanup, checksum verification, candidate identity checks, same-scope rerun handling, foreign-file/symlink refusal, privilege boundaries, PATH guidance, and platform-specific replacement behavior. Make only the Eggpass-specific adaptations below. Retain applicable copyright/license notices when copying or adapting source.

Primary references: [Gregg installer source](https://github.com/eggstack/gregg/tree/fabcaf21bc60f719a5de5b9420490ffed52400e4/packaging), [installer rerun test](https://github.com/eggstack/gregg/blob/fabcaf21bc60f719a5de5b9420490ffed52400e4/scripts/tests/test-install-rerun.sh), and [release workflow](https://github.com/eggstack/gregg/blob/fabcaf21bc60f719a5de5b9420490ffed52400e4/.github/workflows/release-binaries.yml). Snip-it is a secondary reference only: its docs promise method-following updates, but its current updater special-cases Homebrew and otherwise follows a binary-replacement path. Do not copy that mismatch.

### 2. Eggpass-specific installer contract

- Publish a Bash installer and a PowerShell installer alongside the tagged Eggpass release. Support both latest and a pinned stable `X.Y.Z` version, with direct links through the GitHub release assets (latest and exact-tag forms). Do not make the installer command fetch a mutable script from the repository’s `main` branch.
- Reuse Gregg’s latest/exact release acquisition pattern, but use Eggpass’s GitHub releases as the selector: no version means the public latest stable release (`releases/latest`); `--version X.Y.Z` means the exact `vX.Y.Z` release. Never resolve the CLI version from a floating crates.io response. A pinned install must go directly to that release tag. The Eggpass release flow publishes crates.io only after the GitHub release is made public, so a Cargo fallback/update may encounter a short registry lag; request the exact selected version and return a clear retry-later error if it is not yet available—never install an older floating version.
- Use the existing stable target set and asset names from `release/eggpack/distribution.toml`: Linux x86_64/aarch64, macOS x86_64/arm64, and Windows x86_64. Keep the script’s mapping covered by a drift test against that contract; do not silently add an unqualified target.
- Reuse Gregg’s sidecar-verification implementation for the bootstrap scripts, and keep Eggpack’s release validation proving that every sidecar matches the corresponding `release-manifest.json` asset. Avoid introducing `jq`, Python, or a custom shell JSON parser solely for installation. Do not change the manifest schema or duplicate its integrity logic in Rust.
- Verify the downloaded candidate’s exact stable version using Eggpass’s existing `eggpass --version` output contract (not Gregg’s `version` subcommand). Verify before installing or executing it.
- Fail closed on checksum, transport, malformed metadata, or candidate-identity errors. Hosts outside the five published targets are unsupported and must fail with guidance; do not use Cargo fallback there because Eggup has no release-target mapping for subsequent updates. For one of the five supported targets, an exact-version Cargo fallback may be attempted only after a definite missing binary asset (HTTP 404), never after failed verification or transient server/network errors.
- If Cargo fallback is used, request the exact selected CLI version from the standard crates.io registry under an owner-private temporary Cargo root, validate the candidate, then copy it to the installer’s chosen final directory and remove the temporary root. The installed binary must not remain Cargo-managed merely because Cargo built it. If that exact crate is not yet published, fail with retry guidance; do not fall back to an unpinned `cargo install eggpass` or choose a different version.
- Default to a per-user install. Require explicit elevation for a system install; never invoke `sudo` silently. Reuse Gregg’s tested PATH/profile behavior and no-op/refusal checks rather than creating new profile-editing logic. Eggpass has no service-registration post-install step, so omit Gregg’s daemon lifecycle actions.
- Keep same-scope reruns idempotent; refuse to overwrite a symlink or unrelated executable. Preserve separate user/system destinations so rerunning at one scope does not replace another scope’s install.

### 3. Method-aware `eggpass update`

Treat the bootstrap installer’s final binary as a direct/self-managed installation, regardless of whether its candidate was downloaded or built by the temporary Cargo fallback. It should update through the current `eggup` manifest-driven transaction, which already verifies artifact size/digest, candidate identity, and live-file ownership under the update lock.

For a Cargo-managed executable, do not bypass the existing provenance refusal and replace the file directly. Add a Cargo update branch that preserves Cargo’s own package record:

- For a positively identified default-crates.io installation, invoke Cargo with the detected root and the exact target version from the already-resolved Eggpass release (equivalent to `cargo install eggpass --version <to_version> --locked --force --root <detected-root>`). Re-read Cargo’s record and the installed candidate after completion; report success only if Cargo metadata and executable version agree with the requested version.
- Preserve the installation source. The initial implementation does not auto-update `--path`, `--git`, alternate-registry, or unrecognized sources because the current provenance does not retain enough invocation/revision evidence. Keep those cases fail-closed with useful manual guidance; never silently convert them into crates.io installs. If crates.io has not published `<to_version>` yet, fail with retry guidance rather than installing another version.
- Validate Cargo’s running-process replacement behavior on every supported OS. In particular, prove Windows behavior while the updater executable is running. If safe automatic replacement cannot be demonstrated, use a bounded helper that waits for the parent to exit or retain an explicit Cargo command handoff; do not weaken the no-in-place-Cargo-mutation invariant.
- For `SelfManaged`, use the existing Eggup transaction. For `Unprovable`, preserve fail-closed behavior and actionable guidance. Never fall back from a rejected Cargo update to direct replacement.
- Keep `--check` non-mutating and preserve current daemon restart behavior. Deciding whether an already-current check should return a nonzero exit code is a separate issue and is out of scope for this plan unless implementation proves it blocks the installer/update contract.

## Implementation stages

### Stage 0 — Freeze the Gregg source and resolve integration boundaries

- Check out/read Gregg at the pinned commit; inspect its Bash/PowerShell entrypoints, platform helpers, tests, release uploads, and CI gates. Run Gregg’s installer tests at that baseline before adapting them, where a suitable checkout is available.
- Record which files and behaviors will be ported with only identity/contract changes. Keep the Gregg-specific implementation intact wherever Eggpass has no reason to differ.
- Preserve the resolved authority: latest is GitHub’s public latest stable release; pinned is the exact `vX.Y.Z` release. Add a deterministic fixture for crates.io publication lag: a valid binary release still installs from GitHub, while a required Cargo fallback for the exact not-yet-published crate fails safely with retry guidance.
- Use the already-supported Eggpack `InstallerPresentationV1` seam described above: add `release/eggpack/installer-presentation.json`, add its path as `staging.inputs.installer_presentation` in `release/eggpack/github-policy.json`, then regenerate `.github/workflows/release.yml` with the currently pinned Eggpack. Do not change the distribution contract, repurpose `install-policy.toml`, patch the generated workflow by hand, or bump Eggpack unless implementation first proves the pinned feature is absent or defective.
- Extend `.crates.toml` parsing to preserve the Cargo source string. Automatically delegate only when root, package, version, and the recognized default crates.io source are all proven. Path, git, alternate registry, and unrecognized sources remain manual-only; never rewrite them as crates.io installs. Add fixtures for each source classification before enabling the Cargo update branch.

**Exit:** Source revision, copy/adaptation boundary, release attachment mechanism, latest-version authority, and Cargo-source policy are all explicit. No installer code is started until these are settled.

### Stage 1 — Port and qualify the bootstrap installers

- Create `packaging/install.sh` and `packaging/install.ps1` by closely porting Gregg’s entrypoints/helpers at the pinned commit, retaining its tested helper boundaries, safety checks, and applicable license notices. Port/adapt the relevant rerun tests into a deterministic `packaging/tests/` harness. Limit adaptations to Eggpass identity/version output, the five existing asset names/targets, destination naming, and Eggpass-specific messaging.
- Add deterministic fixtures for latest/pinned version selection, all five target mappings, valid/malformed checksum sidecars, identity mismatch, transport failure, exact 404 fallback on a supported target, unsupported-target refusal, cleanup, same-scope rerun, foreign files, symlinks, PATH handling, and privilege behavior.
- Test the Bash script under the minimum supported Bash (including macOS system Bash 3.2); run `bash -n` and ShellCheck. Parse/lint the PowerShell script and run its platform-appropriate fixture tests. No fixture may contact a live release host or require credentials.

**Exit:** The port passes adapted Gregg installer tests and Eggpass-specific fixtures on supported test hosts; checksum/identity/transport failures never install or trigger Cargo fallback.

### Stage 2 — Publish installer files through Eggpack

- Add `release/eggpack/installer-presentation.json` using the resolved `product_wrappers` shape above and add `staging.inputs.installer_presentation` to `release/eggpack/github-policy.json`. Leave the five-target `distribution.toml`, binary manifest schema, and direct-target `install-policy.toml` unchanged.
- Keep `packaging/install.sh` and `packaging/install.ps1` as the public wrapper names. Stage Eggpack’s exact-version generated bootstraps as `install-exact.sh` and `install-exact.ps1`; test that names are non-colliding and wrapper bytes are copied without execution.
- Regenerate the workflow, never edit it directly. Use the same pinned tool and input shape as CI:

  ```sh
  eggpack ci generate \
    --workflow-shape release/eggpack/workflow-shape.json \
    --contract release/eggpack/distribution.toml \
    --github-policy release/eggpack/github-policy.json \
    --output .github/workflows/release.yml
  eggpack ci check \
    --workflow-shape release/eggpack/workflow-shape.json \
    --contract release/eggpack/distribution.toml \
    --github-policy release/eggpack/github-policy.json \
    --workflow .github/workflows/release.yml
  ```

- Extend draft validation to require all four installer assets plus the five binaries, five sidecars, and manifest; verify latest/exact script URLs, and prove wrappers/generated installers are not binary members in `release-manifest.json`. Preserve Eggpack’s draft-first staging and the separate human publish action. No public asset is published by implementation work.

**Exit:** The pinned Eggpack generator stages the product wrappers and exact bootstraps, `eggpack ci check` passes, the manifest remains the existing five-target contract, and the ordinary build/qualification graph is unchanged.

### Stage 3 — Add method dispatch to the updater

- Refactor only the updater policy boundary needed to dispatch `CargoManaged`, `SelfManaged`, and `Unprovable`. Continue using Eggup for manifest fetch/projection, bounded download, digest/size verification, candidate validation, lock ownership, atomic commit, and rollback.
- Preserve Cargo root and source evidence in provenance as needed. Invoke Cargo only for supported Cargo-managed sources; keep its metadata and installed version in sync. Provide explicit, tested guidance for cases that cannot be automated safely.
- Exercise updater behavior using fake Cargo executables and local fixture transports. Test successful/failed Cargo delegation, detected non-default roots, unsupported sources, not-installed Cargo, and Windows running-executable handling. Test direct updates through the same manifest and artifact identity contract as today.

**Exit:** Cargo installs are never directly self-replaced; direct/bootstrap installs update through Eggup; all unsupported or uncertain ownership/source cases fail safely with useful next steps.

### Stage 4 — User docs and end-to-end qualification

- Update the root README install section and `docs/updating.md` with the supported shell/PowerShell commands, pinned install syntax, prerequisites, install destinations, PATH guidance, integrity limits, and method-specific update behavior. Update `docs/releasing.md` with the installer asset review/checks. Keep Cargo installation documented as a separate supported method.
- Run formatting, strict Clippy, locked workspace tests, Linux installer tests, macOS Bash 3.2 installer tests, Windows PowerShell and updater tests, the five-target release build/qualification checks, crate packaging, and `eggpack ci check`. Run `git diff --check` and review that the diff contains no generated binaries, secrets, or unrelated release changes.
- Verify a draft release locally or through a non-publishing validation path only. Any public release remains a separate operation requiring the existing fresh approval gate.

**Exit:** All advertised targets have installation and update evidence, Cargo metadata remains accurate after Cargo updates, installer-created binaries update via Eggup, and the generated release workflow remains in sync.

## Out of scope

- Writing a new installer framework or replacing Gregg’s tested generic installer logic with a separate implementation.
- Homebrew, apt, winget, or other package-manager support not already part of Eggpass’s distribution contract.
- Changes to Eggpack manifest schema, Eggup transaction internals, release signing/TUF, daemon restart policy, or the current `--check` already-current exit-code policy, unless Stage 0 proves a narrowly scoped prerequisite is unavoidable.
- Running public release workflows, publishing installer assets, changing tags, or installing Eggpass on a user machine as part of the implementation-plan task.

## Acceptance checklist

- [ ] Gregg source commit is pinned and its current installer/test structure is ported closely, with license notices retained.
- [ ] Five Eggpass targets map to the existing Eggpack distribution contract without drift.
- [ ] Latest and pinned installers fetch exact release assets, verify checksum and Eggpass version, and fail closed on non-404 errors.
- [ ] On supported targets, Cargo fallback uses an isolated staging root and leaves the final bootstrap install outside Cargo’s managed root; unsupported targets fail rather than promising an un-updatable install.
- [ ] Reinstall-at-same-scope, PATH handling, permissions, symlink and foreign-file protections are covered by adapted Gregg tests.
- [ ] Shell and PowerShell installers are included in the reviewed release draft through Eggpack’s supported generation path; `eggpack ci check` passes.
- [ ] Cargo-managed `eggpass update` delegates to Cargo only for a positively identified default-crates.io source/root and requests the exact resolved CLI version; unsupported sources fail safely and it never self-replaces the Cargo-owned binary.
- [ ] Direct/bootstrap `eggpass update` continues to use Eggup’s manifest-driven verified transaction.
- [ ] CI covers supported OS/architecture behavior; no live credentials or provider/release network access are needed by deterministic tests.
- [ ] Latest selection follows the public GitHub stable release; exact installs/updates use the exact `vX.Y.Z` CLI release, with crate-publication lag handled by a retryable exact-version failure.
- [ ] Eggpack’s pinned product-wrapper staging feature is configured through release inputs; generated workflow is regenerated and checked, and the five-target manifest remains unchanged.
- [ ] CLI, Rust auth, and Python auth version/tag lines remain independent; no existing published tag or package version is overwritten.
- [ ] No public release, tag mutation, or package publication occurs without a separate fresh authorization.

## References

- Eggpass install and release contract: `README.md:19-29`, `docs/releasing.md:15-35,85-95`, `release/eggpack/distribution.toml`, and `release/eggpack/pack.toml`.
- Eggpass generated-workflow gate: `.github/workflows/ci.yml:71-91`; generated release workflow: `.github/workflows/release.yml`.
- Eggpass updater policy and provenance: `src/update.rs:116-153,330-355,750-860`; user-facing behavior: `docs/updating.md`.
- Gregg primary source pinned at `fabcaf21bc60f719a5de5b9420490ffed52400e4`: `packaging/install.sh`, `packaging/install.ps1`, platform helper scripts, `scripts/tests/test-install-rerun.sh`, and `.github/workflows/release-binaries.yml`.
- Eggpack staging support pinned at `242654ab29e59ebbd3beca0a0ff276f4df4df873`: [staging adapter](https://github.com/eggstack/eggpack/blob/242654ab29e59ebbd3beca0a0ff276f4df4df873/crates/eggpack-github/README.md), [staging policy](https://github.com/eggstack/eggpack/blob/242654ab29e59ebbd3beca0a0ff276f4df4df873/crates/eggpack-ci/src/policy.rs), [release renderer](https://github.com/eggstack/eggpack/blob/242654ab29e59ebbd3beca0a0ff276f4df4df873/crates/eggpack-ci/src/render_release.rs), and [CLI staging reference](https://github.com/eggstack/eggpack/blob/242654ab29e59ebbd3beca0a0ff276f4df4df873/crates/eggpack-cli/README.md).
- Snip-it is a secondary comparison only: `packaging/README.md`, `packaging/install.sh`, `docs/USER_GUIDE.md`, and `src/update.rs`; its documented install-method promise is not fully reflected in its current updater dispatch.
