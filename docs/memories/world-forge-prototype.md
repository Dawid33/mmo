---
name: world-forge-prototype
description: F0 + F1 + F2-first-slice DONE 2026-09-20 (uncommitted; engine can SERVE a forged world via VOXEL_WORLD_FILE, live session NOT run) — offline whole-world landscape-evolution "forge" for 16 km ocean-bounded super regions; `just forge-proto [--search N]`; geology-before-erosion, orographic rain, valley floors, spill-level lakes, river/lake objects, seed search; next = F2 engine artifact; don't re-prototype
metadata:
  type: project
---

User decisions 2026-09-20: super region = **16 km, ocean all round**, each pre-generated offline as its
own world (see [[df-worldgen-research]] for the direction change and why).

**F0 prototype:** `crates/worldgen/examples/forge_proto.rs` (`just forge-proto [--cell 32 --iters 200]`,
output `target/world-forge/`), standalone — no engine code or world output touched. Findings + small
renders: `docs/superpowers/specs/2026-09-20-world-forge-f0/findings.md`. Method: noise island + uplift
map with mountain belts → per iteration Priority-Flood+ε (one pass = routing surface + receivers +
downstream-first order) → flow accumulation → implicit stream-power incision (n=1) → depression silting
→ hillslope diffusion; lakes = overdeepened bowls excavated on valley floors, flooded to true spill level.

**Measured:** 32 m 4.8 s / 16 m 23 s / 8 m 126 s single-threaded; slope–area θ 0.62–0.66, Hack 0.52–0.55
(natural range; noise terrain θ = 0.05); 33% land, ~31 river mouths, longest ~8 km, 5–8 lakes; max slope
0.6 (all walkable — cliffs must come from geology). **Gotcha found:** relief is grid-dependent (8 m peaks
45% taller than 16 m) unless incision uses `max(A, HILLSLOPE_AREA_M2 = 6000)`; with it slopes are
resolution-independent, peaks still creep 188→228→251 m. Relief ∝ U/K (6.5e-4 / 2.5e-5 → ~230 m).

**Known gaps → F1 (standalone):** geology-before-erosion erodibility + strata, orographic rain →
rainfall-weighted discharge, sediment deposition (floodplains/deltas), river/lake object extraction,
generate-and-test. **F2 (engine):** artifact format (1–2 m height via multi-res refinement, water
surfaces, river graph), `generate_region`/`coarse_tile` sampling it, 1 m channel carve, elevated voxel
water + LOD "water rounds down", world-edge/LOD clipping. Visible artifacts: D8 right-angle channels,
planar "tent" hillslopes at walking scale. Open: wasm `?offline` mode's world source.

**F1 DONE same day (user said "continue"):** the example is now a multi-module dir
`crates/worldgen/examples/forge_proto/` (world, geology, climate, erosion, post, hydrology, render,
search, main); findings + renders: `docs/superpowers/specs/2026-09-20-world-forge-f1/findings.md`.
Added: rock column in a stratigraphic coordinate `s = (y − uplifted) − bedding` (uplift domes the strata,
granite cores ringed by bands; erodibility = rock the surface currently cuts → scarps, max slope 0.6→~1.2);
orographic rain weighting discharge; height-band fit (scale if peak > 236 m); planed valley floors; rivers
(Strahler order, width, smoothed polylines, monotone water surface) + lakes (level, outlet) as objects →
`*-water.json`; `--search N` generate-and-test at 32 m (64 seeds in 74 s, winner 0xD5C0DE021A: 13 km
river, 9 lakes). **Lessons:** channel-initiation threshold must be AREA-based, not rain-weighted discharge
(else the lee side goes smooth); rain defaults 9 km depletion / 650 m orographic scale / clamp 0.4–3×;
coarse search predicts land/rivers/lakes but NOT peak height (−30%) or basin size → fit relief, re-score
top few at target res; relief never reaches steady state with layered rock (±30 m swings).
**Next = F2 (engine):** artifact rasters at 1–2 m + uplifted/bedding/column for rock-at-(x,y,z) + water
objects; generate_region/coarse_tile sample it; 1 m channel carve; strata banding + regolith materials
(new blocks); elevated voxel water + LOD "water rounds down"; world edge; wasm offline world source.

**F2 first slice DONE same day (user said "continue"):** engine serves forged worlds, opt-in, zero edits
to server/client/LOD code. `crates/worldgen/src/forged.rs`: `WorldArtifact` file (`VXWF`+version+lz4
bincode; f1-best.world = 8.9 MiB), `ForgedWorld` (bicubic ground + detail noise, river-segment buckets,
per-point `refine`: lake water + shore berm, parabolic channel, 2 m levee, bank cut; water levels SNAPPED
to whole blocks before banks are measured), `active()` = process global from `VOXEL_WORLD_FILE`
(`just world_file=<path> run`; unreadable file panics; disk-cache key includes file crc). `lib.rs`:
`generate_region`/`coarse_tile` refactored onto a `Patch` trait (profile + topmost water block per
column) — procedural path bit-identical (37 lib tests pass). Forge writes `<stem>.world` + picks a spawn
site (`examples/forge_proto/spawn.rs`). `just world-map --world <file>` renders via the engine path.
Tests: `crates/worldgen/tests/forged_world.rs` (6). Measured: 50,688 fresh-water columns / 26 open sides
(0.01%) on the real world; dry forged region 82 ms vs procedural 66, river region 125 ms (257 before
per-patch 8 m candidate cells); Region::from_columns fine (0.42–0.50 s); player_walk passes on the forged
spawn. Findings: `docs/superpowers/specs/2026-09-20-world-forge-f2/findings.md`.
**NOT done:** live client/server run on a forged world (first check: water meshing above sea level, LOD
seams over lakes); LOD "water rounds down"/dilated channels; 16 m stepped lake shores; meanders/waterfalls;
rock materials (geology is in the file, unused — needs blocks/textures); finer artifact resolution;
explicit (non-global) world handle for pods; wasm offline world source.

**Tuning page (same day, user asked for a webapp to iterate on knobs):** `just forge-ui [port]` →
http://127.0.0.1:8787/. All forge tunables moved from scattered consts into ONE runtime table
`examples/forge_proto/knobs.rs` (42 knobs, 7 groups; defaults = the tuning; CLI `--set knob=value`;
geology/climate are continuous strengths now). `serve.rs` = std-only GET HTTP on localhost (/knobs,
/forge, /image, /export); `ui.html` = chart-table page (map + margin-note stats + knob rail, changed knobs
in magenta, hold-to-compare previous render, copy-as-flags, save world file at 16 m). Preview 64 m ≈ 1.5 s
but under-counts lakes/peaks vs 16 m. Refactor verified output-identical (winner seed score 0.587).
Gotchas: `target/world-forge/` and `target/world-map/` got emptied by something other than me mid-session
(renders had been moved into docs/…/world-forge-f2/) — world files in target/ are NOT durable, re-forge
with `just forge-proto --seed D5C0DE021A --out target/world-forge/f1-best`; never `pkill -f` a pattern
that appears in your own command line (killed my shell, exit 144) — use `fuser -k <port>/tcp`.

**IN PROGRESS, PAUSED by user 2026-09-20 (island layouts):** `layout` knob (0 one island / 1 archipelago /
2 island arc) + `island_count`, `island_spread`, `island_size_variation`, `island_separation` in knobs.rs
(`CHOICES` table marks layout as a list); world.rs `layout_blobs` + `landmass` (smooth union of blobs,
per-island scaling of coast noise and inland relief, volcanic core uplift for arcs); hydrology.rs island
statistics; search.rs layout-aware score thresholds + "islands" criterion; serve.rs sends `choices`,
`islands`, `largest_island_km2`. Builds clean; one-island world file verified BYTE-IDENTICAL to before
(gotcha: regrouping a float product changed discharge in the last bit — keep `((1-share)*belt)*inland`).
CLI works: `just forge-proto --cell 32 --set layout=1 [--set island_count=12 …]`. Archipelago looks good.
**TODO when resumed:** (1) ui.html does NOT yet render `choices` as a <select> (layout shows as a 0–2
slider) nor show the Islands note; (2) island arc is weak — default gives 8% land, max 89 m, score 0:
islands too small/low (spacing cap `room`, ARC_CORE_UPLIFT) — tune; (3) requested island count often not
reached (12 asked → 5–8 survive); (4) docs (F2 findings, CLAUDE.md) + justfile comment not updated for
layouts; (5) lesson: never slice-replace a file region by two far-apart anchors — it deleted `Params`.
