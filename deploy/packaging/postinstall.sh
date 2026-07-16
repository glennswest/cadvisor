#!/bin/sh
# Post-install for the cadvisor-rs package. Reloads systemd; the service is
# NOT auto-enabled — the operator opts in.
set -e

if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload || true
fi

echo "cadvisor-rs installed. Next:"
echo "  1) optionally set CADVISOR_ARGS in /etc/cadvisor/cadvisor (default port 8080)"
echo "  2) systemctl enable --now cadvisor"
