#!/usr/bin/env bash
# Stop-update-start release of a world's images (`just orion-world-update`), and its way
# back (`just orion-world-rollback`):
#   world_update.sh <context> <namespace> <server-ref> <web-ref> [--dry-run]
#   world_update.sh <context> <namespace> --rollback [--dry-run]
# Both refs are image references by digest (`<repo>@sha256:…`); a tag is refused
# (exit 2), because a moving `latest` would not roll out reliably.
# The hosts are one Deployment each (`host-<name>`, all selected by
# -l app.kubernetes.io/component=host; strategy Recreate, 1 replica), made and deleted by the
# fleet reconciler (Deployment `fleet`, one replica) from the world's desired_hosts rows. The
# reconciler stops FIRST (step 2), and its pod is gone before the host Deployments are listed
# again and stopped: from then on nothing creates a host Deployment — its own rule (it creates
# nothing while the coordinator is at 0) does not cover the time the hosts leave with the
# coordinator still up. The hosts stop next, all of them, so that none runs the old image
# beside a new coordinator: scaling every host Deployment to 0 makes each host ask for a leave
# cut and exit (bounded by about 90 s), then every coordinator replica stops together (the
# count recorded in the Deployment's annotation errantry/replicas, as world-stop does), every
# workload gets its new image — the coordinator, the reconciler and every host Deployment
# while at 0, where a Recreate Deployment starts nothing — the coordinator comes back at its
# count (every replica's /readyz 200, never `rollout status`), then the reconciler (its /readyz
# 200: a pass that saw the coordinator up, which created any missing host on the coordinator's
# new image and re-templated the stopped ones, keeping their new image — waited for at most the
# ready timeout, then a WARNING and the release goes on: one failing host object never keeps the
# world down), toolbox and web roll out, and every host Deployment (listed again) at 0 comes
# back at 1 replica. A host is never
# started on a mixed set of images: no host Deployment is scaled up before the images of all
# of them are set. A world with no host Deployment is updated too (the host steps do
# nothing). `--dry-run` prints the commands and changes nothing (it reads the list of host
# Deployments and the coordinator's count).
# Before anything is scaled a preflight checks the workloads and their container names
# (coordinator, toolbox, fleet, every host Deployment's host, web); a world whose
# coordinator is at 0 replicas is refused (a world stopped by world-stop: hosts started
# without a coordinator cannot leave through a cut — start the world first), unless this run
# repeats the release recorded in release-history (a failed run that stopped the coordinator:
# it comes back at the recorded count). A drained host (/readyz `drained`) and a host MARKED
# draining (`worldctl hosts` drain=, not due yet) are WARNED about, all at once, then one 10 s
# pause before anything changes: the release stops each and starts it again as a NEW host,
# which the coordinator marks again once the name's desired row is draining (a new notice).
#
# Release history: before any image is set, the refs being replaced are recorded in the
# ConfigMap `release-history` of the world's namespace (it lives with the world, not on
# the operator's machine): previous-server, previous-web (what ran), released-server,
# released-web (what this run sets), recorded-at (UTC). A re-run of the recorded release
# (after a failure) keeps the record; a re-run of a release that FINISHED (every workload on
# these refs, the coordinator up, the reconciler at 1, every host Deployment at 1) changes
# nothing and stops nothing. Server images that differ between coordinator, toolbox, fleet
# and any host Deployment (not a re-run) leave no usable record (the previous release is not
# known) and a later --rollback refuses. `--rollback` reads previous-server / previous-web and
# runs this same path to them; it refuses (exit 2, nothing changed) when no previous release
# is recorded or a recorded ref is not a digest. A rollback is a release too: it records what
# it replaced, so a second --rollback goes forward again. When CONTENT_SERVER /
# CONTENT_WEB are set (by `just deploy --game`), the same record carries their
# mmo.content-sha256 values as released-content-server / released-content-web,
# so the next --game can skip a release whose content did not change; a rollback
# clears those keys.
# The ConfigMap `release-in-progress` is created just before the first
# scale-down and deleted once every host is Ready. If this script is interrupted,
# the marker stays: `just deploy --infra` refuses to apply `world` while it exists.
#
# Leave outcome: each host pod's log is followed from before the scale-down until the
# pod ends; once they are gone the script prints each host's leave line, and WARNS (it
# goes on) when a host left without a committed cut — the next start raises the world
# generation (the world rolls back to its last committed cut) — or when no leave line
# was seen.
#
# If the script stops after the reconciler was scaled down, it says so and how to go on:
# a failure before any image changed offers bringing back the coordinator, then the
# reconciler, then the host Deployments (or `just world-start`, which does the same), or a
# re-run; once step 3 began the images are partly replaced and only the re-run with the same
# two refs is offered. A failed run leaves the reconciler at 0 until the re-run (or
# world-start) brings it back. A world whose host Deployments are ALL at 0 replicas is refused
# (a world already stopped) unless this run repeats the release recorded in ConfigMap
# release-history (the re-run of a failed run): a re-run always ends with every host
# Deployment at 1, so it cannot "finish" with the world down.
# Env: WORLD_UPDATE_STOP_TIMEOUT (seconds to wait for the reconciler's, the host and the
# coordinator pods to go, default 150 — a leave cut is bounded by ~90 s),
# WORLD_UPDATE_READY_TIMEOUT (seconds to wait for every coordinator replica, the reconciler
# and every host Ready, default 300), CONTENT_SERVER / CONTENT_WEB (content hashes recorded
# in release-history), WORLD_UPDATE_ALLOW_TAG=1 (the k3d twin uses local tags, not ECR digests).
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
marker=release-in-progress
content_server="${CONTENT_SERVER:-}"
content_web="${CONTENT_WEB:-}"
allow_tag="${WORLD_UPDATE_ALLOW_TAG:-0}"
hostsel='app.kubernetes.io/component=host'

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
step() { echo "world_update: [$1/6] $2"; }
# run <kubectl args…>: prints the command; runs it unless --dry-run.
run() {
    echo "  + kubectl --context $ctx -n $ns $*"
    [ "$dry" -eq 1 ] || k "$@"
}
# The twin's images are local tags (`mmo/<name>:twin`), not ECR digests.
is_digest() {
    [ "$allow_tag" = 1 ] && return 0
    case "$1" in *@sha256:*) return 0 ;; *) return 1 ;; esac
}
# record_field <key>: one key of the release history ('' when there is none); a failed
# read fails (nothing changed yet).
record_field() {
    k get configmap "$history" --ignore-not-found -o "jsonpath={.data.$1}"
}

# Recovery: armed when the hosts are about to be scaled down, disarmed at the end.
# hostdeps: the host Deployments as `deploy/host-<name>` words; were: their replicas before.
hostdeps='' were='' stopped=0 images_touched=0 images_set=0 scaled_up=0 finished=0 where='before step 2'
# coord_down: the coordinator was scaled to 0 (step 2) and is not back yet (step 4).
coord_down=0 coord_replicas='' coord_count=''
# fleet_down: the fleet reconciler was scaled to 0 (step 2) and is not back yet (step 4).
fleet_down=0
follow_dir='' follow_pids=()
# fleet_line: how to bring the reconciler back, when it is down (after the coordinator is Ready).
fleet_line() {
    [ "$fleet_down" -eq 1 ] || return 0
    echo "  kubectl --context $ctx -n $ns scale deploy/fleet --replicas=1   (after the coordinator is Ready; then wait for its /readyz 200)"
}
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
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$images_set" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments are scaled to 0 (were:$were); the world is DOWN. Every image is set"
            echo "world_update: (server $server_ref, web $web_ref): nothing runs the old build. Go on with either:"
            echo "  just world_ctx=$ctx world_ns=$ns world-start   (coordinator, then the fleet reconciler, then every host Deployment)"
            echo "or: $0 $ctx $ns $server_ref $web_ref"
            echo "world_update: if the coordinator does not come up, look first: kubectl --context $ctx -n $ns logs deploy/coordinator"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ] && [ "$images_touched" -eq 1 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments are scaled to 0 (were:$were); the world is DOWN, and the images are PARTLY"
            echo "world_update: replaced (some workloads may run the new build, some the old): do NOT scale the hosts, the"
            echo "world_update: coordinator or the fleet reconciler by hand. Go on by re-running with the same two refs:"
            echo "  $0 $ctx $ns $server_ref $web_ref"
        } >&2
    elif [ "$stopped" -eq 1 ] && [ "$finished" -eq 0 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the host Deployments are scaled to 0 (were:$were); the world is DOWN. No image was changed."
            echo "world_update: go on with either (in this order; \`just world-start\` does the same):"
            if [ "$coord_down" -eq 1 ]; then
                echo "  kubectl --context $ctx -n $ns scale deploy/coordinator --replicas=$coord_count   (first; then wait until every replica is Ready)"
            fi
            fleet_line
            [ -z "$hostdeps" ] || echo "  kubectl --context $ctx -n $ns scale $hostdeps --replicas=1"
            echo "or: $0 $ctx $ns $server_ref $web_ref"
        } >&2
    elif [ "$fleet_down" -eq 1 ] && [ "$finished" -eq 0 ]; then
        {
            echo "world_update: STOPPED at: $where (exit $rc)."
            echo "world_update: the fleet reconciler is at 0; no host was stopped and no image was changed (the world runs,"
            echo "world_update: but no host Deployment is created or deleted). Go on with either:"
            fleet_line
            echo "or: $0 $ctx $ns $server_ref $web_ref"
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
# deploy_words <names>: `deploy/<name>` words, one space apart.
deploy_words() {
    local d out=''
    for d in $1; do out="$out deploy/$d"; done
    printf '%s' "${out# }"
}
hostdeps=$(deploy_words "$hostnames")
# A world at zero hosts (none wanted yet, or all removed) is updated too: the host steps do nothing.
echo "  host Deployments: ${hostdeps:-(none)}"
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
preflight deploy/fleet fleet
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
# A drained host is unready until the coordinator removes its desired row, and a host MARKED draining
# (`worldctl hosts`' drain= column, not due yet) has its notice running: the release stops it now. The
# coordinator drains such a name's desired row (rules 2 / 2b) and removes it once nothing of the name
# runs (rule 3); the reconciler then deletes its Deployment — usually before step 5 lists the hosts
# again; one still listed runs as a NEW host for seconds (marked at once, deleted with its row). A
# warning, not a refusal. All warned at once, then ONE pause to stop.
if [ "$dry" -eq 0 ]; then
    warned=0
    for pod in $(k get pods -l "$hostsel" -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true); do
        body=$(k get --raw "/api/v1/namespaces/$ns/pods/$pod:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)
        if [ "$body" = drained ]; then
            echo "world_update: WARNING: $pod is DRAINED and its desired row not removed yet: the coordinator removes that row and the reconciler deletes its Deployment — usually before step 5; else this release starts it as a NEW host that runs for seconds (marked at once, deleted with its row). To avoid that, wait until the coordinator removed it (just world-hosts) and run the release then." >&2
            warned=1
        fi
    done
    if rows=$(k exec deploy/toolbox -c toolbox -- sh -c 'exec worldctl hosts "$VOXEL_WORLD_DB"' 2>/dev/null); then
        marked=$(printf '%s\n' "$rows" | awk '$3 == "live" && $4 == "dirty" && $NF != "drain=-" { sub(/^drain=/, "", $NF); print $2 " (id " $1 ", due in " int($NF / 1000) " s)" }')
        if [ -n "$marked" ]; then
            printf '%s\n' "$marked" | while read -r m; do
                echo "world_update: WARNING: host $m is MARKED draining: this release stops it now, before its deadline (its players are cut off now); the coordinator then drains and removes its desired row and the reconciler deletes its Deployment (or let the drain finish first)." >&2
            done
            warned=1
        fi
    else
        echo "world_update: could not read the hosts' drain marks (worldctl hosts in the toolbox); a host marked draining comes back unmarked until the coordinator marks it again. Going on." >&2
    fi
    if [ "$warned" -eq 1 ]; then
        echo "world_update: stop here within 10 s (Ctrl-C) to act on the warnings above; nothing has changed yet." >&2
        sleep 10
    fi
fi

step 1 "read the host Deployments' replicas and record the release being replaced"
if [ "$dry" -eq 1 ]; then
    echo "  + kubectl --context $ctx -n $ns get <each host Deployment> -o jsonpath={.spec.replicas}"
    echo "  + refuse when there are host Deployments, every one at 0, and this is not a re-run of the release recorded in ConfigMap $history"
    echo "  + read the images (coordinator, toolbox, fleet, each host Deployment, web); a re-run of the recorded release that finished (every workload on these refs, the coordinator up, fleet at 1, every host Deployment at 1) stops here, changing nothing"
    echo "  + unless this re-runs the recorded release, record them in ConfigMap $history:"
    echo "  + kubectl --context $ctx -n $ns create configmap $history --from-literal=previous-server=<coordinator's image> --from-literal=previous-web=<web's image> --from-literal=released-server=$server_ref --from-literal=released-web=$web_ref --from-literal=released-content-server=$content_server --from-literal=released-content-web=$content_web --from-literal=recorded-at=<now> --dry-run=client -o yaml | kubectl --context $ctx -n $ns apply -f -"
else
    # running: host Deployments above 0; all_up: every one at exactly 1.
    running=0 all_up=1
    for d in $hostdeps; do
        if ! r=$(k get "$d" -o jsonpath='{.spec.replicas}'); then
            echo "world_update: could not read the replicas of $d; nothing changed" >&2
            exit 2
        fi
        case "$r" in ''|*[!0-9]*) echo "world_update: $d's replica count '$r' is not a number; nothing changed" >&2; exit 2 ;; esac
        were="$were ${d#deploy/} $r"
        [ "$r" -eq 0 ] || running=$((running + 1))
        [ "$r" -eq 1 ] || all_up=0
    done
    if ! rec_server=$(record_field released-server) || ! rec_web=$(record_field released-web); then
        echo "world_update: cannot read ConfigMap $history; nothing changed" >&2
        exit 2
    fi
    rerun=0
    [ "$rec_server" = "$server_ref" ] && [ "$rec_web" = "$web_ref" ] && rerun=1
    # A world at zero hosts has no host Deployment to tell a stopped world by: the coordinator's
    # count (refused at 0 above, unless a re-run) does.
    if [ -n "$hostdeps" ] && [ "$running" -eq 0 ] && [ "$rerun" -eq 0 ]; then
        echo "world_update: every host Deployment is at 0 replicas: a world already stopped (and this is not a re-run of" >&2
        echo "world_update: the release recorded in ConfigMap $history). Refusing: start it first (just world-start), or" >&2
        echo "world_update: finish a failed run by re-running it with its two refs. Nothing changed." >&2
        exit 2
    fi
    if [ -n "$hostdeps" ]; then
        echo "  host replicas:$were; every host Deployment comes back at 1"
    else
        echo "  no host Deployment: the host steps do nothing (the reconciler creates the wanted hosts at step 4, on the new image)"
    fi

    # image_of <kind/name> <container>
    image_of() { k get "$1" -o "jsonpath={.spec.template.spec.containers[?(@.name==\"$2\")].image}"; }
    if ! cur_coord=$(image_of deploy/coordinator coordinator) || ! cur_toolbox=$(image_of deploy/toolbox toolbox) ||
        ! cur_fleet=$(image_of deploy/fleet fleet) || ! cur_web=$(image_of deploy/web web); then
        echo "world_update: cannot read the workloads' images; nothing changed" >&2
        exit 2
    fi
    # Every host Deployment's image; same_hosts: they are all the coordinator's; hosts_new: all the new ref.
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
    on_refs=0
    if [ "$cur_coord" = "$server_ref" ] && [ "$cur_toolbox" = "$server_ref" ] && [ "$cur_fleet" = "$server_ref" ] &&
        [ "$hosts_new" -eq 1 ] && [ "$cur_web" = "$web_ref" ]; then
        on_refs=1
    fi
    if [ "$rerun" -eq 1 ]; then
        # A re-run of a release that finished: every workload on these refs and everything up. Nothing
        # to stop or start (a failed run never ends like this: it leaves the reconciler, the
        # coordinator or a host Deployment at 0).
        if ! fleet_replicas=$(k get deploy/fleet -o jsonpath='{.spec.replicas}'); then
            echo "world_update: cannot read the fleet reconciler's replicas; nothing changed" >&2
            exit 2
        fi
        if [ "$on_refs" -eq 1 ] && [ "$coord_replicas" != 0 ] && [ "$fleet_replicas" = 1 ] && [ "$all_up" -eq 1 ]; then
            echo "  a re-run of the recorded release, which finished: every workload runs these refs, the coordinator ($coord_replicas),"
            echo "  the fleet reconciler and every host Deployment are up. Nothing to do; ConfigMap $history kept. (Readiness:"
            echo "  kubectl --context $ctx -n $ns get deploy)"
            finished=1
            echo "world_update: nothing changed (server $server_ref, web $web_ref)"
            exit 0
        fi
        echo "  a re-run of the recorded release: ConfigMap $history kept (previous: server $(record_field previous-server || true), web $(record_field previous-web || true))"
    elif [ "$on_refs" -eq 1 ]; then
        echo "  every workload already runs these refs: ConfigMap $history kept"
    else
        if [ "$cur_coord" = "$cur_toolbox" ] && [ "$cur_coord" = "$cur_fleet" ] && [ "$same_hosts" -eq 1 ]; then
            prev_server="$cur_coord" prev_web="$cur_web"
            echo "  replacing: server $prev_server, web $prev_web"
        else
            prev_server='' prev_web=''
            {
                echo "world_update: WARNING: the server images differ (coordinator $cur_coord, toolbox $cur_toolbox, fleet $cur_fleet, hosts$cur_hosts)"
                echo "world_update: and this is not a re-run of the recorded release: the previous release is not known."
                echo "world_update: ConfigMap $history is written without one; --rollback will refuse until the next release."
            } >&2
        fi
        # The orchestrator's content hashes ride along in the same record.
        content_args=()
        [ -z "$content_server" ] || content_args+=(--from-literal=released-content-server="$content_server")
        [ -z "$content_web" ] || content_args+=(--from-literal=released-content-web="$content_web")
        if ! k create configmap "$history" \
            --from-literal=previous-server="$prev_server" --from-literal=previous-web="$prev_web" \
            --from-literal=released-server="$server_ref" --from-literal=released-web="$web_ref" \
            "${content_args[@]}" \
            --from-literal=recorded-at="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            --dry-run=client -o yaml | k apply -f - > /dev/null; then
            echo "world_update: could not write ConfigMap $history; nothing changed" >&2
            exit 2
        fi
        echo "  recorded in ConfigMap $history (content: server ${content_server:-none}, web ${content_web:-none})"
    fi
    if [ "$rollback" -eq 1 ] && [ "$dry" -eq 0 ]; then
        # A rollback rewrites the released refs but does not re-hash the build it
        # puts back: clear the keys so the next --game releases it again.
        k patch configmap "$history" --type=merge \
            -p '{"data":{"released-content-server":null,"released-content-web":null}}' > /dev/null
    fi
fi

step 2 "the fleet reconciler to 0, then every host Deployment and the coordinator; wait until each one's pods are gone ($stop_timeout s each)"
# wait_no_pods <label selector> <what>: no pod matches (timeout $stop_timeout s; a failed listing is
# retried, never read as empty); exits 1 on the timeout.
wait_no_pods() {
    local deadline=$((SECONDS + stop_timeout)) listing left what
    while :; do
        if listing=$(k get pods -l "$1" -o name 2>&1); then
            left=$(printf '%s\n' "$listing" | grep -c '^pod/' || true)
            [ "$left" -eq 0 ] && return 0
            what="$left $2 pod(s) still there"
        else
            what="the pod listing failed ($(printf '%s' "$listing" | head -1))"
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $stop_timeout s: $what; not updating anything" >&2
            exit 1
        fi
        sleep 2
    done
}
# The reconciler first, and gone: from here nothing creates a host Deployment (one created now
# would start at 1 replica on the old image beside the stop). Nothing recorded: its count is 1.
# Marker for --infra's world guard: created before the first scale-down, deleted
# only after every host is Ready. An interrupted run leaves it in place.
echo "  + kubectl --context $ctx -n $ns create configmap $marker --from-literal=started=<now> --dry-run=client -o yaml | kubectl --context $ctx -n $ns apply -f -"
if [ "$dry" -eq 0 ]; then
    if ! k create configmap "$marker" --from-literal=started="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --dry-run=client -o yaml | k apply -f - > /dev/null; then
        echo "world_update: could not create ConfigMap $marker; nothing changed" >&2
        exit 2
    fi
fi

where='step 2 (fleet reconciler to 0)'
fleet_down=1
run scale deploy/fleet --replicas=0
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod -l app.kubernetes.io/component=fleet exists (timeout $stop_timeout s)"
    echo "  + list the host Deployments again (the reconciler may have created or deleted one since the preflight)"
else
    wait_no_pods app.kubernetes.io/component=fleet "fleet reconciler"
    echo "  the fleet reconciler is down"
    where='step 2 (list the host Deployments)'
    if ! listing=$(k get deploy -l "$hostsel" -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}'); then
        echo "world_update: cannot list the host Deployments; no host was stopped, no image changed" >&2
        exit 1
    fi
    relisted=$(printf '%s\n' "$listing" | grep . || true)
    if [ "$relisted" != "$hostnames" ]; then
        hostdeps=$(deploy_words "$relisted")
        echo "  the host Deployments changed since the preflight (the reconciler): now ${hostdeps:-(none)}"
    fi
fi
where='step 2 (scale down / wait for the host pods)'
# Each host pod's log is followed until the pod ends: its leave line is read after it is gone.
if [ "$dry" -eq 1 ]; then
    echo "  + follow each host pod's log until it ends (kubectl --context $ctx -n $ns logs -f <pod>), for its leave line"
else
    follow_dir=$(mktemp -d)
    if ! listing=$(k get pods -l "$hostsel" -o name 2>&1); then
        echo "world_update: cannot list the host pods ($(printf '%s' "$listing" | head -1)); no host was stopped, no image changed" >&2
        exit 1
    fi
    for pod in $(printf '%s\n' "$listing" | sed -n 's#^pod/##p'); do
        k logs -f --tail=20 "$pod" > "$follow_dir/$pod.log" 2>&1 &
        follow_pids+=("$!")
    done
    sleep 1
fi
if [ -n "$hostdeps" ]; then
    # shellcheck disable=SC2086 # one word per host Deployment
    if ! run scale $hostdeps --replicas=0; then
        echo "world_update: the scale-down failed; check the host Deployments' replicas (kubectl --context $ctx -n $ns get deploy -l $hostsel): one may be at 0 already" >&2
        stopped=1
        exit 1
    fi
else
    echo "  no host Deployment to scale down"
fi
[ "$dry" -eq 1 ] || stopped=1
if [ "$dry" -eq 1 ]; then
    echo "  + wait until no pod -l $hostsel exists (timeout $stop_timeout s; a failed listing is retried, never read as empty)"
    echo "  + print each host's leave line; WARN when one left without a cut"
else
    wait_no_pods "$hostsel" host
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
    wait_no_pods app.kubernetes.io/component=coordinator coordinator
    echo "  the coordinator is down ($coord_count replica(s) to come back)"
fi

step 3 "set the images"
where='step 3 (set image)'
images_touched=1
run set image deploy/coordinator "coordinator=$server_ref"
run set image deploy/toolbox "toolbox=$server_ref"
# The reconciler is at 0 (Recreate): its pod at step 4 is of the new build.
run set image deploy/fleet "fleet=$server_ref"
# Every host Deployment is at 0 (Recreate): this only changes its template, which is what we
# want — the pods created at step 5 come from the new template (a re-template by the reconciler
# keeps the image set here). None is scaled up before all of them are set (a failure here exits
# with the hosts at 0).
for d in $hostdeps; do
    run set image "$d" "host=$server_ref"
done
run set image deploy/web "web=$web_ref"
images_set=1

step 4 "the coordinator back to $coord_count replica(s), each Ready; then the fleet reconciler, Ready; the toolbox and web rollouts"
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
# The reconciler after the coordinator (it creates nothing while the coordinator is at 0), before
# the hosts: its /readyz 200 means a pass without a failure that saw the coordinator up — every
# wanted host without a Deployment got one (at 1 replica, on the coordinator's image: the new one),
# every stopped host Deployment carries the current template (its image kept: the new one). The
# wait is bounded and never keeps the world down (final review, B-I1): one host object that keeps
# failing holds /readyz at 503 for good, so after the timeout the release goes on with a warning
# and starts the host Deployments that exist (their images are the new ones, set at step 3).
where='step 4 (fleet reconciler up)'
run scale deploy/fleet --replicas=1
if [ "$dry" -eq 1 ]; then
    echo "  + wait until the fleet pod answers /readyz 200 (timeout $ready_timeout s)"
else
    deadline=$((SECONDS + ready_timeout))
    while :; do
        said='no pod'
        for p in $(k get pods -l app.kubernetes.io/component=fleet -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers 2>/dev/null | awk '$2 == "<none>" { print $1 }'); do
            if k get --raw "/api/v1/namespaces/$ns/pods/$p:9100/proxy/readyz" > /dev/null 2>&1; then said=ok; break; fi
            said="$p: $(k get --raw "/api/v1/namespaces/$ns/pods/$p:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)"
        done
        [ "$said" = ok ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: WARNING: after $ready_timeout s the fleet reconciler's /readyz is not 200 (${said:-no answer}): no clean pass" >&2
            echo "world_update: yet — a wanted host may have no Deployment, a stopped one may keep an older pod template (its image is" >&2
            echo "world_update: the new one). Going on with the host Deployments that exist. Look: just world_ctx=$ctx world_ns=$ns world-status" >&2
            echo "world_update: (the fleet pod: desired vs host Deployments, errors) and world-logs fleet (which object fails, and why)." >&2
            said=timeout
            break
        fi
        sleep "${WORLD_UPDATE_POLL:-3}"
    done
    [ "$said" = ok ] && echo "  the fleet reconciler is Ready"
fi
fleet_down=0
where='step 4 (toolbox / web rollouts)'
run rollout status deploy/toolbox --timeout=5m
run rollout status deploy/web --timeout=5m

step 5 "scale every host Deployment back to 1"
where='step 5 (scale up)'
# Listed again: the reconciler may have created a host (at 1 already) or deleted one whose desired
# row was removed since step 2. Every one of them carries the new image.
if [ "$dry" -eq 1 ]; then
    echo "  + list the host Deployments again (-l $hostsel)"
else
    if ! listing=$(k get deploy -l "$hostsel" -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}'); then
        echo "world_update: cannot list the host Deployments" >&2
        exit 1
    fi
    relisted=$(printf '%s\n' "$listing" | grep . || true)
    hostdeps=$(deploy_words "$relisted")
fi
if [ -n "$hostdeps" ]; then
    # One at a time: one the reconciler deleted since the listing (its desired row removed) answers
    # NotFound, which is not an error here — step 6 counts it as deleted.
    for d in $hostdeps; do
        run scale "$d" --replicas=1 || echo "world_update: could not scale $d (deleted meanwhile?): step 6 classifies it" >&2
    done
else
    echo "  no host Deployment (the coordinator adds the fleet's minimum, the reconciler creates them)"
fi
scaled_up=1

step 6 "wait until every host is Ready ($ready_timeout s)"
where='step 6 (hosts Ready)'
# Not `rollout status`: a host waiting for a succession is unready for a while. Poll each
# Deployment's readyReplicas; a failed read keeps waiting inside the budget and never counts as
# ready. As world-start: a host whose pod says `drained` is not waited for (the restart of a name
# whose desired row is draining: the coordinator marks the new host at once and removes it once it
# left), nor is a Deployment the reconciler deleted meanwhile (its desired row removed).
if [ "$dry" -eq 1 ]; then
    echo "  + poll kubectl --context $ctx -n $ns get <each host Deployment> -o jsonpath={.status.readyReplicas} until each is 1, drained or deleted (timeout $ready_timeout s)"
else
    deadline=$((SECONDS + ready_timeout))
    while :; do
        ready=0 notready='' drained='' deleted=''
        for d in $hostdeps; do
            if r=$(k get "$d" -o jsonpath='{.status.readyReplicas}' 2>/dev/null) && [ "${r:-0}" = 1 ]; then
                ready=$((ready + 1))
                continue
            fi
            if [ -z "$(k get --ignore-not-found "$d" -o name 2>/dev/null || echo failed)" ]; then
                deleted="$deleted ${d#deploy/host-}"
                continue
            fi
            is_drained=0
            for p in $(k get pods -l "$hostsel,errantry/host=${d#deploy/host-}" -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || true); do
                body=$(k get --raw "/api/v1/namespaces/$ns/pods/$p:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true)
                [ "$body" = drained ] && is_drained=1
            done
            if [ "$is_drained" = 1 ]; then drained="$drained ${d#deploy/host-}"; else notready="$notready ${d#deploy/}"; fi
        done
        [ -z "$notready" ] && break
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "world_update: after $ready_timeout s: $ready hosts Ready; not Ready:$notready" >&2
            exit 1
        fi
        sleep "${WORLD_UPDATE_POLL:-3}"
    done
    [ -z "$drained" ] || echo "  drained (the coordinator removes them):$drained"
    [ -z "$deleted" ] || echo "  deleted by the fleet reconciler while starting (their desired rows removed):$deleted"
fi
echo "  + kubectl --context $ctx -n $ns delete configmap $marker --ignore-not-found"
if [ "$dry" -eq 0 ]; then
    k delete configmap "$marker" --ignore-not-found > /dev/null
fi
finished=1
echo "world_update: done (server $server_ref, web $web_ref)"
