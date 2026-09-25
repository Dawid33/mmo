# World forge — prototype F1 findings (2026-09-20)

Continues `../2026-09-20-world-forge-f0/findings.md`. Still standalone: `crates/worldgen/examples/forge_proto/`
(`just forge-proto`), no engine code or world output touched. F1 adds the five things F0 listed as missing.
Renders next to this file; full-size in `target/world-forge/` (`f1-*` default seed, `f1-best-*` search winner).

## What was added
| module | what it does |
|---|---|
| `geology.rs` | **Geology before erosion.** Rock column (sandstone / shale / limestone / conglomerate formations 18–64 m thick, occasional basalt sills, granite basement below s = −520 m) indexed by a stratigraphic coordinate `s = (y − uplifted) − bedding`, `bedding` = regional dip 0.04 + 2 km folds ±45 m. `y − uplifted` undoes the rock uplift, so the mountain belts **dome the strata** and exhume old rock in their cores — no fudge factor. Each iteration the erodibility of a cell is that of the rock its surface currently cuts. |
| `climate.rs` | **Orographic rain** on the current heights (wind from the west): moisture recharges over sea, rains out with background depletion + windward rise, refreshed every 25 iterations. Rain weights discharge → wet flanks get bigger rivers and erode faster; the lee side is drier and steeper. |
| `post.rs` | **Height-band fit** (scale land if peaks > 236 m), **valley floors** planed beside rivers (half-width 28 m·√km², ≤ 180 m, cross slope 0.012), **lake basins** (as F0). |
| `hydrology.rs` | **Rivers and lakes as objects**: reaches between network nodes with Strahler order, discharge, width `5.5 m·√km²`, Chaikin-smoothed polylines carrying bed + water surface (the flooded routing surface: never rises downstream, flat across lakes); lakes with level, area, max depth, outlet. Written as `*-water.json` — a first sketch of that part of the world artifact. Plus the world statistics. |
| `search.rs` | **Generate-and-test**: forge N seeds at 32 m cells on 80% of the cores, score each (land share, peaks, longest river, biggest basin, lake count, buildable land, rock variety, clear of the rim), rank, thumbnail the top 3. |

## Results
Default seed, 16 m cells: 26 s; peaks 248 m (in band), 8 lakes, 90 river reaches (Strahler ≤ 3), 36 mouths,
60 km of channel, longest river 8.1 km; surface rock 14/35/28/11/3/9 % (sandstone/shale/limestone/
conglomerate/basalt/granite); θ = 0.69, Hack = 0.55; 45 km² buildable.

Search, 64 seeds: **74 s** (12 threads, ~0.37 GB). Winner `0xD5C0DE021A` forged at 16 m (33 s): 42% land,
13.1 km river, 9 lakes (largest 0.15 km²), 88 km of channel, 64 km² buildable, relief fitted ×0.92 → 236 m,
score 0.59 (weakest: biggest basin 14 km², rock variety).

- **Geology map reads like a geological map**: granite cores ringed by concentric strata bands (the dome),
  broad provinces from the regional dip; hard bands stand as scarps/cuestas, shale floors the lowlands.
  Max slope rose from 0.6 (F0, uniform rock) to ~1.1–1.3: the first real cliffs, and they sit on hard beds.
- **θ rises to ~0.7 with geology** (0.62–0.66 in uniform rock): expected — rock contrasts add slope
  variance that is not discharge-driven; it is no longer a pure steady-state check.
- Relief is **not steady**: peaks swing ±30 m over time as different beds are exhumed (e.g. 262 → 279 →
  233 → 257 m). Realistic, but it makes final relief unpredictable → the height-band fit.

## Lessons (things that went wrong first)
1. **Rain-weighted channel threshold smooths the dry side.** Comparing rain-weighted discharge against a
   fixed critical value left the whole lee flank of the ranges undissected ("clay"). Channel initiation
   must be an AREA threshold (scaled by local rain for incision strength): a dry flank is as dissected as
   a wet one, its streams are just weaker. See `ab-climate-geology.png` (TL full-before-fix, TR climate
   only, BL geology only, BR neither).
2. **Rain model defaults were far too orographic** (30 km background depletion, 260 m scale → deserts
   everywhere but the windward crests). 9 km / 650 m, clamped to 0.4–3× the land mean, looks right.
3. **The coarse search does not predict peak height.** F0 showed drainage structure is resolution-stable;
   with geology + climate the 32 m candidate still predicts land share, river length and lake count well
   (14.4 → 13.1 km river, 9 → 9 lakes) but under-predicts the 16 m peak by up to ~30% (195 → 257 m) and
   mis-predicts basin size (20 → 14 km², a divide moved). Hence: fit the relief to the band instead of
   scoring it; and re-score the top few at target resolution before committing to a world.
4. Hardness contrasts multiply relief (granite at 0.45 pushed peaks to 390 m): contrasts were softened
   (0.5–1.7) and uplift lowered to 4.6e-4.

## Still missing / wrong
- D8 routing: channels are smoothed as polylines now, but the *terrain* valleys still follow 8 directions
  at cell scale; planar hillslopes at walking scale (needs per-point detail at region refinement).
- No sediment budget: valley floors are planed geometrically, not deposited; no fans, no deltas; coast is
  static.
- Strata are continuous: no faults, no unconformity relief, no plutons/dykes; one column per world (no
  lateral facies change). Lakes are placed, not caused (no glaciation model / tectonic basins).
- Wind is axis-aligned from the west; no temperature/biomes yet; no snow line.
- Mountain belts are narrow noise-contour ribbons → long sinuous single ridges; a broader orogen (belt
  width varying, parallel ranges) would read better.
- One island per world; islets only by luck. World-edge gameplay (sailing off = loading screen) untouched.

## Next: F2 (engine side) — proposed
1. **World artifact**: tiled rasters at 1–2 m (multi-resolution refinement of the 8–16 m simulation +
   detail noise that respects the river objects), `uplifted` + `bedding` rasters + the rock column (so the
   engine can answer "rock at (x, y, z)"), rain/temperature, river + lake objects. Decide file vs Postgres.
2. `generate_region` / `coarse_tile` sample the artifact; channel + bank carve per 1 m column from the
   river objects; valley detail; strata banding + slope-aware regolith in `material_at` (needs new stone
   blocks/textures).
3. Voxel water above sea level (lake levels, river reaches as flat steps with falls) and the LOD rule
   "water rounds down, solids round up"; world-edge handling (LOD rings clip at the rim, loading trigger).
4. Decide what wasm `?offline` mode uses (small artifact vs today's procedural generator).
