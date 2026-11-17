#!/usr/bin/env bash
# The k3d twin's integration check (`just twin-check`; plans
# docs/superpowers/plans/2026-11-15-r5-k8s.md, Task 13, and
# docs/superpowers/plans/2026-11-15-elastic-1-hosts-as-deployments.md, Task 12). Runs
# against a FRESH `just twin-apply` (it forges the world): the modules, images and server
# code of the world on Kubernetes, end to end, with no client. The hosts are the
# Deployments host-a and host-b (pods selected by app.kubernetes.io/component=host and
# errantry/host=<name>; a pod's name is random, a host's id is issued at registration).
#   1. pg Ready, login / toolbox / web rolled out; the coordinator and both hosts run,
#      unready, /readyz "forge" (nothing forged by a pod). 1b: a host waiting for the
#      forge stops on a plain SIGTERM at once with exit code 0 (PID 1's signal path).
#   2. worldctl forge from the toolbox; the coordinator and both hosts become Ready.
#      2b: every host has a PodDisruptionBudget that allows no disruption (0); an
#      eviction dry run of a host pod is recorded (refused or not: not a check).
#   3. The world registered in the twin's own login service (world-register's kubectl
#      command: the link is the coordinator's, no --quic / --wt).
#   4. Coordinator /metrics: 2 hosts live, a committed world cut; `worldctl hosts`: one
#      live row per host name.
#   5. host-b's pod force-killed: a new pod is Ready, under a NEW id; the old id is ended
#      (landless), no succession, generation unchanged; host-a's pod is untouched, cuts commit again.
#   6. host-a scaled to 0 (SIGTERM): it leaves with a committed leave cut, no succession;
#      scaled to 1: a new id; the killed host-b's old row is gone (pruned at this registration).
#   7. The coordinator restarted: both hosts' links come up again, cuts commit.
#   8. The web Ingress answers through Traefik; its /login proxy reaches login's key set
#      and API, and login's /metrics and /healthz are NOT published (404).
#   9. world-drain b 5 (the recipe): host-b leaves at the deadline and stays up drained
#      (/readyz "drained", /livez 200), not restarted, no new `hosts` row; host-a runs on;
#      world-status names the drained host and world-start does not wait for it; then
#      `rollout restart deployment/host-b` brings it back Ready under a new id.
#  10. world-stop then world-start (the recipes): the hosts leave with committed leave
#      cuts, the coordinator stops after them, everything comes back Ready.
#  11. Records (asserts nothing): each pod's memory after the forge and five minutes
#      after the world-start, and any pod log line about a file it could not write.
# Every wait has a timeout and says what it waited for. Exit 0 = pass, 1 = a check
# failed (pods, events and logs are dumped and saved under target/twin/check-<time>/).
# Env: TWIN_CONTEXT (k3d-mmo), TWIN_NAMESPACE (world-twin).
set -euo pipefail
cd "$(dirname "$0")/.."

ctx="${TWIN_CONTEXT:-k3d-mmo}"
ns="${TWIN_NAMESPACE:-world-twin}"
out="target/twin/check-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"
started=$SECONDS

k() { kubectl --context "$ctx" -n "$ns" "$@"; }
kc() { kubectl --context "$ctx" "$@"; }
# The world recipes of the justfile, aimed at the twin.
wjust() { just world_ctx="$ctx" world_ns="$ns" "$@"; }

bg_pids=()
cleanup() {
    for p in "${bg_pids[@]}"; do kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

say() { echo "  [$((SECONDS - started))s] $*"; }
step() {
    echo
    echo "== $*"
}

dump() {
    echo
    echo "---- pods"
    k get pods -o wide || true
    echo "---- hosts rows"
    wjust world-hosts 2>&1 | tail -20 || true
    echo "---- events (last 30)"
    k get events --sort-by=.lastTimestamp 2>/dev/null | tail -30 || true
    for pod in $(k get pods -o name 2>/dev/null | sed 's#^pod/##'); do
        k logs "$pod" --all-containers > "$out/$pod.log" 2>&1 || true
        k logs "$pod" --all-containers --previous > "$out/$pod.previous.log" 2>/dev/null || rm -f "$out/$pod.previous.log"
        echo "---- $pod (last 25 log lines; whole log in $out/$pod.log)"
        tail -25 "$out/$pod.log"
    done
}

fail() {
    echo "FAIL: $*" >&2
    dump
    echo "twin-check: FAILED after $((SECONDS - started))s: $*" >&2
    exit 1
}
# A command failing outside any check is a failure too, never a silent exit.
set -E
trap 'trap - ERR; fail "unexpected error (exit $?) at line $LINENO: $BASH_COMMAND"' ERR

# wait_until <seconds> <what> <command...>: runs the command every 2 s until it succeeds.
wait_until() {
    local timeout="$1" what="$2" waited_from=$SECONDS
    shift 2
    say "waiting up to ${timeout}s for: $what"
    until "$@"; do
        if [ $((SECONDS - waited_from)) -ge "$timeout" ]; then
            fail "after ${timeout}s, still not: $what"
        fi
        sleep 2
    done
    say "ok after $((SECONDS - waited_from))s: $what"
}

# --- pods ------------------------------------------------------------------------

pod_field() { k get pod "$1" -o jsonpath="$2" 2>/dev/null; }
pod_uid() { pod_field "$1" '{.metadata.uid}'; }
pod_ready() { [ "$(pod_field "$1" '{.status.conditions[?(@.type=="Ready")].status}')" = True ]; }
pod_running() { [ "$(pod_field "$1" '{.status.phase}')" = Running ] && [ -z "$(pod_field "$1" '{.metadata.deletionTimestamp}')" ]; }
pod_restarts() { pod_field "$1" '{.status.containerStatuses[0].restartCount}'; }
# live_pod <label selector>: the pod of that selector that is not being deleted ('' if none).
live_pod() {
    k get pods -l "$1" -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers 2>/dev/null |
        awk '$2 == "<none>" { print $1; exit }' || true
}
coordinator_pod() { live_pod app.kubernetes.io/component=coordinator; }
# host_pod <name>: host <name>'s pod (Deployment host-<name>).
host_pod() { live_pod "app.kubernetes.io/component=host,errantry/host=$1"; }
host_ready() {
    local p
    p=$(host_pod "$1")
    [ -n "$p" ] && pod_ready "$p"
}
all_ready() {
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] && pod_ready "$c" && host_ready a && host_ready b
}
# host_replaced_and_ready <name> <old uid>: host <name> has a new pod, Ready.
host_replaced_and_ready() {
    local p uid
    p=$(host_pod "$1")
    [ -n "$p" ] || return 1
    uid=$(pod_uid "$p") || return 1
    [ -n "$uid" ] && [ "$uid" != "$2" ] && pod_ready "$p"
}
# gone <pod> <uid>: no pod of that name with that uid.
gone() { [ "$(pod_uid "$1" || true)" != "$2" ]; }
# no_pods <label selector>
no_pods() { [ -z "$(k get pods -l "$1" -o name 2>/dev/null || echo failed)" ]; }

# --- health port (through the API server's pod proxy) ---------------------------

proxy() { k get --raw "/api/v1/namespaces/$ns/pods/$1:9100/proxy/$2"; }
# The /readyz body, 200 or not: kubectl replaces a 503's body with a generic error, but logs
# the body itself at -v=8 ("Response Body: forge").
readyz() {
    k get --raw "/api/v1/namespaces/$ns/pods/$1:9100/proxy/readyz" -v=8 2>&1 | sed -n 's/.*Response Body: //p' | tail -1 || true
}
# metric <pod> <name> [<label>=<value>]...: the first sample of that name carrying every
# given label; fails (prints nothing) when the pod does not answer or has no such sample.
metric() {
    local pod="$1" name="$2" text value
    shift 2
    text=$(proxy "$pod" metrics 2>/dev/null) || return 1
    value=$(printf '%s\n' "$text" | awk -v name="$name" -v want="$*" '
        BEGIN { n = split(want, w, " ") }
        /^#/ { next }
        {
            split($1, a, "{")
            if (a[1] != name) next
            ok = 1
            for (i = 1; i <= n; i++) {
                split(w[i], kv, "=")
                if (index($1, kv[1] "=\"" kv[2] "\"") == 0) ok = 0
            }
            if (ok) { print $NF; exit }
        }')
    [ -n "$value" ] || return 1
    printf '%s\n' "$value"
}
coord_metric() {
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] || return 1
    metric "$c" "$@"
}
committed() { coord_metric mmo_cuts_decided_total outcome=committed; }
# committed_above <n>: the coordinator's committed cut count is above n.
committed_above() {
    local v
    v=$(committed) || return 1
    [ "$v" -gt "$1" ]
}
waiting_forge() { [ "$(metric "$1" mmo_waiting why=forge)" = 1 ]; }

# --- the hosts table (worldctl hosts, through the world-hosts recipe) ----------------

# rows: `worldctl hosts`' lines (`<id> <name> <live|dead> <clean|dirty> land=… seen=… drain=…`).
rows() { wjust world-hosts 2>/dev/null | grep -E '^[0-9]+ ' || true; }
# live_id <name>: the id of the one live dirty row of that name ('' unless exactly one).
live_id() {
    local ids
    ids=$(rows | awk -v n="$1" '$2 == n && $3 == "live" && $4 == "dirty" { print $1 }')
    [ "$(printf '%s' "$ids" | grep -c .)" = 1 ] && printf '%s\n' "$ids"
    return 0
}
# row_state <id>: live / dead, '' when there is no such row.
row_state() { rows | awk -v i="$1" '$1 == i { print $3 }'; }
# row_land <id>: the super-regions the row owns (`land=N`), '' when there is no such row.
row_land() { rows | awk -v i="$1" '$1 == i { for (f = 5; f <= NF; f++) if ($f ~ /^land=/) { sub(/^land=/, "", $f); print $f } }'; }
# has_new_live_id <name> <ids…>: host <name> has a live dirty row whose id is none of the given.
has_new_live_id() {
    local name="$1" id
    shift
    id=$(live_id "$name")
    [ -n "$id" ] || return 1
    case " $* " in *" $id "*) return 1 ;; esac
    return 0
}
all_ids() { rows | awk '{ print $1 }' | tr '\n' ' '; }

# --- logs -------------------------------------------------------------------------

# follow <pod> <file>: follows the pod's log into the file in the background (until the
# container ends or the script exits).
follow() {
    k logs -f "$1" > "$2" 2>&1 &
    bg_pids+=("$!")
}
# log_has_since <pod> <time> <fixed string>
# (Read whole first: `grep -q` in a pipe ends it early and pipefail would count the writer's SIGPIPE.)
log_has_since() {
    local text
    text=$(k logs "$1" --since-time="$2" 2>/dev/null) || return 1
    grep -qF -- "$3" <<< "$text"
}

# --- memory (record only) ----------------------------------------------------------

# memory_snapshot <label>: kubectl top (metrics-server, when it answers) and each pod's
# cgroup memory.current / memory.peak (bytes), into the transcript and $out/memory.txt.
memory_snapshot() {
    {
        echo "-- memory: $1 (at $((SECONDS - started))s)"
        if top=$(kc top pod -n "$ns" --no-headers 2>&1); then
            printf '%s\n' "$top" | sed 's/^/   top: /'
        else
            echo "   top: (no answer: $(printf '%s' "$top" | head -1))"
        fi
        for pod in $(k get pods -o name | sed 's#^pod/##'); do
            case "$pod" in web-*) continue ;; esac
            local cur peak age
            cur=$(k exec "$pod" -- cat /sys/fs/cgroup/memory.current 2>/dev/null || echo "?")
            peak=$(k exec "$pod" -- cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo "?")
            age=$(pod_field "$pod" '{.status.startTime}')
            awk -v p="$pod" -v c="$cur" -v k="$peak" -v a="$age" 'BEGIN {
                printf "   cgroup: %-34s current %8s MiB  peak %8s MiB  (started %s)\n", p,
                    (c ~ /^[0-9]+$/ ? sprintf("%.0f", c / 1048576) : c),
                    (k ~ /^[0-9]+$/ ? sprintf("%.0f", k / 1048576) : k), a }'
        done
    } | tee -a "$out/memory.txt"
}

echo "twin-check: context $ctx, namespace $ns; transcript files in $out/"
kc get nodes > /dev/null || fail "the cluster ($ctx) does not answer: just twin-up && just twin-apply"
hostdeps=$(k get deploy -l app.kubernetes.io/component=host -o jsonpath='{.items[*].metadata.name}')
[ "$hostdeps" = "host-a host-b" ] || fail "expected the host Deployments host-a host-b, found: '$hostdeps'"

# ---------------------------------------------------------------------------------
step "1. the database, login, toolbox and web are up; the world waits for its forge"
pg_ready() { [ "$(kc -n postgres get cluster.postgresql.cnpg.io/pg -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null)" = True ]; }
wait_until 300 "postgres cluster pg Ready" pg_ready
kc -n login rollout status deploy/login --timeout=300s > /dev/null || fail "login did not roll out in 300s"
say "login rolled out"
k rollout status deploy/toolbox --timeout=300s > /dev/null || fail "toolbox did not roll out in 300s"
say "toolbox rolled out"
k rollout status deploy/web --timeout=300s > /dev/null || fail "web did not roll out in 300s"
say "web rolled out"
waiting_pods() {
    local c a b
    c=$(coordinator_pod) a=$(host_pod a) b=$(host_pod b)
    [ -n "$c" ] && [ -n "$a" ] && [ -n "$b" ] && pod_running "$c" && pod_running "$a" && pod_running "$b" &&
        waiting_forge "$c" && waiting_forge "$a" && waiting_forge "$b"
}
wait_until 180 "the coordinator, host-a and host-b Running, each waiting for the forge (mmo_waiting{why=forge} 1)" waiting_pods
coord=$(coordinator_pod)
pa=$(host_pod a)
pb=$(host_pod b)
# Record (not a check): restarts before the forge, with the end of the previous run's log.
for pod in "$coord" "$pa" "$pb"; do
    n=$(pod_restarts "$pod")
    if [ "${n:-0}" != 0 ]; then
        k logs "$pod" --previous > "$out/$pod.before-forge.previous.log" 2>&1 || true
        say "record: $pod restarted $n time(s) before the forge; its previous run ended with:"
        tail -3 "$out/$pod.before-forge.previous.log" | sed 's/^/      /'
    fi
done
for pod in "$coord" "$pa" "$pb"; do
    pod_ready "$pod" && fail "$pod is Ready before any forge (is this twin fresh? just twin-down && just twin-up && just twin-apply)"
    body=$(readyz "$pod")
    case "$body" in
        *forge*) say "$pod: not Ready, /readyz says: $(printf '%s' "$body" | tr '\n' ' ')" ;;
        *) fail "$pod /readyz does not say forge: $body" ;;
    esac
done

step "1b. a host waiting for the forge stops at once on SIGTERM, exit code 0"
uid1=$(pod_uid "$pb")
k get pod "$pb" -w -o jsonpath='{.metadata.uid} {.status.containerStatuses[0].state.terminated.exitCode}{"\n"}' \
    > "$out/host-b-sigterm.watch" 2>&1 &
bg_pids+=("$!")
follow "$pb" "$out/host-b-sigterm.log"
sleep 2
t0=$SECONDS
k delete pod "$pb" --wait=false > /dev/null
wait_until 30 "the waiting host-b pod $pb gone (its grace period is 100 s)" gone "$pb" "$uid1"
stop_secs=$((SECONDS - t0))
sleep 1
exit_code=$(awk -v u="$uid1" '$1 == u && $2 != "" { print $2 }' "$out/host-b-sigterm.watch" | tail -1)
[ -n "$exit_code" ] || fail "no exit code of the old host-b pod seen (watch: $out/host-b-sigterm.watch)"
[ "$exit_code" = 0 ] || fail "the waiting host-b pod exited $exit_code on SIGTERM, not 0"
grep -qF "stop signal while waiting for a world: exiting" "$out/host-b-sigterm.log" ||
    fail "the waiting host-b's log has no 'stop signal while waiting for a world' line ($out/host-b-sigterm.log)"
say "host-b stopped in ${stop_secs}s with exit code 0: $(grep -F 'stop signal while waiting' "$out/host-b-sigterm.log" | tail -1)"
wait_until 120 "a new host-b pod Running and waiting for the forge" waiting_pods
pb=$(host_pod b)
pod_ready "$pb" && fail "the new host-b pod is Ready before any forge"

# ---------------------------------------------------------------------------------
step "2. forge from the toolbox; the world becomes Ready"
t0=$SECONDS
wjust world-ctl forge > "$out/forge.txt" 2>&1 || { tail -20 "$out/forge.txt"; fail "worldctl forge failed"; }
tail -5 "$out/forge.txt" | sed 's/^/   forge: /'
say "forge done in $((SECONDS - t0))s"
wait_until 300 "the coordinator, host-a and host-b Ready" all_ready
memory_snapshot "after the forge (toolbox peak = the forge)"

step "2b. every host has a PodDisruptionBudget that allows no disruption"
for d in $hostdeps; do
    allowed=$(k get pdb "$d" -o jsonpath='{.status.disruptionsAllowed}') || fail "no PodDisruptionBudget $d"
    expected=$(k get pdb "$d" -o jsonpath='{.status.expectedPods}')
    [ "$allowed" = 0 ] || fail "PodDisruptionBudget $d allows $allowed disruption(s), not 0"
    [ "$expected" = 1 ] || fail "PodDisruptionBudget $d selects $expected pod(s), not its host's 1"
    say "PodDisruptionBudget $d: disruptionsAllowed 0, expectedPods 1"
done
# Record only: whether this k3s refuses an eviction dry run of a host pod by its budget.
pa=$(host_pod a)
ev=$(printf '{"apiVersion":"policy/v1","kind":"Eviction","metadata":{"name":"%s","namespace":"%s"},"deleteOptions":{"dryRun":["All"]}}' "$pa" "$ns")
if said=$(printf '%s' "$ev" | k create --raw "/api/v1/namespaces/$ns/pods/$pa/eviction?dryRun=All" -f - 2>&1); then
    say "record: an eviction dry run of $pa was ALLOWED by the API server: $(printf '%s' "$said" | head -c 200)"
else
    say "record: an eviction dry run of $pa was refused: $(printf '%s' "$said" | tr '\n' ' ' | head -c 200)"
fi
[ "$(host_pod a)" = "$pa" ] && pod_ready "$pa" || fail "host-a's pod changed after the eviction dry run"

# ---------------------------------------------------------------------------------
step "3. register the world in the twin's login service"
reg=$(wjust world-register Twin) || fail "world-register failed: $reg"
# Not orion: world-register prints the kubectl form for the twin's own login service.
prefix="kubectl --context $ctx -n login exec deploy/login -- login "
line=$(printf '%s\n' "$reg" | grep -F "$prefix" || true)
case "$line" in "$prefix"world\ add\ *) ;; *) fail "world-register printed no '${prefix}world add' command: $reg" ;; esac
args="${line#"$prefix"}"
# The only command this script runs from text: checked word by word first.
printf '%s\n' "$args" | grep -Eq "^world add twin --name 'Twin' --link coordinator\.$ns\.svc\.cluster\.local:6490 --link-cert [0-9a-f]{64}$" ||
    fail "world-register printed an unexpected command: $line"
eval "set -- $args"
said=$(kc -n login exec deploy/login -- login "$@" 2>&1) || fail "login world add failed: $said"
printf '%s\n' "$said" | sed 's/^/   login: /'
said=$(kc -n login exec deploy/login -- login world list 2>&1) || fail "login world list failed: $said"
printf '%s\n' "$said" | sed 's/^/   login: /'
grep -q twin <<< "$said" || fail "the login service does not list world twin"

# ---------------------------------------------------------------------------------
step "4. the coordinator sees two live hosts and commits world cuts; one live row per host"
two_live() { [ "$(coord_metric mmo_hosts_live)" = 2 ]; }
wait_until 60 "mmo_hosts_live 2" two_live
wait_until 60 "mmo_cuts_decided_total{outcome=committed} >= 1" committed_above 0
gen=$(coord_metric mmo_world_generation) || fail "no mmo_world_generation"
say "world generation $gen, committed cuts $(committed)"
rows | sed 's/^/   hosts: /'
id_a=$(live_id a)
id_b=$(live_id b)
[ -n "$id_a" ] && [ -n "$id_b" ] || fail "not exactly one live dirty row each for a and b: $(rows | tr '\n' ';')"
say "host a is id $id_a, host b is id $id_b"

# ---------------------------------------------------------------------------------
step "5. crash: host-b's pod force-deleted"
pa=$(host_pod a)
pb=$(host_pod b)
r0=$(pod_restarts "$pa")
uid0=$(pod_uid "$pa")
uid1=$(pod_uid "$pb")
d0=$(coord_metric mmo_successions_declared_total) || fail "no mmo_successions_declared_total"
s0=$(coord_metric mmo_successions_settled_total) || fail "no mmo_successions_settled_total"
say "before: generation $gen, host-a pod $pa restarts $r0, successions declared $d0 settled $s0"
# Decision 9's precondition (below): host b owns no land. A twin where a player made it claim some
# would see a Crash succession instead — this phase checks the landless end only.
land_b=$(row_land "$id_b")
[ "$land_b" = 0 ] || fail "phase 5 needs host b (id $id_b) landless, it owns land=$land_b: the landless-end check does not apply (a landed host's death is a Crash succession)"
t0=$SECONDS
k delete pod "$pb" --grace-period=0 --force > /dev/null 2>&1
# The new pod registers under a new id: Ready without waiting for the old row to be ended.
wait_until 60 "a new host-b pod Ready" host_replaced_and_ready b "$uid1"
pb=$(host_pod b)
say "new host-b pod $pb Ready $((SECONDS - t0))s after the kill"
c1=$(committed) || fail "the coordinator does not answer after the crash"
left=$((60 - (SECONDS - t0)))
[ "$left" -gt 0 ] || left=1
wait_until "$left" "committed cuts grow again (above $c1), within 60 s of the kill" committed_above "$c1"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed: $gen -> $(coord_metric mmo_world_generation)"
[ "$(pod_uid "$pa")" = "$uid0" ] || fail "host-a's pod was replaced"
[ "$(pod_restarts "$pa")" = "$r0" ] || fail "host-a restarted ($r0 -> $(pod_restarts "$pa"))"
old_row_dead() { [ "$(row_state "$id_b")" = dead ]; }
wait_until 60 "the killed host's old row $id_b ended (dead) by the coordinator" old_row_dead
# Decision 9: a host that dies owning no land (the twin has no player to own any) is ENDED, not succeeded.
# (The landed-crash path, a Crash succession, cannot run in the twin: no player claims land here.)
d1=$(coord_metric mmo_successions_declared_total) || fail "no mmo_successions_declared_total after the kill"
s1=$(coord_metric mmo_successions_settled_total) || fail "no mmo_successions_settled_total after the kill"
o1=$(coord_metric mmo_successions_open) || fail "no mmo_successions_open after the kill"
say "after: generation $gen, host-a untouched, successions declared $d0 -> $d1, settled $s0 -> $s1, open $o1"
[ "$d1" = "$d0" ] || fail "a landless host's death declared a succession: declared $d0 -> $d1 (decision 9: ended, none)"
[ "$o1" = 0 ] || fail "a succession is open after a landless host's death: $o1"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed after the landless host's death: $gen -> $(coord_metric mmo_world_generation)"
rows | sed 's/^/   hosts: /'
has_new_live_id b "$id_a" "$id_b" || fail "host b has no live row under a new id: $(rows | tr '\n' ';')"
id_b2=$(live_id b)
[ "$(row_state "$id_b")" = dead ] || fail "host b's old id $id_b is not a dead row (state '$(row_state "$id_b")'): $(rows | tr '\n' ';')"
say "host b is now id $id_b2; its old id $id_b is dead"
k logs "$pb" > "$out/host-b-after-crash.log" 2>&1 || true

# ---------------------------------------------------------------------------------
step "6. clean leave: host-a scaled to 0 (SIGTERM), then back to 1"
pa=$(host_pod a)
uid0=$(pod_uid "$pa")
d0=$(coord_metric mmo_successions_declared_total)
follow "$pa" "$out/host-a-leave.log"
sleep 2
t0=$SECONDS
k scale deploy/host-a --replicas=0 > /dev/null
wait_until 100 "the leaving host-a pod gone" no_pods "app.kubernetes.io/component=host,errantry/host=a"
say "host-a left in $((SECONDS - t0))s"
sleep 1
grep -qF "[server] left the world in cut" "$out/host-a-leave.log" ||
    fail "host-a's log has no leave-cut line ($out/host-a-leave.log)"
grep -qF "left without a cut" "$out/host-a-leave.log" && fail "host-a left without a cut ($out/host-a-leave.log)"
say "$(grep -F '[server] left the world in cut' "$out/host-a-leave.log" | tail -1)"
k scale deploy/host-a --replicas=1 > /dev/null
wait_until 120 "a new host-a pod Ready" host_replaced_and_ready a "$uid0"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed after a clean leave"
[ "$(coord_metric mmo_successions_declared_total)" = "$d0" ] || fail "a clean leave declared a succession"
say "generation $gen unchanged, no succession"
wait_until 30 "host a has a live row under a new id" has_new_live_id a "$id_a" "$id_b" "$id_b2"
id_a2=$(live_id a)
listing=$(rows)
printf '%s\n' "$listing" | sed 's/^/   hosts: /'
# Not vacuous: an empty listing (a failed read) proves nothing.
[ -n "$listing" ] || fail "the hosts listing is empty: cannot check that host b's old id $id_b is gone"
if printf '%s\n' "$listing" | awk -v i="$id_b" '$1 == i { f = 1 } END { exit !f }'; then
    fail "host b's old id $id_b (dead, landless) is still a row after host a's registration: $(printf '%s' "$listing" | tr '\n' ';')"
fi
say "host a is now id $id_a2; host b's old id $id_b is gone (pruned at this registration); host a's old id $id_a: '$(row_state "$id_a")' (record)"

# ---------------------------------------------------------------------------------
step "7. coordinator restart"
restarted_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
old_coord=$(coordinator_pod)
pa=$(host_pod a)
pb=$(host_pod b)
k rollout restart deploy/coordinator > /dev/null
new_coord() {
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] && [ "$c" != "$old_coord" ] && pod_ready "$c"
}
links_up() {
    log_has_since "$pa" "$restarted_at" "world coordinator link up" && log_has_since "$pb" "$restarted_at" "world coordinator link up"
}
t0=$SECONDS
wait_until 90 "both hosts log 'world coordinator link up' again" links_up
left=$((90 - (SECONDS - t0)))
[ "$left" -gt 0 ] || left=1
wait_until "$left" "the new coordinator Ready" new_coord
left=$((90 - (SECONDS - t0)))
[ "$left" -gt 0 ] || left=1
wait_until "$left" "the new coordinator commits a cut (within 90 s of the restart)" committed_above 0
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed after a coordinator restart"
say "new coordinator $(coordinator_pod), generation $gen"

# ---------------------------------------------------------------------------------
step "8. the web Ingress and its /login proxy (only what login's own Ingress publishes)"
# http_code <path>: the status; the body goes to $out/web-body.
http_code() { curl -s -o "$out/web-body" -w '%{http_code}' --max-time 10 -H 'Host: game.localhost' "http://127.0.0.1:8480$1" || true; }
code=$(http_code /healthz)
[ "$code" = 200 ] || fail "http://game.localhost:8480/healthz answered $code"
say "/healthz 200"
# The key set and the API reach login (/v1/config is 404 here: the twin's login runs no
# identity provider, and that 404 would look like the proxy's own).
code=$(http_code /login/.well-known/jwks.json)
[ "$code" = 200 ] && grep -qF '"keys"' "$out/web-body" ||
    fail "http://game.localhost:8480/login/.well-known/jwks.json answered $code, not login's key set (the proxy does not reach login)"
say "/login/.well-known/jwks.json 200 (a key set)"
code=$(http_code /login/v1/worlds)
[ "$code" = 200 ] && grep -qF '"twin"' "$out/web-body" ||
    fail "http://game.localhost:8480/login/v1/worlds answered $code without world twin (the proxy does not reach login's API)"
say "/login/v1/worlds 200 (lists twin)"
# login's health check and metrics stay private, as its own Ingress keeps them.
for path in /login/metrics /login/healthz; do
    code=$(http_code "$path")
    [ "$code" = 404 ] || fail "http://game.localhost:8480$path answered $code, not 404 (the proxy publishes it)"
    say "$path 404"
done

# ---------------------------------------------------------------------------------
step "9. drain host b (world-drain b 5): drained, not restarted, no new row; then back as a new host"
pa=$(host_pod a)
pb=$(host_pod b)
ua=$(pod_uid "$pa") ra=$(pod_restarts "$pa")
ub=$(pod_uid "$pb") rb=$(pod_restarts "$pb")
id_b2=$(live_id b)
[ -n "$id_b2" ] || fail "host b has no live row before the drain: $(rows | tr '\n' ';')"
ids_before=$(all_ids)
say "host b is id $id_b2 (pod $pb); rows before: $ids_before"
t0=$SECONDS
wjust world-drain b 5 > "$out/world-drain.txt" 2>&1 || { sed 's/^/   /' "$out/world-drain.txt"; fail "world-drain b 5 failed"; }
sed 's/^/   /' "$out/world-drain.txt"
grep -qF "host b (id $id_b2) is drained" "$out/world-drain.txt" || fail "world-drain did not report host b drained"
say "world-drain returned after $((SECONDS - t0))s"
is_drained() { [ "$(readyz "$pb")" = drained ]; }
wait_until 60 "host-b's /readyz says drained" is_drained
# The kubelet marks the pod unready after the probe's failure threshold (3 × 5 s).
pod_unready() { ! pod_ready "$1"; }
wait_until 30 "the drained host-b pod not Ready" pod_unready "$pb"
say "waiting 20 s: the drained pod must stay up, unrestarted"
sleep 20
[ "$(host_pod b)" = "$pb" ] || fail "host-b's pod changed after the drain ($pb -> $(host_pod b))"
[ "$(pod_uid "$pb")" = "$ub" ] || fail "host-b's pod was replaced after the drain"
[ "$(pod_restarts "$pb")" = "$rb" ] || fail "host-b restarted after the drain ($rb -> $(pod_restarts "$pb"))"
is_drained || fail "host-b's /readyz no longer says drained: $(readyz "$pb")"
proxy "$pb" livez > /dev/null 2>&1 || fail "the drained host-b's /livez does not answer 200"
say "host-b: /readyz drained, /livez 200, same pod, $rb restart(s)"
listing=$(rows)
# Not vacuous: an empty listing (a failed read) proves nothing.
[ -n "$listing" ] || fail "the hosts listing is empty after the drain: cannot check that no new row appeared"
for id in $(printf '%s\n' "$listing" | awk '{ print $1 }'); do
    case " $ids_before " in *" $id "*) ;; *) fail "a new hosts row $id appeared after the drain: $(printf '%s' "$listing" | tr '\n' ';')" ;; esac
done
printf '%s\n' "$listing" | sed 's/^/   hosts: /'
say "no new hosts row"
[ "$(pod_uid "$pa")" = "$ua" ] && [ "$(pod_restarts "$pa")" = "$ra" ] || fail "host-a's pod changed or restarted during host-b's drain"
c1=$(committed)
wait_until 60 "committed cuts grow with host-b drained (above $c1)" committed_above "$c1"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed after the drain"
wjust world-status > "$out/world-status-drained.txt" 2>&1 || { sed 's/^/   /' "$out/world-status-drained.txt"; fail "world-status failed"; }
grep -qF 'drained: host b left the world' "$out/world-status-drained.txt" || fail "world-status does not name the drained host b ($out/world-status-drained.txt)"
say "world-status: $(grep -F 'drained: host b' "$out/world-status-drained.txt" | head -1)"
t0=$SECONDS
WORLD_START_TIMEOUT=90 wjust world-start > "$out/world-start-drained.txt" 2>&1 ||
    { sed 's/^/   /' "$out/world-start-drained.txt"; fail "world-start with a drained host failed (it must not wait for it)"; }
sed 's/^/   /' "$out/world-start-drained.txt"
grep -qF "host b is DRAINED" "$out/world-start-drained.txt" || fail "world-start did not name the drained host b"
say "world-start returned in $((SECONDS - t0))s without waiting for the drained host"
k rollout restart deployment/host-b > /dev/null
wait_until 120 "host-b back: a new pod Ready" host_replaced_and_ready b "$ub"
# shellcheck disable=SC2086 # one word per id
wait_until 30 "host b registered under a new id" has_new_live_id b $ids_before
say "host b is back as id $(live_id b)"
rows | sed 's/^/   hosts: /'
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed when host b came back"

# ---------------------------------------------------------------------------------
step "10. world-stop, then world-start"
coord=$(coordinator_pod)
pa=$(host_pod a)
pb=$(host_pod b)
follow "$pa" "$out/host-a-stop.log"
follow "$pb" "$out/host-b-stop.log"
follow "$coord" "$out/coordinator-stop.log"
sleep 2
wjust world-stop > "$out/world-stop.txt" 2>&1 || { sed 's/^/   /' "$out/world-stop.txt"; fail "world-stop failed"; }
sed 's/^/   /' "$out/world-stop.txt"
sleep 1
for h in host-a host-b; do
    grep -qF "[server] left the world in cut" "$out/$h-stop.log" || fail "$h's log has no leave-cut line ($out/$h-stop.log)"
    grep -qF "left without a cut" "$out/$h-stop.log" && fail "$h left without a cut ($out/$h-stop.log)"
    say "$h: $(grep -F '[server] left the world in cut' "$out/$h-stop.log" | tail -1)"
done
[ "$(k get deploy/coordinator -o jsonpath='{.spec.replicas}')" = 0 ] || fail "the coordinator was not scaled to 0"
for d in $hostdeps; do
    [ "$(k get "deploy/$d" -o jsonpath='{.spec.replicas}')" = 0 ] || fail "$d was not scaled to 0"
done
no_pods 'app.kubernetes.io/component in (coordinator,host)' || fail "world pods left after world-stop"
wjust world-start > "$out/world-start.txt" 2>&1 || { sed 's/^/   /' "$out/world-start.txt"; fail "world-start failed"; }
sed 's/^/   /' "$out/world-start.txt"
world_started=$SECONDS
wait_until 30 "the coordinator, host-a and host-b Ready" all_ready
wait_until 60 "the coordinator commits a cut after the start" committed_above 0
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed across stop and start"
say "world back, generation $gen"
rows | sed 's/^/   hosts: /'

# ---------------------------------------------------------------------------------
step "11. records: memory five minutes after the world-start; write failures in pod logs"
wait_left=$((300 - (SECONDS - world_started)))
if [ "$wait_left" -gt 0 ]; then
    say "waiting ${wait_left}s (five minutes after the world-start)"
    sleep "$wait_left"
fi
all_ready || fail "the world is not Ready five minutes after its start"
memory_snapshot "five minutes after the world-start"
for pod in $(k get pods -o name | sed 's#^pod/##'); do
    k logs "$pod" --all-containers > "$out/$pod.log" 2>&1 || true
done
if grep -HinE 'permission denied|read-only file system|os error (13|30)|(cannot|could not) (create|write)|failed to (create|write|open)' "$out"/*.log > "$out/write-failures.txt"; then
    echo "   pod log lines about writing files (record; $out/write-failures.txt):"
    head -20 "$out/write-failures.txt" | sed 's/^/   /'
else
    echo "   no pod log line about a file it could not write"
fi

echo
echo "twin-check: PASS in $((SECONDS - started))s (transcript files: $out/)"
