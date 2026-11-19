---
name: eggpass-eggpack-release
description: |
  Prepare, validate, and publish releases of `eggstack/eggpass` with the checked-in
  Eggpack workflow. Trigger when asked to cut an Eggpass release, run Eggpack,
  inspect its GitHub draft, or publish the release to GitHub/crates.io. Do not use
  for ordinary code changes or another repository's release process.
---

# Eggpass Eggpack Release

## Inputs to collect

- The intended `X.Y.Z` release version and source branch/commit, if not clear from context.
- Whether the request is to prepare a draft or to publish it. Publishing is a separate, public action that also triggers crates.io publication.

## Procedure

1. Read `docs/releasing.md`, the release workflow, `Cargo.toml`, and the changelog. Check the working tree, current version, tag, and existing GitHub release. Releases are immutable: use a new version/tag; do not move or overwrite a published tag or release. For the v0.1.1 release lineage, preserve the v0.1.0 tag at `21e14c93d1b4292b410ce6c0707c064e63c98dc0`.
2. Confirm the release commit has the intended Cargo version (`X.Y.Z`, no `v`) and accurate release notes. Run the repository's CI/release checks before tagging, including formatting, all-target tests, Clippy with warnings denied, and package validation. Fix failures before continuing.
3. Push the release commit, then create and push the annotated tag `vX.Y.Z` pointing at that exact commit. Eggpack does not create or push tags; do not run it against an unpushed or ambiguous tag.
4. Dispatch the checked-in release workflow for the exact tag:

   ```sh
   gh workflow run release.yml --repo eggstack/eggpass --field release_tag=vX.Y.Z
   ```

   Watch the resulting run to completion. The workflow should build and qualify all five targets, produce the manifest and checksum sidecars, and stage a **draft** GitHub release. A successful aggregate/gate alone is insufficient; inspect all matrix jobs and artifacts.
5. Inspect the draft's release notes and download every asset. Verify the manifest independently of CI:
   - `schema_version` is `1`, `product_id` is `eggpass`, and `release_id` is plain `X.Y.Z` (no leading `v`).
   - `source_revision` is the exact 40-character commit the tag peels to.
   - The manifest names exactly the five expected target assets below; each recorded size and lowercase SHA-256 matches the downloaded binary.
   - Each `.sha256` sidecar also validates against its corresponding binary.
   - The release notes agree with the changelog and link to the actual downloads/checksums.

   Expected assets (the Windows executable keeps its `.exe` suffix):
   - `eggpass-aarch64-apple-darwin`
   - `eggpass-aarch64-unknown-linux-gnu`
   - `eggpass-x86_64-apple-darwin`
   - `eggpass-x86_64-pc-windows-msvc.exe`
   - `eggpass-x86_64-unknown-linux-gnu`

   Do not treat CI's binary `--version` validation as proof that the manifest `release_id` is correct; `scripts/validate-release-version.py` validates the executable against Cargo metadata, not the manifest field.
6. If only the current draft's manifest is wrong and the binaries/sidecars are otherwise verified, correct that draft asset alone with `gh release upload vX.Y.Z --repo eggstack/eggpass <corrected-manifest> --clobber`, then download it again and repeat the manifest/artifact checks. Do not edit the release generator or pipeline as an incidental part of a release; propose that as a separate code change if future runs need a permanent fix. Keep the release a draft until every check passes.
7. Immediately before making it public, obtain explicit user confirmation for this exact tag and its crates.io consequence. A broad initial request to “release” is not the final confirmation. After confirmation, publish the draft:

   ```sh
   gh release edit vX.Y.Z --repo eggstack/eggpass --draft=false
   ```

   This public action fires `.github/workflows/publish-crates.yml`, which publishes the crate. Do not expose or print credentials; rely on the configured GitHub CLI session and repository secret.
8. Watch the release-triggered crates.io workflow through its publish step. Confirm the GitHub release is public and the exact `eggpass` version is present on crates.io. If available, run `eggpass update --check` as a final consumer-side check and report its result separately.

## Output contract

Report the source commit and tag; workflow run and matrix outcome; draft/public status; five-target manifest and checksum result; GitHub release URL; crates.io workflow and version result; and any checks skipped or still unverified. Never call a draft “published.”

## Failure handling

- Any failed workflow job, missing target, mismatched checksum/size/revision/version, or inaccurate release notes blocks publication. Leave the release as a draft and report the evidence.
- If an API request is rate-limited, inspect the relevant run/release state and rate limit before retrying; do not repeat an unchanged failing request in a tight loop.
- If the published version/tag already exists, stop rather than overwrite it. A correction must use the documented recovery path or a new release, never silently retarget history.
- Do not request, read, or paste registry tokens or other credentials into chat or logs.

## Windows (win32) platform notes

The Cargo and `gh` commands above also work in PowerShell. Use PowerShell paths and `Get-FileHash -Algorithm SHA256` to compute a local digest; compare its lowercase `Hash` value with the manifest and `.sha256` sidecar. Do not assume `shasum` or `sha256sum` exists.

## Example

For v0.1.1, keep the existing v0.1.0 tag unchanged, stage v0.1.1 as a draft, and verify `release_id` is `0.1.1` (not `v0.1.1`) before publication. The v0.1.1 draft's verified `source_revision` was `578a1eb7f3703cdf76861607d2c8a9bbc9807fe5`; the manifest was corrected and re-downloaded before the user authorized publication.
