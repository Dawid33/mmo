#!/usr/bin/env bash
# Stop-update-start release of a world's images (`just orion-world-update`), and its way
# back (`just orion-world-rollback`):
#   world_update.sh <context> <namespace> <server-ref> <web-ref> [--dry-run]
#   world_update.sh <context> <namespace> --rollback [--dry-run]
# Both refs are image references by digest (`<repo>@sha256:…`); a tag is refused
# (exit 2), because a moving `latest` would not roll out reliably.
# The hosts are one Deployment each (`host-<name>`, all selected by
# -l app.kubernetes.io/component=host; strategy Recreate, 1 replica). They stop first, all
# of them, so that none runs the old image beside a new coordinator: scaling every host
# Deployment to 0 makes each host ask for a leave cut and exit (bounded by about 90 s), then
# every coordinator replica stops together (the count recorded in the Deployment's annotation
# errantry/replicas, as world-stop does), every workload gets its new image — every host and
# the coordinator Deployment while at 0, where a Recreate Deployment starts nothing — the
# coordinator comes back at its count (every replica's /readyz 200, never `rollout status`),
# toolbox and web roll out, and every host Deployment comes back at 1 replica. A host is never
# started on a mixed set of images: no host Deployment is scaled up before the images of all
# of them are set. `--dry-run` prints the commands and changes nothing (it reads the list of
# host Deployments and the coordinator's count).
# Before anything is scaled a preflight checks the workloads and their container names
# (coordinator, toolbox, every host Deployment's host, web); a world with no host
# Deployment is refused, and so is one whose coordinator is at 0 replicas (a world
# stopped by world-stop: hosts started without a coordinator cannot leave through a cut —
# start the world first), unless this run repeats the release recorded in release-history
# (a failed run that stopped the coordinator: it comes back at the recorded count). A drained host (/readyz `drained`) and a host
# MARKED draining (`worldctl hosts` drain=, not due yet) are WARNED about, all at once, then one 10 s
# pause before anything changes: the release stops each and starts it again as a NEW host — a mark
# is lost — so remove a drained one from var.hosts first if it is to go.
#
# Release history: before any image is set, the refs being replaced are recorded in the
# ConfigMap `release-history` of the world's namespace (it lives with the world, not on
# the operator's machine): previous-server, previous-web (what ran), released-server,
# released-web (what this run sets), recorded-at (UTC). A re-run of the recorded release
# (after a failure) keeps the record; server images that differ between coordinator,
# toolbox and any host Deployment (not a re-run) leave no usable record (the previous
# release is not known) and a later --rollback refuses. `--rollback` reads previous-server /
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
# a failure while the hosts stop (step 2, no image changed yet) offers scaling the host
# Deployments back to 1 or a re-run; once step 3 began the images are partly replaced and
# only the re-run with the same two refs is offered. A world whose host Deployments are
# ALL at 0 replicas is refused (a world already stopped) unless this run repeats the
# release recorded in ConfigMap release-history (the re-run of a failed run): a re-run
# always ends with every host Deployment at 1, so it cannot "finish" with the world down.
# Env: WORLD_UPDATE_STOP_TIMEOUT (seconds to wait for the host pods to go, default 150 —
# a leave cut is bounded by ~90 s), WORLD_UPDATE_READY_TIMEOUT (seconds to wait for every
# host Ready, default 300).
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
hostsel='app.kubernetes.io/component=host'

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
# hostdeps: the host Deployments as `deploy/host-<name>` words; were: their replicas before.
hostdeps='' were='' stopped=0 images_touched=0 scaled_up=0 finished=0 where='before step 2'
# coord_down: the coordinator was scaled to 0 (step 2) and is not back yet (step 4).
coord_down=0 coord_replicas='' coord_count=''
follow_dir='' follow_pids=()
on_exit() {
    local rc=$?
    for p in "${follow_pids[@]}"; do kill "$p" 2>/dev/null || true; done
    [ -z "$follow_dir" ] || rm -rf "$follow_dir"
    if [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$scaled_up" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments were scaled back to 1 but are not all Ready yet; the new images are set."
            echo "world_update: look at them: kubectl --context $ctx -n $ns get deploy,pods -l $hostsel"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$images_touched" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments are scaled to 0 (were:$were); the world is DOWN, and the images are PARTLY"
            echo "world_update: replaced (some workloads may run the new build, some the old): do NOT scale the hosts by hand."
            echo "world_update: go on by re-running with the same two refs:"
            echo "  $0 $ctx $ns $server_ref $web_ref"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments are scaled to 0 (were:$were); the world is DOWN. No image was changed."
            echo "world_update: go on with either:"
            if [ "$coord_down" -eq 1 ]; then
                echo "  kubectl --context $ctx -n $ns scale deploy/coordinator --replicas=$coord_count   (first; then wait until every replica is Ready)"
            fi
            echo "  kubectl --context $ctx -n $ns scale $hostdeps --replicas=1"
            echo "  $0 $ctx $ns $server_ref $web_ref"
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

echo "world_update: preflight: the workloads and their containers"
echo "  + kubectl --context $ctx -n $ns get deploy -l $hostsel -o name (a read, also with --dry-run)"
if ! listing=$(k get deploy -l "$hostsel" -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}'); then
    echo "world_update: cannot list the host Deployments; nothing changed" >&2
    exit 2
fi
hostnames=$(printf '%s\n' "$listing" | grep . || true)
if [ -z "$hostnames" ]; then
    echo "world_update: the world has no host Deployment ($hostsel); refusing. Nothing changed." >&2
    exit 2
fi
for d in $hostnames; do hostdeps="$hostdeps deploy/$d"; done
hostdeps="${hostdeps# }"
echo "  host Deployments: $hostdeps"
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
# The release never scales the coordinator: with it at 0 (a world stopped by world-stop) the
# hosts would start with no coordinator and could never leave through a cut.
echo "  + kubectl --context $ctx -n $ns get deploy/coordinator -o jsonpath={.spec.replicas} (a read, also with --dry-run)"
if ! coord_replicas=$(k get deploy/coordinator -o jsonpath='{.spec.replicas}'); then
    echo "world_update: cannot read the coordinator's replicas; nothing changed" >&2
    exit 2
fi
case "$coord_replicas" in ''|*[!0-9]*) echo "world_update: the coordinator's replica count '$coord_replicas' is not a number; nothing changed" >&2; exit 2 ;; esac
# coord_count: the replicas the coordinator comes back with at step 4 — every one of them stops
# at step 2 and starts at step 4 together. A coordinator at 0 is a stopped world, refused, unless
# this run repeats the release recorded in ConfigMap release-history (a failed run stopped it at
# step 2): then the count recorded at that stop (annotation errantry/replicas, else the module's 2).
if [ "$coord_replicas" = 0 ]; then
    if ! rec_server=$(record_field released-server) || ! rec_web=$(record_field released-web); then
        echo "world_update: cannot read ConfigMap $history; nothing changed" >&2
        exit 2
    fi
    if [ "$rec_server" != "$server_ref" ] || [ "$rec_web" != "$web_ref" ]; then
        echo "world_update: the coordinator is at 0 replicas: the world is stopped. Refusing (hosts started without a" >&2
        echo "world_update: coordinator cannot leave through a cut): start it first (just world-start). Nothing changed." >&2
        exit 2
    fi
    coord_count=$(k get deploy/coordinator -o 'jsonpath={.metadata.annotations.errantry/replicas}' 2>/dev/null || true)
    [ -n "$coord_count" ] || coord_count=2
    case "$coord_count" in 0|*[!0-9]*) echo "world_update: the recorded coordinator count '$coord_count' (annotation errantry/replicas) is not a positive number; nothing changed" >&2; exit 2 ;; esac
    echo "  the coordinator is at 0: a re-run of the recorded release; it comes back with $coord_count replica(s)"
else
    coord_count="$coord_replicas"
    echo "  coordinator: $coord_count replica(s), stopped and started together"
fi
for d in $hostdeps; do preflight "$d" host; done
preflight deploy/web web
# A drained host is unready for good, and a host MARKED draining (`worldctl hosts`' drain= column,
# not due yet) has its notice running: the release starts either again as a NEW, unmarked host
# (a warning, not a refusal). All warned at once, then ONE pause to stop.
if [ "$dry" -eq 0 ]; then
    warned=0
    for pod in $(k get pods -l "$hostsel" -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true); do
        body=$(k get --raw "/api/v1/namespaces/$ns/pods/$pod:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)
        if [ "$body" = drained ]; then
            echo "world_update: WARNING: $pod is DRAINED: this release stops it and starts it again as a NEW host. To remove it instead, take its name out of var.hosts and apply." >&2
            warned=1
        fi
    done
    if rows=$(k exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl hosts "$VOXEL_WORLD_DB"' 2>/dev/null); then
        marked=$(printf '%s\n' "$rows" | awk '$3 == "live" && $4 == "dirty" && $NF != "drain=-" { sub(/^drain=/, "", $NF); print $2 " (id " $1 ", due in " int($NF / 1000) " s)" }')
        if [ -n "$marked" ]; then
            printf '%s\n' "$marked" | while read -r m; do
                echo "world_update: WARNING: host $m is MARKED draining: this release restarts it as a NEW host with NO mark — its drain is lost (drain it again afterwards, or let it finish first)." >&2
            done
            warned=1
        fi
    else
        echo "world_update: could not read the hosts' drain marks (worldctl hosts in the toolbox); a host marked draining would come back unmarked. Going on." >&2
    fi
    if [ "$warned" -eq 1 ]; then
        echo "world_update: stop here within 10 s (Ctrl-C) to act on the warnings above; nothing has changed yet." >&2
        sleep 10
    fi
fi

step 1 "read the host Deployments' replicas and record the release being replaced"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns get <each host Deployment> -o jsonpath={.spec.replicas}"
    echo "  + refuse when every host Deployment is at 0 and this is not a re-run of the release recorded in ConfigMap $history"
    echo "  + read the images (coordinator, toolbox, each host Deployment, web); unless this re-runs the recorded release, record them in ConfigMap $history:"
    echo "  + kubectl --context $ctx -n $ns create configmap $history --from-literal=previous-server=<coordinator's image> --from-literal=previous-web=<web's image> --from-literal=released-server=$server_ref --from-literal=released-web=$web_ref --from-literal=recorded-at=<now> --dry-run=client -o yaml | kubectl --context $ctx -n $ns apply -f -"
else
    running=0
    for d in $hostdeps; do
        if ! r=$(k get "$d" -o jsonpath='{.spec.replicas}'); then
            echo "world_update: could not read the replicas of $d; nothing changed" >&2
            exit 2
        fi
        case "$r" in ''|*[!0-9]*) echo "world_update: $d's replica count '$r' is not a number; nothing changed" >&2; exit 2 ;; esac
        were="$were ${d#deploy/} $r"
        [ "$r" -eq 0 ] || running=$((running + 1))
    done
    if ! rec_server=$(record_field released-server) || ! rec_web=$(record_field released-web); then
        echo "world_update: cannot read ConfigMap $history; nothing changed" >&2
        exit 2
    fi
    rerun=0
    [ "$rec_server" = "$server_ref" ] && [ "$rec_web" = "$web_ref" ] && rerun=1
    if [ "$running" -eq 0 ] && [ "$rerun" -eq 0 ]; then
        echo "world_update: every host Deployment is at 0 replicas: a world already stopped (and this is not a re-run of" >&2
        echo "world_update: the release recorded in ConfigMap $history). Refusing: start it first (just world-start), or" >&2
        echo "world_update: finish a failed run by re-running it with its two refs. Nothing changed." >&2
        exit 2
    fi
    echo "  host replicas:$were; every host Deployment comes back at 1"

    # image_of <kind/name> <container>
    image_of() { k get "$1" -o "jsonpath={.spec.template.spec.containers[?(@.name==\"$2\")].image}"; }
    if [ "$rerun" -eq 1 ]; then
        echo "  a re-run of the recorded release: ConfigMap $history kept (previous: server $(record_field previous-server || true), web $(record_field previous-web || true))"
    else
        if ! cur_coord=$(image_of deploy/coordinator coordinator) || ! cur_toolbox=$(image_of deploy/toolbox toolbox) ||
            ! cur_web=$(image_of deploy/web web); then
            echo "world_update: cannot read the workloads' images; nothing changed" >&2
            exit 2
        fi
        # Every host Deployment's image; same_hosts: they are all the coordinator's.
        cur_hosts='' same_hosts=1 hosts_new=1
        for d in $hostdeps; do
            if ! img=$(image_of "$d" host); then
                echo "world_update: cannot read the workloads' images; nothing changed" >&2
                exit 2
            fi
            cur_hosts="$cur_hosts ${d#deploy/}=$img"
            [ "$img" = "$cur_coord" ] || same_hosts=0
            [ "$img" = "$server_ref" ] || hosts_new=0
        done
        if [ "$cur_coord" = "$server_ref" ] && [ "$cur_toolbox" = "$server_ref" ] && [ "$hosts_new" -eq 1 ] &&
            [ "$cur_web" = "$web_ref" ]; then
            echo "  every workload already runs these refs: ConfigMap $history kept"
        else
            if [ "$cur_coord" = "$cur_toolbox" ] && [ "$same_hosts" -eq 1 ]; then
                prev_server="$cur_coord" prev_web="$cur_web"
                echo "  replacing: server $prev_server, web $prev_web"
            else
                prev_server='' prev_web=''
                {
                    echo "world_update: WARNING: the server images differ (coordinator $cur_coord, toolbox $cur_toolbox, hosts$cur_hosts)"
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

step 2 "scale every host Deployment to 0 and wait until no host pod is left ($stop_timeout s)"
where='step 2 (scale down / wait for the host pods)'
# Each host pod's log is followed until the pod ends: its leave line is read after it is gone.
if [ "$dry" -eq 1 ]; then
    echo "  + follow each host pod's log until it ends (kubectl --context $ctx -n $ns logs -f <pod>), for its leave line"
else
    follow_dir=$(mktemp -d)
    if ! listing=$(k get pods -l "$hostsel" -o name 2>&1); then
        echo "world_update: cannot list the host pods ($(printf '%s' "$listing" | head -1)); nothing changed" >&2
        exit 2
    fi
    for pod in $(printf '%s\n' "$listing" | sed -n 's#^pod/##p'); do
        k logs -f --tail=20 "$pod" > "$follow_dir/$pod.log" 2>&1 &
        follow_pids+=("$!")
    done
    sleep 1
fi
# shellcheck disable=SC2086 # one word per host Deployment
if ! run scale $hostdeps --replicas=0; then
    echo "world_update: the scale-down failed; check the host Deployments' replicas (kubectl --context $ctx -n $ns get deploy -l $hostsel): one may be at 0 already" >&2
    stopped=1
    exit 1
fi
[ "$dry" -eq 1 ] || stopped=1
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod -l $hostsel exists (timeout $stop_timeout s; a failed listing is retried, never read as empty)"
    echo "  + print each host's leave line; WARN when one left without a cut"
else
    deadline=$((SECONDS + stop_timeout))
    while :; do
        if listing=$(k get pods -l "$hostsel" -o name 2>&1); then
            left=$(printf '%s\n' "$listing" | grep -c '^pod/' || true)
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
        elif grep -qF "stop signal while drained" "$f"; then
            echo "    $pod: drained earlier (it left the world at its drain): stopped; it comes back as a NEW host"
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
# Then every coordinator replica, together: no replica of the old build runs beside one of the
# new (Recreate would do the same at step 3; stopping first keeps a failure there plain). The
# count is recorded as world-stop records it.
where='step 2 (coordinator to 0)'
coord_down=1
if [ "$coord_replicas" != 0 ]; then
    run annotate deploy/coordinator --overwrite "errantry/replicas=$coord_count"
fi
run scale deploy/coordinator --replicas=0
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod -l app.kubernetes.io/component=coordinator exists (timeout $stop_timeout s)"
else
    deadline=$((SECONDS + stop_timeout))
    while :; do
        if listing=$(k get pods -l app.kubernetes.io/component=coordinator -o name 2>&1); then
            left=$(printf '%s\n' "$listing" | grep -c '^pod/' || true)
            [ "$left" -eq 0 ] && break
            what="$left coordinator pod(s) still there"
        else
            what="the pod listing failed ($(printf '%s' "$listing" | head -1))"
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $stop_timeout s: $what; not updating anything" >&2
            exit 1
        fi
        sleep 2
    done
    echo "  the coordinator is down ($coord_count replica(s) to come back)"
fi

step 3 "set the images"
where='step 3 (set image)'
images_touched=1
run set image deploy/coordinator "coordinator=$server_ref"
run set image deploy/toolbox "toolbox=$server_ref"
# Every host Deployment is at 0 (Recreate): this only changes its template, which is what we
# want — the pods created at step 5 come from the new template. None is scaled up before
# all of them are set (a failure here exits with the hosts at 0).
for d in $hostdeps; do
    run set image "$d" "host=$server_ref"
done
run set image deploy/web "web=$web_ref"

step 4 "the coordinator back to $coord_count replica(s), each Ready; the toolbox and web rollouts"
where='step 4 (coordinator up / rollouts)'
run scale deploy/coordinator --replicas="$coord_count"
# Not `rollout status`: every replica's /readyz (200 once its join committed), as world-start
# waits; a pod being deleted is not counted, a failed read keeps waiting.
if [ "$dry" -eq 1 ]; then
    echo "  + wait until $coord_count coordinator pods answer /readyz 200 (timeout $ready_timeout s)"
else
    deadline=$((SECONDS + ready_timeout))
    while :; do
        up=0
        for p in $(k get pods -l app.kubernetes.io/component=coordinator -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers 2>/dev/null | awk '$2 == "<none>" { print $1 }'); do
            k get --raw "/api/v1/namespaces/$ns/pods/$p:9100/proxy/readyz" > /dev/null 2>&1 && up=$((up + 1))
        done
        [ "$up" -ge "$coord_count" ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $ready_timeout s: $up of $coord_count coordinator replicas answer /readyz 200" >&2
            exit 1
        fi
        sleep "${WORLD_UPDATE_POLL:-3}"
    done
    echo "  $coord_count coordinator replica(s) Ready"
fi
coord_down=0
run rollout status deploy/toolbox --timeout=5m
run rollout status deploy/web --timeout=5m

step 5 "scale every host Deployment back to 1"
where='step 5 (scale up)'
# shellcheck disable=SC2086 # one word per host Deployment
run scale $hostdeps --replicas=1
scaled_up=1

step 6 "wait until every host is Ready ($ready_timeout s)"
where='step 6 (hosts Ready)'
# Not `rollout status`: a host waiting for a succession is unready for a while, and a drained one
# for good (none is, right after this start: each pod is a new host). Poll each Deployment's
# readyReplicas; a failed read keeps waiting inside the budget and never counts as ready.
if [ "$dry" -eq 1 ]; then
    echo "  + poll kubectl --context $ctx -n $ns get <each host Deployment> -o jsonpath={.status.readyReplicas} until each is 1 (timeout $ready_timeout s)"
else
    deadline=$((SECONDS + ready_timeout))
    while :; do
        ready=0 notready=''
        for d in $hostdeps; do
            if r=$(k get "$d" -o jsonpath='{.status.readyReplicas}' 2>/dev/null) && [ "${r:-0}" = 1 ]; then
                ready=$((ready + 1))
            else
                notready="$notready ${d#deploy/}"
            fi
        done
        [ -z "$notready" ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $ready_timeout s: $ready hosts Ready; not Ready:$notready" >&2
            exit 1
        fi
        sleep "${WORLD_UPDATE_POLL:-3}"
    done
fi
finished=1
echo "world_update: done (server $server_ref, web $web_ref)"
