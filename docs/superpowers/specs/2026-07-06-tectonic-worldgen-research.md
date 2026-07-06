# Tectonic-style terrain generation — research

**Date:** 2026-07-06 (updated same day with repo-verified constants)
**Status:** Research (not yet a design spec). Blocks on vertical chunk stacking
(`docs/superpowers/specs/2026-07-06-vertical-chunk-stacking-design.md`) — terrain
worth generating needs a vertical axis first.
**Goal:** Reimplement the Tectonic Minecraft mod's worldgen *mechanisms* (not
Minecraft itself) in this engine's deterministic, chunk-based worldgen
(`crates/worldgen`, currently `generate_region(RegionCoords)` → parity-checkerboard floors).

**Provenance.** Part 1 (mechanisms) came from a fan-out deep-research pass (18 sources,
25 claims adversarially verified). Part 2 (concrete numbers + architecture) came from
**reading the actual repo** — `github.com/Apollounknowndev/tectonic`, commit fetched
2026-07-06, path
`src/common/main/resources/resourcepacks/tectonic/data/{minecraft,tectonic}/worldgen/`.
Where the two disagree, **the repo wins** and the correction is called out below.

---

## TL;DR

Terrain is one scalar field: a **density per voxel**; solid where `final_density > 0`,
else air/fluid. That field is built by a graph of **composable density functions** —
`noise`, `spline`, `add/mul/min/max`, unary transforms, `range_choice`, plus
cache wrappers. The horizontal shape comes from a handful of low-frequency 2D noise maps
(continentalness, erosion, ridges) pushed through **nested spline curves** into a terrain
**height offset**, a vertical **stretch factor**, and a **jaggedness** term; a vertical
**depth gradient** turns that into a 3D density with a surface. Tectonic is this standard
post-1.18 pipeline **retuned to continent scale** — plus a substantial amount of extra
structure (see corrections).

To port it: implement a `DensityFn` graph evaluator + a `Spline` type, wire the
continentalness/erosion/ridges → offset/factor/jaggedness shaping, and copy the scalar
constants and noise octaves in "Verified numbers" below. All of it is a pure function of
`(x, y, z, seed)` and must run through the vendored `libm`/`ordered-float` path for
cross-machine bit-exactness.

---

## Corrections to the earlier (web-only) research

1. **"Tectonic is a pure datapack / zero Java files" — OUTDATED.** True for old versions;
   current Tectonic (the 26.x tree) ships as a **Java mod** with 4 custom density-function
   types — `tectonic:config_noise`, `tectonic:config_constant`, `tectonic:config_clamp`,
   and `tectonic:invert` — plus mixins. Those custom types exist **only to inject
   runtime-config values** (an in-game GUI lets players tune terrain scale/height). The repo
   also ships a **datapack overlay** (`overlay.datapack/`) that replaces every custom type
   with **baked constants and vanilla primitives**. So the core claim still holds *for the
   datapack variant*: the terrain math is expressible in vanilla density functions + fixed
   constants. **Reimplement the datapack variant** — it's the deterministic, self-contained one.
   - `config_noise(x,z) = noise((x·scale)+shiftX, (z·scale)+shiftZ)·mult + offset` — i.e. a
     plain shifted-noise node whose scale/mult/offset come from config instead of JSON.
     `invert(x)=1/x`. These are trivial to replicate; you just hardcode the constants.

2. **The pipeline is far more elaborate than the generic Minecraft description.** Beyond
   continentalness/erosion/PV it adds: a **continent-vs-island duality**, a **card-suit
   "region" system** (spade/heart/club/diamond — a meta-terrain-archetype layer),
   **domain-warped mountain ridges**, and **underground rivers + lava tunnels** as separate
   density fields folded into `final_density`. Details below.

3. **Confirmed:** the `depth` input is a vertical gradient, not noise — Tectonic's is
   `y_clamped_gradient(from_y=-2048→to_y=2048, 17→-15)`, i.e. exactly **−1/128 per block
   downward**, biased by the terrain height offset. This matches the earlier claim.

---

## Verified numbers (datapack variant — copy these)

**World frame** (`noise_settings/overworld.json`):
- `min_y = -64`, `height = 384` → world spans **Y −64 … 320**; `sea_level = 63`.
- `size_horizontal = 1`, `size_vertical = 2` → density is sampled on the **vanilla coarse
  interpolation grid** (≈4 blocks horizontal × 8 vertical per cell) and **trilinearly
  interpolated**, not evaluated per-voxel. `aquifers_enabled` and `ore_veins_enabled` true.
  (Confirm the exact cell→block mapping when implementing; the point is it's a coarse grid + interp.)

**Noise octaves** (`worldgen/noise/*`, `firstOctave` = base wavelength 2^|octave| blocks):
| Noise | firstOctave | amplitudes |
|---|---|---|
| `minecraft:continentalness` | −10 (~1024) | [1.75, 1, 2, 3, 2, 2, 1, 1, 1] |
| `minecraft:erosion` | −10 (~1024) | [2, 1.75, 1.5, 1.5, 1.3, 1, 1, 1, 1] |
| `minecraft:ridge` | −8 (~256) | [1, 2, 1] |
| `tectonic:mountain_ridges/base` | −8 | [1, 0.2] |
| `tectonic:mountain_ridges/detailed` | −7 | [0.2, 1] |
| `tectonic:region/selector` | −11 (~2048) | [1, 2.1, 1.5, 1.7, 1.4, 2, 2] |

**Horizontal sampling scales** (`xz_scale` on the `shifted_noise`/`noise` nodes, datapack):
- continentalness sampled at **`xz_scale = 0.13`**, island continents at **0.11** — *these,
  times the ~1024-block base wavelength, are what make continents thousands+ blocks wide.*
- region selector `xz_scale = 1.1`; mountain base `1.7` (y_scale 1), detailed `2`,
  weathering `2.5` (y_scale 2); islands `jagged` noise `xz_scale = 1500` (tiny sharp peaks).

**Scalar constants** (`__constants/*`, datapack overlay):
- `ocean_offset = -0.8`, `ocean_depth = -0.22`, `deep_ocean_depth = -0.45`
- `max_offset = 1.95`, `min_offset = -0.6`, `vertical_scale = 1.125`, `flat_terrain_skew = 0.1`
- `slope_upper = y_clamped_gradient(Y 290→310, 1→0)` — terrain is forced off above ~Y290
  (this is why peaks *approach but cap near* the build limit).
- `slope_lower = y_clamped_gradient(Y −64→−48, 0→1)` — floor.

---

## The pipeline, in evaluation order

Namespaces: `tectonic:*` = mod's own functions; `minecraft:*` = vanilla noise-router slots
it overrides. All paths below are under `data/tectonic/worldgen/density_function/` unless noted.

1. **`noise/raw_continents`** = `clamp(ocean_offset(-0.8) + spline(abs(continentalness)), -1, 2)`.
   Folding the continent noise through `abs` then a rising spline (0→0, 0.4→0.575, 0.48→0.68)
   and subtracting 0.8 makes **most of the map ocean** (negative), with landmasses where
   `|continentalness|` is large. This single 2D value is the backbone.

2. **Continent/island split.** `island_selector = 1 where raw_continents ∈ [-1, -0.5) else 0`
   (deep ocean gets island chains); `continent_selector = 1 - island_selector`.
   **`noise/full_continents`** = `island_selector·raw_islands + continent_selector·spline(raw_continents)`
   — this is the "continentalness" value fed to biome parameters and ocean-depth splines.

3. **Terrain height → `terrain_spline/offset/final`.** Core = `clamp( island_sel·offset/islands
   + continent_sel·(vertical_scale-boosted offset/continents), min_offset -0.6, max_offset 1.95 )`,
   biased by `-0.50375` and Minecraft's blend system (`blend_offset`/`blend_alpha`, for
   stitching to adjacent pre-generated terrain — droppable in a greenfield engine).
   - `offset/continents` is the big one: a **spline keyed on `raw_continents`**, whose values
     are *themselves* splines on `erosion` → `region_selector` → `temperature_index` →
     `vegetation_index` → `ridges`, bottoming out in per-region height shapes (jungle pillars,
     rolling hills, plateaus, dunes…). Ocean depth is set by nested splines on the
     `deep_ocean_depth`/`ocean_depth` constants at the negative end.

4. **`depth`** = `y_clamped_gradient(-2048→2048, 17→-15)  +  offset/final` (cached). The
   gradient gives density-falloff-with-height (−1/128 per block); adding the offset raises/lowers
   where the surface sits.

5. **`terrain_spline/factor/*`** (vertical stretch, always positive). `range_choice` on
   `continent_selector` picks `factor/continents` vs `factor/islands`. `factor/continents` is a
   spline on `raw_continents` → `erosion_folded`(=`abs(erosion)`) → `ridges`, with values
   **5.6 down to 0.6** (steep, ridged high country pulls factor toward 0.6).

6. **`terrain_spline/jaggedness/*`** — spline on `raw_continents`→`erosion_folded`, up to ~0.2,
   nonzero only in high-continent/low-erosion areas. Islands' jaggedness is multiplied by the
   high-freq `minecraft:jagged` noise (`xz_scale 1500`) for small sharp peaks.

7. **`mountain_ridges/*`** — the long-coherent-range trick. `noiseshift/base` is a low-freq
   noise; `shifteddetail` is a detail noise **domain-warped** by `abs(slopeX)·3000` and
   `abs(slopeZ)·3000` (slopes derived from the base noise) — warping by up to 3000 blocks is
   what bends noise blobs into **long ridge lines** instead of round bumps. `ridges = base +
   0.6·shifteddetail`. `weathering = 1 - clamp(spline(abs(weathering_noise)))` smooths/erodes
   a fraction of ridges.

8. **`sloped_cheese`** (the surface field) ≈
   `4 · quarter_negative( (depth + depth_additive + jaggedness·ridge_terms) · factor )`
   `+ dune_terms + spline(full_continents)·roughness + spline(full_continents)·ocean`.
   `base_terrain = sloped_cheese`. (`quarter_negative` halves negative inputs' slope — makes
   air taper gentler than ground.)

9. **Caves & fluids as separate fields:** `caves` (`cave/cheese`, `cave/noodle`),
   `underground_river/total`, `lava_tunnel/total` — each an independent density field.

10. **`final_density`** = `min( squeeze(0.64 · interpolated(blend_density( slope-clamped
    min(base_terrain, caves) ))), cave/noodle ) + min(0.0002, underground_river) + lava_tunnel`.
    The `slope_upper`/`slope_lower` gradients here clamp terrain into the Y range (mountains
    taper by Y290–310). `> 0` ⇒ stone; below sea level empty ⇒ water via aquifers.

### The card-suit "region" system

`region/selector` noise (firstOctave −11, ~2048-block base wavelength, sampled `xz_scale 1.1`)
plus `temperature_index`/`vegetation_index`/`ridges` select among **spade / heart / club /
diamond** (and `_weak` variants) — meta-terrain archetypes, each a bundle of height splines
(e.g. `region/heart/rolling_hills`, `region/club/plateau_spline`, `region/club/badlands_ridge`,
`region/diamond/dune/*`, `region/heart/jungle_pillar`). This is a **terrain-shape layer,
orthogonal to biome block/mob assignment** — it decides *what the ground looks like* in a
patch, not what blocks/mobs go there. A minimal port can skip regions entirely and use a single
offset/factor/jaggedness triple; regions are how Tectonic gets variety.

---

## What this means for THIS engine

1. **Determinism is the main risk.** Every noise sample and spline eval must be bit-exact
   across client/server (rollback bar: `hash(before) == hash(after undo)`). Route the whole
   noise path through the same `libm` / `ordered-float` discipline the vendored forks already
   enforce for physics — **never `std` transcendentals** in the noise path. See CLAUDE.md
   "Vendored Forks".

2. **Coarse-grid vs per-voxel.** Tectonic (via vanilla) samples density on a coarse grid
   (~4×8) and trilinearly interpolates. Options for our 32³ chunks: full per-voxel (simplest,
   exact) or coarse-grid+interp (faster, but the interpolation joins the deterministic spec).
   Recommend **per-voxel first**, optimize later if profiling demands it.

3. **`worldgen` stays pure.** `generate_region` is a pure `coords → chunks` function; that
   purity is what makes park/restore/regenerate safe. The density graph must stay a pure
   function of `(position, seed)` — no global state, no RNG side effects. With vertical chunks
   the signature generalizes to a 3D region/column key.

4. **Suggested Rust shape.** A `DensityFn` enum (nodes: `Const`, `Noise{octaves, xz_scale,
   y_scale, shift}`, `YGradient`, `Spline{coord: Box<DensityFn>, points}`, `Add/Mul/Min/Max`,
   `Abs/Square/Cube/HalfNeg/QuarterNeg/Squeeze/Clamp`, `RangeChoice`, `Cache2d/FlatCache/CacheOnce`)
   evaluated at `(x,y,z)`. A `Spline` is control points `(location, value: f64|Spline, derivative)`
   with the same cubic-Hermite interpolation Minecraft uses. Splines nest (a point's value can be
   a sub-spline on another coordinate) — this recursion is load-bearing, not optional.

---

## Remaining work / open questions

The scalar constants, noise octaves, sampling scales, and pipeline structure are now
**captured above**. What's *not* transcribed here (deliberately — it's ~590 control points
across 38 spline files) is the exhaustive per-spline control-point tables. When implementing:

1. **Re-clone the repo and read the spline JSON directly** for exact control points —
   especially `terrain_spline/offset/continents.json` (527 lines), `.../factor/continents.json`,
   the `region/*` splines, and `depth_additive.json`. Paths are stable under
   `data/tectonic/worldgen/density_function/`. Start from the **datapack overlay** so
   `__constants` resolve to concrete numbers, not `config_*` nodes.
2. **Decide per-voxel vs coarse-grid** (determinism/perf trade-off, item 2 above).
3. **Scope v1**: single offset/factor/jaggedness triple vs. the full card-suit region system;
   caves/underground-rivers/lava-tunnels in or out. Precede with `superpowers:brainstorming`.
4. **Spline interpolation math**: reproduce Minecraft's exact cubic-Hermite spline eval
   (location/value/derivative) — a small but determinism-critical routine to get bit-identical.

---

## Source paths (in-repo, for re-fetch)

Repo: `github.com/Apollounknowndev/tectonic`, base
`src/common/main/resources/resourcepacks/tectonic/`.
- World frame: `data/minecraft/worldgen/noise_settings/overworld.json`
- Overridden vanilla slots: `data/minecraft/worldgen/density_function/overworld/noise_router/{final_density,continents,depth,erosion,initial_density_without_jaggedness,ridges,barrier}.json`
- Tectonic terrain: `data/tectonic/worldgen/density_function/{base_terrain,sloped_cheese,depth,continent_selector,island_selector}.json`,
  `terrain_spline/{offset,factor,jaggedness,ocean,roughness}/*`, `mountain_ridges/*`,
  `noise/{raw_continents,full_continents,raw_islands,region_selector}.json`, `region/*`
- Noise octaves: `data/tectonic/worldgen/noise/**`, plus `minecraft:{continentalness,erosion,ridge}`
- Constants (baked): `overlay.datapack/data/tectonic/worldgen/density_function/__constants/*`
- Custom DF types (mod variant, for reference): `src/**/worldgen/densityfunction/{ConfigNoise,ConfigConstant,ConfigClamp,Invert}.java`
- Refuted earlier claim: Tectonic does **not** depend on TerraBlender for base biome placement
  (it uses vanilla multi-noise); TerraBlender/Biolith are add-on-biome integration only.
