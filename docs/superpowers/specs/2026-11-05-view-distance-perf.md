# View distance and near-terrain performance (2026-11-05)

Handoffs: `docs/superpowers/plans/2026-11-05-view-distance-perf-sweep-handoff.md`,
`…-intel-fps-measurement-handoff.md`.

## Setup

- Git `04ba422` + the uncommitted tree of 2026-11-05. Release builds at 256 m regions
  (`VOXEL_REGION_BLOCKS=256`), 16-block chunks.
- Intel Iris Xe (ADL GT2, Mesa 25.2.8, Vulkan); `AdapterInfo` checked in every log.
  USB-C source 20 V × 6.5 A (130 W), battery full. Windowed 1280×720, vsync off.
- World: a 64 km Continent forged with its spawn moved to the middle of the land
  (`just forge-proto --size 64000 --cell 32 --spawn 32000,32000 --out target/world-forge/sweep64`),
  staged as `worlds/sweep64/` (92 % land within ±16 km; lowland forest at the spawn,
  ranges 8–15 km NE/E).
- **Embarked**, as the game is played: `client debug embarked` with
  `VOXEL_DEBUG_WORLD=sweep64 VOXEL_DEBUG_EMBARK_AT=0,0` — the fortress at region (0,0), so
  the 3×3 fortress neighbourhood (9 regions) is simulated. (A skip-menu client is
  un-embarked and simulates ONE region; numbers from it understate the game.)
- Camera: the commander rig facing the horizon over the canopy,
  `VOXEL_CAMERA_YAW_DEG=-45 VOXEL_CAMERA_PITCH_DEG=15 VOXEL_CAMERA_DISTANCE_M=150`.
  (The default pitch looks at the ground and hides far LOD; at the default 45 m distance
  a low pitch looks into the trees.)
- GPU per pass: Mesa `INTEL_MEASURE` over the last 150 frames of a 200 s run (after the
  near meshing has drained). Captures in `target/gpu-measure/<label>/` and
  `target/perf-capture/*-embarked-*`.

## Findings

1. **The 3×3 neighbourhood, not view distance, was the cost.** At a 1 km view the frame
   was ~53 ms, GPU-bound at 47.8 ms: main opaque pass 23.5 ms, far shadow cascade
   (to 300 m) 15.6 ms, cascades 1–3 6.5 ms, cull compute 1.2 ms.
2. **Geometry-bound**: a quarter of the pixels (`VOXEL_WINDOW=640x360`) only took the main
   pass 23.5 → 20.6 ms. Near terrain = **22.8M quads** in 4,346 sim chunks (~2.5M per
   256 m region: chiselled branches, half blocks, leaves), drawn at full detail out to
   ~400–700 m by the camera and again by the far cascade
   (`VOXEL_SHADOW_DISTANCE_M=150`: cascade 4 15.6 → 5.1 ms).
3. **Join**: near meshing of ~311k bricks takes 100–175 s; it is paced per frame, so slow
   frames slow it (feedback loop). The loading screen lifts on its 90 s timeout first.
   `world_ready` never fires embarked: the readiness gate expects the un-embarked LOD key
   count (`lod_roots=72/80` — 8 keys are fortress regions). Not fixed.
4. **View distance, 1 → 15 km** (before the fix, horizon view): frame +~9 ms,
   client RSS +0.4 GB (3.55 → 3.93 GB), LOD all received 7.7 s → 44 s, slowest LOD reply
   6 → 36 s. Server RSS ~1.0–1.3 GB, no send timeouts.

| step | keys | LOD all in | near drained | frame p50 | client RSS |
|---|---|---|---|---|---|
| V1 1 km | 72 | 7.7 s | 140 s | 55.0 ms | 3.55 GB |
| V2 2 km | 280 | 8.5 s | 148 s | 54.3 | 3.60 |
| V3 4 km | 1,080 | 12.0 s | 158 s | 58.0 | 3.69 |
| V4 6 km | 2,392 | 18.0 s | 166 s | 58.9 | 3.83 |
| V5 8 km | 2,905 | 22.9 s | 166 s | 65.1 | 3.86 |
| V6 12 km | 4,216 | 35.2 s | 175 s | ~62.6 (1 sample) | 3.97 |
| V7 15 km | 4,552 | 44.2 s | 175 s | ~63.9 (1 sample) | 3.93 |

(One run each; the repeat set was stopped to profile the near cost first.)

## Fix: coarse twins (`crates/client/src/renderer/meshing/coarse.rs`)

Each sim chunk with quads gets a twin: its blocks at block-scale cells
(`game::lod::block_scale_cells`) meshed by the LOD mesher, padded with its six
neighbours' boundary blocks (only opaque, unchiselled ones count), drawn as a child at
the LOD placement. The GPU cull (`voxel_pull_cull.wgsl` `band_visible`) draws the chunk
within `NEAR_DETAIL_DISTANCE_M` (96 m) of the MAIN camera's eye — horizontal distance to
the chunk's xz rect, which the twin shares — and the twin beyond it, in every view
including the shadow cascades. `VOXEL_NEAR_DETAIL_M=M` moves the switch, `0` turns it off.
CPU-culled backends skip twins. Twins are rebuilt with their chunk's mesh (joins, edits).

| 1 km view, embarked, horizon | before | twins |
|---|---|---|
| GPU / frame | 47.8 ms | **23.6 ms** |
| main pass | 23.5 | 10.3 |
| far cascade | 15.6 | 4.1 |
| cull compute | 1.2 | 3.2 (2× slots) |
| frame | ~53 ms | **28.8 ms** |
| near meshing drained | 146 s | 99 s |

Twins hold 1.13M quads against 22.8M full-detail. Screenshot diffs: ~1 % of pixels differ
strongly, all beyond ~100 m (solid foliage instead of cutout leaves, tree outlines and their
shadows); the switch line shows only as faint half-block risers, cut view included.

At the 15 km default: frame ~64 → ~41 ms, GPU 35.4 ms — the main pass is 22.1 ms, now
dominated by the far rings' **21.3M LOD quads** (4,607 tiles).

## Next levers (ranked)

1. **LOD ring quads** (21.3M at 15 km): the first ring at 256 m regions is factor 8
   (half-metre cells); factor 16 there roughly quarters it. Then the pixel budget
   (`LOD_MAX_CELL_PX` 2.5) for the outer rings, and whether LOD tiles are drawn into the
   shadow cascades at all beyond 300 m.
2. **Cull compute** 3 ms for ~13k slots × 5 views: skip slots by band before the 8-corner
   frustum test; one dispatch over all views.
3. **Shadow distance** 300 → ~150 m: −4 ms more at most now.
4. **Join**: the embarked `world_ready` gate (`regions=9/1 lod_roots=72/80`); near meshing
   paced by frame rate; LOD requests paced at 32 per 50 ms tick.

## Bench knobs added (native)

`VOXEL_DEBUG_WORLD=<slug>` / `VOXEL_DEBUG_EMBARK_AT=x,z` (`debug_launch.rs`),
`VOXEL_CAMERA_{PITCH_DEG,YAW_DEG,DISTANCE_M}` (`commander/mod.rs`), `VOXEL_WINDOW=WxH`
(`main.rs`), `VOXEL_NEAR_DETAIL_M`, the DIAG `[arena]` line (resident quads: near / with
twin / coarse twins / LOD), `forge-proto --spawn X,Z`. Haze: exponential-squared at the
same visibility (`atmosphere.rs`), clear near, still 2 % at the visibility distance.
