# Voxel water: algorithms and data structures

**Date:** 2026-09-22 · **Status:** research only, nothing implemented.
**Question:** which water model and storage layout is cheaper than Minecraft's, and cheaper
than the per-cell plan in `docs/2026-07-15-water-implementation-plan.md`? It also has to be
bit-exact on x86_64 and wasm32, undoable, and re-simulated up to ~20 ticks on a misprediction.

**Short answer:** most of the cost is in *settled and slowly changing large bodies*, not in
flowing water. A per-cell cellular automaton (Minecraft's, or ours) pays per cell for
something that is really one number, the lake's level. Once disturbed, a finite-volume CA
also takes O(L²) steps to re-level a lake of width L. The recommended model is a hybrid:

- **Water bodies** are objects. Each has one surface level, a volume, and run-length
  column membership. A connected pool, lake, sea or flooded cave is one body.
- **A small cellular automaton** handles only the *flowing* water: streams, waterfalls,
  spreading sheets, and the outlets and inlets where the CA meets a body.
- **Settled** water costs 0. A **drain** costs O(openings) plus the stream it feeds.
  Changing a level costs O(1), plus O(shoreline) once per metre crossed.
- **The sea and rivers** are *infinite* bodies with a fixed surface. This is Minecraft's
  source-block trick, made principled.

§5 has the details, §6 the costs, and §8 what to prototype first.

The micro-benchmarks behind the ns figures are in `/home/dawids/.claude/jobs/ce041ce6/tmp/wb/`
(`b.rs`, `c.rs`): `rustc -O`, single thread, pinned `taskset -c 0-3`. They ran on this laptop,
whose clock was visibly throttling (400 MHz to 3.9 GHz across cores; see the power-clamp note
in CLAUDE.md). **Read them as ratios and orders of magnitude.** Expect wasm32 to be about
1.5–2× slower.

---

## 1. Numbers that frame everything

Scale: 1 cell = 1 m³ = 4096 units. A region is 64 × 64 m in plan with a few hundred metres
of height. There are 20 ticks/s, and water steps at 10 Hz in the plan.

| Quantity | Value |
|---|---|
| Cells in a 64 m cube | 262,144 |
| Sea-covered region, 64 m deep | ~262 k wet cells, all settled |
| Typical 40 × 40 m lake, 8 m deep | 1,600 surface cells, 12,800 m³ |
| Re-sim multiplier | ≤ 20 ticks ⇒ ≤ 10 water steps per rollback at 10 Hz |
| **Dense `u16` edge sweep** (bench `b.rs`, dam break, in-place, flat array) | **~9 ns per wet cell per step** (0.59 ms per step for 65 k wet cells; empty cells ~1 ns to skip) |
| **Same sweep, but cells in a `BTreeMap<ChunkKey, [u16; 8]>`**, which is the plan's 8-cell `WaterChunk` map (bench `c.rs`) | **~460 ns per visited cell** (**50× slower**: every neighbour read is a map lookup) |
| Stack flood fill over a dense `u16` grid | ~5 ns per cell (164 k cells in 0.8 ms) |
| Bit-sliced full/empty CA (64 cells per `u64`), fall + spread | ~0.02 ns per cell (5 µs for 262 k cells) |

Two lessons follow before any algorithm is chosen:

1. **Storage layout outweighs the choice of rule.** An 8-cell chunk keyed in an ordered map
   is fine as the *undo* granularity. As the *iteration* structure it is fatal. Keep hot
   water data in dense bricks (§4).
2. **Settled cost is already solved by sleeping**, both in the plan and in Minecraft. What is
   left to win is **(a) the cost and duration of disturbances to big bodies, (b) state and
   snapshot size, and (c) the rollback multiplier.**

## 2. Cell-level cellular automata

### 2.1 Minecraft levels (non-conserving)

A **source** block is static and never ticks. **Flowing** water carries a level 1–7. A flow
update runs every 5 game ticks (4 m/s spread). Each flowing block goes down first. Otherwise
it spreads sideways, weighting each direction by the shortest path (≤ 4 blocks) to a drop,
and loses one level per block. Two sources next to each other create a new source. Water is
not conserved: sources are infinite and flowing water vanishes when its source is removed.
([Minecraft Wiki: Water](https://minecraft.wiki/w/Water),
[Fluid](https://minecraft.wiki/w/Fluid)).

- **Cost:** 0 when settled. Flowing water is bounded by 7 × 7 fans per source. Each flowing
  update does a small BFS (≤ 4 steps, up to ~4 directions × a few dozen lookups) and
  triggers block-update cascades. Most of the lag people report comes from the cascades and
  re-lighting, not from the rule itself.
- **Believability:** readable and gamey. **You cannot drain a lake.** Infinite water and
  7-block spreading are the price of the cheap bound.
- **For us:** deterministic and trivially small (4 bits per cell). It is the right *bounding
  idea* (a fixed-level infinite reservoir costs nothing), not the right *volume* model.

### 2.2 Finite-volume transfer (the plan's family)

The references here are Forsyth, W-Shadow and jgallant. Each cell holds an amount. Each step
moves mass down first, then sideways toward equal fill, capped by a max-flow per step.
([Forsyth, *Cellular Automata for Physical Modelling*, GPG3 2001](https://tomforsyth1000.github.io/papers/cellular_automata_for_physical_modelling.html);
[W-Shadow, *Simple Fluid Simulation with CA*](https://w-shadow.com/blog/2009/09/01/simple-fluid-simulation/);
[jgallant, *2D Liquid Simulator with CA*](https://www.jgallant.com/2d-liquid-simulator-with-cellular-automaton-in-unity/),
[source](https://github.com/jongallant/LiquidSimulator)).

- **Pressure:** Forsyth and W-Shadow let a cell hold slightly more than full (~1–2 % per
  cell of depth) so pressure can travel through full cells. That lets water climb U-tubes.
  With integers this is fine (e.g. `MAX_COMPRESS` = 64 units per cell of depth), but it
  makes "full" ambiguous for rendering and collision.
- **In-place vs snapshot:** Forsyth avoids double buffering with a per-cell turn stamp. An
  in-place ordered sweep (the plan's baseline) is deterministic because the order is part
  of the protocol. It is biased, though, and a falling packet travels 1 cell per step or
  the whole column in one step depending on the sweep's y direction. A snapshot plus flux
  buffer (Jacobi) is order-free and symmetric, and SIMD-friendly, at 2× the memory
  traffic.
- **The structural problem is levelling time.** Horizontal exchange by a fraction `f` per
  step is explicit diffusion (D ≈ f cells²/step). A disturbance in a body of width L decays
  with time constant ≈ L² / (π² D):

  | Body width L | f = 0.2 (plan-like ⅕ transfer) | f = 0.5 |
  |---|---|---|
  | 16 m | 130 steps = 13 s | 52 steps = 5 s |
  | 40 m | 810 steps = 81 s | 320 steps = 32 s |
  | 64 m | 2,075 steps = 3.5 min | 830 steps = 83 s |
  | 1 km (seam-crossing lake) | ~5.8 days | ~2.3 days |

  Getting from "a 25-unit imbalance" to under the sleep threshold takes another ~2–3 time
  constants. So one block of water removed from a 40 m lake wakes its *whole surface*
  (1,600 cells) for about a minute. A drain leaves a visible funnel sloping toward the
  outlet. The sleep threshold also leaves permanent sub-threshold stair-steps. Each step is
  cheap. The trouble is that there are so many steps, over so many cells.

### 2.3 Checkerboard and Margolus (block) CA

- **Checkerboard (red-black):** update every other cell, then the rest. It is in-place and
  still symmetric, since no cell sees a half-updated neighbour of the same colour. It is a
  cheap fix for directional bias with no flux buffer.
- **Margolus / block CA:** partition the grid into 2×2(×2) blocks, and shift the partition
  by (1,1,1) on alternate steps. The rule redistributes mass *inside a block*, so
  conservation holds by construction. Blocks are independent, so the update is
  embarrassingly parallel and deterministic. The undo unit is the block. This is the
  standard falling-sand-on-GPU trick.
  ([Block cellular automaton](https://en.wikipedia.org/wiki/Block_cellular_automaton);
  [Margolus rules lexicon](http://psoup.math.wisc.edu/mcell/rullex_marg.html);
  [GPU falling sand with block CA](https://github.com/GelamiSalami/GPU-Falling-Sand-CA)).
  The downside is speed: mass moves ≤ 1 cell per half-step, and the levelling problem is
  the same as §2.2. It is a good choice for a *frontier* solver, not a fix for big bodies.

### 2.4 Bit-sliced / bitboard CA

Pack 64 cells of a row into a `u64`, e.g. `water` and `solid` bit-planes. Fall is
`fall = water[y] & !(water[y-1] | solid[y-1])`, and sideways moves are shifts. The bench
measured ~0.02 ns per cell, **~450× cheaper than the `u16` CA**. The catch is that it only
knows *full* and *empty*. Partial cells need 12 bit-planes (bit-sliced arithmetic,
~12–30 ops per 64 cells), and "equalize toward the neighbour" is awkward in bit-sliced
form.

**Where bitboards pay off for us:** bit-planes are the right tool for the **masks and
flood fills** in §5. These are body membership, "which cells are air below level L and
connected", active/sleep masks, and "any active" hierarchies. A 64 × 64 × 384 region is
24 k `u64` words (192 KiB per plane). A full word-parallel dilation pass over it takes
~10–20 µs.

### 2.5 Noita-style sparsity

Noita (the "Falling Everything" engine) splits the world into 64 × 64 chunks. Each chunk
keeps a **dirty rect**, and only the rect is iterated. A chunk that stops changing sleeps
until a neighbour touches it. Chunks are updated in a 4-phase checkerboard so threads never
touch adjacent chunks.
([80.lv on Noita](https://80.lv/articles/noita-a-game-based-on-falling-sand-simulation);
[GDC talk notes](https://braindump.jethro.dev/posts/gdc_vault_exploring_the_tech_and_design_of_noita/)).
Terraria adds a global panic valve. If the liquid update count exceeds a threshold, it
switches to `QuickWater()`, which *settles liquids instantly*.
([Liquid.cs](https://github.com/TheVamp/Terraria-Source-Code/blob/master/Terraria/Liquid.cs)).
Terraria's panic mode is the per-cell CA admitting that big bodies should be settled
analytically. That is what §5 does all the time.

## 3. Heightfield and column models

### 3.1 Virtual pipes and shallow water

O'Brien & Hodgins (1995) connect each column to its 4 neighbours with "pipes". Each pipe
carries a flux accelerated by the height difference: `f += Δt·g·A·Δh / l`. Outflow is scaled
so a column never goes negative, and the height is updated from the net flux. Mei, Decaudin
& Hu (2007) made this the standard GPU erosion solver. Št'ava et al. (2008) and Beneš
(2007) used it interactively.
([Mei et al. 2007 PDF](http://www-evasion.imag.fr/Publications/2007/MDH07/FastErosion_PG07.pdf);
[Beneš 2007, Real-time erosion using shallow water](https://www.cs.purdue.edu/cgvlab/www/resources/papers/Benes-2007-Real-Time_Erosion_Using_Shallow_Water_Simulation.pdf)).
Games in this family include Timberborn and From Dust. Timberborn simulates only the water
*sheet over terrain*: each tile has a depth and 4 pipes, and currents come from flux history.
([Timberborn deep dive](https://www.gamedeveloper.com/design/deep-dive-timberborn-s-water-mechanics),
[wiki](https://timberborn.fandom.com/wiki/Water)).

- **Strength:** flux persists between steps, so the model has **momentum**. Disturbances
  travel as waves at √(gh), so a lake re-levels in **O(L)** steps instead of O(L²), and you
  get sloshing and currents for free. State per column is `h` plus 4 fluxes (or 2 shared
  edges): 12–20 B per column, ~80 KiB for a 64 × 64 region.
- **Limits for us:**
  1. **One layer.** It has no caves, overhangs or cell-level waterfalls. A waterfall is
     just a steep sheet.
  2. **CFL:** stability needs Δt ≤ Δx/√(gh). At 10 Hz with 1 m cells that means
     h ≲ 10 m. A 64 m-deep sea wants 2–3 substeps, or damping.
  3. It rings and never truly settles without a flux threshold and damping, which is the
     sleep problem again.
- **Determinism:** fine in fixed point, e.g. `h` in 1/4096 m as `i32` and flux in
  units/step as `i32` with a `>> k` gravity factor. The outflow-scaling division is an
  integer division, so it is deterministic. Avoid `sqrt`, or use an integer table.

### 3.2 Layered / stacked heightfields (spans)

Store each column as a list of vertical **spans**, alternating solid and air. The water in
an air span has its own level. Flow runs between vertically overlapping spans of
neighbouring columns. A waterfall is a transfer from a high span into a neighbour's lower
span, with the fall time ignored or approximated. McDonald's SoilMachine layer map is a
pooled RLE section list per column
([nickmcd.me](https://nickmcd.me/2022/04/15/soilmachine/)). Nilles et al. (VMV 2024) run
pipe erosion on multi-layered heightmaps, with overhangs and caves, at 6 ms per step for
2048² on an RTX 3070
([paper](https://diglib.eg.org/bitstream/handle/10.2312/vmv20241211/vmv20241211.pdf)).
Dagenais et al. (2018) add **extended pipes** because plain multi-layer pipes *cannot resolve
flow through fully flooded passages*, i.e. U-bends and siphons
([paper](http://profs.etsmtl.ca/epaquette/research/Papers/Dagenais.2018.cg/)).

- **Cost:** per wet *span*, not per wet cell. An 8 m-deep lake is 8× fewer units and the sea
  64× fewer. Settled spans sleep.
- **Complexity:** high. Span splits and merges on every edit, span-to-span adjacency in 3D,
  and the flooded-passage special case.
- **Takeaway:** the right *storage* for membership (§5.3 uses column spans). As a *solver*,
  it costs more than it saves when bodies are handled analytically.

## 4. Sparse storage

| Structure | Per 64 m region | Use |
|---|---|---|
| Dense `u16` per cell, whole region (64² × 384) | 3 MiB | too big to keep for every region |
| Plan: `BTreeMap<ChunkCoords, WaterChunk>` with 8 cells + mask + settle age | sea: ~32 k entries × (8·3 B + key + node overhead) ≈ 1.5–2 MiB | fine as the undo unit, **50× too slow as the iteration structure** (§1) |
| **Bricks of 8³ cells** (= 4³ terrain chunks), `u16` amounts, allocated only where CA water is | 1 KiB per brick, plus a 512-bit active mask and a dirty AABB (6 B) per brick | the frontier CA. A neighbour read is an index inside the brick, plus one cached brick pointer at brick faces |
| Region-level "brick active" bitmask (8 × 8 × 48 bricks = 3 × `u64` rows per y-slab) | ~400 B | skip all sleeping space in O(active bricks) (Noita's dirty rect, one level up) |
| **Column membership spans** `(y_bottom: u16, body: u16)` | 4,096 columns × 1–3 spans × 4 B ≈ **16–48 KiB** | which body owns which wet cells. The span top is implicit: min(body level, ceiling) |
| Bodies: `{level: u32 (units), volume: u64, area_by_layer: Vec<u32>, flags, outlets: Vec<CellPos>}` | ~64 B + 4 B per layer spanned + outlets | the whole state of every settled water body |

Compare snapshots of a sea-covered region, before lz4:

- the plan: ~1.5 MiB;
- the body model: ~20 KiB of spans plus a 64-byte body;
- the sea as an *implicit* infinite body (every air cell below `SEA_LEVEL` that worldgen
  marked sea-connected): ~0.

## 5. Aggregate "body" models, the big lever

### 5.1 Prior art

- **Priority-Flood** (Barnes, Lehman & Mulla 2014). This is depression filling on a DEM: flood
  inward from the edges in priority-queue order of elevation. It is O(n) for integer
  elevations. ([arXiv 1511.04463](https://arxiv.org/abs/1511.04463)). The forge
  (`examples/forge_proto`) already uses it for spill-level lakes.
- **Fill–Spill–Merge** (Barnes, Callaghan & Wickert, ESurf 2021). Builds a **depression
  hierarchy**, a binary tree of basins joined at sills. Runoff is routed by *filling* a
  basin's volume, *spilling* the excess over its sill into the neighbour, and *merging* the
  two into the parent once both are full to the sill. There is no per-cell iteration:
  water moves as volumes between tree nodes.
  ([paper](https://esurf.copernicus.org/articles/9/105/2021/),
  [code](https://github.com/r-barnes/Barnes2020-FillSpillMerge)). This is the conceptual
  core of what we want, lifted from a 2D DEM to 3D voxels.
- **Dwarf Fortress pressure.** Water falling onto full 7/7 water "teleports" along a path of
  full cells (down and orthogonal only) to the nearest open cell, which makes communicating
  vessels work.
  ([DF wiki: Pressure](https://dwarffortresswiki.org/index.php/DF2014:Pressure)). A body
  model gets the same physics without tracing a path: the connected body *is* one level.
- **Minecraft infinite sources** = a body with fixed level and infinite volume.
- **Hierarchical connected components** (Sean Barrett). Two levels: local components per
  subgrid, then union-find over the components. On 1024² it re-labels after a block edit in
  0.25 ms typical and 2.5 ms worst case, versus 50 ms for a full recompute
  ([stb blog](https://stb.handmade.network/blog/p/1136-connected_components_algorithm)).
  This is the template for incremental membership maintenance.

### 5.2 3D basins = the merge tree of `y` over air cells

In 3D the depression hierarchy is the **join tree of the height function y on the air-cell
graph**. Sweep layers bottom-up, add each layer's air cells, and union each one with its
already-added face neighbours. A component that appears is a basin floor. Two components
uniting at layer y means a sill at y. A body at level L in basin B owns exactly *the air
cells of B's component with y ≤ L*. That includes connected caves below the surface, so
U-tubes and siphons come out right. Trapped ceiling air is ignored, as DF and Minecraft do.

With a y-major layout the sweep is already sorted. Cost: ~5–10 ns per air cell with
union-find, so ~10 ms for a whole 64² × 300 region. You never need the whole tree at run
time. §5.3 computes the pieces lazily and bounded.

### 5.3 The recommended body mechanics (integer-only)

A **body** has these fields:

- `level` in units, i.e. `y*4096 + fill`, as an integer;
- `volume` in units, `u64`;
- `infinite: bool`;
- `area[y]`, the count of member cells per layer;
- membership as column spans (§4).

The volume–level relation is exact:
`volume = 4096·Σ_{y<⌊L⌋} area[y] + area[⌊L⌋]·(L mod 4096)`.

- **Level from volume** is O(1) inside a layer: add `Δvolume / area[top]`. The remainder is
  handled per below, and the level only changes layers on a real crossing.
  Remainder rule: keep the fractional remainder *in the body* as `volume` minus the level
  implied by the cells, i.e. a sub-unit reservoir. Nothing is lost and nothing is
  distributed per cell. The plan's "one unit of tolerance" disappears because there are no
  per-cell amounts to go uneven.
- **Settled:** 0 work. No cell is ever visited.
- **Inflow:** a CA cell that is next to a member, at or below the level, gives its amount
  to `volume` and becomes empty. A waterfall landing in a lake costs O(1) per
  landing cell.
- **Outlet:** a non-member air cell next to a member at y ≤ ⌊L⌋. Outlets appear only through
  events: a terrain edit, or the level rising into a new layer, found by scanning that
  layer's shoreline. Each water step, every outlet emits
  `q = min(MAX_FACE_FLOW, head·K)` units into its CA cell. Here `head = L − y_face·4096`;
  a Torricelli √ is available via an integer table. The emitted amount is subtracted from
  `volume`. Cost is **O(outlets)**.
  - When an outlet's CA cell is full and quiet, it is **absorbed**: the cell becomes a
    member and `area` updates. Small pockets therefore fill at the outlet rate and then
    join the lake, with no teleporting and no special case.
- **Displacement:** placing a solid block into a member cell shrinks `area` and pushes
  `volume` up (O(1)). If that lifts the level to a sill, the sill cell becomes an outlet
  and the overflow spills there. This replaces the plan's "displace in face order, else
  reject" rule.
- **Layer crossing:** happens once per metre of level change and costs O(shoreline) for
  that layer.
  - **Falling:** the members of the vacated layer leave. Then run a **bounded connectivity
    check** (bitset or stack flood, ~5 ns per cell) to see whether the body split at a sill.
    If it did, it becomes several bodies with volumes by exact per-component counts. Only
    the component that holds an outlet keeps draining; the others stop at the sill, which
    is FSM's behaviour. For a 40 × 40 × 8 lake that is ≤ 13 k cells ≈ 65 µs, once per metre.
  - **Rising:** scan the new layer's shoreline for air cells that join the body or become
    outlets.
- **Merge:** when an absorbed cell, or a rising level, touches a second body at the same
  level (±1 unit), union them. Volumes add, and areas add layer-wise.
- **Coalescing CA water into a body:** a connected patch of CA cells that has been asleep
  K steps with a flat top becomes a new body. That costs a flood fill over the patch, once.
  CA water is transient by design.
- **Infinite bodies** (sea, rivers, springs): the level is fixed and outlet emission never
  depletes them. The sea's membership can stay **implicit** (worldgen-marked
  "sea-connected air below `SEA_LEVEL`"). Only player edits below sea level need explicit
  spans. Rivers from the forge would be infinite bodies with a *per-column* baked surface,
  like Minecraft flowing-from-source: static until an edit opens an outlet.

**Determinism:**

- Everything above is integer math, and all cell sets iterate in canonical
  `(y, z, x)` order.
- Body ids come from a monotonic counter in rollback state.
- A split assigns new ids in order of each component's minimum cell.
- A flood fill's traversal order does not change its *result* set; iterate that set sorted
  whenever order matters.
- Work budgets are counted in **cells, never time**, so a bounded flood that resumes next
  step is still deterministic.

### 5.4 What stays per-cell: the frontier CA

This covers streams, waterfalls, sheets spreading over dry land, and the water inside an
outlet's first cells. Use the plan's integer rule: down first, then the horizontal edges,
capped, with the axis permutation by tick. Two things change:

1. Store the cells in **8³ bricks** with dirty AABBs, not an ordered map (the 50× from §1).
2. Use a **checkerboard or Margolus** ordering instead of the pure ordered sweep. You get
   symmetry without a flux buffer, and the order-invariance makes golden-hash tests less
   fragile.

The frontier is small: hundreds to a few thousand cells even in a big drain.

## 6. Cost comparison

Scenarios, for one region:

- **Settled:** a sea-covered region plus a 40 × 40 × 8 m lake, nothing touched.
- **Small spill:** a player breaks one block in the lake's bank next to a 10 m³ dry pocket.
- **Big drain:** a player cuts a 1 × 2 m opening at the lake's bottom, into a valley or cave
  below. Outflow is ~1 m³ per water step (a 10 m³/s tunable), so ~12,800 steps (21 min at
  10 Hz) to empty.

Costs are per water step at ~10 ns per active cell, single thread. ×10 = the worst-case
rollback re-sim.

| Model | State per region (sea + lake) | Settled | Small spill | Big drain | Determinism | 3D generality | Believability |
|---|---|---|---|---|---|---|---|
| **Minecraft levels + sources** | 4 bits per cell (~130 KiB dense), or block states | 0 | ~10–30 flowing blocks ≈ 1–5 µs per flow tick, then static | **Can't drain**: the lake is sources, so a bounded 7-block fan, ~50–100 blocks | easy (ints); scheduled-tick order must be canonical | full | low for volume (infinite water), high for readability |
| **Plan: `u16` finite-volume + sleep, BTreeMap chunks** | ~1.5–2 MiB (+ settle ages) | 0 | pocket plus the **whole lake surface** re-levelling (1,600 cells × ~200–800 steps): ~0.7 ms per step as stored (460 ns per cell), ~15 µs in dense bricks | 1,600 surface cells + ~500 stream cells for 21 min: **~1 ms per step as stored**, ~20 µs dense; the lake surface **slopes toward the outlet** | easy with fixed order; bias needs care | full | medium: slow levelling, stair-steps, no U-tubes without compression |
| Plan + compression (Forsyth / W-Shadow) | same | 0 (if slept) | as above | as above; pressure spreads to full cells | easy | full | medium+ (U-tubes), ambiguous "full" |
| Checkerboard / Margolus CA | same, or less with bricks | 0 | as plan | as plan | **easiest** (order-free) | full | as plan (levelling still O(L²)) |
| Bit-sliced CA (full/empty) | 1 bit per plane (~32 KiB per 64³) | 0 | ~ns | ~µs even with everything awake | easy | full | **low**: no partial fills, grainy spreading |
| **Virtual pipes / SWE heightfield** | ~80 KiB (`h` + fluxes per column) | ~0 (needs damping + threshold) | lake rings, re-levels in **O(L)** ≈ 40 steps × 1,600 cols ≈ 30 µs per step | all lake columns active ≈ 30–40 µs per step; nice currents | fixed point needed, **CFL** at 10 Hz: depth ≲ 10 m or substeps | **none** (one layer) | high on open terrain (waves, currents); no caves or waterfalls |
| Layered spans + pipes | ~50–150 KiB | ~0 | per wet span ≈ ⅛ of cell CA | per wet span ≈ ⅛ of cell CA | fixed point; complex | good, but flooded passages need extended pipes | high |
| **Bodies + frontier CA (recommended)** | **~20–50 KiB** spans + bodies (sea implicit: ~0) + bricks only where water flows | **0** | pocket fills via 1 outlet: ~10 cells ≈ **0.1 µs per step**; lake level O(1), no surface wake | **O(1) body + ~200–2,000 CA cells ≈ 2–20 µs per step**, + ~65 µs once per metre of drop; surface stays **flat** | easy: integers, canonical ids and order, cell-count budgets | **full** (caves, U-tubes exact) | high for lakes, pools, floods; CA-level for streams; no waves (shader) |

**Undo log size per water step**, as before-images:

- **Plan:** one before-image per touched `WaterChunk`, ~24 B + key.
  - Big drain: ~250–1,000 chunks × ~40 B ≈ **10–40 KiB per step**, i.e. 100–400 KiB per
    10-step rollback window.
  - Small spill: the same, for minutes.
- **Body model:** each body touched costs a before-image of ~64 B, plus `area[top]` (4 B).
  Frontier CA cells cost `(brick, cell, old u16)` ≈ 6 B each, or a 1 KiB brick image when
  more than 1/6 of the brick changed.
  - Big drain: ~64 B + 200–2,000 × 6 B ≈ **1–12 KiB per step**.
  - Small spill: ~100 B per step.
  - Settled: 0.
  - Membership edits (a layer crossing or split) log their span before-images: one column
    list per changed column, ~O(shoreline) × 8 B, once per metre.

## 7. Rollback-specific options

The client re-simulates up to ~20 ticks, so water pays **up to 10×** at a 10 Hz cadence.

1. **Predict water, as the plan does.** Water stays in `Rollback` and the client runs the
   same code. With bodies the per-step cost is µs, so ×10 is still negligible. The spikes
   are membership recomputes (flood fills, ≤ ~0.1–1 ms). Bound them by a **cell budget** and
   let them resume next step, which stays deterministic.
   **This is the recommendation.** Swimming and buoyancy read water inside predicted
   movement, so water must be in the same deterministic state as the player to avoid
   mispredicts near changing water.
2. **Lower cadence for the frontier CA only,** e.g. every 4 ticks (5 Hz). Minecraft flows at
   4 Hz. This halves re-sim cost, with no correctness impact while the cadence stays a
   protocol constant.
3. **Delayed-input water (hybrid delay/rollback).** The water step at tick N reads terrain
   edits from tick N − D. Replaying from tick R to H only changes water steps with
   N − D ≥ R, so if D ≥ the typical rollback depth, most rollbacks re-run **zero** water
   steps. The price is that water reacts D ticks late: D = 6 gives 300 ms, which is
   invisible for water. It needs a small edit queue in rollback state, and the water
   subsystem must be skippable during replay when its inputs are unchanged. That is a
   bigger change to the rollback driver, so keep it in reserve until profiling shows water
   re-sim matters.
4. **Server-authoritative, not predicted:** the server sends body level/volume deltas plus
   frontier cell deltas. That removes client water CPU but breaks prediction:
   - swimming and edits near moving water mispredict;
   - a player's dam-break reacts only after an RTT;
   - water leaves the state-hash desync check.
   It also *adds* bandwidth, where today deterministic evolution needs none. With the body
   model, prediction is cheap, so this is not worth it. The one place it fits is the
   rendering-only surface wobble, which is shader work and never simulated anyway.

**Cross-region:** infinite bodies (sea, rivers) have a fixed level, so seams don't matter.
Finite bodies that cross a seam should become **manager-owned world bodies**. Each region
reports its per-step `Δvolume` for that body, and the manager broadcasts the new level. That
is *one number per body per step*, much cheaper than the plan's per-cell boundary strips.
The frontier CA still needs the plan's strip exchange, but only where CA water is actually
at a seam. v1 can keep the plan's closed seam for finite bodies.

## 8. Recommended architecture and first prototype

**Architecture:** "Bodies + frontier CA".

1. `Body { id, level: u32, volume: u64, infinite, area: Vec<u32> (per layer), outlets }` lives
   in rollback state as a tier-1 `UndoMap<BodyId, Body>`. Membership is column spans in a
   per-region `UndoMap<ColumnIdx, SmallVec<[Span; 2]>>`. The sea is an implicit infinite body.
2. A frontier CA in 8³ `u16` bricks with active masks, dirty AABBs and a region
   brick-active bitmask. Checkerboard or Margolus ordering, integer, capped transfer. It
   runs at 10 Hz, or 5 Hz if needed.
3. Interfaces between the two, all O(1) per interface cell: outlets emit, inflows are
   absorbed, quiet CA patches coalesce into new bodies, and bodies merge on contact.
4. Membership changes happen only on events: edits, layer crossings, splits and merges. Each
   is a bounded flood fill (bitset or stack, ~5 ns per cell) with a cell budget.
5. Rendering gets one uniform per body for the sub-metre level, and remeshes only the layer
   crossed. A draining lake stops being a per-cell remesh storm. This is a side win for the
   render path.

**Estimated costs per region:**

| Scenario | Cost per water step | Rollback worst case (×10) |
|---|---|---|
| Settled world | 0 µs | 0 |
| A player splashing about | < 1 µs | < 10 µs |
| A 20-minute lake drain | ~2–20 µs, plus ~65 µs per metre of level drop | ≤ 0.2 ms |

State per region is ~20–50 KiB against ~1.5–2 MiB. The plan is already as cheap as
Minecraft when settled. The body model goes further in four ways:

- it conserves volume;
- lakes can be drained;
- U-tubes come out correct;
- there is no minutes-long levelling tail.

**Prototype first:** an offline, headless, throwaway harness. Use a `game` test or an
example, with no renderer or netcode. It covers the three scenarios in §6 on a 64³ fixture
and runs both the **plan CA in dense bricks** and **bodies + frontier CA**. Measure:

- ns per step and total steps to sleep;
- undo bytes per step;
- exact mass conservation;
- surface flatness during a drain (max − min level across the lake);
- the same fixture under wasm32, to check the golden hash.

The riskiest pieces, which should be built first, are:

1. **Outlet → CA → absorb → coalesce** without mass drift or oscillation between "member"
   and "CA cell" at the interface. Absorb and coalesce need hysteresis: K quiet steps, and
   a level match within ±1 unit.
2. **Split at a sill during a drain,** checked by the per-layer connectivity pass.

If the body model's interface rules turn out fiddly, the fallback is still a large win. Keep
the plan's CA, but (a) store it in dense bricks, which is the 50× from §1, and (b) add
Terraria-style **analytic settling**: once a connected wet patch's activity falls below a
threshold, flood it and set it flat to its mean level in one step. That is the body model's
"coalesce" without the bodies.

## Sources

- Forsyth, *Cellular Automata for Physical Modelling* (GPG3, 2001): https://tomforsyth1000.github.io/papers/cellular_automata_for_physical_modelling.html
- W-Shadow, *Simple Fluid Simulation With Cellular Automata*: https://w-shadow.com/blog/2009/09/01/simple-fluid-simulation/
- jgallant, *2D Liquid Simulator With Cellular Automaton*: https://www.jgallant.com/2d-liquid-simulator-with-cellular-automaton-in-unity/ · https://github.com/jongallant/LiquidSimulator
- Minecraft fluids: https://minecraft.wiki/w/Water · https://minecraft.wiki/w/Fluid
- Dwarf Fortress pressure / flow: https://dwarffortresswiki.org/index.php/DF2014:Pressure · https://dwarffortresswiki.org/index.php/DF2014:Flow
- Terraria liquid source (panic mode / QuickWater): https://github.com/TheVamp/Terraria-Source-Code/blob/master/Terraria/Liquid.cs · https://terraria.wiki.gg/wiki/Liquids
- Noita / Falling Everything: https://80.lv/articles/noita-a-game-based-on-falling-sand-simulation · https://braindump.jethro.dev/posts/gdc_vault_exploring_the_tech_and_design_of_noita/
- Block CA / Margolus: https://en.wikipedia.org/wiki/Block_cellular_automaton · http://psoup.math.wisc.edu/mcell/rullex_marg.html · https://github.com/GelamiSalami/GPU-Falling-Sand-CA
- Mei, Decaudin, Hu 2007, *Fast Hydraulic Erosion Simulation and Visualization on GPU*: http://www-evasion.imag.fr/Publications/2007/MDH07/FastErosion_PG07.pdf
- Beneš 2007, *Real-Time Erosion Using Shallow Water Simulation*: https://www.cs.purdue.edu/cgvlab/www/resources/papers/Benes-2007-Real-Time_Erosion_Using_Shallow_Water_Simulation.pdf
- Dagenais et al. 2018, *Extended virtual pipes…*: http://profs.etsmtl.ca/epaquette/research/Papers/Dagenais.2018.cg/
- Nilles et al. 2024, *3D Hydraulic Erosion using Multi-Layered Heightmaps*: https://diglib.eg.org/bitstream/handle/10.2312/vmv20241211/vmv20241211.pdf
- McDonald, *SoilMachine layer map*: https://nickmcd.me/2022/04/15/soilmachine/
- Müller-Fischer, *Fast Water Simulation for Games Using Height Fields* (GDC 2008): https://media.gdcvault.com/gdc08/slides/S6509i1.pdf
- Timberborn water: https://www.gamedeveloper.com/design/deep-dive-timberborn-s-water-mechanics · https://timberborn.fandom.com/wiki/Water
- Barnes, Lehman, Mulla 2014, *Priority-Flood*: https://arxiv.org/abs/1511.04463
- Barnes, Callaghan, Wickert 2021, *Fill–Spill–Merge*: https://esurf.copernicus.org/articles/9/105/2021/ · https://github.com/r-barnes/Barnes2020-FillSpillMerge
- Barrett, *Connected components algorithm* (hierarchical union-find): https://stb.handmade.network/blog/p/1136-connected_components_algorithm
