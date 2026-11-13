#!/usr/bin/env bash
#
# End-to-end verification against the real x.ai.
#
# This is the only script that talks to x.ai for real. It is deliberately
# manual: it needs a human to approve the device-code login, and it spends
# tokens.
#
# Usage:
#   scripts/verify-live.sh              # build, log in, check everything
#   scripts/verify-live.sh --skip-login # reuse the credentials already stored
#
# Everything runs against a throwaway configuration directory, so your real
# ~/.config/grokproxy is left alone.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/grokproxy"
PORT="${GROKPROXY_VERIFY_PORT:-8485}"
SKIP_LOGIN=0
KEEP_HOME=0

if [ -n "${GROKPROXY_HOME:-}" ]; then
    # Reuse the caller's configuration directory instead of a throwaway one.
    WORK="$GROKPROXY_HOME"
    KEEP_HOME=1
else
    WORK="$(mktemp -d "${TMPDIR:-/tmp}/grokproxy-verify.XXXXXX")"
fi
export GROKPROXY_HOME="$WORK"

for argument in "$@"; do
    case "$argument" in
        --skip-login) SKIP_LOGIN=1 ;;
        *) echo "unknown option: $argument" >&2; exit 2 ;;
    esac
done

say() {
    printf '\n\033[1m== %s\033[0m\n' "$*"
}

fail() {
    printf '\033[31mFAILED: %s\033[0m\n' "$*" >&2
    exit 1
}

cleanup() {
    "$BIN" stop >/dev/null 2>&1 || true
    if [ "$KEEP_HOME" -eq 0 ]; then
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

say "building"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

say "logging in"
if [ "$SKIP_LOGIN" -eq 1 ]; then
    echo "skipping the login; using the stored credentials"
else
    echo "A code and a URL will be printed. Open the URL, enter the code, then"
    echo "come back here. The script continues once the login completes."
    "$BIN" login --device --force
fi

STATUS="$("$BIN" status)"
echo "$STATUS"
echo "$STATUS" | grep -q "Account:  logged in" || fail "not logged in"
echo "the account is logged in"

say "starting the server on port $PORT"
"$BIN" port "$PORT" >/dev/null
"$BIN" run
BASE="http://127.0.0.1:$PORT"

say "checking /health"
curl -sf "$BASE/health" | tee /dev/stderr | grep -q '"status":"ok"' \
    || fail "the health endpoint did not answer"

say "checking the model catalogue"
# This is the check that settles whether api.x.ai accepts our OAuth token.
MODELS="$(curl -sf "$BASE/v1/models")"
echo "$MODELS" | python3 -c '
import json, sys
models = json.load(sys.stdin)["data"]
ids = [model["id"] for model in models]
print("models:", ", ".join(ids))
missing = [wanted for wanted in ("grok-4.7", "grok-4.6") if wanted not in ids]
if missing:
    print("missing:", ", ".join(missing), file=sys.stderr)
    sys.exit(1)
'
echo "grok-4.7 and grok-4.6 are both offered"

say "checking a single model"
curl -sf "$BASE/v1/models/grok-4.7" | grep -q '"id":"grok-4.7"' \
    || fail "GET /v1/models/grok-4.7 did not answer"

say "checking a chat completion"
ANSWER="$(curl -sf -X POST "$BASE/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"grok-4.7","messages":[{"role":"user","content":"Reply with the single word: ready"}],"max_tokens":32}')"
echo "$ANSWER" | python3 -c '
import json, sys
body = json.load(sys.stdin)
choices = body.get("choices") or []
if not choices:
    print("no choices in the response", file=sys.stderr)
    sys.exit(1)
print("model:", body.get("model"))
print("finish_reason:", choices[0].get("finish_reason"))
print("content:", (choices[0].get("message") or {}).get("content"))
'

say "checking a streaming chat completion"
STREAM="$(curl -sfN -X POST "$BASE/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"grok-4.7","messages":[{"role":"user","content":"Count from one to five."}],"stream":true}')"
echo "$STREAM" | grep -q '^data: ' || fail "the stream carried no data lines"
echo "$STREAM" | grep -q 'data: \[DONE\]' || fail "the stream was not terminated"
CHUNKS="$(echo "$STREAM" | grep -c '^data: ')"
echo "the stream carried $CHUNKS chunks"
[ "$CHUNKS" -gt 1 ] || fail "the stream was not incremental"

say "checking that a client key never reaches x.ai"
# The proxy strips the client's credentials; a request with a bogus key must
# still succeed when no key is configured.
curl -sf -X POST "$BASE/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer not-the-real-key' \
    -d '{"model":"grok-4.7","messages":[{"role":"user","content":"ok"}],"max_tokens":8}' \
    >/dev/null || fail "the proxy rejected a request with a client key"
echo "the client key was replaced with the Grok token"

say "checking token refresh"
# The first request above refreshed the imported credential and wrote it to
# our own file; age it so the next request has to refresh it again.
test -f "$WORK/auth.json" || fail "no credentials were stored in $WORK/auth.json"
chmod 600 "$WORK/auth.json"
python3 - "$WORK/auth.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    auth = json.load(handle)
auth["expires_at"] = 1  # long expired
with open(path, "w") as handle:
    json.dump(auth, handle)
print("the stored token has been aged out")
PY
curl -sf -X POST "$BASE/v1/chat/completions" \
    -H 'Content-Type: application/json' \
    -d '{"model":"grok-4.7","messages":[{"role":"user","content":"ok"}],"max_tokens":8}' \
    >/dev/null || fail "the request after a forced refresh failed"
python3 - "$WORK/auth.json" <<'PY'
import json, sys, time
with open(sys.argv[1]) as handle:
    auth = json.load(handle)
assert auth["expires_at"] > time.time(), "the token was not refreshed"
assert auth.get("refresh_token"), "the refresh token was lost"
print("the token was refreshed and the refresh token was kept")
PY

say "checking that the daemon stops cleanly"
"$BIN" stop
curl -sf --max-time 2 "$BASE/health" >/dev/null 2>&1 \
    && fail "the server is still answering after stop"
echo "the server stopped"

printf '\n\033[32mAll live checks passed.\033[0m\n'
