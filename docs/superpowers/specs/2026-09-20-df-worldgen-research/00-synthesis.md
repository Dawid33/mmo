# Dwarf Fortress worldgen research — synthesis for this engine

2026-09-20. Four subagent reports, this file ties them together:

| | report | what it answers |
|---|---|---|
| 01 | `01-pipeline-and-fields.md` | DF's ordered worldgen steps, its scalar fields, biome thresholds, world→local refinement |
| 02 | `02-rivers-lakes-erosion.md` | DF's river/erosion/lake algorithm as far as it can be reconstructed, artifacts, pseudocode |
| 03 | `03-geology.md` | DF's layer-stack geology, inclusions, caverns; real-geology extensions as pure functions |
| 04 | `04-adapting-to-chunked-generator.md` | how to get the same results in an unbounded, deterministic, chunk-local generator; ranked architectures |

Evidence quality: DF is closed source — no algorithm is public beyond Tarn Adams' one-sentence
descriptions. Parameters, thresholds and data structures (DFHack) are solid; dig amounts, lake growth,
smoothing filters and local refinement are reconstruction. Every report tags claims by source.
Provenance caveat: report 02 read three Bay 12 forum threads through the forum's `wap2` view after the
normal fetch hit an anti-bot page (should not have been done; noted in that report's Sources; the
`[C]`-only claims there are unverified). Reports 01, 03, 04 recorded blocked sources as gaps.
Report 04 cites a few items from memory because the fetch failed (Cordonnier 2016's lake graph, Red
Blob's 2010 "elevation from distance to coast", Veloren's column sampling, the uber-noise talk, Minecraft
river specifics) — see its "Access gaps and retrieval notes"; architecture B's design rests on sources
read directly (TerraForged code, Génevaux 2013, Dendry, Tzathas 2024, the alcatrazEscapee gist, runevision).

## What DF actually does (and why it looks good)

1. **Causes first, biomes derived.** Six independent fractal fields (elevation, rainfall, temperature,
   drainage, volcanism, savagery); biomes are a pure threshold function of them. *Drainage* as a second
   ground axis (swamp vs forest, flat vs hilly) is Tarn's "non-obvious win"; rain shadows the biggest
   single upgrade.
2. **Strict causal order:** final elevation → rainfall (orographic) → temperature → vegetation → biomes.
3. **Erosion = disposable agents.** Hundreds of throw-away "test rivers" walk downhill from mountain
   bases and *dig through* wherever they get stuck (never fill), interleaved with cliff relaxation so
   "everything isn't canyons". Its first purpose is removing fractal artifacts.
4. **Real rivers second**, traced on the carved field: forced outlets, loop-erased, flow accumulated
   downstream → size classes (brook → major river). Lakes are blobs grown along rivers, not filled basins.
5. **Rivers are objects with their own water-surface profile.** At local scale terrain is cut down to the
   river's water elevation (gorges; waterfalls where the profile steps). Every tile EDGE stores where a
   river crosses, how wide, at what elevation — a seam contract between neighbouring tiles.
6. **Geology is a lookup:** per-tile ordered layer stack (soil → sedimentary | extrusive → metamorphic →
   intrusive), hung from the surface; minerals from host→inclusion tables; caverns/magma sea as separate
   feature layers. Assigned AFTER erosion, so rock never shapes terrain.
7. All of it runs on ≤257² tiles of ~1.5 km, which is why reject-and-reroll is affordable.

**Don't copy:** 4-neighbour staircase rivers; rivers ignoring rainfall (rain shadows come after rivers);
carve-when-stuck trench canyons; erosion that only removes (no floodplains/fans); reject-and-retry
validation (useless for a streamed world — our equivalent is the offline seed scan); blob lakes; random
drainage field; **surface-draped strata** (why a DF mountainside is one material).

## What fits this engine

Constraint recap: pure function of (position, seed), bit-exact across machines, unbounded world, regions in
any order, far LOD point-samples the same function. A deterministic cache keyed by a coarse cell is fine
if tiles agree exactly at borders.

Key insight (04 §0): rivers are the one feature with unbounded context. **Determinism ≠ border
agreement**; what gives both is making every derived value a pure function of a BOUNDED neighbourhood of
hash-jittered lattice nodes, with the cache tile computed on a window padded by that dependency radius
(the same argument as `ProfileGrid` corners). Priority-flood, implicit stream-power solves and sequential
droplets are not tileable; capped integer graph passes are.

### Hydrology — recommended: architecture B (04 §6)
Jittered ~512 m lattice drainage graph per ~16 km cache tile, queried per point:
- receivers by capped multi-source integer Dijkstra from ocean nodes (distance-from-coast has no interior
  minima ⇒ every node drains; cost noise makes rivers wander along low ground);
- flow = upstream tree size capped at D hops; **elevation assigned FROM the graph** coast-upward by
  Flint's law `z += k/√A · len` (= stream-power steady state, needs only `sqrt`), so tributaries are
  graded to trunks — DF's waterfall-at-every-confluence and "aqueduct" artifacts cannot occur;
- terrain = lower envelope of `z_seg + valley(d)` replacing the land base term; `mountains *=
  valley_mask(d)` — mountains ≥ 0 so ground never drops below river grade; water gaps through ranges free;
- lakes where old terrain lies below the graded profile (flat level = spill); rivers above sea level as
  staircases of flat reaches with deliberate waterfalls; channel carve per 1 m column AFTER the 4 m
  `ProfileGrid` interpolation;
- LOD: "water rounds DOWN, solids round up", lake levels snapped to a coarse lattice, dilated channel test.
- Est. 30–50 ms per tile cold, +50–100 ns per column (all estimates — benchmark). Risk medium.

Then **A as B's detail layer**: a per-point erosion filter (runevision's Phacelle-noise gullies) on the
relief term, fed B's slope direction so gullies run into a real network. ~+1.5–2.5 µs/sample, needs
sin/cos via libm or polynomials. **Defer C** (Veloren/Cordonnier-style stream-power simulation per closed
continent): best at map scale, empty at walking scale, cold stalls on both machines, closed continents
change the world, whole-world cache invalidation on any tuning change.

What B takes from DF: rivers as first-class objects with monotone water profiles; terrain carved to meet
them; per-edge agreement (here by construction, not stored records); flow → size classes; sculpt-then-
trace becomes "graph first, elevation from graph". What it fixes: staircase paths, ungraded confluences,
trenches, blob lakes. What it only imitates: erosion of the mountains themselves (valley mask, later A / C′).

### Geology — recommended minimal cut (03 §6)
1. **Stratigraphic coordinate** `s = y − B(x,z)` with `B = α·H_smooth + tilt + folds` (fold amplitude
   gated by the mountain field), looked up in a per-province rock column. The existing height field then
   cuts the layers: banded cliffs and valley sides for free; α<1 mesas, α>1 old rock in range cores.
2. **Bed banding**: 1D hash, 2–6 m beds inside 16–48 m formations.
3. **Slope/altitude-aware regolith** so bare rock shows (today's unconditional grass/dirt skin hides it).
   Start with three hand-written columns. From DF keep: ordering discipline, host→mineral tables with
   nesting, aquifer as a material property, soil thinning with elevation, deep cavern bands + magma floor.
   Second tier: provinces from continentalness/mountains + a volcanism noise, faults as step offsets
   (never bilerped), plutons/dykes, ores in (x,z,s), karst bias for caves, hardness nudging density.
   Open renderer question: LOD `merge_group` keeps the material visible from above → flattens cliff
   banding in far rings.

### Climate (from 01, cheap, later)
Rainfall with a rain-shadow pass off the mountain field, temperature from latitude + altitude, and a
drainage-like field — ideally DERIVED from B's graph (flat + low + near river = poorly drained) rather
than DF's random fractal. Drives ground cover and, with B's optional rainfall weighting, river size.

## Order of work (proposal)
1. **B0** standalone prototype (like the continents one; no world output change): graph + graded
   elevations drawn over `just world-map` relief. Judge: dendritic, no grid showing, trunks km apart.
2. **B1** terrain from the graph (envelope + valley mask); validate with tiling-invariance `assert_eq`,
   monotone grade, and priority-flood + D8 over the rendered heightmap (depressions outside lakes ≈ 0,
   D8 network overlaps the river mask), hypsometry vs today's 32.8% land / 11 / 47 / 143 / 264 m.
3. Geology minimal cut (independent of B; needs new stone blocks/textures) — can run in parallel.
4. **B2** channels, water levels, lakes, LOD water rule; live look at walking scale.
5. Erosion-filter detail (A), then climate/ground cover.

Biggest risks: lattice regularity showing as evenly spaced rivers; tent-like terrain between rivers if
the valley cross-section is naive (Red Blob himself failed to fit elevation to grown basins — the graph
is easy, the fit is the design problem); land elevations change everywhere (new seed scan, pinned tests);
the `DensityFn` oracle cannot express a graph lookup; elevated water surfaces vs the LOD rings'
any-solid rule and sea-lattice assumption.

---

# Addendum (2026-09-20, later) — constraints changed: finite, pre-generated super regions

User direction: the world is divided into **super regions, one Kubernetes pod each, with a loading screen
between them**; each super region is generated **as its own finite world** with a seam at its edge, with
an **unlimited offline CPU budget**; gameplay will be designed around that. (This promotes the *fallback*
of `2026-09-20-multi-instance-k8s-postgres-research.md` §1a decision 2 — static super-regions with a hard
border — to the chosen direction, and goes further: terrain is no longer a function of position.)

## What this removes
- "Pure function of (position, seed), evaluable at any point in any order" — gone. Terrain becomes a
  **stored artifact per world**, generated once, versioned, frozen. This matches §1a decision 3
  (freeze on first run) and simplifies it: every region's pristine state is the artifact, there is no
  worldgen-version seam at an exploration frontier.
- Cross-machine bit-exactness **of the generator** — only one machine ever generates a world; everyone
  else reads the artifact. (Rollback/sim determinism is untouched.) Transcendentals, parallel reductions,
  iterative solvers are all allowed; the requirement shrinks to "artifact is canonical + reproducible
  enough to debug".
- Border agreement, bounded dependency radius, capped accumulation, per-point cost, cold-start stalls,
  "cache invalidation of whole worlds" — the entire reason report 04 ranked the simulation approach (C)
  last. Report 04's architecture B (jittered-lattice graph, capped Dijkstra, padded cache tiles) was an
  answer to a problem that no longer exists.

## What it enables — the DF/Veloren shape, done properly
A finite closed map is exactly DF's and Veloren's setting, and offline budget lets us do what DF cannot:
1. **Initial condition** from today's pipeline (continents → land mask, ridge/mountain field → *uplift
   map*), shaped to the world's edge type (below). Not wasted work — it becomes the sim's input.
2. **Geology BEFORE erosion** (Tarn: "should really be earlier"): province map, strata with tilt/folds/
   faults, plutons → a rock **hardness/erodibility field**. Erosion then produces cuestas, mesas,
   resistant ridges, water gaps — structure that reads as geology from a distance, and the same
   stratigraphic model paints the cliffs (report 03 §6).
3. **Fluvial landscape evolution on the whole map**: uplift + stream-power incision (implicit O(n)
   solver à la Braun–Willett/Cordonnier, as Veloren ships) + hillslope/thermal diffusion + sediment
   deposition (floodplains, fans, deltas — what DF's remove-only erosion lacks), multi-resolution
   (e.g. 32 m → 8 m → 2 m), optionally finished with droplet hydraulic erosion at fine scale.
4. **Exact hydrology**: priority-flood depression filling → lakes at true spill levels with outlets;
   exact D8/D∞ flow accumulation → river hierarchy, width ∝ √Q; **rainfall-weighted** discharge from
   the climate step, so deserts get wadis and wet ranges get big rivers.
5. **Climate on the final heightfield**: prevailing-wind moisture advection → rain shadows, temperature
   from latitude/altitude; iterate 3↔5 a few times so wet flanks erode more (DF's rivers ignore rain).
   Drainage/wetness DERIVED from slope + flow (swamps on flat low ground), not a random fractal.
6. **Generate-and-test** like DF: reject/re-roll worlds on contiguity-aware criteria (a navigable river to
   the sea, N lakes, a range above the snow line, enough flat buildable land, good spawn sites).
7. Later, on the same artifact: biomes/ground cover, caves/karst biased by limestone provinces,
   settlement/road/history layers — all the things DF hangs off its world map.

## What carries over unchanged
- Walking-scale realisation is still the hard, engine-specific part: **rivers as objects with a monotone
  water-surface profile**, channel/bank carve per 1 m column (the sim grid is too coarse for a 5 m
  channel), valley cross-sections, waterfalls where the profile steps, per-point detail/gully noise on
  top — i.e. B's *query side* and A, fed by the simulated map instead of a lattice graph.
- Voxel water above sea level and the LOD rule ("water rounds down, solids round up", lake levels on a
  coarse lattice, dilated channel test) — still needed; the any-solid LOD rule and the sea-lattice
  assumption are engine facts.
- Slope/altitude regolith, strat-coordinate banding, `just world-map` as the viewer (pointed at the
  artifact, plus river/lake/province overlays).

## New architecture sketch
`worldforge` (offline tool, new crate or `worldgen` bin): seed + world params → **world artifact** =
tiled rasters: height (u16, ~1–2 m), water surface + river graph (polylines with width/level),
province/strat parameters, erodibility, climate fields, material hints; stored as a file/object per
world (memory-mapped by the pod) and/or Postgres rows per tile. `generate_region` becomes *artifact
sample + deterministic local refinement* (voxel surface smoothing/chisel, detail noise, channel carve,
strata lookup); `coarse_tile` samples the same artifact (cheaper than today). Rough sizes (estimates):
16 km × 16 km at 1 m = 268 M samples ≈ 512 MiB raw u16, far less compressed; at 2 m, 128 MiB. A
whole-map stream-power run at 8 m (4 M cells) is seconds-to-minutes; 2 m droplet passes are
minutes-to-hours — irrelevant under the stated budget.

## Consequences to flag
- **Super-region size is now THE worldgen parameter.** The k8s doc's S = 8 regions is 512 m (8 × 64 m;
  its "≈ 8 km" is a slip — 1 km even at `region_chunks=64`). A world with rivers from mountains to sea
  wants ≥ 8–16 km. Pod load is set by *active* regions/players, not world area, so area mostly costs
  storage and generation time.
- **Edge type** decides the sim's boundary condition: ocean all round (sea level = natural Dirichlet
  boundary; sail/fly off the edge → loading screen) is simplest and best for hydrology; alternatives:
  impassable range with passes/tunnels/portals, or an edge-matched strip shared by neighbours (so the
  far side can be shown as LOD) — more work, only worth it if gameplay wants visible neighbours.
- **Offline / wasm `?offline` mode** (`local_server.rs` runs worldgen in the browser) needs either a
  downloadable small artifact or to keep today's procedural generator as a "quick world" fallback.
- LOD rings clip at the world edge; spawn selection becomes a world-level site search, not a seed scan.
- Tuning loop gets slower (simulate → render); keep a low-res preview mode in the forge.

## Revised recommendation
Drop architecture B's graph machinery. Prototype the **offline forge** standalone (no engine changes):
F0 = land/uplift from today's pipeline on a bounded map (ocean edge) → stream-power + diffusion +
priority-flood + flow accumulation at ~16–32 m → render with `world-map`-style relief + river/lake
overlay; judge drainage realism (Horton/Hack statistics, hypsometry) and look. F1 = erodibility from a
first geology model + climate coupling + deposition. F2 = artifact format + `generate_region` sampling
it + channel carve + elevated water/LOD rule, live at walking scale.
