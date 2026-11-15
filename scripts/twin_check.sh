#!/usr/bin/env bash
# The k3d twin's integration check (`just twin-check`; plan
# docs/superpowers/plans/2026-11-15-r5-k8s.md, Task 13). Runs against a FRESH
# `just twin-apply` (it forges the world): the modules, images and server code of the
# world on Kubernetes, end to end, with no client.
#   1. pg Ready, login / toolbox / web rolled out; the coordinator and both hosts run,
#      unready, /readyz "forge" (nothing forged by a pod). 1b: a host waiting for the
#      forge stops on a plain SIGTERM at once with exit code 0 (PID 1's signal path).
#   2. worldctl forge from the toolbox; the coordinator and both hosts become Ready.
#   3. The world registered in the twin's own login service (world-register's kubectl command).
#   4. Coordinator /metrics: 2 hosts live, a committed world cut.
#   5. hosts-1 force-killed: the world keeps its generation, hosts-0 is untouched,
#      hosts-1 comes back Ready, cuts commit again; what a landless host's death does
#      to the successions (one Crash succession, settled: the ledger's ruling).
#   6. hosts-0 deleted (SIGTERM): it leaves with a committed leave cut, comes back.
#   7. The coordinator restarted: both hosts' links come up again, cuts commit.
#   8. The web Ingress answers through Traefik; its /login proxy reaches login's key set
#      and API, and login's /metrics and /healthz are NOT published (404).
#   9. world-stop then world-start (the recipes): the hosts leave with committed leave
#      cuts, the coordinator stops after them, everything comes back Ready.
#  10. Records (asserts nothing): each pod's memory after the forge and five minutes
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
# The coordinator's pod that is not being deleted (a Deployment's pod has a generated name).
coordinator_pod() {
    k get pods -l app.kubernetes.io/component=coordinator \
        -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers 2>/dev/null |
        awk '$2 == "<none>" { print $1; exit }' || true
}
all_ready() {
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] && pod_ready "$c" && pod_ready hosts-0 && pod_ready hosts-1
}
# replaced_and_ready <pod> <old uid>: a new pod of that name is Ready.
replaced_and_ready() {
    local uid
    uid=$(pod_uid "$1") || return 1
    [ -n "$uid" ] && [ "$uid" != "$2" ] && pod_ready "$1"
}
# gone <pod> <uid>: no pod of that name with that uid.
gone() { [ "$(pod_uid "$1" || true)" != "$2" ]; }

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
                printf "   cgroup: %-28s current %8s MiB  peak %8s MiB  (started %s)\n", p,
                    (c ~ /^[0-9]+$/ ? sprintf("%.0f", c / 1048576) : c),
                    (k ~ /^[0-9]+$/ ? sprintf("%.0f", k / 1048576) : k), a }'
        done
    } | tee -a "$out/memory.txt"
}

echo "twin-check: context $ctx, namespace $ns; transcript files in $out/"
kc get nodes > /dev/null || fail "the cluster ($ctx) does not answer: just twin-up && just twin-apply"

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
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] && pod_running "$c" && pod_running hosts-0 && pod_running hosts-1 &&
        waiting_forge "$c" && waiting_forge hosts-0 && waiting_forge hosts-1
}
wait_until 180 "the coordinator, hosts-0 and hosts-1 Running, each waiting for the forge (mmo_waiting{why=forge} 1)" waiting_pods
coord=$(coordinator_pod)
# Record (not a check): restarts before the forge, with the end of the previous run's log.
for pod in "$coord" hosts-0 hosts-1; do
    n=$(pod_restarts "$pod")
    if [ "${n:-0}" != 0 ]; then
        k logs "$pod" --previous > "$out/$pod.before-forge.previous.log" 2>&1 || true
        say "record: $pod restarted $n time(s) before the forge; its previous run ended with:"
        tail -3 "$out/$pod.before-forge.previous.log" | sed 's/^/      /'
    fi
done
for pod in "$coord" hosts-0 hosts-1; do
    pod_ready "$pod" && fail "$pod is Ready before any forge (is this twin fresh? just twin-down && just twin-up && just twin-apply)"
    body=$(readyz "$pod")
    case "$body" in
        *forge*) say "$pod: not Ready, /readyz says: $(printf '%s' "$body" | tr '\n' ' ')" ;;
        *) fail "$pod /readyz does not say forge: $body" ;;
    esac
done

step "1b. a host waiting for the forge stops at once on SIGTERM, exit code 0"
uid1=$(pod_uid hosts-1)
k get pod hosts-1 -w -o jsonpath='{.metadata.uid} {.status.containerStatuses[0].state.terminated.exitCode}{"\n"}' \
    > "$out/hosts-1-sigterm.watch" 2>&1 &
bg_pids+=("$!")
follow hosts-1 "$out/hosts-1-sigterm.log"
sleep 2
t0=$SECONDS
k delete pod hosts-1 --wait=false > /dev/null
wait_until 30 "the waiting hosts-1 gone (its grace period is 100 s)" gone hosts-1 "$uid1"
stop_secs=$((SECONDS - t0))
sleep 1
exit_code=$(awk -v u="$uid1" '$1 == u && $2 != "" { print $2 }' "$out/hosts-1-sigterm.watch" | tail -1)
[ -n "$exit_code" ] || fail "no exit code of the old hosts-1 seen (watch: $out/hosts-1-sigterm.watch)"
[ "$exit_code" = 0 ] || fail "the waiting hosts-1 exited $exit_code on SIGTERM, not 0"
grep -qF "stop signal while waiting for a world: exiting" "$out/hosts-1-sigterm.log" ||
    fail "the waiting hosts-1's log has no 'stop signal while waiting for a world' line ($out/hosts-1-sigterm.log)"
say "hosts-1 stopped in ${stop_secs}s with exit code 0: $(grep -F 'stop signal while waiting' "$out/hosts-1-sigterm.log" | tail -1)"
wait_until 120 "the new hosts-1 Running and waiting for the forge" waiting_pods
pod_ready hosts-1 && fail "the new hosts-1 is Ready before any forge"

# ---------------------------------------------------------------------------------
step "2. forge from the toolbox; the world becomes Ready"
t0=$SECONDS
wjust world-ctl forge > "$out/forge.txt" 2>&1 || { tail -20 "$out/forge.txt"; fail "worldctl forge failed"; }
tail -5 "$out/forge.txt" | sed 's/^/   forge: /'
say "forge done in $((SECONDS - t0))s"
wait_until 300 "the coordinator, hosts-0 and hosts-1 Ready" all_ready
memory_snapshot "after the forge (toolbox peak = the forge)"

# ---------------------------------------------------------------------------------
step "3. register the world in the twin's login service"
reg=$(wjust world-register Twin) || fail "world-register failed: $reg"
# Not orion: world-register prints the kubectl form for the twin's own login service.
prefix="kubectl --context $ctx -n login exec deploy/login -- login "
line=$(printf '%s\n' "$reg" | grep -F "$prefix" || true)
case "$line" in "$prefix"world\ add\ *) ;; *) fail "world-register printed no '${prefix}world add' command: $reg" ;; esac
args="${line#"$prefix"}"
# The only command this script runs from text: checked word by word first.
printf '%s\n' "$args" | grep -Eq "^world add twin --name 'Twin' --quic 127\.0\.0\.1:30466 --wt https://127\.0\.0\.1:30467/ --link hosts-0\.hosts\.$ns\.svc\.cluster\.local:6490 --link-cert [0-9a-f]{64}$" ||
    fail "world-register printed an unexpected command: $line"
eval "set -- $args"
said=$(kc -n login exec deploy/login -- login "$@" 2>&1) || fail "login world add failed: $said"
printf '%s\n' "$said" | sed 's/^/   login: /'
said=$(kc -n login exec deploy/login -- login world list 2>&1) || fail "login world list failed: $said"
printf '%s\n' "$said" | sed 's/^/   login: /'
grep -q twin <<< "$said" || fail "the login service does not list world twin"

# ---------------------------------------------------------------------------------
step "4. the coordinator sees two live hosts and commits world cuts"
two_live() { [ "$(coord_metric mmo_hosts_live)" = 2 ]; }
wait_until 60 "mmo_hosts_live 2" two_live
wait_until 60 "mmo_cuts_decided_total{outcome=committed} >= 1" committed_above 0
gen=$(coord_metric mmo_world_generation) || fail "no mmo_world_generation"
say "world generation $gen, committed cuts $(committed)"

# ---------------------------------------------------------------------------------
step "5. crash: hosts-1 force-deleted"
r0=$(pod_restarts hosts-0)
uid0=$(pod_uid hosts-0)
uid1=$(pod_uid hosts-1)
d0=$(coord_metric mmo_successions_declared_total) || fail "no mmo_successions_declared_total"
s0=$(coord_metric mmo_successions_settled_total) || fail "no mmo_successions_settled_total"
say "before: generation $gen, hosts-0 restarts $r0, successions declared $d0 settled $s0"
t0=$SECONDS
k delete pod hosts-1 --grace-period=0 --force > /dev/null 2>&1
wait_until 60 "a new hosts-1 Ready" replaced_and_ready hosts-1 "$uid1"
c1=$(committed) || fail "the coordinator does not answer after the crash"
left=$((60 - (SECONDS - t0)))
[ "$left" -gt 0 ] || left=1
wait_until "$left" "committed cuts grow again (above $c1), within 60 s of the kill" committed_above "$c1"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed: $gen -> $(coord_metric mmo_world_generation)"
[ "$(pod_uid hosts-0)" = "$uid0" ] || fail "hosts-0 was replaced"
[ "$(pod_restarts hosts-0)" = "$r0" ] || fail "hosts-0 restarted ($r0 -> $(pod_restarts hosts-0))"
d1=$(coord_metric mmo_successions_declared_total)
s1=$(coord_metric mmo_successions_settled_total)
o1=$(coord_metric mmo_successions_open)
say "after: generation $gen, hosts-0 untouched, successions declared $d0 -> $d1, settled $s0 -> $s1, open $o1"
# Ruling (ledger): a host that owns no land still gets a Crash succession when it dies —
# Coordinator::declare_wanted never looks at land; the new boot vouches its old row dead
# (seen_ms 0 = lapsed at once), waits for the succession, and registers after the settle.
[ "$d1" = $((d0 + 1)) ] || fail "expected exactly one succession declared for the killed host, got $d0 -> $d1"
[ "$s1" = "$d1" ] && [ "$o1" = 0 ] || fail "the succession is not settled (declared $d1, settled $s1, open $o1)"
k logs hosts-1 > "$out/hosts-1-after-crash.log" 2>&1 || true

# ---------------------------------------------------------------------------------
step "6. clean leave: hosts-0 deleted (SIGTERM)"
uid0=$(pod_uid hosts-0)
d0=$(coord_metric mmo_successions_declared_total)
follow hosts-0 "$out/hosts-0-leave.log"
sleep 2
t0=$SECONDS
k delete pod hosts-0 --wait=false > /dev/null
wait_until 100 "the leaving hosts-0 gone" gone hosts-0 "$uid0"
say "hosts-0 left in $((SECONDS - t0))s"
sleep 1
grep -qF "[server] left the world in cut" "$out/hosts-0-leave.log" ||
    fail "hosts-0's log has no leave-cut line ($out/hosts-0-leave.log)"
grep -qF "left without a cut" "$out/hosts-0-leave.log" && fail "hosts-0 left without a cut ($out/hosts-0-leave.log)"
say "$(grep -F '[server] left the world in cut' "$out/hosts-0-leave.log" | tail -1)"
wait_until 120 "a new hosts-0 Ready" replaced_and_ready hosts-0 "$uid0"
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed after a clean leave"
[ "$(coord_metric mmo_successions_declared_total)" = "$d0" ] || fail "a clean leave declared a succession"
say "generation $gen unchanged, no succession"

# ---------------------------------------------------------------------------------
step "7. coordinator restart"
restarted_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
old_coord=$(coordinator_pod)
k rollout restart deploy/coordinator > /dev/null
new_coord() {
    local c
    c=$(coordinator_pod)
    [ -n "$c" ] && [ "$c" != "$old_coord" ] && pod_ready "$c"
}
links_up() {
    log_has_since hosts-0 "$restarted_at" "world coordinator link up" && log_has_since hosts-1 "$restarted_at" "world coordinator link up"
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
step "9. world-stop, then world-start"
coord=$(coordinator_pod)
follow hosts-0 "$out/hosts-0-stop.log"
follow hosts-1 "$out/hosts-1-stop.log"
follow "$coord" "$out/coordinator-stop.log"
sleep 2
wjust world-stop > "$out/world-stop.txt" 2>&1 || { sed 's/^/   /' "$out/world-stop.txt"; fail "world-stop failed"; }
sed 's/^/   /' "$out/world-stop.txt"
sleep 1
for h in hosts-0 hosts-1; do
    grep -qF "[server] left the world in cut" "$out/$h-stop.log" || fail "$h's log has no leave-cut line ($out/$h-stop.log)"
    grep -qF "left without a cut" "$out/$h-stop.log" && fail "$h left without a cut ($out/$h-stop.log)"
    say "$h: $(grep -F '[server] left the world in cut' "$out/$h-stop.log" | tail -1)"
done
[ "$(k get deploy/coordinator -o jsonpath='{.spec.replicas}')" = 0 ] || fail "the coordinator was not scaled to 0"
[ -z "$(k get pods -o name | grep -E '^pod/(hosts-|coordinator-)' || true)" ] || fail "world pods left after world-stop"
wjust world-start > "$out/world-start.txt" 2>&1 || { sed 's/^/   /' "$out/world-start.txt"; fail "world-start failed"; }
sed 's/^/   /' "$out/world-start.txt"
world_started=$SECONDS
wait_until 30 "the coordinator, hosts-0 and hosts-1 Ready" all_ready
wait_until 60 "the coordinator commits a cut after the start" committed_above 0
[ "$(coord_metric mmo_world_generation)" = "$gen" ] || fail "the world generation changed across stop and start"
say "world back, generation $gen"

# ---------------------------------------------------------------------------------
step "10. records: memory five minutes after the world-start; write failures in pod logs"
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
