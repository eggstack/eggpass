# Updating

```sh
eggpass update            # install the latest published release
eggpass update 0.1.0      # install an exact version
eggpass update --check    # show the proposed update without changing anything
```

`eggpass update` verifies a release before replacing the binary. It does not
restart a running server. Restart manually when you are ready; restarting ends
active streams.

## Installers

The Bash installer supports Linux and macOS on the published targets:

```sh
curl -fsSL https://github.com/eggstack/eggpass/releases/latest/download/install.sh | bash
curl -fsSL https://github.com/eggstack/eggpass/releases/download/vX.Y.Z/install.sh | bash -s -- --version X.Y.Z
```

On Windows, download the release script, then run it in PowerShell:

```powershell
$installer = Join-Path $env:TEMP "eggpass-install.ps1"
Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/eggstack/eggpass/releases/latest/download/install.ps1" -OutFile $installer
& $installer
```

For a pinned Windows release, use the matching `vX.Y.Z` script URL and pass
`-Version X.Y.Z`. Bash requires `curl` and `sha256sum` or `shasum`; PowerShell
5.1 or later is supported. Prebuilt installs need no Rust; exact-version Cargo
fallback requires Cargo and retries only the selected version. The default
destinations are `~/.local/bin` and `%LOCALAPPDATA%\Eggpass`. Bash can append a
safe PATH block to a Bash/Zsh profile; PowerShell prints PATH guidance, and the
Windows directory can be added to the user PATH or invoked by its full path.
System installs require explicit elevation (`--system` or `-System`).

## Installation-aware updates

The updater preserves the installation method:

- A binary installed by the Eggpass installer is self-managed. It uses Eggup's
  existing release-manifest transaction, including exact size/digest checks,
  candidate identity validation, lock ownership, atomic commit, and rollback.
- A Cargo-managed install is updated by Cargo only when `.crates.toml` proves
  the root, installed version, package, and default crates.io source. The
  updater first resolves an exact Eggpass release from GitHub, then requests
  that exact version with `--locked --force --root`; it never directly replaces
  a Cargo-owned binary. If the release was just published and crates.io has not
  indexed that version yet, retry later; no older or floating version is used.
- On Windows, the running executable cannot be safely replaced by an in-process
  Cargo update, so Eggpass prints the exact Cargo command to run after it exits.
- Path, Git, alternate-registry, malformed, or unrecognized Cargo sources are
  not converted to crates.io installs. They remain manual-only, and `--check`
  does not mutate any installation.

If ownership or source cannot be established, the updater refuses mutation.
For a successful Cargo update it re-reads Cargo's record and runs the installed
`eggpass --version`; success is reported only when both identify the requested
release.

## Integrity limit

The SHA-256 check detects corruption or a mismatch with the manifest. It does
not prove who published the manifest or binary. Releases are not signed by this
update process.

## Cargo installation sources

Only a positively identified default crates.io install is eligible for automatic
Cargo delegation. Cargo's package record remains authoritative for its root and
source. If the install came from a local path, Git, an alternate registry, or
metadata that Eggpass cannot interpret, update it using the original Cargo
source options; Eggpass will not silently switch it to crates.io.

To reinstall from the current repository checkout instead:

```sh
cargo install --path . --locked --force
```

## Exact versions and rollback

An exact version selects one published release, but `eggpass update` refuses
to downgrade a running build. To install an older stable release, use that
release's pinned installer URL and version instead. The updater does not delete
or overwrite published releases.
