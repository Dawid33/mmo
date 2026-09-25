# World forge — F2 (engine side), first slice (2026-09-20)

Continues `../2026-09-20-world-forge-f1/findings.md`. F2's first slice makes the engine able to SERVE a
forged world, opt-in, without touching server/client/LOD code and without changing procedural output.
Renders next to this file were made through the engine's own sampling path (`just world-map --world …`).

## What landed
- **World file** (`worldgen::forged::WorldArtifact`, `<stem>.world`): `VXWF` + version + lz4(bincode).
  Rasters at the simulation resolution (height, lake level, `uplifted`, `bedding`), the rock column, river
  reaches (order, discharge, width, polyline with bed + water surface), lakes, and the **spawn site**.
  `f1-best.world` (16 km at 16 m cells): **8.9 MiB**. `from_bytes` rejects foreign/truncated files and
  other versions ("re-forge the world").
- **Forge → file**: `just forge-proto` now also writes the world file; `spawn.rs` picks the spawn (flat
  dry ground ±48 m, 3–45 m up, a river 60–300 m away, sea within ~1.5 km, a mountain within ~2.5 km) —
  a finite world has a site worth choosing; the procedural world had to scan seeds.
- **Engine sampling** (`crates/worldgen/src/forged.rs`): `ForgedWorld` = artifact + river-segment index
  (64 m buckets) + detail noise. Ground = Catmull-Rom bicubic of the raster + slope-scaled detail noise
  (32 m / 16 m, fades out at the coast). Per point `refine` adds the water features: lake water where the
  cell is lake and ground < level, a **berm** on the dry cells around a lake, river **channel** (parabolic
  bed, depth 0.5·√width clamped 0.8–3 m), a 2 m **levee** ring ≥ 0.3 m above the water, banks cut back at
  0.7. Water levels are **snapped to whole blocks before banks are measured** (a bank built to the raw
  level can end up under a surface that rounded up). Artifact spawn ↔ engine block (8, 8) of region (0, 0);
  outside the artifact = open ocean, so LOD rings past the rim are just sea.
- **One generator, two sources**: `generate_region` and `coarse_tile` were refactored onto a `Patch`
  (column profile + topmost water block per column). `ProceduralPatch` = the old behaviour bit-for-bit
  (all 37 lib tests incl. the coarse-vs-full comparisons pass); `ForgedPatch` = 4 m lattice of the forged
  ground (same corner rule ⇒ seamless region edges) + per-point refine with per-patch 8 m candidate cells.
  Underwater beds above sea level are sand. `VOXEL_WORLD_FILE=<path>` (or `just world_file=<path> …`)
  switches the process to the forged world; the worldgen disk-cache key includes the file crc.
- **Viewer**: `just world-map --world <file> [--span …]` renders through `ForgedWorld::sample`.
- **Tests** (`crates/worldgen/tests/forged_world.rs`, synthetic world with a river and a lake): file
  round-trip + rejection, spawn mapping + ocean outside, channel carved / whole-block water / banks hold,
  flat lake + shore berm, region determinism + **zero open water sides**, lake present in regions and in
  factor-16 LOD.

## Measured (release, this laptop)
- Real world, 120 river/lake regions: **50,688 fresh-water columns, 26 open sides (0.01%)**.
- `generate_region`: dry forged region p50 **82 ms** vs procedural 66 ms; river regions p50 **125 ms**
  (max 167) — was 257 ms before the per-patch candidate cells.
- Engine `Region::from_columns` on forged regions (`snapshot_size` with the env var): spawn region 0.42 s,
  1.05 MiB on the wire; a river + lake region 0.50 s, 2.06 MiB. `tests/player_walk.rs` passes on the
  forged spawn (land, walk, jump, bit-exact rollback).

## Known gaps (next slices)
- **Not run live**: no client/server session on a forged world yet (`just world_file=… run`). First things
  to look at: water rendering/meshing above sea level, LOD ring seams over lakes.
- **Far LOD water**: coarse cells are point-sampled — rivers narrower than a cell flicker in and out with
  distance; lake surfaces obey "any-solid rounds up" only through the block snap. The "water rounds down /
  dilated channel" rule in `game::lod` downsampling is not done.
- **Lake shores step in 16 m cells** where the lake mask, not the ground, ends the water (see
  `engine-lake-900m.png`); store a finer lake mask or the lake polygon.
- **Rivers are straight between D8 corners** (Chaikin only rounds them); no meanders; water steps down in
  1-block weirs with no waterfall geometry; tributaries join at the trunk's level by construction of the
  routing surface but channel depths differ.
- **Materials**: geology rasters + column are in the file but unused — needs rock blocks/textures, then
  `s = (y − uplifted) − bedding` lookup + slope-aware regolith in the material rule.
- **Resolution**: the artifact is the 16 m simulation grid; walking-scale relief between cells is bicubic +
  ±2 m noise. Multi-resolution refinement (8 m sim, 2–4 m raster) and gully detail are still open.
- The active world is a process global read from the environment (keeps every call site unchanged); the
  pod/super-region work will want it passed explicitly. wasm `?offline` has no way to get a file yet.

## Tuning page (added later the same day)
`just forge-ui [port]` → http://127.0.0.1:8787/ — every forge tunable as a slider with a live PNG preview.
- The constants that were scattered over the forge modules moved into ONE runtime table,
  `examples/forge_proto/knobs.rs` (42 knobs in 7 groups: Island, Uplift, Erosion, Geology, Climate, Shaping,
  Rivers; key, label, help, default, range). Defaults are unchanged — the winning seed forges the same
  statistics and score (0.587) as before the refactor. CLI: `--set <knob>=<value>`; rock contrast and rain
  shadow are now continuous strengths (`--no-geology`/`--no-climate` set them to 0).
- `serve.rs`: GET-only HTTP/1.1 on std::net, localhost only, no new dependencies. `/knobs`, `/forge`
  (returns statistics + score + an id; knob values clamped to their range), `/image?id=&view=` (PNG encoded
  in memory; last 4 worlds kept), `/export` (forges and writes `<out>-ui-<seed>.world`).
- `ui.html`: chart-table layout (map sheet + scale bar + margin notes; knob rail). Changed knobs are marked
  in chart magenta with a per-knob reset; log sliders for wide ranges; requests coalesce while a forge is
  in flight; press-and-hold the map for the previous render; values persist in localStorage; "Copy as
  flags" gives the matching `just forge-proto --seed … --set …` line.
- Measured: 64 m preview ≈ 1.3–1.5 s, 32 m ≈ 5 s, 16 m ≈ 30 s. Coarse previews keep the island's outline
  and drainage pattern but under-count lakes (2 at 64 m vs 8 at 16 m on the default seed) and flatten
  peaks — the page says so; judge lakes/relief at 16 m.
