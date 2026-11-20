#!/usr/bin/env python3
"""Synchronize or validate the self-contained eggpass-auth source snapshot."""

from __future__ import annotations

import argparse
import filecmp
import shutil
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = PACKAGE_ROOT.parents[1]
CANONICAL = REPOSITORY_ROOT / "crates" / "eggpass-auth"
VENDORED = PACKAGE_ROOT / "vendor" / "eggpass-auth"
ROOT_FILES = ("Cargo.toml", "README.md", "LICENSE")
SOURCE_DIRS = ("src", "examples")


def files_under(root: Path) -> set[Path]:
    files = {Path(name) for name in ROOT_FILES if (root / name).is_file()}
    for directory in SOURCE_DIRS:
        source_dir = root / directory
        if source_dir.exists():
            files.update(path.relative_to(root) for path in source_dir.rglob("*") if path.is_file())
    return files


def check() -> int:
    expected = files_under(CANONICAL)
    actual = files_under(VENDORED)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    changed = sorted(
        path for path in expected & actual if not filecmp.cmp(CANONICAL / path, VENDORED / path, shallow=False)
    )
    if not (missing or extra or changed):
        print("vendored eggpass-auth source matches crates/eggpass-auth")
        return 0
    if missing:
        print("missing vendored files: " + ", ".join(map(str, missing)))
    if extra:
        print("extra vendored files: " + ", ".join(map(str, extra)))
    if changed:
        print("out-of-date vendored files: " + ", ".join(map(str, changed)))
    print("run `python tools/sync_core.py --sync` from python/eggpass-auth to refresh the snapshot")
    return 1


def sync() -> int:
    for relative_path in sorted(files_under(CANONICAL)):
        destination = VENDORED / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(CANONICAL / relative_path, destination)
    return check()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--check", action="store_true", help="fail if the snapshot differs (default)")
    action.add_argument("--sync", action="store_true", help="copy canonical crate files into vendor/")
    args = parser.parse_args()
    if not CANONICAL.is_dir():
        parser.error("source synchronization requires the eggpass monorepo checkout")
    return sync() if args.sync else check()


if __name__ == "__main__":
    raise SystemExit(main())
