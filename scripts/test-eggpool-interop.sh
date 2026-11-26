#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: sh scripts/test-eggpool-interop.sh /path/to/eggpool/rust/Cargo.toml" >&2
    exit 2
fi

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
EGGPASS_ROOT=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
EGGPOOL_MANIFEST=$1
case "$EGGPOOL_MANIFEST" in
    /*) ;;
    *) EGGPOOL_MANIFEST=$(pwd)/$EGGPOOL_MANIFEST ;;
esac
if [ ! -f "$EGGPOOL_MANIFEST" ]; then
    echo "EggPool manifest not found: $EGGPOOL_MANIFEST" >&2
    exit 2
fi

# The consumer integration test invokes this binary with a fresh EGGPASS_HOME
# and captures the one-time ticket in memory; it never prints ticket contents.
CARGO_TARGET_DIR="$EGGPASS_ROOT/target" cargo build     --manifest-path "$EGGPASS_ROOT/Cargo.toml"     --bin eggpass
EGGPASS_BIN="$EGGPASS_ROOT/target/debug/eggpass" cargo test     --manifest-path "$EGGPOOL_MANIFEST"     --test agent_enrollment     real_eggpass_cli_ticket_completes_callback_mtls_and_authenticated_api_request     -- --ignored
