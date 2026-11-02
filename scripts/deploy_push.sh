#!/usr/bin/env bash
# Ships a built deploy (`just deploy-build`: target/web + the release server) to
# the game box and restarts the game (`just deploy-push`). See
# deploy/web/compose.yml for what runs there.
#   /srv/mmo/releases/<rev>/   one web bundle per deploy (hard-linked against the
#                              previous one, so unchanged assets are not re-sent);
#                              the last KEEP are kept
#   /srv/mmo/web -> releases/<rev>   swapped atomically
#   /srv/mmo/bin/server        the release server binary
#   /srv/mmo/worlds/           the world, its history and region saves (never touched)
# Env: DEPLOY_HOST (root@5.9.7.138), DEPLOY_DIR (/srv/mmo), WEB (target/web),
# SERVER_BIN (target/release/server).
set -euo pipefail
cd "$(dirname "$0")/.."
host="${DEPLOY_HOST:-root@5.9.7.138}"
dir="${DEPLOY_DIR:-/srv/mmo}"
web="${WEB:-target/web}"
bin="${SERVER_BIN:-target/release/server}"
keep=3

[ -f "$web/api/version" ] || { echo "deploy-push: no $web (run just deploy-build)" >&2; exit 1; }
[ -x "$bin" ] || { echo "deploy-push: no $bin (run just deploy-build)" >&2; exit 1; }
rev=$(cat "$web/api/version")
ssh_() { ssh -o BatchMode=yes "$host" "$@"; }

ssh_ "mkdir -p $dir/releases $dir/bin $dir/worlds"
prev=$(ssh_ "readlink $dir/web || true")
link=()
[ -n "$prev" ] && link=(--link-dest="$dir/$prev")
echo "deploy: web $rev -> $host:$dir/releases/$rev"
rsync -az --delete "${link[@]}" "$web/" "$host:$dir/releases/$rev/"
echo "deploy: server binary"
rsync -az "$bin" "$host:$dir/bin/server.new"
rsync -az deploy/web/compose.yml "$host:$dir/compose.yml"

ssh_ bash -s -- "$dir" "$rev" "$keep" <<'REMOTE'
set -euo pipefail
dir=$1 rev=$2 keep=$3
cd "$dir"
mv -f bin/server.new bin/server
ln -sfn "releases/$rev" web.next && mv -Tf web.next web
# Prune old bundles, never the live one.
{ ls -1t releases | grep -vxF "$rev" || true; } | tail -n +"$keep" | while read -r old; do rm -rf "releases/$old"; done
docker compose -f compose.yml up -d --force-recreate --remove-orphans
REMOTE

echo "deploy: waiting for the WebTransport ingress"
for _ in $(seq 1 60); do
    if ssh_ "docker compose -f $dir/compose.yml logs --since 5m server 2>&1 | grep -q 'WebTransport endpoint listening'"; then
        echo "deploy: $rev is live — https://orion.factoriosurvivalworld.com/"
        exit 0
    fi
    sleep 2
done
echo "deploy: the server did not report its WebTransport endpoint within 120 s:" >&2
ssh_ "docker compose -f $dir/compose.yml logs --tail 40 server" >&2
exit 1
