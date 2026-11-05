#!/usr/bin/env bash
# One self-describing performance capture: release server + client, everything a later
# analysis needs in ONE directory, plus a manifest saying exactly what was run.
#
#   target/perf-capture/<utc-timestamp>-<adapter>-<world>-view<VOXEL_VIEW_DISTANCE_M>/
#     manifest.json   provenance: git rev + dirty, profile, adapter asked for AND selected,
#                     world scale, render backends, thread counts, host, durations, exit codes
#     client.log      full client log: [perf] join timeline, [client-budget], DIAG dumps
#     server.log      full server log: [perf] join timeline, [server] counters, [region] lines
#     rss.csv         1 Hz VmRSS / VmHWM / threads / CPU time for both processes
#     gpu.csv         1 Hz client GPU engine time + GPU memory (DRM fdinfo), sysfs GPU counters
#     diag.txt        scripts/diag_summary.py verdict (CPU-bound vs GPU-bound + pass table)
#     summary.txt     scripts/perf_capture_report.py: READ THIS FIRST
#
# Invoked by `just perf-capture` (which does the pinned release build first); the binaries
# themselves are launched directly and never pinned or niced, so the numbers are real.
# Close the client window to end the capture — or set PERF_CAPTURE_SECONDS=<n> to have the
# script close it after n seconds (SIGTERM; fine for logs, but see `just trace-join` if you
# need a Chrome trace, which only flushes on a clean exit).
set -uo pipefail

adapter="${1:-}"
world="${2:-}"
profile="${PERF_CAPTURE_PROFILE:-release}"
bin_dir="target/$profile"

# Same presets `crates/client/src/world_scale.rs` accepts; checked here so a typo fails
# before a capture directory, a server and a 1-second "capture" of a panic exist.
case "$world" in
    ""|tiny|small|full) ;;
    *) echo "perf-capture: invalid world '$world' (expected tiny, small, or full; empty = full)" >&2; exit 2 ;;
esac

slug() { local s="${1:-}"; s="${s//[^[:alnum:]._-]/_}"; echo "${s:-$2}"; }
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="target/perf-capture/${stamp}-$(slug "$adapter" default)-$(slug "$world" full)-view$(slug "${VOXEL_VIEW_DISTANCE_M:-}" default)"
mkdir -p "$out"

if [ -n "$adapter" ]; then export WGPU_ADAPTER_NAME="$adapter"; else unset WGPU_ADAPTER_NAME || true; fi
# DIAG=1 (per-pass GPU timestamps) is part of the capture by default; PERF_CAPTURE_DIAG=0
# drops it when the timestamp queries themselves are under suspicion.
if [ "${PERF_CAPTURE_DIAG:-1}" != "0" ]; then export DIAG=1; fi
# The game-thread budget line is logged at DEBUG; PERF_LOG turns it (and only it) on
# without drowning the log in every other crate's debug output. See `log_filter` in crates/client/src/main.rs.
export PERF_LOG=1
export RUST_BACKTRACE=1

started_epoch="$(date +%s.%N)"
"$bin_dir/server" > "$out/server.log" 2>&1 &
server_pid=$!
sleep 1
"$bin_dir/client" > "$out/client.log" 2>&1 &
client_pid=$!

# 1 Hz resource sampler (no root): rss.csv = RSS / threads / CPU ticks for both
# processes; gpu.csv = the client's own GPU engine time + GPU memory from DRM fdinfo,
# plus whole-GPU sysfs counters where the driver has them. Exits with the client.
scripts/perf_sample.py "$out" "client:$client_pid" "server:$server_pid" &
sampler_pid=$!

cleanup() {
    kill "$client_pid" 2>/dev/null || true
    kill "$server_pid" 2>/dev/null || true
    kill "$sampler_pid" 2>/dev/null || true
}
trap cleanup EXIT

echo ">> perf capture -> $out"
if [ -n "${PERF_CAPTURE_SECONDS:-}" ]; then
    echo ">> the client will be closed after ${PERF_CAPTURE_SECONDS}s"
    ( sleep "$PERF_CAPTURE_SECONDS"; kill "$client_pid" 2>/dev/null ) &
else
    echo ">> let the world load, play ~30s, then CLOSE THE WINDOW"
fi
wait "$client_pid"; client_rc=$?
ended_epoch="$(date +%s.%N)"
kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null; server_rc=$?
wait "$sampler_pid" 2>/dev/null || true
trap - EXIT

scripts/diag_summary.py "$out/client.log" --timeline > "$out/diag.txt" 2>&1 || true
scripts/perf_capture_report.py "$out" \
    --adapter-requested "$adapter" --world-requested "$world" --profile "$profile" \
    --started "$started_epoch" --ended "$ended_epoch" \
    --client-rc "$client_rc" --server-rc "$server_rc"
echo
cat "$out/summary.txt"
echo
echo ">> capture directory: $out   (start with summary.txt)"
