#!/usr/bin/env bash
#
# Manually probe candidate image-generation routes through a running EggPass.
#
# Usage:
#   EGGPASS_API_KEY=... scripts/probe-media-endpoints.sh
#   EGGPASS_BASE_URL=http://127.0.0.1:8787 EGGPASS_API_KEY=... scripts/probe-media-endpoints.sh
#   EGGPASS_XAI_MODEL=grok-2-image EGGPASS_OPENAI_MODEL=gpt-image-1 scripts/probe-media-endpoints.sh
#
# The server must be running with a signed-in account for each candidate.
# Optional model overrides: EGGPASS_XAI_MODEL and EGGPASS_OPENAI_MODEL.
# The script prints only each HTTP status and Content-Type, never the response
# body, response headers, or API key. A non-2xx result is not enabled.

set -euo pipefail

BASE_URL="${EGGPASS_BASE_URL:-http://127.0.0.1:8787}"
API_KEY="${EGGPASS_API_KEY:-}"
XAI_MODEL="${EGGPASS_XAI_MODEL:-grok-2-image}"
OPENAI_MODEL="${EGGPASS_OPENAI_MODEL:-gpt-image-1}"

if [ -z "$API_KEY" ]; then
    echo "EGGPASS_API_KEY is required" >&2
    exit 2
fi

if ! printf '%sX' "$API_KEY" | awk 'index($0, "\r") || NR > 1 { bad = 1 } END { exit bad }'; then
    echo "EGGPASS_API_KEY must be a single-line value" >&2
    exit 2
fi

for model in "$XAI_MODEL" "$OPENAI_MODEL"; do
    case "$model" in
        ''|*[!A-Za-z0-9._-]*) echo "model overrides may contain only letters, digits, dot, underscore, and hyphen" >&2; exit 2 ;;
    esac
done

BASE_URL="${BASE_URL%/}"

probe() {
    candidate="$1"
    model="$2"
    url="$BASE_URL/v1/images/generations"
    body="{\"model\":\"$model\",\"prompt\":\"a white 1x1 square\",\"n\":1,\"size\":\"1024x1024\"}"

    if result=$(printf '%s' "$API_KEY" \
        | sed 's/\\/\\\\/g; s/"/\\"/g' \
        | sed 's/^/header = "Authorization: Bearer /; s/$/"/' \
        | curl --config - --silent --output /dev/null --write-out '%{http_code}|%{content_type}' \
            --connect-timeout 5 --max-time 600 \
            --request POST --header 'Content-Type: application/json' \
            --data-binary "$body" "$url" 2>/dev/null); then
        status="${result%%|*}"
        content_type="${result#*|}"
    else
        status=000
        content_type=
    fi

    case "$status" in
        2[0-9][0-9]) enabled=yes ;;
        *) enabled=no ;;
    esac
    [ -n "$content_type" ] || content_type=-
    printf '%-12s %-8s %-28s %s\n' "$candidate" "$status" "$content_type" "$enabled"
}

printf '%-12s %-8s %-28s %s\n' CANDIDATE STATUS CONTENT_TYPE ENABLED
probe 'x.ai' "$XAI_MODEL"
probe 'OpenAI' "$OPENAI_MODEL"
printf '\nNon-2xx candidates stay disabled (media_paths: &[]).\n'
