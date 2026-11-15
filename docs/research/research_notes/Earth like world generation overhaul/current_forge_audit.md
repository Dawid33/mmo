# Audit of the current world generator ("the forge") — notes for the Earth-like overhaul report

Audited 2026-11-14 on `develop` (working tree, uncommitted changes included). Read-only: nothing was
built or run, so every number below is quoted from code, a test or a doc, not measured now.
Paths are relative to `/home/dawids/Projects/mmo`. `wg/` abbreviates `crates/worldgen/src/`.

Short answer to "how far is today's code from the five goals":

| Goal | Today | Gap |
|---|---|---|
| 1. Equator in the middle, poles top and bottom | Already a knob value: `pole = 3` ("Both") in `wg/forge/climate.rs:132-133`. Default is `pole = 1` (north pole only, equator on the south edge). | Small in the forge (a default + presets + tests). The real problem is scale: 180° of latitude over 16–100 km. |
| 2. Plate-tectonics continents and aged ranges | Static Voronoi plates with one random drift direction each; land/sea comes from a separate layout formula, not from plates. No time, no ages, no crust type. | Large. New stage; `World::new_continental` is the single place that consumes it. |
| 3. ~72 biomes from seasonal climate | 20 biomes from annual mean temperature and annual precipitation. | Medium-large: classifier, stored fields, about ten consumers with fixed-size tables. |
| 4. All water features | Rivers (polylines), flat lakes at spill level, salt from a water-balance rule, static ice. Sea is a fixed mask, at most 90 m deep, coast set by noise. | Large. Several features are impossible with a fixed ocean mask. |
| 5. Runtime climate cycle | None. Stated non-goal of the current design. A calendar exists (336-day year, 56 real minutes). | Large, mostly in `game`/`client`, under the determinism rules. |

---

## 1. World scale

**Units.** 1 block = 1 m = 16³ voxels (`crates/game/src/block.rs:14`). Region edge is a build-time dial
(`VOXEL_REGION_BLOCKS` = 64 / 128 / 256 / 512; play recipes use 256, tests 64 — `CLAUDE.md` "Build/run
dials"). Sim chunk = 16 blocks. Sea level is world block Y 64 (`wg/pipeline.rs:23`), the sea's top
block is 63 (`:32`). Forge heights are metres relative to sea level (`wg/forge/world.rs:424-425`).

**Sizes offered.**
- `ForgeParams::default()` = 16,000 m at 32 m cells (`wg/forge/world.rs:11-16`, `:399-409`). Used by the
  server's missing-world forge, tests, and the `debug embarked` launch (`crates/client/src/debug_launch.rs:5`, `:501`).
- Main-menu generator: `WORLD_SIZES = [16 km, 32 km, 64 km, 100,352 m "100 km (DF Medium)"]`, default
  index 3 = 100,352 m (`wg/forge/play.rs:88-90`). Cells offered: 16 m (default), 32 m, 8 m
  (`:94`); a size/cell pair is offered only if its estimated peak RAM is under 12 GiB (`:98-106`),
  so 100 km is 16 m or 32 m, never 8 m.
- The world is always **square**: one `size_m`, `n = round(size_m / cell_m)`, rasters `n × n`
  (`wg/forge/world.rs:457`, `wg/forged.rs:278-286`). There is no width/height pair anywhere in the
  forge or the artifact.

**Cells in a "full" world.** 100,352 / 16 = 6,272 cells per side = 39.3 M cells. At 32 m: 3,136² = 9.8 M.
In regions: 392 × 392 at 256 m regions, 196 × 196 at 512 m (the "196 tiles of 512 m" of `play.rs:86`).
In store tiles: `TILE_CELLS = 256` cells = 4,096 m at 16 m cells (`wg/forged_tiles.rs:32-34`), so
25 × 25 tiles (the last row/column partial).

**Coarse-to-fine schedule** (`wg/forge/mod.rs:270-300`): up to `coarse_levels` (default 2) levels, each
2× coarser, dropped if a level would have fewer than `MIN_LEVEL_CELLS = 32` cells per side (`:260`).
Coarsest level runs `iters` (200), the final level `refine_iters` (80), any level between
`INTERMEDIATE_REFINE_SHARE = 0.5` of that (40) (`:268`). Defaults in `wg/forge/knobs.rs:85-90`.

| World | Levels (cell → grid, iterations) |
|---|---|
| 16 km / 32 m (dev default) | 128 m → 125², 200 · 64 m → 250², 40 · 32 m → 500², 80 |
| 100 km / 16 m (menu default) | 64 m → 1,568², 200 · 32 m → 3,136², 40 · 16 m → 6,272², 80 |

Each iteration is `dt_years = 2,500` (`knobs.rs:86`), so the run models about 0.8 Myr of erosion; this
is a landscape-evolution step count, not geological history.

**Forge time and memory (from docs/code, not re-measured).**
- `wg/forge/play.rs:26-41`: measured 2026-11-01, release, rayon on 20 logical cores: 16 km / 16 m
  (1.0 M cells) 9.1 s; 32 km / 16 m (4.0 M) 33.7 s. Model: `9.0 s × (Mcells)^1.05`, RAM
  `100 MB + 180 B × cells`. **64 km and 100 km at 16 m were not timed**; the model gives about 7 min
  and 7.2 GB for 100 km / 16 m (the test at `play.rs:251-254` only bounds it to 300–900 s, 6–9 GB).
- `docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md:74-100`: the older
  single-threaded forge took 1 h 9 min and 5.5 GB for 100 km / 16 m, 9 min 19 s for 100 km / 32 m.
  The row "100 km / 16 m, optimised — not yet measured" is still empty there.
- Preview in the generator: the finest of 32/64/128/256 m with at most 512² cells, "about a second"
  (`play.rs:9-24`); 100 km previews at 256 m.

---

## 2. The pipeline, stage by stage

Entry point `forge_with` (`wg/forge/mod.rs:305-399`); stage list in the module header (`:9-36`).
Everything is `f64` on a row-major grid (z rows, x columns), z = 0 is **north** (`world.rs:68-70`).

| # | Stage | File | Inputs | Outputs |
|---|---|---|---|---|
| 1 | Initial world | `world.rs` `World::new` / `new_continental` | seed, size, cell, knobs | `h`, `ocean` mask, `uplift` rate, `outlet` mask, `edges` |
| 2 | Geology | `geology.rs` `Geology::new` | seed, knobs, grid | `bedding` raster, rock `column`, erodibility per rock |
| 3 | Climate | `climate.rs` `compute` | current `h`, `ocean`, `edges`, lakes | `temperature_c`, `precip_mm`, `latitude_deg` (per row) |
| 4 | Erosion loop | `erosion.rs` `step`, `routing_fast.rs` | `h`, `uplift`, geology, rain weights | evolved `h`, `uplifted` |
| 5 | Shaping | `post.rs` | eroded `h`, rain | height fit, floodplains, lake basins |
| 6 | Hydrology | `hydrology.rs` `analyze` | final `h`, rain | lakes, river reaches, stats |
| 7 | Fields | `fields.rs` `compute` | final world, analysis, geology, plates | drainage, volcanism, salinity (u8 0..100), volcano sites |
| 8 | Spawn | `spawn.rs` `pick` | world, analysis | one spawn point |
| 9 | Biomes/regions | `regions.rs` `derive` (called from `Forged::artifact`, `mod.rs:144`) | the **quantised** stored fields | biome, region ids, region records, savagery |

Order inside `forge_with`: per level — build `World::new` at that cell, upsample the coarser result onto
it (`world.rs:690-721`: `h` and `uplifted` bilinear; `ocean`, `outlet`, `uplift` taken fresh from the
fine initial condition), new `Geology`, fresh climate, then the iteration loop (`mod.rs:330-371`). After
the last level: `fit_height_band` → climate → `carve_floodplains` → `carve_lake_basins` →
`follow_land_edges` → final climate → `analyze` → `fields::compute` (`mod.rs:377-398`).

**Stage 4 details.** One step (`erosion.rs:168-188`, `evolve` `:195-300`):
- Routing: Priority-Flood + ε (Barnes 2014) on the first step of a level (`:76-143`), then the
  linear-time depression graph of Cordonnier, Bovy & Braun 2019 (`routing_fast.rs:1-38`). D8,
  single receiver (steepest descent).
- Discharge = drained land area × relative rain, "m² of average rain" (`:145-166`). It is an area, not
  a volume flow.
- Implicit stream-power incision, `dh/dt = U − K·√Q·S` (Braun & Willett 2013, n = 1), downstream-first
  (`:233-269`). Cells under a depression do not incise; they silt up by `depression_fill` (0.15) of
  their depth per step (`:250-254`).
- Hillslope: `q.max(hillslope_area_m2 * rain)` floors the discharge so relief does not depend on the
  cell size (`:14-21`, `:261`), then explicit linear diffusion, sub-stepped (`:271-298`).
- Land is clamped to `LAND_FLOOR_M = 0.05` m (`world.rs:382`); **land never becomes sea and sea never
  becomes land** — the `ocean` mask is fixed by stage 1.
- Parallel over whole outlet trees and rows; bit-identical for any thread count
  (`erosion.rs:192-194`, test `parallel_step_matches_serial` `:335`).

**Stage 5 details** (`post.rs`):
- `fit_height_band` (`:39-54`): if the highest peak exceeds `max_peak_m` (2,800 m, max 3,500), all land
  `h` and `uplifted` are scaled by one factor. Relief is "only predictable to ~30%" (`:34-36`).
- `carve_floodplains` (`:57-94`): flat floor beside rivers with discharge ≥ 1e6 m², half-width
  `floodplain_m_per_sqrt_km2` (28) × √km², capped at `floodplain_max_m` (180 m).
- `carve_lake_basins` (`:97-161`): `lake_basins` (9) bowls, radius 140–420 m, depth 5–14 m, at least
  `lake_spacing_m` (1,500 m) apart, on valley floors with discharge 0.7e6–2.5e7 m² and at least 6 m above
  the sea; "never dug below the sea, so lakes stay fresh" (`:154-156`). The comment at `:17-20` states
  the reason: "a fluvial steady state has no closed depressions of its own".

**Key tunables** — all in `wg/forge/knobs.rs:45-141` (one f64 table, 9 groups). Defaults worth quoting:
`layout 3` (Continent), `sea_edges 2`, `continent_coast 0.45`, `plate_count 7`,
`plate_belt_width 0.15`, `belt_uplift 1.4`, `interior_ranges 2`, `belt_length_km 8`,
`volcano_count_max 6`, `uplift 5e-3 m/yr`, `lowland_uplift_share 0.04`, `erodibility 2.5e-5`,
`diffusivity 0.05`, `hillslope_area_m2 6000`, `iters 200`, `dt_years 2500`, `coarse_levels 2`,
`refine_iters 80`, `pole 1`, `latitude_span_deg 90`, `equator_temp_c 27`, `pole_temp_c −25`,
`lapse_c_per_km 6.5`, `max_peak_m 2800`, `lake_basins 9`, `good_share 0.1`, `evil_share 0.1`,
`river_min_discharge_m2 400000`, `river_width_m 5.5`.

**What stages assume about each other.**
- The initial condition must be recomputable at any cell size from seed + knobs alone, because every
  level calls `World::new` again (`mod.rs:333`). Plates, belts, coast and volcano cones are therefore
  analytic functions of position, never simulation state (`plates.rs:14-18`, `world.rs:226-232`).
  **A plate simulation that produces a raster breaks this assumption** unless its output is resampled
  per level.
- Geology indexes rock by `s = (y − uplifted) − bedding` (`geology.rs:6`), so it needs `uplifted`
  carried through erosion, upsampling and the height fit (`post.rs:47-50`).
- Climate reads only `h`, `ocean`, `edges` and a lake mask; erosion reads climate only as a relative
  weight (`climate.rs:335-345`).
- Biomes are classified from the *stored, quantised* rasters so a reader that re-classifies gets the
  stored value (`mod.rs:142-144`, `regions.rs:343-362`).
- The forge is "Not bit-reproducible across machines by design (sqrt/ln/sin used freely): the forge's
  output is a stored artifact, not a function both sides evaluate" (`mod.rs:38-39`).

---

## 3. Layouts, edges, wrapping

Six layouts (`knobs.rs:150-159`): 0 One island, 1 Archipelago, 2 Island arc, 3 Continent (default),
4 Continents, 5 Inland. `is_continental()` = layout ≥ 3 (`:168-170`). The two families share almost
no code path: `World::new` branches at `world.rs:454-456`.

**How land/sea is decided — a "landness" scalar > 0, never plates.**
- Island layouts (`world.rs:453-549`): smooth union of circular blobs (`layout_blobs` `:280-340`,
  `landmass` `:353-379`) + 8 km and 1 km coast noise, minus a superellipse ocean margin beyond
  `ocean_margin_start` (0.84 of the half-width) that forces sea all round (`:483-506`); asserted at `:542-546`.
- Continental layouts (`ContinentalMass` `:96-170`, `ContinentalShape` `:175-224`):
  - Continent: `min over sea edges of (1 − proximity − coast[e])`, capped at 1 — i.e. a coast line
    `continent_coast` (0.45 half-widths, ±40% by seed) in from each sea edge (`:144-148`).
  - Continents: the same, further cut by seas along the Voronoi boundaries of 2–4 random centres
    (`continent_count` clamps to 2..4, `:115`), channel half-width `continent_sea_width` (`:149-164`).
  - Inland: distance from a random inland-sea centre minus `inland_sea_size` (`:165-167`).
  - Plus the same two coast noises with wavelengths scaled by `size_m / 16 km` (`:194-199`, `:207-216`).
- Sea floor: `(shape × 160 − 1).max(−90)` — the deepest ocean is **90 m** (`:511-516`, `:589-594`).
  Initial land: `1 + 45·smoothstep(landness) + 3 m of noise` — almost flat; erosion makes the relief
  (`:519-523`, `:597`).

**Edges** (`EdgeKind`, `world.rs:49-57`; order N, E, S, W `:68-70`).
- `Sea`: ocean margin keeps the border drowned. `Land`: an open drainage boundary; border cells are
  outlets whose base level is their own height, held `LAND_EDGE_OUTLET_SLOPE = 0.004` below the cell
  inside and only ever lowered (`follow_land_edges` `:641-668`).
- Which edges are sea is **picked by the seed**, `sea_edges` (default 2) of the four (`edges()` `:75-87`);
  inland has none. There is no knob that says "north and south are sea/ice". For an Earth-like map with
  polar caps on the top and bottom edges this choice has to become explicit.
- Belt uplift tapers to zero over `LAND_EDGE_TAPER_M = 2,000` m from a land edge (`:23-34`, `:599-610`).
- At runtime, past a sea edge the ground is `OUTSIDE_HEIGHT_M = −90` m; past a land edge the border's
  ground is extended outward as a constant (`wg/forged.rs:62-65`, `place`/`ground` `:1263-1337`).
  The playable world ends at the map's outer seam (`MapExtent`, `crates/game/src/world_map.rs:108-127`).

**Wrapping: the map does not wrap.** `EdgeKind` has only `Sea` and `Land`. A full plan for an east–west
cylinder exists and is **not implemented** (verified: no `Wrap` variant, no `wrap_x` in `game`/`worldgen`):
`docs/superpowers/plans/2026-11-05-east-west-world-wrap.md`. Its design section already lists the work
(appended `EdgeKind::Wrap`, canonical `RegionCoords`, periodic noise, periodic Voronoi images, river
reaches across the seam, size snapped to a multiple of 512 m). Its decision D1: "Cylinder, x only.
Latitude drives climate, so z never wraps."

---

## 4. Plates and uplift

**What `plates.rs` computes (363 lines) — a static geometric stand-in, not a simulation.**
1. `plates()` (`:69-94`): `plate_count` (7, clamp 2..64) Voronoi sites, rejection-sampled with a minimum
   gap, over the world grown by 15% on each side; each plate gets one **unit drift vector at a random
   angle** (`:89-92`). No speed, no rotation pole, no oceanic/continental type, no age.
2. `convergent_boundaries()` (`:147-186`): for each pair, closing speed
   `0.5·((va − vj)·û)` along the line between sites; strength `smoothstep(0.10, 0.35, closing)`
   (`:43-44`, `:155-156`). The bisector is clipped by every other site's half-plane so a boundary ends
   at its triple junctions. Divergent and transform boundaries produce nothing.
3. `soften_along_land_edges()` (`:192-228`): weakens boundary stretches that run parallel to a land edge.
4. `plate_field()` (`:245-271`): per cell, after a domain warp of 5% of the world size (`:33`,
   `:114-129`): plate id (`u16`) and belt strength 0..1 =
   `max over boundaries (strength × profile(distance))`, flat to 35% of the half-width then a smoothstep
   to zero at `plate_belt_width × size_m` (`:50`, `:240-242`), times an along-strike variation of up to 30% (`:47`, `:265-266`).
5. `interior_ranges()` / `interior_strength()` (`:283-306`): `interior_ranges` (2) **straight segments**
   of `belt_length_km` (8 km, capped at 60% of the world) at random positions and angles, at 35% of a
   boundary belt's strength (`:53`). The header calls them "old belts inside a plate" (`:10-12`); this is
   the only notion of belt age in the code, and it is one constant.

**How `world.rs` turns it into uplift** (`new_continental`, `:556-629`), per land cell:

```
belt   = max(field.convergence, interior_strength)
uplift = k.uplift × coast_taper × ( lowland_uplift_share
         + (belt_uplift − lowland_uplift_share) × max(belt × edge_taper, volcano_cone) )
```
(`:598-610`). So uplift is a **rate** (m/yr), constant in time for the whole run; height comes only from
erosion balancing it. The plate id raster is used for nothing except a test and volcanism's
`convergence`. Plates do **not** influence where land is, the coast, the sea floor depth, or rock type.

**Volcano sites** (`world.rs:226-243`, `fields.rs:360-407`): cells where the volcanism field reaches 100
on a fixed 250 m reference grid, inland by landness 0.15, clear of land edges, taken in seeded order,
at least `VOLCANO_SPACING_M = 8,000` m apart, at most `volcano_count_max` (6). Each is a linear cone of
uplift of radius `VOLCANO_CONE_RADIUS_M = 3,000` m (`fields.rs:80-90`, `:402-407`). Both distances are
absolute metres, not scaled with the world. Island layouts have no plates and no volcanoes
(`fields.rs:124-128`).

**Volcanism field** (`fields.rs:316-358`): `0.45 × base noise (16 km at reference size)` +
`1.3 × flank(convergence) × hot-spot noise`, where `flank` ramps in between convergence 0.1–0.35 and
is damped 70% toward the belt core. Stored as u8; consumed only by the volcano picker and the map
(`TileSummary::volcano`). Not coupled to geology ("Magma and igneous geology coupling: later" — design doc `:190`).

**What a moving-plate simulation with per-belt ages would have to change.**
- Supply, per final-grid cell: an uplift-rate raster, an initial-height raster (today nearly flat:
  `world.rs:597`), a land/ocean mask and sea-floor depth (today from the layout formula), and ideally
  crust age / belt age and a boundary-type raster.
- `World::new_continental` is the single consumer; `ContinentalMass`/`ContinentalShape` (land/sea),
  `plates::plate_field` and `interior_ranges` are what it would replace. `volcano_sites` and
  `fields::volcanism` also call `plate_field` and need the new boundary data (`fields.rs:124-128`, `:393`).
- The coarse-to-fine schedule calls `World::new` at 2–3 resolutions and expects the same world each time
  (`mod.rs:333`; test `plates_agree_across_resolutions`, `plates.rs:331-342`). A simulated result must be
  computed once (on a fixed coarse grid, as the volcano grid already is: `fields.rs:85-88`) and resampled.
- "Age" has no carrier today. Options that fit the existing model: (a) lower uplift rate for old belts
  (the erosion then lowers and rounds them — this is physically what age does, but 0.8 Myr of modelled
  time is far too short for an old range to decay on its own, so it must be an initial-height +
  low-uplift input); (b) a per-cell erodibility/diffusivity modifier; (c) a different exposed rock
  (old belts expose basement — the `basement_top_s` mechanism at `geology.rs:71`, `:107` already does this
  where `uplifted` is large). `erosion.rs` reads uplift and erodibility per cell (`:248`, `:260`), so (a)
  and (b) need no structural change there.
- `fit_height_band` scales **all** land by one factor (`post.rs:44-52`); several ranges "of varied size"
  survive it, but absolute heights are not controllable per range.
- Hard-coded to the current scheme: `belts_follow_plate_boundaries` (`world.rs:920-953`),
  `plate_field_is_deterministic_and_uses_every_plate`, `belt_field_is_continuous` (`plates.rs:312-362`),
  `land_edges_drain_across_without_a_lip_or_ridge` (`mod.rs:570-597`).
- Note for the report writer: `docs/superpowers/specs/2026-07-06-tectonic-worldgen-research.md` is about the
  *Tectonic Minecraft mod* (noise splines), not plate tectonics. No plate-simulation research exists in `docs/`.

---

## 5. Climate

All in `wg/forge/climate.rs` (632 lines). The stored climate is **two annual rasters**:
"Mean annual temperature at the ground" and "Annual precipitation, mm/yr" (`:102-110`). **Nothing is
seasonal or monthly anywhere in the forge, the artifact or the runtime.**

**Latitude** (`latitudes`, `:122-138`) — one value per **row**; `t = (z + 0.5)/n`, 0 = north edge:

| `pole` | Formula | Meaning |
|---|---|---|
| 0 None | `latitude_center_deg` | whole map at one latitude |
| 1 North (**default**) | `90 − span·t` | pole on the north edge, equator on the south edge at span 90 |
| 2 South | `−90 + span·(1 − t)` | mirrored |
| 3 Both | `90 − span·2t` for t < 0.5, else `−90 + span·2(1 − t)` | **poles on both edges, equator across the centre** at span 90 |

Knobs `pole`, `latitude_span_deg` (90), `latitude_center_deg` (45): `knobs.rs:107-109`. Southern
latitudes are negative; everything downstream uses `abs` (`climate.rs:7-8`). Test
`latitudes_follow_the_pole` (`:621-631`) already covers "Both". So goal (1) is reachable today with
`pole = 3`; no preset uses it (`presets.rs:17-140`: "Continent" = north pole; "Temperate band",
"Tropical", all island presets = no pole; "Frozen north" = span 30).

**Meander** (`meander_deg`, `:142-156`): three sine waves (shares of wavelength 1, 0.5, 0.27; amplitudes
1, 0.6, 0.35) added to the row latitude; `meander_amplitude_deg` 5°, `meander_wavelength_km` 40 (capped at
half the world). Effective latitude per cell = row latitude + meander (`:166-170`).

**Temperature** (`:173-176`, `:247-252`):

```
T = pole_temp_c + (equator_temp_c − pole_temp_c) · cos(|lat|)  −  (lapse_c_per_km/1000) · max(h, 0)
```
Defaults −25, 27, 6.5. Sea cells use height 0. No ocean moderation, no continentality, no currents
(the design doc lists "optional small ocean moderation near coasts" at `:171`; it was not built).

**Winds** (`westerly_weight`, `:181-186`): strictly **zonal** (along rows). From the east below
`trade_wind_edge_deg` (30°) and above `polar_edge_deg` (60°), from the west between; blended over
`BAND_BLEND_DEG = 5°` (`:71`). No meridional component, no monsoon, no seasonal shift of the bands.

**Precipitation** (`orographic`, `:267-330`; `compute_with_lakes` `:254-256`):
- Base by latitude, linear between table points (`LATITUDE_PRECIP_MM`, `:59-60`):
  0° 2500, 10° 1800, 25° 180, 40° 900, 55° 1000, 70° 400, 90° 200 mm/yr.
- One moisture scalar swept along each row, from the west and/or from the east. Entering air is 1.0
  across a sea edge, `CONTINENTAL_MOISTURE = 0.65` across a land edge (`:66`, `:273`). Over sea it
  recharges toward 1 over `sea_recharge_m` (5 km); over land it relaxes toward 0.65 over
  `land_depletion_m` (9 km); a lake recharges it; a rise rains out `rise / orographic_scale_m` (650 m)
  of it (`:279-301`).
- Cell rain relative to base = `moisture × (1 + rise × OROGRAPHIC_RUN_M / orographic_scale_m / dx) / 0.65`,
  blurred (1,500 m cross-wind, 500 m along-wind, land only), clamped to `rain_min..rain_max` (0.4..3.0),
  scaled by `climate_strength` (`:293`, `:314-329`). Heights are blurred over 400 m first (`:94`).
- All smoothing lengths are in metres so the field does not depend on cell size (tests `:553-591`).
  **They are absolute, not scaled with the world**: at 100 km the interior reaches continental
  equilibrium 9 km from the coast.

**Refresh during erosion.** Every `RAIN_REFRESH_ITERS = 25` iterations (`mod.rs:66`, `:348-355`), plus
once at the start of each level (`:342`), once after the height fit (`:382`) and once at the end for the
stored field (`:394`). Lakes for the refresh come from the last step's routing surface (no extra flood).

**Consumers.**
- `discharge_weight` = precip / mean land precip (`:335-345`) → erosion incision and channel-head area
  (`erosion.rs:261`), floodplains, lake sites, hydrology (river threshold, widths, Strahler).
- Stored `temperature_c`, `precip_mm` → biome classifier (`wg/forged.rs:389-398`), tree habitat
  (`wg/habitat.rs:105-113`), snow line / ice / glacier thickness at runtime (`wg/forged.rs:1222-1257`),
  world map layers (`wg/tile_map.rs:263-303`, `:539-556`), seed score (`wg/forge/quality.rs:282`).
- `Climate::latitude_deg` is **not stored** in the artifact (see §9).

---

## 6. Hydrology and water

**Forge side.**
- *Rivers* (`hydrology.rs:217-289`): a cell is channel when rain-weighted drained area ≥
  `river_min_discharge_m2` (400,000 m²). Reaches run between network nodes (source, confluence, mouth);
  Strahler order; width = `river_width_m` (5.5) × √(Q in km²), minimum 1.5 m; two Chaikin passes smooth
  the D8 staircase. A reach ends on a confluence, the sea (bed 0, water 0) or a land-edge outlet
  (`:266-276`). Each point stores `x, z, bed, water, in_lake`. Single-thread D8 trees only: **no
  distributaries, so no deltas or braided channels are representable**.
- *Lakes* (`:113-160`; `mod.rs:156-216`): a flood without ε fills each closed depression exactly to its
  spill level (`hydrology.rs:294-296`); a lake cell is ≥ `LAKE_MIN_DEPTH_M = 0.3` m under it; bodies under
  4 cells are dropped. Stored bodies are 4-connected groups at one level with their cells as row-major
  runs. Lakes exist essentially because `carve_lake_basins` digs them (§2) — default 9 per world,
  whatever its size.
- *Floodplains*: planed valley floors (§2); also the low-drainage zones of the drainage field.
- *Sea level* is the constant 0; the ocean mask is fixed from the layout (§3). No sea-level change,
  no drowned valleys, no shelf/abyss beyond the −90 m clamp.
- *Drainage* field 0..100 (`fields.rs:141-309`): `0.35 + 0.4·permeability + 0.5·slope/0.2`, damped up to
  70% on flat ground within the flood height of the nearest river/lake (flood height
  `4 m × min(1, (Q/1e8)^0.4)`, reach 1.5–3 km, slope under 0.025). Sea and lakes are 0.
- *Salinity* field 0..100 (`fields.rs:411-467`): 100 on sea and on **closed lakes**, falling to 0 over
  `SALT_REACH_M = 120` m inland (`wg/habitat.rs:22`). Every forged lake spills geometrically, so "closed"
  is a water-balance rule: largest discharge in the body < `ENDORHEIC_INFLOW_RATIO = 3` × its own area
  (`fields.rs:24-30`, `:106-108`). The sea itself has no salinity gradient (no brackish estuaries).
- *Stats* used by the quality gate: land share, height percentiles, concavity θ, Hack exponent, mouths,
  channel km, Strahler, lake count (`hydrology.rs:55-84`, `:332-390`).

**How water reaches the game.**
- Runtime sampler `ForgedWorld::refine` (`wg/forged.rs:1351-1424`): lake water where the raster cell is
  lake and the ground is below the (block-snapped) level; a berm of `FREEBOARD_M = 0.3` m on dry cells
  next to a lake; rivers carved at 1 m resolution from the nearest centreline segment — parabolic bed of
  depth `0.5·√width` clamped 0.8–3.0 m, a 2 m levee ring, bank cut back at slope 0.7 over 24 m
  (`:77-88`). Water at or below sea level "is just the sea" (`:1421-1422`). Segments are indexed in 64 m
  buckets (`:90`).
- `Patch::sample` returns `(profile, water_top block, is_lake)` (`wg/lib.rs:331-336`,
  `wg/forged.rs:1551-1572`); `columns_from_patch` writes `ColumnGround::water_top` only where water stands
  above the ground top (`wg/lib.rs:893-898`) and leaves the chunk cells as **air** — "the water under the
  surface included: it is the recipe's (`ColumnGround::water_top`), never a block" (`:952-954`).
- `ColumnGround` (`crates/game/src/ground.rs:36-59`): per block column `surface: i16`, `geo: i16`,
  `water_top: i16` (`NO_WATER = i16::MIN`), `water_body: u32`; it derives `StateHash` and serde, so it is
  hashed sim state and is saved with the region.
- Sim read: `Rollback::water_units_at` → `static_water_units` = `WATER_FULL` (4096) for an air cell
  inside `surface+1 ..= water_top`, else 0 (`crates/game/src/water.rs:55-58`, `:189-195`). Bodies are
  labelled once at genesis, per region, by 4-connected flood over equal `water_top`
  (`label_water_bodies`, `:65-112`) with `{level, volume}`.
- LOD: `coarse_water` builds a `LodWater` layer per LOD key (`wg/lib.rs:1078-1180`, `crates/game/src/lod.rs:538-580`).

**What the runtime can and cannot represent today.**

| Can | Cannot |
|---|---|
| One static water surface per block column, on whole-block levels | Flowing water, currents, any water volume moving (`TODO.md:135`: "no currents") |
| Lakes and rivers above sea level; rivers stepping down as a staircase of flat bodies | Changing water levels (floods, seasonal rivers, tides, drying lakes) — `water_top` is written once at genesis |
| Wade / swim depth rules (`water.rs:19-29`) | Drowning (`DROWN_DEPTH` unused, `TODO.md:135`); water in dug pockets; placed blocks re-levelling water |
| Ice as **solid blocks** baked at generation on cold lake/sea water: 1 block per 4 °C below 0, max 4 (`wg/forged.rs:110-113`, `:874-879`; `wg/lib.rs:948-950`); rivers never freeze (`wg/lib.rs:205-207`, `:231`) | Freezing / thawing at runtime; sea-ice extent that moves with the season |
| Glacier ice as solid blocks over dry ground of biome Glacier, 4–40 blocks, thinning at the edge (`wg/forged.rs:114-119`, `:881-888`) | Glaciers as moving or seasonal bodies; glacial valleys/lakes as a forge process |
| Salt flats as a ground cover around closed lakes (`wg/forged.rs:124-127`, `:1243-1247`) | Salt water as a property of the water itself (salinity is a land-side field) |

The flow engine is researched and prototyped but not in the engine:
`docs/superpowers/plans/2026-11-06-water-engine-plan-handoff.md` ("no engine water code exists" at the
time; the static layer then landed 2026-11-07, `TODO.md:134`). Binding user decisions recorded there:
water is deterministic and in-sim on server and every client; **water is finite everywhere**, the sea a
finite body with a volume ledger. Open decision: "is the world ledger the water cycle (recommended
yes)?" (`TODO.md:143`). Planned hook: "step water when `tick % WATER_STEP_TICKS == 0` on the shared
`WorldClock`" (`TODO.md:145`). A runtime climate would be the natural source/sink of that ledger.

---

## 7. Geology

`wg/forge/geology.rs` (148 lines). Geology is built **before** erosion so hardness shapes the land.
- Six rocks, ids fixed by table order: sandstone, shale, limestone, conglomerate, basalt, granite
  (`:24-31`). The artifact stores rock *names* and refuses an unknown one (`wg/forged.rs:515-520`);
  `Materials::rocks` is `[BlockId; 6]` (`wg/material.rs:18`, `:178`) and `Knobs::rock_erodibility`
  indexes a 6-array (`knobs.rs:202-212`); `fields::PERMEABILITY` and `Stats::rock_shares` are 6 long.
- One global column for the whole world, from `s = 700` m down: sedimentary formations 18–64 m thick
  picked with weights 34/30/22/14% (never the same twice), a 6–12 m basalt sill on 10% of boundaries,
  granite basement from `basement_top_s` (−1,500 m) down without limit (`:79-107`, `:117-125`).
- Per cell only `bedding` varies: regional dip (0.04, direction from the seed) + 2 km folds of 45 m
  (`:66-77`). Rock at a point: `column` at `s = (y − uplifted) − bedding` (`:6`, `:128-130`).
- Erodibility multipliers (knobs): sandstone 1.0, shale 1.7, limestone 0.75, conglomerate 0.65,
  basalt 0.5, granite 0.6, with a contrast knob (`knobs.rs:92-104`).
- No igneous provinces, no metamorphic rock, no link between plates/volcanism and rock type. One
  column means the same formation sequence under every continent.

---

## 8. Biomes and regions

**Classifier** `game::classify` (`crates/game/src/biome.rs:237-296`), a pure function of `CellClimate`
(`:193-207`): `height_m`, `temperature_c` (annual mean), `precip_mm` (annual), `drainage` u8,
`salinity` u8, `is_lake`. Order of the gates:
1. lake → `Lake`; height < 0 → `FrozenOcean` if T < −5 °C else `Ocean`;
2. height ≥ `MOUNTAIN_MIN_M = 1200` → `Mountain`, **whatever its climate** (`:21`, `:245-247`);
3. T < −12, or T < −5 with DF drainage ≥ 75 → `Glacier`; T < −5 → `Tundra`;
4. rain index (mm → DF 0..100 via `RAIN_INDEX_MM`, `:28-29`; cuts at 250 / 450 / 650 / 800 / 1,100 mm)
   × wet (DF drainage < 33): desert family (`SandDesert` / `RockyWasteland` / `Badlands`), `Grassland`,
   `Savanna`, `Shrubland` | `Marsh`, conifer | swamp, broadleaf | swamp;
5. variants: `Taiga` under 3 °C, tropical forms from 20 °C, `MangroveSwamp` = tropical swamp with
   salinity ≥ 66 and DF drainage ≤ 9.

**The 20 biomes**, discriminants 0..19 (`:79-100`): Ocean, FrozenOcean, Lake, Glacier, Tundra, Taiga,
ConiferForest, BroadleafForest, TropicalConiferForest, TropicalBroadleafForest, Grassland, Savanna,
Shrubland, SandDesert, RockyWasteland, Badlands, Marsh, Swamp, MangroveSwamp, Mountain.
"Stored as its `u8` discriminant in the world file: append only, never renumber" (`:75-76`).

**Region types** (10, `:299-312`): Ocean, Lake, Glacier, Tundra, Mountains, Forest, Wetland, Grassland,
Hills, Desert. `region_type(biome, drainage)` is an exhaustive `match` (`:357-379`); the grassland
family becomes Hills at DF drainage ≥ 50.

**Regions** (`wg/forge/regions.rs`): 4-connected components of one region type (`:129-169`); components
under one DF world tile (768 m × 768 m, `:29`, `:37`) merge into their largest neighbour, never across
the water/land or frozen/thawed line unless the `u16` id cap forces it (`:68-77`, `:184-253`). Measured
on the 100 km / 64 m default continent: 15,000 raw components → 282 regions (`:32-36`).
There is **no region naming** in code: a region is `{id, kind, cells, alignment}` (`:59-66`); the
map shows "Forest #17" (`wg/tile_map.rs:690`).
- Good/evil (`:293-329`): DF's rule — per size bucket (≤ 24, < 100, ≥ 100 DF tiles) whole regions in a
  seeded order turn good until `good_share` (0.1) of the bucket's area, then evil; ocean excluded.
- Savagery (`:333-341`): 4-octave noise, 12 km first wavelength, absolute metres, `50 + 50·2.6·noise`.
- The module header says "None of it has a consumer yet" (`:5-7`); since then the world map and
  history's capacity rule read them (below).

**Every consumer of `Biome` / `RegionType` (what must change when biomes are added).**

| Where | What | Kind of change |
|---|---|---|
| `crates/game/src/biome.rs:79-190` | enum, `ALL: [Biome; 20]`, `name()`, `BIOME_MAP_RGB: [[u8;3]; 20]` (+ a test that pins colour spread, mentioned at `:167`) | append variants, extend the three tables |
| `crates/game/src/biome.rs:237-296`, `:357-379` | `classify`, `region_type` | new inputs need new `CellClimate` fields; `region_type` is an exhaustive match |
| `crates/game/src/biome.rs:389-495` | tests: `classify_table` requires a row for **every** biome (`:431-433`), `rain_cuts_in_millimetres`, `biome_bytes_round_trip` | extend |
| `wg/material.rs:197-238` | `Materials::ground_cover` — exhaustive match per biome; snow rules name `Mountain`/`Glacier`; `BiomeBlocks` has 11 named blocks (`:28-40`) resolved by name, **all or nothing** (`:78-81`) | one arm per biome; new blocks need manifest entries and art |
| `wg/lib.rs:184-201`, `:208-240` | `treeless`, `forest`, `biome_column` (snow, salt flat, glacier, ice rules) | per-biome decisions |
| `wg/forged.rs:883`, `:1240-1248` | glacier thickness and tundra snow patches keyed on `Biome::Glacier` / `Biome::Tundra` | new ice/snow biomes must be added by hand |
| `wg/trees/placement.rs:26-53` | `BIOME_TREE_DENSITY: [(Biome, f32); 20]`, `NATIVE_ONLY_BIOMES`; a test iterates `Biome::ALL` (`:295`) | one entry per biome |
| `wg/trees/species.rs:241`, `:283`, `:328-334`, `:379-382`, `:426` | per-species biome lists (8 species: oak, pine, birch, spruce, acacia, palm, jungle, cactus — `justfile:654`) plus climate niches on annual temperature and moisture (`:10-37`, `:40-61`) | new biomes get no native trees until listed |
| `wg/habitat.rs:101-114` | tree habitat from annual precip / 900 mm and annual temperature | seasonal inputs would go here |
| `wg/forge/regions.rs:102-104`, `:366-374` | region typing; `biome_shares` uses `[usize; Biome::ALL.len()]` | follows the enum |
| `wg/forge/quality.rs:190-197`, `:207-292` | seed score: `WANTED_BIOMES = 10` distinct biomes, forest/mountain region counts, peaks above snow, `polar` flag | re-tune for 72 |
| `wg/tile_map.rs:263-303`, `:504-532`, `:621-636`, `:681-713` | per-tile dominant biome (mode, lowest discriminant wins ties), map layers, info lines | arrays sized by `Biome::ALL.len()`; any change bumps `WORLD_MAP_VERSION` |
| `crates/game/src/world_map.rs:21-59` | `TileSummary { biome, region, temperature_c, precip_mm, savagery, alignment, volcano }` — a **wire and stored** type | layout change = protocol + stored-map change |
| `crates/game/src/history/rules.rs:41-50`, `history/grid.rs:85` | `biome_capacity_per_mille`: 0 on Ocean/FrozenOcean/Lake/Glacier, 500 on Tundra and the deserts, 1000 otherwise (wildcard arm) | new harsh biomes silently get 1000 |
| `wg/forge/render.rs:284-297`, `wg/examples/forge_proto/{main,render,serve}.rs`, `wg/examples/world_map.rs:104-125`, `:431-450` | preview / debug biome and region layers | follow the palette |
| `crates/client/src/renderer/slint_overlay.rs:43`, `:830`, `:1062-1064`, `:2060` | world-map window: layer buttons from `MapLayer::ALL`, tile info lines | no per-biome code; a legend does not exist |
| `crates/server/src/world_file.rs:562`; tests in `crates/game/tests/*`, `crates/server/tests/*`, `crates/world-store/tests/codec.rs` | fixtures that build `TileSummary` by hand | compile-time churn on a layout change |

Not consumers (checked): creatures (`tools/creatures`, `crates/game/src/creature_shapes.rs`) have no biome
link — "Creatures and wildlife per biome" is a stated non-goal of the current design
(design doc `:54`); `wg/sites` (the town kit) does not read biomes; `game::slope::classify` and
`mc-import`'s `classify` are unrelated functions of the same name.

---

## 9. The stored artifact

**Forge file** (`terrain.world`; `WorldArtifact`, `wg/forged.rs:278-328`). Header `VXWF` + version u32 LE,
then zstd level 19 over bincode, every raster row-delta encoded (`:409-427`). Native only (no codec on wasm).

| Field | Type | Resolution / quantisation |
|---|---|---|
| `version`, `seed`, `size_m`, `cell_m`, `n` | u32, u64, f64, f64, u32 | global; a tiled world requires `n × cell_m == size_m` exactly (`:641-647`) |
| `height` | `QRaster` (u16) | per cell; step 0.0625 m; "16-bit span of 4095.9 m" (`:49-50`) |
| `uplifted`, `bedding` | `QRaster` | per cell; step 0.0625 m, widened automatically if the span exceeds 16 bits (`:183-195`) |
| `temperature_c` | `QRaster` | per cell; step 0.1 °C; annual mean |
| `precip_mm` | `QRaster` | per cell; step 1 mm/yr; annual |
| `column`, `rock_names` | `Vec<(f32, u8)>`, `Vec<String>` | one global rock column |
| `rivers` | `Vec<RiverReach{order, discharge_m2, width_m, points[x, z, bed, water, in_lake]}>` | polylines, f32 metres |
| `lakes` | `Vec<Lake{level, area_m2, max_depth, outlet, cells: runs}>` | sparse runs of raster cells |
| `spawn` | `[f64; 2]` | maps to engine block (8, 8) of region (0, 0) (`:59-61`) — the artifact is positioned by its spawn |
| `edges` | `[EdgeKind; 4]` | N, E, S, W; "its variants are part of the format" (`world.rs:47-48`) |
| `drainage`, `volcanism`, `salinity`, `savagery` | `Vec<u8>` 0..100 | per cell |
| `volcanoes` | `Vec<[f32; 2]>` | sites |
| `biome` | `Vec<u8>` | per cell; `Biome` discriminant; validated on load (`:487-490`) |
| `region`, `regions` | `Vec<u16>`, `Vec<RegionRecord>` | per cell id + table; at most 65,536 regions |

Raw size is 17 bytes per cell (5 × u16 + 5 × u8 + u16) before delta + zstd: about 670 MB raw for
100 km / 16 m. The compressed size of a current-format 100 km world is **not stated anywhere I found**
(the design doc's 422 MB is the old format, `:85`).

**Not stored:** latitude, the pole choice, the lapse rate, wind bands, plate ids, convergence, the
initial world, discharge per cell, the knobs. The knobs are kept only in `worlds/<slug>/world.json`
(`WorldMeta.knobs`, `wg/world_store.rs:33-51`), beside the file; I did not find them in the database.
The runtime therefore **assumes the default lapse**: "the forge's default lapse (the world file does
not store the knob it was forged with)" (`wg/forged.rs:849-851`, `:1002`).

**Versioning.**
- `ARTIFACT_VERSION = 7` (`wg/forged.rs:35-42`): v3 quantised rasters, v4 edges, v5 latitude climate,
  v6 drainage/volcanism/salinity, v7 biome/regions/savagery. An older file is refused with
  "re-forge this world"; a newer one is refused too (`:435-442`). There is no migration of forge files —
  by decision: "Worlds in `worlds/` are git-ignored dev data, so no migration is needed" (design doc `:116-118`).
- A new raster is "one line here (plus its field)" in `qrasters()` / `byte_rasters()` (`:366-385`), but the
  arrays are fixed at 5 and 5 and mirrored in `QRasterId::ALL` / `ByteRasterId::ALL` (`:558-591`),
  `WorldGlobals::q_scale: [(f32, f32); 5]` (`:610`) and `Tile { q: [Vec<u16>; 5], bytes: [Vec<u8>; 5] }`
  (`wg/forged_tiles.rs:115-136`).
- Tiled form in the world store: `TILED_FORMAT_VERSION = 1` (`wg/forged_tiles.rs:35-37`), tiles of 256²
  cells carrying the 10 rasters + region ids + a precomputed coast-distance f32 + lake runs + river
  segments and 64 m buckets. Tables `forged_meta`, `forged_tiles`, `forged_world_map`, `forged_archive`
  (the original forge file, kept for re-tiling) — `crates/world-store/src/forged.rs:1-10`, migration
  `0002_forged_tiles.sql` (13 migrations exist, sqlite and pg). A decoded tile is about 1.4 MB
  (21 bytes × 65,536 cells); the cache budget is 256 MiB (`VOXEL_TILE_CACHE_MB`, `forged_tiles.rs:38-43`).
  `MAX_READ_RADIUS_CELLS = 4` bounds how far any sample reads and refuses cells finer than about 8 m (`:46-69`).
- `WORLD_MAP_VERSION = 1` (`wg/tile_map.rs:58-67`): the stored `WorldMap` is built once at write time;
  "bump it on ANY change to what `build_world_map` returns for the same world" (also `CLAUDE.md`,
  Server / store). A mismatch makes the server re-tile from the archived forge file.
- `world.content_crc` (crc32 over meta + tiles + world map) keys the worldgen disk cache
  (`crates/world-store/src/forged.rs:45-55`, `wg/forged.rs:842`).
- `WorldMeta` (`world.json`) records `region_blocks`, `chunk_blocks`, `artifact_version`; a mismatch with
  the build makes the world unloadable in the menu (`wg/world_store.rs:100-121`).

**Rules about changing it.**
- Biome discriminants: append only (`biome.rs:75-76`). `EdgeKind` variants: part of the format.
- Rock names: an unknown name fails the load (`wg/forged.rs:515-520`).
- `CLAUDE.md`: "A store read ERROR never regenerates a region … only a missing row generates";
  migrations `0004`/`0005` "are never edited (sqlx checksums them)"; a store with saved regions that
  predates `0011_voxel_terrain_returns` is refused: "re-forge it".
- **Saved regions freeze their terrain.** Once a region has been saved, its chunks and `ColumnGround`
  (with `water_top`) come from the store, not from `generate_region`. Changing the generator for an
  existing world would put new terrain next to old saved regions; in practice an overhaul means
  re-forging and starting worlds afresh.

---

## 10. From forged fields to voxels

**Entry points** (`wg/lib.rs`): `generate_region` (`:385`), `generate_columns` (`:403`, any window gives
exactly the same columns), `generate_region_forged` (`:523`), `coarse_tile` / `coarse_water` for LOD
(`:1007`, `:1078`), `ground_y` (`:443`). With a forged world installed (`forged::active()`, `:413`) the
`seed` argument is ignored. The procedural noise world (`pipeline.rs`, `VOXEL_WORLD_FILE=procedural`)
is a separate path used by tests and goldens; its `column()` must mirror the test-only `DensityFn`
graph bit for bit (`CLAUDE.md`, Worldgen).

**"Pure function of coordinates."** `wg/lib.rs:1-4`: "`generate_region` is a pure function of region
coordinates: same coords → identical output on every machine, which is what makes 'cycle out = park,
cycle in = restore-or-regenerate' safe". For forged worlds: pure function of coordinates + the installed
world (`CLAUDE.md`). On the sampling path "Only + - * /, floor and sqrt are used" (`wg/forged.rs:17`,
`wg/habitat.rs:8`, `wg/trees/placement.rs:6`). Neighbouring windows agree bit for bit because ground is
sampled on a world-aligned corner lattice (`wg/forged.rs:1452-1458`). So: the forge may be
non-reproducible, but **everything the runtime derives from the artifact must be exactly reproducible**.

**Stored versus added at runtime.**

| Stored (16 m cells) | Added at runtime (1 m blocks) |
|---|---|
| height raster | Catmull-Rom bicubic interpolation (`wg/forged.rs:1290-1311`) + two octaves of detail noise (32 m, 16 m), amplitude 0.25 m + 3 m × slope, max 1.8 m, faded out below 3 m so the coast does not move (`:67-75`, `:1318-1337`) |
| river polylines, lake cell runs | channels, levees, banks, lake berms (§6); water levels snapped to whole blocks (`:1359-1362`) |
| biome per cell | block-scale border dither: each block column reads the biome of the cell at its position displaced by up to 24 m by a 32 m noise (`:97-106`, `:1202-1216`) |
| temperature, height | snow line = height where annual mean T reaches −2 °C using the default lapse, bilinear (`:107-109`, `:1218-1230`); ice and glacier thickness |
| salinity, coast distance | salt flats (`:1243-1247`) |
| uplifted + bedding, rock column | per-column `geo` offset and the `strata` table; rock at any depth is `strata` at `s = y − geo` (`:1101-1123`, `wg/lib.rs:357-372`) |
| — | whole blocks only; a one-block rise becomes a slope chisel (`wg/lib.rs:86-92`, `slopes.rs`); trees; structures and paints (`:757-782`, `:971-980`) |

**Surface materials** (`biome_column` `wg/lib.rs:208-240`, `column_material` `:245-262`,
`Materials::ground_cover` `wg/material.rs:197-238`): a `GroundCover { surface, soil, soil_depth }` per
biome — grass/dirt for temperate biomes, dry grass (savanna), podzol (taiga), frozen dirt over gravel
(tundra), bare rock under ice (glacier), 8 blocks of sand (sand desert), gravel (wasteland), red sand
(badlands), mud over peat (marsh, swamp), thin soil (mountain); sand on beaches (surface ≤ 1 block above
the sea) and on lake/river beds; snow on top above the snow line (patches by noise in tundra). Below the
soil the block is the geology recipe's rock.

**Vegetation.** Only trees exist. A lattice of 8-block cells with at most one candidate each, a pure
function of `(seed, cell)` and the habitat; candidates compete with their 3 × 3 neighbours
(`wg/trees/placement.rs:1-20`). Species suitability = product of trapezoid niches on temperature,
moisture, slope, soil depth, salinity (`wg/trees/species.rs:10-61`), times the biome's density.
A tree belongs to the region of its base and never writes outside it (`wg/lib.rs:302-304`).
"Ground plants and farming" are a stated non-goal of the current design (design doc `:55`).
All of it is baked into chunk blocks at genesis: there is no seasonal foliage state.

**Vertical band.** Forged columns are scanned from block 4096 down to −1024 (`wg/lib.rs:52-55`); the
height raster's 16-bit span at 0.0625 m is 4,096 m; `max_peak_m` tops out at 3,500 m "forged terrain is
generated up to block 4096; the map ramps end at 3,500 m" (`knobs.rs:125`). Sea floor to peak must fit
in about 4 km. Earth-like ocean depths (kilometres) do not fit without widening the height step.

---

## 11. Runtime simulation: calendar, season, weather, day/night

**What exists.**
- A **calendar** in `crates/game/src/history/calendar.rs`: `DAY_MS = 10_000` real ms per game day,
  `TICKS_PER_DAY = DAY_MS / TICK_RATE` = 200 ticks (`TICK_RATE` is a 50 ms period,
  `crates/game/src/lib.rs:147`), 28 days × 12 months = **336 days per year** (`:6-11`), DF month names
  (`:14-17`). One game year = 56 real minutes; one month = 4 min 40 s. `CalendarTick(u64)` counts sim
  ticks since year 0; history starts at year 250 (`history/rules.rs:9`).
- The calendar belongs to the **coordinator / history**, not to region sims: the coordinator derives it
  from its own anchor (`crates/game/src/coordinator/mod.rs:265-269`) and advances history with it in
  `maintain` (`:427-435`); hosts receive `CoordMsg::Calendar` and extrapolate
  (`crates/game/src/world_manager/world_link.rs:198-208`), with the explicit note "nothing in the
  simulation reads it (clients show it)". No `calendar` reference exists in `region.rs`, `state.rs` or
  `region_runner.rs` (grep).
- History uses months as its step unit (`Step::SiteMonth`, monthly per-mille rates in `history/rules.rs:12-25`)
  with no seasonal variation.
- **No season, weather, temperature, wind, precipitation or day/night code** in `crates/game/src` or
  `crates/server/src` (grep for season / weather / day_night / time_of_day / temperature: hits are only
  `biome.rs`, `world_map.rs` and the history calendar files).
- Client: lighting is static. `SUN_DIRECTION` is a `const` (`crates/client/src/renderer/atmosphere.rs:28`),
  as are sun colour, illuminance and fog; the skybox is a fixed day sky. No sky cycle, no rain or snow
  rendering. The world map's snow cap is a constant 2,200 m (`wg/tile_map.rs:433`).
- The current design lists "Real-time weather and seasons" under **Non-goals**
  (`docs/superpowers/specs/2026-11-01-df-scale-world-terrain-design.md:63`). The DF research rates
  weather, temperature and freezing/thawing as "later" tiers and recommends an event-driven design:
  "per-region state + lazy tile effects" (`…/2026-10-20-dwarf-fortress-research/08-fluids-and-environment.md:94-113`).
  DF itself: with `POLE:NONE` "there are no seasons" (`…/2026-09-20-df-worldgen-research/01-pipeline-and-fields.md:41`).
- `docs/game-design.md` does not mention climate, weather or seasons beyond "forged up front with
  geology, climate, biomes and history" (`:17`).

**Determinism rules a runtime weather system must obey (quoted from `CLAUDE.md`).**
- "Rollback bar: `hash(before) == hash(after undo)`, bit-exact." Any weather state inside a region is
  rollback state with true inverse undo closures.
- "Hashing is the `StateHash` trait (`crates/state-hash`), never std `Hash` or serialized bytes."
- "No inherent `f32` transcendentals in sim paths (use `libm`); `BTreeMap`, never `HashMap`, for ordered
  sim state; randomness only via stateless hash rolls (`roll(tick, subject, salt)`); tunables as named consts."
  A seasonal curve (`cos` of day-of-year, `cos` of latitude) in the sim must use `libm` or integer tables.
- "Desync is caught by the server's state-hash interval (100 ticks, `VOXEL_STATE_HASH_INTERVAL`) — never weaken it."
- "The region is the threading unit: one OS thread per running region, nothing parallel inside one;
  regions talk only via `RegionInput`/`RegionOutput` messages … never shared memory."
- "Goal: a seam changes nothing observable. One world-numbered tick in every region (`WorldClock`)".
  Weather that is a pure function of `(world tick, position, stored climate)` satisfies this for free;
  weather with state that moves (fronts, storms) must cross seams as stamped transfers.
- "Sim code reads neighbours through `WorldView` (own ∪ halo) and writes anything it does not own as an
  `Intent`, never directly." "Sims stay region-local; the world offset exists only at the render boundary."
- "`carry_components` / `Client::arrive` destructure with no `..` on purpose: a new `Ecs`/`Client` field
  must be classified as travelling or not."
- Client: "any new per-world resource, system `Local` or entity root must be registered
  (`add_session_reset`, `SessionLocal`) or tagged `SessionScoped`"; wasm has "no shared-memory threads".
- "`just determinism-wasm` after touching sim math, the physics forks, or anything hashed/serialized."

**How the tick/clock works across regions and hosts** (`crates/game/src/world_clock.rs`):
`WorldClock { anchor_ms, anchor_tick }`, `tick_at(now_ms) = anchor_tick + (now_ms − anchor_ms) / TICK_RATE`
(`:11-33`); time is the manager's explicit `now_ms`, never `Instant` (`:1-4`). A region started late
joins at the world's current tick and catches up at most 5 ticks per burst (`:7-9`, `:66-70`). With
several hosts the anchor is a `ClockAnchor { unix_ms, tick }` in the world row; it "needs hosts on
different machines to have synchronised wall clocks (NTP): skew becomes tick skew" (`:35-52`).
Parked regions do not tick at all, so anything seasonal in a region must be recomputable from the world
tick when it wakes (the calendar is; accumulated per-region state such as snow depth is not, unless it
is a closed-form function of the date).

---

## 12. Tooling and gates

- `just forge-proto *args` — the forge CLI, PNGs, seed search (`justfile:685-686`,
  `wg/examples/forge_proto/`). `just forge-ui [port]` — the tuning page, every knob as a slider with a
  live preview (`justfile:714-721`, `--serve`). `just forge-bench size cell` (`:709-712`).
  `just forge-db` (`:649-650`), `just world-map` (`:587-588`).
- **`forge_quality` gate**: `forge_quality_matches_baseline` (`crates/worldgen/tests/forge_lib.rs:36-66`),
  16 km / 32 m, pinned to the **one-island layout and the no-pole 45° climate** on purpose, compared with
  `crates/worldgen/tests/fixtures/forge_quality_16k_32m.ron` (land share 0.33, p50 60 m, p90 1,084 m,
  max 2,327 m, θ 0.668, Hack 0.470, 30 mouths, 49 km of channel, Strahler 3, 6 lakes) within
  `QualityTolerance::DEFAULT` (10% heights/km, ±0.08 exponents, 25% counts, ±1 Strahler —
  `wg/forge/quality.rs:54-61`). Re-bless only with `FORGE_QUALITY_BLESS=1` "ONLY on purpose". It measures
  the island path, so it neither protects nor blocks continental changes unless erosion/hydrology change.
- **Seed-search score** (`wg/forge/quality.rs:169-205`): land share band per layout (continental 30–60%),
  ≥ 3 snow peaks, ≥ 3 mountain regions bordering land, ≥ 3 forest regions, river order ≥ 4, ≥ 10 distinct
  biomes, ≤ 2,750 regions, a glacier expected when a pole is on the map.
- **Bit-exact pins an overhaul will hit:**
  - `island_layout_unchanged` (`wg/forge/world.rs:958-992`): crc32 of `h` and `uplift` of the one-island
    initial condition at two sizes, and of `h` after 15 erosion steps (`0x5771_874C`). Any change to
    `World::new`'s island branch, the noise, `climate::compute` or `erosion::step` breaks it.
  - `scene_hash_golden` (`crates/worldgen/tests/scene_hash_golden.rs`) and `game` `wasm_determinism`:
    the **procedural** world, region (0, 0); they skip when a forged world file is set. They break only
    if shared code changes (`pipeline.rs`, `noise.rs`, materials, `ColumnGround` layout, trees).
  - `parallel_step_matches_serial`, `step_with_reused_scratch_matches_fresh` (`erosion.rs:307`, `:335`).
- **Behavioural tests tied to today's model:** climate (`climate.rs:366-631`: lapse exactly 6.5 °C/km,
  rain-shadow direction per band, latitude table on flat land, band edges without a step, resolution
  independence), plates (§4), edges (`world.rs:833-916`, `mod.rs:570-597`), schedule (`mod.rs:438-471`),
  `default_forge_reaches_kilometre_peaks`, `forged_fields_follow_the_terrain`
  (`tests/forge_lib.rs:75-101`), `classify_table` and drainage anchors (`biome.rs:389-482`, `fields.rs:498-515`),
  presets in range (`presets.rs:164-`), world-map tests (`crates/worldgen/tests/tile_map.rs`),
  forged-world and tile round-trips (`tests/forged_world.rs`, `tests/forged_tiles.rs`, `tests/water_recipe.rs`).
- Presets (`wg/forge/presets.rs:17-140`): 14 named knob-override lists; the menu shows "Custom" when none
  matches exactly, so changing a default silently renames what users see.
- Testing rules from `CLAUDE.md` that apply: never run whole suites as a routine check; tests over 100 ms
  are `#[ignore]`d and run via `just test-full`; most forge tests are in that set.

---

## 13. Constraints, risks and extension points

**A. Scale mismatch (the biggest design question).**
- The world is 16–100 km across. With both poles on the map, 180° of latitude spans at most 100 km:
  **0.56 km per degree** (Earth: 111 km). A whole climate zone is 5–15 km wide; a player walks from
  tundra to desert in about 25 km. The default 5° meander is about 2.8 km at that scale.
- Erosion, rivers, floodplains, lakes, volcano cones and the climate's relaxation lengths are in
  **physical metres** and are not scaled with the world (9 km inland relaxation, 5 km sea recharge,
  8 km volcano spacing, 3 km cones, 2 km edge taper, 8 km interior ranges, 140–420 m lakes, 12 km savagery).
  Coast noise and plate geometry *are* scaled (`REFERENCE_SIZE_M`, `world.rs:18-21`). The forge is thus
  a 1:1 landscape model wearing a planet-scale climate. An Earth-like overhaul must pick one: a real
  planet abstracted at coarse cells (then 16 m cells and the 39 M-cell budget are the wrong tool for the
  global stage) or a ~100 km "pocket planet" with compressed latitude (then plate tectonics is a
  stylised generator, not a physical one).
- Height: sea floor ≥ −90 m, peaks ≤ 3,500 m, block scan −1,024..4,096, 16-bit height at 6.25 cm.
- Counts that do not grow with world size: `lake_basins` 9, `interior_ranges` 2, `volcano_count_max` 6,
  `plate_count` 7, `continent_count` ≤ 4.
- Forge cost at the top size is unmeasured for the current code (§1) and RAM is capped at 12 GiB for
  in-client forging. A plate simulation and a seasonal climate add to it.

**B. Things hard-coded to today's geometry.**
- Square world only (one `size_m`, `n × n`). An equirectangular-style map (2:1) needs a second dimension
  through `World`, every raster, tiles, `WorldGlobals::validate`, the world map and the menu.
- Latitude is a function of the **row** only, north = z 0; winds blow along rows only.
- Sea edges are chosen by the seed, not by the user; polar edges cannot be requested as sea or ice.
- No wrap. An equator-centred world with real edges on east and west has continents cut by the map
  border; the unimplemented wrap plan is the natural companion to goal (1).
- `Mountain` is height ≥ 1,200 m regardless of latitude (a polar plateau and a tropical one are the same
  biome); the snow cap on maps is a fixed 2,200 m.
- The runtime's lapse rate is the default knob, not the world's.

**C. Places that assume annual means.**
- `Climate` struct and both stored climate rasters (`climate.rs:102-110`, `forged.rs:294-297`).
- `CellClimate` and `classify` (`biome.rs:193-207`, `:237`); Köppen-style classes need at least the
  warmest/coldest month temperature, the driest month and the summer/winter precipitation split.
- `LATITUDE_PRECIP_MM` (one annual table), the single wind-band layout, `discharge_weight`
  (annual rain → river size; a seasonal river needs a seasonality measure per reach).
- Tree niches (`species.rs`), `habitat.rs` (`REFERENCE_PRECIP_MM = 900`), the snow line
  (`SNOW_LINE_C = −2` on the annual mean), ice thickness, glacier thickness, `FROZEN_C`/`GLACIER_C`.
- `TileSummary.temperature_c` / `precip_mm` and their map layers and colour ramps.
- The seed score's snow-peak count (`quality.rs:282`).

**D. Storage growth.**
- 12 monthly temperature + 12 monthly precipitation rasters at u16 = +48 bytes per cell on top of 17:
  about +1.9 GB raw at 39 M cells, and tiles grow from about 1.4 MB to about 4.5 MB decoded (the 256 MiB
  cache then holds about 57 tiles of 625).
- Cheaper encodings that fit the existing `QRaster` scheme: mean + annual amplitude + phase (2–3 extra
  rasters per variable), or monthly fields on a coarser climate grid (climate varies over kilometres;
  the 16 m grid is needed only for the lapse-rate term, which can be re-applied at runtime from height).
- Every added raster touches the fixed-size arrays listed in §9 and bumps `ARTIFACT_VERSION`,
  `TILED_FORMAT_VERSION`, and (if the map shows it) `WORLD_MAP_VERSION` and the `TileSummary` wire type.
- 72 biomes still fit a u8; the `u16` region cap and the merge rule are unaffected, but more biome kinds
  mean more, smaller regions unless `RegionType` grouping absorbs them (regions are typed by
  `RegionType`, not by biome, so 72 biomes can keep ~10–15 region types).

**E. Water features versus the current model** (which mechanisms are missing, not how to build them).

| Feature | Blocker today |
|---|---|
| Bays, gulfs, peninsulas | exist only as coast-noise shapes; no tectonic or drowned-valley origin |
| Fjords, rias, estuaries | ocean mask fixed before erosion; land clamped ≥ 0.05 m; no sea-level change; no glacial erosion law |
| Deltas, braided rivers | D8 single-receiver routing; no sediment transport or deposition at the coast (silting exists only in depressions) |
| Lagoons, barrier islands, atolls | no coastal process; sea floor is a clamp of the layout scalar |
| Glacial lakes, cirques, U-valleys | lakes are 9 carved bowls on valley floors; no ice flow in the forge |
| Salt lakes | present (water-balance rule) but depend on the same carved basins; no playa without a lake |
| Seasonal rivers, wadis | no seasonality; a reach is either above the discharge threshold or absent; runtime water is static |
| Glaciers | a biome plus a baked ice thickness; not connected to precipitation or to valleys |
| Sea ice | baked 1–4 solid blocks where the annual mean is below 0 °C; `FrozenOcean` biome at −5 °C |
| Waterfalls, rapids | emerge only as block-snapped steps between flat river bodies |
| Deep ocean, trenches, ridges, shelves | −90 m clamp; plates do not shape the sea floor |
| Springs, aquifers, wetlands' open water | no groundwater; marsh/swamp are ground covers with no standing water |

**F. Runtime climate risks.**
- No region-sim consumer of the calendar exists; the calendar lives in the coordinator and reaches hosts
  as a message that "nothing in the simulation reads". A sim-visible date must come from the world tick
  (every region has it) rather than from that message.
- Snow, ice, foliage and ground cover are **blocks baked at genesis and saved with the region**. Seasonal
  change means either mass block edits (rollback + save + network cost), or a derived overlay the
  renderer and sim compute from `(date, stored climate)` without touching chunks. The second fits the
  existing "recipe" pattern (`ColumnGround`), the first fights it.
- Parked regions do not tick; anything accumulated must be closed-form in the date.
- Weather must be identical on server, native client and wasm client: `libm` only, integer or fixed-point
  preferred (DF's own temperature is 16-bit integers — research doc 08 `:69-71`).
- The static-water engine cannot change levels; seasonal rivers and freeze/thaw depend on the unbuilt
  flow/ledger phases and on two still-open user decisions (`TODO.md:143-145`).
- Multi-host worlds number ticks from wall clocks; calendar-driven weather inherits that and needs no
  extra synchronisation, but any stateful weather crossing a host boundary goes through the peer link
  rules (`Reliable::InCut` for state a cut accounts for).

**G. Process constraints.**
- Stored artifact changes force a re-forge of every existing world; saved regions are not regenerated.
- The island layouts are pinned bit for bit (`island_layout_unchanged`) and the quality gate measures
  them; keeping them as a frozen legacy path is the cheapest way to keep both green while the
  continental path is replaced.
- `game`, `server` and `worldgen` must stay free of Bevy and windowing code (`CLAUDE.md`, Project).
  `Biome`, `classify` and `TileSummary` live in `game`, so biome work touches the wire protocol crate.
- Knob defaults *are* the tuning (`knobs.rs:4-5`); presets are exact-match lists; `CLAUDE.md` asks to
  "check at 16 m cells before keeping a value; the `forge_quality` gate must pass".
- New block types for new ground covers must exist in the block manifest; `BiomeBlocks::from_names`
  turns **all** biome covers off if any one name is missing (`material.rs:62-82`).

**H. Extension points that already exist.**
- `pole = Both` and `latitude_span_deg` (goal 1, forge side).
- `World { h, ocean, uplift, outlet, edges }` as the contract between "tectonics" and "erosion": a new
  stage only has to fill it (and do so consistently per level).
- Per-cell `uplift`, per-rock erodibility and `uplifted`-driven exhumation as carriers for belt age.
- `Knobs` table + `forge-ui` for any new tunable; `PreviewLayer` / `MapLayer` for new map layers.
- `QRaster` / byte-raster lists and the tile codec for new per-cell fields.
- `CellClimate` as the single input struct of the classifier; `regions::derive` re-classifies from stored fields.
- `ColumnClimate` (`forged.rs:854-870`) as the single struct the block column's cover, snow and ice derive from.
- `GroundCover` per biome; `Species::biomes` + niches for vegetation.
- `EdgeKind` (append a variant) and the written wrap plan.
- The calendar constants and `WorldClock` for a date every region can compute.
- The planned water ledger ("is the world ledger the water cycle") as the meeting point of runtime
  climate and runtime water.

**Not verified in this audit:** actual forge time, RAM and file size of a current 100 km / 16 m world;
whether the world's knobs are stored anywhere in the database (only `world.json` was found); the Slint
sources for the map window (only the Rust side was read); `wg/islands.rs`, `wg/sites/`, `wg/structures.rs`
and `wg/cache.rs` beyond their headers; the docs `02-rivers-lakes-erosion.md`, `03-geology.md`,
`04-adapting-to-chunked-generator.md` and the `world-forge-f0..f2` findings were not read in full.
