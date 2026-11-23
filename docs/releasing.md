# Releasing

This guide covers the Eggpass CLI release. The checked-in Eggpack workflow
builds and qualifies the platform binaries, then stages a GitHub **draft**.
Publishing that draft is a separate, public action and triggers crates.io
publication.

## Prerequisites

- Push access to `eggstack/eggpass` and permission to run GitHub Actions.
- The repository secret `CARGO_REGISTRY_TOKEN` must authorize publishing the
  crates.io package being released (`eggpass` or `eggpass-auth`). Do not put
  the token in a file, command line, or log.
- The release workflow is generated from `release/eggpack/`. Its pinned Eggpack revision is in `release/eggpack/github-policy.json`; CI checks the generated workflow for drift.

## Release identity and assets

| Item | Rule |
| --- | --- |
| Git tag | `vX.Y.Z` |
| `Cargo.toml` version | Plain `X.Y.Z`, without `v` |
| Manifest `product_id` | `eggpass` |
| Manifest `release_id` | Plain `X.Y.Z`, without `v` |
| Manifest `source_revision` | Full commit SHA peeled from the tag |
| Manifest file | `release-manifest.json` |

The release contains these five binary assets, a matching `.sha256` sidecar
for each, `release-manifest.json`, and four auxiliary installer assets:

- `eggpass-aarch64-apple-darwin`
- `eggpass-aarch64-unknown-linux-gnu`
- `eggpass-x86_64-apple-darwin`
- `eggpass-x86_64-pc-windows-msvc.exe`
- `eggpass-x86_64-unknown-linux-gnu`
- `install.sh` and `install.ps1` (Eggpass product wrappers)
- `install-exact.sh` and `install-exact.ps1` (Eggpack-generated exact bootstraps)

The Windows executable keeps its `.exe` suffix. Asset names are not
versioned, so binary URLs and the latest installer URLs use
`releases/latest/download/<name>`. Pinned installer scripts select the exact
`vX.Y.Z` release; they do not fetch scripts from the repository's `main` branch.
The four installer files are auxiliary staging assets and are not members of
`release-manifest.json` or the five-target distribution contract.

## Prepare and stage a release

1. Choose a new version and confirm that neither its Git tag nor GitHub release
   already exists. Update the root package version in `Cargo.toml` and the
   matching package entry in `Cargo.lock`. Add accurate user-facing notes
   under that version in `CHANGELOG.md`. Do not bump independently
   versioned crates unless they are part of this release.

2. From a clean checkout based on current `main`, run the release checks:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --all-targets --locked
   cargo package --locked --allow-dirty
   git diff --check
   ```

   Stage only the intended release files, review the staged diff, run
   `git diff --cached --check`, commit, and push the release commit to `main`.
   Do not stage unrelated worktree changes.

3. Create and push an annotated tag pointing to that exact commit. The workflow
   does not create or push tags:

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   git tag -a "$release_tag" -m "eggpass ${release_version}"
   git push origin "$release_tag"
   ```

4. Dispatch the checked-in workflow for that exact tag:

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   gh workflow run release.yml --repo eggstack/eggpass --field "release_tag=$release_tag"
   ```

   Watch the run to completion. Inspect all five build, qualification, and
   consumer-validation jobs, the required gate, aggregation, and draft staging;
   a successful aggregate alone is not enough.

5. Review the staged draft and download every asset into a clean local
   verification directory. Confirm that:
   - all five expected binaries, all five checksum sidecars,
     `release-manifest.json`, and all four installer assets (`install.sh`,
     `install.ps1`, `install-exact.sh`, and `install-exact.ps1`) are present;
   - latest installer URLs select `releases/latest/download`, pinned installs
     select the exact `releases/download/vX.Y.Z` tag, and no installer fetches
     mutable source from `main`;
   - the four installer files are auxiliary assets and do not appear as
     artifacts or targets in `release-manifest.json`;
   - `schema_version` is `1`, `product_id` is `eggpass`, `release_id` is the
     plain package version, and `source_revision` matches the full commit the
     tag peels to;
   - the manifest names exactly the five expected target assets, and each
     recorded size and lowercase SHA-256 matches the downloaded binary;
   - each `.sha256` sidecar validates against its binary, and the draft notes
     accurately describe the changelog and link to the release assets.

   Keep the release as a draft if any job or check fails. Correct and re-download
   an incorrect manifest before proceeding; never retag or overwrite a published
   version.

## Publish and verify

Publishing the reviewed draft makes the binaries public and triggers
`.github/workflows/publish-crates.yml`. That workflow checks that the package
version matches the tag, runs `cargo package --locked --allow-dirty`, and then
publishes `eggpass` to crates.io using the repository secret. Immediately
before publishing, obtain fresh explicit human authorization for the exact tag
and its crates.io consequence:

```sh
set -eu
release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
release_tag="v${release_version}"
gh release edit "$release_tag" --repo eggstack/eggpass --draft=false
```

After publication, watch the release-triggered crates.io workflow through its
publish step. Confirm that the GitHub release is public and the exact version
is available on crates.io. If possible, run `eggpass update --check` separately
as a consumer-side check.

## Manifest and integrity

Eggpack generates the `release-manifest.json` file using the Eggpack release
manifest v1 schema. Its `schema_version` is `1`; artifact sizes are exact byte
counts and `sha256` values are 64 lowercase hexadecimal characters. The
manifest SHA-256 checks integrity, not authenticity: this release flow does not
sign the manifest or binaries.

## Recovery

- A failed build, missing asset, bad manifest, size, digest, source revision,
  version, or release note blocks publication; leave the draft unpublished.
- If only the draft manifest is wrong and all binaries and sidecars are
  independently verified, correct that draft asset and repeat the download and
  manifest checks.
- Never move or overwrite an existing published tag or release. A published
  crate version cannot be replaced; fixes require a new version.

## Independently versioned auth releases

### Rust crate

`eggpass-auth` in `crates/eggpass-auth/Cargo.toml` is versioned separately from
the Eggpass CLI. Its release tag is `eggpass-auth-vX.Y.Z`. The
`publish-eggpass-auth.yml` workflow checks the tag against the crate version
and runs `cargo package`. A manual dispatch on an auth tag is verification-only;
crates.io publication runs only after a human publishes a GitHub release for
that exact tag. Publish the crate before the next Eggpass crates.io release
that depends on it, so Cargo can resolve the dependency.

### Python distribution

The Python package in `python/eggpass-auth` has its own version and uses the
`eggpass-auth-python-vX.Y.Z` tag. Keep `pyproject.toml` and its `Cargo.toml`
versions equal. The PyPI workflow validates five stable-ABI wheels (Linux
x86_64/aarch64, macOS x86_64/arm64, Windows x86_64) and an sdist; CI tests
them on CPython 3.11–3.15. Configure the PyPI Trusted Publisher and protected
GitHub `pypi` environment before first publication. A published matching GitHub
release triggers the verified-files publish job using PyPI Trusted Publishing.
