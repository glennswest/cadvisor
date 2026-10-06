#!/usr/bin/env bash
# Structural /metrics diff: real cadvisor (REF_URL) vs cadvisor-rs (OURS_URL).
# Both must observe the same host. Exits nonzero on unexpected deltas.
set -euo pipefail

REF_URL=${REF_URL:-http://localhost:18080/metrics}
OURS_URL=${OURS_URL:-http://localhost:18081/metrics}
HERE=$(cd "$(dirname "$0")" && pwd)
W=$(mktemp -d "${TMPDIR:-/tmp}/conformance.XXXXXX")
trap 'rm -rf "$W"' EXIT

curl -sf "$REF_URL" > "$W/ref.prom"
curl -sf "$OURS_URL" > "$W/ours.prom"
python3 "$HERE/normalize-metrics.py" "$W/ref.prom" > "$W/ref.norm"
python3 "$HERE/normalize-metrics.py" "$W/ours.prom" > "$W/ours.norm"

if diff -u "$W/ref.norm" "$W/ours.norm" > "$W/metrics.diff"; then
    echo "metrics conformance: OK ($(wc -l < "$W/ref.norm") normalized lines)"
else
    echo "metrics conformance: DIFFERENCES"
    head -80 "$W/metrics.diff"
    exit 1
fi
