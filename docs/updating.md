# Updating

```sh
eggpass update            # install the latest published release
eggpass update 0.1.0      # install an exact version
eggpass update --check    # show the proposed update without changing anything
```

`eggpass update` verifies a release before replacing the binary. It does not
restart a running server. Restart manually when you are ready; restarting ends
active streams.

## Verification

For the selected release, the updater:

1. Refuses to replace a binary managed by Cargo. Cargo must update its own
   installation.
2. Fetches the release manifest and the matching binary for this system.
3. Checks the download's size and SHA-256 against the manifest.
4. Runs the candidate with `--version` and checks the reported version.
5. Replaces the binary under a lock and rolls back if the operation fails.

The updater replaces only the binary it can prove it owns. If ownership or a
check cannot be established, it refuses the update.

## Integrity limit

The SHA-256 check detects corruption or a mismatch with the manifest. It does
not prove who published the manifest or binary. Releases are not signed by this
update process.

## Cargo installations

If you installed with Cargo, the updater refuses to replace that binary and
prints a command with `--locked --force --root` set to the detected Cargo
installation. Run that command unchanged so Cargo updates the correct
installation. For a default crates.io installation, the equivalent is:

```sh
cargo install eggpass --locked --force
```

To reinstall from the current repository checkout instead:

```sh
cargo install --path . --locked --force
```

## Exact versions and rollback

An exact version can install an older release, so rollback uses the same
command:

```sh
eggpass update 0.1.0
```

The updater does not delete or overwrite published releases.
