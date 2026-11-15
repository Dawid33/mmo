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
# Before anything is scaled a preflight checks the four workloads and their
# container names (coordinator, toolbox, host, web).
# If the script stops after the hosts were scaled down, it says so and how to go
# on: `kubectl … scale statefulset/hosts --replicas=N`, or re-run with
# HOSTS_REPLICAS=N (a cluster that reads 0 hosts is refused without it, so a re-run
# cannot "finish" with the world down).
# Env: HOSTS_REPLICAS (the count to restore), WORLD_UPDATE_STOP_TIMEOUT (seconds
# to wait for the host pods to go, default 150 — a leave cut is bounded by ~90 s).
# Exit: 0 done, 1 a step failed or timed out, 2 bad arguments or a refused start.
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
stop_timeout="${WORLD_UPDATE_STOP_TIMEOUT:-150}"

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
step() { echo "world_update: [$1/6] $2"; }
# run <kubectl args…>: prints the command; runs it unless --dry-run.
run() {
    echo "  + kubectl --context $ctx -n $ns $*"
    [ "$dry" -eq 1 ] || k "$@"
}

# Recovery: armed when the hosts are about to be scaled down, disarmed at the end.
replicas='' stopped=0 finished=0 where='before step 2'
on_exit() {
    local rc=$?
    if [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the hosts are scaled to 0 (were $replicas); the world is DOWN."
            echo "world_update: go on with either:"
            echo "  kubectl --context $ctx -n $ns scale statefulset/hosts --replicas=$replicas"
            echo "  HOSTS_REPLICAS=$replicas $0 $ctx $ns $server_ref $web_ref"
        } >&2
    fi
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "world_update: preflight: the four workloads and their containers"
# preflight <kind/name> <container>: the workload exists and has that container.
preflight() {
    local obj="$1" want="$2" names
    echo "  + kubectl --context $ctx -n $ns get $obj -o jsonpath={.spec.template.spec.containers[*].name}"
    [ "$dry" -eq 1 ] && return 0
    if ! names=$(k get "$obj" -o jsonpath='{.spec.template.spec.containers[*].name}'); then
        echo "world_update: cannot read $obj; nothing changed" >&2
        exit 2
    fi
    case " $names " in
        *" $want "*) ;;
        *) echo "world_update: $obj has containers '$names', no '$want'; nothing changed" >&2; exit 2 ;;
    esac
}
preflight deploy/coordinator coordinator
preflight deploy/toolbox toolbox
preflight statefulset/hosts host
preflight deploy/web web

step 1 "remember the hosts' replica count"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns get statefulset/hosts -o jsonpath={.spec.replicas}"
    replicas="${HOSTS_REPLICAS:-<replicas>}"
else
    if ! read_count=$(k get statefulset/hosts -o jsonpath='{.spec.replicas}'); then
        echo "world_update: could not read the replica count; nothing changed" >&2
        exit 2
    fi
    case "$read_count" in ''|*[!0-9]*) echo "world_update: replica count '$read_count' is not a number; nothing changed" >&2; exit 2 ;; esac
    replicas="${HOSTS_REPLICAS:-$read_count}"
    case "$replicas" in ''|*[!0-9]*) echo "world_update: HOSTS_REPLICAS='$replicas' is not a number" >&2; exit 2 ;; esac
    if [ "$replicas" -eq 0 ]; then
        echo "world_update: the hosts read 0 replicas: a world already stopped (or a failed run). Refusing; say how many to restore with HOSTS_REPLICAS=<n>. Nothing changed." >&2
        exit 2
    fi
    echo "  hosts run $read_count replicas; will restore $replicas"
fi

step 2 "scale hosts to 0 and wait until no host pod is left ($stop_timeout s)"
[ "$dry" -eq 1 ] || stopped=1
where='step 2 (scale down / wait for the host pods)'
run scale statefulset/hosts --replicas=0
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod of statefulset/hosts exists (timeout $stop_timeout s; a failed listing is retried, never read as empty)"
else
    deadline=$((SECONDS + stop_timeout))
    while :; do
        if listing=$(k get pods -o name 2>&1); then
            left=$(printf '%s\n' "$listing" | grep -c '^pod/hosts-[0-9]' || true)
            [ "$left" -eq 0 ] && break
            what="$left host pod(s) still there"
        else
            what="the pod listing failed ($(printf '%s' "$listing" | head -1))"
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $stop_timeout s: $what; not updating anything" >&2
            exit 1
        fi
        sleep 2
    done
fi

step 3 "set the images"
where='step 3 (set image)'
run set image deploy/coordinator "coordinator=$server_ref"
run set image deploy/toolbox "toolbox=$server_ref"
run set image statefulset/hosts "host=$server_ref"
run set image deploy/web "web=$web_ref"

step 4 "wait for the coordinator and web rollouts"
where='step 4 (rollouts)'
run rollout status deploy/coordinator --timeout=5m
run rollout status deploy/web --timeout=5m

step 5 "scale hosts back to $replicas"
where='step 5 (scale up)'
run scale statefulset/hosts "--replicas=$replicas"

step 6 "wait until every host is Ready (300 s)"
where='step 6 (hosts Ready)'
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns rollout status statefulset/hosts --timeout=300s"
else
    k rollout status statefulset/hosts --timeout=300s
    # rollout status on a freshly scaled StatefulSet can return before the new pods
    # exist; insist on the full count.
    ready=$(k get statefulset/hosts -o jsonpath='{.status.readyReplicas}')
    [ "${ready:-0}" -eq "$replicas" ] || { echo "world_update: ${ready:-0} of $replicas hosts Ready" >&2; exit 1; }
fi
finished=1
echo "world_update: done (server $server_ref, web $web_ref)"
