# Releasing

How a release is cut. This is a maintainer document; users do not need it.

The rule the whole process exists to enforce: **the workflow stages a draft, a
human publishes it.** An unreviewed set of bytes never becomes "the latest
release" that every installed `eggpass` then pulls.

## What you need

- Push access to `eggstack/eggpass`.
- The `CARGO_REGISTRY_TOKEN` repository secret — a crates.io API token with
  the `publish` scope for this crate. Without it the crates.io step fails,
  and only that step: the GitHub release is unaffected.

```sh
gh secret set CARGO_REGISTRY_TOKEN --repo eggstack/eggpass
```

## The shape of a release

| Fact | Value |
| --- | --- |
| Git tag | `v0.1.0` — the version, with a `v` |
| `Cargo.toml` version | `0.1.0` — the version, without a `v` |
| Manifest `release_id` | `0.1.0` — **no `v`** |
| Manifest `product_id` | `eggpass` |
| Manifest `source_revision` | the 40-character commit the tag peels to |
| Manifest file name | `eggpass-release-manifest.json` |

The `release_id` carries no `v` because that is what `eggpass update`
compares against the running build. A `v`-prefixed `release_id` makes the
updater refuse to compare the two, and it reports it in a confusing way. The
**tag** carries the `v`, because that is the URL the assets are served from.

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

1. **Bump the version** in `Cargo.toml`, commit, and push.

   ```sh
   # edit the version in Cargo.toml
   cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
   git commit -am "chore: release 0.1.0"
   git push origin main
   ```

2. **Tag the commit.** The workflow does not create tags.

   ```sh
   git tag -a v0.1.0 -m "eggpass 0.1.0"
   git push origin v0.1.0
   ```

   Pushing the tag starts `.github/workflows/release.yml`.

3. **Wait for the workflow.** It verifies the tree, resolves the release
   identity, builds the five binaries, writes the manifest, and stages a
   **draft** release with every asset attached.

   If the tag was pushed and the workflow missed it, run it by hand:

   ```sh
   gh workflow run release.yml -f version=0.1.0
   ```

4. **Check the draft.**

   ```sh
   gh release view v0.1.0 --repo eggstack/eggpass
   gh release download v0.1.0 --repo eggstack/eggpass --pattern 'eggpass-*' --dir /tmp/eggpass-check
   ```

   The manifest should name every asset with its exact size and digest:

   ```sh
   python3 -c "import json;m=json.load(open('eggpass-release-manifest.json'));print(m['release_id'],len(m['targets']))"
   ```

   To check a digest by hand, on macOS:

   ```sh
   shasum -a 256 eggpass-aarch64-apple-darwin
   ```

5. **Publish the draft.** This is the human action.

   ```sh
   gh release edit v0.1.0 --repo eggstack/eggpass --draft=false
   ```

   Publishing the release fires `.github/workflows/publish-crates.yml`, which
   publishes the crate to crates.io.

6. **Check the updater.** With a release published, the updater has
   something to install:

   ```sh
   eggpass update --check
   ```

   It should report the published release and the asset it would install.

## The manifest

The manifest is the whole input the updater reads. It is the eggstack
family's release-manifest v1 — the schema
[`eggpack-manifest`](https://crates.io/crates/eggpack-manifest) defines:

```json
{
  "schema_version": 1,
  "product_id": "eggpass",
  "release_id": "0.1.0",
  "source_revision": "0f1e2d3c4b5a69788796a5b4c3d2e1f0a9b8c7d6",
  "targets": [
    {
      "target": "aarch64-apple-darwin",
      "form": {
        "kind": "direct",
        "artifact": {
          "name": "eggpass-aarch64-apple-darwin",
          "size": 4088320,
          "sha256": "9f2c…64 lowercase hex characters…"
        },
        "install": "eggpass"
      }
    }
  ]
}
```

The rules the reader enforces:

- `schema_version` is exactly `1`.
- `release_id` is a plain `X.Y.Z`.
- Every target's `artifact.name` is the exact asset name, `artifact.size` is
  its exact byte count, and `artifact.sha256` is its digest in lowercase hex.
- `install` is the name the artifact takes on disk: `eggpass`.

Unknown fields fail closed, and so does an unsupported schema version.

## Integrity, not authenticity

The SHA-256 in the manifest is an **integrity** fact. It proves the download
was not corrupted. It does **not** prove that the download came from us —
there is no signature in this chain. Say so plainly wherever the updater is
described.

## Rollback

`eggpass update 0.1.0` installs an exact version, so a rollback is an
ordinary update to an older release. There is no separate rollback command,
and no release is ever deleted or overwritten: publish a new one.

## If a release goes wrong

- **A wrong digest.** The updater refuses the candidate before it runs, so a
  bad digest means a stale manifest. Rebuild and restage the draft.
- **A missing target.** The updater reports that the canonical target is not
  in the manifest. Check the build matrix — `ubuntu-24.04-arm` must be
  available for `aarch64-unknown-linux-gnu`.
- **A version mismatch.** The workflow refuses to stage a release whose
  `Cargo.toml` version is not the tag's version. Bump the version, commit,
  and tag the new commit.
