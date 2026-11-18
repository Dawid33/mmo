#!/usr/bin/env bash
# `just dev-r3`'s supervisor: the recipe builds, resets the world clock and runs this.
# It starts the login service (players log in with the dev Keycloak, `just kc-up`, which the
# recipe starts first: it reads VOXEL_OIDC_ISSUER from the recipe's environment), the
# coordinator and every region host, each in a session of
# its own (setsid: a terminal Ctrl-C reaches only this script), then watches them:
#
#   - a host that exits 5 (EXIT_WORLD_ROLLBACK: the world went back to its last cut) or 137
#     (SIGKILL, e.g. `kill -9`) is started again after a back-off — but under `claim` ownership
#     (what the hosts run) a killed host's land has moved to an heir by then and does NOT come
#     back by itself: the restarted process registers as a NEW host (a new id issued by the
#     store; its unchanged VOXEL_HOST_NAME lapses the old row at once) and owns nothing at first;
#     a host whose env words say VOXEL_OWNERSHIP=claim is also started again on exit 3
#     (EXIT_FENCED: it was replaced — a hung host that woke after its succession — and returns
#     as a new host owning nothing; under `static` exit 3 stays fatal: a second process of the
#     one static host would be refused or fence it); at most
#     DEV_R3_RESTART_MAX times within DEV_R3_RESTART_WINDOW_SECS; any other exit — 0, a host that
#     left cleanly, included — or one restart too many, ends its loop; never a restart once the
#     stack is stopping;
#   - when any member ends on its own (login, the coordinator, a host loop) the stack goes
#     down and this script exits 1;
#   - Ctrl-C (or TERM, or HUP: a closed terminal) stops the stack in order: every host
#     server (their leave cut needs the coordinator), then the coordinator, then login. Each
#     stage has a deadline, after which what is left is KILLed; a second signal kills at
#     once. Exit 0 only if everything stopped in time and is confirmed gone, else 1. Only
#     this script decides: the host loops ignore HUP / TERM / INT, and a pid is signalled
#     only while it is still the process started (its command line names the binary).
#
#   scripts/dev_r3.sh <dir> <coordinator env words> <host 0 env words> [<host 1 env words> ...]
#
# Env words are `NAME=value` separated by spaces (no value holds a space). State in <dir>:
# coordinator.log, host-<n>.log, host-<n>.pid (while the n-th host's server runs; n is the
# position on the command line, not a host id: ids come from the store), `stopping`, and the
# pins the coordinator writes (coordinator.cert-sha256, login-link.cert-sha256). The login
# service is registered with the coordinator's login link (VOXEL_LOGIN_LINK_BIND, 127.0.0.1:6490)
# once the coordinator has written login-link.cert-sha256.
# Knobs (the defaults are what `just dev-r3` uses; tests drive the script with stubs):
#   DEV_R3_SERVER (./target/debug/server), DEV_R3_LOGIN (./target/debug/login),
#   DEV_R3_LOGIN_LOG (target/dev-login/login.log), DEV_R3_PIN_WAIT_SECS (180),
#   DEV_R3_HOST_STOP_SECS (95: a host's own bound on its exit, see below), DEV_R3_STOP_SECS (60),
#   DEV_R3_RESTART_MAX (3), DEV_R3_RESTART_WINDOW_SECS (60), DEV_R3_RESTART_BACKOFF_SECS (1).
# Coordinator replicas (Elastic 2b; `just coordinators=2 dev-r3`): DEV_R3_REPLICAS is a `|`-separated
# list of env-word lists, one more coordinator each (its own VOXEL_COORDINATOR_BIND / VOXEL_LOGIN_LINK_BIND;
# the same certificate files as the first, which has written them by then), started once the first
# coordinator's pin exists, stopped with it, and any one ending takes the stack down like it.
# DEV_R3_LOGIN_LINK (127.0.0.1:6490) is the login link target world `dev` is registered with (the
# comma-separated addresses of every replica's login listener).
set -uo pipefail

# `wait -n -p` (the watch) needs bash 5.1.
if ((BASH_VERSINFO[0] < 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] < 1))); then
    echo "dev-r3: needs bash >= 5.1 (this is $BASH_VERSION)" >&2
    exit 2
fi

if [ $# -lt 3 ]; then
    echo "usage: $0 <dir> <coordinator env words> <host 0 env words> [<host n env words> ...]" >&2
    exit 2
fi
dir=$1
coord_env=$2
shift 2
host_envs=("$@")
server=${DEV_R3_SERVER:-./target/debug/server}
login=${DEV_R3_LOGIN:-./target/debug/login}
login_log=${DEV_R3_LOGIN_LOG:-target/dev-login/login.log}
pin_wait_secs=${DEV_R3_PIN_WAIT_SECS:-180}
# A host bounds its own exit (crates/server/src/cuts.rs `leave_world`): its leave cut must
# BEGIN within game::LEAVE_CUT_TIMEOUT_SECS (30) of its request, and one begun is given up
# game::WORLD_CUT_DEADLINE_MS (30 s) + server::EXIT_SAVE_TIMEOUT (30 s) after it began:
# 90 s at worst, + 5 s margin. KILLing a host before that leaves its row dirty (the world
# rolls back at its return); the usual leave takes ~3 s, and a second signal kills at once.
# (A leaver's land is given away at once, not after a wait; a crash's HOST_LEASE_MS 10 s and
# SUCCESSION_DEADLINE_MS 30 s run on the survivors, not on the stopping host.)
host_stop_secs=${DEV_R3_HOST_STOP_SECS:-95}
stop_secs=${DEV_R3_STOP_SECS:-60}
restart_max=${DEV_R3_RESTART_MAX:-3}
restart_window=${DEV_R3_RESTART_WINDOW_SECS:-60}
restart_backoff=${DEV_R3_RESTART_BACKOFF_SECS:-1}
login_link=${DEV_R3_LOGIN_LINK:-127.0.0.1:6490}
replica_envs=()
if [ -n "${DEV_R3_REPLICAS:-}" ]; then IFS='|' read -ra replica_envs <<<"$DEV_R3_REPLICAS"; fi

# Never fails (a closed terminal must not end a stop part-way).
say() { echo "dev-r3: $*" 2>/dev/null || true; }
self=$0

mkdir -p "$dir" "$(dirname "$login_log")"
pin=$dir/coordinator.cert-sha256
login_pin=$dir/login-link.cert-sha256
rm -f "$pin" "$login_pin" "$dir"/host-*.pid "$dir/stopping"

lpid=""
cpid=""
xpids=()
rpid=""
hpids=()
interrupted=0
escalate=0
failed=0

# ours <pid> <binary>: whether <pid> is alive and still the process we started from
# <binary> (its command line names it) — never a recycled pid, never a zombie.
ours() {
    local cmd
    cmd=$( { tr '\0' ' ' <"/proc/$1/cmdline"; } 2>/dev/null) || return 1
    [[ -n "$cmd" && "$cmd" == *"$2"* ]]
}

# wait_gone <deadline secs> <what> <binary> [pid...]: waits until no pid is ours any more;
# what is left at the deadline — or at once after a second signal — is KILLed (the stop
# then counts as failed), and one that outlives the KILL is reported.
wait_gone() {
    local secs=$1 what=$2 bin=$3 p
    shift 3
    local pids=()
    for p in "$@"; do
        if ours "$p" "$bin"; then pids+=("$p"); fi
    done
    if [ "${#pids[@]}" -eq 0 ]; then return 0; fi
    say "waiting up to ${secs}s for $what (pids ${pids[*]}); Ctrl-C again kills at once"
    local end=$((SECONDS + secs)) killed=0
    while :; do
        local left=()
        for p in "${pids[@]}"; do
            if ours "$p" "$bin"; then left+=("$p"); fi
        done
        if [ "${#left[@]}" -eq 0 ]; then return 0; fi
        pids=("${left[@]}")
        if [ "$killed" = 1 ] && ((SECONDS >= end)); then
            say "could not confirm $what gone (pids ${pids[*]})"
            failed=1
            return 0
        fi
        if [ "$killed" = 0 ] && { [ "$escalate" = 1 ] || ((SECONDS >= end)); }; then
            say "killing $what (pids ${pids[*]})"
            kill -KILL "${pids[@]}" 2>/dev/null || true
            failed=1
            killed=1
            end=$((SECONDS + 2))
        fi
        sleep 0.2
    done
}

# The host servers named by the remaining pid files that are still ours.
host_servers() {
    local f p
    for f in "$dir"/host-*.pid; do
        [ -s "$f" ] || continue
        p=$(cat "$f" 2>/dev/null) || continue
        if ours "$p" "$server"; then echo "$p"; fi
    done
}

# stop_hosts <what>: TERMs every host server still running and waits for them.
stop_hosts() {
    local spids=()
    mapfile -t spids < <(host_servers)
    if [ "${#spids[@]}" -gt 0 ]; then
        say "stopping $1 (each saves a leave cut with the coordinator)"
        kill -TERM "${spids[@]}" 2>/dev/null || true
        wait_gone "$host_stop_secs" "$1" "$server" "${spids[@]}"
    fi
}

# The EXIT trap: stops whatever runs, in order. Exits 1 if the stack failed or a stage
# needed a KILL or could not be confirmed.
stop() {
    local code=$?
    trap - EXIT
    trap 'escalate=1' INT TERM HUP
    trap '' PIPE
    touch "$dir/stopping"
    if [ -n "$rpid" ]; then kill "$rpid" 2>/dev/null || true; fi
    # Every host server, whether or not its loop still runs (a pid file outlives a loop
    # that was killed); the loops ignore signals and end once their server has.
    stop_hosts "the hosts"
    if [ "${#hpids[@]}" -gt 0 ]; then wait_gone 5 "the host loops" "$self" "${hpids[@]}"; fi
    # A loop may have started a server after the first look (a restart racing the stop).
    stop_hosts "the hosts started late"
    if [ -n "$cpid" ] && ours "$cpid" "$server"; then
        say "stopping the coordinator"
        kill -TERM "$cpid" 2>/dev/null || true
        wait_gone "$stop_secs" "the coordinator" "$server" "$cpid"
    fi
    if [ "${#xpids[@]}" -gt 0 ]; then
        say "stopping the other coordinator replicas"
        kill -TERM "${xpids[@]}" 2>/dev/null || true
        wait_gone "$stop_secs" "the other coordinator replicas" "$server" "${xpids[@]}"
    fi
    if [ -n "$lpid" ] && ours "$lpid" "$login"; then
        say "stopping the login service"
        kill -TERM "$lpid" 2>/dev/null || true
        wait_gone "$stop_secs" "the login service" "$login" "$lpid"
    fi
    if [ -n "$(host_servers)" ]; then
        say "host servers still running: $(host_servers | tr '\n' ' ')"
        failed=1
    else
        rm -f "$dir"/host-*.pid
    fi
    wait 2>/dev/null
    if [ "$code" -ne 0 ] || [ "$failed" = 1 ]; then
        say "stopped (failed)"
        exit 1
    fi
    say "stopped"
    exit 0
}
trap stop EXIT
trap 'interrupted=1' INT TERM HUP

# The <n>-th host with <env words...>: started again on exit 5 or 137 — and on 3 under `claim`
# ownership — bounded; never on 0 (a clean leave) and never once stopping.
host_loop() {
    local n=$1
    shift
    local log=$dir/host-$n.log restarts=() rc spid now t w claim=0
    for w in "$@"; do
        if [ "$w" = VOXEL_OWNERSHIP=claim ]; then claim=1; fi
    done
    # Only the supervisor decides when a host stops; the server gets default dispositions.
    trap '' HUP TERM INT
    while :; do
        (
            trap - HUP TERM INT
            exec env "$@" RUST_BACKTRACE=1 setsid "$server" >>"$log" 2>&1 </dev/null
        ) &
        spid=$!
        echo "$spid" >"$dir/host-$n.pid"
        if [ -e "$dir/stopping" ]; then kill -TERM "$spid" 2>/dev/null || true; fi
        rc=0
        wait "$spid" || rc=$?
        rm -f "$dir/host-$n.pid"
        if [ -e "$dir/stopping" ]; then exit "$rc"; fi
        case $rc/$claim in
            5/* | 137/* | 3/1) ;;
            *)
                say "host $n exited $rc; see $log"
                exit "$rc"
                ;;
        esac
        now=$SECONDS
        local recent=()
        for t in "${restarts[@]+"${restarts[@]}"}"; do
            if ((now - t < restart_window)); then recent+=("$t"); fi
        done
        restarts=("${recent[@]+"${recent[@]}"}")
        if [ "${#restarts[@]}" -ge "$restart_max" ]; then
            say "host $n exited $rc after $restart_max restarts within ${restart_window}s: giving up; see $log"
            exit "$rc"
        fi
        restarts+=("$now")
        say "host $n exited $rc (5: the world rolled back to its last cut; 137: killed; 3: replaced, under claim): starting it again"
        sleep "$restart_backoff"
        if [ -e "$dir/stopping" ]; then exit "$rc"; fi
    done
}

VOXEL_SERVICE_TOKEN_FILE=$dir/service-token setsid "$login" serve >"$login_log" 2>&1 </dev/null &
lpid=$!
# Once the coordinator has written its login-link pin: world `dev` gets the link (no addresses:
# a host's spawn ack names its endpoint).
(
    until [ -s "$login_pin" ]; do sleep 0.5; done
    "$login" world add dev --name "Dev world" \
        --link "$login_link" --link-cert "$(cat "$login_pin")" >/dev/null &&
        say "world dev registered with the coordinator's login link ($login_link)"
) &
rpid=$!
read -ra coord_words <<<"$coord_env"
env "${coord_words[@]}" RUST_BACKTRACE=1 setsid "$server" >"$dir/coordinator.log" 2>&1 </dev/null &
cpid=$!
say "login on http://127.0.0.1:8790 ($login_log), coordinator on 127.0.0.1:6480 ($dir/coordinator.log)"
deadline=$((SECONDS + pin_wait_secs))
until [ -s "$pin" ]; do
    if [ "$interrupted" = 1 ]; then exit 0; fi
    if ! kill -0 "$cpid" 2>/dev/null; then
        say "the coordinator exited; see $dir/coordinator.log" >&2
        exit 1
    fi
    if ! kill -0 "$lpid" 2>/dev/null; then
        say "the login service exited; see $login_log" >&2
        exit 1
    fi
    if ((SECONDS >= deadline)); then
        say "no coordinator pin after ${pin_wait_secs}s; see $dir/coordinator.log" >&2
        exit 1
    fi
    sleep 0.5
done
for n in "${!replica_envs[@]}"; do
    read -ra rwords <<<"${replica_envs[$n]}"
    env "${rwords[@]}" RUST_BACKTRACE=1 setsid "$server" >"$dir/coordinator-$((n + 1)).log" 2>&1 </dev/null &
    xpids[n]=$!
    say "coordinator replica $((n + 1)) ($dir/coordinator-$((n + 1)).log)"
done
for n in "${!host_envs[@]}"; do
    read -ra words <<<"${host_envs[$n]}"
    host_loop "$n" "${words[@]}" &
    hpids[n]=$!
    port=""
    for w in "${words[@]}"; do
        case $w in VOXEL_SERVER_PORT=*) port=${w#*=} ;; esac
    done
    say "host $n on UDP ${port:-?} ($dir/host-$n.log)"
done
# Watch: the first member to end on its own takes the stack down.
while :; do
    if [ "$interrupted" = 1 ]; then exit 0; fi
    ended=""
    rc=0
    wait -n -p ended "${hpids[@]}" "${xpids[@]}" "$cpid" "$lpid" || rc=$?
    if [ "$interrupted" = 1 ]; then exit 0; fi
    if [ -z "$ended" ]; then continue; fi
    what="?"
    if [ "$ended" = "$cpid" ]; then what="the coordinator ($dir/coordinator.log)"; fi
    if [ "$ended" = "$lpid" ]; then what="the login service ($login_log)"; fi
    for n in "${!hpids[@]}"; do
        if [ "$ended" = "${hpids[$n]}" ]; then what="host $n ($dir/host-$n.log)"; fi
    done
    for n in "${!xpids[@]}"; do
        if [ "$ended" = "${xpids[$n]}" ]; then what="coordinator replica $((n + 1)) ($dir/coordinator-$((n + 1)).log)"; fi
    done
    say "$what ended ($rc): stopping the stack" >&2
    exit 1
done
