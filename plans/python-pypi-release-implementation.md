# Python `eggpass-auth` — PyPI Release Implementation Plan

**Status:** Implementation prepared; local checks passed on macOS arm64 / CPython 3.14. The full GitHub target matrix, GitHub/PyPI publisher configuration, reviewed commit/tag/release, Raspberry Pi device check, and first public upload remain outstanding. This document and the implementation do not authorize publication.

**Research checked:** 2026-11-17.

**Scope:** Prepare the independent `python/eggpass-auth` distribution for a first PyPI release, with documented Python support, reviewed wheel coverage, and a secure, separate publish path. This extends the packaging/validation work in [`python-pyo3-bindings-implementation.md`](python-pyo3-bindings-implementation.md); it does not redo the binding implementation.

## Goal

Publish a reviewed `eggpass-auth` Python release with accurate package metadata and user documentation, tested distributions for a clearly defined set of CPython/platform targets, and an isolated PyPI Trusted Publishing workflow. Keep the Rust crate and Eggpass CLI release identities and workflows independent.

This implementation prepares release-readiness files in the worktree. It does not authorize a public PyPI upload; the first upload requires fresh, explicit confirmation immediately before publication.

## Pre-implementation baseline and known gaps

The following records the state found during research, before the implementation described below.

- `python/eggpass-auth/pyproject.toml` declares package `eggpass-auth` version `0.1.0`, `requires-python = ">=3.9"`, `readme = "README.md"`, and Python classifiers through 3.14. The README reference is correctly relative to the package root; keep it pointed at the package's auth-library guide rather than the root CLI README.
- The package README already has Python authorization-code, device-flow, and profile examples. `docs/README.md` does not currently link to that guide. Link the guide; do not duplicate or replace the root CLI README.
- `.github/workflows/python-auth.yml` tests Ubuntu x86_64 on Python 3.9 and 3.14 and separately builds/installs an sdist. The package README reports a local macOS arm64 / CPython 3.14 wheel and sdist install, but the GitHub workflow has not run; Windows and other architectures remain unqualified.
- The current binding enables PyO3 `abi3-py39` and `extension-module`, and CI pins Maturin 1.9.5. The stable ABI can reduce the number of per-Python-version wheels for GIL-enabled CPython, but each OS/architecture still needs its own wheel. PyO3's current guide recommends migrating away from `extension-module`; review this alongside the CI/build changes.
- The existing auth Rust publisher is crates.io-only and follows `eggpass-auth-vX.Y.Z`. There is no separate PyPI publisher in the checked workflow set. Do not reuse the Rust auth tag for Python releases.
- The observed worktree contains numerous modified and untracked files, including the Python package and workflows. Review and commit the intended release inputs explicitly; never stage the entire worktree as part of this plan.
- The live `https://pypi.org/pypi/eggpass-auth/json` endpoint returned `Not Found` on 2026-11-17. Treat this only as a point-in-time availability check, not a reservation or guarantee.

## Approved first-release policy

The user selected these support boundaries on 2026-11-17; metadata, docs, and CI should stay aligned with them.

1. **Python policy:** Declare and test GIL-enabled CPython 3.11–3.15. Set `requires-python = ">=3.11"` and PyO3's stable ABI floor to `abi3-py311`; test the same `cp311-abi3` wheels on each supported CPython minor. Require Maturin `>=1.16.0,<2.0` and pin CI/release builds to 1.16.0.
2. **Free-threaded CPython 3.15:** Exclude 3.15t from the first-release claim. Reconsider only after separately building/testing the required `abi3t` wheels and auditing native thread-safety.
3. **Baseline PyPI wheels:** Target Linux x86_64 and aarch64, macOS x86_64 and arm64, and Windows x86_64, plus an sdist. Use the manylinux2014 compatibility target for Linux wheels. Musllinux and Windows arm64 are not included.
4. **32-bit ARM / Raspberry Pi:** The selected initial path is a Linux aarch64 wheel plus an sdist. Do not claim armv7l/armv6l or universal Raspberry Pi support; a compatible 64-bit Pi OS must still be checked on-device before claiming it as qualified. sdist and piwheels builds for Pi are not guaranteed until tested. Maturin supports additional targets, but PyPI ARM wheels must demonstrate portability beyond Raspberry Pi hardware.
5. **Release identity:** Keep the current Python package version `0.1.0` aligned between `pyproject.toml` and the binding crate manifest. Use `eggpass-auth-python-vX.Y.Z`, separate from `eggpass-auth-vX.Y.Z` and the root `vX.Y.Z` CLI tag, with a protected GitHub `pypi` environment.
6. **README/docs:** Keep `python/eggpass-auth/README.md` as the PyPI long-description source, preserve its Python auth-library examples, and link it from the docs index. Keep the root README focused on the CLI.

The PyPI JSON endpoint returned `Not Found` when rechecked on 2026-11-17. This is not a name reservation or proof that the project will remain available. No PyPI Publisher or GitHub Environment configuration, tag, commit, GitHub release, TestPyPI upload, or PyPI upload has been performed by this implementation.

## Implementation progress

- **Stage 0 — Policy:** The user approved CPython 3.11–3.15 (GIL-enabled), the five-platform wheel matrix, a Linux aarch64 wheel plus sdist, and no 32-bit ARM/Pi or free-threaded claim. The PyPI name endpoint was rechecked; ownership and account settings still need maintainer action.
- **Stage 1 — Metadata/docs:** Updated Python minimum/classifiers, `abi3-py311`, Maturin build requirement, project URLs, package README support boundaries, docs index, and release instructions. The package-local auth guide remains the PyPI README; the root README was not changed.
- **Stage 2 — CI:** Authored wheel build/install tests for all five target platforms across CPython 3.11–3.15, plus isolated sdist build/install tests across those minors. Locally on macOS arm64 / CPython 3.14, Rust fmt/clippy/tests, Maturin 1.16.0 wheel and sdist builds, `twine check`, clean wheel/sdist installs, imports, `pip check`, mypy, pytest (4 passed each), and `maturin develop` passed. `actionlint` and `yamllint` pass. The GitHub matrix has not run, and no Raspberry Pi device or piwheels build was tested.
- **Stage 3 — Publisher:** Added a Python-only publisher with version/tag checks, read-only build/validation jobs, artifact/tag validation, verification-only manual dispatch, and OIDC permission isolated to the publish job. `actionlint`/`yamllint` pass. PyPI Trusted Publisher and protected GitHub environment/reviewer settings are not configured.
- **Stage 4 — Candidate:** Not complete. These files have not been committed or tagged; no GitHub release or TestPyPI/PyPI upload was created. The first upload remains gated on full CI/artifact review and fresh explicit user authorization.

## Staged implementation

### Stage 0 — Approve policy and confirm release boundary

- Recheck PyPI project-name availability immediately before implementation; confirm who owns/configures the project and its Trusted Publisher. PyPI supports configuring a pending Trusted Publisher that creates a new project on its first successful upload.
- Approve the Python floor, supported Python builds (GIL-enabled only versus free-threaded 3.15), initial OS/architecture matrix, and Raspberry Pi/ARMv6 expectations. Record them once and use the same policy in metadata, docs, CI, and wheel filenames.
- Confirm the Python package version to publish and the independent tag format. Confirm root and crates.io workflows ignore Python release tags.
- Inventory the exact package/workflow/docs files intended for commit and keep unrelated worktree changes untouched.

**Exit:** Name/ownership, version policy, platform scope, tag boundary, and non-publication implementation scope are explicit.

### Stage 1 — Align metadata, README, and distribution contents

- Update `requires-python`, Python classifiers, and PyO3 stable-ABI floor to the approved minimum; add the Python 3.15 classifier if 3.15 is in the supported range. Do not add a free-threaded claim unless Stage 0 selected it.
- Keep `readme = "README.md"` pointing to the package-local guide. Link the Python auth guide from `docs/README.md`; keep the root README focused on the Eggpass CLI.
- Review package classifiers, project URLs, license metadata/files, and version fields for completeness and consistency. Verify the sdist contains the package README, license, Rust binding source, lockfile as required, and self-contained vendored `eggpass-auth` source.
- Remove PyO3's deprecated `extension-module` feature. Require Maturin `>=1.16.0,<2.0` and pin CI/release builds to 1.16.0; validate Rust tests, wheel builds, sdist installs, and local development against that version.
- Add no unnecessary user-facing promises: distinguish compiled wheels, sdist/source builds, generic ARM, Raspberry Pi, and free-threaded Python support.

**Exit:** Package metadata and PyPI long description accurately describe the released API and supported targets; an isolated sdist includes everything needed to build without repository-only paths.

### Stage 2 — Expand and execute validation CI

- Extend the Python test matrix to every supported minor version (recommended 3.11, 3.12, 3.13, 3.14, 3.15, GIL-enabled). Build the stable-ABI wheel per OS/architecture, then install and test it on each supported Python minor version; do not create redundant per-minor wheels unless the chosen ABI requires them.
- Build/install native wheels on Linux x86_64 and aarch64, macOS x86_64 and arm64, and Windows x86_64. Use Maturin's supported manylinux build path and a maintained Maturin action where appropriate. Keep build-only cross-compilation distinct from import/tests on a target runtime; use native runners or documented emulation where feasible.
- Run the existing Rust formatting/lint/tests, Python type checks, local-stub Python tests, and clean sdist build/install. Run the workflow on GitHub and record actual results; configuration alone is not qualification.
- Add armv7l and/or armv6l testing only for the explicitly selected ARM support path. Test on actual or representative generic ARM systems, not only Raspberry Pi. Before claiming Pi support from the sdist, confirm that piwheels can build this Rust package and verify a released version's build outcome; separately test 64-bit Pi OS against the aarch64 PyPI wheel.
- Check wheel tags, linked native dependencies and platform floors. Test clean installation/import on each target, not just wheel creation. Test the minimum Python version as well as 3.15.

**Exit:** All advertised interpreters and wheels have clean install/import and test evidence; sdist rebuild works in an isolated environment; unqualified targets remain clearly excluded from claims.

### Stage 3 — Add an isolated PyPI publication workflow

- Create a dedicated Python-only publishing workflow. Build and validate the sdist/wheels in a separate job with read-only permissions; transfer only the verified artifacts to a narrowly scoped publish job.
- Use PyPI Trusted Publishing via GitHub OIDC and the official `pypa/gh-action-pypi-publish@release/v1`. Grant `id-token: write` only to the publish job, not the workflow or build matrix. Use the exact repository/workflow/environment in the PyPI Trusted Publisher configuration and a protected `pypi` GitHub environment, with required reviewer approval if available.
- Validate the release tag against the Python package version and reject the Rust auth tag, root Eggpass tags, branches, pull requests, and mismatched versions. A dry-run/manual validation path must be incapable of uploading, even if an input is changed. Do not add long-lived PyPI API tokens.
- Publish only prebuilt, reviewed artifacts; keep the publish job free of checkout/build steps so code execution is minimized near OIDC credentials. Use PyPI's automatic attestations from the official publishing action and retain artifact/build provenance where practical.
- Test the workflow with a safe non-production path (for example, a separately configured TestPyPI publisher) if needed. Never assume TestPyPI metadata or package state is shared with PyPI.
- Update maintainer release guidance with the Python version/tag convention, exact workflow procedure, artifact review, OIDC configuration, and distinction from Rust/crate releases.

**Exit:** Workflow/static checks prove the only eligible publish path is the intended protected Python release; no root or crates.io release action is triggered by its tag; no credentials are stored.

### Stage 4 — Prepare and review the first release candidate (no upload in this plan)

- Ensure the Python package, CI, docs links, and publisher are included in a reviewed commit. Do not tag uncommitted/untracked content or include unrelated worktree changes.
- Build the exact release commit's sdist and wheels; run `twine check` (or equivalent metadata validation), inspect the rendered long description, confirm filenames/tags, and install from the artifacts in clean environments.
- Recheck PyPI name availability and configure the exact pending/normal Trusted Publisher. Inspect the release tag, package version, files, attestation configuration, and protected environment before the publish job can run.
- Stop before the first public upload until the user gives fresh, explicit confirmation. A published file/version cannot be overwritten; fix mistakes with a new version or yank where appropriate, not by reusing the same version.

**Exit:** Candidate artifacts and workflow are reviewed and ready; publication remains unperformed and separately gated.

## Acceptance checklist

- [x] Python minimum, classifiers, ABI floor, GIL/free-threaded scope, and OS/architecture promises agree.
- [x] Python examples remain in the package README; docs index links to it; root README remains the CLI guide.
- [ ] Clean sdist and every advertised wheel build, install, import, and pass the package checks on the stated target matrix.
- [ ] Linux x86_64/aarch64, macOS x86_64/arm64, and Windows x86_64 are qualified for the chosen CPython range; ARMv7/armv6/Pi coverage is separately evidenced or explicitly excluded.
- [x] Local-stub tests and type checks pass; package source is self-contained; no provider-live OAuth tests are needed for CI.
- [ ] Publisher is PyPI Trusted Publishing/OIDC, protected by an exact environment/tag/version boundary; only the publish job has OIDC permission; no long-lived token is used.
- [x] Python release tag cannot trigger root Eggpass or crates.io publication.
- [ ] First release artifact review is complete; no upload occurs without fresh explicit user confirmation.

## Remaining scope boundaries

- No commit, push, tag, GitHub release, TestPyPI upload, PyPI Publisher/GitHub Environment configuration, or PyPI upload was performed. The first upload needs fresh, explicit user authorization.
- Unrelated worktree changes were preserved. Review and stage only the intended package/workflow/docs changes; do not stage the whole worktree.
- Do not claim ARMv6, 32-bit ARM/Pi wheels, universal Raspberry Pi support, musllinux, Windows ARM64, PyPy, or free-threaded Python 3.15. A compatible 64-bit Pi still requires an actual device/OS check.
- Do not modify the Rust crate API or merge Python versioning into the Rust/CLI release process.

## Research references

Repository evidence:

- [`python-pyo3-bindings-implementation.md`](python-pyo3-bindings-implementation.md) — current binding plan and pending platform validation.
- `python/eggpass-auth/pyproject.toml`, `python/eggpass-auth/README.md`, `python/eggpass-auth/Cargo.toml`, `.github/workflows/python-auth.yml`, `.github/workflows/publish-eggpass-auth-pypi.yml`, `.github/workflows/publish-eggpass-auth.yml`, `docs/README.md`, and `docs/releasing.md`.

External guidance checked 2026-11-17:

- [Python version status](https://devguide.python.org/versions/) — lifecycle of Python 3.11–3.15.
- [PyO3 0.30.1 building and distribution](https://pyo3.rs/v0.30.1/building-and-distribution) and [features](https://pyo3.rs/v0.30.1/features.html) — `abi3`, `abi3t`, minimum versions, and Maturin requirements; [FAQ](https://pyo3.rs/v0.30.1/faq) — migration away from `extension-module`.
- [Maturin platform support](https://www.maturin.rs/platform_support), [Maturin action](https://github.com/PyO3/maturin-action), and [Maturin changelog](https://www.maturin.rs/changelog) — target support, CI building, and current Python 3.15/stable-ABI fixes in 1.16.0.
- [piwheels FAQ](https://www.piwheels.org/faq.html) — Raspberry Pi build availability, 32-/64-bit OS limits, and caution about publishing Raspberry-Pi-built ARM wheels to PyPI for broader ARM audiences.
- [Adding a PyPI Trusted Publisher](https://docs.pypi.org/trusted-publishers/adding-a-publisher/), [publishing with one](https://docs.pypi.org/trusted-publishers/using-a-publisher/), [security model](https://docs.pypi.org/trusted-publishers/security-model/), and [producing attestations](https://docs.pypi.org/attestations/producing-attestations/) — OIDC, least-privilege publish jobs, protected environments, and automatic attestations.
