# View-distance performance sweep — handoff (2026-11-05)

**Goal:** measure what the long view distance costs, step by step from the old
1 km to the new 15 km default, find the knee, attribute every regression to a
system with evidence, and recommend (a) fixes and (b) a default view distance.
Deliver a findings doc `docs/superpowers/specs/2026-11-xx-view-distance-perf.md`.
Measure first; do not change defaults or tunables before the baseline sweep is
in. You have the user's go-ahead to run the benchmarks below.

## What changed (uncommitted on `develop`, 2026-11-05)

- `game::lod::LOD_FACTORS` = 2…512, doubling (128 and 512 added).
  `ring_table_for(horizon_regions)` builds a ring table by the pixel-budget
  rule (each ring the coarsest factor within `LOD_MAX_CELL_PX` = 2.5 px where it
  starts). Play-size default (`LOD_RING_TABLE` at 256 m regions) now reaches
  60 regions = 15,360 m (was 4 regions = 1 km, 80 keys).
- Client knob `VOXEL_VIEW_DISTANCE_M` / `just view=<m>` (`world_scale.rs`) builds
  the table for that distance; an explicit `VOXEL_LOD_RINGS` wins.
- Fog (`renderer/atmosphere.rs` `ViewDistance`): exponential haze, 2 % contrast
  at visibility = 0.8 × ring reach; `VOXEL_FOG_VISIBILITY_M` overrides the haze
  alone. Perspective far plane = ring reach (`sync_far_plane`). Fog itself is
  per-fragment ALU — not expected to cost anything; LOD volume is.
- Ground picks keep a 1 km reach (`selection::PICK_REACH_M`).
- `perf-capture` dirs are now named `…-view<VOXEL_VIEW_DISTANCE_M>`; the
  manifest records `VOXEL_VIEW_DISTANCE_M` / `VOXEL_FOG_VISIBILITY_M`.

## Ground rules (from CLAUDE.md + memory — read CLAUDE.md "Profiling" first)

- Work on `develop` in the main checkout. **No worktrees.** Don't commit unless asked.
- **Release only** (`perf-capture`, `diag`, `fly-probe`, `gpu-measure` build release).
  The dev profile measures the build, not the game.
- **Pin and confirm the GPU.** Default to `adapter=Intel` (the 6700 eGPU is often
  unplugged and the fallback is the ~30× slower NVK T600). Check the `AdapterInfo`
  line in every client log; discard runs on the wrong adapter. If the 6700 is
  present, repeat the key points on it (`adapter=6700`).
- **Check the power source** (`/sys/class/power_supply/ucsi-source-psy-*`) and
  `scaling_cur_freq` under load: on the ~88 W USB-C source the cores clamp to
  400–700 MHz. Record it; compare ratios within one session, never across.
- One server + one client. Vsync is off by default (`VOXEL_VSYNC=on` restores it),
  so fps shows headroom.
- Each step needs ≥2 runs; note variance. Same world, same spawn, same camera,
  same duration for every step.
- Builds go through `just` (CPU-capped). Running binaries are not capped.

## World to run on

The default dev world (`worlds/default.world`, 16 km) and `just run`'s debug world
(16 km) are too small: a 15 km view from spawn mostly looks at cheap ocean past the
world's sea edge and would UNDER-state cost. Forge a bigger one once and use it for
every run:

```sh
just forge-proto --size 64000 --cell 32 --out target/world-forge/sweep64
# then every recipe below with: world_file=target/world-forge/sweep64.world
```

Verify with `just region_blocks=256 world-map --world target/world-forge/sweep64.world`
that the spawn region has land in most directions to ≥15 km. If spawn is near an
edge, note it and say which direction the camera faces. (Also worth one 16 km-world
run at 15 km as the "small world" comparison point.)

## Configurations

Play size (256 m regions, the play recipes' default; `world=full`). Key counts
are for the commander's one-region near window, computed by the same rule as
`ring_table_for`:

| # | `view=` (m) | ring table `(bound regions, factor)` | LOD keys | haze visibility |
|---|---|---|---|---|
| V1 | `1024` | (2,8) (3,16) (4,32) | 80 | 819 m (old look ≈ linear 600–1024 m) |
| V2 | `2048` | … (6,32) (8,64) | 288 | 1.6 km |
| V3 | `4096` | … (12,64) (16,128) | 1,088 | 3.3 km |
| V4 | `6144` | … (12,64) (24,128) | 2,400 | 4.9 km |
| V5 | `8192` | … (24,128) (32,256) | 2,913 | 6.6 km |
| V6 | `12288` | … (47,256) (48,512) | 4,224 | 9.8 km |
| V7 | *(empty = default 15,360)* | … (47,256) (60,512) | 4,560 | 12.3 km |

Note the jump V3 → V4: the 4 m and 8 m rings (1.5–6 km) are region-keyed (one key
per 256 m region), so they hold ~2,200 of the keys. Factor 256/512 keys are tiles
of 2×2 / 4×4 regions and are cheap in count.

Controls (run at V7 and one low step, e.g. V2):

- **C1 fog-only:** `VOXEL_FOG_VISIBILITY_M=819` at V7 vs plain V1. Same haze, 57×
  the LOD, so this isolates LOD cost from anything the fog changes (expect fog ≈ 0).
- **C2 no LOD:** `world=tiny` (no rings). The floor everything else is measured against.
- **C3 server LOD pool:** `LOD_BUILD_THREADS=2`, `4`, `6` at V7: join-to-`lod_all_received`
  vs build parallelism (default is a quarter of the cores, clamped 2..=6).
- **C4 camera:** the default commander view looks down, so far rings may barely be on
  screen. Get a horizon-facing view too: `fly-probe` (first person, flies a line,
  rings re-subscribe on every seam crossing). Also look at the top-down view (V) once
  at V7 — the orthographic far plane is not the ring reach; check whether far LOD is
  still meshed/drawn for nothing there.
- **C5 web (optional, last):** `just web` + `?view=2048` / `?view=15360` in a browser,
  profiled with `just firefox-profile`. The WebGL2 path draws per-chunk meshes,
  one index buffer each, so LOD key count may hurt there first.

## Per configuration, run

Prefix every command with `just adapter=Intel world_file=target/world-forge/sweep64.world view=<m>`:

1. `PERF_CAPTURE_SECONDS=120 … perf-capture`. The main capture: join timeline, server
   counters, RSS, GPU engine time. Read `summary.txt`; keep the directory.
2. `… diag` for ~60 s steady state: CPU- vs GPU-bound verdict and the per-pass GPU table.
3. `… fly-probe 45`: crossing churn under far rings (`scripts/crossing_summary.py` output).
4. Only where a step regresses: `… trace-join` (join phase) / `… trace-chrome`
   (steady state, per-system self time), `… perf-server` / `… perf-client` (folded
   stacks + `perf_summary.py`), `… gpu-measure <label>` (Intel: per-pass GPU time,
   incl. the shadow cascades `diag` cannot time).

## What to extract per step (one row each in the findings table)

Join / streaming:
- `[perf]` client: `lod_requests_done`, `lod_all_received`, `world_ready`,
  `loading_screen_lifted` (t_ms), and any `main_stall` (count, max ms).
- Server `[server]` counters: LOD build time vs latency, LOD backlog, writer-task send
  time + `timeouts`, channel backlogs; LOD keys/s actually built.
- Bytes on the wire for LOD (payload sizes per factor if you can get them).

Steady state (after `lod_all_received`, camera still and panning):
- Client frame time p50 / p95 / max, fps; CPU- vs GPU-bound verdict.
- GPU ms per pass (opaque, shadow cascades, prepass if on); draw count; quads.
- `entity_count`; LOD roots / tiles live.
- Client and server RSS (`rss.csv`), GPU memory (`gpu.csv`), server `lod_cache` size.
- Panning: re-subscription rate (`RequestLod` / `ReleaseLod` per s), hitches.

Crossing (`fly-probe`): per-crossing worst frame, keys re-requested per crossing,
`region_crossing` stall numbers vs V1.

## Suspects to confirm or rule out (none measured)

- Client request pacing `LOD_REQUESTS_PER_TICK` = 32 per 50 ms tick
  (`crates/client/src/instance.rs`): 4,560 keys ≈ 7 s minimum to subscribe.
- QUIC stream-credit window (100 concurrent uni streams) and the server's shared
  writer (CLAUDE.md, WebTransport notes) under thousands of LOD replies. Watch
  `timeouts` and the dropped-packet gap (`send_bounded` 2 s `SEND_DEADLINE`, memory
  "netcode-dropped-packet-gap": a dropped packet can panic the client's event buffer).
- Server worker pool (`lod_workers.rs`) and `worldgen::coarse_tile` cost per key
  (factor 64 region key = 4,096 profile samples; 128/256/512 keys = 1,024), and
  whether the worldgen disk cache helps on a second run.
- Server `lod_cache` growth / memory (does it bound itself?).
- Client: `lod_rings.rs` decode + `meshing/lod_tiles.rs` merge per 8³-group tile, the
  meshing task pool, extract cost per LOD entity (an older note suspected
  `extract_chunk_quads` at 16k ChunkQuads/frame), GPU cull work per LOD chunk.
- GPU: far LOD in the shadow cascades (shadows reach only 300 m — are far tiles
  culled out of the cascade passes?), overdraw at grazing angles.
- Sticky band `LOD_STICKY_BAND` = 1 region: at 4 m/8 m rings, does panning one region
  re-request a whole ring edge each time?

## Deliverable

`docs/superpowers/specs/2026-11-xx-view-distance-perf.md`:
1. Setup (git rev, adapter, power source, world, durations).
2. The table above filled with measured columns (join times, frame p50/p95, GPU ms,
   RSS client/server, keys/s), V1…V7 + controls.
3. The knee: first step where frame time, join time or memory grows faster than
   key count, with the system responsible and the trace that shows it.
4. Ranked fixes with expected gain, and a recommended default view distance for the
   Intel iGPU and (if measured) the 6700.
Point to the capture directories under `target/perf-capture/` for every number.

## Known unrelated failure

`just region_blocks=256 test-one worldgen "--lib coarse"`:
`coarse_sea_surface_is_exact_at_every_factor` fails at factor 16 at the 256 m build
(its coast location is chosen for the 64 m test size). Pre-existing, not caused by
this change; it passes at the default test size.
