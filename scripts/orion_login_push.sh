#!/usr/bin/env bash
# Ships the release `login` binary to the orion box and restarts the login
# service (`just orion-login-push`). The pod runs stock debian over this file
# through a read-only hostPath (deploy/modules/login), so nothing is built into
# an image.
#   $DEPLOY_DIR/bin/login.new   the upload in flight, then renamed (atomic) onto
#   $DEPLOY_DIR/bin/login       the binary the pods run
#   $DEPLOY_DIR/bin/login.prev  the binary that ran before the last push (copied
#                               just before the rename); the way back after a bad push
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
# Keep the previous binary; a failed copy aborts before the rename.
ssh_ "set -e; if [ -e $dir/bin/login ]; then cp -pf $dir/bin/login $dir/bin/login.prev; fi; mv -f $dir/bin/login.new $dir/bin/login"

if out=$(kubectl --context "$ctx" -n login get deploy/login -o name 2>&1); then
    echo "orion-login: rolling restart"
    if ! { kubectl --context "$ctx" -n login rollout restart deploy/login &&
           kubectl --context "$ctx" -n login rollout status deploy/login --timeout=5m; }; then
        {
            echo "orion-login-push: the new pods did not become ready."
            echo "The old pods are still serving (a rolling restart keeps them until the new ones are Ready), but a restart of one of them would run the new binary."
            echo "Way back:"
            echo "  ssh $host 'cp -pf $dir/bin/login.prev $dir/bin/login'"
            echo "  kubectl --context $ctx -n login rollout restart deploy/login"
        } >&2
        exit 1
    fi
    echo "orion-login: restarted"
elif [[ "$out" == *NotFound* ]]; then
    # Absent Deployment (or namespace): the login root is not applied yet.
    echo "orion-login: binary shipped; apply the login root"
else
    echo "$out" >&2
    echo "orion-login-push: the binary is on the box but was NOT rolled out — fix the cluster access and run \`kubectl --context $ctx -n login rollout restart deploy/login\`" >&2
    exit 2
fi
