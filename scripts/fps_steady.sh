#!/usr/bin/env bash
# Steady-state frame time of the WINDOWED release client (never fullscreen):
# release server + client on $WGPU_ADAPTER_NAME (default Intel), DIAG dumps every
# second, closed cleanly after FPS_SECONDS (default 60) via VOXEL_EXIT_AFTER_SECS;
# prints mean / median / p90 of the frame_time dumps after the first FPS_SKIP (25)
# — the join is over by then. Extra `KEY=VALUE` arguments go to the client only,
# for A/B runs (e.g. VOXEL_SHADOW_CASCADES=1, VOXEL_FACE_CULL=off).
#
#   scripts/fps_steady.sh <label> [KEY=VALUE ...]     (log: target/fps-steady/<label>.log)
#
# Invoked by `just fps-steady` (which builds first, at the play region size).
# Note: a window on this desktop (Cinnamon) is paced to the 60 Hz refresh by the
# compositor, so ~16.7 ms is a floor; to compare above it raise the load
# (`just region_blocks=512 fps-steady …`), not the window mode.
set -uo pipefail
label="${1:?usage: fps_steady.sh <label> [KEY=VALUE ...]}"; shift
mkdir -p target/fps-steady
log="target/fps-steady/$label.log"
export VOXEL_SKIP_MENU=1 DIAG=1 DIAG_INTERVAL=1
# Measuring: the player's vsync setting (on by default) must not cap the frame rate.
export VOXEL_VSYNC="${VOXEL_VSYNC:-off}"
: "${WGPU_ADAPTER_NAME:=Intel}"; export WGPU_ADAPTER_NAME
./target/release/server > "target/fps-steady/$label.server.log" 2>&1 &
server_pid=$!
trap 'kill "$server_pid" 2>/dev/null || true' EXIT
sleep 1
env "$@" VOXEL_EXIT_AFTER_SECS="${FPS_SECONDS:-60}" ./target/release/client > "$log" 2>&1
python3 - "$log" "$label" "${FPS_SKIP:-25}" <<'PY'
import re, statistics as st, sys
text = open(sys.argv[1], errors="ignore").read()
adapter = re.search(r'AdapterInfo \{ name: "([^"]+)"', text)
xs = [float(m.group(1)) for m in re.finditer(r"frame_time\s+:\s+([\d.]+)ms", text)][int(sys.argv[3]):]
if not xs:
    sys.exit(f"{sys.argv[2]}: no frame_time dumps in {sys.argv[1]}")
xs.sort()
print(f"{sys.argv[2]}: {adapter.group(1) if adapter else '?'}  n={len(xs)} mean={st.mean(xs):.2f} "
      f"median={st.median(xs):.2f} p90={xs[int(len(xs) * .9)]:.2f} ms  (~{1000 / st.mean(xs):.0f} fps)")
PY
