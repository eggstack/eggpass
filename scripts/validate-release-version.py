#!/usr/bin/env python3
"""Check that a release candidate reports the package's Cargo version."""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

MAX_MANIFEST_BYTES = 1_000_000
MAX_OUTPUT_BYTES = 8_192
TIMEOUT_SECONDS = 10


def fail(reason: str) -> int:
    print(f"validation failed: {reason}", file=sys.stderr)
    return 1


def package_version(root: Path) -> str | None:
    manifest = root / "Cargo.toml"
    try:
        if manifest.stat().st_size > MAX_MANIFEST_BYTES:
            return None
        text = manifest.read_bytes().decode("utf-8")
    except (OSError, UnicodeError):
        return None

    package = re.search(r"(?ms)^\[package\][ \t]*\r?\n(.*?)(?=^\[|\Z)", text)
    if package is None:
        return None
    version = re.search(
        r'(?m)^version\s*=\s*"([0-9A-Za-z.+-]+)"\s*$', package.group(1)
    )
    return version.group(1) if version else None


def child_environment() -> dict[str, str]:
    allowed = (
        "PATH",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "SYSTEMROOT",
        "SystemRoot",
        "COMSPEC",
        "PATHEXT",
        "WINDIR",
    )
    return {key: value for key in allowed if (value := os.environ.get(key))}


def main() -> int:
    if len(sys.argv) != 2:
        return fail("expected one candidate path")

    root = Path(__file__).resolve().parent.parent
    expected_version = package_version(root)
    if expected_version is None:
        return fail("could not read the package version")

    candidate = Path(sys.argv[1])
    try:
        if candidate.is_symlink():
            return fail("candidate is not a regular file")
        candidate = candidate.resolve(strict=True)
        if not candidate.is_file():
            return fail("candidate is not a regular file")
    except OSError:
        return fail("candidate is not available")

    try:
        result = subprocess.run(
            [str(candidate), "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=TIMEOUT_SECONDS,
            check=False,
            shell=False,
            cwd=root,
            env=child_environment(),
        )
    except (OSError, subprocess.TimeoutExpired):
        return fail("candidate did not complete the version check")

    if result.returncode != 0:
        return fail("candidate returned a failure status")
    if len(result.stdout) > MAX_OUTPUT_BYTES or len(result.stderr) > MAX_OUTPUT_BYTES:
        return fail("candidate output exceeded the limit")
    try:
        lines = result.stdout.decode("utf-8").splitlines()
        result.stderr.decode("utf-8")
    except UnicodeError:
        return fail("candidate output was not valid UTF-8")

    if lines != [f"eggpass {expected_version}"] or result.stderr:
        return fail("candidate version did not match Cargo.toml")
    print("validation passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
