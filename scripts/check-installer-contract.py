#!/usr/bin/env python3
"""Static release-contract checks for Eggpass's installer presentation."""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPECTED_TARGETS = {
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
}
EXPECTED_AUXILIARY = {
    "install.sh",
    "install.ps1",
    "install-exact.sh",
    "install-exact.ps1",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def main() -> int:
    distribution = tomllib.loads((ROOT / "release/eggpack/distribution.toml").read_text())
    targets = distribution["targets"]
    triples = {target["triple"] for target in targets}
    require(triples == EXPECTED_TARGETS, f"distribution target drift: {sorted(triples)}")
    require(distribution["product"]["id"] == "eggpass", "unexpected distribution product")

    binary_assets: set[str] = set()
    checksum_assets: set[str] = set()
    for target in targets:
        triple = target["triple"]
        asset = target["asset"]["asset"].replace("{product}", "eggpass").replace("{target}", triple)
        sidecar = target["checksum"]["sidecar"].replace("{asset}", asset)
        require(asset not in binary_assets, f"duplicate binary asset: {asset}")
        binary_assets.add(asset)
        checksum_assets.add(sidecar)

    presentation = json.loads((ROOT / "release/eggpack/installer-presentation.json").read_text())
    require(presentation == {
        "schema_version": 1,
        "mode": {
            "product_wrappers": {
                "posix_source": "packaging/install.sh",
                "powershell_source": "packaging/install.ps1",
                "generated_posix_name": "install-exact.sh",
                "generated_powershell_name": "install-exact.ps1",
            }
        },
    }, "installer presentation must stage the reviewed wrappers and exact bootstraps")

    auxiliary = EXPECTED_AUXILIARY
    release_members = binary_assets | checksum_assets | {"release-manifest.json"}
    require(not (auxiliary & release_members), "installer assets must remain outside the binary manifest contract")

    for relative in ("packaging/install.sh", "packaging/install.ps1"):
        path = ROOT / relative
        require(path.is_file() and not path.is_symlink(), f"installer source is missing or unsafe: {relative}")
        require(path.stat().st_size <= 1_048_576, f"installer exceeds Eggpack's 1 MiB bound: {relative}")
        text = path.read_text(encoding="utf-8")
        require("eggstack/eggpass" in text, f"wrong repository in {relative}")
        require("latest/download" in text, f"latest release URL missing from {relative}")
        require("--version" in text or "Version" in text, f"pinned-version option missing from {relative}")
        require(".sha256" in text, f"checksum sidecar handling missing from {relative}")
        script_targets = EXPECTED_TARGETS if relative.endswith(".sh") else {"x86_64-pc-windows-msvc"}
        for triple in script_targets:
            require(triple in text, f"target mapping missing from {relative}: {triple}")
        require("raw.githubusercontent.com" not in text, f"installer must not fetch a mutable source file: {relative}")

    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    for url in (
        "https://github.com/eggstack/eggpass/releases/latest/download/install.sh",
        "https://github.com/eggstack/eggpass/releases/latest/download/install.ps1",
        "https://github.com/eggstack/eggpass/releases/download/vX.Y.Z/install.sh",
        "https://github.com/eggstack/eggpass/releases/download/vX.Y.Z/install.ps1",
    ):
        require(url in readme, f"install documentation is missing release URL: {url}")

    draft = json.loads((ROOT / "release/eggpack/github-template.json").read_text())
    for asset in sorted(auxiliary):
        require(asset in draft["body"], f"draft review text omits auxiliary asset: {asset}")
    print("Eggpass installer release contract is consistent")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, UnicodeError, json.JSONDecodeError, tomllib.TOMLDecodeError, KeyError, TypeError, ValueError) as exc:
        print(f"installer contract check failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
