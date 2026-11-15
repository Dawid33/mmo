#!/usr/bin/env bash
# Stop-update-start release of a world's images (`just orion-world-update`):
#   world_update.sh <context> <namespace> <server-ref> <web-ref> [--dry-run]
# Both refs are image references by digest (`<repo>@sha256:…`); a tag is refused
# (exit 2), because a moving `latest` would not roll out reliably.
# The hosts stop first, all of them, so that none runs the old image beside a new
# coordinator: scaling the StatefulSet to 0 makes each host ask for a leave cut and
# exit (bounded by about 90 s), then every workload gets its new image, the
# coordinator and web roll out, and the hosts come back at the replica count the
# script remembered. `--dry-run` prints the commands and runs none.
# Exit: 0 done, 1 a step failed or timed out (the message says what state the
# world is in), 2 bad arguments.
set -euo pipefail

usage() { echo "usage: $0 <context> <namespace> <server-ref> <web-ref> [--dry-run]" >&2; exit 2; }
[ $# -eq 4 ] || { [ $# -eq 5 ] && [ "$5" = "--dry-run" ]; } || usage
ctx="$1" ns="$2" server_ref="$3" web_ref="$4"
dry=0
[ "${5:-}" = "--dry-run" ] && dry=1
for ref in "$server_ref" "$web_ref"; do
    case "$ref" in
        *@sha256:*) ;;
        *) echo "world_update: '$ref' is not a digest reference (<repo>@sha256:…); refusing" >&2; exit 2 ;;
    esac
done

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
# step <text>: announces a step.
step() { echo "world_update: [$1/6] $2"; }
# run <command…>: prints the command; runs it unless --dry-run.
run() {
    echo "  + kubectl --context $ctx -n $ns $*"
    [ "$dry" -eq 1 ] || k "$@"
}

step 1 "remember the hosts' replica count"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns get statefulset/hosts -o jsonpath={.spec.replicas}"
    replicas='<replicas>'
else
    replicas=$(k get statefulset/hosts -o jsonpath='{.spec.replicas}')
    case "$replicas" in ''|*[!0-9]*) echo "world_update: could not read the replica count ('$replicas'); nothing changed" >&2; exit 1 ;; esac
    echo "  hosts run $replicas replicas"
fi

step 2 "scale hosts to 0 and wait until no host pod is left (150 s)"
run scale statefulset/hosts --replicas=0
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod of statefulset/hosts exists (timeout 150 s)"
else
    deadline=$((SECONDS + 150))
    while :; do
        left=$(k get pods -o name | grep -c '^pod/hosts-[0-9]' || true)
        [ "$left" -eq 0 ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: $left host pod(s) still there after 150 s; the hosts are scaled to 0, nothing is updated. Look at them, then re-run." >&2
            exit 1
        fi
        sleep 2
    done
fi

step 3 "set the images"
run set image deploy/coordinator "coordinator=$server_ref"
run set image deploy/toolbox "toolbox=$server_ref"
run set image statefulset/hosts "host=$server_ref"
run set image deploy/web "web=$web_ref"

step 4 "wait for the coordinator and web rollouts"
run rollout status deploy/coordinator --timeout=5m
run rollout status deploy/web --timeout=5m

step 5 "scale hosts back to $replicas"
run scale statefulset/hosts "--replicas=$replicas"

step 6 "wait until every host is Ready (300 s)"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns rollout status statefulset/hosts --timeout=300s"
else
    k rollout status statefulset/hosts --timeout=300s
    # rollout status on a freshly scaled StatefulSet can return before the new pods
    # exist; insist on the full count.
    ready=$(k get statefulset/hosts -o jsonpath='{.status.readyReplicas}')
    [ "${ready:-0}" -eq "$replicas" ] || { echo "world_update: $ready of $replicas hosts Ready" >&2; exit 1; }
fi
echo "world_update: done (server $server_ref, web $web_ref)"
