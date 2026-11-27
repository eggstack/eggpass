#!/usr/bin/env bash
# Eggpass bootstrap installer, adapted from eggstack/gregg packaging/install.sh
# at fabcaf21bc60f719a5de5b9420490ffed52400e4.
# Copyright (c) 2026 The Gregg Authors. MIT License.
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.

if [ -z "${BASH_VERSION:-}" ]; then echo "error: install.sh requires bash; run it with bash, not sh" >&2; exit 1; fi
set -euo pipefail
REPO="eggstack/eggpass"
BASE_URL="https://github.com/$REPO/releases"
VERSION=""; STRIPPED_VERSION=""; TAG=""
SYSTEM_INSTALL=false; NO_SHELL_PROFILE=false
usage() {
 cat <<'EOF'
usage: install.sh [--version X.Y.Z] [--system] [--no-shell-profile]

Install Eggpass from a verified GitHub release. The default is $HOME/.local/bin.
--system selects /usr/local/bin and requires an already elevated shell; this
installer never invokes sudo. Only the five published Eggpack targets are supported.

  --version X.Y.Z    install exact release vX.Y.Z
  --system           explicit system-wide install (requires root)
  --no-shell-profile do not append a safe user-local PATH block
  --help, -h         show this help
EOF
}
die() { echo "error: $*" >&2; exit 1; }
while [[ $# -gt 0 ]]; do
 case "$1" in
  --version) [[ $# -ge 2 && -n "${2:-}" ]] || die "--version requires X.Y.Z"; VERSION="$2"; shift 2 ;;
  --system) SYSTEM_INSTALL=true; shift ;;
  --no-shell-profile) NO_SHELL_PROFILE=true; shift ;;
  --help|-h) usage; exit 0 ;;
  *) die "unknown argument: $1" ;;
 esac
done
if [[ -n "$VERSION" ]]; then
 STRIPPED_VERSION="${VERSION#v}"
 [[ "$STRIPPED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--version must be stable X.Y.Z (got '$VERSION')"
 TAG="v$STRIPPED_VERSION"
fi
# EGGPASS_TEST_OS/ARCH are test-only host overrides; production uses uname.
if [[ -n "${EGGPASS_TEST_OS:-}" ]]; then OS="$EGGPASS_TEST_OS"; else OS="$(uname -s 2>/dev/null || echo unknown)"; fi
if [[ -n "${EGGPASS_TEST_ARCH:-}" ]]; then ARCH="$EGGPASS_TEST_ARCH"; else ARCH="$(uname -m 2>/dev/null || echo unknown)"; fi
TARGET=""
case "$OS" in
 Linux) case "$ARCH" in x86_64|amd64|AMD64) TARGET=x86_64-unknown-linux-gnu ;; aarch64|arm64|ARM64) TARGET=aarch64-unknown-linux-gnu ;; esac ;;
 Darwin) case "$ARCH" in x86_64|amd64|AMD64) TARGET=x86_64-apple-darwin ;; arm64|aarch64|ARM64) TARGET=aarch64-apple-darwin ;; esac ;;
 MINGW*|MSYS*|CYGWIN*|Windows_NT) case "$ARCH" in x86_64|amd64|AMD64) TARGET=x86_64-pc-windows-msvc ;; esac ;;
esac
SUPPORTED_BINARY=false
case "$TARGET" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin|x86_64-pc-windows-msvc) SUPPORTED_BINARY=true ;; esac
ASSET_SUFFIX=""; INSTALL_NAME=eggpass
if [[ "$TARGET" == x86_64-pc-windows-msvc ]]; then ASSET_SUFFIX=.exe; INSTALL_NAME=eggpass.exe; fi
echo "Detected: OS=$OS ARCH=$ARCH TARGET=${TARGET:-unknown} ${TAG:-latest}" >&2
if [[ "$SYSTEM_INSTALL" == true ]]; then
 [[ $EUID -eq 0 ]] || die "--system requires an already elevated shell; rerun as root (the installer never invokes sudo)"
 DEST_DIR="${EGGPASS_TEST_SYSTEM_DIR:-/usr/local/bin}"
else
 [[ -n "${HOME:-}" ]] || die "HOME is empty; cannot select a per-user destination"
 DEST_DIR="$HOME/.local/bin"
fi
is_system_install() { [[ "$SYSTEM_INSTALL" == true ]]; }

# --- user-local PATH persistence (Plan 130) ----------------------------------
#
# Post-install user experience only, never installation identity. Destinations
# stay canonical (root -> /usr/local/bin, non-root -> $HOME/.local/bin).
# A piped child installer cannot export into its parent shell, so profile
# edits persist for future shells while the documented trailing parent-shell
# `export PATH="$HOME/.local/bin:$PATH"` activates the current shell.

dest_on_path() {
  case ":${PATH:-}:" in
    *":${DEST_DIR}:"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Active-integration predicate (Plan 131 corrective pass): success means the
# selected startup file contains a recognizable active PATH integration for
# the canonical user-local bin directory — either Eggpass's intact managed
# block (exact marker plus an active functional PATH line) or a supported,
# non-commented user-authored PATH (or zsh tied `path` array) assignment
# containing `$HOME/.local/bin`, `${HOME}/.local/bin`, `~/.local/bin`, or the
# expanded `$HOME/.local/bin` as a discrete PATH entry. Arbitrary `.local/bin`
# text (comments, commented-out assignments, prose, echo/printf, aliases,
# functions, non-PATH variables, subpaths, or longer names) is not sufficient.
# Read-only: profile contents are never sourced, evaluated, executed, or
# command-substituted. Conservative: exotic shell metaprogramming fails closed
# (returns nonzero) so the installer appends its safe managed block rather
# than falsely claiming persistence.
line_has_canonical_bin_entry() {
  local code="$1"
  if [[ $code =~ \$HOME/\.local/bin([^/A-Za-z0-9_.-]|$) ]]; then
    return 0
  fi
  if [[ $code =~ \$\{HOME\}/\.local/bin([^/A-Za-z0-9_.-]|$) ]]; then
    return 0
  fi
  if [[ $code =~ \~/\.local/bin([^/A-Za-z0-9_.-]|$) ]]; then
    return 0
  fi
  # Expanded `$HOME/.local/bin` (deterministic, read-only): the installer
  # knows `$HOME`, so an explicit absolute user-authored entry for this home
  # counts when it appears as a discrete PATH entry. Boundary-checked with
  # fixed-string matching to avoid regex-escaping the home path.
  if [[ -n "${HOME:-}" ]]; then
    local expanded="${HOME}/.local/bin"
    if [[ "$code" == *"$expanded"* ]]; then
      local rest="$code"
      while [[ "$rest" == *"$expanded"* ]]; do
        rest="${rest#*"$expanded"}"
        local next="${rest:0:1}"
        if [[ -z "$next" ]]; then
          return 0
        fi
        # shellcheck disable=SC2053
        if [[ "$next" =~ [^/A-Za-z0-9_.-] ]]; then
          return 0
        fi
        [[ "$rest" == *"$expanded"* ]] || break
      done
    fi
  fi
  return 1
}

# Intact Eggpass-managed block: the exact marker plus at least one active
# (non-commented) functional `export PATH=` line carrying the canonical entry.
# Marker text alone — including an unrelated comment that happens to match it,
# or a block whose functional line was manually commented out — is not enough.
profile_has_managed_block() {
  local file="$1"
  grep -Fq "added by eggpass installer" "$file" 2>/dev/null || return 1
  local line trimmed code
  while IFS= read -r line || [[ -n "$line" ]]; do
    trimmed="${line#"${line%%[![:space:]]*}"}"
    [[ -z "$trimmed" ]] && continue
    [[ "$trimmed" == \#* ]] && continue
    code="$trimmed"
    # The managed export lives on a case-branch line (`*) export ... ;;`);
    # strip one leading branch prefix before matching the functional part.
    if [[ "$code" == '*)'* ]]; then
      code="${code#\*\)}"
      code="${code#"${code%%[![:space:]]*}"}"
    fi
    if [[ "$code" == *'export PATH='* ]] && line_has_canonical_bin_entry "$code"; then
      return 0
    fi
  done < "$file"
  return 1
}

# Supported user-authored integration: a non-commented line that starts (after
# leading whitespace) with `export PATH=`, `PATH=`, `export path=`, or `path=`
# (zsh tied array) and carries the canonical entry discretely. `echo`,
# `printf`, aliases, functions, non-PATH variables, and other paths that
# merely contain the substring are ignored. Full-line and leading-whitespace
# comments are skipped; inline comments after an active assignment are
# accepted because matching is prefix-anchored, not full-line.
profile_has_user_path_integration() {
  local file="$1"
  local line trimmed code
  while IFS= read -r line || [[ -n "$line" ]]; do
    trimmed="${line#"${line%%[![:space:]]*}"}"
    [[ -z "$trimmed" ]] && continue
    [[ "$trimmed" == \#* ]] && continue
    code="$trimmed"
    if [[ "$code" =~ ^export[[:space:]]+PATH= ]] || [[ "$code" =~ ^PATH= ]]; then
      if line_has_canonical_bin_entry "$code"; then
        return 0
      fi
    elif [[ "$code" =~ ^export[[:space:]]+path= ]] || [[ "$code" =~ ^path= ]]; then
      if line_has_canonical_bin_entry "$code"; then
        return 0
      fi
    fi
  done < "$file"
  return 1
}

profile_has_active_path_integration() {
  local file="$1"
  [[ -f "$file" ]] || return 1
  if profile_has_managed_block "$file"; then
    return 0
  fi
  if profile_has_user_path_integration "$file"; then
    return 0
  fi
  return 1
}

zsh_profile_path() {
  local zdot="${ZDOTDIR:-}"
  if [[ -n "$zdot" && "$zdot" == /* && "$zdot" != *$'\n'* ]]; then
    printf '%s/.zshrc' "$zdot"
  else
    printf '%s/.zshrc' "${HOME}"
  fi
}

# Deterministic bash startup target: Linux centers on ~/.bashrc; macOS honors
# login-shell behavior (existing ~/.bash_profile, ~/.bash_login, or ~/.profile
# wins, otherwise ~/.bash_profile is created). Takes explicit inputs so tests
# can exercise macOS selection without a real interactive shell.
bash_profile_path() {
  local os_name="${1:-$OS}"
  local home_dir="${2:-$HOME}"
  if [[ "$os_name" == "Darwin" ]]; then
    if [[ -f "${home_dir}/.bash_profile" ]]; then
      printf '%s/.bash_profile' "$home_dir"
    elif [[ -f "${home_dir}/.bash_login" ]]; then
      printf '%s/.bash_login' "$home_dir"
    elif [[ -f "${home_dir}/.profile" ]]; then
      printf '%s/.profile' "$home_dir"
    else
      printf '%s/.bash_profile' "$home_dir"
    fi
  else
    printf '%s/.bashrc' "$home_dir"
  fi
}

# Prints the supported profile path, or returns nonzero for unsupported shells.
# Only zsh and bash are integrated; anything else falls back to manual advice.
select_shell_profile() {
  local shell_name=""
  if [[ -z "${SHELL:-}" ]]; then
    return 1
  fi
  shell_name="$(basename "${SHELL}" 2>/dev/null || true)"
  case "$shell_name" in
    zsh) zsh_profile_path ;;
    bash) bash_profile_path "$OS" "$HOME" ;;
    *) return 1 ;;
  esac
}

# Appends one bounded, idempotent PATH block. Never evaluates or sources the
# profile, never rewrites it, never touches system shell policy. Returns
# nonzero when the target is malformed, inaccessible, or outside the expected
# user home/config location; callers report that separately without rolling
# back the already successful binary install.
append_profile_entry() {
  local file="$1"
  local parent
  # Never follow a profile symlink outside $HOME (would append to an
  # attacker-chosen target); the caller reports the miss as manual guidance.
  if [[ -L "$file" ]]; then
    return 1
  fi
  parent="$(dirname "$file")"
  if [[ ! -d "$parent" ]]; then
    return 1
  fi
  if [[ -e "$file" && ! -f "$file" ]]; then
    return 1
  fi
  if [[ -e "$file" ]]; then
    if [[ ! -w "$file" && $EUID -ne 0 ]]; then
      return 1
    fi
  else
    local expected_parent_ok=false
    if [[ "$parent" == "$HOME" ]]; then
      expected_parent_ok=true
    elif [[ -n "${ZDOTDIR:-}" && "$parent" == "${ZDOTDIR}" && "${ZDOTDIR}" == /* ]]; then
      expected_parent_ok=true
    fi
    if [[ "$expected_parent_ok" != "true" ]]; then
      return 1
    fi
    if [[ ! -w "$parent" ]]; then
      return 1
    fi
  fi
  {
    echo ""
    echo "# added by eggpass installer: ensure user-local binaries are on PATH"
    echo "case \":\$PATH:\" in"
    echo "  *\":\$HOME/.local/bin:\"*) ;;"
    echo "  *) export PATH=\"\$HOME/.local/bin:\$PATH\" ;;"
    echo "esac"
  } >>"$file" 2>/dev/null || return 1
  return 0
}

print_manual_path_guidance() {
  echo "  export PATH=\"\$HOME/.local/bin:\$PATH\"" >&2
}

PATH_INTEGRATION_DONE=false

# Runs at most once per installer invocation, after all verified installs.
run_path_integration_once() {
  if [[ "$PATH_INTEGRATION_DONE" == "true" ]]; then
    return 0
  fi
  PATH_INTEGRATION_DONE=true

  # System installs never mutate user or global shell startup files.
  if is_system_install; then
    if dest_on_path; then
      echo "${DEST_DIR} is available on the current PATH." >&2
    else
      if [[ "$DEST_DIR" == "/usr/local/bin" ]]; then
        echo "note: /usr/local/bin is not in PATH; add it to your shell configuration:" >&2
        echo "  export PATH=\"/usr/local/bin:\$PATH\"" >&2
      else
        echo "note: ${DEST_DIR} is not in PATH; add it to your shell profile:" >&2
        print_manual_path_guidance
      fi
    fi
    return 0
  fi

  # Non-root user-local installs.
  if dest_on_path; then
    echo "${DEST_DIR} is available on the current PATH." >&2
    return 0
  fi

  if [[ "$NO_SHELL_PROFILE" == "true" ]]; then
    echo "${DEST_DIR} is not on the current PATH." >&2
    echo "Add it to your shell configuration, or run for this shell:" >&2
    print_manual_path_guidance
    return 0
  fi

  local profile=""
  if ! profile="$(select_shell_profile)"; then
    echo "${DEST_DIR} is not on the current PATH." >&2
    echo "Add it to your shell configuration, or run for this shell:" >&2
    print_manual_path_guidance
    return 0
  fi

  if profile_has_active_path_integration "$profile"; then
    echo "${DEST_DIR} is not on the current PATH." >&2
    echo "Your shell profile already integrates ${DEST_DIR}; for this shell, run:" >&2
    print_manual_path_guidance
    return 0
  fi

  if append_profile_entry "$profile"; then
    echo "Added ${DEST_DIR} to ${profile} for future shells." >&2
    echo "For this shell, run:" >&2
    print_manual_path_guidance
    return 0
  else
    echo "${DEST_DIR} is not on the current PATH." >&2
    echo "Could not update ${profile}; add it to your shell configuration, or run for this shell:" >&2
    print_manual_path_guidance
    return 0
  fi
}

# --- candidate and checksum verification ------------------------------------
SELECTED_VERSION=""; SCOPE=""
existing_version_for() { local f="$1"; if [[ -x "$f" ]]; then "$f" --version 2>/dev/null || true; fi; }
classify_destination() {
 local f="$1" output
 [[ ! -e "$f" ]] && { echo absent; return 0; }
 output="$(existing_version_for "$f")"
 if [[ "$output" =~ ^eggpass\ [0-9]+\.[0-9]+\.[0-9]+$ ]]; then echo replace; else echo foreign; fi
}
verify_checksum() {
 local file="$1" sidecar="$2" expected actual
 expected="$(awk '{print $1}' "$sidecar" 2>/dev/null || true)"
 expected="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
 [[ "$expected" =~ ^[0-9a-f]{64}$ ]] || die "checksum sidecar is empty or malformed"
 if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$file" | awk '{print $1}')"
 elif command -v shasum >/dev/null 2>&1; then actual="$(shasum -a 256 "$file" | awk '{print $1}')"
 else die "no SHA-256 utility found (need sha256sum or shasum)"; fi
 actual="$(printf '%s' "$actual" | tr '[:upper:]' '[:lower:]')"
 [[ "$expected" == "$actual" ]] || die "SHA-256 mismatch for $(basename "$file")"
}
verify_candidate_version() {
 local candidate="$1" output
 if ! output="$("$candidate" --version 2>&1)"; then die "candidate failed eggpass --version validation: $output"; fi
 [[ "$output" =~ ^eggpass\ ([0-9]+\.[0-9]+\.[0-9]+)$ ]] || die "candidate identity must be exactly 'eggpass X.Y.Z': $output"
 SELECTED_VERSION="${BASH_REMATCH[1]}"
 if [[ -n "$STRIPPED_VERSION" && "$SELECTED_VERSION" != "$STRIPPED_VERSION" ]]; then die "candidate version $SELECTED_VERSION does not match requested $STRIPPED_VERSION"; fi
 echo "Verified candidate: $output" >&2
}
cargo_fallback() {
 local existing="$1" selected="$STRIPPED_VERSION" stage_root cargo_root candidate
 [[ "$selected" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "cannot use Cargo fallback without an exact selected release version"
 command -v cargo >/dev/null 2>&1 || die "Cargo is unavailable; retry exact version $selected after installing Rust"
 stage_root="$(mktemp -d "${TMPDIR:-/tmp}/eggpass-install.XXXXXX")"
 # shellcheck disable=SC2064
 trap "rm -rf \"$stage_root\"" EXIT
 cargo_root="$stage_root/cargo-root"
 local -a cargo_args=(install --locked --version "=$selected" --root "$cargo_root" eggpass)
 echo "Building exact Eggpass $selected in private Cargo staging." >&2
 if ! cargo "${cargo_args[@]}"; then die "Cargo could not install exact eggpass $selected; retry when that crate version is available"; fi
 candidate="$cargo_root/bin/eggpass$ASSET_SUFFIX"
 [[ -x "$candidate" ]] || die "Cargo produced no staged candidate at $candidate"
 verify_candidate_version "$candidate"
 [[ "$SELECTED_VERSION" == "$selected" ]] || die "Cargo candidate does not match selected GitHub release $selected"
 mkdir -p "$DEST_DIR"
 [[ ! -L "$DEST_DIR/$INSTALL_NAME" ]] || die "$DEST_DIR/$INSTALL_NAME became a symlink; refusing to overwrite it"
 if command -v install >/dev/null 2>&1; then install -m 755 "$candidate" "$DEST_DIR/$INSTALL_NAME"; else cp "$candidate" "$DEST_DIR/$INSTALL_NAME"; chmod 755 "$DEST_DIR/$INSTALL_NAME"; fi
 rm -rf "$stage_root"; trap - EXIT
 if [[ "$SCOPE" == replace ]]; then echo "Updated eggpass at $DEST_DIR/$INSTALL_NAME ($existing -> $("$DEST_DIR/$INSTALL_NAME" --version 2>&1))" >&2; else echo "Installed eggpass to $DEST_DIR/$INSTALL_NAME" >&2; fi
}

install_program() {
 local asset url sha_url tmpdir dest scope existing meta curl_status http_code effective_url
 if [[ "$SUPPORTED_BINARY" != true ]]; then die "unsupported target $OS/$ARCH (${TARGET:-unknown})"; fi
 asset="eggpass-$TARGET$ASSET_SUFFIX"
 if [[ -n "$TAG" ]]; then url="$BASE_URL/download/$TAG/$asset"; else url="$BASE_URL/latest/download/$asset"; fi
 sha_url="$url.sha256"
 dest="$DEST_DIR/$INSTALL_NAME"
 [[ ! -L "$dest" ]] || die "$dest is a symlink; refusing to overwrite it"
 scope="$(classify_destination "$dest")"
 [[ "$scope" != foreign ]] || die "$dest is foreign; refusing to overwrite it"
 SCOPE="$scope"; existing=""
 if [[ "$scope" == replace ]]; then existing="$(existing_version_for "$dest")"; echo "Found $existing at $dest; same-scope rerun will replace it." >&2; fi
 tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/eggpass-install.XXXXXX")"
 # shellcheck disable=SC2064
 trap "rm -rf \"$tmpdir\"" EXIT
 set +e
 meta="$(curl --proto '=https' --tlsv1.2 -fsSL --max-time 100 --max-filesize 67108864 -H "User-Agent: eggpass/installer" -o "$tmpdir/$asset" -w "%{http_code}\n%{url_effective}" "$url" 2>/dev/null)"
 curl_status=$?; set -e
 http_code="${meta%%$'\n'*}"; effective_url="${meta#*$'\n'}"
 [[ "$effective_url" == "$meta" ]] && effective_url=""
 http_code="${http_code:-000}"
if [[ "$http_code" == 404 ]]; then
 if [[ -z "$STRIPPED_VERSION" ]]; then
  local release_tag
  release_tag="$(printf '%s' "$effective_url" | awk -F/ '{print $(NF-1)}')"
  [[ "$release_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ && "$effective_url" == "$BASE_URL/download/$release_tag/$asset" ]] || die "latest 404 did not resolve to an exact release tag; refusing floating Cargo fallback"
  TAG="$release_tag"; STRIPPED_VERSION="${release_tag#v}"
 fi
 echo "Binary asset missing (HTTP 404); using exact release $STRIPPED_VERSION for Cargo fallback." >&2
 rm -rf "$tmpdir"; trap - EXIT; cargo_fallback "$existing"; return 0
fi
if [[ $curl_status -ne 0 ]]; then die "curl failed (exit $curl_status, HTTP $http_code); only a definite 404 permits Cargo fallback"; fi
[[ "$http_code" == 2* ]] || die "unexpected HTTP $http_code; refusing install and Cargo fallback"
if ! curl --proto '=https' --tlsv1.2 -fsSL --max-time 30 --max-filesize 4096 -H "User-Agent: eggpass/installer" -o "$tmpdir/$asset.sha256" "$sha_url"; then die "checksum download failed for $asset"; fi
verify_checksum "$tmpdir/$asset" "$tmpdir/$asset.sha256"
chmod +x "$tmpdir/$asset"
verify_candidate_version "$tmpdir/$asset"
mkdir -p "$DEST_DIR"
[[ ! -L "$dest" ]] || die "$dest became a symlink; refusing to overwrite it"
if command -v install >/dev/null 2>&1; then install -m 755 "$tmpdir/$asset" "$dest"; else cp "$tmpdir/$asset" "$dest"; chmod 755 "$dest"; fi
if [[ "$scope" == replace ]]; then echo "Updated eggpass at $dest ($existing -> $("$dest" --version 2>&1))" >&2; else echo "Installed eggpass to $dest" >&2; fi
rm -rf "$tmpdir"; trap - EXIT
}
install_program
run_path_integration_once
echo "Done. Installed eggpass to $DEST_DIR" >&2
echo "  $DEST_DIR/$INSTALL_NAME --version => $("$DEST_DIR/$INSTALL_NAME" --version 2>&1 || echo not-runnable)" >&2
