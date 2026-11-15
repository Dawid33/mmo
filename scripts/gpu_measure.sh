#!/usr/bin/env bash
# GPU time per render pass from the Intel Vulkan driver itself (Mesa anv's
# INTEL_MEASURE), for the passes Bevy's RenderDiagnosticsPlugin cannot time —
# the shadow cascades, the Slint composite, post-processing. Release server +
# client on the Intel adapter, the client closed cleanly after <seconds>
# (VOXEL_EXIT_AFTER_SECS), DIAG on for the frame time.
#
#   scripts/gpu_measure.sh <out-dir> [seconds=45]
#
# Writes <out-dir>/{measure.csv,client.log,server.log} and prints
# scripts/gpu_measure_summary.py's table plus the client's last frame time.
# Invoked by `just gpu-measure` (which builds first). Intel GPUs only: other
# drivers ignore INTEL_MEASURE and the CSV never appears.
set -uo pipefail
out="${1:?usage: gpu_measure.sh <out-dir> [seconds]}"
secs="${2:-45}"
frames="${GPU_MEASURE_FRAMES:-150}"
rm -rf "$out"; mkdir -p "$out"
export DIAG=1 VOXEL_SKIP_MENU=1
: "${WGPU_ADAPTER_NAME:=Intel}"; export WGPU_ADAPTER_NAME
./target/release/server > "$out/server.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT
sleep 1
echo ">> GPU measure ($WGPU_ADAPTER_NAME, ${secs}s) -> $out"
# batch_size is allocated per command buffer: 262144 took the client past 11 GB
# within seconds of a 15 km world (OOM-killed, 2026-11-14); 8192 is enough.
INTEL_MEASURE="file=$out/measure.csv,renderpass,interval=1,batch_size=8192" \
    VOXEL_EXIT_AFTER_SECS="$secs" ./target/release/client > "$out/client.log" 2>&1
kill "$server_pid" 2>/dev/null; wait "$server_pid" 2>/dev/null
grep -a "AdapterInfo" "$out/client.log" | head -1 | sed 's/.*AdapterInfo/AdapterInfo/' | cut -c1-120
if [ ! -s "$out/measure.csv" ]; then
    echo "no $out/measure.csv — not an Intel (anv) adapter?" >&2; exit 1
fi
scripts/gpu_measure_summary.py "$out/measure.csv" "$frames"
grep -a "frame_time " "$out/client.log" | tail -1 | sed 's/.*frame_time/frame_time/'
grep -a " fps " "$out/client.log" | tail -1 | sed 's/.* fps/fps/'
