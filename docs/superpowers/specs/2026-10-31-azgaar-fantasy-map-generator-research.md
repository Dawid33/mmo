# Azgaar's Fantasy Map Generator — how it works

Research 2026-10-31 from the source (github.com/Azgaar/Fantasy-Map-Generator @ d279257, 2026-10-28,
TypeScript, ~115k lines; MIT). Paths below are relative to that repo. Purpose: see what is worth
borrowing for the Dwarf Fortress–style world (the forge + `WorldMap`), not to reproduce FMG.

## TL;DR

- FMG is a **2D map toy with a deep society layer on a shallow physical layer**. One strictly ordered
  pipeline (`src/model/generation-pipeline.ts`) mutates two global graphs, `grid` and `pack`. The
  step order *is* the dependency graph: grid → heightmap → lakes → temperature → precipitation →
  repack → rivers → biomes → goods → population → cultures → burgs → states → routes → religions →
  provinces → names → economy → military/markers/zones.
- **Unit = Voronoi cell**, not raster: ~10k jittered-lattice points (Delaunator), then a second
  "packed" Voronoi that drops deep ocean and adds points along coasts.
- **Terrain has no noise and no erosion.** Heightmaps are *scripts of BFS blob stamps* (Hill, Pit,
  Range, Trough, Strait, Mask, Smooth, Add/Multiply). The "erosion" in the 3D view is a cosmetic GPU
  noise filter (`src/renderers/erosion-bake.ts`).
- **Hydrology is a single downhill pass** over cells sorted high→low, after iterative depression
  filling. Climate is a 1-D wind sweep along lattice rows/columns.
- **Everything political is one algorithm**: multi-source Dijkstra flood (a flat binary heap) with a
  per-entity cost function and a cost cap. The cost tables keyed by culture *type* (Naval, Highland,
  Nomadic…) produce most of the flavour. Leftover land becomes "wildlands" or "neutrals".
- **Nothing is simulated over time.** No history and no agents. Wars, diplomacy and campaigns are
  weighted dice rolls over the final borders. The economy pass (production/markets) is the only
  agent-ish loop. This is the big difference from DF.

## Physical layer

### Grid (`src/model/graph/lattice.ts`, `grid-generator.ts`, `voronoi.ts`)
- The point count comes from a density slider (1k…100k; default 10,000 on a 1280×800 canvas).
  `spacing = sqrt(area / points)`. The lattice is square (default), hex, diamond or triangle;
  `jitter` 0.9 moves points up to 0.9 × spacing/2. Boundary pseudo-points outside the canvas clip
  the edge cells.
- Delaunator triangulation → Voronoi cells (`cells.c` neighbours, `cells.v` vertices) and vertices
  (always 3 cells per vertex). Saves keep only the points; the diagram is rebuilt on load.
- Cell lookup: start at the lattice slot, then greedy-walk to closer neighbours. The lattice gives
  O(1) row/column access even though the cells are irregular.

### Heightmap (`src/model/physical/heightmap-generator.ts`, `src/data/heightmap-templates.ts`)
Heights are `Uint8` 0–100, with sea level 20. A template is a text script, one tool per line:
`Tool count height rangeX% rangeY%` (counts like `1.5` mean 1, plus 1 more with P 0.5).
Templates include volcano, high/low island, continents, archipelago, atoll, mediterranean,
peninsula, pangea, isthmus, shattered, taklamakan, old world, fractious… Alternatively a precreated
PNG is read with a lightness curve (`l < 0.2 ? l : 0.2 + (l-0.2)^0.8`).

- **Hill**: pick a start cell (retried ≤50× until `h+add ≤ 90`). BFS outward; each neighbour gets
  `change[parent]^blobPower × U(0.9,1.1)` until the change drops below 1. `blobPower` is tuned per
  cell count (0.93 at 1k … 0.98 at 10k … 0.9973 at 100k), so a blob covers the same map fraction at
  any density.
- **Pit**: the same blob, subtracted.
- **Range / Trough**: a greedy walk from start to end cell (each step takes the neighbour closest to
  the end; with P 0.15–0.2 a neighbour's distance is halved for wiggle). Then a ring-by-ring BFS
  from the ridge line, with `h ← h^linePower − 1` per ring (`linePower` 0.75…0.93 by density). Every
  6th ridge cell sends a "prominence" spur downhill: `h[down] = (2·h[cur] + h[down]) / 3`.
- **Strait**: a trough-like cut across the map, vertical or horizontal.
- **Mask**: multiply by `(1−nx²)(1−ny²)`, so the map edges sink; a negative power inverts it.
- **Smooth**: blend with the neighbour mean, `(h·(fr−1) + mean)/fr`.
- **Add / Multiply / Invert**: on a height band (`land` = 20–100, rescaled relative to 20).

### Features and lakes (`features-generator.ts`, `grid-generator.ts`, `lakes.ts`)
- Flood fill labels connected land/water components as island / lake / ocean (an ocean touches the
  border). `cells.t` is a signed distance-to-coast field (1, 2, 3… on land; −1, −2… in water).
- `addDeepDepressionLakes`: a local-minimum land cell that cannot reach water within
  `lakeElevationLimit` (20) height units becomes a lake (h = 19).
- `openNearSeaLakes`: a lake separated from the ocean by one coastal cell of h ≤ 22 is breached and
  becomes sea.
- Lake climate: flux = Σ shoreline precipitation; evaporation from a Penman-ish
  `((700·(T + 0.006·hMeters))/50 + 75)/(80 − T) × cells`. A lake with flux > evaporation and not
  `closed` gets an outlet at its lowest shore cell; otherwise it is endorheic (salt/dry subtypes).

### Temperature (`temperature-generator.ts`)
Per lattice row: latitude from the map's lat box. Inside the tropics (16°N…20°S) the drop is
0.15 °C/°; outside it is linear to the pole temperatures. Lapse rate 6.5 °C/km, with height
converted to metres as `(h−18)^exponent` (exponent 2). This is purely latitude + altitude: no
ocean currents and no continentality.

### Precipitation (`precipitation-generator.ts`)
- The wind direction per 30° latitude tier comes from options (trade winds / westerlies). Winds
  enter from the map edge along each lattice **row** (west/east) or **column** (north/south),
  carrying `humidity = maxPrec − h[first]`, scaled by a per-5°-latitude modifier table
  `[4,2,2,2,1,1,2,2,2,2,3,3,2,2,1,1,1,0.5]` (ITCZ wet, horse latitudes dry…).
- Over water the wind gains `+5·mod` humidity per cell. The first land cell after water gets
  `humidity / U(10,20)` as coastal rain.
- Over land each cell rains `max(humidity/(10·mod), 1) + max(Δh,0)·(h_next/70)²` (orographic), and
  some evaporation returns. A cell above 85 blocks the wind entirely (rain shadow). Cells below
  −5 °C get nothing.
- It is cheap because it is 1-D along lattice lines, which is why the grid keeps rows.

### Repack (`src/model/graph/pack-generator.ts`)
Drops ocean cells with `t < −2`, drops 3/4 of interior lake cells, and splits coastal neighbour
pairs that are far apart with a midpoint. A second Voronoi is built from those points. `pack.cells.g`
maps each packed cell to its parent grid cell. **All later steps run on `pack`.** The result is
denser cells where the map matters, i.e. coasts.

### Rivers (`river-generator.ts`)
1. `alterHeights`: add a tiny `t/100 + mean(neighbour t)/10000` so flat land drains away from the
   coast-distance ridge.
2. `resolveDepressions` (≤250 iterations, lowest cells first): raise any land cell not above its
   lowest neighbour to `min + 0.1`; raise an open lake to `minShore + 0.2`. After 75% of the budget,
   lakes that still fail are declared `closed`. The pass aborts and reverts if progress stalls.
   This is iterative pit filling, a slow cousin of Priority-Flood.
3. `drainWater`: land cells sorted high→low. Each adds `prec / (points/10000)^0.25` to its flux and
   passes its flux to its lowest neighbour (or its haven or lake outlet). A river is born when
   flux ≥ 30 (`MIN_FLUX_TO_FORM_RIVER`). Where rivers meet, the bigger flux keeps the id and the
   other becomes its tributary (`riverParents`). A lake outlet carries `flux − evaporation`.
4. Rivers with fewer than 3 cells are dropped. Meandering points are added for drawing; width comes
   from flux, with Fibonacci-like length progression.
5. `downcutRivers` (when erosion is allowed): cells with h ≥ 35 drop by `min(⌊fl / mean upstream fl⌋, 5)`.
   This is the only height change from water.

### Biomes (`biomes-generator.ts`)
- moisture = `4 + mean(prec of the cell and its land neighbours)`, plus `max(fl/10, 2)` on rivers.
- Override order: water → Marine; T < −5 → Glacier; T ≥ 25, dry and no river → Hot desert;
  wetland (moisture > 40 and h < 25, or moisture > 24 and 25 ≤ h < 60, unless T ≤ −2).
- Otherwise a 5 × 26 lookup matrix `[moistureBand = min(moisture/5, 4)][tempBand = clamp(20 − T, 0, 25)]`
  → 13 biomes.
- Each biome carries `habitability` `[0,4,10,22,30,50,100,80,90,12,4,0,12]` and movement `cost`
  `[10,200,150,60,50,70,70,80,90,200,1000,5000,150]`. These two numbers drive everything downstream.

## Society / political layer

### The one algorithm
Cultures, states, religions, heresies, provinces and markets all run a **multi-source Dijkstra
flood**:
- All centres go into one `FlatQueue` (mourner/flatqueue: parallel id/priority arrays) at priority 0.
- Pop a cell and relax its neighbours. A neighbour is claimed when `total < cost[n]`, and the owner
  is written immediately. There is no closed set; stale entries are re-expanded.
- Expansion stops at `total > maxCost`.

The result is weighted-Voronoi territories shaped by terrain, with unclaimed remainder.

### Population — `rankCells` (`src/model/society/population-generator.ts`)
```
score = habitability + 250·normalize(flux+conf, medianFlux, maxFlux) − (h−50)/5
        + coast bonus (estuary +15, ocean +5, sheltered harbour +20, freshwater lake +30, lava lake −30…)
s     = score/5 (+ bonus goods nearby)
pop   = s · area/meanArea          (1 point = 1000 people)
```
`cells.s` is computed once and is *the* ranking for culture centres, capitals, towns and religions.

### Cultures (`cultures-generator.ts`)
- Picked from a named set (world, european, highFantasy…; 128 cultures). Each culture has its own
  `sort(i)` preference (e.g. Norse prefer 5 °C, Elladan prefer 18 °C and high ground).
- Centre: `sorted[biased(0, len/2, 5)]`, with spacing `(W+H)/2/count` shrinking ×0.9 per failure.
- **Type** from the centre cell: Nomadic (low desert/grass) / Highland (h > 50) / Lake / Naval
  (harbour, isle) / River (fl > 100) / Hunting (inland forest/taiga/tundra) / Generic.
  Expansionism multipliers: 0.7…1.5 × a random size-variety factor.
- Flood cost per step = `(biome + biomeChange(20) + height + river + coast) / expansionism`, with
  cap `cells·0.6`. The type tables make the flavour: Naval crosses water at `area·2` versus
  `area·6` for others (Nomadic `area·50`); Highland pays 3000 below h 44; River cultures pay 100 off
  rivers; Nomadic pays ×10 in forests.

### Burgs (settlements) (`src/model/political/burgs-generator.ts`)
- Capitals: the top cells by `s·U(0.5,1)` with a spacing check (relaxed ÷1.2 if not all fit). One
  state per capital.
- Towns: `s·gauss(1,3)` with spacing that halves per pass.
- Ports: grouped per water body. Every landmass touching the water gets at least one; a water body
  that ends up with a single port gets none.
- After routes: population = `s/5 × 1.5 (capital) × connectivity(routes) × gauss`. Type and
  buildings (citadel, walls, temple, shanty) are rolled from population. Groups (city, town,
  village, hamlet, fort, monastery…) come from a first-match rule list.

### States (`states-generator.ts`)
- Expansionism is U(1,5). The flood cost is like the culture one plus culture ±(−9 / +100),
  near-walls on unpopulated land (5000), water 1000 (Naval 300), and mountains 2200. Capped at
  `cells/2`.
- One `normalize` pass flips border cells mostly surrounded by another state.
- **Diplomacy** is dice: per pair a weighted relation by proximity (neighbours lean
  Suspicion/Rival, distant states Neutral/Unknown). Vassals when the area ratio is > 2. Wars come
  from rivals plus a power check, and allies join or defect by power ratios. Campaign dates are
  `gauss`.
- Form tier from area (Duchy → Empire), with culture-specific names (Khanate, Tsardom, Shogunate,
  Caliphate…). Theocracy if the centre religion is state-bound. Republic, Union, Anarchy and
  Free City are rare rolls.

### Routes (`src/model/economy/routes-generator.ts`)
- Which pairs connect: the **Urquhart graph** of burgs per landmass (Delaunay with the longest edge
  of each triangle dropped). This is sparse and looks like real road networks; it is not an MST.
- Order: sea routes between ports, then roads between capitals, then trails between all burgs.
  Later paths pay ×0.5 on existing links and are cut where they join one, so trunks are shared.
- Dijkstra (no A*). Land cost `dist² × (1 + (100−hab)/1000) × (1 + max(h−25,25)/25) × (burg ? 1 : 3)`;
  the ×3 off-burg factor pulls roads through towns. Sea cost rises with distance from the coast
  (×1 … ×8). Navigable rivers count as sea lanes.

### Religions (`src/model/society/religions-generator.ts`)
- Folk: one per culture, copied from the culture map, no flood.
- Organized and Cults: centres at big burgs. Expansion mode comes from the generated name: `state`
  stays in its state, `culture` in its culture, `global` goes anywhere. Flood cost along roads ≈ 1,
  off-road = biome cost, water 500 (50 on a sea route), so religion spreads along the road network.
- Heresies: seeded on the borders of high-expansionism religions, flooding almost only inside the
  parent.

### Provinces
Per state, the top ~20% of burgs become province centres. A bounded flood (mountains 100, hills 30,
land 10, water 100) splits the state; leftover cells become Island / Colony ("New X") / Territory
"wild" provinces.

### Names (`src/model/society/names-generator.ts`, `src/data/name-bases.ts`)
- 43 name bases (lists of real-ish names per language with min/max length and allowed doubled
  letters).
- **First-order Markov chain keyed on the previous letter but emitting syllables** (≤5 chars, split
  at vowel boundaries). Frequency acts as weight. Clean-up rules remove triple letters and
  disallowed doubles.
- State names add per-language suffixes (-ia, -land, -stan-like, " Guo"…).

### Economy, military, markers, zones
- Goods are scattered by per-good `distribution` predicates.
- Markets: greedy spacing plus a territory flood.
- Production: a per-burg worker loop with recipes, demand and trade. This is the only agent-style
  simulation.
- Taxes are computed from the deals.
- Military: alert level from expansion, diplomacy and neighbours. Troops are drafted from
  population with culture/religion/landmass penalties and merged into regiments by quadtree.
- Markers (36 kinds) and zones (invasion, plague, eruption…) are random picks or small random
  BFS blobs.

## Engineering notes
- **Per-stage reseeding** (`Math.random = Alea(seed)` at the start of heightmap, precipitation,
  rivers, routes, provinces, goods, markets). Each stage stays reproducible no matter how much
  randomness earlier stages consumed, and a user can regenerate one layer alone. Our forge should
  key a sub-seed per stage the same way.
- Typed arrays for every per-cell field; a quadtree for spacing tests; a flat binary heap for every
  flood.
- Editing a heightmap re-runs the downstream pipeline (`ErasePipeline`). `MapRestoration` re-finds
  every entity *by position, not index*, after the graph is rebuilt.
- Known quirks: Dijkstra early-exits one step before the goal (near-optimal paths); `plaza` is set
  after burg groups are assigned, so caravanserai/trading_post never match on a fresh map; one
  name-chain break condition compares a boolean with a string.

## What this means for us

Our forge (`crates/worldgen/examples/forge_proto/`) is already far ahead on the physical side:
stream-power erosion with geology, Priority-Flood drainage, spill-level lakes, orographic rain on a
raster. FMG's heightmap and hydrology are **not worth porting**. Its blob-stamp templates are only
interesting as an *art-directable* macro layer (e.g. "put a volcano here") before erosion.

What *is* worth taking for a DF-style world map, in rough priority order:
1. **Cell suitability → population** (`rankCells`): one scalar per region/tile from biome
   habitability, river flux, elevation and coast type. It is cheap and drives every placement after
   it. Our `WorldMap` per-region summaries are the natural cells; or use a coarser graph over the
   forge raster.
2. **Cost-capped multi-source Dijkstra for every territory** (civilisations, kingdoms, religions,
   creature ranges), with a per-type cost table. One generic function covers six systems. It is
   deterministic and fast on a few thousand cells.
3. **Urquhart graph + shared-trunk Dijkstra for roads.** Plausible networks in a few lines;
   better-looking than an MST and cheaper than a gravity model.
4. **Syllable Markov names per name base.** Small and good enough. DF uses word lists per language
   instead; either fits.
5. **Per-stage sub-seeds** (above).

What FMG does **not** give us is DF's defining layer: a **history simulation** (civilisations
founding sites, wars changing borders, figures, artifacts over centuries). FMG places everything
in one static pass and rolls diplomacy afterwards. If we want DF-like legends, FMG's floods are a
good *initial* placement (year 0) for a history sim to evolve, not a replacement for one. See
`docs/superpowers/specs/2026-10-20-dwarf-fortress-research/` and
`2026-09-20-df-worldgen-research/` for DF's side.
