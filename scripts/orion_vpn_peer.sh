#!/usr/bin/env bash
# Adds a device to the operator VPN on orion (`just orion-vpn-peer <name> <octet>`;
# docs/superpowers/plans/2026-11-15-orion-vpn.md). The device gets 10.99.0.<octet>.
# Generates the device's key pair HERE, adds its public key to the box's wg0 (live,
# `wg set`, and in /etc/wireguard/wg0.conf so a reboot keeps it), and writes the
# device's WireGuard config to target/orion-vpn/<name>.conf (mode 600, holds its
# private key; git-ignored). Move that file to the device and delete it here.
#   NetworkManager: nmcli connection import type wireguard file <name>.conf, then
#     nmcli connection modify <name> ipv4.dns 10.43.0.10 \
#       ipv4.dns-search "~cluster.local,~vpn.errantry.renownandruin.com" ipv4.never-default yes
#     (split DNS: only those two domains go to CoreDNS)
#   wg-quick (no NetworkManager): add to [Interface]
#     PostUp = resolvectl dns %i 10.43.0.10; resolvectl domain %i ~cluster.local ~vpn.errantry.renownandruin.com
# Refuses a name or address the box already has. Env: VPN_HOST (root@5.9.7.138).
set -euo pipefail
name="${1:?usage: orion_vpn_peer.sh <name> <octet 3..254>}"
octet="${2:?usage: orion_vpn_peer.sh <name> <octet 3..254>}"
host="${VPN_HOST:-root@5.9.7.138}"
[[ "$name" =~ ^[A-Za-z0-9_-]{1,15}$ ]] || { echo "name: letters, digits, _ or -, at most 15" >&2; exit 2; }
[[ "$octet" =~ ^[0-9]+$ ]] && (( octet >= 3 && octet <= 254 )) || { echo "octet: 3..254 (1 = the box, 2 = the first laptop)" >&2; exit 2; }
ip="10.99.0.$octet"

out=target/orion-vpn
mkdir -p -m 700 "$out"
conf="$out/$name.conf"
[ -e "$conf" ] && { echo "$conf exists: move or delete it first" >&2; exit 2; }

server_pub=$(ssh -o BatchMode=yes "$host" 'wg show wg0 public-key')
umask 077
key=$(wg genkey)
pub=$(printf '%s' "$key" | wg pubkey)

# On the box: refuse duplicates, then add the peer live and to the config file.
ssh -o BatchMode=yes "$host" bash -s -- "$name" "$ip" "$pub" <<'EOF'
set -euo pipefail
name=$1 ip=$2 pub=$3 f=/etc/wireguard/wg0.conf
if grep -qE "^# ${name}\$" "$f"; then echo "the box already has a peer named $name" >&2; exit 2; fi
if grep -qE "^AllowedIPs = ${ip}/32\$" "$f"; then echo "the box already has a peer at $ip" >&2; exit 2; fi
wg set wg0 peer "$pub" allowed-ips "$ip/32"
printf '\n[Peer]\n# %s\nPublicKey = %s\nAllowedIPs = %s/32\n' "$name" "$pub" "$ip" >> "$f"
EOF

cat > "$conf" <<EOF
# Operator VPN into orion: device $name at $ip (docs/superpowers/plans/2026-11-15-orion-vpn.md).
[Interface]
Address = $ip/32
PrivateKey = $key

[Peer]
PublicKey = $server_pub
Endpoint = 5.9.7.138:51820
AllowedIPs = 10.99.0.0/24, 10.42.0.0/16, 10.43.0.0/16
PersistentKeepalive = 25
EOF
echo "peer $name added at $ip; its config: $conf (holds its private key — move it to the device, then delete it)"
