#!/usr/bin/env bash
# Fallback for when the operator VPN is down (`orion` goes over the VPN since
# 2026-11-15, docs/superpowers/plans/2026-11-15-orion-vpn.md): forward the k3s API
# on the deploy box to localhost:6443, in the background, for the `orion-ssh`
# kubeconfig context (its server is https://127.0.0.1:6443; the box's firewall
# keeps 6443 closed to the outside).
#   scripts/tunnel.sh [user@host]   (default root@5.9.7.138)
# Stop it with: pkill -f 'L 6443:127.0.0.1:6443'
set -euo pipefail
host="${1:-root@5.9.7.138}"
if ss -ltn 'sport = :6443' | grep -q LISTEN; then
  echo "localhost:6443 is already forwarded (or in use)"
  exit 0
fi
ssh -fN -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -L 6443:127.0.0.1:6443 "$host"
echo "tunnel up: kubectl --context orion ..."
