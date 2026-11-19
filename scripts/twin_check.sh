#!/usr/bin/env bash
# The k3d twin's integration check (`just twin-check`; plans
# docs/superpowers/plans/2026-11-15-r5-k8s.md, Task 13,
# docs/superpowers/plans/2026-11-15-elastic-1-hosts-as-deployments.md, Task 12, and
# docs/superpowers/plans/2026-11-19-elastic-4-reconciler-scaling.md, Task 6). Runs against a
# FRESH `just twin-apply` (it forges the world): the modules, images and server code of the
# world on Kubernetes, end to end, with no client. The hosts are made by the fleet reconciler
# (Deployment fleet) from the coordinator's desired_hosts rows: Deployments host-<name> with
# names the store issues (h1, h2, …; pods selected by app.kubernetes.io/component=host and
# errantry/host=<name>; a pod's name is random, a host's id is issued at registration). Host
# names are discovered, never assumed; the twin's fleet is min = max = TWIN_HOSTS (2: k3d maps
# exactly the node ports of two slots, 30466-30469; checked at the start).
# The coordinator runs TWIN_COORDINATORS replicas (default 2, the module's default; checked
# at the start). Counters (cuts decided, successions, fleet decisions) are summed over the live
# replica pods (each counts what it wrote); hosts live is the ticker's (mmo_coordinator_is_ticker
# 1); the world generation must be the same on every replica that answers.
#   1. pg Ready, login / toolbox / web rolled out; every coordinator replica and the fleet
#      reconciler run, unready, /readyz "forge" (nothing forged by a pod); no host Deployment
#      exists (nothing wants one before the forge). 1b: a pod waiting for the forge (the
#      reconciler's) stops on a plain SIGTERM at once with exit code 0 (PID 1's signal path).
#   2. worldctl forge from the toolbox; every coordinator replica and the reconciler become Ready.
#      2c: the two hosts appear by themselves (the coordinator's "below the minimum" rule adds
#      the desired rows h1, h2 — at most one decision per 10 s — and the reconciler creates
#      their Deployments): each on the coordinator's image, its Service on node ports
#      port_base + 2·slot and + 1 (30466-30469), both Ready; mmo_fleet_added_total summed over
#      the replicas 0 -> 2.
#      2b: every host has a PodDisruptionBudget that allows no disruption (0), the
#      coordinator one with min_available 1 over its replicas; an eviction dry run of a
#      host pod is recorded (refused or not: not a check).
#   3. The world registered in the twin's own login service (world-register's kubectl
#      command: the link is the coordinator's, no --quic / --wt).
#   4. Coordinator /metrics: exactly one ticker, every replica counts them all live; 2 hosts
#      live, a committed world cut; `worldctl hosts`: one live row per host name.
#   5. The second host's pod force-killed: a new pod (same Deployment, same name) is Ready,
#      under a NEW id; the old id is ended (landless: dead, or already pruned), no succession,
#      generation unchanged; the first host's pod is untouched, cuts commit again.
#   6. The first host scaled to 0 (SIGTERM): it leaves with a committed leave cut, no
#      succession; scaled to 1: a new id; the killed host's old row is gone (pruned at this
#      registration). The reconciler leaves a Deployment at 0 alone.
#   7. One coordinator replica killed mid-play: the ticker's pod force-deleted. A join through
#      the login service (an account issued by `login account issue-refresh`, POST /v1/join
#      through the web proxy) answers 200 before the kill and again within 90 s of it; cuts
#      commit within 90 s (the counters of the surviving pods grow past the survivor's own);
#      another replica becomes the ticker; both hosts' coordinator links end up on a live
#      replica (a host that was on the killed one logs `world coordinator link up` again);
#      the generation is unchanged; the Deployment brings the replica count back, all Ready.
#   7b. Every replica killed and restarted: the coordinator scaled to 0 (no pod left; the
#      reconciler's /readyz says "stopped"), then back to its count: every host re-attaches
#      (`world coordinator link up` after the start, both hosts Ready), a cut commits, the
#      generation is unchanged.
#   7c. Land assigned, idle, released, re-assigned (Elastic 3; the twin's hosts run
#      VOXEL_LAND_IDLE_MS=20000, the module's land_idle_ms): once no super_regions row is owned
#      (phase 7's land went back), a join through the login service (phase 7's helper, no
#      client connects) → within 30 s exactly one row is owned, by a live host id (assigned);
#      within 210 s that row's owner is -1 at a higher epoch and its host's
#      mmo_land_released_total grew (idle, released: the placed order pins its region for
#      SPAWN_PIN_MS, 120 s, while it waits for a session, then 20 s idle, then the next cut); a second join → the row has an owner again
#      at a higher epoch (re-assigned). The coordinators' mmo_land_assigned_total grew by 2 or
#      more, the generation is unchanged and no succession row was added. Rows are read with
#      psql in the CloudNativePG primary (database world_<id>, TWIN_WORLD_DB).
#   8. The web Ingress answers through Traefik; its /login proxy reaches login's key set
#      and API, and login's /metrics and /healthz are NOT published (404).
#   9. world-drain <second host> 5 (the recipe): it leaves at the deadline (drained); the
#      coordinator marks its desired row draining (mmo_fleet_drained_total grows), removes the
#      row once the host left (mmo_fleet_removed_total grows) and the reconciler deletes its
#      Deployment, Service and PDB — all by themselves, no apply, no `rollout restart`; then a
#      third name appears (min 2) on the freed slot (its node ports), Ready. The first host
#      runs on untouched; cuts commit; the generation is unchanged.
#  10. world-stop then world-start (the recipes): the reconciler stops first (its Deployment
#      at 0, no pod), the hosts leave with committed leave cuts, every coordinator replica
#      stops after them (the count recorded in the annotation errantry/replicas); everything
#      comes back Ready at that count, the reconciler at 1 and Ready. Then a stray: a
#      Deployment host-zz labelled for this world (errantry/fleet, errantry/host=zz; replicas 0)
#      made by hand is deleted by the reconciler within a few passes; the hosts are untouched.
#  10b. The reconciler's pod deleted mid-play: nothing running changes (the host Deployments,
#      their pods and restarts; cuts commit meanwhile); a new reconciler pod is Ready and, a
#      few passes on, has created and deleted nothing.
#  11. Records (asserts nothing): each pod's memory after the forge and five minutes
#      after the world-start, any pod log line about a file it could not write, and each
#      coordinator replica's `[coordinator] window=` lines (transitions, conflicts, assigned).
#  12. To zero (Elastic 4 Task 8): VOXEL_FLEET_MIN=0 set on the coordinator Deployment (every
#      replica restarts together, as in 7b). With nobody online the hosts go ONE AT A TIME
#      (rule 6 drains one while two are wanted, rule 7 the last: each after the module's
#      host_min_lifetime_ms 60 s and scale_down_after_ms 30 s; never two desired rows draining
#      at once, never a second drain before the first host's Deployment is gone) and are
#      removed: no desired row, no host Deployment, no live dirty hosts row; the coordinator
#      replicas stay Ready; no succession row added, the generation unchanged.
#  13. The join that starts the world: a join (phase 7's helper) answers 503 world_starting; a
#      wanted desired row and its host pod appear; the join repeated every second answers 200;
#      the seconds from the first 503 to the first 200 are RECORDED in cold-start.txt (the cold
#      start, images already on the node). The repeated joins add no second host (one desired
#      row, mmo_fleet_added_total grows by exactly 1). The generation is unchanged, no
#      succession row added. VOXEL_FLEET_MIN is then set back (every replica restarts again).
# Every wait has a timeout and says what it waited for. Exit 0 = pass, 1 = a check
# failed (pods, events and logs are dumped and saved under target/twin/check-<time>/).
# Env: TWIN_CONTEXT (k3d-mmo), TWIN_NAMESPACE (world-twin), TWIN_COORDINATORS (2),
# TWIN_HOSTS (2), TWIN_WORLD_DB (world_twin).
set -euo pipefail
cd "$(dirname "$0")/.."

ctx="${TWIN_CONTEXT:-k3d-mmo}"
ns="${TWIN_NAMESPACE:-world-twin}"
coordinators="${TWIN_COORDINATORS:-2}"
# The fleet's size (the root's fleet.min = fleet.max), and the world id (the errantry/fleet label).
fleet_hosts="${TWIN_HOSTS:-2}"
world_id="${ns#world-}"
world_db="${TWIN_WORLD_DB:-world_twin}"
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
    echo "---- hosts rows, desired hosts"
    wjust world-hosts 2>&1 | tail -30 || true
    echo "---- host Deployments, Services, PDBs"
    k get deploy,svc,pdb -l "errantry/fleet=$world_id" 2>&1 || true
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
# fleet_pod: the fleet reconciler's pod not being deleted ('' if none).
fleet_pod() { live_pod app.kubernetes.io/component=fleet; }
# fleet_ready: its pod is Ready and answers /readyz 200.
fleet_ready() {
    local p
    p=$(fleet_pod)
    [ -n "$p" ] && pod_ready "$p" && proxy "$p" readyz > /dev/null 2>&1
}
# coordinator_pods: every coordinator replica pod not being deleted, one per line.
coordinator_pods() {
    k get pods -l app.kubernetes.io/component=coordinator -o custom-columns=NAME:.metadata.name,DELETING:.metadata.deletionTimestamp --no-headers 2>/dev/null |
        awk '$2 == "<none>" { print $1 }' || true
}
# coords_ready: exactly $coordinators replica pods, every one Ready.
coords_ready() {
    local n=0 p
    for p in $(coordinator_pods); do
        pod_ready "$p" || return 1
        n=$((n + 1))
    done
    [ "$n" = "$coordinators" ]
}
# host_pod <name>: host <name>'s pod (Deployment host-<name>).
host_pod() { live_pod "app.kubernetes.io/component=host,errantry/host=$1"; }
host_ready() {
    local p
    p=$(host_pod "$1")
    [ -n "$p" ] && pod_ready "$p"
}
# host_names: the host Deployments' names without `host-` (h1 h2 …), in number order, one per line.
host_names() {
    k get deploy -l app.kubernetes.io/component=host -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}' 2>/dev/null |
        sed -n 's/^host-//p' | sort -V || true
}
# hosts_ready <n>: exactly n host Deployments, each host's pod Ready.
hosts_ready() {
    local names h n=0
    names=$(host_names)
    for h in $names; do
        host_ready "$h" || return 1
        n=$((n + 1))
    done
    [ "$n" = "$1" ]
}
all_ready() { coords_ready && fleet_ready && hosts_ready "$fleet_hosts"; }
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
# coord_sum <name> [<label>=<value>]...: the sum of that sample over the coordinator replica
# pods that answer (a counter: each replica counts what it wrote); fails when none answers.
coord_sum() {
    local p v sum=0 any=0
    for p in $(coordinator_pods); do
        v=$(metric "$p" "$@") || continue
        sum=$((sum + v)) any=1
    done
    [ "$any" = 1 ] && printf '%s\n' "$sum"
}
# coord_max <name> [<label>=<value>]...: the largest sample over the replicas that answer.
coord_max() {
    local p v max='' any=0
    for p in $(coordinator_pods); do
        v=$(metric "$p" "$@") || continue
        if [ "$any" = 0 ] || [ "$v" -gt "$max" ]; then max=$v; fi
        any=1
    done
    [ "$any" = 1 ] && printf '%s\n' "$max"
}
# coord_gen: the world generation, when every replica that answers says the same one
# ('mixed:<values>' otherwise, which equals no generation); fails when none answers.
coord_gen() {
    local p v seen=''
    for p in $(coordinator_pods); do
        v=$(metric "$p" mmo_world_generation) || continue
        case " $seen " in *" $v "*) ;; *) seen="${seen:+$seen }$v" ;; esac
    done
    [ -n "$seen" ] || return 1
    case "$seen" in *' '*) printf 'mixed:%s\n' "$seen" ;; *) printf '%s\n' "$seen" ;; esac
}
# tickers: the replica pods whose /metrics say mmo_coordinator_is_ticker 1, one per line.
tickers() {
    local p
    for p in $(coordinator_pods); do
        [ "$(metric "$p" mmo_coordinator_is_ticker || true)" = 1 ] && printf '%s\n' "$p"
    done
    return 0
}
# ticker_pod: the one ticker replica pod ('' unless exactly one says so).
ticker_pod() {
    local t
    t=$(tickers)
    [ "$(printf '%s' "$t" | grep -c .)" = 1 ] && printf '%s\n' "$t"
    return 0
}
one_ticker() { [ -n "$(ticker_pod)" ]; }
# ticker_metric <name> [<label>=<value>]...: the sample of the ticker replica (it alone reads the hosts).
ticker_metric() {
    local t
    t=$(ticker_pod)
    [ -n "$t" ] || return 1
    metric "$t" "$@"
}
committed() { coord_sum mmo_cuts_decided_total outcome=committed; }
# committed_above <n>: the replicas' committed cut count (summed) is above n.
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

# --- the desired hosts (worldctl desired, through the world-ctl recipe) -------------------

# desired: `worldctl desired`'s lines (`<name> slot=<n> <wanted|draining> age=<ms>`); '' when the
# read fails (callers that need a row treat that as "not yet").
desired() { wjust world-ctl desired 2>/dev/null | grep -E '^[^ ]+ slot=[0-9]+ ' || true; }
# desired_state <name>: wanted / draining, '' when there is no such row.
desired_state() { desired | awk -v n="$1" '$1 == n { print $3 }'; }
# desired_slot <name>: its slot, '' when there is no such row.
desired_slot() { desired | awk -v n="$1" '$1 == n { sub(/^slot=/, "", $2); print $2 }'; }
# node_ports <name>: Service host-<name>'s node ports `<quic> <wt>` ('' when there is none).
node_ports() {
    k get svc "host-$1" -o jsonpath='{.spec.ports[?(@.name=="quic")].nodePort} {.spec.ports[?(@.name=="wt")].nodePort}' 2>/dev/null || true
}
# host_image <name>: Deployment host-<name>'s `host` container image.
host_image() { k get deploy "host-$1" -o jsonpath='{.spec.template.spec.containers[?(@.name=="host")].image}' 2>/dev/null || true; }
# coordinator_image: the coordinator Deployment's `coordinator` container image (what a new host gets).
coordinator_image() { k get deploy/coordinator -o jsonpath='{.spec.template.spec.containers[?(@.name=="coordinator")].image}'; }
# objects_of <name>: host <name>'s Deployment, Service and PDB that exist, as `kind/name` words.
objects_of() {
    local o
    for o in deploy svc pdb; do
        k get "$o" --ignore-not-found "host-$1" -o name 2>/dev/null || echo "$o/unknown(read failed)"
    done | tr '\n' ' '
}
# host_gone <name>: no desired row, Deployment, Service or PDB of that name (a failed read is not gone).
host_gone() {
    local listing
    listing=$(desired)
    [ -n "$listing" ] || return 1
    printf '%s\n' "$listing" | awk -v n="$1" '$1 == n { f = 1 } END { exit f }' || return 1
    [ -z "$(objects_of "$1" | tr -d ' ')" ]
}

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
k get deploy/fleet > /dev/null 2>&1 || fail "no Deployment fleet in $ns (the fleet reconciler, Elastic 4): just twin-apply with this tree"
want=$(k get deploy/coordinator -o jsonpath='{.spec.replicas}')
[ "$want" = "$coordinators" ] || fail "the coordinator Deployment wants $want replica(s), this check expects $coordinators (TWIN_COORDINATORS)"
# The fleet's dials: phases 2c and 9 count on min = max = TWIN_HOSTS (two slots, the node ports k3d maps).
env_of() { k get "deploy/$1" -o jsonpath="{.spec.template.spec.containers[?(@.name==\"$2\")].env[?(@.name==\"$3\")].value}"; }
fmin=$(env_of coordinator coordinator VOXEL_FLEET_MIN)
fmax=$(env_of coordinator coordinator VOXEL_FLEET_MAX)
[ "$fmin" = "$fleet_hosts" ] && [ "$fmax" = "$fleet_hosts" ] ||
    fail "the coordinator runs VOXEL_FLEET_MIN='$fmin' VOXEL_FLEET_MAX='$fmax'; this check expects both $fleet_hosts (TWIN_HOSTS; the root's fleet.min / fleet.max)"
port_base=$(env_of fleet fleet VOXEL_FLEET_PORT_BASE)
case "$port_base" in ''|*[!0-9]*) fail "the fleet Deployment's VOXEL_FLEET_PORT_BASE is '$port_base', not a port" ;; esac
say "fleet: min = max = $fleet_hosts, node ports from $port_base, world id $world_id"

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
# Nothing wants a host before the forge: no desired row, so the reconciler (itself waiting) made none.
[ -z "$(host_names)" ] || fail "host Deployments exist before any forge: $(host_names | tr '\n' ' ')(is this twin fresh? just twin-down && just twin-up && just twin-apply)"
waiting_pods() {
    local c f n=0
    f=$(fleet_pod)
    [ -n "$f" ] && pod_running "$f" && waiting_forge "$f" || return 1
    for c in $(coordinator_pods); do
        pod_running "$c" && waiting_forge "$c" || return 1
        n=$((n + 1))
    done
    [ "$n" = "$coordinators" ]
}
wait_until 180 "$coordinators coordinator replicas and the fleet reconciler Running, each waiting for the forge (mmo_waiting{why=forge} 1)" waiting_pods
coords=$(coordinator_pods | tr '\n' ' ')
pf=$(fleet_pod)
say "coordinator replicas: $coords; fleet reconciler: $pf"
# Record (not a check): restarts before the forge, with the end of the previous run's log.
# shellcheck disable=SC2086 # one word per pod
for pod in $coords "$pf"; do
    n=$(pod_restarts "$pod")
    if [ "${n:-0}" != 0 ]; then
        k logs "$pod" --previous > "$out/$pod.before-forge.previous.log" 2>&1 || true
        say "record: $pod restarted $n time(s) before the forge; its previous run ended with:"
        tail -3 "$out/$pod.before-forge.previous.log" | sed 's/^/      /'
    fi
done
# shellcheck disable=SC2086 # one word per pod
for pod in $coords "$pf"; do
    pod_ready "$pod" && fail "$pod is Ready before any forge (is this twin fresh? just twin-down && just twin-up && just twin-apply)"
    body=$(readyz "$pod")
    case "$body" in
        *forge*) say "$pod: not Ready, /readyz says: $(printf '%s' "$body" | tr '\n' ' ')" ;;
        *) fail "$pod /readyz does not say forge: $body" ;;
    esac
done
[ -z "$(host_names)" ] || fail "host Deployments appeared before any forge: $(host_names | tr '\n' ' ')"
added0=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
[ "$added0" = 0 ] || fail "the coordinators added $added0 host(s) before any forge"

step "1b. a pod waiting for the forge (the fleet reconciler's) stops at once on SIGTERM, exit code 0"
uid1=$(pod_uid "$pf")
k get pod "$pf" -w -o jsonpath='{.metadata.uid} {.status.containerStatuses[0].state.terminated.exitCode}{"\n"}' \
    > "$out/fleet-sigterm.watch" 2>&1 &
bg_pids+=("$!")
follow "$pf" "$out/fleet-sigterm.log"
sleep 2
t0=$SECONDS
k delete pod "$pf" --wait=false > /dev/null
# Its grace period is 30 s: gone within 20 s means it exited by itself (the exit code says how).
wait_until 20 "the waiting fleet pod $pf gone (its grace period is 30 s)" gone "$pf" "$uid1"
stop_secs=$((SECONDS - t0))
sleep 1
exit_code=$(awk -v u="$uid1" '$1 == u && $2 != "" { print $2 }' "$out/fleet-sigterm.watch" | tail -1)
[ -n "$exit_code" ] || fail "no exit code of the old fleet pod seen (watch: $out/fleet-sigterm.watch)"
[ "$exit_code" = 0 ] || fail "the waiting fleet pod exited $exit_code on SIGTERM, not 0"
grep -qF "stop signal while waiting for a world: exiting" "$out/fleet-sigterm.log" ||
    fail "the waiting fleet pod's log has no 'stop signal while waiting for a world' line ($out/fleet-sigterm.log)"
say "the fleet pod stopped in ${stop_secs}s with exit code 0: $(grep -F 'stop signal while waiting' "$out/fleet-sigterm.log" | tail -1)"
wait_until 120 "a new fleet pod Running and waiting for the forge" waiting_pods
pf=$(fleet_pod)
pod_ready "$pf" && fail "the new fleet pod is Ready before any forge"

# ---------------------------------------------------------------------------------
step "2. forge from the toolbox; the coordinator and the reconciler become Ready"
t0=$SECONDS
wjust world-ctl forge > "$out/forge.txt" 2>&1 || { tail -20 "$out/forge.txt"; fail "worldctl forge failed"; }
tail -5 "$out/forge.txt" | sed 's/^/   forge: /'
say "forge done in $((SECONDS - t0))s"
coords_and_fleet_ready() { coords_ready && fleet_ready; }
wait_until 300 "$coordinators coordinator replicas and the fleet reconciler Ready" coords_and_fleet_ready

step "2c. the hosts appear by themselves: the coordinator adds $fleet_hosts desired rows, the reconciler their Deployments"
# The ticker adds one row per decision (at most one per 10 s, each after a hosts read); the
# reconciler creates each row's objects within a pass (2 s); a host is Ready once registered and attached.
want_names=$(seq 1 "$fleet_hosts" | sed 's/^/h/' | tr '\n' ' ')
names_are_first() { [ "$(host_names | tr '\n' ' ')" = "$want_names" ]; }
wait_until 120 "the host Deployments $want_names(desired rows added by the coordinator, created by the reconciler)" names_are_first
desired | sed 's/^/   desired: /'
wait_until 300 "$fleet_hosts hosts Ready" hosts_ready "$fleet_hosts"
memory_snapshot "after the forge (toolbox peak = the forge)"
cimg=$(coordinator_image)
ports_seen=''
for h in $(host_names); do
    img=$(host_image "$h")
    [ "$img" = "$cimg" ] || fail "host $h runs image '$img', not the coordinator's '$cimg' (the image rule)"
    [ "$(desired_state "$h")" = wanted ] || fail "host $h's desired row is '$(desired_state "$h")', not wanted"
    slot=$(desired_slot "$h")
    case "$slot" in ''|*[!0-9]*) fail "host $h has no desired slot: $(desired | tr '\n' ';')" ;; esac
    ports=$(node_ports "$h")
    [ "$ports" = "$((port_base + 2 * slot)) $((port_base + 2 * slot + 1))" ] ||
        fail "host $h (slot $slot) has node ports '$ports', not $((port_base + 2 * slot)) $((port_base + 2 * slot + 1))"
    ports_seen="$ports_seen $ports"
    say "host $h: slot $slot, node ports $ports, image $img"
done
# shellcheck disable=SC2086 # one word per port
[ "$(printf '%s\n' $ports_seen | sort -n | tr '\n' ' ')" = "$(seq "$port_base" $((port_base + 2 * fleet_hosts - 1)) | tr '\n' ' ')" ] ||
    fail "the hosts' node ports are$ports_seen, not $port_base-$((port_base + 2 * fleet_hosts - 1)) (the ports k3d maps)"
added=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
[ "$added" = "$fleet_hosts" ] || fail "the coordinators' mmo_fleet_added_total is $added, not $fleet_hosts (0 before the forge)"
say "mmo_fleet_added_total $added0 -> $added; the reconciler created $(metric "$(fleet_pod)" mmo_fleet_created_total || echo '?') object(s)"
# The hosts' names from here on: ha the first, hb the second (phase 9 replaces hb by a new name, hc).
ha=$(host_names | sed -n 1p)
hb=$(host_names | sed -n 2p)
hostdeps=$(host_names | sed 's/^/host-/' | tr '\n' ' ')

step "2b. every host has a PodDisruptionBudget that allows no disruption"
for d in $hostdeps; do
    allowed=$(k get pdb "$d" -o jsonpath='{.status.disruptionsAllowed}') || fail "no PodDisruptionBudget $d"
    expected=$(k get pdb "$d" -o jsonpath='{.status.expectedPods}')
    [ "$allowed" = 0 ] || fail "PodDisruptionBudget $d allows $allowed disruption(s), not 0"
    [ "$expected" = 1 ] || fail "PodDisruptionBudget $d selects $expected pod(s), not its host's 1"
    say "PodDisruptionBudget $d: disruptionsAllowed 0, expectedPods 1"
done
if [ "$coordinators" -gt 1 ]; then
    min=$(k get pdb coordinator -o jsonpath='{.spec.minAvailable}') || fail "no PodDisruptionBudget coordinator"
    expected=$(k get pdb coordinator -o jsonpath='{.status.expectedPods}')
    allowed=$(k get pdb coordinator -o jsonpath='{.status.disruptionsAllowed}')
    [ "$min" = 1 ] || fail "PodDisruptionBudget coordinator has minAvailable '$min', not 1"
    [ "$expected" = "$coordinators" ] || fail "PodDisruptionBudget coordinator selects $expected pod(s), not the $coordinators replicas"
    [ "$allowed" = $((coordinators - 1)) ] || fail "PodDisruptionBudget coordinator allows $allowed disruption(s), not $((coordinators - 1))"
    say "PodDisruptionBudget coordinator: minAvailable 1, expectedPods $expected, disruptionsAllowed $allowed"
fi
# Record only: whether this k3s refuses an eviction dry run of a host pod by its budget.
pa=$(host_pod "$ha")
ev=$(printf '{"apiVersion":"policy/v1","kind":"Eviction","metadata":{"name":"%s","namespace":"%s"},"deleteOptions":{"dryRun":["All"]}}' "$pa" "$ns")
if said=$(printf '%s' "$ev" | k create --raw "/api/v1/namespaces/$ns/pods/$pa/eviction?dryRun=All" -f - 2>&1); then
    say "record: an eviction dry run of $pa was ALLOWED by the API server: $(printf '%s' "$said" | head -c 200)"
else
    say "record: an eviction dry run of $pa was refused: $(printf '%s' "$said" | tr '\n' ' ' | head -c 200)"
fi
[ "$(host_pod "$ha")" = "$pa" ] && pod_ready "$pa" || fail "host $ha's pod changed after the eviction dry run"

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
step "4. one ticker; the coordinator sees two live hosts and commits world cuts; one live row per host"
wait_until 60 "exactly one coordinator replica is the ticker (mmo_coordinator_is_ticker 1)" one_ticker
# every_replica_counts_all: every replica's live set holds every replica.
every_replica_counts_all() {
    local p n=0
    for p in $(coordinator_pods); do
        [ "$(metric "$p" mmo_coordinator_replicas_live || true)" = "$coordinators" ] || return 1
        n=$((n + 1))
    done
    [ "$n" = "$coordinators" ]
}
wait_until 60 "every replica counts $coordinators live replicas (mmo_coordinator_replicas_live)" every_replica_counts_all
say "the ticker is $(ticker_pod)"
two_live() { [ "$(ticker_metric mmo_hosts_live || true)" = 2 ]; }
wait_until 60 "the ticker's mmo_hosts_live 2" two_live
wait_until 60 "mmo_cuts_decided_total{outcome=committed} >= 1" committed_above 0
gen=$(coord_gen) || fail "no mmo_world_generation"
say "world generation $gen, committed cuts $(committed)"
rows | sed 's/^/   hosts: /'
id_a=$(live_id "$ha")
id_b=$(live_id "$hb")
[ -n "$id_a" ] && [ -n "$id_b" ] || fail "not exactly one live dirty row each for $ha and $hb: $(rows | tr '\n' ';')"
say "host $ha is id $id_a, host $hb is id $id_b"

# ---------------------------------------------------------------------------------
step "5. crash: host $hb's pod force-deleted"
pa=$(host_pod "$ha")
pb=$(host_pod "$hb")
r0=$(pod_restarts "$pa")
uid0=$(pod_uid "$pa")
uid1=$(pod_uid "$pb")
d0=$(coord_sum mmo_successions_declared_total) || fail "no mmo_successions_declared_total"
s0=$(coord_sum mmo_successions_settled_total) || fail "no mmo_successions_settled_total"
say "before: generation $gen, host $ha pod $pa restarts $r0, successions declared $d0 settled $s0"
# Decision 9's precondition (below): host $hb owns no land. A twin where a player made it claim some
# would see a Crash succession instead — this phase checks the landless end only.
land_b=$(row_land "$id_b")
[ "$land_b" = 0 ] || fail "phase 5 needs host $hb (id $id_b) landless, it owns land=$land_b: the landless-end check does not apply (a landed host's death is a Crash succession)"
t0=$SECONDS
k delete pod "$pb" --grace-period=0 --force > /dev/null 2>&1
# The new pod registers under a new id: Ready without waiting for the old row to be ended.
wait_until 60 "a new host $hb pod Ready" host_replaced_and_ready "$hb" "$uid1"
pb=$(host_pod "$hb")
say "new host $hb pod $pb Ready $((SECONDS - t0))s after the kill"
c1=$(committed) || fail "the coordinator does not answer after the crash"
left=$((60 - (SECONDS - t0)))
[ "$left" -gt 0 ] || left=1
wait_until "$left" "committed cuts grow again (above $c1), within 60 s of the kill" committed_above "$c1"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed: $gen -> $(coord_gen)"
[ "$(pod_uid "$pa")" = "$uid0" ] || fail "host $ha's pod was replaced"
[ "$(pod_restarts "$pa")" = "$r0" ] || fail "host $ha restarted ($r0 -> $(pod_restarts "$pa"))"
# Dead, or already pruned: the new pod's registration prunes a dead row owning nothing, and a pod that
# takes a while to start registers after the coordinator ended the old row (the log line is the proof).
# Any other state (live, or anything else a listing may show) is not an end.
old_row_gone_or_dead() {
    local listing s
    listing=$(rows)
    # Not vacuous: an empty listing (a failed read) proves nothing.
    [ -n "$listing" ] || return 1
    s=$(printf '%s\n' "$listing" | awk -v i="$id_b" '$1 == i { print $3 }')
    [ -z "$s" ] || [ "$s" = dead ]
}
wait_until 60 "the killed host's old row $id_b ended by the coordinator (dead or pruned)" old_row_gone_or_dead
# Decision 9: a host that dies owning no land (the twin has no player to own any) is ENDED, not succeeded.
# (The landed-crash path, a Crash succession, cannot run in the twin: no player claims land here.)
d1=$(coord_sum mmo_successions_declared_total) || fail "no mmo_successions_declared_total after the kill"
s1=$(coord_sum mmo_successions_settled_total) || fail "no mmo_successions_settled_total after the kill"
o1=$(coord_max mmo_successions_open) || fail "no mmo_successions_open after the kill"
say "after: generation $gen, host $ha untouched, successions declared $d0 -> $d1, settled $s0 -> $s1, open $o1"
[ "$d1" = "$d0" ] || fail "a landless host's death declared a succession: declared $d0 -> $d1 (decision 9: ended, none)"
[ "$o1" = 0 ] || fail "a succession is open after a landless host's death: $o1"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed after the landless host's death: $gen -> $(coord_gen)"
rows | sed 's/^/   hosts: /'
has_new_live_id "$hb" "$id_a" "$id_b" || fail "host $hb has no live row under a new id: $(rows | tr '\n' ';')"
id_b2=$(live_id "$hb")
old_row_gone_or_dead || fail "host $hb's old id $id_b is neither dead nor pruned (state '$(row_state "$id_b")'): $(rows | tr '\n' ';')"
say "host $hb is now id $id_b2; its old id $id_b is dead or pruned ('$(row_state "$id_b")')"
k logs "$pb" > "$out/host-$hb-after-crash.log" 2>&1 || true

# ---------------------------------------------------------------------------------
step "6. clean leave: host $ha scaled to 0 (SIGTERM), then back to 1"
pa=$(host_pod "$ha")
uid0=$(pod_uid "$pa")
d0=$(coord_sum mmo_successions_declared_total)
dep_uid=$(k get "deploy/host-$ha" -o jsonpath='{.metadata.uid}')
follow "$pa" "$out/host-$ha-leave.log"
sleep 2
t0=$SECONDS
k scale "deploy/host-$ha" --replicas=0 > /dev/null
wait_until 100 "the leaving host $ha pod gone" no_pods "app.kubernetes.io/component=host,errantry/host=$ha"
say "host $ha left in $((SECONDS - t0))s"
sleep 1
grep -qF "[server] left the world in cut" "$out/host-$ha-leave.log" ||
    fail "host $ha's log has no leave-cut line ($out/host-$ha-leave.log)"
grep -qF "left without a cut" "$out/host-$ha-leave.log" && fail "host $ha left without a cut ($out/host-$ha-leave.log)"
say "$(grep -F '[server] left the world in cut' "$out/host-$ha-leave.log" | tail -1)"
# The reconciler left the Deployment at 0 alone (same object, still at 0: it never scales, and its
# template is current); the wait above spans several of its passes.
[ "$(k get "deploy/host-$ha" -o jsonpath='{.metadata.uid} {.spec.replicas}')" = "$dep_uid 0" ] ||
    fail "the reconciler touched deploy/host-$ha at 0 replicas: $(k get "deploy/host-$ha" -o jsonpath='{.metadata.uid} {.spec.replicas}' || true) (was $dep_uid 0)"
k scale "deploy/host-$ha" --replicas=1 > /dev/null
wait_until 120 "a new host $ha pod Ready" host_replaced_and_ready "$ha" "$uid0"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed after a clean leave"
[ "$(coord_sum mmo_successions_declared_total)" = "$d0" ] || fail "a clean leave declared a succession"
say "generation $gen unchanged, no succession"
wait_until 30 "host $ha has a live row under a new id" has_new_live_id "$ha" "$id_a" "$id_b" "$id_b2"
id_a2=$(live_id "$ha")
listing=$(rows)
printf '%s\n' "$listing" | sed 's/^/   hosts: /'
# Not vacuous: an empty listing (a failed read) proves nothing.
[ -n "$listing" ] || fail "the hosts listing is empty: cannot check that host $hb's old id $id_b is gone"
if printf '%s\n' "$listing" | awk -v i="$id_b" '$1 == i { f = 1 } END { exit !f }'; then
    fail "host $hb's old id $id_b (dead, landless) is still a row after host $ha's registration: $(printf '%s' "$listing" | tr '\n' ';')"
fi
say "host $ha is now id $id_a2; host $hb's old id $id_b is gone (pruned at this registration); host $ha's old id $id_a: '$(row_state "$id_a")' (record)"

# ---------------------------------------------------------------------------------
step "7. one coordinator replica killed mid-play (the ticker's pod)"
pa=$(host_pod "$ha")
pb=$(host_pod "$hb")
ua=$(pod_uid "$pa") ra=$(pod_restarts "$pa")
ub=$(pod_uid "$pb") rb=$(pod_restarts "$pb")
# link_ip <host pod>: the coordinator replica IP the host's link is up to, from its last svc-link line
# about the coordinator (fails when that line is a drop, or there is none).
link_ip() {
    local text last
    text=$(k logs "$1" 2>/dev/null) || return 1
    last=$(printf '%s\n' "$text" | grep -E '\[svc-link\] (up to|link to) coordinator\.' | tail -1) || return 1
    case "$last" in
        *"[svc-link] up to coordinator."*) printf '%s\n' "$last" | sed -n 's/.*(\([0-9.]*\):6480).*/\1/p' ;;
        *) return 1 ;;
    esac
}
# A join through the login service, as a client makes it: an account's refresh token (issued in the
# login pod, never printed), POST /v1/join through the web's /login proxy. Each 200 rotates the token.
tok=$(kc -n login exec deploy/login -- login account issue-refresh twin-check@example.com 2> "$out/issue-refresh.err") ||
    fail "login account issue-refresh failed: $(head -3 "$out/issue-refresh.err")"
# The token is the last line: the login binary may log to stdout first (its migrations).
tok=$(printf '%s\n' "$tok" | grep . | tail -1 | tr -d '[:space:]' || true)
[ -n "$tok" ] || fail "login account issue-refresh printed no token"
join_said=''
join_ok() {
    local resp code body new
    resp=$(jq -nc --arg t "$tok" '{refresh_token: $t, world: "twin"}' |
        curl -s --max-time 20 -w '\n%{http_code}' -H 'Host: game.localhost' -H 'Content-Type: application/json' \
            --data-binary @- "http://127.0.0.1:8480/login/v1/join") || { join_said="curl failed"; return 1; }
    code=${resp##*$'\n'}
    body=${resp%$'\n'*}
    if [ "$code" != 200 ]; then
        join_said="$code $(printf '%s' "$body" | head -c 160)"
        return 1
    fi
    new=$(printf '%s' "$body" | jq -r '.refresh_token // empty' 2>/dev/null) || return 1
    [ -n "$new" ] || { join_said="200 without a refresh token"; return 1; }
    tok=$new
    join_said=200
}
join_or_fail() {
    local timeout="$1" what="$2" from=$SECONDS
    say "waiting up to ${timeout}s for: $what"
    until join_ok; do
        [ $((SECONDS - from)) -lt "$timeout" ] || fail "after ${timeout}s, still not: $what (last answer: $join_said)"
        sleep 2
    done
    say "ok after $((SECONDS - from))s: $what"
}
join_or_fail 60 "a join through the login service answers 200 ($coordinators replicas)"
wait_until 60 "exactly one ticker" one_ticker
victim=$(ticker_pod)
victim_uid=$(pod_uid "$victim")
victim_ip=$(pod_field "$victim" '{.status.podIP}')
survivors=$(coordinator_pods | grep -vxF "$victim" | tr '\n' ' ' || true)
[ -n "$survivors" ] || fail "no coordinator replica beside the ticker $victim"
ip_a=$(link_ip "$pa" || true)
ip_b=$(link_ip "$pb" || true)
say "the ticker is $victim ($victim_ip); the others: $survivors; host $ha's link is on ${ip_a:-?}, host $hb's on ${ip_b:-?}"
# Every replica's whole log until it ends (its `[coordinator] window=` lines: step 11).
followed=''
follow_coordinators() {
    local c
    for c in $(coordinator_pods); do
        case " $followed " in *" $c "*) continue ;; esac
        follow "$c" "$out/$c.until-end.log"
        followed="$followed $c"
    done
}
follow_coordinators
sleep 1
killed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
t0=$SECONDS
k delete pod "$victim" --grace-period=0 --force > /dev/null 2>&1
wait_until 30 "the killed replica's pod $victim gone" gone "$victim" "$victim_uid"
# The killed replica's counts left with it: cuts committed after the kill raise the sum over the
# pods there now (the survivors and the Deployment's replacement) above the survivors' own sum now.
base=0
for p in $survivors; do
    v=$(metric "$p" mmo_cuts_decided_total outcome=committed) || fail "the surviving replica $p does not answer"
    base=$((base + v))
done
say "killed $victim; the survivors' committed cuts: $base"
left_of() {
    local l=$(($1 - (SECONDS - t0)))
    [ "$l" -gt 0 ] || l=1
    echo "$l"
}
new_ticker() {
    local t
    t=$(ticker_pod)
    [ -n "$t" ] && [ "$t" != "$victim" ]
}
wait_until "$(left_of 90)" "another replica is the ticker (within 90 s of the kill)" new_ticker
say "the ticker is now $(ticker_pod)"
wait_until "$(left_of 90)" "cuts commit after the kill (above $base, within 90 s of it)" committed_above "$base"
# links_on_live: both hosts' links are up to a replica pod there now; a host that was on the killed
# replica logged its link up again since the kill.
links_on_live() {
    local ips p ip
    ips=" $(for p in $(coordinator_pods); do pod_field "$p" '{.status.podIP}'; echo; done | tr '\n' ' ') "
    for p in "$pa" "$pb"; do
        ip=$(link_ip "$p") || return 1
        case "$ips" in *" $ip "*) ;; *) return 1 ;; esac
    done
    { [ "$ip_a" != "$victim_ip" ] || log_has_since "$pa" "$killed_at" "world coordinator link up"; } &&
        { [ "$ip_b" != "$victim_ip" ] || log_has_since "$pb" "$killed_at" "world coordinator link up"; }
}
wait_until "$(left_of 90)" "both hosts' coordinator links up on a live replica" links_on_live
join_or_fail "$(left_of 90)" "a join answers 200 after the kill (within 90 s of it)"
say "host $ha's link is on $(link_ip "$pa" || echo '?'), host $hb's on $(link_ip "$pb" || echo '?')"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed after a replica's death: $gen -> $(coord_gen)"
[ "$(pod_uid "$pa")" = "$ua" ] && [ "$(pod_restarts "$pa")" = "$ra" ] || fail "host $ha's pod changed or restarted when a coordinator replica died"
[ "$(pod_uid "$pb")" = "$ub" ] && [ "$(pod_restarts "$pb")" = "$rb" ] || fail "host $hb's pod changed or restarted when a coordinator replica died"
wait_until 120 "the coordinator back at $coordinators replicas, every one Ready" coords_ready
wait_until 60 "every replica counts $coordinators live replicas" every_replica_counts_all
say "replicas $(coordinator_pods | tr '\n' ' '), generation $gen, hosts untouched"

# ---------------------------------------------------------------------------------
step "7b. every coordinator replica killed, then restarted"
pa=$(host_pod "$ha")
pb=$(host_pod "$hb")
ua=$(pod_uid "$pa") ra=$(pod_restarts "$pa")
ub=$(pod_uid "$pb") rb=$(pod_restarts "$pb")
follow_coordinators
sleep 1
k scale deploy/coordinator --replicas=0 > /dev/null
wait_until 90 "no coordinator pod left" no_pods app.kubernetes.io/component=coordinator
# The reconciler sees the coordinator Deployment at 0 within a pass: it creates nothing now.
fleet_stopped() { [ "$(readyz "$(fleet_pod)")" = stopped ]; }
wait_until 20 "the fleet reconciler's /readyz says stopped" fleet_stopped
restarted_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
t0=$SECONDS
k scale deploy/coordinator --replicas="$coordinators" > /dev/null
wait_until 120 "$coordinators coordinator replicas back, every one Ready" coords_ready
links_up() {
    log_has_since "$pa" "$restarted_at" "world coordinator link up" && log_has_since "$pb" "$restarted_at" "world coordinator link up"
}
wait_until "$(left_of 120)" "both hosts log 'world coordinator link up' after the start" links_up
both_hosts_ready() { host_ready "$ha" && host_ready "$hb"; }
wait_until "$(left_of 120)" "both hosts Ready (attached)" both_hosts_ready
wait_until "$(left_of 120)" "a cut commits after the restart" committed_above 0
wait_until 30 "exactly one ticker" one_ticker
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed after every replica restarted: $gen -> $(coord_gen)"
[ "$(pod_uid "$pa")" = "$ua" ] && [ "$(pod_restarts "$pa")" = "$ra" ] || fail "host $ha's pod changed or restarted when the coordinator restarted"
[ "$(pod_uid "$pb")" = "$ub" ] && [ "$(pod_restarts "$pb")" = "$rb" ] || fail "host $hb's pod changed or restarted when the coordinator restarted"
say "replicas $(coordinator_pods | tr '\n' ' '), ticker $(ticker_pod), generation $gen, hosts untouched"

# ---------------------------------------------------------------------------------
step "7c. land assigned, idle, released, re-assigned (the hosts' VOXEL_LAND_IDLE_MS=20000)"
pg_primary=$(kc -n postgres get pods -l cnpg.io/cluster=pg,cnpg.io/instanceRole=primary -o jsonpath='{.items[0].metadata.name}' 2> /dev/null || true)
[ -n "$pg_primary" ] || fail "no CloudNativePG primary pod of cluster pg (namespace postgres)"
# sql <query>: its rows, columns separated by '|' (psql in the primary, as the postgres superuser).
# psql's errors go to $out/sql.err (appended): a failing query is not read as "no row".
sql() { kc -n postgres exec "$pg_primary" -c postgres -- psql -d "$world_db" -v ON_ERROR_STOP=1 -Atc "$1" 2>> "$out/sql.err"; }
# owned_rows: the super_regions rows with an owner (not -1), `sx|sz|owner|epoch`, one per line.
owned_rows() { sql "select sx, sz, owner, epoch from super_regions where owner <> -1 order by sx, sz"; }
no_owned_land() {
    local r
    r=$(owned_rows) || return 1
    [ -z "$r" ]
}
# host_name_of <id>: the name on that id's hosts row ('' when there is none).
host_name_of() { rows | awk -v i="$1" '$1 == i { print $2 }'; }
# one_owned_live: exactly one owned row, its owner a live hosts row; sets land_row.
land_row=''
one_owned_live() {
    local r owner
    r=$(owned_rows) || return 1
    [ "$(printf '%s' "$r" | grep -c .)" = 1 ] || return 1
    owner=$(printf '%s' "$r" | cut -d'|' -f3)
    [ "$(row_state "$owner")" = live ] || return 1
    land_row=$r
}
# the_row: our row's `owner|epoch`.
the_row() { sql "select owner, epoch from super_regions where sx = $sr_x and sz = $sr_z"; }
# released_above <epoch> <host pod> <count>: our row is free at an epoch above the given one, and the
# host pod's mmo_land_released_total is above the given count.
released_above() {
    local r v
    r=$(the_row) || return 1
    [ "${r%%|*}" = -1 ] && [ "${r##*|}" -gt "$1" ] || return 1
    v=$(metric "$2" mmo_land_released_total) || return 1
    [ "$v" -gt "$3" ]
}
# owned_above <epoch>: our row is owned by a live host at an epoch above the given one.
owned_above() {
    local r
    r=$(the_row) || return 1
    [ "${r%%|*}" != -1 ] && [ "${r##*|}" -gt "$1" ] && [ "$(row_state "${r%%|*}")" = live ]
}
succ0=$(sql "select count(*) from successions") || fail "cannot read the successions rows of $world_db"
assigned0=$(coord_sum mmo_land_assigned_total) || fail "no coordinator replica answers mmo_land_assigned_total"
# Phase 7's joins placed the same player (no client connected): its land goes back first.
wait_until 210 "no super_regions row owned (phase 7's land released)" no_owned_land
join_or_fail 60 "a join through the login service answers 200"
t0=$SECONDS
wait_until 30 "exactly one super_regions row owned, by a live host (assigned)" one_owned_live
IFS='|' read -r sr_x sr_z owner1 epoch1 <<< "$land_row"
owner_name=$(host_name_of "$owner1")
owner_pod=$(host_pod "$owner_name")
[ -n "$owner_pod" ] || fail "no pod of host '$owner_name' (id $owner1), the owner of super-region ($sr_x, $sr_z)"
released0=$(metric "$owner_pod" mmo_land_released_total) || fail "host pod $owner_pod does not answer mmo_land_released_total"
say "assigned: super-region ($sr_x, $sr_z) to host $owner_name (id $owner1) at epoch $epoch1; its released total $released0"
# The placed spawn order pins the land for SPAWN_PIN_MS (120 s) waiting for its session; then
# the dial's 20 s idle; then the next world cut (save_secs 10) releases it: ~150 s.
wait_until 210 "super-region ($sr_x, $sr_z) free at an epoch above $epoch1 and host $owner_name's released total above $released0 (idle, released)" \
    released_above "$epoch1" "$owner_pod" "$released0"
r=$(the_row) || fail "cannot read super-region ($sr_x, $sr_z)"
epoch2=${r##*|}
say "released after $((SECONDS - t0))s: super-region ($sr_x, $sr_z) free at epoch $epoch2; host $owner_name released $(metric "$owner_pod" mmo_land_released_total || echo '?') in all"
join_or_fail 60 "a second join answers 200"
wait_until 30 "super-region ($sr_x, $sr_z) owned again, by a live host, at an epoch above $epoch2 (re-assigned)" owned_above "$epoch2"
r=$(the_row) || fail "cannot read super-region ($sr_x, $sr_z)"
say "re-assigned: super-region ($sr_x, $sr_z) to host $(host_name_of "${r%%|*}") (id ${r%%|*}) at epoch ${r##*|}"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed across the land's release and re-assignment: $gen -> $(coord_gen)"
succ1=$(sql "select count(*) from successions") || fail "cannot read the successions rows of $world_db"
[ "$succ1" = "$succ0" ] || fail "a succession row was added while land was assigned, released and re-assigned ($succ0 -> $succ1)"
assigned1=$(coord_sum mmo_land_assigned_total) || fail "no coordinator replica answers mmo_land_assigned_total"
[ "$assigned1" -ge $((assigned0 + 2)) ] || fail "the coordinators' mmo_land_assigned_total grew by less than 2 ($assigned0 -> $assigned1)"
say "generation $gen unchanged, successions rows $succ1 (unchanged), land assigned $assigned0 -> $assigned1; hosts' land: $(for h in $ha $hb; do printf '%s=%s ' "$h" "$(metric "$(host_pod "$h")" mmo_host_land || echo '?')"; done)"

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
step "9. drain host $hb (world-drain $hb 5): drained, its row and objects removed by themselves, a new name on its slot"
pa=$(host_pod "$ha")
pb=$(host_pod "$hb")
ua=$(pod_uid "$pa") ra=$(pod_restarts "$pa")
id_b2=$(live_id "$hb")
[ -n "$id_b2" ] || fail "host $hb has no live row before the drain: $(rows | tr '\n' ';')"
slot_b=$(desired_slot "$hb")
[ -n "$slot_b" ] && [ "$(desired_state "$hb")" = wanted ] || fail "host $hb has no wanted desired row before the drain: $(desired | tr '\n' ';')"
names_before=$(desired | awk '{ print $1 }' | tr '\n' ' ')
drained0=$(coord_sum mmo_fleet_drained_total) || fail "no coordinator replica answers mmo_fleet_drained_total"
removed0=$(coord_sum mmo_fleet_removed_total) || fail "no coordinator replica answers mmo_fleet_removed_total"
added0=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
say "host $hb is id $id_b2 (pod $pb, slot $slot_b); desired: $names_before; fleet drained $drained0, removed $removed0, added $added0"
t0=$SECONDS
wjust world-drain "$hb" 5 > "$out/world-drain.txt" 2>&1 || { sed 's/^/   /' "$out/world-drain.txt"; fail "world-drain $hb 5 failed"; }
sed 's/^/   /' "$out/world-drain.txt"
grep -qF "host $hb (id $id_b2) is drained" "$out/world-drain.txt" || fail "world-drain did not report host $hb drained"
say "world-drain returned after $((SECONDS - t0))s"
# Record: the drained pod says drained until the reconciler deletes it (which may already have happened).
say "record: host $hb's pod $pb /readyz: '$(readyz "$pb")'"
# The row follows the operator's mark (rule 2: DrainHost of the row), then, once the host left (land
# kept or not), the coordinator removes it (rule 3: RemoveHost) — one decision per 10 s at most —
# and the reconciler deletes the Deployment, the PDB and the Service within a pass.
drained_more() { local v; v=$(coord_sum mmo_fleet_drained_total) || return 1; [ "$v" -gt "$drained0" ]; }
wait_until 60 "the coordinators' mmo_fleet_drained_total above $drained0 (host $hb's desired row draining)" drained_more
say "host $hb's desired row: '$(desired_state "$hb")' (draining, or already removed)"
wait_until 120 "host $hb's desired row removed and its Deployment, Service and PDB deleted (no apply, no rollout restart)" host_gone "$hb"
removed=$(coord_sum mmo_fleet_removed_total) || fail "no coordinator replica answers mmo_fleet_removed_total"
[ "$removed" -gt "$removed0" ] || fail "host $hb is gone but the coordinators' mmo_fleet_removed_total did not grow ($removed0 -> $removed)"
say "host $hb removed $((SECONDS - t0))s after the drain began; fleet removed $removed0 -> $removed"
# Below the minimum again: the coordinator adds a NEW name (names are never reused) on the freed slot.
hc=''
third_name() {
    local n
    for n in $(desired | awk '$3 == "wanted" { print $1 }'); do
        case " $names_before " in *" $n "*) continue ;; esac
        hc=$n
        return 0
    done
    return 1
}
wait_until 120 "a third desired host name, wanted (the coordinator's minimum, $fleet_hosts)" third_name
[ "$(desired_slot "$hc")" = "$slot_b" ] || fail "the new host $hc got slot '$(desired_slot "$hc")', not the freed slot $slot_b: $(desired | tr '\n' ';')"
wait_until 120 "host $hc's pod Ready (its Deployment created by the reconciler)" host_ready "$hc"
[ "$(host_image "$hc")" = "$(coordinator_image)" ] || fail "host $hc runs image '$(host_image "$hc")', not the coordinator's '$(coordinator_image)'"
[ "$(node_ports "$hc")" = "$((port_base + 2 * slot_b)) $((port_base + 2 * slot_b + 1))" ] ||
    fail "host $hc has node ports '$(node_ports "$hc")', not slot $slot_b's $((port_base + 2 * slot_b)) $((port_base + 2 * slot_b + 1))"
added=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
[ "$added" -gt "$added0" ] || fail "host $hc exists but the coordinators' mmo_fleet_added_total did not grow ($added0 -> $added)"
say "host $hc on slot $slot_b (node ports $(node_ports "$hc")), Ready $((SECONDS - t0))s after the drain began; fleet added $added0 -> $added"
desired | sed 's/^/   desired: /'
rows | sed 's/^/   hosts: /'
[ "$(host_names | tr '\n' ' ')" = "$ha $hc " ] || fail "the host Deployments are '$(host_names | tr '\n' ' ')', not '$ha $hc '"
[ "$(pod_uid "$pa")" = "$ua" ] && [ "$(pod_restarts "$pa")" = "$ra" ] || fail "host $ha's pod changed or restarted during host $hb's drain"
c1=$(committed)
wait_until 60 "committed cuts grow (above $c1)" committed_above "$c1"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed across the drain and the new host"
hostdeps="host-$ha host-$hc"

# ---------------------------------------------------------------------------------
step "10. world-stop, then world-start"
pa=$(host_pod "$ha")
pc=$(host_pod "$hc")
follow "$pa" "$out/host-$ha-stop.log"
follow "$pc" "$out/host-$hc-stop.log"
follow_coordinators
sleep 2
wjust world-stop > "$out/world-stop.txt" 2>&1 || { sed 's/^/   /' "$out/world-stop.txt"; fail "world-stop failed"; }
sed 's/^/   /' "$out/world-stop.txt"
sleep 1
for h in "$ha" "$hc"; do
    grep -qF "[server] left the world in cut" "$out/host-$h-stop.log" || fail "host $h's log has no leave-cut line ($out/host-$h-stop.log)"
    grep -qF "left without a cut" "$out/host-$h-stop.log" && fail "host $h left without a cut ($out/host-$h-stop.log)"
    say "host $h: $(grep -F '[server] left the world in cut' "$out/host-$h-stop.log" | tail -1)"
done
[ "$(k get deploy/fleet -o jsonpath='{.spec.replicas}')" = 0 ] || fail "world-stop did not scale the fleet reconciler to 0"
[ "$(k get deploy/coordinator -o jsonpath='{.spec.replicas}')" = 0 ] || fail "the coordinator was not scaled to 0"
recorded=$(k get deploy/coordinator -o jsonpath='{.metadata.annotations.errantry/replicas}')
[ "$recorded" = "$coordinators" ] || fail "world-stop recorded '$recorded' coordinator replicas (annotation errantry/replicas), not $coordinators"
say "world-stop recorded $recorded coordinator replicas; the fleet reconciler at 0"
for d in $hostdeps; do
    [ "$(k get "deploy/$d" -o jsonpath='{.spec.replicas}')" = 0 ] || fail "$d was not scaled to 0"
done
no_pods 'app.kubernetes.io/component in (coordinator,host,fleet)' || fail "world pods left after world-stop"
wjust world-start > "$out/world-start.txt" 2>&1 || { sed 's/^/   /' "$out/world-start.txt"; fail "world-start failed"; }
sed 's/^/   /' "$out/world-start.txt"
world_started=$SECONDS
[ "$(k get deploy/coordinator -o jsonpath='{.spec.replicas}')" = "$coordinators" ] ||
    fail "world-start did not bring the coordinator back at $coordinators replicas: $(k get deploy/coordinator -o jsonpath='{.spec.replicas}')"
[ "$(k get deploy/fleet -o jsonpath='{.spec.replicas}')" = 1 ] || fail "world-start did not bring the fleet reconciler back to 1"
wait_until 30 "$coordinators coordinator replicas, the fleet reconciler and $fleet_hosts hosts Ready" all_ready
[ "$(host_names | tr '\n' ' ')" = "$ha $hc " ] || fail "the host Deployments after the start are '$(host_names | tr '\n' ' ')', not '$ha $hc '"
wait_until 60 "the coordinator commits a cut after the start" committed_above 0
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed across stop and start"
say "world back, generation $gen"
rows | sed 's/^/   hosts: /'
# A stray: a Deployment labelled for this world under a name no desired row holds (not a host by
# the recipes' label, so world-start would not scale it). The reconciler deletes it within a pass
# or two; nothing else moves.
pa=$(host_pod "$ha") pc=$(host_pod "$hc")
ua=$(pod_uid "$pa") uc=$(pod_uid "$pc")
deleted0=$(metric "$(fleet_pod)" mmo_fleet_deleted_total) || fail "the fleet pod does not answer mmo_fleet_deleted_total"
stray=$(printf '{"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"host-zz","labels":{"errantry/fleet":"%s","errantry/host":"zz"}},"spec":{"replicas":0,"selector":{"matchLabels":{"twin-check":"stray"}},"template":{"metadata":{"labels":{"twin-check":"stray"}},"spec":{"containers":[{"name":"host","image":"%s"}]}}}}' "$world_id" "$(coordinator_image)")
printf '%s' "$stray" | k create -f - > /dev/null || fail "could not create the stray Deployment host-zz"
say "created a stray Deployment host-zz (errantry/fleet=$world_id, errantry/host=zz, replicas 0)"
stray_gone() { [ -z "$(k get deploy --ignore-not-found host-zz -o name 2>/dev/null || echo failed)" ]; }
wait_until 15 "the reconciler deleted the stray host-zz (a pass every 2 s)" stray_gone
deleted=$(metric "$(fleet_pod)" mmo_fleet_deleted_total) || fail "the fleet pod does not answer mmo_fleet_deleted_total"
[ "$deleted" -gt "$deleted0" ] || fail "host-zz is gone but the fleet's mmo_fleet_deleted_total did not grow ($deleted0 -> $deleted)"
[ "$(pod_uid "$(host_pod "$ha")")" = "$ua" ] && [ "$(pod_uid "$(host_pod "$hc")")" = "$uc" ] || fail "a host pod changed while the stray was deleted"
[ "$(host_names | tr '\n' ' ')" = "$ha $hc " ] || fail "the host Deployments changed with the stray: '$(host_names | tr '\n' ' ')'"
say "stray deleted (fleet deleted $deleted0 -> $deleted); the hosts untouched"

# ---------------------------------------------------------------------------------
step "10b. the fleet reconciler's pod deleted mid-play: nothing running changes; it returns Ready"
pa=$(host_pod "$ha") pc=$(host_pod "$hc")
ua=$(pod_uid "$pa") ra=$(pod_restarts "$pa")
uc=$(pod_uid "$pc") rc=$(pod_restarts "$pc")
deps0=$(k get deploy -l app.kubernetes.io/component=host -o jsonpath='{range .items[*]}{.metadata.name}={.metadata.uid}/{.spec.replicas} {end}')
pf=$(fleet_pod)
uf=$(pod_uid "$pf")
c1=$(committed)
follow "$pf" "$out/fleet-delete.log"
sleep 1
k delete pod "$pf" --wait=false > /dev/null
wait_until 40 "the fleet pod $pf gone (its grace period is 30 s)" gone "$pf" "$uf"
grep -qF "[fleet] stop signal: exiting" "$out/fleet-delete.log" || say "record: no '[fleet] stop signal: exiting' line in its log ($out/fleet-delete.log)"
new_fleet_ready() { local p; p=$(fleet_pod); [ -n "$p" ] && [ "$p" != "$pf" ] && fleet_ready; }
wait_until 120 "a new fleet pod Ready (/readyz 200)" new_fleet_ready
wait_until 60 "committed cuts grow meanwhile (above $c1)" committed_above "$c1"
say "waiting 10 s: several passes of the new reconciler"
sleep 10
pf=$(fleet_pod)
created=$(metric "$pf" mmo_fleet_created_total) || fail "the new fleet pod does not answer mmo_fleet_created_total"
deleted=$(metric "$pf" mmo_fleet_deleted_total) || fail "the new fleet pod does not answer mmo_fleet_deleted_total"
[ "$created" = 0 ] && [ "$deleted" = 0 ] || fail "the new reconciler created $created and deleted $deleted object(s) in a world that needed nothing"
deps1=$(k get deploy -l app.kubernetes.io/component=host -o jsonpath='{range .items[*]}{.metadata.name}={.metadata.uid}/{.spec.replicas} {end}')
[ "$deps1" = "$deps0" ] || fail "the host Deployments changed across the reconciler's restart: '$deps0' -> '$deps1'"
[ "$(pod_uid "$pa")" = "$ua" ] && [ "$(pod_restarts "$pa")" = "$ra" ] || fail "host $ha's pod changed or restarted when the reconciler restarted"
[ "$(pod_uid "$pc")" = "$uc" ] && [ "$(pod_restarts "$pc")" = "$rc" ] || fail "host $hc's pod changed or restarted when the reconciler restarted"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed when the reconciler restarted"
say "new fleet pod $pf Ready, created 0, deleted 0, errors $(metric "$pf" mmo_fleet_errors_total || echo '?'); hosts untouched"

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
# Each replica's `[coordinator] window=` lines (the running replicas' logs, and the stopped ones'
# followed at world-stop): windows, transitions, conflicts and land assignments summed per file,
# into windows.txt.
for f in "$out"/coordinator-*.log; do
    [ -e "$f" ] || continue
    grep -F '[coordinator] window=' "$f" | awk -v f="$(basename "$f")" '
        { for (i = 1; i <= NF; i++) { split($i, kv, "="); v[kv[1]] = kv[2] }
          n++; t += v["transitions"]; c += v["conflicts"]; w += v["write_failures"]; a += v["assigned"]
          if (v["conflicts"] + 0 > cmax) cmax = v["conflicts"] + 0
          if (v["transitions"] + 0 > tmax) tmax = v["transitions"] + 0 }
        END { printf "   %s: %d windows, transitions %d (max %d per window), conflicts %d (max %d per window), write failures %d, assigned %d\n", f, n, t, tmax, c, cmax, w, a }' || true
done | tee "$out/windows.txt"

# ---------------------------------------------------------------------------------
step "12. to zero: VOXEL_FLEET_MIN=0, nobody online: the hosts drain one at a time and are removed"
succ0=$(sql "select count(*) from successions") || fail "cannot read the successions rows of $world_db"
# coord_pods_with_min <value>: exactly $coordinators replica pods, every one Ready and running
# VOXEL_FLEET_MIN=<value> (its pod spec: the Deployment's template it was made from).
coord_pods_with_min() {
    local p n=0
    for p in $(coordinator_pods); do
        [ "$(pod_field "$p" '{.spec.containers[?(@.name=="coordinator")].env[?(@.name=="VOXEL_FLEET_MIN")].value}')" = "$1" ] || return 1
        pod_ready "$p" || return 1
        n=$((n + 1))
    done
    [ "$n" = "$coordinators" ]
}
follow_coordinators
k set env deploy/coordinator VOXEL_FLEET_MIN=0 > /dev/null || fail "kubectl set env VOXEL_FLEET_MIN=0 on the coordinator failed"
say "the coordinator Deployment runs VOXEL_FLEET_MIN=0 now (Recreate: every replica restarts together)"
wait_until 150 "$coordinators coordinator replicas running VOXEL_FLEET_MIN=0, every one Ready" coord_pods_with_min 0
follow_coordinators
wait_until 30 "exactly one ticker" one_ticker
# The land of phase 7c's joins went back long ago (phase 11 waited five minutes); rule 7 also
# needs 0 running regions on the last host.
wait_until 210 "no super_regions row owned" no_owned_land
# desired_counts: `<all> <draining>` rows of desired_hosts (a failed read prints nothing).
desired_counts() { sql "select count(*), count(*) filter (where state = 1) from desired_hosts" | tr '|' ' '; }
# live_dirty_rows: the hosts rows live and dirty (state 0, clean 0).
live_dirty_rows() { sql "select count(*) from hosts where state = 0 and clean = 0"; }
names0=$(host_names | tr '\n' ' ')
say "hosts before: $names0; desired (all, draining): $(desired_counts || echo '?')"
t0=$SECONDS
drains=''
# One sample every 2 s until nothing is left: no desired row, no host Deployment, no live dirty
# hosts row. Each sample checks "one at a time": at most one desired row draining, and a row
# turns draining only while no other host's Deployment is still being removed.
while :; do
    counts=$(desired_counts) || counts=''
    if [ -n "$counts" ]; then
        read -r all draining <<< "$counts"
        [ "$draining" -le 1 ] || fail "$draining desired rows draining at once (one at a time): $(desired | tr '\n' ';')"
        for n in $(desired | awk '$3 == "draining" { print $1 }'); do
            case " $drains " in *" $n "*) continue ;; esac
            # A new drain: every earlier drained name is gone already.
            for d in $drains; do
                host_gone "$d" || fail "host $n drains while host $d (drained before it) still has a row or objects: $(objects_of "$d")"
            done
            drains="$drains $n"
            say "host $n drains ($((SECONDS - t0))s); desired (all, draining): $all $draining; hosts: $(host_names | tr '\n' ' ')"
        done
        live=$(live_dirty_rows || echo '?')
        if [ "$all" = 0 ] && [ -z "$(host_names)" ] && [ "$live" = 0 ]; then
            break
        fi
    fi
    [ $((SECONDS - t0)) -lt 480 ] ||
        fail "after 480s the world is not at zero hosts: desired (all, draining) '${counts:-?}', Deployments '$(host_names | tr '\n' ' ')', live dirty rows '$(live_dirty_rows || echo '?')', drains so far:$drains"
    sleep 2
done
say "zero hosts after $((SECONDS - t0))s; drained in order:$drains"
case "$drains" in *' '*' '*) ;; *) fail "the hosts went without two separate drains (drains seen:$drains)" ;; esac
rows | sed 's/^/   hosts: /'
coords_ready || fail "a coordinator replica is not Ready with zero hosts"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed going to zero hosts: $gen -> $(coord_gen)"
succ1=$(sql "select count(*) from successions") || fail "cannot read the successions rows of $world_db"
[ "$succ1" = "$succ0" ] || fail "a succession row was added going to zero hosts ($succ0 -> $succ1)"
# Still zero a little later (min 0: nothing adds a host without a login).
sleep 20
[ "$(desired_counts || echo '?')" = "0 0" ] && [ -z "$(host_names)" ] || fail "a host came back with nobody joining: desired '$(desired_counts || echo '?')', Deployments '$(host_names | tr '\n' ' ')'"
say "still zero 20 s later; replicas Ready; generation $gen, successions rows $succ1 (unchanged)"

# ---------------------------------------------------------------------------------
step "13. a join starts the world: 503 world_starting, a host is added, the join repeated every second gets in"
added0=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
join_ok && fail "a join answered 200 with zero hosts"
case "$join_said" in
    503*world_starting*) ;;
    *) fail "the first join with zero hosts answered '$join_said', not 503 world_starting" ;;
esac
first_503=$SECONDS
say "the first join: $join_said"
# one_wanted: exactly one desired row, wanted; sets hd.
hd=''
one_wanted() {
    local d
    d=$(desired)
    [ "$(printf '%s' "$d" | grep -c .)" = 1 ] || return 1
    [ "$(printf '%s' "$d" | awk '{ print $3 }')" = wanted ] || return 1
    hd=$(printf '%s' "$d" | awk '{ print $1 }')
}
wait_until 30 "one wanted desired row (the login asked for a host)" one_wanted
say "host $hd wanted $((SECONDS - first_503))s after the first 503"
# The join repeated every second (a client's REJOIN_RETRY_MS) until it answers 200; every other
# answer must be a 503 (world_starting while no host takes logins). Meanwhile: never a second
# desired row or host Deployment (the repeated joins add no second host).
answers=''
until join_ok; do
    case "$join_said" in
        503*world_starting*) answers="${answers}s" ;;
        503*) answers="${answers}u" ;;
        *) fail "a repeated join answered '$join_said' (only 503 or 200 expected)" ;;
    esac
    d=$(desired | grep -c . || true)
    [ "$d" -le 1 ] || fail "a second desired row while the joins repeated: $(desired | tr '\n' ';')"
    [ "$(host_names | grep -c . || true)" -le 1 ] || fail "a second host Deployment while the joins repeated: $(host_names | tr '\n' ' ')"
    [ $((SECONDS - first_503)) -lt 300 ] || fail "no 200 within 300s of the first 503 (last answer: $join_said; host $hd pod '$(host_pod "$hd")')"
    sleep 1
done
cold=$((SECONDS - first_503))
printf 'cold start (first 503 world_starting -> first 200): %s s; host %s; answers before it: %s (s = world_starting, u = another 503)\n' \
    "$cold" "$hd" "${answers:-none}" | tee "$out/cold-start.txt" | sed 's/^/   /'
host_ready "$hd" || say "record: host $hd's pod is not Ready yet at the first 200"
[ "$(desired | grep -c . || true)" = 1 ] || fail "not exactly one desired row after the join: $(desired | tr '\n' ';')"
[ "$(host_names | tr '\n' ' ')" = "$hd " ] || fail "the host Deployments are '$(host_names | tr '\n' ' ')', not '$hd '"
added=$(coord_sum mmo_fleet_added_total) || fail "no coordinator replica answers mmo_fleet_added_total"
[ "$added" = $((added0 + 1)) ] || fail "the coordinators' mmo_fleet_added_total went $added0 -> $added, not by exactly 1"
[ "$(coord_gen)" = "$gen" ] || fail "the world generation changed when the world started again: $gen -> $(coord_gen)"
succ1=$(sql "select count(*) from successions") || fail "cannot read the successions rows of $world_db"
[ "$succ1" = "$succ0" ] || fail "a succession row was added when the world started again ($succ0 -> $succ1)"
say "one host ($hd), fleet added $added0 -> $added, generation $gen, successions rows $succ1 (unchanged)"
# The twin's own dial back (the root's fleet.min); its replicas restart together again.
k set env deploy/coordinator VOXEL_FLEET_MIN="$fmin" > /dev/null || fail "kubectl set env VOXEL_FLEET_MIN=$fmin on the coordinator failed"
wait_until 150 "$coordinators coordinator replicas back on VOXEL_FLEET_MIN=$fmin, every one Ready" coord_pods_with_min "$fmin"

echo
echo "twin-check: PASS in $((SECONDS - started))s (transcript files: $out/)"
