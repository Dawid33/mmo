#!/usr/bin/env bash
# Ships the release `login` binary to the orion box and restarts the login
# service (`just orion-login-push`). The pod runs stock debian over this file
# through a read-only hostPath (deploy/modules/login), so nothing is built into
# an image.
#   $DEPLOY_DIR/bin/login.new   the upload, then renamed (atomic) onto
#   $DEPLOY_DIR/bin/login       the binary the pods run
# Then, only if the Deployment exists (namespace login), a rolling restart and
# wait for it: with two replicas, logins continue through it. Before the login
# root is applied there is no Deployment: the binary is shipped and the script
# says so.
# Env: DEPLOY_HOST (root@5.9.7.138), DEPLOY_DIR (/srv/mmo), LOGIN_BIN
# (target/release/login), KUBE_CONTEXT (orion).
set -euo pipefail
cd "$(dirname "$0")/.."
host="${DEPLOY_HOST:-root@5.9.7.138}"
dir="${DEPLOY_DIR:-/srv/mmo}"
bin="${LOGIN_BIN:-target/release/login}"
ctx="${KUBE_CONTEXT:-orion}"

[ -x "$bin" ] || { echo "orion-login-push: no $bin (run just orion-login-build)" >&2; exit 1; }
ssh_() { ssh -o BatchMode=yes "$host" "$@"; }

echo "orion-login: binary -> $host:$dir/bin/login"
ssh_ "mkdir -p $dir/bin"
rsync -az -e "ssh -o BatchMode=yes" "$bin" "$host:$dir/bin/login.new"
ssh_ "mv -f $dir/bin/login.new $dir/bin/login"

if kubectl --context "$ctx" -n login get deploy/login >/dev/null 2>&1; then
    echo "orion-login: rolling restart"
    kubectl --context "$ctx" -n login rollout restart deploy/login
    kubectl --context "$ctx" -n login rollout status deploy/login --timeout=5m
    echo "orion-login: restarted"
else
    echo "orion-login: binary shipped; apply the login root"
fi
