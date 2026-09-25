# DF-quality rivers, lakes, valleys and erosion in an unbounded, deterministic, chunk-local generator

Research report, 2026-09-20. Fourth of the set: `01-pipeline-and-fields.md`,
`02-rivers-lakes-erosion.md` and `03-geology.md` document what Dwarf Fortress does on one finite
world map. This report is the bridge: which techniques give hydrologically plausible terrain when
there is **no finite map**, generation is a **pure function of (position, seed)**, and results must
be **bit-identical on every machine**. It ends with three candidate architectures for this engine
and a recommendation.

Evidence tags used throughout:

| Tag | Meaning |
|---|---|
| **[S]** | Read this session from the primary source (paper text, source code, author's post) |
| **[K]** | Background knowledge, not re-verified this session — treat as "probably right, check before relying on a number" |
| **[E]** | My estimate or inference. All cost figures for things we have not built are [E] |
| **[D]** | Design proposal for this engine |

---

## 0. The problem in one page

### 0.1 What our generator is (from the code)

- `TerrainPipeline::column(x, z)` (`crates/worldgen/src/pipeline.rs`): six 2D octave noises →
  `ColumnProfile { offset_h, factor, jaggedness }`. Surface height is a 2D heightfield:
  `offset = clamp(base(raw)·(0.55 + 0.45·egate) + mountains·1.75)`, × 96 m, + sea level 64.
  `mountains = inland_gate(raw) · egate · peaks(pv)` is **always ≥ 0**, and `pv` is the
  peaks-and-valleys fold of a domain-warped ridge noise (valley floor at `ridges = 0`).
  Measured 377 ns per call.
- `generate_region` samples `column()` on a 4 m corner lattice (`ProfileGrid`, 17² = 289 samples
  per 64 m region), bilinearly interpolates, scans the surface per 1 m column and chisels the
  surface blocks to 1/16 m voxels. ~64 ms per region, of which `column()` is ~0.2%.
- `coarse_tile` point-samples the same profile once per LOD cell column (cells 1–16 m). Water is
  "fill to `SEA_TOP_BLOCK`"; the sea surface sits on a multiple of every LOD cell because
  any-solid downsampling rounds surfaces **up** a cell and an off-lattice sea showed as a wall at
  ring boundaries.
- Border agreement today is by construction: lattice corners are pure functions of world
  coordinates, so neighbouring regions compute identical values
  (`profile_grid_continuous_across_region_edges`).

### 0.2 Why rivers are the hard case

A river is the one terrain feature whose value at a point depends on unbounded context: its
elevation depends on everything **downstream** (it must stay above the sea and below its own
upstream), its size on everything **upstream** (drainage area). alcatrazEscapee's survey for
TerraFirmaCraft states the taxonomy cleanly [S]: a generation method is *context free* (pure
noise), *bounded* (needs a fixed finite neighbourhood) or *unbounded* ("traverse until reaching
ocean"); only the first two work in a chunked world, and both constructive river methods —
drain-to-source and source-to-drain — are unbounded as naively stated.

Every shipped solution is one of four moves:

1. **Give up the context** — noise rivers (Minecraft). No direction, no hierarchy, one elevation.
2. **Make the world finite** — simulate a whole map, store it, sample it (DF, Veloren).
3. **Partition so the context is bounded** — watersheds/continent cells that are closed by
   construction (TerraForged, TFC-TNG, DF's per-edge crossing records).
4. **Invert the dependency** — build the drainage graph first from a field that has no interior
   minima, then derive elevation *from* the graph, so "downhill" is true by construction
   (Génevaux 2013, Red Blob mapgen2/mapgen4, Dendry).

Moves 3 and 4 are the ones compatible with our constraints, and they combine.

### 0.3 Two different requirements that are easy to conflate

- **Determinism**: both machines compute the same bits. Satisfied by *any* pure function of a
  fixed key (a tile coordinate included), using only correctly-rounded operations in a fixed order.
- **Border agreement**: two *adjacent cache tiles* produce the same terrain along their shared
  edge. This is a continuity property and is *not* implied by determinism. A per-tile droplet
  simulation is perfectly deterministic and still leaves a step at every tile edge.

The general rule that delivers both [D, the LayerProcGen principle — S]:

> Define every derived value as a pure function of a **bounded neighbourhood** of lower-level
> data (hash-jittered lattice nodes, noise samples). A tile is then only a *cache granularity*:
> compute the tile's core from a window padded by the dependency radius, and the core values are
> identical no matter which tile (or tiling) computed them.

Corollaries used below:
- An **explicit** stencil run for `k` double-buffered (Jacobi) iterations has dependency radius
  `k·r`; a window padded by `k·r` cells gives bit-identical cores. Exactly tileable.
- An **implicit** solve along flow paths (FastScape), **priority-flood**, **flow accumulation**
  and **sequential droplets** have unbounded dependency chains. Not exactly tileable unless the
  domain is closed (an island) or the quantity is explicitly **capped** at a hop count.

### 0.4 Arithmetic that is safe

- `+ − × ÷`, `floor`, comparisons, integer ops, **`sqrt`** — IEEE-754 requires `sqrt` to be
  correctly rounded, and x86 `sqrtsd`, AArch64 `fsqrt` and wasm `f64.sqrt` all are [K]. Rust does
  not fuse `a*b+c` into FMA unless asked [K]. So `sqrt` is available, which matters: the two
  exponents hydrology needs are near ½ (channel width ∝ Q^0.5; channel slope ∝ A^−0.5; stream
  power `m ≈ 0.4–0.5`). `A^0.5 = sqrt(A)`, `A^0.25 = sqrt(sqrt(A))`, `A^0.375 =
  sqrt(sqrt(sqrt(A·A·A)))`.
- `powf`, `exp`, `ln`, `sin`, `cos` via the platform libm are **not** portable. Via the pure-Rust
  `libm` crate (already forced for `simba` in this workspace) they are, at a few tens of ns each.
- Graph algorithms should run on **integers**: quantise node heights/costs once
  (`floor(h·256) as i32`), break every tie by node coordinates. Then heap order, sort stability
  and float associativity cannot matter.

---

## 1. Global-simulation methods, and what bounding them costs

### 1.1 Stream power law with uplift (Cordonnier 2016; Braun & Willett 2013; Schott 2023)

Model: `∂h/∂t = U − K·A^m·S^n` — uplift `U` against fluvial incision proportional to drainage
area `A` and slope `S`; usually `n = 1`, `m ≈ 0.4–0.5` [S Veloren source: `m = 0.4`, `n = 1`].

- **Braun & Willett 2013 (FastScape)** [S abstract, K details]: order nodes by the receiver tree
  (the "stack"), accumulate `A` in one reverse pass, then solve the implicit update from outlets
  upstream in one pass. O(n) per step, unconditionally stable, so time steps can be huge. With
  `n = 1` the update is linear:
  `h_i ← (h_i + Δt·(U_i + c_i·h_recv)) / (1 + Δt·c_i)`, `c_i = K·A_i^m / Δx` — only `A^m` is
  non-arithmetic, and `m = 0.5` makes it a `sqrt`.
- **Cordonnier et al. 2016** [S abstract, K details]: first CG use; user-painted uplift map;
  stream graph on an irregular mesh; local minima handled by a **lake graph** — each basin finds
  its lowest pass to a neighbour, a minimum spanning tree of passes routes every lake to the
  boundary. Outputs mountain ranges with dendritic valleys at the scale of 100 km; resolution is
  coarse (hundreds of metres) and detail is added afterwards.
- **Schott et al. 2023** [S project page]: authoring in the *uplift domain*; GPU, interactive; a
  parallel approximation of drainage area that converges over iterations. Code:
  `H-Schott/StreamPowerErosion`.
- **Tzathas et al. 2024** [S paper text]: analytical solutions of the SPL — time becomes "the
  parameter of a mathematical function" instead of an iteration count. The steady state is the
  part that matters to us: along a flow path, `z(x) = z_outlet + ∫ (U/K)^{1/n} · A^{−m/n} ds`.
  **Given a drainage tree with areas, steady-state river elevations are a single upstream
  integral** — no time stepping at all. Its stated limitation: the river network itself still
  needs depression breaching on the input, and pure SPL gives infinite slopes at ridges, so
  hillslope terms are added [S].

Quality: the best available. Dendritic networks at every scale, concave river long-profiles,
ridges as watershed divides, water gaps. From above it reads as real topography; at walking scale
it has **no content** below its grid spacing — everything under ~2 cells is interpolation plus
whatever detail you add.

Against our constraints:
- *Locality*: elevation depends on the whole downstream path, area on the whole upstream tree.
  Padded macro-tiles do **not** agree at borders unless the padding contains every catchment that
  touches the tile and its path to base level. With 15–40 km landmasses that means a ~100 km
  window per tile — millions of cells [E].
- *The natural boundary condition exists*: **sea level at the coast is a Dirichlet boundary**. A
  landmass entirely surrounded by ocean is a closed system; simulate it once, cache it, and there
  is nothing to disagree about at its border. The catch is that "landmass" is a connected
  component of a noise field, whose size is unbounded in principle (at ~33% land we are below
  percolation, so components are finite, but the tail is heavy [E]). TerraForged sidesteps this by
  *making* continents closed: continents are jittered cells with a falloff to ocean at the cell
  edge [S source].
- *Determinism*: fine with `m = ½`, integer-ordered sorts, single-threaded or order-independent
  passes. Veloren's version uses `powf`/`exp` on `f64`/`f32` and is not designed to be bit-portable
  — it does not need to be, the map is an asset [S].
- *Cost* [E]: ~50–100 ns per cell per iteration for sort + receivers + lake MST + solve, 50–100
  iterations to approach steady state. 256² cells ≈ 0.2–0.6 s; 512² ≈ 1–2.5 s; 1024² ≈ 5–10 s.

### 1.2 Depression filling and flow routing (Barnes et al.; D8/D∞)

- **Priority-Flood** (Barnes, Lehman, Mulla 2014) [S abstract]: flood inward from the DEM edge
  with a priority queue; O(n log n) floats, O(n) integers; the improved variant uses a plain queue
  inside a found depression. The filled level of a depression *is* its spill elevation — the
  lake surface. The "+ε" variant leaves a tiny gradient so flow directions are defined across
  flats [K].
- Planchon–Darboux is the older equivalent; Veloren's `fill_sinks` uses it [S docs].
- **D8** sends all flow to the steepest of 8 neighbours (tree → clean river lines, grid bias);
  **D∞ / multiple-flow** splits flow (smoother area field, no single channel). Veloren uses
  multi-receiver weights for erosion and single paths for rivers [S source].
- Tileability: flooding from the boundary has unbounded reach — a lake's level is set by a pass
  that can be arbitrarily far away. Only exact on a closed domain, or when **capped** (search for
  a spill within R cells, else declare the basin endorheic).

### 1.3 Particle / droplet hydraulic erosion (Beyer 2015; Lague; McDonald)

- Beyer's thesis / Sebastian Lague's implementation [S existence, K details]: a droplet follows
  the bilinear gradient with inertia, erodes when under capacity and deposits when over, through a
  radial brush; ~50–200k droplets for a 256² map. Produces gullies and fans; no persistent rivers.
- Nick McDonald's SimpleHydrology [S]: particles additionally write a time-averaged **stream
  map** and a **pool map**; stuck particles raise a flood plane until a drain point is found.
  ~1 µs per particle step on 256². Author's own limits: pools "see a wall at the boundary of the
  map", the result is "highly dependent on the choice of initial terrain", canyons are hard.
- **How TerraForged tiles it** [S — read `ErodedNoiseGenerator.java`/`ErosionFilter.java`]: per
  16×16 chunk it assembles a 3×3-chunk (48×48) heightmap from cached neighbour noise, runs 350
  droplets *per chunk of the window*, each droplet's start seeded by the **absolute** chunk
  coordinate and iteration index, and keeps only the centre chunk. Same droplets fall in the same
  places whichever centre is being generated, but the droplets are applied sequentially to an
  evolving window and droplets from two chunks away are missing — so adjacent chunks agree only
  *approximately*. It works because each droplet moves very little material.
- For us: deterministic (pure function of the region key), but (a) not exactly continuous across
  region borders — at 1/16 m voxels a centimetre step is a visible crease [E]; (b) it cannot be
  evaluated at a point, so `coarse_tile` cannot reproduce it and near/far terrain would
  disagree; (c) at 1 m resolution it costs more than the region does now (48² ≈ 2.3k cells is
  cheap, but our window would be 192² plus padding). **Rejected as a base technique**; acceptable
  only as a cosmetic near-field micro-pass, which we do not need if §3's filter works.

### 1.4 Thermal erosion / hillslope diffusion

Explicit, local stencils (talus-angle slumping; linear diffusion). `k` Jacobi iterations are
exactly tileable with `k` cells of padding (§0.3). Useful as a *finishing pass* on a coarse
cached grid (C) — it is what removes the infinite ridge slopes of pure SPL. Veloren does both
(thermal earlier, implicit ADI diffusion later) [S devblog 43, source].

### 1.5 What people do to bound global methods — summary

| Move | Who | Exact borders? | Cost to us |
|---|---|---|---|
| Finite stored map, sample per chunk | DF, Veloren | n/a (one map) | World becomes finite, or one map per continent |
| Closed continent cells (ocean at cell edge) | TerraForged | yes (nothing at the border) | Continent shapes change: a cell mask on `continents` |
| Watershed partitions, rivers local to each | TFC-TNG, TerraForged ≤0.2 | yes, rivers never cross partitions | Partition borders are forced divides |
| Per-edge crossing records | DF mid-level map (report 02 §3) | yes, neighbours share the record | Needs a coarse graph anyway |
| Tiny partitions with direct height lookup | Streams mod | yes | Visibly repetitive [S gist] |
| Hierarchical coarse→fine | Dendry, Génevaux, DF world→embark | yes if each level is bounded | The approach of §2 |

---

## 2. Hydrology-first procedural methods (no simulation)

### 2.1 Génevaux et al. 2013 [S paper text]

Pipeline: river mouths seeded on the domain contour → the network **grows inland**: repeatedly
take the lowest candidate node within a tolerance ζ, prefer the highest priority (Horton–Strahler
number), expand by one of three grammar rules — continuation `Pc`, symmetric junction `Ps`
(two children of order n−1), asymmetric junction `Pa` (orders n and m<n); e.g. `Pc/Ps/Pa =
0.2/0.7/0.1` gives many small streams. New node elevation comes from a **river slope map**, and a
candidate must keep a minimum distance from existing edges. Then: Voronoi cells of the nodes are
the sub-watersheds (cell edges not crossed by a river are ridges); flow from drainage area by the
empirical law `φ = 0.42·A^0.69`; Rosgen river types pick channel geometry; terrain is a
**construction tree** of primitives blended and carved, evaluable at any point and natively
multiresolution. Reported: terrains of ~1000–3400 km² in a few seconds (graph 0.1–5 s, tree
~4.5 s), ~1.5 kB/km². Limits stated: only fluvially eroded terrain; the network subdivides
upstream only (no deltas, oxbows); greedy growth can give meandering crests.

Take-aways: (1) growing from the coast inland by elevation priority is exactly a Dijkstra-ordered
traversal, which is why rivers cannot form pits; (2) terrain is *fitted to* the network;
(3) evaluation is per-point over nearby primitives — the right shape for us. What does not
transfer: growth over a whole bounded domain with collision tests against all existing edges.

### 2.2 Red Blob Games: polygon maps, mapgen4, river growing

- **Polygon map generation (2010)** [K; page not fetchable this session — TLS error]: elevation
  of a polygon corner = graph distance from the coast; rivers then "always flow downhill" and
  there are no local minima, *by construction*. The author's framing: make the elevation serve
  the features you want rather than hoping features emerge from elevation.
- **mapgen4** [S, partial]: Delaunay/Voronoi dual mesh; elevation built from distance fields
  (coast, mountain peaks) — "elevation to match the desired look"; rivers live on the triangle
  mesh as a **binary tree built from the coastline uphill** ("making bends and forks until I end
  the tree with springs"); flow accumulation is one reverse-order array pass adding rainfall to
  each node and pushing it to the parent; rainfall comes from a wind/orographic pass. Whole-map
  regeneration is interactive.
- **Procedural river growing (x/1723)** [S]: basins grown inward from the map boundary with a
  hybrid of BFS and Dijkstra, a noise field biasing which frontier triangle expands next;
  triangles classify as source/bend/fork; 1–3 M triangles/s in JavaScript. The author could
  *not* find a satisfying way to then fit elevation to the grown basins — a useful warning: the
  graph is easy, the terrain fit is the design problem.

### 2.3 Dendry (Gaillard et al., I3D 2019) [S paper text]

"A locally computable procedural function that generates branching patterns." A coarse jittered
grid of key-points is connected by following a **control function** `c` (each key-point links to
its lowest neighbour in a Moore neighbourhood); each finer level doubles the grid resolution and
connects its new key-points to the nearest lower point (`a`, `b` or the midpoint) of the existing
tree; segments are subdivided and displaced (`Δ ≤ 0.08`); evaluation needs only a 7×7
neighbourhood at level 0 and 5×5 at finer levels. Terrain use: river slopes from Flint's law
`S = ρ(2µ−1)^β`, `β = −0.6`, with magnitude `µ` tripling per level (no real accumulation — "we
cannot predict in advance the length of the tributaries"); junction angle `cos α = S_main/S_trib`;
heights elsewhere = river height + `tan θ · distance`, averaged over nearby constant-height
primitives. Cost: "about ten seconds for a 512×512 grid" on four cores ⇒ **~150 µs per sample per
core** [E from that figure] — 400× our `column()`. The *structure* is what we want; the per-point
re-derivation is not. Cache the tree per tile and the per-point cost collapses to a
nearest-segment query.

### 2.4 TerraForged 1.18 rewrite — the closest shipped design [S — read `RiverGenerator.java`, `RiverCarver.java`, `ContinentGenerator.java`]

- Continents are a jittered cell grid; each cell has a low-octave `noise()` value that rises
  inland, and a threshold decides land vs ocean.
- **One river link per cell**: cell A connects to its lowest 4-neighbour B (`DIRS` is 4-connected,
  which also rules out crossing links). `connects()` recomputes B's own lowest neighbour to find
  A's inflows. So a cell's pieces depend on a radius-2 cell neighbourhood — bounded. Pieces are
  computed lazily per cell and held in a 1024-entry lossy cache.
- Each link becomes 2–3 segments: A → C (midpoint of A–M displaced perpendicular by ±0.35 ×
  random) → M (midpoint of A–B); if B is ocean the link is extended all the way so rivers do not
  stop on the beach. Segment height and radius are **functions of the cell value only**
  (`getRadius`: wider toward the coast) — there is no flow accumulation.
- A point query warps the position with simplex noise (cheap meanders), scans the 3×3 cells
  around the nearest cell, keeps the nearest river segment and the nearest lake segment, and
  carves: inside `valleyWidth` the **base level** is lerped to the river's level; the terrain is
  lerped down to bank level with a U-shaped-to-linear falloff modulated by a ridge noise, then to
  bed level inside `bankWidth`. Lakes: a short flat segment at source cells, with probability
  `lakeDensity` or where the base value is low.
- The decisive structural idea: terrain = **base** (smooth, monotone inland, carries the water
  level) + **height** (mountains, hills) on top. Rivers ride the base; relief is suppressed
  toward them. Downhill is guaranteed because both the link and the river level are monotone in
  the same scalar.

Weaknesses [E from the code]: no accumulation (a coastal creek is as wide as a trunk river at the
same distance inland); a cell with no lower neighbour simply has no outflow; river spacing equals
the continent-cell size.

### 2.5 How DF itself fits (see report 02)

DF's transferable structure is the same split: rivers are first-class objects carrying a
**monotone water elevation independent of the terrain field**, with per-edge crossing records,
and the local generator *cuts the terrain down to the river* (report 02 §3.3). Its artifacts —
confluence waterfalls because tributaries are not graded to the trunk, "aqueducts" where river
elevation exceeds terrain, trench canyons from carve-when-stuck — are precisely what assigning
elevations along the tree from the coast upward avoids.

### 2.6 Per-point query against a cached drainage graph — the recipe [D]

1. Find the lattice cell of `(x, z)`; gather segments of the cells within the maximum valley
   half-width (3×3 or 5×5 cells, or a per-region prefiltered list).
2. For each segment: projection `t = clamp(dot(p−a, b−a)/|b−a|², 0, 1)`, distance
   `d = sqrt(|p − (a + t(b−a))|²)` — arithmetic plus one `sqrt`.
3. River quantities at `t`: bed/water elevation `lerp(z_a, z_b, t)` (or the reach's quantised
   water level, §5), width `lerp(w_a, w_b, t)`.
4. Terrain = **lower envelope** over nearby segments of `z_seg(t) + g(d, w)`, where `g` is the
   valley cross-section (bed → bank → floodplain → valley wall). A min of continuous functions is
   continuous; from any point, walking toward the winning segment descends and the segment then
   descends to the sea, so **the envelope has no pits** — every point drains. Ridges appear where
   two segments' envelopes meet: watershed divides, for free.
5. Lakes are flagged nodes: a flat water level `z_node` and a carved bowl.

---

## 3. Pure-function "fake erosion" on noise

| Technique | Mechanism | Dendritic valleys? | Cost per sample [E unless noted] |
|---|---|---|---|
| IQ derivative-damped fBm [S] | accumulate analytic noise derivatives `d`; octave amplitude `/(1 + dot(d,d))` | No — smooths slopes, keeps flats rough; "eroded look", no channels | ≈ plain fBm with analytic derivatives |
| de Carpentier "Swiss" / "Jordan" turbulence (Scape) [S] | ridged/billowed octaves; finer octaves sampled at `p + warp·dsum` (gradient warp) and damped by `1/(1+|dsum|²)` | Partially — elongated gullies down slopes, glacial look; no hierarchy | ≈ fBm + derivative bookkeeping |
| NMS "uber noise" (Murray, GDC 2017) [K] | one fBm with per-octave knobs: ridge sharpness, slope erosion, altitude erosion, gradient domain warp, amplitude damping | Same family as above; variety, not drainage | ≈ fBm |
| Ridged multifractal + domain warp | what we already have in the `pv` fold + `RIDGE_WARP_M` | Valley *lines* (noise contours): closed loops, no direction | already paid |
| Clay John 2018 / Fewes 2023 erosion noise [S via runevision] | per octave, stripes (`cos` for height, `sin` for slope) oriented **along the local gradient**, cell-blended | **Yes** — branching gullies and ridges | see next row |
| **runevision erosion filter + Phacelle noise (2026)** [S] | the above plus: *fade* toward a valley→peak target where slope→0 (crisp peaks/valleys), *stacked fading* (small gullies cannot overwrite big ones), *normalised* gullies, *straight* gullies (sign of the sine → triangle waves → clean branching angles) | **Yes, the best available per-point**; MPL-2.0; ports exist for Unity Burst, Godot, Blender, Houdini, a Minecraft mod and Hytale | per octave one Worley-style cell neighbourhood (hash + rotate + cos/sin each) ≈ 0.15–0.3 µs; 4–5 octaves ≈ **1–1.5 µs**, plus the input height **gradient** = 2 extra `column()` samples by finite difference (≈0.75 µs) unless derivatives are propagated analytically |
| Dendry [S] | §2.3 | Yes, true trees | ~150 µs (uncached) |

Author-stated limits of the erosion filter [S]: gullies sometimes "just stop halfway down a
mountainside"; **no flow accumulation**; the gradient direction is undefined at zero slope, so
behaviour around flats/summits depends on the fade target, and "using altitude [as the fade
target] is indeed not robust in the general case". It is a *texture of drainage*, not drainage:
no river reaches the sea because of it, no lake forms, gully size is set by octave, not by
catchment.

Determinism: it needs `sin`/`cos` — use the `libm` crate, or, since the "straight gullies" variant
already reduces the slope term to a sign (a triangle wave), replace both with piecewise
polynomials [E]. Everything else is arithmetic and hashing. Fully local, so tiling and far-LOD
sampling are free — except that octaves with wavelength under ~2–4 LOD cells alias under point
sampling and should be dropped per LOD factor (their amplitude is small, and `coarse_tile` is
already documented as not bit-consistent with full generation).

How it looks: from above, convincing dendritic mountain flanks — the look of eroded DEM
hillshades. At walking scale, gullies with sharp creases and ridgelines to climb, 10–100 m
features, which is exactly the "sub-100 m surface detail" the continents findings list as still
missing. It does nothing for lowlands (no slope, no gullies).

---

## 4. What voxel games actually ship

- **Minecraft vanilla** [K]: before 1.18, rivers were a biome-layer trick (edges between zoomed
  random cells). Since 1.18 they are the low end of the peaks-and-valleys fold of the
  "weirdness"/ridges noise: where `pv` is lowest the offset spline dips below sea level. Hence:
  always at sea level (y 63), no source or mouth, closed loops and dead ends, no tributary
  hierarchy, canyons wherever a contour crosses high ground. **Our pipeline already contains this
  mechanism** — `pv = −1` at `ridges = 0` is a valley-floor network; a noise river is one spline
  edit away, with exactly these limits.
- **Tectonic, Terralith, William Wythers' Overhauled Overworld** [K]: datapacks over the same
  density-function router. Better continents, ranges and valley shapes; rivers remain noise
  contours at sea level. **RTG** (1.7–1.12) [K]: distance-to-cell-border (cellular noise) rivers,
  same limits. **Terra / Iris** [K]: configurable noise pipelines and carvers, no hydrology.
- **Streams (delvr)** [S gist]: real downhill streams from direct height lookups inside tiny
  256-block partitions — bounded, but "visibly repetitive".
- **TerraFirmaCraft TNG** [S gist]: world partitioned into watersheds early (along plate
  boundaries); several drain-to-source rivers generated inside each; intersection avoidance;
  **midpoint-bisection fractal** refinement for sinuosity. The author's verdict on the central
  trade-off: big partitions allow good fractal rivers but "it was infeasible to incorporate
  height"; small partitions allow height but repeat.
- **TerraForged** [S source]: §2.4 rivers, §1.3 erosion. The most complete shipped answer to our
  exact problem, in Minecraft's infinite-world setting.
- **Veloren** [S book, devblog 43, `erosion.rs`, docs]: three stages — *geological* (noise for
  uplift, rock strength, humidity → erosion sim → altitude map stored as a `.bin` asset),
  *filling* at server start (lakes, rivers, densities), *reshaping* per chunk (cliffs, caves).
  The sim: `dh/dt = U − k·A^m·S^n`, `m = 0.4`, `n = 1`, implicit Gauss–Seidel in height order,
  multi-receiver drainage, sediment/bedrock with exponential soil production, ADI hillslope
  diffusion, thermal erosion; `get_lakes` builds the Cordonnier-style lake graph (lowest pass
  between adjacent basins, MST by pass height) and rewrites `downhill` so every lake spills;
  `get_rivers` sizes channels from Manning's equation (`w ∝ Q^{3/8}·S^{−3/16}`) and classifies
  each chunk as `River`/`Lake`/`Ocean` with spline derivatives. Default map 1024×1024 chunks
  (`x_lg = 10`), ~100 erosion iterations [S search summary]; chunks are 32 blocks [K], so the
  world is ~32 km — *the size of one of our continents*. Columns then interpolate altitude and
  evaluate river splines between a chunk and its downhill neighbour [K; `column/mod.rs` not
  fetchable this session]. The developer's summary: "Just having rivers and lakes guaranteed to
  actually flow downhill and connect up globally makes a huge difference in feel." Known rough
  edges [K]: rivers as chords between chunk centres, sloped lake/river surfaces, awkward
  waterfalls — all consequences of a 32 m simulation grid feeding 1 m voxels.
- **Dwarf Fortress**: reports 01/02. Coarse simulated map (erosion by throwaway downhill
  walkers, then traced rivers with flow accumulation) → mid-level records per world tile →
  48×48 local blocks from interpolation + fractal + a second carve to the river's elevation.

The pattern across DF, Veloren, TerraForged, Génevaux and Dendry is the same two-scale split:
**a coarse, hydrologically consistent skeleton** (cached or stored) plus **local detail that is
forbidden from breaking the skeleton**.

---

## 5. Lakes, elevated water and waterfalls in a voxel world

### 5.1 Lake levels

- Simulation route: depression filling; the fill level is the spill elevation, flat by
  definition (Priority-Flood; Cordonnier/Veloren lake graph for chains of lakes).
- Graph route [D]: assign river elevations from the coast upward with a minimum slope,
  `z(n) = z(recv) + max(S_min, S(A_n))·len`. Where the pre-existing low-frequency terrain `H(n)`
  lies well *below* the graded profile (`H(n) < z(n) − T_lake`), the noise had a basin there and
  the graded river stands above it — flag a **lake at level `z(n)`**. This is depression filling
  along the tree: the level equals what the basin would fill to before spilling down the
  receiver, and it is computed in one bounded pass. Inflows arrive at `≥ z(n)` automatically.
- DF's lakes are blobs placed on rivers with no spill logic (report 02 §5.7); TerraForged's are
  probabilistic at source cells. Both look fine in play, which says lake *placement* matters
  less than lake *flatness and containment*.

### 5.2 Rivers above sea level

Static voxel water must be locally flat, so a descending river is a **staircase of flat
reaches**. [D]:
- Water level of a reach = the segment's graded elevation quantised to the water resolution
  (whole blocks today: water "fills through" a block index). A 60 m descent over 10 km is 60
  one-block steps ≈ every 170 m (riffles); headwaters at 10% slope step every 10 m (cascades).
  Where `S·len` exceeds a threshold, concentrate the drop into one multi-block **waterfall** at
  a segment joint — a deliberate knickpoint rather than DF's accidental confluence falls.
- **Containment invariant**: for every column within the channel mask the terrain is `< W`; for
  every column in the bank ring just outside, terrain `≥ W + freeboard`, both computed from *the
  same* segment parameter `t`. Then water never has an open side except at a reach step, where
  the neighbour is lower water — a waterfall face, which is what we want.
- Where two channels overlap near a confluence, take terrain = min of the carves and water = the
  **lowest** `W` among covering channels; the tributary ends in a step down to the trunk. Because
  elevations are assigned along the tree, the step is at most one reach quantum unless a fall
  was placed there.
- Risk: if the engine later simulates flowing water, static elevated water needs source
  semantics at reach heads (DF does exactly this: infinite source at the upstream map edge).

### 5.3 LOD rings

The sea wall bug came from any-solid downsampling rounding the water surface **up** to different
lattices in adjacent rings. Elevated water will be off-lattice almost everywhere. Options [D]:

1. **Water rounds down, solids round up.** In `coarse_tile` (and the downsamplers' `merge_group`
   rule), a cell whose visible-from-above material is water takes `floor` to the cell lattice
   instead of any-solid ceiling. Water can then never stand proud of a finer ring; at worst the
   shore stands a cell above the water, which reads as a bank. Cheapest; keeps the payload format.
2. **Snap lake levels to a coarse lattice at generation** (e.g. multiples of 4 or 8 m above sea).
   Cheap, slightly DF-like terracing of lake elevations, and only lakes — the large flat surfaces
   where a ring-boundary wall is glaring — need it. Rivers are narrow enough that option 1 is
   enough.
3. Carry water as a separate height layer in LOD payloads. Correct, but a format change.

Thin rivers under point sampling: a 6 m river against 16 m cells flickers in and out. In
`coarse_tile`, **dilate the channel test by half a cell** (`d < w/2 + cell/2`) so a river is
always at least one cell wide, and paint it as water material at the terrain's own (floored)
height — a blue ribbon with no walls. Below some width per factor, drop it (a creek at 2 km is
sub-pixel anyway given `LOD_MAX_CELL_PX`).

---

## 6. Candidate architectures for this engine

### A. Per-point erosion filter + noise rivers — cheap, fully local

- **What**: port runevision's erosion filter (MPL-2.0 — keep it in its own file) to `f64`, apply
  it to the mountain/relief term of `column()`; optionally lower the `pv ≈ −1` valley floors
  below sea level for Minecraft-style rivers.
- **Determinism / borders**: pure function of `(x, z, seed)`; `sin`/`cos` via `libm` or
  polynomial stand-ins; nothing cached, nothing to agree on.
- **Cost** [E]: +1.5–2.5 µs per profile (gradient by finite differences + 4–5 octaves) on a
  0.38 µs base ⇒ `column()` ≈ 5–7× slower. Per region: 289 lattice samples → +0.7 ms on 64 ms.
  But the 4 m `ProfileGrid` lattice band-limits at ~8–16 m wavelength; finer gullies need the
  filter evaluated per 1 m column (4096/region ≈ +6–10 ms) or a 2 m lattice. `world-map` and
  `coarse_tile` scale with the per-sample cost.
- **Pipeline changes**: `column()` gains the filter between the `mountains` term and `offset`
  (it needs the gradient of the pre-filter height, so either three evaluations of the noise
  front-end or analytic derivatives through the splines); the `#[cfg(test)]` `DensityFn` oracle
  needs a matching node. `ColumnProfile` unchanged.
- **Far LOD**: works unchanged; drop octaves below ~2–4 cells per factor.
- **Quality**: mountain flanks go from smooth to dendritic. **No rivers to the sea, no lakes, no
  tributary hierarchy, nothing in the lowlands.** Noise rivers are sea-level canyons in loops.
- **Failure modes**: gullies that stop mid-slope; artifacts at flats where the gradient
  direction flips; slope steepening (p99 slope is already ~2 rise/run at 4 m) making ranges
  harder to walk and raising sub-chunk counts; aliasing in coarse rings.
- **Risk: low. Ceiling: low** — it is detail, not hydrology.

### B. Deterministic drainage graph on a jittered lattice, cached per macro-tile, queried per point — recommended

**Skeleton** [D]:
1. *Nodes*: jittered lattice, spacing `L0` (start at 512 m), offset rows (6-neighbour, so links
   never cross; TerraForged gets the same from 4-connectivity). Position from an integer hash of
   `(seed, i, j)`. Node terrain `H(n)` = the **low-frequency** part of today's height at the node
   (base·erosion, no ridge term), quantised to `i32` (1/256 m). `H < 0` ⇒ ocean node.
2. *Receivers*: capped multi-source Dijkstra from all ocean nodes; edge cost =
   `len · (1 + k_h·max(0, H) + k_n·noise)` in integers, cap `C_max` ⇒ reach ≤ `R` hops.
   `recv(n)` = the neighbour minimising `(dist + cost, H, i, j)` — a total order, so the result
   is independent of heap order. Like distance-from-coast (Red Blob) the distance field has **no
   interior minima**, so every land node within reach drains to the sea; like river-growing
   (x/1723) and Génevaux, the cost term makes rivers follow low ground and wander instead of
   running straight to the nearest beach. Nodes beyond `R` (deeper inland than ~24 km) fall back
   to lowest-`H` neighbour and may end in an endorheic lake — rare by construction at our
   continent sizes.
3. *Flow*: `A(n)` = size of the upstream tree **capped at `D` hops**. Exact and bounded; rivers
   simply stop widening after `D·L0` of length (24 km at D = 48), which is the scale of our
   continents. Optional rainfall weight per node later (DF ignores climate here; report 02 §5.2).
4. *Elevation*: from the coast upward, `z(n) = z(recv) + clamp(k_s / sqrt(A(n)), S_min, S_max) ·
   len` — Flint's law with concavity ½, which is also the SPL steady state for `m/n = ½`
   (Tzathas 2024), using only `sqrt`. Tributaries are graded to their trunk by construction.
   `k_s` is tuned so headwaters ~20 km inland sit near today's +58 m interior plateau. Lakes per
   §5.1; falls per §5.2.
5. *Geometry*: each link → 2–3 sub-segments by hashed midpoint displacement (TerraForged's
   A→C→M scheme or TFC's bisection), plus a low-amplitude domain warp of the query point for
   meanders. Width `w = w0·sqrt(A)` clamped (hydraulic geometry; DF's is linear with a 47-tile
   cap), bed depth and valley half-width as functions of `w` and local relief (narrow V in
   headwaters, wide floor in lowlands — the Rosgen idea without the taxonomy).

**Determinism and border agreement**: every node quantity is a pure function of nodes within
`R + D + 1` hops. A cache tile (say 32×32 nodes = 16 km) is computed on a window padded by that
radius and only its core is kept; any tiling yields the same bits (§0.3). Test it the way
`profile_grid_continuous_across_region_edges` tests the lattice: build the same nodes from two
different tile origins and `assert_eq!`. All graph work is integer; the per-point query is
arithmetic + `sqrt`.

**Cost** [E]: window (32 + 2·97)² ≈ 51k nodes × (one low-frequency profile ≈ 0.3 µs + heap and
tree passes ≈ 0.3 µs) ≈ **30–50 ms per 16 km tile**, lazily, cached in memory (a few hundred kB
per tile) and optionally in the existing worldgen disk cache. One tile covers the whole 2 km
horizon most of the time; at most four are live. Per point: with a per-region prefiltered
segment list (usually 0–6 segments within valley range of a 64 m region) ≈ **50–100 ns per
column**, and regions with an empty list skip the river path entirely. The wasm client pays
~2–3× on the tile build — keep it off the main thread like region generation.

**Pipeline changes** [D]:
- New module `worldgen::hydrology` (graph build, tile cache, `RiverQuery` for a region/tile).
- `column()`: on land, the base term becomes graph-derived — the lower envelope
  `min_seg(z_seg + g(d))` of §2.6 blended into `offset_base(raw)` near the coast so the
  coastline (the `raw = 0` contour) and the offshore drop-off stay exactly where they are; the
  **`mountains` term is multiplied by a valley mask `m(d, w) ∈ [0, 1]`** that reaches 0 on the
  valley floor. Because `mountains ≥ 0`, terrain near a river is always ≥ the river grade:
  no "aqueducts", no conflicts, and water gaps through ranges come for free. This is
  TerraForged's base/height split mapped onto our existing `base + mountains` sum.
- The channel itself (bed, banks, 3–40 m wide) is **too narrow for the 4 m bilinear
  `ProfileGrid`**: apply the channel carve and the water rule per 1 m column after
  interpolation (and per voxel for bank chiselling), from the same `RiverQuery`.
  `ColumnProfile` gains `water_level: Option<…>` or the carve is applied to `offset_h` after
  `profile_at`.
- Water: the column fill rule becomes "water up to `max(sea, W(p))` where the channel/lake mask
  holds". Materials: bed gravel/sand, bank soil.
- `coarse_tile`: same `RiverQuery`, dilated channel test, water-rounds-down (§5.3).
- The `DensityFn` test oracle cannot express a graph lookup; keep the oracle for the noise
  front-end and pin the hydrology with its own invariants (below).
- Fallout, same list as the continents change: new `WORLD_SEED`/spawn scan, tests pinned to
  specific terrain, snapshot sizes, `just bench-column`.

**Far LOD**: the graph cache is resolution-independent; a factor-256 tile asks the same
`RiverQuery`. Valleys (hundreds of metres wide) survive every ring; channels become ribbons,
then vanish.

**Quality**: true source-to-sea rivers with hierarchy and graded confluences; valleys whose
width follows river size; ridges as divides between neighbouring envelopes; lakes at flat
levels with outflows; waterfalls where we choose. From above it reads like mapgen4/Génevaux; at
walking scale it is as good as the cross-section function `g` and the detail noise on top.
Better than DF on every artifact in report 02 §5 except that DF's *erosion of the mountains
themselves* is only imitated (valley mask) rather than simulated.

**Failure modes**:
- Lattice regularity showing through as evenly spaced rivers → jitter, offset rows, cost noise;
  later a second, finer level draining into the first (Dendry's refinement) for creeks.
- Kinks at nodes and chord-like reaches → sub-segments, warp, optional quadratic smoothing.
- Node says land where the column says sea (or vice versa) at the coast → extend mouth segments
  into the ocean node (TerraForged does) and fade the carve below sea level.
- Envelope terrain too "tent-like" between rivers if `g` is linear → make `g` concave-down
  (hillslope) and let `mountains·mask` + `jaggedness` + (later) A's filter supply texture.
- Base elevation changes the world everywhere on land; interior heights are now set by river
  grade. Tune `k_s` against today's hypsometry (p50/p90/p99 above sea: 11/47/143 m).
- Capped accumulation is a visible simplification only on landmasses larger than `D·L0`.
- 5×5 cell scans if prefiltering is skipped: ~0.6 µs per point — more than `column()` itself.

**Risk: medium. Ceiling: high.**

### C. Bounded coarse SPL simulation per continent cell, cached, refined locally — highest quality, highest risk

- **What**: partition the plane into large jittered continent cells (64–128 km); force ocean
  along cell borders with a falloff on `continents` (TerraForged's closure); per cell, run
  Veloren/Cordonnier-style uplift + stream power + lake graph + hillslope diffusion on a coarse
  grid (256 m → 256²–512² cells), with noise-driven uplift (our `mountains` field is a natural
  uplift map) and sea level as the Dirichlet boundary; cache heights, receivers, areas, lake
  levels; per point, bicubic-sample the grid, evaluate river splines between cell centres
  (Veloren), add local detail.
- **Determinism / borders**: a closed cell has no neighbours to disagree with — ocean floor
  comes from the unmodified noise. Inside: `m = ½` (sqrt), `n = 1` (linear implicit update),
  integer-keyed height sort with index tie-break, fixed iteration count, single-threaded or
  colour-ordered passes. No `powf`/`exp` (Veloren's soil-production `exp` would go through
  `libm` or be dropped).
- **Cost** [E]: 256² × 60 iterations × ~80 ns ≈ 0.3 s; 512² ≈ 1.3 s; per continent, once,
  disk-cached; ~1–4 MB per continent in memory. Per point afterwards ≈ 50–100 ns. The first
  region on a new continent stalls for that long on **both** server and client (2–3× in wasm)
  unless precomputed ahead of the player.
- **Pipeline changes**: `column()`'s land branch is replaced by grid sampling + detail; the
  continent mask changes world layout (land share, continent shapes, the spawn seed scan); the
  `DensityFn` oracle no longer describes land at all; `coarse_tile` samples the grid (cheaper
  than today).
- **Far LOD**: trivially consistent — the grid is the coarse truth.
- **Quality**: real fluvial landscapes at map scale. At walking scale the 256 m grid carries
  nothing: valleys narrower than ~500 m, channels, banks and gullies are all post-hoc — i.e. you
  still need B's per-point channel carving and A's detail. Veloren's known rough edges (chord
  rivers, sloped lakes, awkward falls) live exactly at this seam.
- **Failure modes**: tuning is slow (each parameter change = re-simulate, re-render);
  SPL without enough diffusion gives knife ridges, with too much gives mush; iteration count vs
  steady state; lakes at non-lattice levels (§5.3); cold-start stalls; any future change to the
  sim invalidates every cached continent and every world built on it; closed cells make
  continents look cell-shaped unless the mask is well warped.
- **Variant C′ (bounded explicit polish)** [D]: keep B's skeleton and run `k` explicit Jacobi
  iterations of SPL + thermal on a 16–32 m grid per tile with receivers fixed from B, window
  padded by `k` cells. Exactly tileable (§0.3), adds real incision between B's segments without
  closing continents. Costs a grid per tile and cannot be point-sampled for far LOD without the
  cache. Worth considering only after B exists.
- **Risk: high. Ceiling: highest at map scale, not at walking scale.**

### Ranking

| | Rivers to sea | Lakes | Valleys from above | Walking scale | Cold cost | Per-point | Risk |
|---|---|---|---|---|---|---|---|
| A | no | no | textured flanks only | good gullies | 0 | +1.5–2.5 µs | low |
| **B** | **yes, graded** | **yes, flat** | good (envelope + mask) | good, limited by `g` | 30–50 ms / 16 km tile | +0.05–0.1 µs | medium |
| B + A | yes | yes | **very good** | **very good** | same | sum | medium |
| C | yes | yes (spill levels) | best | needs B + A anyway | 0.3–1.3 s / continent | ~0.1 µs | high |

**Recommendation: prototype B first; add A as the detail layer once B's skeleton is in; defer C
(and C′) until B + A is judged insufficient.** Reasons:
1. B is the only option that delivers what "DF-quality" means to a player — a river you can
   follow from a mountain spring through a lake to the sea — within every hard constraint, with
   exact border agreement by a bounded-context argument rather than by hoping.
2. B decides the macro structure that A and C both need. A's filter wants a slope direction and
   a valley→peak fade target; B's envelope supplies both (gradient toward the nearest river,
   `d`/valley-width as the fade), which also fixes the filter's headline weakness: gullies then
   run *into* a real network instead of stopping mid-slope.
3. C's extra quality is at map scale, where we see 2 km at a time; its weaknesses are at walking
   scale, where players live. Its risks (cold stalls on both machines, closed continents,
   re-tuning loops, cache invalidation of whole worlds) are the expensive kind.

### Prototype plan and validation with `just world-map`

Follow the continents precedent: a standalone prototype that `#[path]`-includes the real
`noise.rs`/`spline.rs` and renders through the `world_map` example, touching no world output.

- **B0 — graph only.** Nodes, capped Dijkstra, capped accumulation, graded `z`. Render over the
  existing relief at `--span 65536` and `--span 12000`: segments as lines with width ∝ `sqrt(A)`,
  lakes as discs, colour by Strahler order. Judge by eye against mapgen4 / real drainage maps:
  dendritic, not parallel; trunk rivers several km apart; no grid showing.
- **B1 — terrain from the graph.** Envelope base + valley mask on `mountains`. Re-render colour
  relief and the 16-bit heightmap; hillshade should show valleys continuous from range to coast
  and water gaps through ranges.
- **B2 — channels, water levels, lakes, LOD rule**, then a live look at walking scale
  (`world=tiny`).

Automated checks (cheap, worldgen-crate tests, tagged slow where >100 ms):
1. **Tiling invariance**: same nodes from two tile origins → `assert_eq!` on every field.
2. **Monotone grade**: along every link `z(n) ≥ z(recv(n)) + S_min·len`; every path reaches an
   ocean node or a flagged endorheic lake within `R` hops.
3. **Containment** (§5.2): sampled channel columns have terrain `< W`; bank ring `≥ W`.
4. **Hydrological conformance of the rendered heightmap** — the quantitative one: run
   priority-flood + D8 flow accumulation over a rendered patch (e.g. 4096² at 4 m). Report
   (a) depression volume and area outside the lake mask — today's noise terrain should score
   badly, B1 near zero at the graph's scale; (b) overlap between the D8 high-accumulation cells
   and our river mask — the terrain should route water where we drew rivers.
5. **Network statistics** as regression numbers: drainage density (km of channel per km²),
   Horton bifurcation ratio (natural range ≈ 3–5), Hack's law exponent (≈ 0.6), slope–area
   concavity (≈ 0.5 by construction).
6. **Hypsometry**: land share and p50/p90/p99/max height vs today's 32.8% / 11 / 47 / 143 /
   264 m, so B1 tuning keeps the world recognisable.
7. `just bench-column` before/after; a `hydrology_tile_build` bench with the 30–50 ms estimate
   as the number to beat or refute.

---

## Sources

### Access gaps and retrieval notes

No source below was obtained by getting around a refusal (no mirrors, caches, alternate
endpoints or archive copies). Where a fetch failed, the source is a **gap** and anything said
about it is tagged [K] (memory) in the text.

| Wanted | Why unavailable | What depends on it |
|---|---|---|
| Cordonnier et al. 2016, full PDF (purdue.edu copy) | Fetch tool size limit (>10 MB); not retried by other means | §1.1 lake-graph / MST description and "coarse resolution, detail added afterwards" — from memory, cross-checked only against Veloren's `get_lakes`, which implements the same idea |
| Peytavie et al. 2019, "Procedural Riverscapes" PDF | Fetch tool size limit | Nothing — listed as further reading only |
| Amit Patel, "Polygon Map Generation" (2010), stanford.edu | TLS certificate mismatch on the host. I then fetched the author's separate mapgen2 demo page on redblobgames.com hoping it restated the method; it does not, and nothing is taken from it | §2.2 first bullet (elevation = distance from coast ⇒ rivers always downhill, no local minima) — from memory |
| Veloren `world/src/column/mod.rs` | 404 on the GitHub mirror path (the project lives on GitLab); not pursued | §4 "columns interpolate altitude and evaluate river splines between a chunk and its downhill neighbour" and the "known rough edges" sentence — from memory |
| Sean Murray GDC 2017 talk | Video; not watched this session | §3 "uber noise" row — from memory |
| Minecraft river specifics | Wiki search results did not state them | §4 first bullet (sea-level rivers from the low end of the PV fold, pre-1.18 layer rivers) — from memory |

Retrieval method, for the record: web pages and the Génevaux and Tzathas PDFs came through the
fetch tool (the PDFs arrived as binary and were converted locally with `pdftotext`). Two things
were fetched with `curl` instead, neither after a refusal: TerraForged's public GitHub repository
(REST tree listing + raw files, because `gh` is not installed here) and the Dendry PDF from its
first author's own site (open-access copy). Claims resting on those two: all of §2.4, the
TerraForged paragraph of §1.3, and all of §2.3 plus the Dendry row in §3.

Several [S] items were read through the fetch tool's summariser rather than verbatim (the
alcatrazEscapee gist, Veloren's `erosion.rs`, the runevision post). One summary — mapgen4's
landing page — echoed my own prompt wording back as if quoted; I discarded it and used the
author's river-representation post instead.

### Source list

| Source | URL | Contributed | Reliability |
|---|---|---|---|
| alcatrazEscapee, "Why Are Rivers So Complicated?" | https://gist.github.com/alcatrazEscapee/8bd54572c8d3fe070d1ed92704dd6aed | Context-free / bounded / unbounded taxonomy; survey of Minecraft-mod river methods; TFC-TNG watershed partition + fractal bisection; the partition-size trade-off | High — practitioner who shipped it; read via summary |
| TerraForged source (`RiverGenerator`, `RiverCarver`, `ErodedNoiseGenerator`, `ErosionFilter`, `NoiseTileSize`) | https://github.com/TerraForged/TerraForged | Cell-link rivers, base/height split, carve profile, lakes; 3×3-chunk droplet erosion with absolutely-seeded droplets | High — read the code directly; interpretation of seam behaviour is mine |
| Veloren `erosion.rs` | https://github.com/veloren/veloren/blob/master/world/src/sim/erosion.rs (docs: https://docs.veloren.net/veloren_world/sim/erosion/) | SPL parameters, implicit solver, multi-receiver drainage, lake MST, Manning river sizing, `RiverKind` | High for the sim (read via summary of source); column sampling details not re-read |
| Veloren book — Worldgen | https://book.veloren.net/internals/worldgen/worldgen.html | Three-stage pipeline; map stored as `.bin` asset | Medium (page marked WIP) |
| This Week in Veloren 43; 78 | https://veloren.net/devblog-43/ ; https://veloren.net/devblog-78 | Design rationale, uplift/thermal/diffusion, developer quotes; map size as `2^lg` chunks | High for intent, no numbers |
| runevision, "Fast and Gorgeous Erosion Filter" (2026-03) and "Phacelle — Cheap Directional Noise" (2026-01) | https://blog.runevision.com/2026/03/fast-and-gorgeous-erosion-filter.html ; https://blog.runevision.com/2026/01/phacelle-cheap-directional-noise.html ; Shadertoy https://www.shadertoy.com/view/wXcfWn | The per-point erosion filter: mechanism, improvements, stated limits, MPL-2.0, ports | High — author's own write-up |
| Clay John, "Eroded Terrain Noise" (2018); Fewes (2023) | https://www.shadertoy.com/view/MtGcWh ; https://www.shadertoy.com/view/7ljcRW | Origin of gradient-aligned gully noise | High (code), not read line by line |
| LayerProcGen (runevision) | https://runevision.github.io/LayerProcGen/ | Bounded-context layers with padding = deterministic contextual generation in infinite worlds | High; framework is C#, we take the principle only |
| Génevaux, Galin, Guérin, Peytavie, Beneš 2013, "Terrain Generation Using Procedural Models Based on Hydrology" | https://www.cs.purdue.edu/cgvlab/www/resources/papers/Genevaux-ACM_Trans_Graph-2013-Terrain_Generation_Using_Procedural_Models_Based_on_Hydrology.pdf | Coast-inland growth grammar, Horton–Strahler priorities, Voronoi watersheds, `φ = 0.42·A^0.69`, timings, limitations | High — read the paper text |
| Open implementation of the above | https://github.com/bmelaiths/terrainHydrology | Reference code | Not inspected |
| Gaillard et al. 2019, "Dendry: A Procedural Model for Dendritic Patterns" | https://www.mgaillard.fr/content/publications/pdfs/Gaillard19I3D.pdf | Locally computable hierarchical river trees, neighbourhood sizes, Flint's law slopes, junction angles, cost | High — read the paper text |
| Tzathas, Gailleton, Steer, Cordonnier 2024, "Physically-based analytical erosion for fast terrain generation" | https://www-sop.inria.fr/reves/Basilic/2024/TGSC24/Analytical_Terrains_EG.pdf | Analytical / steady-state SPL: elevation as an upstream integral of `A^{−m/n}` | High — read abstract, contributions, limitations |
| Cordonnier et al. 2016, "Large Scale Terrain Generation from Tectonic Uplift and Fluvial Erosion" | https://hal.science/hal-01262376 | Uplift + SPL in CG, lake graph | High paper; PDF too large to fetch — details from memory and from Veloren's use of it |
| Schott et al. 2023, "Large-scale Terrain Authoring through Interactive Erosion Simulation" | https://aparis69.github.io/public_html/projects/schott2023_Uplift.html ; code https://github.com/H-Schott/StreamPowerErosion | Uplift-domain authoring, parallel drainage-area approximation | High; project page only |
| Braun & Willett 2013, FastScape | https://www.researchgate.net/publication/236741975 ; library https://fastscape.org/fastscapelib-fortran/ | O(n) implicit SPL solver, stack ordering | High paper; details from memory |
| Barnes, Lehman, Mulla 2014, "Priority-Flood" | https://arxiv.org/abs/1511.04463 | Optimal depression filling; fill level = spill level | High — abstract read |
| Peytavie et al. 2019, "Procedural Riverscapes" | https://hal.science/hal-02281637 | River geometry/cross-sections from a heightfield + network (further reading for B2) | High paper; not read this session |
| Derzapf et al. 2011, "River Networks for Instant Procedural Planets" | https://www.researchgate.net/publication/230363605_River_Networks_for_Instant_Procedural_Planets | On-the-fly river networks with LOD on planets (further reading) | Not read this session |
| Red Blob Games — mapgen4; river representation; procedural river growing; polygon map generation | https://www.redblobgames.com/maps/mapgen4/ ; https://simblob.blogspot.com/2018/10/mapgen4-river-representation.html ; https://www.redblobgames.com/x/1723-procedural-river-growing/ ; http://www-cs-students.stanford.edu/~amitp/game-programming/polygon-map-generation/ | Elevation from distance fields; rivers as binary trees grown from the coast; reverse-order flow accumulation; BFS/Dijkstra basin growth and the author's doubts about fitting elevation afterwards | High; the 2010 article could not be fetched (TLS) — cited from memory |
| Inigo Quilez, "more noise" | https://iquilezles.org/articles/morenoise/ | Derivative-damped fBm | High |
| Giliam de Carpentier, "Scape: Procedural extensions" | https://www.decarpentier.nl/scape-procedural-extensions | Swiss and Jordan turbulence formulas | High |
| Sean Murray, "Building Worlds Using Math(s)", GDC 2017 | https://www.gdcvault.com/play/1024514/Building-Worlds-Using ; https://www.youtube.com/watch?v=C9RyEiEzMiU | Uber noise | Talk not re-watched — from memory |
| Nick McDonald, "Procedural Hydrology"; SimpleHydrology | https://nickmcd.me/2020/04/15/procedural-hydrology/ ; https://github.com/weigert/SimpleHydrology | Particle streams and pools; author's stated limits | High |
| Hans T. Beyer 2015 thesis; Sebastian Lague's implementation | http://www.firespark.de/resources/downloads/implementation%20of%20a%20methode%20for%20hydraulic%20erosion.pdf ; https://github.com/SebLague/Hydraulic-Erosion | Droplet erosion reference | High; not re-read |
| Minecraft Wiki — World generation, Noise settings | https://minecraft.wiki/w/World_generation ; https://minecraft.wiki/w/Noise_settings | Weirdness/ridges → PV fold | Medium; river specifics from memory |
| Sibling reports 01–03 (this directory) | `01-pipeline-and-fields.md`, `02-rivers-lakes-erosion.md`, `03-geology.md` | What DF does; river-elevation-independent-of-terrain and its artifacts | As tagged there |
