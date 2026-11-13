# Updating

```sh
eggpass update            # the latest published release
eggpass update 0.2.0      # one exact version — which is also the rollback
eggpass update --check    # report what would happen, change nothing
```

**Exit codes:** `0` updated, or `--check` completed · `1` the release is
unavailable, the digest did not match, the candidate failed to prove its
version, or this installation may not be updated.

## What it does

The update is performed by [`eggup`](https://github.com/eggstack/eggup), the
eggstack family's verified updater. `eggpass` owns the *policy* — which
release is acceptable, whether this installation may be updated at all, and
what the candidate must say about itself. `eggup` owns the mechanics.

The sequence is:

1. **Classify the installation.** A binary that Cargo owns is refused: Cargo
   owns its bookkeeping, and replacing it behind Cargo's back would leave
   `cargo install --list` and `cargo uninstall` lying. The command prints the
   `cargo install` line to run instead.
2. **Fetch the release manifest** for the wanted release.
3. **Check the SHA-256** of what was downloaded against the manifest.
4. **Make the candidate prove its version** — the downloaded binary is run in
   a bounded way and must report the version it claims to be.
5. **Replace the binary** under a lock, with a rollback if anything goes
   wrong.

The ownership proof is the file's exact SHA-256, re-read immediately before
the commit. The file name, the `PATH` order and the directory it lives in
prove nothing.

## Integrity, not authenticity

The SHA-256 check is an **integrity** check. It proves the download was not
corrupted — it does **not** prove that the download came from us. There is no
signature in this chain.

## The release manifest

A release publishes `eggpass-release-manifest.json` beside the binaries it
names. The manifest is the whole input: it carries the release id, the asset
name for each target, the asset's exact size, and its SHA-256.

It is read from one of two places, depending on whether a version was named:

```
https://github.com/eggstack/eggpass/releases/latest/download/eggpass-release-manifest.json
https://github.com/eggstack/eggpass/releases/download/v{version}/eggpass-release-manifest.json
```

The schema is the eggstack family's release-manifest v1 — the same one
[`eggpack`](https://github.com/eggstack/eggpack) produces and
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

Rules that matter:

- `release_id` is the **plain version**, with no `v` — `0.1.0`, not `v0.1.0`.
  The release tag is `v0.1.0`.
- The **asset name** in the manifest is the exact name of the release asset,
  and the asset names are **version-free**:
  `eggpass-{canonical-triple}`, e.g. `eggpass-aarch64-apple-darwin`. A
  version in the asset name would break `releases/latest/download`.
- The **canonical triple** is the one the binary reports for its own host:
  `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`,
  `x86_64-pc-windows-msvc`.
- `install` is the name the artifact takes on disk: `eggpass`.
- `size` is the exact byte count of the asset, and `sha256` is its digest in
  lowercase hex.

How a release is actually cut is in [releasing.md](releasing.md).

## A running server keeps the old binary

`eggpass update` replaces the binary but does not restart the daemon. The
running server keeps serving with the old code until it is restarted.

That is deliberate: an implicit restart during an update would cut live
streams. `eggpass update` says so rather than doing it for you — run
`eggpass restart` when it suits you.
