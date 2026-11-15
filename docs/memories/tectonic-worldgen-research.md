---
name: tectonic-worldgen-research
description: Tectonic worldgen research → IMPLEMENTED & merged (develop 8ba8790, 2026-07-07); see spec/plan, don't re-plan
metadata: 
  node_type: memory
  type: project
  originSessionId: 48c681a9-2906-4782-87cb-d79c392cf150
---

**DONE (2026-07-07, develop `8ba8790`).** The research was implemented as the density-pipeline
worldgen and merged. We reimplemented the *mechanism* with **our own tuned splines** (NOT a
byte-exact Tectonic port — the ~590 control points were never transcribed). Modules in
`crates/worldgen/src`: `noise` (deterministic Perlin+octaves), `spline` (nested cubic-Hermite),
`density` (`DensityFn` graph), `pipeline` (`TerrainPipeline`, tuned continentalness/erosion/ridges
→ offset/factor/jaggedness → `final_density`; `WORLD_SEED` lives here), `material`, and
`generate_region` (coarse per-block surface-find + per-voxel straddle smoothing → Column/chisel,
with a bit-exact 2D column-profile cache, ~51 ms/region). Design + plan:
`docs/superpowers/specs/2026-07-07-worldgen-density-pipeline-design.md` and
`docs/superpowers/plans/2026-07-07-worldgen-density-pipeline.md`.

**Deferred (not built):** card-suit "region" archetype system, caves, underground rivers,
lava tunnels — each folds into `final_density` later; the `DensityFn` `Min`/`Max`/`RangeChoice`
nodes are already there (allow-dead) for them. **Blocker for actually playing on it:**
[[collider-perf-followup]].

Original research doc (verified Tectonic constants/octaves/pipeline structure):
`docs/superpowers/specs/2026-07-06-tectonic-worldgen-research.md`. Key framing that guided it:

The doc was updated 2026-07-06 by **reading the actual repo** — it now carries the
verified scalar constants, noise octaves, sampling scales, and full pipeline structure
(not just mechanisms). Key takeaways so future work starts from the right frame:
- Reimplement the **datapack overlay variant** — the mechanism is Minecraft's 1.18+
  density-function + continentalness/erosion/PV spline pipeline, retuned to continent scale.
- Correction to earlier notes: current Tectonic is NOT zero-Java. It ships a mod with 4
  custom DF types (`config_noise/constant/clamp`, `invert`) that inject a runtime config;
  the `overlay.datapack` bakes those into vanilla primitives + constants → port that one.
- Build a composable `DensityFn` graph + a nested `Spline` type; solid where `final_density > 0`.
- Determinism: route ALL noise/spline math through the vendored `libm`/`ordered-float`
  discipline — never `std` transcendentals — or cross-machine drift breaks rollback.
- Biome selection is orthogonal; Tectonic does NOT rely on TerraBlender (refuted).
- Constants are captured in the doc; the ~590 spline control points across 38 files are NOT
  transcribed — re-clone the repo and read `terrain_spline/**` + `region/**` when implementing.

Related: [[game-direction-entities]].
