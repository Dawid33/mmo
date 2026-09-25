# Dwarf Fortress geology — layer model, inclusions, special structures, and what to copy

Research report, 2026-09-20. Part 3 of the DF worldgen research set (siblings cover the overall
pipeline, hydrology, and adapting simulation-style worldgen to a chunked generator). Scope here is
geology only: how DF decides what rock is where, why it reads as real, where it is fake, and the
smallest rule set that would give our voxel generator (`crates/worldgen`, 1 m blocks, y = −64…320,
sea level 64, peaks ~260, materials today = grass/dirt/sand/stone by depth below surface) lifelike
strata on cliffs and when digging — as a pure function of `(x, y, z, seed)` plus the existing 2D
fields.

Evidence grades used throughout:

- **[P]** primary: DF raw files, DFHack `df-structures` (reverse-engineered struct layouts carrying
  Bay 12's original symbol names), DFHack plugin source that reproduces the game's layer placement,
  Tarn Adams' own words.
- **[W]** DF wiki (community-maintained; mostly accurate, occasionally stale or version-mixed).
- **[O]** player observation / forum or talk-page claim; unverified.
- **[I]** my inference from the above.

---

## 0. TL;DR

1. DF geology is a **table lookup, not a simulation**. Each world tile (one cell of the 2D world
   map, = 16×16 embark tiles = 768×768 game tiles) carries a `geo_index` into a world-global list of
   **geo biomes**. A geo biome is an ordered stack of ≤16 **layers**, each = one layer stone + a
   list of inclusions (veins/clusters) + a top/bottom depth *relative to the surface*. [P]
2. The stack obeys a **textbook ordering**: soil → sedimentary *or* igneous extrusive →
   metamorphic → igneous intrusive at the bottom, above the magma sea. Which stack a tile gets
   depends on four surface classes (normal, sand desert, shallow ocean, deep ocean) × a volcanism
   flag — eight `geo_biome_type`s. [P]
3. Strata are **perfectly conformal to the surface**: no dip, no folds, no faults, no intrusions
   cutting layers. The stack hangs from each column's surface elevation; soil thins with elevation
   (`max_soil = max((154 − elev)/5, 1)`, zero on mountains). [P]
4. What makes it *feel* real is not the geometry but the **mineral–host consistency**: every ore,
   gem and accessory mineral is tied to a host rock class or a specific host stone by
   `[ENVIRONMENT]` / `[ENVIRONMENT_SPEC]` tokens that follow real mineral associations (coal and
   iron in sedimentary rock, kimberlite → diamonds in gabbro, marble hosting the only
   non-sedimentary flux, cassiterite in granite…), nested up to three deep. [P]
5. For a game where cliffs are seen from afar, DF's draped layers are the wrong thing to copy:
   on a slope every column restarts at the top of the stack, so a mountainside shows one material.
   The cheap fix — used by structural-geology tools (Noddy) and partly by TerraFirmaCraft — is a
   **stratigraphic coordinate**: `s = y − B(x, z)` with `B` built from tilt, folds, uplift and fault
   offsets, looked up in a DF-style per-province layer table. The height field then *is* the erosion
   surface and cuts visible bands for free. Details in §6.

---

## 1. The layer model

### 1.1 Data structures (DFHack `df-structures`, Bay 12 names in brackets) [P]

```
region_map_entry [region_squarest]        one per world tile
  elevation   0-99 ocean, 100-149 everything else, 150+ mountains
  rainfall, vegetation, temperature, evilness, savagery   0-100
  drainage    [field_GEOLOGY]   0-100     <- Bay 12 calls drainage "GEOLOGY"
  volcanism   [field_VOLCANISM] 0-100
  salinity, ...
  region_id   -> world_region (the named surface region / biome patch)
  geo_index   [geological_index] -> world_geo_biome

world_geo_biome [geological_regionst]     world_data.geo_biomes[]
  type   geo_biome_type [GeologicalRegionType]:
           NORMAL_STANDARD, NORMAL_VOLCANIC,
           OCEAN_DEEP_STANDARD, OCEAN_DEEP_VOLCANIC,
           OCEAN_SHALLOW_STANDARD, OCEAN_SHALLOW_VOLCANIC,
           DESERT_SAND_STANDARD, DESERT_SAND_VOLCANIC
  layers[] -> world_geo_layer

world_geo_layer [geological_layerst]
  type   geo_layer_type [GeologicalLayerType]:
           SOIL, SEDIMENTARY, METAMORPHIC, IGNEOUS_EXTRUSIVE, IGNEOUS_INTRUSIVE,
           SOIL_OCEAN, SOIL_SAND, SEDIMENTARY_OCEAN_SHALLOW, SEDIMENTARY_OCEAN_DEEP
  mat_index      [basic_stone]            the layer stone
  vein_mat[]     [included_stone]         inclusion materials
  vein_nested_in[] [included_stone_parent] index of the inclusion this one sits inside, or -1
  vein_type[]    [included_stone_inclusion] VEIN | CLUSTER | CLUSTER_SMALL | CLUSTER_ONE
  vein_freq[]    [included_stone_freq]
  top_height     [min_rel_z]   negative, relative to surface
  bottom_height  [max_rel_z]
```

Things this tells us directly:

- **Geology is its own 2D partition of the world**, separate from surface regions: a world tile has
  both a `region_id` and a `geo_index`. The wiki's phrasing matches: "a biome will contain only one
  set of stone layers, though these usually expand beyond a single biome" [W]. Geo biomes are shared
  by many tiles (the list is far shorter than the tile count). The worldgen scratch struct has
  `geo_layers[100]` "placement_layer" and a running `geoindex`/`max_geo_index` [P], i.e. stacks are
  built one geo biome at a time during the "geological layers" worldgen step.
- The inclusion list is **resolved per layer at worldgen**, not per embark: a given layer of a given
  geo biome has a fixed set of possible minerals (this is why DFHack `prospect` can predict an
  embark's minerals before it is generated). Mineral scarcity prunes that list. [P]
- Layer extents are **surface-relative z ranges** (`top_height` is negative). There is no absolute
  elevation datum for strata.
- Bay 12's internal name for the drainage field is `GEOLOGY`, and the worldgen parameters for the
  drainage bands are named `geo_low_min / geo_mid_min / geo_hi_min` [P]. Drainage is therefore DF's
  stand-in for "how permeable/rocky the ground is" — it splits swamp vs forest vs hills/badlands at
  equal rainfall [W], and its value is reused as the pseudo-random source for aquifer weight (§3.1).

### 1.2 Ordering rules [W, consistent with P]

Top to bottom:

| Position | Layer class | Rule |
|---|---|---|
| top | **Soil** (0 to 10+ z-levels) | absent on mountains; sand variants in sand deserts; ocean oozes under deep ocean |
| first stone | **Sedimentary** *or* **igneous extrusive** | "80 % chance of being the first stone layer after soil" is sedimentary; sedimentary and extrusive are mutually exclusive in one stack |
| middle | **Metamorphic** | never above sedimentary, never the bottom layer |
| bottom | **Igneous intrusive** | "the deepest and often the thickest layer will always be igneous intrusive, just above the magma sea"; never top-most, never above sedimentary |
| anywhere but bottom | igneous extrusive | only in volcanic geo biomes |

This is the classic crustal column from an introductory geology text: regolith, sedimentary cover,
metamorphosed older cover, crystalline (plutonic) basement, melt. The wiki's explanation of *why*
follows the rock cycle the same way (deposition → burial/heat/pressure → melting → intrusive cooling
at depth or extrusive at the surface) [W].

Layer stones by class (from `inorganic_stone_layer.txt`) [P]:

- `[SEDIMENTARY]` (11): sandstone\*, siltstone, mudstone, shale, claystone, rock salt, limestone,
  conglomerate\*, dolomite, chert, chalk. (\* = `[AQUIFER]`.)
  `[SEDIMENTARY_OCEAN_SHALLOW]` permits sandstone, shale, conglomerate under shallow ocean;
  `[SEDIMENTARY_OCEAN_DEEP]` permits only limestone under deep ocean — a real facies rule
  (clastics near shore, carbonates offshore).
- `[IGNEOUS_INTRUSIVE]` (3): granite, diorite, gabbro.
- `[IGNEOUS_EXTRUSIVE]` (5): rhyolite, basalt, andesite, dacite, obsidian (obsidian also `[LAVA]`,
  the stone made where magma meets water and the lining of volcano pipes).
- `[METAMORPHIC]` (6): quartzite, slate, phyllite, schist, gneiss, marble.
- `[SOIL]` (19): clay, silty clay, sandy clay, clay loam, sandy clay loam\*, silty clay loam\*,
  loam\*, sandy loam\*, silt loam\*, loamy sand\*, silt\*, peat\*, fire clay, and five sands\*
  (tan/yellow/white/black/red, additionally `[SOIL_SAND]`). This is the USDA soil texture triangle,
  enumerated.
- `[SOIL_OCEAN]` (3): pelagic clay\*, calcareous ooze\*, siliceous ooze\* — real deep-sea sediments.

### 1.3 Count and thickness

- Tools cap the stack at **16 layers** (`nlayers = min(16, layers.size())` in both `prospector` and
  `embark-assistant`), and the last layer is stretched down to the bottom of the map ("stretch layer
  if needed") [P].
- Thickness is whatever `top_height`/`bottom_height` say; the generator's distribution is not
  documented. Observed scale: individual layers from a few z-levels to tens — the wiki's example is
  "fifteen layers may be mostly quartzite, but the next thirty may be dominated by gabbro" [W].
  Mountain embarks typically show "one lighter rock type, generally sedimentary… then two further
  layers of heavy intrusive igneous or metamorphic rock" [W]. With default depth parameters the
  surface-to-magma distance is roughly 50–150 z-levels (REGION templates ~50–60 to the underworld,
  ISLAND templates "hundreds") [W], so a stack is typically **3–8 stone layers of 5–40 levels each**
  [I].
- Transitions are **sharp**: one z-level is quartzite, the next is gabbro. No interbedding, no
  gradation. [W]

### 1.4 How a stack is hung on a column (the placement algorithm) [P]

`plugins/prospector.cpp` and `plugins/embark-assistant/survey.cpp` replicate the game's placement
closely enough to predict embarks, so this is the best available description of the real algorithm:

```
per embark tile (48×48 columns; "mid-level tile"):
  biome tile   = world tile + offset chosen by details->biome[x][y] ∈ 1..9   (3×3 neighbourhood)
  geo_biome    = geo_biomes[ region_map[biome tile].geo_index ]
  elevation    = details->elevation[x][y]
  max_soil     = max((154 − elevation) / 5, 1)
                 0 if the biome tile is mountain (region elevation ≥ 150)
                 0 if shallow-ocean geo biome and the tile's beach soil_freq < 500
  soil_size    = total thickness of SOIL / SOIL_SAND layers in the stack
  soil_erosion = soil_size − min(soil_size, max_soil)
  // remove soil_erosion levels from the top of the stack, shift everything below up,
  // then hang the stack from (elevation − 1); the last layer stretches to the map bottom
```

Consequences:

- **Soil depth is a function of elevation only** (given the stack): elevation 100 (coast) allows 10
  levels, 149 allows 1, mountains 0. The embark screen's "Little / Some / Deep / Very deep /
  Extremely deep" soil is this number. The wiki's "soil accumulates near the edges of mountains;
  partial soil layers down the side for quite a few z-levels" [W] is this rule seen in section.
- There is a **replicated bug**: when a soil layer is only partly eroded, the removed levels are
  handed to the layer below instead of shifting the stack (comment in both plugins) — evidence that
  the plugins were fitted against real maps, not guessed.
- Rock layers keep their thickness regardless of elevation; only the deepest layer absorbs the
  difference. So a mountain and the plain beside it (same geo biome) have the same upper stack; the
  mountain simply has a far thicker basal intrusive layer.

### 1.5 How stacks are assigned to world tiles

Known:

- The **class** of stack follows the surface: ocean deep / ocean shallow / sand desert / everything
  else, each in a standard and a volcanic flavour (`geo_biome_type`) [P].
- **Volcanism** (one of the seeded-then-fractal-filled base fields, 0–100) "controls the occurrence
  of igneous layers, and the formation of volcanoes" [W]. Player testing (40d-era talk page):
  "igneous extrusive layers are only found in areas with a volcanism higher than 90"; "high minimum
  volcanism and you won't get sedimentary or metamorphic layers, just lots of igneous rock" [O].
  Volcanoes need a tile with volcanism exactly 100 (`VOLCANO_MIN` placements; worldgen rejects the
  world if there are not enough such tiles) [W].
- Order in the worldgen pipeline, per Tarn Adams: base fields (elevation, rainfall, temperature,
  drainage, volcanism, wildness) seeded on a grid and fractally filled → … → volcano placement
  "respecting volcanism hotspots" → erosion and rivers → … → "biome identification, **geological
  layers**, and verification". He adds that "the geological stuff should really be earlier", and
  that extreme elevation differences would ideally be smoothed "using mineral types… but we don't
  yet" [P]. So in DF **geology is painted on after the terrain exists and never feeds back into
  terrain shape** — rock hardness does not influence erosion, and mountains are not made of
  anything in particular.

Unknown (not documented anywhere I could find): how geo biome *patches* are drawn (size, shape,
whether they follow region boundaries), and the exact random process that picks each layer's stone
and thickness. The wiki's "80 %" figure is the only quantitative statement. [O]

### 1.6 Horizontal variation and what a boundary looks like at embark scale

- Inside one geo biome the stack is **identical everywhere** (same stones, same thicknesses),
  merely translated vertically with the surface. "Three-dimensional variance is planned but not yet
  implemented" [W].
- Each embark tile picks its biome (and hence geo biome) from the **3×3 neighbourhood of world
  tiles** via a per-embark-tile selector (`details->biome[x][y]` ∈ 1..9); the wiki describes this as
  "noise is calculated and applied which causes the irregular region boundaries on the local map"
  [P, W]. The boundary between two geo biomes is therefore a jagged **vertical wall**: on one side
  15 levels of limestone over gneiss, on the other basalt over granite, with no transition. "Sometimes
  a clear dividing line can be drawn between two biomes from the stone layers underneath, sometimes
  not" (not, when both tiles share a `geo_index`) [W].
- Aquifers, soil types, sand/clay availability all flip across the same wall, because they are
  properties of the stack (§3.1).

---

## 2. Inclusions: veins, clusters, and the environment tokens

### 2.1 Token grammar [P, W]

```
[ENVIRONMENT:<class>:<inclusion type>:<frequency 1-100>]
[ENVIRONMENT_SPEC:<specific stone ID>:<inclusion type>:<frequency>]
class ∈ { SOIL, SOIL_OCEAN, SOIL_SAND, METAMORPHIC, SEDIMENTARY, IGNEOUS_INTRUSIVE,
          IGNEOUS_EXTRUSIVE, IGNEOUS_ALL, ALL_STONE, ALLUVIAL }
inclusion type ∈ { VEIN, CLUSTER, CLUSTER_SMALL, CLUSTER_ONE }
```

`ENVIRONMENT` ties a mineral to a whole layer class; `ENVIRONMENT_SPEC` to one host stone — which
may itself be an inclusion, giving **nesting** (the `vein_nested_in` array). Frequency is a relative
weight when the layer's inclusion list is rolled. `ALLUVIAL` exists in the enum and the raws (native
gold, cassiterite, platinum, all the jades as `ALLUVIAL:CLUSTER_SMALL`) but there is no alluvial
`geo_layer_type`, so those entries appear to be dormant [I].

### 2.2 Shapes and sizes [W, P]

| Type | Shape | Size | Count |
|---|---|---|---|
| `CLUSTER` | flat oval, ~20×40 tiles, "nearly half of a 48×48 block" | ~750 tiles | 1 per 48×48 block per z-level |
| `VEIN` | sinuous line 1–4 tiles wide, may double back | ~100 tiles avg, some > 200 | 2–4 per block per z-level (prospector's estimate) |
| `CLUSTER_SMALL` | 1–9 tiles inside a 3×3 | 4–5 avg | pool of ~10 per block per z-level, shared between layer-hosted and cluster-hosted |
| `CLUSTER_ONE` | single tile | 1 | only inside a small cluster in vanilla (star ruby in rubies, coloured diamonds in faint-yellow diamonds) |

All inclusions are **confined to a single z-level** — flat lenses and ribbons lying in the bedding
plane. Different inclusions overlap freely (one wins per tile); caverns, rivers and erosion truncate
them. The unit of placement is the 48×48 block per z-level. Mineral scarcity (100 "everywhere" …
2 500 default … 50 000 "very rare") prunes *which* minerals survive in a layer's list; per DFHack's
authors it does not change the number or size of veins that do occur [P comment].

### 2.3 The host table (vanilla raws, all frequency 100 unless noted) [P]

Ores and fuels:

| Mineral | Host → type |
|---|---|
| hematite | sedimentary, ign. extrusive → vein |
| limonite | sedimentary → vein |
| magnetite | sedimentary → **cluster** (the one valuable large cluster) |
| bituminous coal, lignite | sedimentary → vein |
| native copper | ign. extrusive, sandstone → vein |
| malachite | limestone, marble → vein |
| tetrahedrite | all stone → vein |
| galena | ign. extrusive, metamorphic, granite, limestone → vein |
| sphalerite | metamorphic → vein |
| cassiterite | granite → vein (+ dormant alluvial) |
| native gold | all igneous → vein (+ dormant alluvial) |
| native silver | granite, gneiss → vein; **horn silver** → small cluster *inside native silver* |
| garnierite (nickel) | gabbro → vein |
| cobaltite | all igneous, metamorphic → vein |
| cinnabar | ign. extrusive, shale, quartzite → vein |
| native platinum | olivine, magnetite → vein; chromite → small cluster |
| bismuthinite, pitchblende, cryolite | granite → small cluster |
| bauxite | sedimentary → cluster; **ruby, sapphire** → small cluster inside bauxite; star ruby/sapphire → single inside those |
| kimberlite | gabbro → vein; **diamonds** → small cluster inside kimberlite; coloured diamonds → single (freq 1) inside faint-yellow diamond |

Rock-forming / accessory minerals (mostly large clusters — these are what make a dug-out level look
mottled instead of uniform): microcline in all stone; orthoclase in intrusive + metamorphic; mica in
metamorphic + granite; olivine in gabbro (→ chromite veins, serpentine small clusters, platinum);
gypsum in sedimentary (→ alabaster, selenite, satinspar, anhydrite, borax, brimstone nested);
kaolinite in sedimentary (→ alunite, marcasite, turquoise); talc in dolomite; puddingstone in
conglomerate; sylvite and borax in rock salt; jet in sedimentary; alunite in ign. extrusive;
calcite in limestone/marble; graphite in gneiss/quartzite/marble/schist; hornblende and pyrolusite in
igneous; realgar, orpiment, stibnite, brimstone, native aluminium in ign. extrusive (fumarole
minerals); saltpeter and petrified wood in sedimentary.

Gems (all `CLUSTER_SMALL`): the silica family (agates at freq 20, jaspers, chalcedony, onyx,
carnelian, tiger iron…) in sedimentary; garnets, spinels, tourmalines, zircons in metamorphic and
granite/diorite/gabbro; the beryls (emerald, aquamarine, heliodor, morganite, red beryl at freq 1)
in granite, schist and marble — i.e. pegmatites and their metamorphic hosts; opals in all stone at
freq 8–10; quartz varieties in all stone at 100; peridot and tanzanite in gabbro; sunstone in
basalt/gneiss; lapis in intrusive + marble; grossulars/melanite/topazolite in marble (skarn garnets).

Reading the table as a designer, the rules of thumb DF encodes:

- **Sedimentary = the economy layer**: iron (three ores), coal, flux (limestone/dolomite/chalk are
  the layer stones themselves), clay/sand above it, but poor gems.
- **Igneous extrusive = volcanic metals and sulphur minerals**, rarely flux.
- **Metamorphic = base-metal veins and mid-value gems**; marble is the one non-sedimentary flux.
- **Igneous intrusive = the treasure layer** (gold, tin, silver, rare-element minerals, beryl gems,
  diamonds via gabbro → kimberlite), and it is always deepest, so value increases with depth without
  any explicit depth rule.
- Nesting gives **prospecting logic**: find gabbro → look for olivine → chromite/platinum; find
  bauxite → rubies; find native silver → horn silver.

### 2.4 Tarn Adams on why

I did not find a quote where Adams explains the geology model in its own right; the recoverable
statements are the pipeline description above (geology is a late worldgen step he considers
misplaced) and the wish to use mineral types in erosion. The wiki's standing advice — "to understand
the terms used here, you may want to crack open a geology textbook (a high school one should
suffice)" [W] — is an accurate description of the level of the model: real classification and real
mineral associations, no structural geology. The chapter(s) he contributed to *Procedural Generation
in Game Design* (Short & Adams, 2017) were not accessible online; treat anything attributed to them
as unverified.

---

## 3. Special structures

DF's underground is the stone stack **plus a separate set of world-scale "feature layers"**
(`world_underground_region` [feature_layerst]: `layer_depth` 0–2 = caverns, 3 = magma sea,
4 = underworld; per-world-tile `region_min_z`/`region_max_z`; water %, openness and passage-density
ranges) and point features attached to embark tiles (`feature_type`: `outdoor_river`, `cave`, `pit`,
`magma_pool`, `volcano`, `deep_special_tube`, `deep_surface_portal`, plus the three `*_from_layer`
types) [P]. Features are carved out of the stack; they do not change it.

### 3.1 Aquifers [P, W]

- A property of the **material**: `[AQUIFER]` on sandstone, conglomerate, puddingstone, and most
  non-clay soils (all sands, loams, silts, peat, the ocean oozes). Pure clays, fire clay, mudstone,
  shale, siltstone: no. [P raws] (The wiki also lists kaolinite; the mirror of the raws I read does
  not tag it.)
- Presence rule as replicated by `embark-assistant`: an embark tile has an aquifer iff some layer in
  its (soil-eroded) stack has an aquifer-capable material **and** that layer's bottom is ≥ 3 levels
  below the surface (`bottom_z <= elevation − 3`). [P]
- **Heavy vs light**: `region_map_entry.drainage % 20 == 7` → heavy, otherwise light — a
  deterministic 1-in-20 hash of the drainage field, matching the wiki's "light aquifers are ~19 out
  of every 20" [P, W].
- The wiki's "low elevations near rivers and oceans are more prone, locations near mountains much
  less" [W] falls out of §1.4: thick soil survives only at low elevation, and most aquifer materials
  are soils [I].
- Vertical extent = the full thickness of the bearing layer; horizontal extent = the geo biome
  patch, so an embark straddling two geo biomes can be half aquifer, and "biomes of neighbouring
  tiles can spill over… resulting in different, unannounced aquifers" [W].

### 3.2 Caverns [W, P]

- Default **three cavern layers**, stacked, below them the magma sea. Depth is set by worldgen
  parameters (defaults): `LEVELS_ABOVE_GROUND:15`, `LEVELS_ABOVE_LAYER_1:5` (minimum stone above
  cavern 1), `LEVELS_ABOVE_LAYER_2:1`, `_3:1` (between caverns), `_4:1` (cavern 3 → magma),
  `_5:2`, `LEVELS_AT_BOTTOM:1`. These are minima; in practice "the top of the first cavern usually
  resides about 10–11 z-levels below the surface".
- Each layer is a **world-spanning 2D region** with per-world-tile z ranges, so caverns are
  continuous across embarks and creatures migrate through open map edges. Per layer: openness
  (0–100, passage size), passage density (0–100), water (0–100) drawn between the `_MIN`/`_MAX`
  parameters. Low openness + high density = a maze of narrow tunnels; high openness = huge halls with
  pillars.
- Inside a layer: floors at several z-levels joined by ramps, pillars, lakes; **downward passages**
  (twisting ramp tunnels) and **deep pits** (fixed-shape vertical shafts, bare rock floor) link
  cavern layers; `cave` features link the surface to cavern 1 (placed at worldgen:
  `MOUNTAIN_CAVE_MIN`, `NON_MOUNTAIN_CAVE_MIN`, `CAVE_MIN_SIZE:5`…`CAVE_MAX_SIZE:25`).
- Caverns **ignore lithology**: they cut whatever stone is there; walls show the local layer stone
  and any veins/clusters they intersect (which is how players prospect). No karst preference for
  limestone. [I from the data model]

### 3.3 Magma sea, magma pools, volcanoes [W, P]

- **Magma sea**: feature layer depth 3, "always below the last cavern level", typically around
  z −120 on default worlds (wiki range −5 … −450). Its floor and surround are impassable
  **semi-molten rock**; below that the underworld, floored/lined with `[DEEP_SURFACE]` slade.
- **Magma pools** (`magma_pool`): vertical tubes rising from the magma sea part-way up, walled with
  obsidian, self-refilling. `start_depth` is expressed as the cavern layer index the pool reaches
  (`magma_level = 2 − start_depth` in embark-assistant).
- **Volcano** (`volcano`): "very large, very hollow pillar of obsidian with magma inside", a
  **one-embark-tile feature with the same cross-section from the surface to the map bottom**, i.e. a
  pipe, not a cone (any cone is whatever the elevation field happens to do). Placed at worldgen on
  tiles with volcanism 100, "tend to appear in mountainous igneous extrusive biomes"; mountain peaks
  carry an `is_volcano` flag. Refills when drained.
- There is no lava-flow stratigraphy: extrusive layers are just layer stones selected by the
  volcanism field, unrelated in geometry to any particular volcano. [I]

### 3.4 Adamantine spires [W, P]

`deep_special_tube` features made of the `[DEEP_SPECIAL]` stone (raw adamantine): near-vertical
pillars rising out of the magma sea, 5 to 30–40+ z-levels tall, some reaching cavern 3; about one
per 2×2 embark tiles, randomly placed (a 3×3 embark guarantees one). Cross-section 4×4 solid at the
top; lower down a hollow 2×2 core inside the 4×4 shell leading to the underworld. `start_depth`
records which cavern layer the tip reaches.

### 3.5 Summary column (default parameters, non-mountain, non-ocean)

```
 +15   sky (LEVELS_ABOVE_GROUND)
  0    surface ── soil: 1..10 levels by elevation; 0 on mountains
       stone layer 1  sedimentary (≈80 %) | igneous extrusive (volcanic geo biomes)
       stone layer 2… metamorphic / more of class 1          ← aquifer = any tagged layer ≥3 deep
 ~−10  cavern 1      (world-scale 2D layer; openness/density/water per layer)
       cavern 2
       cavern 3      ← adamantine tips, magma pool tops
       igneous intrusive (always last, stretched to fit)
 ~−120 magma sea     ← volcano pipe runs from here to the surface, obsidian-lined
       semi-molten rock
       underworld (slade)
```

---

## 4. Surface expression

What a DF player actually sees of the geology above ground:

- **Soil everywhere except mountains.** Lowland surfaces are soil (sand in sand deserts and on
  some beaches, peat/loams/clays elsewhere per the geo biome's soil layers). Soil thickness falls
  linearly with elevation and hits zero at the mountain threshold, so foothills show a wedge of soil
  pinching out upslope, and mountain tiles are bare layer stone with no trees ("mountains have a lot
  of ore, but no soil") [P, W].
- **Cliffs and canyon walls show a true vertical section of the *upper* column**: because each
  column's stack hangs from its own top, a cliff face exposes soil, then layer 1, then layer 2 as
  horizontal bands, with veins (ribbons) and large clusters (broad lenses) visible in the face.
  River gorges and the brook/river channel banks do the same over a few levels. This is the only
  place DF's geology is visible as strata.
- **Slopes show nothing**: on ramped terrain every column's top is the top of the stack, so a whole
  mountainside is one stone (layer 1) regardless of height — there are no outcropping bands, no
  hogbacks, no mesas capped by a hard layer. [I from §1.4]
- **Beaches**: each ocean-adjacent mid-level region tile has an `ocean_beach_comp` record with
  frequencies for *soil, pebbles, solid rock, boulders, driftwood* [P]; shallow-ocean geo biomes
  with `soil_freq < 500` get zero soil (rocky shore). So beaches vary sand ↔ pebble ↔ bare rock by
  region tile.
- **Material-driven surface detail**: boulders and pebbles on the surface are made of the local
  layer stone; sand/clay availability (glass, ceramics) is a property of the soil layers; obsidian
  rims volcanoes and anywhere magma met water.
- **Geo biome seams** are visible on bare mountain surfaces as an abrupt, noisy-edged colour change.
- Worldgen erosion (`EROSION_CYCLE_COUNT`, `PERIODICALLY_ERODE_EXTREMES`) runs **before** layers are
  assigned, so canyons expose nothing special — the layers are draped over the already-eroded
  surface afterwards [P: Adams' step order].

---

## 5. Fidelity to real geology, and how cheap generators go further

### 5.1 What DF gets right

- The four-fold rock classification with correct member stones; USDA soil texture classes; real
  pelagic sediments; obsidian where lava meets water.
- The gross crustal column (regolith → sedimentary cover → metamorphic → plutonic basement → melt).
- Facies hints: clastics under shallow seas, limestone under deep ones; sand in deserts.
- Volcanic provinces as a separate regime driven by a volcanism field; fumarole mineral suite
  (sulphur, realgar, orpiment, stibnite, cinnabar) in extrusive rock.
- Mineral associations, including multi-level parageneses: ultramafic chain (gabbro → olivine →
  chromite/serpentine → platinum), kimberlite → diamond, evaporite chain (gypsum → alabaster/
  selenite/anhydrite; rock salt → sylvite/borax), laterite chain (bauxite → corundum gems),
  pegmatite gems in granite, skarn garnets in marble, supergene horn silver on native silver,
  graphite only in metamorphic hosts, coal only in sedimentary ones.
- Aquifers as a property of porous materials (sands, loams, sandstone, conglomerate) and not of
  clays/shales — hydrogeologically correct.

### 5.2 Where it simplifies

| Real geology | DF |
|---|---|
| Strata are deposited horizontal, then tilted, folded, faulted | Always parallel to the *present surface*; no dip, fold, or fault anywhere |
| Erosion cuts across structure → outcrop bands, cuestas, mesas, hogbacks | Layers draped after erosion; slopes never expose lower layers |
| Rock hardness controls relief (resistant caps, strike valleys) | No feedback; Adams notes this as a to-do |
| Lateral facies change, pinch-outs, interbedding | One stone per layer, uniform across the whole geo biome; walls between geo biomes |
| Intrusions cut across layers (plutons, dykes, sills) with contact aureoles | "Intrusive" is just the bottom layer class; nothing cross-cuts |
| Metamorphic grade tied to orogens and intrusions | Metamorphic is a depth slot |
| Lava flows, ash beds, cones around vents | Extrusive layer selected by a 2D field; volcano is a uniform pipe |
| Veins are steep, fracture-controlled sheets; coal is a bed | Everything is a flat 1-level lens/ribbon; coal is a "vein" |
| Unconformities | None |
| Karst in limestone | Caverns ignore lithology |
| Placers in river gravels | `ALLUVIAL` defined but unused |

Net: DF is a **1D geology** (a column) stamped over a 2D map. It is convincing when you *dig*
(sequence, host-consistent minerals, aquifers, magma at depth) and says almost nothing when you
*look at a landscape*. Our requirement — strata that read from afar on cliffs and mountainsides — is
exactly the part DF does not do.

### 5.3 Techniques that go further and stay cheap

**(a) TerraFirmaCraft — DF's model in a chunked voxel generator, plus skew**
(`RegionChunkDataGenerator.generateRock`, 1.20.x) [P source]. A pure function of position and
surface height:

```
adjSurface = surfaceY ≤ 125 ? surfaceY : 125 + 0.3·(surfaceY − 125)   // damp the drape on mountains
Δy = adjSurface − y
walk layers n = 0,1,2…: h_n(x,z) = simplex2D(3 oct, range 43..63 blocks, freq 0.014,
                                            sampled at (x,z)+per-layer offset); stop when Δy ≤ h_n
skew: (sx, sz) = simplex2D(2 oct, ±1.8, freq 0.01);  x' = x + sx·(Δy + c),  z' = z + sz·(Δy + c)
point = rockLayerArea(x', z')         // 2D cellular "rock region" map carrying seed + type
rock  = settings.sampleAtLayer(point, n)
```

Notable: (1) layer *thickness* is a smooth 2D noise, so boundaries undulate; (2) the **skew** shifts
the horizontal lookup in proportion to depth, so region boundaries and layer contacts are inclined
surfaces rather than vertical walls — dipping contacts for one multiply-add; (3) above y = 125 the
stack follows the surface at only 30 %, so **tall mountains cut down into deeper layers** instead of
being wrapped in layer 0; (4) the stack is a **Markov chain by rock**: the data-driven
`rock_layer_settings` gives each layer a map `rock → next layer id`, entry points per region type
(`ocean_floor`, `volcanic`, `land`, `uplift`), and a `bottom` list that repeats forever — the same
ordering discipline as DF (sedimentary high, metamorphic anywhere, intrusive low, extrusive high)
expressed as transitions instead of slots. Rock regions are "over a kilometre across", with "two or
three rock layers under your feet".

**(b) Vintage Story — bottom-up thickness budget per geologic province** [W, mod docs]. A
low-frequency *geologic province* noise map selects per-province maximum thicknesses for each rock
group (sedimentary, metamorphic, igneous, volcanic). Igneous always fills from the mantle up;
metamorphic and sedimentary are optional caps; each rock type has its own 2D thickness noise and the
group's budget is split proportionally, strata < 2 blocks culled. Special cases: basalt may cap
anything (flood basalt), halite as large domes, marble as veins in phyllite/slate, obsidian in
basalt. Lateral pinch-outs and interfingering fall out of per-rock thickness maps — something
neither DF nor TFC has.

**(c) Province-driven Minecraft mods** (June's GeOverhaul; "Minecraft Mineralogy"; GeoStrata) [W mod
pages]: split the world into large **provinces** ("ancient pieces of crust with similar rock types"),
put orogens/volcanic arcs on province boundaries, bias rock families by province kind (basalt and
pumice on volcanic arcs, gneiss and schist in mountain belts, limestone and chalk on coastal
shelves, salt and gypsum in arid basins), and host ores only in rocks that can host them (diamonds
only in kimberlite, malachite in limestone, porphyry copper along plate boundaries). Tectonic (the
terrain datapack our height pipeline resembles) does terrain shape only — no strata.

**(d) Noddy / kinematic structural modelling** (Jessell; Noddyverse, ESSD 2022) [P paper]. The
formal version of the trick, from geophysics: a geological history is a list of idealised events —
`STRAT, TILT, FOLD, FAULT, UNCONFORMITY, DYKE, PLUG, SHEAR-ZONE` — and the rock at a point is found
by "taking the current x,y,z position of a point and unravelling the kinematics (using idealised
displacement equations) until we get back to the time when the infinitesimal volume of rock was
formed". Each event is an **inverse coordinate transform** applied youngest-first; the final
coordinate indexes a flat stratigraphic column. No state, no simulation, embarrassingly parallel —
precisely a pure function of `(x, y, z, seed)`. One million models were generated this way from 343
event orderings. Related: SubsurfaceGen-style fault parameterisation (centre, azimuth, dip,
curvature radius → planar vs listric, slip vector; "each voxel in the post-fault model pull-sampled
from its pre-slip position").

**(e) Cordonnier et al., "Sculpting Mountains" (IEEE TVCG 24(5), 2018)** [P paper]. Couples folded
crustal layers with uplift + fluvial erosion; "the stratigraphy allows the system to render folded
strata on eroded cliffs". Too heavy for us (simulation state), but it confirms the visual target and
the principle: *compute structure independently of the surface, let erosion cut it*.

**(f) Layer-lookup-with-noise for erosion terrains** (Astrolith devlog, gamedev.net; not retrievable
— 403 — cited from the search abstract only): layer index = f(depth + noise offset), hardness per
layer feeding erosion rate → terraces. The hardness → relief coupling is the one thing a pure
height-field generator cannot get exactly, but it can be faked (§6, rule 8).

---

## 6. Recommendations for `crates/worldgen`

Constraints restated: deterministic pure function of position; no global state; must be cheap
enough for `coarse_tile` profile sampling (LOD factors ≥ 16 never generate full regions) and must
agree between full generation and coarse sampling; existing 2D fields per column are continentalness
(`cont`), erosion (`ero`), warped ridge (`ridge` → `pv`/`mountains`), and the smooth surface term
`offset_h` of `ColumnProfile` (surface height before jaggedness).

Design in one line: **DF's tables, Noddy's coordinates.** Rock at a voxel =
`table[province(x,z)].lookup(s)` where `s` is a *stratigraphic coordinate* obtained from `y` by a
short chain of inverse deformations that are 2D per-column quantities (so they fit next to
`ColumnProfile`), plus a depth-below-surface regolith rule on top.

Ordered by visual return per unit of work:

### Rule 1 — Stratigraphic coordinate instead of depth-below-surface (the big one)

```
s(x,y,z) = y − B(x,z)                  B = structural datum, a smooth 2D field, metres
rock     = column[province].at(s)      binary search in ≤ 16 boundaries
```

With `B ≡ 0` strata are flat in world space and the existing height field already carves
layer-cake canyons, mesas and banded cliffs — zero extra noise evaluations. Everything below only
shapes `B`. Keep depth-below-surface **only** for regolith (rule 4). This single change is what
makes geology visible from afar; DF/TFC-style draping cannot do it.

### Rule 2 — Bed-scale banding inside each formation

Formations 16–48 m thick read at distance; cliffs need finer rhythm. Inside a formation pick a
variant from a 1D hash of the bed index:

```
bed     = floor((s + bedjitter(formation)) / bed_thickness(formation))     // 2–6 m
variant = hash(seed, formation_id, bed) → {main, main, main, light, dark, interbed}
```

e.g. sandstone with darker siltstone partings, limestone with chert bands, a 1–2 m coal seam at a
fixed `s` in one formation (coal as a *bed*, not DF's vein). Two or three tints per rock type is
enough. Keep major bed thickness ≥ 3–4 m so LOD factor-2/4 rings still show it; rings ≥ 16
should resolve formations only. Because `s` is shared, beds line up across a valley — the cue that
sells "these are the same strata".

### Rule 3 — Uplift and folds from fields we already have

```
B(x,z) = α(prov)·H̃(x,z)  +  tilt(prov)·(x,z)  +  A(prov)·fold(x,z)
H̃     = smooth surface height (offset_h, no jaggedness) — already computed per column
α      = 0.0–0.3  plateau / basin      → flat strata, mesas and stepped canyons
         ~1.0     (DF/TFC draping)      → avoid: nothing outcrops
         1.3–1.7  orogen                → strata arch over ranges; deeper (older) units are
                                          exposed in range cores, younger on flanks, dipping
                                          away from the crest as hogbacks/flatirons
tilt   = per-province dip 0–4° in a hashed azimuth   (cuestas for free)
fold   = sin(k·(x cosθ + z sinθ) + φ) style trains or one low octave of 2D noise,
         wavelength 150–600 m, amplitude A = lerp(2 m, 60 m, mountains)   // `mountains` gate
```

At the surface `s = H − αH̃ ≈ (1−α)H`: α < 1 exposes younger rock on heights (layer cake), α > 1
exposes older rock on heights (eroded anticline) — one scalar per province flips the regime.
TFC's "0.3 above y = 125" is the same knob. Everything is 2D → goes in the column profile and can be
bilinearly interpolated on the existing 4 m `ProfileGrid` lattice.

### Rule 4 — Regolith by slope and elevation (so rock actually shows)

DF's `max_soil = max((154 − elev)/5, 1)`, zero on mountains, translated:

```
soil_depth = round( base(prov, climate) · (1 − smoothstep(h_lo, h_hi, H)) · (1 − smoothstep(25°, 45°, slope)) )
             // 0–6 m; slope from finite differences of the column height on the profile lattice
top block  : grass | sand (beach band, arid) | bare rock when soil_depth = 0
below top  : dirt / clay (low `ero`-gate wet lowlands, near sea level) / sand / gravel-scree
             (slope just under the cliff threshold, below a steep cell)
```

Bare rock above a slope threshold is the enabling condition for rules 1–3 to be seen at all; today's
unconditional grass/dirt 4-block skin hides everything. Beaches: copy DF's per-tile beach
composition idea with a low-frequency 2D hash choosing sand / pebble / bare rock shore.

### Rule 5 — Provinces: which column goes where

DF's `geo_biome_type` (surface class × volcanic) generalised to a 2D province function of existing
fields plus one new low-frequency noise `volc(x,z)`:

| Province | Selector (sketch) | Column top → bottom | α, fold A |
|---|---|---|---|
| Ocean floor | `cont` deep ocean | ooze/clay 2–4 m → basalt → gabbro | 0, ~0 |
| Shelf / coast | `cont` shallow–coast | sand → limestone/chalk → sandstone → shale → basement | 0–0.2, small |
| Sedimentary basin / plateau | inland, low `mountains`, high erosion-gate flatness | thick cover: sandstone (red/tan beds) → shale → limestone → sandstone → conglomerate → basement | 0–0.3, 2–8 m |
| Craton / shield | inland, low `mountains`, hashed alternative to basin | thin cover (0–20 m) → gneiss/granite with steep foliation (rule 7) | ~0.5, n/a |
| Orogen | `mountains` high | folded metasediments: slate → phyllite → schist → quartzite/marble → gneiss; granite cores (rule 6) | 1.3–1.7, 30–60 m |
| Volcanic | `volc` > threshold (DF: volcanism > 90 of 100) | basalt/andesite flows with 3–10 m flow beds + ash partings, capping whatever column lies below (VS flood-basalt rule); cones at local maxima of `volc` | 0 for the cap |

Columns are generated per province *cell* (jittered-grid or Voronoi id, ~1–3 km) from a Markov table
in the TFC style — entry rock per province, `rock → allowed next rocks`, DF ordering constraints
(sedimentary never below metamorphic; intrusive last; extrusive only in volcanic; ≤ 16 layers, last
one infinite), thickness = hash in a per-rock range. **Blend cells instead of walls**: evaluate
boundaries' `s` values in the two nearest cells and interpolate *thicknesses* (not rock ids) across a
200–400 m margin, and skew the cell lookup with depth as TFC does
(`x' = x + skew·(H − y)`), so province contacts are inclined, interfingering surfaces rather than
DF's vertical seams. Where rock ids differ between neighbours, the pinch-out (thickness → 0) handles
it.

### Rule 6 — Faults as offset discontinuities

Jittered grid, cell 600–1500 m, each cell with probability p(prov) owning one fault: strike azimuth
(biased parallel to the local ridge direction in orogens), dip 60–90°, throw 4–40 m, finite length
with throw tapering to zero at the tips.

```
for each fault in the 3×3 neighbouring cells:
    d = signed distance from (x, z) to the fault trace, shifted by (y − y0)·cot(dip)
    s += throw · taper(along) · (d > 0 ? +0.5 : −0.5)
```

Bands step across a line on a cliff — the single most "this is real rock" detail after the bands
themselves. Cost: ≤ 9 cheap plane tests per column (the `y` term is linear, so per-voxel work is one
multiply-add per active fault, usually 0–2). **Do not bilerp across faults**: smooth parts of `B`
may come from the profile lattice, fault offsets must be evaluated per column. Optional 1–2 block
fault-gouge/breccia material where `|d| < 1`.

### Rule 7 — Basement unconformity

Below an unconformity surface `s < s_u(prov)` switch to a *different* coordinate: basement foliation
`s₂ = (x cosθ + z sinθ)·sin δ + y·cos δ` with steep δ (50–80°) and its own bed hash (gneiss banding,
schist/quartzite alternation). Flat cover over steeply banded basement is an angular unconformity —
one branch and one dot product. Exposed in deep canyons, orogen cores and shields.

### Rule 8 — Hardness-aware relief (fake the feedback DF lacks)

We cannot erode, but we can let lithology nudge the density field: where the surface cuts a
resistant unit (quartzite, limestone, basalt cap, granite) multiply `factor`/add to jaggedness
(steeper, cliffier); in weak units (shale, mudstone, ash) flatten. A bounded, smooth modulation
keyed on `rock(s at H̃)` produces ledge-and-slope canyon profiles and caprock mesas. Second-tier:
needs care with the bit-exact `ColumnProfile` contract and the LOD coarse path, so treat as an
experiment after rules 1–5.

### Rule 9 — Intrusions and contact metamorphism

- **Plutons**: one low-frequency 3D noise (wavelength 200–500 m) thresholded with a depth- and
  province-dependent bias (`n − k·s_depth − p(prov) > 0` → granite/diorite/gabbro). Evaluate on a
  coarse 8 m lattice and trilerp; skip entirely in provinces with p = 0.
- **Aureole**: where the same value is within ε below threshold, map host → contact rock
  (limestone → marble, sandstone → quartzite, shale → slate/hornfels). Free, and it reproduces DF's
  marble/granite gem logic spatially.
- **Dykes**: jittered-grid planes `|n·p − d| < 0.5–1.5 m`, near-vertical, basalt/dolerite, applied
  *after* (= evaluated before) faults they post-date so they run straight through offset strata.
  **Sills**: the same in `s`-space (`|s − s₀| < 1` within a patch).
- **Volcano necks / kimberlite pipes**: vertical cylinders from a sparse jittered grid gated by
  province (DF: one-tile obsidian-lined pipe; kimberlite only in gabbro/craton, diamonds inside).

### Rule 10 — Ores and minerals: DF's host table, placed in stratigraphic space

Copy the `ENVIRONMENT` idea verbatim as data: `mineral → [(host class | host rock | host mineral,
shape, weight)]`, nested ≤ 3 deep, with the associations of §2.3. Place shapes in `(x, z, s)` rather
than `(x, z, y)` so lenses and seams **follow folds and step across faults**:

- seam/bed (coal, ironstone, evaporite): `|s − s₀| < t` within a 2D patch mask;
- lens (DF `CLUSTER`, ~20×40 m, 1–3 m thick): ellipse in `(x,z)` × thin in `s`, one per ~48 m cell
  per bed — DF's density;
- ribbon vein (DF `VEIN`): thresholded ridged 2D noise in `(x,z)` within a bed, 1–4 m wide — or,
  better than DF, steep fracture veins as thin planes like dykes, clustered near faults and pluton
  margins (where hydrothermal veins really are);
- pocket (DF `CLUSTER_SMALL`, 1–9 blocks) and single (`CLUSTER_ONE`): per-cell hash.

Depth-value gradient then emerges as in DF from the column order (economy in sedimentary cover,
treasure in the intrusive basement) with no explicit depth rule.

### Rule 11 — Let lithology drive the other systems

- Aquifers/springs: tag porous rocks (sand, sandstone, conglomerate) exactly as DF does; a spring
  line where a tagged bed outcrops above an impermeable one (shale/clay) is a pure function of the
  column.
- Caves: multiply cave density by host (limestone ×3, sandstone ×1, granite/gneiss ×0.2) for karst —
  beyond DF, nearly free.
- Deep structure: DF's three world-scale cavern layers + magma sea at fixed depth bands under
  everything is a good template for our y < 0 range; hosts there should be the basement column.

### Cost and determinism notes

- Per column (fits beside `ColumnProfile`; smooth parts bilerp-able on the 4 m lattice): province
  cell ids + blend weight, `B` (α·H̃ + tilt + fold: at most one extra 2D noise), ≤ 2 active faults
  (evaluated per column, never interpolated), soil depth, surface slope.
- Per voxel: `s = y − B (+ fault y-terms)`, one binary search over ≤ 16 boundaries, one integer hash
  for the bed variant. 3D noise only for plutons, coarse-latticed, province-gated.
- All table generation from integer hashes of `(seed, cell id, layer index)`; float work confined to
  the fields already in the pipeline → same cross-machine determinism posture as today.
- **LOD agreement**: the coarse path must call the same `rock_at(column, y)`; point-sample at the
  cell centre (deterministic, consistent with any-solid occupancy). Open question for the renderer
  side: `merge_group` keeps the material *visible from above*, which is right for plateaus but will
  flatten banding on cliff **faces** in far rings; if bands matter at distance, LOD cells on steep
  columns may need the side-visible material instead. Flagging, not solving, here.

### Minimal first cut

Rules 1 + 2 + 4 with three hand-written columns (basin, orogen, volcanic) selected by the existing
`mountains` gate and one new `volc` noise, α per province from rule 3 with no folds. That is: one
subtraction, one table lookup, one hash per voxel, and a slope-aware soil rule — and it already
yields banded canyon walls, bare striped mountainsides with older rock in the cores, and soil-mantled
lowlands. Faults (rule 6) are the next cheapest large gain; folds, unconformity, intrusions and ores
follow.

---

## 7. Uncertainties

- **How DF generates geo biome patches and rolls each layer's stone/thickness is undocumented.**
  The 8-type enum, the per-tile `geo_index`, and the placement arithmetic are solid [P]; the
  "80 % sedimentary first", "volcanism > 90 for extrusive" numbers are wiki/talk-page claims, the
  latter from the 40d era (2008–2010) [O].
- Whether the live map generator hangs the stack per column or per 48×48 block is not established;
  the DFHack estimators work per 48×48 embark tile using that tile's elevation. Either way the
  strata are surface-conformal.
- Layer thickness distribution and typical layer count are inferred from anecdotes and default depth
  parameters, not measured.
- The raws were read from a graphics-pack mirror (Phoebus, 0.4x-era); ENVIRONMENT tokens have been
  stable across versions, but v50+ may differ in details (e.g. whether kaolinite is aquifer-tagged).
- Inclusion counts per block (2–4 veins, 1 cluster, ~10 small clusters per z-level) are the DFHack
  authors' fitted guesses, labelled as such in their source.
- No first-hand Tarn Adams statement on the *design intent* of the geology was found; the PGiGD book
  chapters were not accessible. The gamedeveloper.com Q&A gives only pipeline order and the
  "geology should be earlier / use mineral types in erosion" remarks.
- TFC numbers are from the 1.20.x branch source; Vintage Story details are from its wiki and mod
  documentation, not source.
- Section 6 parameters (α ranges, fold amplitudes, fault spacing) are design proposals sized for a
  ~200 m relief band with 1 m voxels, not values taken from any source; they need tuning in
  `examples/world_map.rs`-style previews.

---

## Sources

| # | Source | URL | Contributed | Reliability |
|---|---|---|---|---|
| 1 | DFHack `df-structures`: `df.region.xml` | https://github.com/DFHack/df-structures/blob/master/df.region.xml | `region_map_entry` fields (drainage = `field_GEOLOGY`, volcanism, `geo_index`), `world_geo_biome`, `world_geo_layer` (top/bottom height, vein arrays, nesting), `world_underground_region`, worldgen depth/cavern parameter names | High — reverse-engineered memory layouts with Bay 12 symbol names; semantics of some fields annotated as guesses |
| 2 | DFHack `df-structures`: `df.d_basics.xml` | https://github.com/DFHack/df-structures/blob/master/df.d_basics.xml | `geo_biome_type` (8 types), `geo_layer_type` (9), `inclusion_type`, `environment_type` enums | High |
| 3 | DFHack `df-structures`: `df.feature.xml`, `df.region_midmap.xml` | https://github.com/DFHack/df-structures | `feature_type` list (pit, magma_pool, volcano, deep_special_tube…), `ocean_beach_comp` (soil/pebble/rock/boulder/driftwood) | High |
| 4 | DFHack `plugins/prospector.cpp` | https://github.com/DFHack/dfhack/blob/develop/plugins/prospector.cpp | Layer placement algorithm (soil erosion rule `(154−elev)/5`, stack shift, 16-layer cap, stretch-last-layer, replicated bug), inclusion count/size estimates | High for the algorithm (validated against real embarks); inclusion sizes self-described as guesses |
| 5 | DFHack `plugins/embark-assistant/survey.cpp` | https://github.com/DFHack/dfhack/blob/develop/plugins/embark-assistant/survey.cpp | Per-embark-tile biome offset (3×3), aquifer presence rule (tagged layer ≥ 3 deep), heavy aquifer = `drainage % 20 == 7`, magma pool / adamantine `start_depth` | High |
| 6 | DF raws (`inorganic_stone_layer/_mineral/_gem/_soil.txt`), Phoebus pack mirror | https://github.com/DFgraphics/Phoebus/tree/master/raw/objects | Complete layer-class membership, AQUIFER tags, every ENVIRONMENT / ENVIRONMENT_SPEC line (§2.3) | High (game data), but an older-version mirror |
| 7 | DF wiki: Layer / Stone layers | https://dwarffortresswiki.org/index.php/Layer , https://dwarffortresswiki.org/index.php/Stone_layers | Ordering rules, 80 % claim, sharp transitions, "3D variance planned", biome-boundary behaviour | Medium — community; numbers unverified |
| 8 | DF wiki: Stone; Sedimentary layer; Mountain | https://dwarffortresswiki.org/index.php/Stone , https://dwarffortresswiki.org/index.php/Sedimentary_layer , https://dwarffortresswiki.org/index.php/Mountain | Stone lists, typical mountain stack, soil wedge on mountain edges | Medium |
| 9 | DF wiki: Vein | https://dwarffortresswiki.org/index.php/DF2014:Vein | Inclusion shapes/sizes, single-z-level confinement | Medium (player measurements) |
| 10 | DF wiki: Inorganic material definition token | https://dwarffortresswiki.org/index.php/Inorganic_material_definition_token | Token semantics, environment classes, inclusion types | Medium-high |
| 11 | DF wiki: Soil; Aquifer; Biome | https://dwarffortresswiki.org/index.php/Soil , https://dwarffortresswiki.org/index.php/Aquifer , https://dwarffortresswiki.org/index.php/Biome | Soil classes, aquifer materials and 19/20 light ratio, elevation/drainage biome logic, neighbouring-tile spill-over | Medium |
| 12 | DF wiki: Cavern; Magma sea; Volcano; Raw adamantine | https://dwarffortresswiki.org/index.php/Cavern , https://dwarffortresswiki.org/index.php/Magma_sea , https://dwarffortresswiki.org/index.php/Volcano , https://dwarffortresswiki.org/index.php/Raw_adamantine | Cavern depth/structure, deep pits, magma pools, magma sea depth, volcano pipe shape, spire geometry and density | Medium |
| 13 | DF wiki: Advanced world generation; World token | https://dwarffortresswiki.org/index.php/Advanced_world_generation , https://dwarffortresswiki.org/index.php/World_token | Volcanism/volcano parameters, mineral scarcity values, cavern and LEVELS_* defaults, erosion parameters | Medium-high (mirrors `world_gen.txt`) |
| 14 | DF wiki: 40d Talk:Advanced world generation | https://dwarffortresswiki.org/index.php/40d_Talk:Advanced_world_generation | "Extrusive only where volcanism > 90"; high min volcanism → only igneous | Low-medium — old-version player testing |
| 15 | Game Developer Q&A with Tarn Adams | https://www.gamedeveloper.com/design/q-a-dissecting-the-development-of-i-dwarf-fortress-i-with-creator-tarn-adams | Worldgen step order; geological layers assigned late; "should really be earlier"; mineral types not yet used in erosion | High (first-hand) |
| 16 | TerraFirmaCraft source, `RegionChunkDataGenerator.java` (1.20.x) | https://github.com/TerraFirmaCraft/TerraFirmaCraft/blob/1.20.x/src/main/java/net/dries007/tfc/world/chunkdata/RegionChunkDataGenerator.java | Pure-function rock lookup: noise layer heights 43–63, depth-proportional skew ±1.8, 0.3 surface damping above y 125 | High (source) |
| 17 | TFC docs: world preset / rock layer settings; field guide geology | https://terrafirmacraft.github.io/Documentation/1.20.x/worldgen/world-preset/ , https://terrafirmacraft.github.io/Field-Guide/18/en_us/the_world/geology.html | Markov-chain layer tables, `ocean_floor/volcanic/land/uplift` entry points, `bottom` repeat; region scale | High |
| 18 | Vintage Story wiki: Terrain Generation; Rock-Strata-Variety mod README | https://wiki.vintagestory.at/Terrain_Generation , https://github.com/chisps427/Rock-Strata-Variety | Geologic province map, bottom-up group budgets, per-rock thickness noise, special cases (basalt caps, halite domes) | Medium |
| 19 | June's GeOverhaul (Modrinth); Minecraft Mineralogy / GeoStrata (CurseForge) | https://modrinth.com/project/IUHq9hUx , https://www.curseforge.com/minecraft/mc-mods/minecraft-mineralogy , https://www.curseforge.com/minecraft/mc-mods/geostrata-mineralogy-remade | Province-based rock families tied to tectonic setting; host-restricted ores | Low-medium (mod descriptions, no algorithm detail) |
| 20 | Jessell et al., "Into the Noddyverse", ESSD 14, 381–392 (2022); pynoddy | https://essd.copernicus.org/articles/14/381/2022/ , https://github.com/cgre-aachen/pynoddy | Kinematic inverse-displacement modelling; event list (STRAT, TILT, FOLD, FAULT, UNC, DYKE, PLUG, SHEAR) — the formal basis for §6's stratigraphic coordinate | High (peer-reviewed) |
| 21 | Cordonnier et al., "Sculpting Mountains: Interactive Terrain Modeling Based on Subsurface Geology", IEEE TVCG 24(5), 2018 | https://www.cs.purdue.edu/cgvlab/www/publications/cordonnier2018sculpting/ | Folded strata + uplift + erosion; strata rendered on eroded cliffs | High (peer-reviewed); abstract-level read only |
| 22 | SubsurfaceGen (arXiv 2605.30541), via search abstract | https://arxiv.org/pdf/2605.30541 | Fault parameterisation (azimuth, dip, curvature → listric, slip; pull-sampling from pre-slip position) | Medium — abstract only |
| 23 | Astrolith devlog "Rock Layers for Real-Time Erosion Simulation" | https://www.gamedev.net/blogs/entry/2284060-rock-layers-for-real-time-erosion-simulation/ | Layer index from depth + noise offset; hardness → erosion | Low — page returned 403; search snippet only |

### Access notes and gaps

Retrieval method, stated explicitly because the research brief's source-access rule (do not route
around a refused fetch) arrived after the research was done:

- **Nothing in this report was obtained by getting past a refusal.** No archive/cache, no alternate
  forum endpoint (wap2/mobile/plain-text), no login or paywall bypass was used, and no source was
  re-tried through another route after the fetch tool was refused.
- **Sources 1–6 and 16 were downloaded with `curl` from `raw.githubusercontent.com`** (plus one
  unauthenticated `api.github.com/repos/DFHack/df-structures/contents/` directory listing to find
  which file holds the geology enums) rather than through the fetch tool. This was a first choice so
  the files could be grepped whole, not a reaction to a block. Afterwards I spot-checked the same
  URLs with the fetch tool: `prospector.cpp`, the Phoebus `inorganic_stone_layer.txt` and the TFC
  `RegionChunkDataGenerator.java` were all served normally and the quoted lines matched;
  `df.d_basics.xml` was served but is too large for the tool's summariser to reach the enum section
  (a size limit, not a refusal). Claims that rest on these curl-retrieved files: all of §1.1, §1.4,
  the raws-derived lists in §1.2, the host table in §2.3, the `[P]` items of §3 (aquifer rule,
  heavy-aquifer hash, feature types, `start_depth`), the beach composition in §4, and the TFC
  algorithm in §5.3(a).
- The Phoebus repository (source 6) is a community graphics pack that bundles the game's raw files;
  it was used because the vanilla raws ship only inside the game download, not because another
  source refused. Treat it as an older-version copy (see §7).

Gaps — wanted, not obtained, not worked around:

| Wanted | Why unavailable | Effect on the report |
|---|---|---|
| Astrolith devlog "Rock Layers for Real-Time Erosion Simulation" (gamedev.net, source 23) | fetch tool got HTTP 403 | §5.3(f) rests on the search-result snippet only; nothing in §6 depends on it |
| "The Geology of Dwarf Fortress" (YouTube, MEkeUJ-4Gjo) | fetch returned only page chrome, no description or transcript | not used |
| Tarn Adams' chapters in *Procedural Generation in Game Design* (2017) | book, not openly online | §2.4: no first-hand design-intent statement; nothing is attributed to it |
| GitHub code search for `geo_layer_type` | API requires authentication (401); not authenticated | located the enum by listing and downloading the public repo files instead |
| Bay 12 forum analyses of geo-biome assignment; Bay 12 dev-log entries on when/why stone layers were added | not reached — searches surfaced no forum threads, and I did not attempt the forum directly | §1.5 "unknown" paragraph and §7 stand as gaps; the volcanism > 90 claim rests on the wiki talk page alone |
| A default `world_gen.txt` | the two graphics-pack paths I tried returned 404 | defaults in §3.2 taken from the wiki's World token page (source 13) instead |
