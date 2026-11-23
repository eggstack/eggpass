#!/usr/bin/env bash
# Offline fixtures adapted from eggstack/gregg test-install-rerun.sh at
# fabcaf21bc60f719a5de5b9420490ffed52400e4.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALL_SH="$ROOT/packaging/install.sh"
INSTALL_PS="$ROOT/packaging/install.ps1"
CONTRACT="$ROOT/release/eggpack/distribution.toml"
PASS=0; FAIL=0
ok() { PASS=$((PASS + 1)); echo "ok - $1"; }
fail() { FAIL=$((FAIL + 1)); echo "not ok - $1" >&2; }
contains() { [[ "$1" == *"$2"* ]]; }
SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/tmp" "$SANDBOX/fakebin"
export TMPDIR="$SANDBOX/tmp"
FAKEBIN="$SANDBOX/fakebin"
export CURL_LOG="$SANDBOX/curl.log" CARGO_LOG="$SANDBOX/cargo.log" CARGO_ROOT_LOG="$SANDBOX/cargo-root.log"
: > "$CURL_LOG"; : > "$CARGO_LOG"; : > "$CARGO_ROOT_LOG"
cat > "$FAKEBIN/curl" <<'CURL_FIXTURE'
#!/usr/bin/env bash
set -eu
out=""; url=""; format=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --proto) [[ $# -ge 2 && "$2" == '=https' ]] || exit 91; shift 2 ;;
    --max-time) [[ $# -ge 2 && "$2" =~ ^[0-9]+$ ]] || exit 91; shift 2 ;;
    --max-filesize) [[ $# -ge 2 && "$2" =~ ^[0-9]+$ ]] || exit 91; shift 2 ;;
    -H) [[ $# -ge 2 && "$2" == 'User-Agent: eggpass/installer' ]] || exit 91; shift 2 ;;
    -o) [[ $# -ge 2 && -z "$out" ]] || exit 91; out="$2"; shift 2 ;;
    -w) [[ $# -ge 2 && -z "$format" ]] || exit 91; format="$2"; shift 2 ;;
    --tlsv1.2|-fsSL) shift ;;
    https://*) [[ -z "$url" ]] || exit 91; url="$1"; shift ;;
    *) printf 'fake curl refused unexpected argument: %s\n' "$1" >&2; exit 91 ;;
  esac
done
[[ -n "$out" && -n "$url" ]] || exit 90
case "$url" in https://github.com/eggstack/eggpass/releases/*) ;; *) printf 'fake curl refused unexpected host/path: %s\n' "$url" >&2; exit 92 ;; esac
if [[ "$url" == "$FIXTURE_EXPECTED_URL" ]]; then
  [[ "$format" == '%{http_code}\n%{url_effective}' ]] || exit 93
elif [[ "$url" == "$FIXTURE_EXPECTED_URL.sha256" ]]; then
  [[ -z "$format" ]] || exit 93
else
  printf 'fake curl refused unexpected URL: %s\n' "$url" >&2; exit 94
fi
printf '%s\n' "$url" >> "$CURL_LOG"
if [[ "$url" == "$FIXTURE_EXPECTED_URL.sha256" ]]; then
  case "$FIXTURE_CURL_MODE" in sha-error) exit 22 ;; malformed-sha) printf 'not-a-digest\n' > "$out"; exit 0 ;; wrong-sha) printf '%064d  fixture\n' 0 > "$out"; exit 0 ;; esac
  asset="$(printf '%s' "$out" | sed 's/\.sha256$//')"
  if command -v sha256sum >/dev/null 2>&1; then digest="$(sha256sum "$asset" | awk '{print $1}')"; else digest="$(shasum -a 256 "$asset" | awk '{print $1}')"; fi
  printf '%s  %s\n' "$digest" "$(basename "$asset")" > "$out"
  exit 0
fi
latest="$FIXTURE_LATEST_VERSION"
effective="$url"
case "$url" in */latest/download/*) effective="https://github.com/eggstack/eggpass/releases/download/v$latest/$(basename "$url")" ;; esac
case "$FIXTURE_CURL_MODE" in
  transport) printf '000\n%s' "$effective"; exit 7 ;;
  http500) printf '500\n%s' "$effective"; exit 22 ;;
  404) if [[ "$FIXTURE_NO_EFFECTIVE_TAG" == 1 ]]; then effective="$url"; fi; printf '404\n%s' "$effective"; exit 22 ;;
esac
line="eggpass $FIXTURE_VERSION"
if [[ -n "$FIXTURE_IDENTITY" ]]; then line="$FIXTURE_IDENTITY"; fi
printf '#!/bin/sh\nif [ "$1" = "--version" ]; then printf "%%s\\n" "%s"; exit 0; fi\nexit 0\n' "$line" > "$out"
chmod +x "$out"
printf '200\n%s' "$effective"
CURL_FIXTURE
chmod +x "$FAKEBIN/curl"
cat > "$FAKEBIN/cargo" <<'CARGO_FIXTURE'
#!/usr/bin/env bash
set -eu
[[ $# -eq 7 && "$1" == install && "$2" == --locked && "$3" == --version && "$4" == "=$FIXTURE_CARGO_EXPECTED_VERSION" && "$5" == --root && "$7" == eggpass ]] || { printf 'fake cargo refused unexpected arguments\n' >&2; exit 95; }
root="$6"
case "$root" in "$TMPDIR"/eggpass-install.*/cargo-root) ;; *) printf 'fake cargo refused unexpected staging root: %s\n' "$root" >&2; exit 96 ;; esac
printf 'cargo' >> "$CARGO_LOG"
for arg in "$@"; do printf ' <%s>' "$arg" >> "$CARGO_LOG"; done
printf '\n' >> "$CARGO_LOG"
printf '%s\n' "$root" > "$CARGO_ROOT_LOG"
mkdir -p "$root/bin"
line="eggpass $FIXTURE_CARGO_EXPECTED_VERSION"
if [[ -n "$FIXTURE_CARGO_IDENTITY" ]]; then line="$FIXTURE_CARGO_IDENTITY"; fi
cargo_name=eggpass; [[ "$FIXTURE_EXPECTED_URL" == *windows-msvc.exe ]] && cargo_name=eggpass.exe
printf '#!/bin/sh\nif [ "$1" = "--version" ]; then printf "%%s\\n" "%s"; exit 0; fi\nexit 0\n' "$line" > "$root/bin/$cargo_name"
chmod +x "$root/bin/$cargo_name"
printf 'fake Cargo metadata\n' > "$root/.crates.toml"
CARGO_FIXTURE
chmod +x "$FAKEBIN/cargo"
export PATH="$FAKEBIN:/usr/bin:/bin" SHELL=/bin/zsh
OS=Linux; ARCH=x86_64; FIXTURE_CURL_MODE=ok; FIXTURE_VERSION=1.2.3
FIXTURE_LATEST_VERSION=1.2.3; FIXTURE_IDENTITY=""; FIXTURE_CARGO_IDENTITY=""; FIXTURE_NO_EFFECTIVE_TAG=0
export OS ARCH FIXTURE_CURL_MODE FIXTURE_VERSION FIXTURE_LATEST_VERSION FIXTURE_IDENTITY FIXTURE_CARGO_IDENTITY FIXTURE_NO_EFFECTIVE_TAG
fresh_home() {
  HOME="$SANDBOX/home-$1"; mkdir -p "$HOME"; export HOME
  DEST_DIR="$HOME/.local/bin"; mkdir -p "$DEST_DIR"; export DEST_DIR
  PATH="$FAKEBIN:$DEST_DIR:/usr/bin:/bin"; export PATH
}
fixture_target() {
  case "$OS" in
    Linux) case "$ARCH" in x86_64|amd64|AMD64) printf '%s' x86_64-unknown-linux-gnu ;; aarch64|arm64|ARM64) printf '%s' aarch64-unknown-linux-gnu ;; esac ;;
    Darwin) case "$ARCH" in x86_64|amd64|AMD64) printf '%s' x86_64-apple-darwin ;; arm64|aarch64|ARM64) printf '%s' aarch64-apple-darwin ;; esac ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT) case "$ARCH" in x86_64|amd64|AMD64) printf '%s' x86_64-pc-windows-msvc ;; esac ;;
  esac
}
run_install() {
  local requested="" want_version=0 arg target asset cargo_version
  for arg in "$@"; do
    if [[ "$want_version" == 1 ]]; then requested="$arg"; want_version=0; continue; fi
    if [[ "$arg" == --version ]]; then want_version=1; fi
  done
  target="$(fixture_target)"
  asset=""
  EXPECTED_URL=""
  if [[ -n "$target" ]]; then
    asset="eggpass-$target"
    [[ "$target" == x86_64-pc-windows-msvc ]] && asset="$asset.exe"
    if [[ -n "$requested" ]]; then EXPECTED_URL="https://github.com/eggstack/eggpass/releases/download/v$requested/$asset"
    else EXPECTED_URL="https://github.com/eggstack/eggpass/releases/latest/download/$asset"; fi
  fi
  cargo_version="$FIXTURE_LATEST_VERSION"
  [[ -n "$requested" ]] && cargo_version="$requested"
  set +e
  OUT="$(EGGPASS_TEST_OS="$OS" EGGPASS_TEST_ARCH="$ARCH" FIXTURE_EXPECTED_URL="$EXPECTED_URL" FIXTURE_CARGO_EXPECTED_VERSION="$cargo_version" bash "$INSTALL_SH" "$@" 2>&1)"
  STATUS=$?
  set -e
}
reset_logs() { : > "$CURL_LOG"; : > "$CARGO_LOG"; : > "$CARGO_ROOT_LOG"; }
assert_no_cargo() { if [[ ! -s "$CARGO_LOG" ]]; then ok "$1"; else fail "$1 (unexpected Cargo call)"; fi; }

# Target mapping must remain in the checked-in five-triple Eggpack contract.
for triple in aarch64-apple-darwin aarch64-unknown-linux-gnu x86_64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-gnu; do
  if grep -Fq "triple = \"$triple\"" "$CONTRACT"; then ok "distribution contract includes $triple"; else fail "distribution contract missing $triple"; fi
done
# Cross-installer static policy checks: PowerShell uses the same release URLs,
# Windows distribution asset name, exact candidate identity, and exact Cargo pin.
# shellcheck disable=SC2016 # These literals intentionally match source text.
for policy in '$BaseUrl/latest/download/$Asset' '$BaseUrl/download/$Tag/$Asset' 'eggpass-$Target.exe' 'eggpass --version' '=$SelectedVersion' 'Set-PrivateDirectoryAcl -Path $stageRoot'; do
  if grep -Fq "$policy" "$INSTALL_PS"; then ok "PowerShell policy includes $policy"; else fail "PowerShell policy missing $policy"; fi
done
if grep -Fq 'cargo install eggpass' "$INSTALL_PS"; then fail "PowerShell contains floating Cargo install"; else ok "PowerShell has no floating Cargo install"; fi
check_target() {
  OS="$1"; ARCH="$2"; expected="$3"; asset="$4"; export OS ARCH
  fresh_home "target-$3"; reset_logs; FIXTURE_CURL_MODE=ok; export FIXTURE_CURL_MODE
  run_install
  executable=eggpass; [[ "$expected" == x86_64-pc-windows-msvc ]] && executable=eggpass.exe
  if [[ $STATUS -eq 0 && -x "$DEST_DIR/$executable" ]]; then ok "target maps to $expected"; else fail "target mapping $expected ($OUT)"; fi
  expected_urls="$(printf '%s\n%s' "$EXPECTED_URL" "$EXPECTED_URL.sha256")"
  if [[ $STATUS -eq 0 && "$(cat "$CURL_LOG")" == "$expected_urls" ]]; then ok "asset and checksum URLs are exact for $expected"; else fail "wrong release URL for $expected: $(cat "$CURL_LOG")"; fi
  if ! grep -Fq "triple = \"$expected\"" "$CONTRACT"; then fail "mapping $expected drifted from distribution contract"; fi
}
check_target Linux x86_64 x86_64-unknown-linux-gnu eggpass-x86_64-unknown-linux-gnu
check_target Linux aarch64 aarch64-unknown-linux-gnu eggpass-aarch64-unknown-linux-gnu
check_target Darwin x86_64 x86_64-apple-darwin eggpass-x86_64-apple-darwin
check_target Darwin arm64 aarch64-apple-darwin eggpass-aarch64-apple-darwin
check_target MINGW64_NT x86_64 x86_64-pc-windows-msvc eggpass-x86_64-pc-windows-msvc.exe

OS=Linux; ARCH=x86_64; export OS ARCH
fresh_home latest; reset_logs; run_install
if [[ $STATUS -eq 0 && "$(cat "$CURL_LOG")" == "$(printf '%s\n%s' "$EXPECTED_URL" "$EXPECTED_URL.sha256")" ]]; then ok "latest selects only GitHub releases/latest"; else fail "latest URL authority"; fi
fresh_home pinned; reset_logs; FIXTURE_VERSION=2.3.4; export FIXTURE_VERSION; run_install --version 2.3.4
if [[ $STATUS -eq 0 && "$EXPECTED_URL" == 'https://github.com/eggstack/eggpass/releases/download/v2.3.4/eggpass-x86_64-unknown-linux-gnu' && "$(cat "$CURL_LOG")" == "$(printf '%s\n%s' "$EXPECTED_URL" "$EXPECTED_URL.sha256")" ]]; then ok "pinned selection uses only exact v2.3.4 authority"; else fail "pinned URL authority"; fi

for mode in malformed-sha wrong-sha sha-error; do
  fresh_home "failure-$mode"; reset_logs; FIXTURE_CURL_MODE="$mode"; export FIXTURE_CURL_MODE; run_install
  if [[ $STATUS -ne 0 && ! -e "$DEST_DIR/eggpass" ]]; then ok "$mode fails before install"; else fail "$mode did not fail closed"; fi
  assert_no_cargo "$mode never triggers Cargo"
done
fresh_home mismatch; reset_logs; FIXTURE_CURL_MODE=ok; FIXTURE_VERSION=9.9.8; export FIXTURE_CURL_MODE FIXTURE_VERSION; run_install --version 1.2.3
if [[ $STATUS -ne 0 && ! -e "$DEST_DIR/eggpass" ]]; then ok "pinned candidate mismatch fails"; else fail "pinned mismatch was accepted"; fi
assert_no_cargo "identity mismatch never triggers Cargo"
fresh_home extra-identity; reset_logs; FIXTURE_VERSION=1.2.3; FIXTURE_IDENTITY='eggpass 1.2.3 extra'; export FIXTURE_VERSION FIXTURE_IDENTITY; run_install --version 1.2.3
if [[ $STATUS -ne 0 && ! -e "$DEST_DIR/eggpass" ]]; then ok "extra candidate identity fields are rejected"; else fail "extra identity accepted"; fi
assert_no_cargo "candidate identity failure never triggers Cargo"
FIXTURE_IDENTITY=""; export FIXTURE_IDENTITY
for mode in transport http500; do
  fresh_home "transport-$mode"; reset_logs; FIXTURE_CURL_MODE="$mode"; export FIXTURE_CURL_MODE; run_install --version 1.2.3
  if [[ $STATUS -ne 0 ]]; then ok "$mode fails"; else fail "$mode was accepted"; fi
  assert_no_cargo "$mode does not trigger fallback"
done

# Exact 404 is the only fallback trigger; latest must resolve to an exact tag.
OS=Linux; ARCH=x86_64; export OS ARCH; fresh_home fallback-latest; reset_logs
FIXTURE_CURL_MODE=404; FIXTURE_LATEST_VERSION=1.2.3; FIXTURE_VERSION=1.2.3; export FIXTURE_CURL_MODE FIXTURE_LATEST_VERSION FIXTURE_VERSION; run_install
if [[ $STATUS -eq 0 ]] && contains "$(cat "$CARGO_LOG")" '<--version> <=1.2.3'; then ok "latest 404 fallback pins Cargo to resolved release"; else fail "latest fallback did not pin Cargo ($OUT)"; fi
if [[ -x "$DEST_DIR/eggpass" && "$("$DEST_DIR/eggpass" --version)" == 'eggpass 1.2.3' ]]; then ok "Cargo candidate copied to per-user binary path"; else fail "Cargo result not installed"; fi
cargo_root="$(cat "$CARGO_ROOT_LOG")"
if [[ -n "$cargo_root" && ! -e "$cargo_root" && ! -e "$(dirname "$cargo_root")" ]]; then ok "private Cargo staging root and parent are removed"; else fail "Cargo staging leaked: $cargo_root"; fi
if [[ ! -e "$HOME/.cargo" && ! -e "$DEST_DIR/.crates.toml" ]]; then ok "Cargo metadata is not retained at final install"; else fail "Cargo metadata leaked"; fi
fresh_home fallback-pinned; reset_logs; FIXTURE_CURL_MODE=404; FIXTURE_VERSION=3.4.5; export FIXTURE_CURL_MODE FIXTURE_VERSION; run_install --version 3.4.5
if contains "$(cat "$CARGO_LOG")" '<--version> <=3.4.5'; then ok "pinned fallback asks Cargo for the exact version"; else fail "pinned fallback version was not exact"; fi
fresh_home fallback-unresolved; reset_logs; FIXTURE_CURL_MODE=404; FIXTURE_NO_EFFECTIVE_TAG=1; export FIXTURE_CURL_MODE FIXTURE_NO_EFFECTIVE_TAG; run_install
if [[ $STATUS -ne 0 ]]; then ok "unresolved latest 404 is refused"; else fail "unresolved latest fallback accepted"; fi
assert_no_cargo "latest without exact tag never invokes Cargo"
FIXTURE_NO_EFFECTIVE_TAG=0; export FIXTURE_NO_EFFECTIVE_TAG

fresh_home unsupported; reset_logs; OS=FreeBSD; ARCH=x86_64; export OS ARCH; FIXTURE_CURL_MODE=404; export FIXTURE_CURL_MODE; run_install
if [[ $STATUS -ne 0 ]]; then ok "unsupported platform is rejected"; else fail "unsupported platform accepted"; fi
assert_no_cargo "unsupported platform never falls back"
if [[ ! -s "$CURL_LOG" ]]; then ok "unsupported platform performs no HTTP request"; else fail "unsupported platform reached the downloader"; fi
OS=Linux; ARCH=x86_64; export OS ARCH; fresh_home rerun; reset_logs; FIXTURE_CURL_MODE=ok; FIXTURE_VERSION=1.0.0; export FIXTURE_CURL_MODE FIXTURE_VERSION; run_install
FIXTURE_VERSION=1.2.3; export FIXTURE_VERSION; run_install
if [[ $STATUS -eq 0 && "$("$DEST_DIR/eggpass" --version)" == 'eggpass 1.2.3' && "$OUT" == *"same-scope rerun"* ]]; then ok "same-scope rerun replaces the owned binary"; else fail "same-scope rerun failed"; fi
fresh_home foreign; printf '%s\n' '#!/bin/sh' 'echo foreign 1.0.0' > "$DEST_DIR/eggpass"; chmod +x "$DEST_DIR/eggpass"; reset_logs; FIXTURE_CURL_MODE=ok; export FIXTURE_CURL_MODE; run_install
if [[ $STATUS -ne 0 && "$("$DEST_DIR/eggpass")" == 'foreign 1.0.0' ]]; then ok "foreign executable is preserved"; else fail "foreign executable overwritten"; fi
fresh_home symlink; printf '%s\n' 'preserve me' > "$SANDBOX/target-file"; ln -s "$SANDBOX/target-file" "$DEST_DIR/eggpass"; reset_logs; run_install
if [[ $STATUS -ne 0 && -L "$DEST_DIR/eggpass" && "$(cat "$SANDBOX/target-file")" == 'preserve me' ]]; then ok "destination symlink is refused safely"; else fail "destination symlink safety failed"; fi

# All profile paths and HOME values are inside SANDBOX.
fresh_home path; PATH="$FAKEBIN:/usr/bin:/bin"; export PATH; reset_logs; FIXTURE_CURL_MODE=ok; FIXTURE_VERSION=1.2.3; export FIXTURE_CURL_MODE FIXTURE_VERSION; run_install
if [[ $STATUS -eq 0 && -f "$HOME/.zshrc" ]] && grep -Fq 'added by eggpass installer' "$HOME/.zshrc"; then ok "zsh profile gets managed PATH block"; else fail "PATH block missing"; fi
run_install
if [[ $(grep -c 'added by eggpass installer' "$HOME/.zshrc") -eq 1 ]]; then ok "profile PATH append is idempotent"; else fail "PATH entry duplicated"; fi
fresh_home path-symlink; PATH="$FAKEBIN:/usr/bin:/bin"; export PATH; printf '%s\n' safe > "$SANDBOX/profile-target"; ln -s "$SANDBOX/profile-target" "$HOME/.zshrc"; run_install
if [[ -L "$HOME/.zshrc" && "$(cat "$SANDBOX/profile-target")" == safe ]]; then ok "profile symlink is not followed"; else fail "profile symlink was changed"; fi
fresh_home path-present; PATH="$DEST_DIR:$FAKEBIN:/usr/bin:/bin"; export PATH; reset_logs; run_install --no-shell-profile
if [[ $STATUS -eq 0 && "$OUT" == *"is available on the current PATH"* && ! -e "$HOME/.zshrc" ]]; then ok "existing PATH entry is detected without profile edits"; else fail "current PATH detection failed"; fi
fresh_home path-manual; PATH="$FAKEBIN:/usr/bin:/bin"; export PATH; reset_logs; run_install --no-shell-profile
if [[ $STATUS -eq 0 && "$OUT" == *"export PATH=\"\$HOME/.local/bin:\$PATH\""* && ! -e "$HOME/.zshrc" ]]; then ok "no-profile mode prints manual PATH guidance"; else fail "manual PATH guidance missing"; fi

fresh_home user-scope; reset_logs; FIXTURE_CURL_MODE=ok; export FIXTURE_CURL_MODE; run_install
if [[ $STATUS -eq 0 && -x "$HOME/.local/bin/eggpass" ]]; then ok "default install is per-user"; else fail "default user scope failed"; fi
if [[ $EUID -eq 0 ]]; then
  fresh_home system-scope; EGGPASS_TEST_SYSTEM_DIR="$SANDBOX/system-bin"; export EGGPASS_TEST_SYSTEM_DIR; run_install --system
  if [[ $STATUS -eq 0 && -x "$SANDBOX/system-bin/eggpass" ]]; then ok "explicit system scope uses isolated system destination"; else fail "system scope failed"; fi
else
  fresh_home system-denied; run_install --system
  if [[ $STATUS -ne 0 && "$OUT" == *"requires an already elevated shell"* ]]; then ok "system scope requires elevation"; else fail "system elevation boundary failed"; fi
fi
printf '\npass=%s fail=%s\n' "$PASS" "$FAIL"
[[ $FAIL -eq 0 ]]
