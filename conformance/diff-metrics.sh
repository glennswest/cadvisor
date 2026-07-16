#!/usr/bin/env bash
# Structural /metrics diff: real cadvisor (REF_URL) vs cadvisor-rs (OURS_URL).
# Both must observe the same host. Exits nonzero on unexpected deltas.
set -euo pipefail

REF_URL=${REF_URL:-http://localhost:18080/metrics}
OURS_URL=${OURS_URL:-http://localhost:18081/metrics}
HERE=$(cd "$(dirname "$0")" && pwd)

curl -sf "$REF_URL" > /tmp/conformance-ref.prom
curl -sf "$OURS_URL" > /tmp/conformance-ours.prom
python3 "$HERE/normalize-metrics.py" /tmp/conformance-ref.prom > /tmp/conformance-ref.norm
python3 "$HERE/normalize-metrics.py" /tmp/conformance-ours.prom > /tmp/conformance-ours.norm

if diff -u /tmp/conformance-ref.norm /tmp/conformance-ours.norm > /tmp/conformance-metrics.diff; then
    echo "metrics conformance: OK ($(wc -l < /tmp/conformance-ref.norm) normalized lines)"
else
    echo "metrics conformance: DIFFERENCES"
    head -80 /tmp/conformance-metrics.diff
    exit 1
fi
