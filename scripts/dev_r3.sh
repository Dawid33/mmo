#!/usr/bin/env bash
# `just dev-r3`'s supervisor: the recipe builds, resets the world clock and runs this.
# It starts the login service, the coordinator and every region host, each in a session of
# its own (setsid: a terminal Ctrl-C reaches only this script), then watches them:
#
#   - a host that exits 5 (EXIT_WORLD_ROLLBACK: the world went back to its last cut) or 137
#     (SIGKILL, e.g. `kill -9`) is started again after a back-off, at most
#     DEV_R3_RESTART_MAX times within DEV_R3_RESTART_WINDOW_SECS; any other exit, or one
#     restart too many, ends its loop — and never a restart once the stack is stopping;
#   - when any member ends on its own (login, the coordinator, a host loop) the stack goes
#     down and this script exits 1;
#   - Ctrl-C stops the stack in order: the hosts (their leave cut needs the coordinator),
#     then the coordinator, then login. Each stage has a deadline, after which what is left
#     is KILLed; a second Ctrl-C kills at once. Exit 0 if everything stopped in time, else 1.
#
#   scripts/dev_r3.sh <dir> <coordinator env words> <host 0 env words> [<host 1 env words> ...]
#
# Env words are `NAME=value` separated by spaces (no value holds a space). State in <dir>:
# coordinator.log, host-<n>.log, host-<n>.pid (while host n's server runs), `stopping`.
# Knobs (the defaults are what `just dev-r3` uses; tests drive the script with stubs):
#   DEV_R3_SERVER (./target/debug/server), DEV_R3_LOGIN (./target/debug/login),
#   DEV_R3_LOGIN_LOG (target/dev-login/login.log), DEV_R3_PIN_WAIT_SECS (180),
#   DEV_R3_HOST_STOP_SECS (45: the leave cut's 30 s timeout + margin), DEV_R3_STOP_SECS (60),
#   DEV_R3_RESTART_MAX (3), DEV_R3_RESTART_WINDOW_SECS (60), DEV_R3_RESTART_BACKOFF_SECS (1).
set -uo pipefail

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
host_stop_secs=${DEV_R3_HOST_STOP_SECS:-45}
stop_secs=${DEV_R3_STOP_SECS:-60}
restart_max=${DEV_R3_RESTART_MAX:-3}
restart_window=${DEV_R3_RESTART_WINDOW_SECS:-60}
restart_backoff=${DEV_R3_RESTART_BACKOFF_SECS:-1}

say() { echo "dev-r3: $*"; }

mkdir -p "$dir" "$(dirname "$login_log")"
pin=$dir/coordinator.cert-sha256
host_pin=$dir/host-0.cert-sha256
rm -f "$pin" "$host_pin" "$dir"/host-*.pid "$dir/stopping"

lpid=""
cpid=""
rpid=""
hpids=()
interrupted=0
escalate=0
failed=0

# wait_gone <deadline secs> <what> [pid...]: waits until every pid is gone; what is left at
# the deadline — or at once after a second Ctrl-C — is KILLed (and the stop counts as failed).
wait_gone() {
    local secs=$1 what=$2 p
    shift 2
    local pids=()
    for p in "$@"; do
        if kill -0 "$p" 2>/dev/null; then pids+=("$p"); fi
    done
    if [ "${#pids[@]}" -eq 0 ]; then return 0; fi
    say "waiting up to ${secs}s for $what (pids ${pids[*]}); Ctrl-C again kills at once"
    local end=$((SECONDS + secs))
    while :; do
        local left=()
        for p in "${pids[@]}"; do
            if kill -0 "$p" 2>/dev/null; then left+=("$p"); fi
        done
        if [ "${#left[@]}" -eq 0 ]; then return 0; fi
        pids=("${left[@]}")
        if [ "$escalate" = 1 ] || ((SECONDS >= end)); then
            say "killing $what (pids ${pids[*]})"
            kill -KILL "${pids[@]}" 2>/dev/null || true
            failed=1
            return 0
        fi
        sleep 0.2
    done
}

# The EXIT trap: stops whatever runs, in order. Exits 1 if the stack failed or a stage
# needed a KILL.
stop() {
    local code=$?
    trap - EXIT
    trap 'escalate=1' INT TERM
    touch "$dir/stopping"
    if [ -n "$rpid" ]; then kill "$rpid" 2>/dev/null || true; fi
    # Only servers whose loop still runs: a loop removes its pid file when it ends.
    local spids=() n f
    for n in "${!hpids[@]}"; do
        f=$dir/host-$n.pid
        if kill -0 "${hpids[$n]}" 2>/dev/null && [ -s "$f" ]; then spids+=("$(cat "$f")"); fi
    done
    if [ "${#spids[@]}" -gt 0 ]; then
        say "stopping the hosts (each saves a leave cut with the coordinator)"
        kill -TERM "${spids[@]}" 2>/dev/null || true
        wait_gone "$host_stop_secs" "the hosts" "${spids[@]}"
    fi
    if [ "${#hpids[@]}" -gt 0 ]; then wait_gone 5 "the host loops" "${hpids[@]}"; fi
    if [ -n "$cpid" ] && kill -0 "$cpid" 2>/dev/null; then
        say "stopping the coordinator"
        kill -TERM "$cpid" 2>/dev/null || true
        wait_gone "$stop_secs" "the coordinator" "$cpid"
    fi
    if [ -n "$lpid" ] && kill -0 "$lpid" 2>/dev/null; then
        say "stopping the login service"
        kill -TERM "$lpid" 2>/dev/null || true
        wait_gone "$stop_secs" "the login service" "$lpid"
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
trap 'interrupted=1' INT TERM

# Host <n> with <env words...>: started again on exit 5 or 137, bounded; never once stopping.
host_loop() {
    local n=$1
    shift
    local log=$dir/host-$n.log restarts=() rc spid now t
    while :; do
        env "$@" RUST_BACKTRACE=1 setsid "$server" >>"$log" 2>&1 </dev/null &
        spid=$!
        echo "$spid" >"$dir/host-$n.pid"
        if [ -e "$dir/stopping" ]; then kill -TERM "$spid" 2>/dev/null || true; fi
        rc=0
        wait "$spid" || rc=$?
        rm -f "$dir/host-$n.pid"
        if [ -e "$dir/stopping" ]; then exit "$rc"; fi
        case $rc in
            5 | 137) ;;
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
        say "host $n exited $rc (5: the world rolled back to its last cut; 137: killed): starting it again"
        sleep "$restart_backoff"
        if [ -e "$dir/stopping" ]; then exit "$rc"; fi
    done
}

VOXEL_SERVICE_TOKEN_FILE=$dir/service-token setsid "$login" serve >"$login_log" 2>&1 </dev/null &
lpid=$!
# Once host 0 has written its login-link pin: world `dev` gets the link.
(
    until [ -s "$host_pin" ]; do sleep 0.5; done
    "$login" world add dev --name "Dev world" --quic 127.0.0.1:6466 --wt https://127.0.0.1:6467/ \
        --link 127.0.0.1:6490 --link-cert "$(cat "$host_pin")" >/dev/null &&
        say "world dev registered with host 0's login link (127.0.0.1:6490)"
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
    ended=""
    rc=0
    wait -n -p ended "${hpids[@]}" "$cpid" "$lpid" || rc=$?
    if [ "$interrupted" = 1 ]; then exit 0; fi
    if [ -z "$ended" ]; then continue; fi
    what="?"
    if [ "$ended" = "$cpid" ]; then what="the coordinator ($dir/coordinator.log)"; fi
    if [ "$ended" = "$lpid" ]; then what="the login service ($login_log)"; fi
    for n in "${!hpids[@]}"; do
        if [ "$ended" = "${hpids[$n]}" ]; then what="host $n ($dir/host-$n.log)"; fi
    done
    say "$what ended ($rc): stopping the stack" >&2
    exit 1
done
