# Density-pipeline worldgen — design spec

**Date:** 2026-07-07
**Status:** Design (approved for planning).
**Depends on:** vertical chunk stacking (`docs/superpowers/specs/2026-07-06-vertical-chunk-stacking-design.md`, landed) and the block/chisel data model (`docs/superpowers/specs/2026-07-06-block-abstraction-data-model-design.md`).
**Research basis:** `docs/superpowers/specs/2026-07-06-tectonic-worldgen-research.md`.

**Goal.** Replace the parity-checkerboard flat floors in `crates/worldgen` with
continent-scale terrain, by reimplementing Tectonic's post-1.18 density-function
*mechanism* — a composable `DensityFn` graph driven by low-frequency noise pushed
through nested Hermite splines — and tuning it with **our own** splines and constants.
We build the reusable engine; we do not transcribe Tectonic's ~590 spline control points.

---

## Decisions (locked during brainstorming)

1. **Fidelity: mechanism, our own tuning.** Build the `DensityFn`-graph + nested-`Spline`
   machinery and the continentalness/erosion/ridge → offset/factor/jaggedness pipeline,
   but author our own (few, hand-tuned) splines rather than porting Tectonic's exact
   numbers. We own the terrain look and carry no dependency on re-cloning the mod.
2. **v1 feature scope: base continents + surface materials.** Density pipeline producing
   solid/water plus a simple height/depth-based material pass (stone/dirt/grass/sand).
   **Deferred to their own future specs:** the card-suit "region" archetype system, caves,
   underground rivers, lava tunnels.
3. **Surface granularity: smooth per-voxel surface**, resolved via a two-tier
   coarse-block-find then per-voxel-smooth strategy (§4) so voxel-resolution cost stays
   bounded to the surface band.
4. **Add a world seed** to the generator now (§7) — worldgen is currently seedless.

---

## Scale model (why Minecraft's constants port directly)

From `crates/game/src/block.rs` and `protocol.rs`:

- `1 block = 16³ voxels = 1 m³` (`BLOCK_VOXELS = 16`).
- A `Chunk` is 32³ voxels = **2³ blocks** (`CHUNK_BLOCKS = 2`).
- A region is `REGION_CHUNKS = 8` chunks per axis = **16×16 blocks = 16 m** wide
  (`REGION_SIZE = 256.0` world units, where a world unit is one voxel).

Minecraft's entire constant set is keyed in **blocks (1 m units)**, so our **block grid
is Minecraft's density grid** — constants map 1:1 at block scale. Continentalness' ~1024 m
base wavelength spans **64 regions**, i.e. genuinely continent-scale. The 16× finer voxel
layer is sub-block chisel detail (§4).

**World frame (proposal, tunable during implementation):** `sea_level = 64` blocks;
terrain band ≈ Y −64 … 320. The `Column` type is vertically unbounded and sparse, so only
the surface band around each column is materialized; below reads as a stone sentinel, above
as air/water.

---

## 1. Architecture & module layout

`crates/worldgen/src/` grows from one `lib.rs` into focused, independently testable modules:

| Module | Responsibility | Depends on |
|---|---|---|
| `noise.rs` | Deterministic gradient (Perlin) noise + octave summation. Pure `f64`. | — |
| `spline.rs` | `Spline` type + Minecraft-style cubic-Hermite eval; nesting. | — |
| `density.rs` | `DensityFn` node enum + evaluator over world-meter `(x,y,z)`. | `noise`, `spline` |
| `pipeline.rs` | Builds *our* concrete graph + tuned constants/splines → `final_density`. | `density` |
| `material.rs` | Solid voxel `(depth-below-surface, y)` → `BlockId`. | `game` |
| `lib.rs` | `generate_region` orchestration: surface-find, voxel smoothing, `Column` assembly. | all |
| `main.rs` | Region-gen bench (extended). | `lib` |

Each unit answers "what does it do / how do you use it / what does it depend on" in
isolation. `worldgen` stays a pure `coords → columns` function (no clocks, no RNG side
effects) — that purity is what makes park/restore/regenerate safe in the multi-region world.

## 2. The engine — `DensityFn` + `Spline` + `Noise`

The reusable mechanism, worth building well independent of tuning.

**`DensityFn`** is a tree evaluated at a world position; a pure `fn(x, y, z, seed) -> f64`.
Node set (v1):

- Leaves: `Const(f64)`, `Noise { id, xz_scale, y_scale }` (samples a named noise at meter
  coords), `YGradient { from_y, to_y, from_v, to_v }` (the vertical depth falloff).
- Binary: `Add`, `Mul`, `Min`, `Max`.
- Unary: `Abs`, `Clamp { min, max }`, `Squeeze`, `HalfNegative`, `QuarterNegative`
  (the shaping transforms the pipeline needs).
- `Spline { coord: Box<DensityFn>, spline: Spline }` — remaps a sub-expression through a curve.
- `RangeChoice { input, min, max, in_range, out_range }` — selects between two sub-graphs.
- Cache wrappers (`FlatCache`/`Cache2d`/`CacheOnce`) — **optional in v1**, an optimization;
  correctness must not depend on them. Include only if the surface-find loop needs them for
  acceptable perf.

**`Spline`** is control points `(location, value, derivative)` where `value` is either a
scalar or a nested `Spline` (recursion is load-bearing — a point's value can itself be a
spline on another coordinate). Eval is Minecraft's cubic-Hermite between adjacent points,
clamped/linear-extrapolated outside the point range. This is a small, determinism-critical
routine — unit-tested against hand-computed values.

**`noise.rs`** is standard Perlin gradient noise with per-octave amplitude summation,
seeded by hashing `(world_seed, noise_id)` into the permutation/gradient tables. Perlin uses
only multiply/add/`floor` and a polynomial fade — **no transcendentals, no fma** (see §6).

## 3. The shaping pipeline (our tuning)

Same *shape* as Tectonic, our *numbers*. Built in `pipeline.rs`:

- Low-frequency 2D noises: `continentalness` (base wavelength ~1024 m), `erosion`, `ridges`.
- `raw_continents = ocean_offset + spline(|continentalness|)` — most of the map is ocean
  (negative); landmasses where `|continentalness|` is large.
- **offset** (surface height vs sea level): spline on `raw_continents`, inland values
  modulated by an `erosion` sub-spline — high erosion → flat plains, low erosion → allow
  mountains. Clamped to `[min_offset, max_offset]`.
- **factor** (vertical stretch, always positive): spline on continents/erosion — large in
  low-erosion inland = tall mountains, small elsewhere.
- **jaggedness**: spline on erosion, multiplied by a `ridges` term for sharp peaks; ~0 in
  low-relief areas.
- `depth(y) = y_gradient + offset` (the gradient gives density falloff with height; adding
  offset raises/lowers where the surface sits).
- `final_density ≈ (depth · factor) + jaggedness · ridge_term`, clamped into the Y band by
  upper/lower slope gradients (`slope_upper` tapers peaks near the ceiling; `slope_lower`
  floors it).

**Threshold:** solid where `final_density > 0`; else, below `sea_level` → water, above → air.

The spec fixes the *structure and node wiring*, not final magic numbers. Exact spline control
points and constants are tuned iteratively against the visual bench during implementation.

## 4. Surface resolution — coarse find, fine smooth

Two-tier evaluation bounds per-voxel cost to the surface band:

1. **Coarse per-block (1 m).** For each of the 256×256 region column footprints (in voxel
   units; = 16×16 = 256 block columns per region, each block column is a 1 m² cell), walk
   down in 1 m steps evaluating `final_density` per block to locate the sign-change block(s)
   → the surface band. Whole blocks below become uniform solid `SubChunk`s (or the `Column`
   `below` sentinel); whole blocks above become uniform air/water — **no voxel work**.
2. **Fine per-voxel (1/16 m).** Only within blocks the surface crosses, sample density at
   voxel resolution and set voxels solid/air, producing a chiseled straddle block → smooth
   slopes. Because the depth gradient dominates `∂density/∂y`, this is a cheap local refine,
   not a full 3D voxel scan.

The result packs directly into the existing `Column::new(base_sub_y, below, above, sub_chunks)`
shape and the `Chunk` block+chisel representation (mirrors how `flat_floor_with` already
emits a chiseled straddle slab).

Water below sea level is emitted as a water `BlockId` fill in the air-but-submerged band
(uniform `SubChunk`s where a whole block is underwater; chisel only at the exact shoreline
straddle if needed).

## 5. Surface materials

A thin deterministic post-pass in `material.rs` over solid voxels, keyed only on
depth-below-surface and `y` (no extra noise in v1):

- Topmost solid block: grass, or sand when at/just below sea level (beaches).
- Next few blocks down: dirt.
- Below that: stone.

Uses registry `BlockId`s already available to `game`. Because it is a pure function of
position it adds no determinism surface beyond §6.

## 6. Determinism rules (the primary risk)

Rollback bar: `hash(before) == hash(after undo)`, bit-exact, and worldgen also runs on
**wasm32** (the offline client, `crates/client/src/local_server.rs`).

- The noise/spline/density path uses **only** IEEE-basic `f64` ops (`+ - * /`) and `floor`
  — all reproducible across x86-64, ARM, and wasm32.
- **Forbidden in the worldgen path:** `f64::mul_add`/fma, `std` transcendentals
  (`sin`/`cos`/`exp`/`powf`/`sqrt` where avoidable), and any `rayon`/parallel accumulation
  with nondeterministic reduction order. Perlin + Hermite need none of these.
- Output is integer `BlockId` + chisel data thresholded from density, so once the `> 0`
  comparison is bit-identical (it is, from basic ops running the *same code* on both sides),
  the whole generated `Column` set is identical. Determinism reduces to "same code, same
  ops, no fma."
- Guarded by a cross-run CRC test (extends the existing `generation_is_deterministic`).

Note: `ordered-float` matters at the *stored/hashed state* boundary, which the generated
`Column`s already satisfy (their `Block`/chisel contents are integer/ordered). Transient
`f64` inside the density eval is fine as long as the forbidden ops are avoided.

## 7. Seed threading

`generate_region` is currently `fn(RegionCoords, BlockId) -> Vec<(ColumnCoords, Column)>`
with no world seed; the generator closure at `crates/server/src/lib.rs:247` is
`move |rc| worldgen::generate_region(rc, stone)`, and `RegionGenerator` in
`crates/game/src/world_manager.rs` is the injected `Fn(RegionCoords) -> …`.

Change: introduce a `WorldSeed(u64)` (or plain `u64`) captured into the generator closure so
`generate_region(coords, seed, materials…)` is seeded. Same seed → same world on server and
in the offline client. This is a small, mechanical change touching the closure at
`server/src/lib.rs`, the offline path in `client/src/local_server.rs`, and the `worldgen`
signature. The `RegionSeed` in the region-runner layer is unrelated (it is
fresh/parked state, not a worldgen seed) and stays as-is.

## 8. Testing

- **Unit — `spline.rs`:** Hermite eval against hand-computed control-point cases; monotonic
  and clamped-outside-range behavior; one nested-spline case.
- **Unit — `noise.rs`:** output range bounds; continuity (nearby samples close); seeded
  reproducibility (same seed → same value; different seed → different).
- **Unit — `density.rs`:** node algebra (`Add`/`Mul`/`Clamp`/`RangeChoice`) on constant
  inputs; a small assembled graph evaluates as expected.
- **Determinism:** CRC of a generated region equal across two runs (extends existing tests);
  an assertion/comment documenting the native↔wasm same-`f64`-path guarantee.
- **Shape/sanity:** ocean fraction is the majority of a large sampled area; a
  forced-high-continentalness column has land above sea level; a surface straddle block is
  actually chiseled (mirrors `generated_columns_carry_chisel_slabs`); materials layer
  grass-over-dirt-over-stone top-down.
- **Bench:** extend `main.rs` / `tests/region_load_cost.rs` to time a full region gen
  (coarse find + voxel smoothing) so perf regressions surface. Keep the justfile in sync if a
  new bench invocation is added.

## 9. Explicitly deferred (each a future spec)

Folded into `final_density` as independent fields later, **without** reworking the base:

- **Card-suit "region" archetype system** (spade/heart/club/diamond terrain-shape variety).
- **Caves** (`cave/cheese` + `cave/noodle` 3D density fields).
- **Underground rivers** and **lava tunnels**.

Called out so v1 scope stays honest: v1 is base continents + surface materials only.

---

## Open items to settle during implementation (not blockers)

1. Final world frame numbers (`sea_level`, Y band) and whether the Y band needs to be a
   `worldgen` constant shared with `game`.
2. Whether cache wrappers (§2) are needed for acceptable coarse-find perf, or if plain
   evaluation is fast enough at 256 block-columns/region.
3. Exact material thicknesses and the beach/sand band width.
4. Water `BlockId` — confirm one exists in the registry or add it.
