# Releasing

How a release is cut. This is a maintainer document; users do not need it.

The rule the whole process exists to enforce: **the workflow stages a draft, a
human publishes it.** An unreviewed set of bytes never becomes "the latest
release" that every installed `eggpass` then pulls.

## What you need

- Push access to `eggstack/eggpass`.
- The `CARGO_REGISTRY_TOKEN` repository secret, which must be a crates.io
  API token with the `publish` scope for the package being released (`eggpass`
  or `eggpass-auth`). Without it, the crates.io step fails, but the GitHub
  release is unaffected.
- The release workflow is generated from `release/eggpack/`. Its pinned Eggpack
  revision is in `github-policy.json`; CI checks the generated workflow for drift.

```sh
gh secret set CARGO_REGISTRY_TOKEN --repo eggstack/eggpass
```

## The shape of a release

| Fact | Value |
| --- | --- |
| Git tag | `vX.Y.Z` (the version prefixed with `v`) |
| `Cargo.toml` version | `X.Y.Z` (without the `v`) |
| Manifest `release_id` | The same plain `X.Y.Z` version |
| Manifest `product_id` | `eggpass` |
| Manifest `source_revision` | the 40-character commit the tag peels to |
| Manifest file name | `release-manifest.json` |

The `release_id` has no `v` because `eggpass update` compares it with the
running build. A `v`-prefixed value makes that comparison fail. The Git tag
includes the `v` because release asset URLs use that format.

The asset names carry **no version** either, so that
`releases/latest/download/<name>` keeps working across releases:

```
eggpass-aarch64-apple-darwin
eggpass-aarch64-unknown-linux-gnu
eggpass-x86_64-apple-darwin
eggpass-x86_64-pc-windows-msvc.exe
eggpass-x86_64-unknown-linux-gnu
```

Five targets, matching the rest of the family. The Windows asset keeps its
`.exe` suffix because that is the file's real name; every target's `install`
value is `eggpass`.

## Cutting a release

1. **Bump and commit the version.** Update the root `eggpass` package version
   in `Cargo.toml`; keep independently versioned packages unchanged unless
   they are part of this release. Stage only the files intended for release,
   then run the checks and commit the prepared changes. Do not stage the whole
   worktree when unrelated changes are present. Push the release commit to
   `main` before tagging it.

   ```sh
   cargo test --workspace --all-targets --locked
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo fmt --all -- --check
   ```

2. **Tag the commit.** The workflow does not create tags.

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   git tag -a "$release_tag" -m "eggpass ${release_version}"
   git push origin "$release_tag"
   ```

   The release workflow does not create or push tags.

3. **Run the release workflow.** It is triggered manually after the tag exists.
   It verifies the source, builds and qualifies all five targets, writes the
   manifest and checksum sidecars, then stages a **draft** release.

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   gh workflow run release.yml --repo eggstack/eggpass -f "release_tag=$release_tag"
   ```

4. **Check the draft.**

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   gh release view "$release_tag" --repo eggstack/eggpass
   gh release download "$release_tag" --repo eggstack/eggpass --pattern '*' --dir /tmp/eggpass-check
   ```

   The manifest should name every asset with its exact size and digest:

   ```sh
   python3 -c "import json;m=json.load(open('/tmp/eggpass-check/release-manifest.json'));print(m['release_id'],len(m['targets']))"
   ```

   To check a digest by hand, on macOS:

   ```sh
   shasum -a 256 -c /tmp/eggpass-check/eggpass-aarch64-apple-darwin.sha256
   ```

5. **Publish the draft.** This is the human action.

   ```sh
   set -eu
   release_version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "eggpass"))')"
   release_tag="v${release_version}"
   gh release edit "$release_tag" --repo eggstack/eggpass --draft=false
   ```

   Publishing the release fires `.github/workflows/publish-crates.yml`, which
   publishes the crate to crates.io.

6. **Check the updater.** With a release published, the updater has
   something to install:

   ```sh
   eggpass update --check
   ```

   It should report the published release and the asset it would install.

## Independent auth crate release

`eggpass-auth` is versioned independently in
`crates/eggpass-auth/Cargo.toml`; do not use the root `Cargo.toml` version for
it. Root Eggpass releases keep the `vX.Y.Z` tag convention, while auth releases
use `eggpass-auth-vX.Y.Z` (for example, `eggpass-auth-v0.1.0`). The auth
workflow packages and publishes only `eggpass-auth`. Because the root Eggpass
crate depends on this library, publish the auth crate first before the next
Eggpass crates.io release; otherwise Cargo cannot resolve the dependency from
crates.io. The root package check patches `eggpass-auth` to the workspace path so CI can package-check Eggpass without a registry release of that dependency.

To verify the exact tagged source without creating a GitHub release, dispatch
the auth workflow against the tag with its default dry-run enabled:

```sh
gh workflow run publish-eggpass-auth.yml --repo eggstack/eggpass \
  --ref eggpass-auth-v0.1.0 -f dry_run=true
```

Dispatch is verification-only and cannot publish, even if `dry_run` is changed.
The workflow checks that the tag version matches the auth crate manifest and
runs `cargo package --locked -p eggpass-auth`.

After reviewing a successful dry run and rechecking crate-name availability
and dependency versions, publishing still requires a fresh, explicit human
authorization. Publishing a GitHub release for the exact auth tag is the
publication gate and triggers crates.io publication; this implementation work
does not authorize that public action.

## Independent Python PyPI release

The Python distribution in `python/eggpass-auth` is versioned independently of both the Rust auth crate and the Eggpass CLI. The CI and PyPI workflows build five stable-ABI wheels for GIL-enabled CPython: Linux x86_64/aarch64 with manylinux2014, macOS x86_64/arm64, and Windows x86_64. CI installs and tests each wheel on CPython 3.11–3.15. The workflows do not build musllinux, free-threaded Python, or 32-bit ARM wheels. A compatible 64-bit Raspberry Pi OS may be able to use the Linux aarch64 wheel, but that combination is not separately qualified.

The Python tag is `eggpass-auth-python-vX.Y.Z` (for example, `eggpass-auth-python-v0.1.0`). Keep the version in `python/eggpass-auth/pyproject.toml` equal to the package version in `python/eggpass-auth/Cargo.toml`. Do not use `eggpass-auth-vX.Y.Z` (the Rust auth release) or `vX.Y.Z` (the Eggpass CLI release).

### One-time PyPI and GitHub setup

Before the first release:

1. Recheck that `eggpass-auth` is available and confirm the PyPI project owner. The PyPI endpoint returned `Not Found` on 2026-11-19; this is only a point-in-time lookup and does not reserve the project name.
2. Configure a PyPI Trusted Publisher for GitHub Actions with repository owner `eggstack`, repository `eggpass`, workflow file `publish-eggpass-auth-pypi.yml`, and environment name `pypi`. If the project does not exist yet, configure a pending publisher for the new project.
3. Create the GitHub `pypi` environment and require an authorized reviewer. The publish job receives `id-token: write`; build and validation jobs do not. Do not add a long-lived PyPI token.

### Candidate and publication procedure

1. Review and commit only the intended Python package, CI, publisher, and documentation changes; preserve unrelated worktree files. Require the Python CI workflow to pass, including wheel-install tests for every platform/Python combination and sdist build/install tests on CPython 3.11–3.15. Do not claim a platform combination as release-qualified unless its required CI checks pass; Raspberry Pi OS compatibility remains a separate device-specific check.
2. Run the publisher workflow manually with `workflow_dispatch` for a verification-only build, or prepare the exact versioned source commit and tag. Manual dispatch builds and validates files but cannot publish.
3. Create a draft GitHub release for `eggpass-auth-python-vX.Y.Z`. Publishing that GitHub release starts the Python-only workflow; it checks the tag/version, builds the five wheels and sdist, validates metadata/tags/contents, and uploads the validated files as an Actions artifact.
4. Inspect the completed build and validated artifact in the Actions run. The final publish job is held at the protected `pypi` environment. Approve it only after reviewing the exact files and, for the first public upload, after fresh explicit authorization. Publishing the GitHub release or approving that job is an externally visible release action; this implementation does not authorize either.

The publisher job has no checkout or build step and uses `pypa/gh-action-pypi-publish@release/v1` with GitHub OIDC. PyPI publish attestations are produced by the official action by default. The Python tag does not match the root `vX.Y.Z` crates publisher or the Rust auth `eggpass-auth-vX.Y.Z` publisher. A published PyPI file/version cannot be overwritten; correct mistakes with a new version or yank the release where appropriate.

## The manifest

Eggpack generates the `release-manifest.json` file using eggstack's
release-manifest v1 schema, defined by
[`eggpack-manifest`](https://crates.io/crates/eggpack-manifest). Do not hand-edit
a release manifest. Its required fields are:

| Field | Required value |
| --- | --- |
| `schema_version` | `1` |
| `product_id` | `eggpass` |
| `release_id` | The package version as plain `X.Y.Z`, without `v` |
| `source_revision` | The full 40-character commit SHA that the release tag peels to |
| `targets[].target` | The exact Rust target triple |
| `targets[].form.kind` | `direct` |
| `targets[].form.artifact.name` | The exact release asset name |
| `targets[].form.artifact.size` | The asset's exact byte count |
| `targets[].form.artifact.sha256` | The full 64-character lowercase SHA-256 digest |
| `targets[].form.install` | `eggpass` |

The updater checks the manifest version and the selected asset's size and digest
before replacing the binary.

## Integrity, not authenticity

The SHA-256 in the manifest checks **integrity** and detects a corrupted
download. It does **not** prove who published the download because this chain
has no signature. State this limit wherever the updater is described.

## Rollback

`eggpass update 0.1.0` installs an exact version, so a rollback is an
ordinary update to an older release. There is no separate rollback command,
and no release is ever deleted or overwritten: publish a new one.

## If a release goes wrong

- **A wrong digest.** The updater refuses the candidate before it runs, so a
  bad digest means a stale manifest. Rebuild and restage the draft.
- **A missing target.** The updater reports that the canonical target is not
  in the manifest. Check that the build matrix includes `ubuntu-24.04-arm`
  for `aarch64-unknown-linux-gnu`.
- **A version mismatch.** The workflow refuses to stage a release whose
  `Cargo.toml` version is not the tag's version. Bump the version, commit,
  and tag the new commit.
