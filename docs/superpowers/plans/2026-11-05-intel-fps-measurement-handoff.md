# Handoff: measuring and improving client fps on the Intel iGPU (2026-11-05)

Purpose: repeat this work after the fog / LOD-distance increase regressed fps. This
records how the 2026-11-05 session measured the client, what it found, what it
changed, and the traps. Read it whole before launching anything.

## Ground rules (from the user)

- **Perf numbers come from builds at the play region size, 256 blocks** (`play_blocks`;
  the perf recipes below pass it). Tests (`smoke`, `test-one`) stay at 64.
- **Windowed only — never fullscreen.** On this desktop (Cinnamon / muffin) a window is
  paced to the 60 Hz refresh by the compositor even with vsync off, so ~16.7 ms is a
  floor. To see differences above that, **raise the load with `region_blocks=512`**
  (a bigger region = more near terrain to draw). Do not fly the camera far: it outruns
  the LOD distance around the home region and then nothing renders.
- **Drive the game in-process, never with xdotool / wmctrl / `import`.** Use env knobs,
  sim inputs / client packets (the autopilot, `renderer/autopilot.rs`, and the test
  harnesses are the models), and `VOXEL_SCREENSHOT` for captures.
- Use the Intel adapter (`adapter=Intel`; the RX 6700 eGPU is usually unplugged). Check
  the `AdapterInfo` line of every run. Plug the laptop in first (on battery it
  throttles; check `/sys/class/power_supply/BAT0/status` is not `Discharging`).
- Ask before running client unit tests (slow Bevy build). `just smoke` is the routine check.

## Tools (all in the repo)

| what | command | gives |
|---|---|---|
| GPU time per render pass, from the driver | `just gpu-measure <label> [secs]` | `scripts/gpu_measure.sh` runs release server+client with Mesa's `INTEL_MEASURE` (anv, `renderpass` granularity) and `scripts/gpu_measure_summary.py` prints mean GPU µs/frame and per render pass over the last 150 frames, plus DIAG frame time. Output `target/gpu-measure/<label>/`. |
| Windowed steady-state frame time | `just fps-steady <label> [KEY=VALUE …]` | `scripts/fps_steady.sh`: 60 s run, DIAG every 1 s, mean / median / p90 of frame_time after the first 25 s. Extra `KEY=VALUE` go to the client (A/B). Logs `target/fps-steady/`. |
| Whole picture + GPU busy % | `PERF_CAPTURE_SECONDS=60 just adapter=Intel perf-capture` | `summary.txt`: join timeline, RSS, **fdinfo GPU engine-busy %** (honest), DIAG verdict (NOT honest about GPU, see traps). |
| CPU per Bevy system | `perf record` on the `profiling` build, then `scripts/perf_systems.py` | ms/frame per system and per thread (recipe below). |
| CPU per system (spans) | `VOXEL_EXIT_AFTER_SECS=45 just adapter=Intel trace-chrome` | Chrome trace + `trace_summary.py` table. Heavy overhead (frames ~2× slower); use for attribution, not absolute numbers. |

CPU profile recipe (`perf_event_paranoid` is -1 here, no sudo needed; `just perf-client`
uses sudo and waits for a human, so do it by hand):

```sh
VOXEL_REGION_BLOCKS=256 nice -n 10 taskset -c 0-15 cargo build --bin client --bin server --profile profiling
export VOXEL_REGION_BLOCKS=256 VOXEL_SKIP_MENU=1 WGPU_ADAPTER_NAME=Intel
./target/profiling/server >/dev/null 2>&1 & sp=$!; sleep 1
DIAG=1 VOXEL_EXIT_AFTER_SECS=45 perf record -o perf.data -F 999 -D 20000 \
    --call-graph dwarf,16384 -e cpu-clock ./target/profiling/client > client.log 2>&1
kill $sp
perf script --no-inline -i perf.data | inferno-collapse-perf > folded.txt
scripts/perf_systems.py folded.txt <frames recorded ≈ 25 s / frame time> 40
scripts/perf_summary.py folded.txt --top 25      # per-symbol view
```

Client env knobs added for this work (native only):

- `VOXEL_EXIT_AFTER_SECS=N` — clean `AppExit` after N s (unattended runs; flushes traces).
- `VOXEL_SCREENSHOT=<secs>:<path>` — in-process PNG of the window (before/after image diffs:
  `compare -metric AE -fuzz 2% a.png b.png diff.png`; the minimap's moving dots always differ).
- `VOXEL_VSYNC=on|immediate|mailbox|fifo` — default is now `AutoNoVsync` (no vsync).
- `VOXEL_SHADOW_CASCADES=N`, `VOXEL_SHADOW_DISTANCE_M=M` — override the sun's cascades
  (`renderer/atmosphere.rs`) for A/B runs.
- Existing: `VOXEL_FACE_CULL=off` (no face-range culling), `VOXEL_OCCLUSION=on`, `DIAG=1`,
  `DIAG_INTERVAL`, `PERF_LOG=1`, `VOXEL_WORLD`, `VOXEL_LOD_RINGS`, `VOXEL_FORTRESS_RADIUS`.

## Method that worked

1. Baseline with `perf-capture` (frame time, fdinfo GPU busy %) **and** `gpu-measure`
   (GPU time per pass). If fdinfo says the GPU is busy but DIAG's per-pass sum is small,
   the time is in passes DIAG cannot see.
2. Name the passes in the `gpu-measure` table by order and draw type: `rp#0` cull compute +
   copies; then one `draw indexed indirect count` pass per **shadow cascade** (4, nearest
   first, each with a ~0.2 ms "slow depth clear"); then the **main opaque pass** (voxel
   forward shader + a cutout shader); then small passes (skybox/prepass), SMAA, tonemap,
   Slint composite (`draw` passes at the end). Shader hashes change per build.
3. Attack the biggest pass; re-run `gpu-measure` (GPU µs) and `fps-steady` (frame); verify
   the image with `VOXEL_SCREENSHOT` before/after (expect pixel-identical for a lossless cull).
4. When frame time stops following GPU time, the frame is CPU- or present-bound: take the
   `perf` CPU profile. Main + render thread together were only ~7.3 ms/frame at 256.
5. Removing work (`VOXEL_SHADOW_CASCADES=1 VOXEL_SHADOW_DISTANCE_M=20`) is a quick test of
   what bounds the frame: if fps does not move, that work was not the limit.

## Results (256-block build, default commander view, 1280×720 window, Iris Xe)

| stage | GPU ms/frame | frame |
|---|---|---|
| baseline (vsync, before the fix) | 22.5 — cascades 1.6 / 2.9 / 4.5 / 8.2, main 3.8 | 26–31 ms (32–38 fps) |
| after cull fix | 11.7 — cascades 0.9 / 1.4 / 1.9 / 3.4, main 2.5 | 16.3 ms (60, vsync) |
| no vsync, windowed | ~12.4 | ~17 ms — compositor floor |

CPU at that point (`perf_systems.py`): main+render threads ~7.3 ms/frame, compute pool ~6.1
(parallel). Biggest systems: `check_visibility_cpu_culling` 0.75 ms (59k entities),
`level_input` 0.33, `mark_dirty_trees` 0.25, Slint `slint_input` ×2 + `render_slint_layer`
~0.7 together, `prepare_clusters_for_gpu_clustering` 0.23. Not optimised yet.

## The change that halved GPU time

`assets/shaders/voxel_pull_cull.wgsl` + `crates/client/src/renderer/voxel_pull/gpu.rs`
(`GpuNearCullView::{camera, cut}`, `cull_camera`, `cull_cut`). The GPU cull emits one
indirect draw per face direction per chunk; it used to keep all 6 for every orthographic
view (shadow cascades, top-down) and for every view while the z-level cut was on (the
commander default). Now:

- `camera.w = 2` for orthographic views: xyz = direction toward the viewer (`back()`),
  face ranges whose normal points away are dropped (±`EDGE_ON` tolerance). Lossless: the
  depth pipeline already back-face culls; this only saves their vertex work.
- `cut` vec4 (home-region origin xz, extent, cut y): `cut_state` per chunk — wholly outside
  the home rect → face-cull as usual (the shaders discard back faces there anyway); wholly
  inside and above the cut → skipped (every quad collapses in the vertex shader); otherwise
  all faces (back faces below the cut are the cap).

Verified by screenshot diff (pixel-identical terrain and shadows).

## Traps found

- **`just diag` says "CPU-BOUND" when it is not.** Bevy 0.19's shadow pass records no GPU
  timestamp (nor do Slint composite / post), so DIAG's per-pass sum misses them. Trust
  fdinfo busy % (`perf-capture`) and `gpu-measure`.
- **Unpinned adapter = NVK T600 (8 fps on `just run`).** Fixed: `client::adapter_pick`
  ranks adapters before Bevy starts (discrete non-NVK > integrated > NVK > CPU) when
  `WGPU_ADAPTER_NAME` is unset or names an absent GPU.
- **Windowed fps floor ~16.7 ms** from the compositor, with any present mode. Raise load
  (512 regions) instead of changing window mode.
- `INTEL_MEASURE` adds some CPU overhead; use it for GPU µs, `fps-steady` for frame time.
- perf names Bevy's render thread after the process (`client`), so "client" = main + render.
- Frame-time samples are noisy (±3 ms per 2 s dump); compare means over ≥30 dumps.

## Leads for the fog / LOD-distance regression

Longer fog / LOD horizon = more LOD rings and more far geometry in every view:

1. Run `gpu-measure` before/after the distance change and diff per pass. If the **main
   pass** grew: LOD quads (count `quads=` in the `[perf-lod]` dump at exit) and the far
   ring factors (`game::lod::LOD_RING_TABLE`, `LOD_MAX_CELL_PX`). If the **cascades** grew:
   check whether LOD roots are drawn into shadow cascades at all; cascade 4 ends at
   `SHADOW_MAX_DISTANCE_M` (300 m) — far LOD inside a cascade's frustum is pure waste
   there (they may be candidates to exclude from shadow views, or to cull by distance).
2. Per-chunk GPU cull work (`rp#0` compute) and `check_visibility_cpu_culling` scale with
   entity count (LOD roots / tiles): watch `entity_count` in DIAG.
3. The cull's frustum test omits the far plane (infinite reverse-Z); fog-hidden geometry
   beyond `LOD_HORIZON_UNITS` is still drawn if it exists — a fog-distance cull in
   `voxel_pull_cull.wgsl` is cheap if the fog end is known per view.
4. Re-run the CPU profile: extract/prepare systems over LOD entities may dominate.

## State at handoff (all uncommitted on develop)

Mine: `voxel_pull_cull.wgsl`, `voxel_pull/gpu.rs`, `adapter_pick.rs` (+ `wgpu` dep, `lib.rs`,
`main.rs` call + vsync knob), `renderer/diag.rs` (exit / screenshot knobs) + `renderer/mod.rs`
wiring, shadow overrides in `atmosphere.rs`, `scripts/{gpu_measure.sh,gpu_measure_summary.py,
fps_steady.sh,perf_systems.py}`, justfile `gpu-measure` / `fps-steady`, CLAUDE.md notes.
The user's concurrent work in the same tree: frontend loading screen, fog / LOD distance
(`atmosphere.rs`, `game/src/lod.rs`, `world_scale.rs`). Unrun: client unit tests
(`just test-one client "--lib adapter_pick::"`, `"--lib voxel_pull::"` — shader validation).
