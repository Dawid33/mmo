# World forge — prototype F0 findings (2026-09-20)

Context: user direction — the world is split into super regions (one k8s pod each, loading screen between),
each **pre-generated offline as its own finite world**; chosen parameters: **16 km, ocean all round**. See
`../2026-09-20-df-worldgen-research/00-synthesis.md` (addendum) for why this makes whole-map simulation
the right approach. F0 asks one question: *does simulated landscape evolution give the rivers, valleys
and lakes we want, at a cost that does not matter?* Answer: yes.

Code: originally the single file `crates/worldgen/examples/forge_proto.rs`, since grown into the F1
multi-module example `crates/worldgen/examples/forge_proto/` — `just forge-proto --no-geology --no-climate`
reproduces F0's behaviour (plus F1's valley floors and height fit). (Standalone; `#[path]`-includes the real `noise.rs`; no
engine code or world output touched). Run: `just forge-proto [--cell 32 --iters 200 …]` → PNGs + stats in
`target/world-forge/`. Small renders next to this file: `initial.png` (noise only), `rivers-16m.png`
(after evolution), `zoom-4km-8m.png`.

## What it does
1. **Initial condition**: noise-warped island (shape noise fades toward the rim, a superellipse margin
   forces sea at the world edge — asserted), low initial relief, an **uplift map** with linear mountain
   belts along the zero contour of a 4 km noise, inland only. Noise says where rock rises, not what the
   surface looks like.
2. **Per iteration** (500 × 2,500 yr): Priority-Flood+ε from the coast (Barnes 2014) gives, in ONE pass,
   a depression-free routing surface, receivers, and a downstream-first order (cells pop in nondecreasing
   flooded height; receivers are then upgraded to steepest descent, still strictly lower) → flow
   accumulation → **implicit stream-power incision** `dh/dt = U − K·√A·S` (Braun & Willett 2013, n = 1,
   unconditionally stable) → sediment slowly fills depressions → explicit hillslope diffusion.
3. **Lakes**: a fluvial steady state has no closed depressions (459 noise pits → 0), so basins are
   excavated afterwards on valley floors (hashed sites, 1.5–25 km² drained, ≥ 1.8 km apart, 5–14 m deep
   bowls — what glaciers/subsidence do) and Priority-Flood floods each to its **true spill level**: flat
   surface, natural outlet, river continues. Plus free sea-level lagoons where the coast encloses sea.
4. **Analysis**: hypsometry, slopes, lake count/area, river mouths/length, Flint slope–area concavity,
   Hack's law exponent.

## Results (seed 0x00D5C0DE0206, 16 km world)
| cells | grid | wall time | peak RSS | max / p99 / p90 / p50 height (m) | slope p50 / p90 / p99 / max | θ | Hack |
|---|---|---|---|---|---|---|---|
| 32 m | 500² | 4.8 s | — | 188 / 136 / 77 / 11 | 0.043 / 0.276 / 0.391 / 0.602 | 0.66 | 0.55 |
| 16 m | 1000² | 23 s | ~0.2 GB | 228 / 165 / 94 / 14 | 0.046 / 0.288 / 0.400 / 0.597 | 0.64 | 0.54 |
| 8 m | 2000² | 126 s | ~0.4 GB | 251 / 191 / 109 / 17 | 0.048 / 0.294 / 0.407 / 0.611 | 0.62 | 0.52 |

Single-threaded, this laptop. Land 33% (84.5 km²), ~31 river mouths, ~64 km of channel ≥ 0.4 km² drained,
longest river ~8 km, biggest basin ~15 km², 5–8 lakes (largest ~0.12–0.16 km²). Initial noise terrain for
comparison: θ = 0.05 (no fluvial signature), 337–459 pits. Nature: θ ≈ 0.4–0.6, Hack ≈ 0.5–0.6.

- **Drainage structure is resolution-stable** (mouths, longest river, biggest basin agree across 32/16/8 m)
  and **slopes are resolution-independent** once a **critical hillslope area** (`HILLSLOPE_AREA_M2` =
  6,000 m²) caps the 1/√A steepening at channel heads. Without it 8 m cells built peaks 45% taller than
  16 m cells (337 vs 232 m). Peak height still creeps up with resolution (188 → 228 → 251 m) as narrower
  valleys resolve; converging.
- Relief scales with `U/K`; `--uplift 6.5e-4 --k 2.5e-5` lands ranges at ~230 m (engine band: +256 m).
  Steady state is reached by ~iteration 450.
- Max slope ≈ 0.6 (31°): everything is walkable — the opposite of the noise pipeline's 78° faces. Cliffs
  will have to come from geology (hard bands), not from the erosion law.

## What is visibly wrong / missing (→ next stages)
- **D8 routing**: right-angle/45° channel segments at cell scale (zoom). Fix at realisation: river
  polylines smoothed + meandered, or route on a jittered/triangulated mesh (Cordonnier).
- **Planar "tent" hillslopes** at walking scale between channels — expected; needs per-point detail
  (gully/erosion-filter noise, regolith, rock outcrops) in the region refinement step.
- **Erosion only removes**: no sediment transport → no floodplains, fans, deltas; valley floors are V
  not U/flat. Add transport-limited deposition.
- Uniform rock (erodibility = noise ±): no cuestas/mesas/water gaps. **Geology before erosion** (F1).
- Uniform rain: no rain shadow, no dry side. Climate coupling (F1).
- Lakes are drawn with the river line through them; lake/river objects (polylines with water-surface
  levels, widths from √A) are not extracted yet — needed for the artifact and for voxel water.
- Coast is static (no wave erosion, no sea-level lagoon outlets); one island per world; no seed scan /
  generate-and-test yet.

## Proposed stages
- **F1 (still standalone)**: geology model → erodibility (+ strata for cliffs); orographic rain → rainfall-
  weighted discharge, iterate; sediment deposition; extract river/lake objects; DF-style generate-and-test
  (navigable river to sea, N lakes, range above snow line, flat buildable land, spawn sites).
- **F2 (engine)**: world artifact format (height at 1–2 m by multi-resolution refinement, water surfaces,
  river graph, provinces, climate) + `generate_region`/`coarse_tile` sampling it + 1 m channel carve +
  elevated voxel water and the LOD "water rounds down" rule + world-edge handling (LOD rings clip at the
  rim, loading-screen trigger at sea).
