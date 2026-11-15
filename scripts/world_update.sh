#!/usr/bin/env bash
# Stop-update-start release of a world's images (`just orion-world-update`), and its way
# back (`just orion-world-rollback`):
#   world_update.sh <context> <namespace> <server-ref> <web-ref> [--dry-run]
#   world_update.sh <context> <namespace> --rollback [--dry-run]
# Both refs are image references by digest (`<repo>@sha256:…`); a tag is refused
# (exit 2), because a moving `latest` would not roll out reliably.
# The hosts stop first, all of them, so that none runs the old image beside a new
# coordinator: scaling the StatefulSet to 0 makes each host ask for a leave cut and
# exit (bounded by about 90 s), then every workload gets its new image, the
# coordinator and web roll out, and the hosts come back at the replica count the
# script remembered. `--dry-run` prints the commands and changes nothing.
# Before anything is scaled a preflight checks the four workloads and their
# container names (coordinator, toolbox, host, web).
#
# Release history: before any image is set, the refs being replaced are recorded in the
# ConfigMap `release-history` of the world's namespace (it lives with the world, not on
# the operator's machine): previous-server, previous-web (what ran), released-server,
# released-web (what this run sets), recorded-at (UTC). A re-run of the recorded release
# (after a failure) keeps the record; server images that differ between coordinator,
# toolbox and hosts (not a re-run) leave no usable record (the previous release is not
# known) and a later --rollback refuses. `--rollback` reads previous-server /
# previous-web and runs this same path to them; it refuses (exit 2, nothing changed)
# when no previous release is recorded or a recorded ref is not a digest. A rollback is
# a release too: it records what it replaced, so a second --rollback goes forward again.
#
# Leave outcome: each host pod's log is followed from before the scale-down until the
# pod ends; once they are gone the script prints each host's leave line, and WARNS (it
# goes on) when a host left without a committed cut — the next start raises the world
# generation (the world rolls back to its last committed cut) — or when no leave line
# was seen.
#
# If the script stops after the hosts were scaled down, it says so and how to go on:
# a failure while the hosts stop (step 2, no image changed yet) offers
# `kubectl … scale statefulset/hosts --replicas=N` or a re-run; once step 3 began the
# images are partly replaced and only the re-run with HOSTS_REPLICAS=N and the same two
# refs is offered (a cluster that reads 0 hosts is refused without it, so a re-run
# cannot "finish" with the world down).
# Env: HOSTS_REPLICAS (the count to restore), WORLD_UPDATE_STOP_TIMEOUT (seconds
# to wait for the host pods to go, default 150 — a leave cut is bounded by ~90 s),
# WORLD_UPDATE_READY_TIMEOUT (seconds to wait for every host Ready, default 300).
# Exit: 0 done, 1 a step failed or timed out, 2 bad arguments or a refused start.
set -euo pipefail

usage() {
    echo "usage: $0 <context> <namespace> <server-ref> <web-ref> [--dry-run]" >&2
    echo "       $0 <context> <namespace> --rollback [--dry-run]" >&2
    exit 2
}
[ $# -ge 3 ] || usage
ctx="$1" ns="$2"
shift 2
rollback=0 dry=0
if [ "$1" = "--rollback" ]; then
    rollback=1
    shift
    server_ref='' web_ref=''
else
    [ $# -ge 2 ] || usage
    server_ref="$1" web_ref="$2"
    shift 2
fi
case "$*" in
    '') ;;
    --dry-run) dry=1 ;;
    *) usage ;;
esac
stop_timeout="${WORLD_UPDATE_STOP_TIMEOUT:-150}"
ready_timeout="${WORLD_UPDATE_READY_TIMEOUT:-300}"
history=release-history

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
step() { echo "world_update: [$1/6] $2"; }
# run <kubectl args…>: prints the command; runs it unless --dry-run.
run() {
    echo "  + kubectl --context $ctx -n $ns $*"
    [ "$dry" -eq 1 ] || k "$@"
}
is_digest() { case "$1" in *@sha256:*) return 0 ;; *) return 1 ;; esac; }
# record_field <key>: one key of the release history ('' when there is none); a failed
# read fails (nothing changed yet).
record_field() {
    k get configmap "$history" --ignore-not-found -o "jsonpath={.data.$1}"
}

# Recovery: armed when the hosts are about to be scaled down, disarmed at the end.
replicas='' stopped=0 images_touched=0 scaled_up=0 finished=0 where='before step 2'
follow_dir='' follow_pids=()
on_exit() {
    local rc=$?
    for p in "${follow_pids[@]}"; do kill "$p" 2>/dev/null || true; done
    [ -z "$follow_dir" ] || rm -rf "$follow_dir"
    if [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$scaled_up" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the hosts were scaled back to $replicas but are not all Ready yet; the new images are set."
            echo "world_update: look at them: kubectl --context $ctx -n $ns get pods"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$images_touched" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the hosts are scaled to 0 (were $replicas); the world is DOWN, and the images are PARTLY"
            echo "world_update: replaced (some workloads may run the new build, some the old): do NOT scale the hosts by hand."
            echo "world_update: go on by re-running with the same two refs:"
            echo "  HOSTS_REPLICAS=$replicas $0 $ctx $ns $server_ref $web_ref"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the hosts are scaled to 0 (were $replicas); the world is DOWN. No image was changed."
            echo "world_update: go on with either:"
            echo "  kubectl --context $ctx -n $ns scale statefulset/hosts --replicas=$replicas"
            echo "  HOSTS_REPLICAS=$replicas $0 $ctx $ns $server_ref $web_ref"
        } >&2
    fi
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$rollback" -eq 1 ]; then
    echo "world_update: rollback: the previous release from ConfigMap $history"
    echo "  + kubectl --context $ctx -n $ns get configmap $history (a read, also with --dry-run)"
    if ! server_ref=$(record_field previous-server) || ! web_ref=$(record_field previous-web) ||
        ! at=$(record_field recorded-at); then
        echo "world_update: cannot read ConfigMap $history; nothing changed" >&2
        exit 2
    fi
    if [ -z "$server_ref" ] || [ -z "$web_ref" ]; then
        echo "world_update: no previous release is recorded in ConfigMap $history (no release by this script yet," >&2
        echo "world_update: or the last one could not tell what it replaced); refusing. Nothing changed." >&2
        echo "world_update: roll back by naming the refs: $0 $ctx $ns <server>@sha256:… <web>@sha256:…" >&2
        exit 2
    fi
    for ref in "$server_ref" "$web_ref"; do
        if ! is_digest "$ref"; then
            echo "world_update: the recorded previous image '$ref' is not a digest reference (<repo>@sha256:…);" >&2
            echo "world_update: refusing. Nothing changed. Roll back by naming digest refs: $0 $ctx $ns <server>@sha256:… <web>@sha256:…" >&2
            exit 2
        fi
    done
    echo "  previous release (replaced at $at): server $server_ref, web $web_ref"
fi
for ref in "$server_ref" "$web_ref"; do
    is_digest "$ref" || { echo "world_update: '$ref' is not a digest reference (<repo>@sha256:…); refusing" >&2; exit 2; }
done

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

step 1 "remember the hosts' replica count and record the release being replaced"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns get statefulset/hosts -o jsonpath={.spec.replicas}"
    replicas="${HOSTS_REPLICAS:-<replicas>}"
    echo "  + read the four images; unless this re-runs the recorded release, record them in ConfigMap $history:"
    echo "  + kubectl --context $ctx -n $ns create configmap $history --from-literal=previous-server=<coordinator's image> --from-literal=previous-web=<web's image> --from-literal=released-server=$server_ref --from-literal=released-web=$web_ref --from-literal=recorded-at=<now> --dry-run=client -o yaml | kubectl --context $ctx -n $ns apply -f -"
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

    # image_of <kind/name> <container>
    image_of() { k get "$1" -o "jsonpath={.spec.template.spec.containers[?(@.name==\"$2\")].image}"; }
    if ! rec_server=$(record_field released-server) || ! rec_web=$(record_field released-web); then
        echo "world_update: cannot read ConfigMap $history; nothing changed" >&2
        exit 2
    fi
    if [ "$rec_server" = "$server_ref" ] && [ "$rec_web" = "$web_ref" ]; then
        echo "  a re-run of the recorded release: ConfigMap $history kept (previous: server $(record_field previous-server || true), web $(record_field previous-web || true))"
    else
        if ! cur_coord=$(image_of deploy/coordinator coordinator) || ! cur_toolbox=$(image_of deploy/toolbox toolbox) ||
            ! cur_hosts=$(image_of statefulset/hosts host) || ! cur_web=$(image_of deploy/web web); then
            echo "world_update: cannot read the workloads' images; nothing changed" >&2
            exit 2
        fi
        if [ "$cur_coord" = "$server_ref" ] && [ "$cur_toolbox" = "$server_ref" ] && [ "$cur_hosts" = "$server_ref" ] &&
            [ "$cur_web" = "$web_ref" ]; then
            echo "  every workload already runs these refs: ConfigMap $history kept"
        else
            if [ "$cur_coord" = "$cur_toolbox" ] && [ "$cur_coord" = "$cur_hosts" ]; then
                prev_server="$cur_coord" prev_web="$cur_web"
                echo "  replacing: server $prev_server, web $prev_web"
            else
                prev_server='' prev_web=''
                {
                    echo "world_update: WARNING: the server images differ (coordinator $cur_coord, toolbox $cur_toolbox, hosts $cur_hosts)"
                    echo "world_update: and this is not a re-run of the recorded release: the previous release is not known."
                    echo "world_update: ConfigMap $history is written without one; --rollback will refuse until the next release."
                } >&2
            fi
            if ! k create configmap "$history" \
                --from-literal=previous-server="$prev_server" --from-literal=previous-web="$prev_web" \
                --from-literal=released-server="$server_ref" --from-literal=released-web="$web_ref" \
                --from-literal=recorded-at="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
                --dry-run=client -o yaml | k apply -f - > /dev/null; then
                echo "world_update: could not write ConfigMap $history; nothing changed" >&2
                exit 2
            fi
            echo "  recorded in ConfigMap $history"
        fi
    fi
fi

step 2 "scale hosts to 0 and wait until no host pod is left ($stop_timeout s)"
where='step 2 (scale down / wait for the host pods)'
# Each host pod's log is followed until the pod ends: its leave line is read after it is gone.
if [ "$dry" -eq 1 ]; then
    echo "  + follow each host pod's log until it ends (kubectl --context $ctx -n $ns logs -f <pod>), for its leave line"
else
    follow_dir=$(mktemp -d)
    if ! listing=$(k get pods -o name 2>&1); then
        echo "world_update: cannot list the pods ($(printf '%s' "$listing" | head -1)); nothing changed" >&2
        exit 2
    fi
    for pod in $(printf '%s\n' "$listing" | sed -n 's#^pod/\(hosts-[0-9][0-9]*\)$#\1#p'); do
        k logs -f --tail=20 "$pod" > "$follow_dir/$pod.log" 2>&1 &
        follow_pids+=("$!")
    done
    sleep 1
fi
if ! run scale statefulset/hosts --replicas=0; then
    echo "world_update: the scale-down failed; nothing was stopped, nothing changed" >&2
    exit 1
fi
[ "$dry" -eq 1 ] || stopped=1
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod of statefulset/hosts exists (timeout $stop_timeout s; a failed listing is retried, never read as empty)"
    echo "  + print each host's leave line; WARN when one left without a cut"
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
    # The followers end with their pods; give the last lines a moment, then stop any left.
    sleep 1
    for p in "${follow_pids[@]}"; do kill "$p" 2>/dev/null || true; done
    follow_pids=()
    echo "  the hosts' leave:"
    [ "$(ls "$follow_dir")" ] || echo "    (no host pod was running)"
    no_cut='' unknown=''
    for f in "$follow_dir"/*.log; do
        [ -e "$f" ] || continue
        pod=$(basename "$f" .log)
        if grep -qF "left without a cut" "$f"; then
            echo "    $pod: LEFT WITHOUT A CUT: $(grep -F 'left without a cut' "$f" | tail -1 | sed 's/.*\(\[server\]\)/\1/')"
            no_cut="$no_cut $pod"
        elif grep -qF "[server] left the world in cut" "$f"; then
            echo "    $pod: $(grep -F '[server] left the world in cut' "$f" | tail -1 | sed 's/.*\(\[server\] left the world in cut\)/\1/')"
        else
            echo "    $pod: no leave line seen in its log"
            unknown="$unknown $pod"
        fi
    done
    if [ -n "$no_cut" ]; then
        {
            echo "world_update: ************************************************************************"
            echo "world_update: WARNING: host(s)$no_cut left WITHOUT a committed cut. The next start raises the"
            echo "world_update: world generation: the world rolls back to its last committed cut. Going on."
            echo "world_update: ************************************************************************"
        } >&2
    fi
    if [ -n "$unknown" ]; then
        echo "world_update: WARNING: no leave line seen for$unknown (its log could not be followed to the end); whether it left with a cut is not known. Going on." >&2
    fi
fi

step 3 "set the images"
where='step 3 (set image)'
images_touched=1
run set image deploy/coordinator "coordinator=$server_ref"
run set image deploy/toolbox "toolbox=$server_ref"
# The hosts' StatefulSet is OnDelete: this only changes the template, which is what we want
# (the hosts are at 0; the pods created at step 5 come from the new template).
run set image statefulset/hosts "host=$server_ref"
run set image deploy/web "web=$web_ref"

step 4 "wait for the coordinator, toolbox and web rollouts"
where='step 4 (rollouts)'
run rollout status deploy/coordinator --timeout=5m
run rollout status deploy/toolbox --timeout=5m
run rollout status deploy/web --timeout=5m

step 5 "scale hosts back to $replicas"
where='step 5 (scale up)'
run scale statefulset/hosts "--replicas=$replicas"
scaled_up=1

step 6 "wait until every host is Ready ($ready_timeout s)"
where='step 6 (hosts Ready)'
# Not `rollout status`: kubectl refuses it for an OnDelete StatefulSet. Poll readyReplicas;
# a failed read keeps waiting inside the budget and never counts as ready.
if [ "$dry" -eq 1 ]; then
    echo "  + poll kubectl --context $ctx -n $ns get statefulset/hosts -o jsonpath={.status.readyReplicas} until it is $replicas (timeout $ready_timeout s)"
else
    deadline=$((SECONDS + ready_timeout))
    while :; do
        if ready=$(k get statefulset/hosts -o jsonpath='{.status.readyReplicas}' 2>/dev/null) &&
            [ "${ready:-0}" -eq "$replicas" ] 2>/dev/null; then
            break
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $ready_timeout s: ${ready:-0} of $replicas hosts Ready" >&2
            exit 1
        fi
        sleep "${WORLD_UPDATE_POLL:-3}"
    done
fi
finished=1
echo "world_update: done (server $server_ref, web $web_ref)"
