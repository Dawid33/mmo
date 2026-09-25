# Dwarf Fortress world generation — pipeline and field model

Research report 01 of the DF worldgen series (2026-09-20). Scope: the overall map-generation
pipeline, the scalar field model, the biome rules, the scale hierarchy, and why it works.
Sibling reports cover rivers/lakes/erosion, geology/stone layers, and adaptation to a
chunk-local generator in depth; those topics are only sketched here.

**Evidence tags used throughout**

| Tag | Meaning |
|---|---|
| **[T]** | Tarn Adams' own words (interview, book chapter, dev log, or a quote relayed by the wiki with a citation) |
| **[W]** | Dwarf Fortress wiki — community documentation, mostly derived from the in-game parameter UI and player experiments |
| **[RE]** | Reverse-engineered data structures (DFHack `df-structures`), which since v50 carry Bay 12's *original* struct/field names |
| **[I]** | Inference — mine or the community's; plausible but not confirmed |

DF is closed source. Everything about *algorithms* below is either [T] at the level of a
paragraph of prose, or [I]. Everything about *parameters, ranges and thresholds* is [W]/[RE]
and is solid. Section 7 lists the gaps explicitly.

**Access note.** The Bay 12 forum sits behind an anti-bot wall and the fetch tool refuses
web.archive.org, so no forum thread was read directly. Forum-sourced claims appear here only
as the wiki summarises them and are marked "forum, via wiki".

---

## 1. The pipeline, in order

The authoritative ordering is Tarn's 2019 description [T] (Game Developer Q&A, also quoted
verbatim on the wiki's *World generation* page). The 2008 Gamasutra interview [T] and the
Game AI Pro 2 chapter [T] add algorithmic detail. The in-game progress strings
("Preparing elevation… / Setting temperature… / Running rivers… / Forming lakes and
minerals… / Growing vegetation… / Verifying terrain… / Importing wildlife… / Recounting
legends… / Placing civilizations… …") are listed on the wiki with the caveat "this order is
not completely correct" [W].

### Step 0 — Allocate, choose the pole
- Allocates the map; picks the pole configuration (`POLE`: NONE, NORTH, SOUTH,
  NORTH_AND_SOUTH, or the random variants NORTH_OR_SOUTH / NORTH_AND_OR_SOUTH) [T][W].
- The equator is on the opposite edge from a single pole, or in the middle with two [W].
  With `POLE:NONE` there are no seasons and the tropical/frozen biome variants stop
  following latitude bands [W].

### Step 1 — Seed the six base fields on a coarse grid, fill fractally
- "The basic map field values (elevation, rainfall, temperature, drainage, volcanism,
  wildness) are seeded along a grid of variable size, respecting various settings (oceans,
  island sizes, other variances, etc.), and then filled in fractally." [T]
- 2008: "it uses midpoint displacement to make an elevation map… The drainage map is just
  another fractal, with values from 0 to 100." [T]
- Inputs: per-field `min:max:x_variance:y_variance`, per-field mesh size + five band
  weights, and the ocean-edge requirements. Mechanism detail in section 2.
- Each field is an **independent** fractal at this point; no field reads another yet.

### Step 2 — Poles bias temperature; peaks are chosen
- "The poles vary the temperature, and it selects some points for the highest peaks." [T]
- Peaks can only be tiles at elevation 400 (`PEAK_NUMBER_MIN`, large-region default 50) [W].

### Step 3 — First check, repair, or reject
- "Here it does a first pass to see how it is doing, and attempts to adjust some altitudes
  to fit the map within the desired parameters if it missed. The world can be rejected at
  this point if it is unfixable, and it tries again." [T]
- So rejection is the *second* line of defence: DF first tries to nudge elevations
  (e.g. toward the required ocean edges / elevation band counts) and only then rerolls [T].
  How the nudge works is unknown.
- Checks that belong here [W]: `ELEVATION_RANGES`, `RAIN_RANGES`, `DRAINAGE_RANGES`,
  `SAVAGERY_RANGES`, `VOLCANISM_RANGES` (minimum counts of low/mid/high tiles),
  `PARTIAL_OCEAN_EDGE_MIN` / `COMPLETE_OCEAN_EDGE_MIN`, `PEAK_NUMBER_MIN`.

### Step 4 — Vegetation (first derived field) and the initial biome test
- "The first derived field, vegetation, is then set based on elevation, rainfall,
  temperature, etc., and it tests for biome rejections if the map's biomes don't satisfy the
  ranges set in the parameters." [T]
- Biome checks here are the *initial* columns of `REGION_COUNTS:<type>:<init squares>:<init
  regions>:<final regions>` — minimum tile count and minimum contiguous-region count, measured
  "before things like erosion take place" [W].

### Step 5 — Flatten the mid elevations; place volcanoes
- "The mid-level elevations are smoothed at this point to make more plains areas, and
  volcanoes are placed respecting the hot spots in the volcanism field." [T]
- A volcano needs volcanism = 100 on the tile (`VOLCANO_MIN`) [W]. A volcano tile adjacent
  to a mountain tile is generated as if its elevation were 400 and counts as mountain [W].
- The plains smoothing is a deliberate *art-direction* pass: raw fractal terrain has too
  little flat land, so the 100–299 band is flattened before erosion. Filter unknown.

### Step 6 — Erosion and rivers (detail in sibling report)
- "Small oceans are dried out, and it locates edges of mountain sides where it can run test
  rivers… Many fake rivers flow downward from these points, carving channels in the elevation
  field if they can't find a path to the sea. Extreme elevations differences are often
  smoothed here so that everything isn't canyons. Ideally we'd use mineral types for that,
  but we don't yet. Lakes are grown out at several points along the rivers." [T, 2019]
- 2008 gives the motivation and the agent rule: "You still get some artifacts from the
  midpoint displacement. It tends to like vertical and horizontal lines, so the next step is
  the erosion phase. It picks out the bases of the mountains (mountains are all squares above
  a given elevation), then it runs temporary river paths out from there, preferring the
  lowest elevation and digging away at a square if it can't find a lower one, until it get to
  the ocean or gets stuck." [T]
- Then the real rivers: "This will generally leave some good channels to the ocean, so it
  runs the real rivers after this. However, some of them don't make it, so it forces paths to
  the ocean after that, and bulges out some lakes. Then it loop-erases the rivers, and sends
  out (invisible on the world map) brooks out from them. During the loop-erase it also
  calculates flow amounts and decides which rivers are tributaries, and names the rivers."
  [T, 2008; dev log 2006-05-25 and 2007-03-16 confirm loop erasure, flow amounts, tributaries]
- Parameters [W]: `EROSION_CYCLE_COUNT` (0–1000, default 250; higher = less jagged, wider
  major rivers; at max "your mountains will dissolve before your eyes into plains"), `RIVER_MINS:<pre>:<post>` (riverhead
  counts required before/after erosion; large-region default 400:400; very high pre-erosion
  values "speed erosion greatly"), `PERIODICALLY_ERODE_EXTREMES` (turns sheer walls into ramp
  series). Rivers only appear if max elevation ≥ 104 [W].
- [RE] tile flags confirm the two-stage design: `TEMPRIVER`, `RIVER`, `RIVER_N/S/E/W`,
  `ORIGINAL_RIVER_SOURCE`, `NEWLAKE`, `LAKE`, `BROOK`.

### Step 7 — Second smoothing; peak/volcano touch-up
- "Elevations are smoothed again from the mountains down to the sea, and the peaks and
  volcanoes do some local adjustments." [T] After this, elevation is final.

### Step 8 — Rain shadows / orographic precipitation (needs final elevation)
- "Now that the elevations are finalized, it makes adjustments to rainfall based on rain
  shadows and orographic precipitation." [T]
- The wiki's description of the model: moist air from the ocean is forced up by rising
  terrain and rains out on the seaward side; past a tall enough range nothing is left [W].
- Toggle `OROGRAPHIC_PRECIPITATION`. It can push rainfall *outside* the configured min/max,
  and makes rainfall more extreme (more forest, desert, marsh, grassland) [W].
- Forum finding via wiki: tiles with drainage ≥ 50 also cast rain shadows regardless of
  elevation/biome (drainage doubles as "hilliness") [W, forum via wiki — not read directly].
- Wind direction model: **unknown**. The runtime tile struct has air-mass and daily-wind
  fields for the weather sim [RE], but nothing public says worldgen uses them.

### Step 9 — Temperature reset, final vegetation
- "Temperatures are reset based on elevation and rainfall and the dampening effects of
  forests, and it uses the new values to set the vegetation level one final time." [T]
- Dependency chain is therefore: elevation → rainfall → temperature → vegetation.
  Dev log 2006-06-11: "took humidity into consideration for temperature variations" [T].

### Step 10 — Salinity
- "Salinity values are set for the ocean and tiles neighboring the ocean." [T, 2019]
- 2008: "it just sets the salinity to 100 at the oceans (oceans are just all the low
  elevation spots), and tapers it off quickly over the land squares." [T]

### Step 11 — Regions: flood-fill biome groups, name them
- "Now that everything has settled down, we can detect the limits of the final biome regions
  and give them names and their own identity." [T]
- 2008: "it actually looks at the general biome groups of each square (what I call a region
  type) and forms regions, giving them names. So you can have the 'Silvery Forest' which is
  taiga in the cold regions and a jungle in the warm region, as long as it is connected." [T]
- Region types [W][RE]: Swamp(wetland), Desert, Forest, Mountains, Ocean, Lake, Glacier,
  Tundra, Grassland, Hills. Grouping table in section 3.4. Landmasses are identified and
  islands named too (dev log 2007-05-21) [T].
- `SUBREGION_MAX` (default 2750, cap 5000) rejects maps that fragment into too many regions —
  this is effectively a *smoothness* constraint on the combined fields [W].

### Step 12 — Geology and underground layers (detail in sibling report)
- "We also add the geological layers and the underground layers here, though the geological
  stuff should really be earlier." [T] — Tarn flags this himself as mis-ordered: erosion
  cannot respond to rock hardness because rock does not exist yet.

### Step 13 — Final verification
- "There's a final verification process against the parameters here, to make sure it hasn't
  drifted too far afield from what the player wanted." [T]
- Here: the *final* region counts of `REGION_COUNTS`, post-erosion `RIVER_MINS`,
  `SUBREGION_MAX`, cave counts [W].

### Step 14 — Wildlife, weather
- "It generates the initial wildlife populations in each region, and sets some weather
  variables." [T] Populations are per region, by biome (2008: "strictly by biome type").

### Step 15 — Good/evil, civilizations, history
- Good/evil is *not* a fractal: it is assigned per whole region, bucketed by region size
  (small ≤ 24 tiles, medium 25–99, large ≥ 100) until `GOOD_SQ_COUNTS` / `EVIL_SQ_COUNTS`
  tile goals are met; any non-zero goal forces at least one whole region of that size class
  [W]. It is nevertheless stored per tile as `field_EVIL` [RE].
- Then civ placement, caves, megabeasts, and history ("a giant zero-player strategy game")
  [T]. Out of scope here, except: civ placement is a **late rejection source** — dwarves need
  non-evil, non-good, non-savage mountain tiles not surrounded by ocean [W].

### Pipeline as a dependency graph

```
seeds ──► elevation₀ ─┬─► [check/adjust/reject]
          rainfall₀   │
          temperature₀ (+pole/latitude)
          drainage    │
          volcanism   │
          savagery    ▼
                vegetation₀ ─► [initial biome-count reject]
elevation₀ ─► plains smoothing ─► volcano placement
           ─► dry small oceans ─► test rivers carve ─► cliff smoothing ─► lakes
           ─► real rivers ─► forced outlets ─► loop-erase/flow/tributaries ─► brooks
           ─► smoothing mountains→sea ─► peak/volcano touch-up ─► elevation_final
elevation_final + rainfall₀ (+drainage≥50) ─► rainfall_final   (rain shadow)
elevation_final + rainfall_final + forest ─► temperature_final
all of the above ─► vegetation_final ─► salinity ─► biome(tile) ─► regions (flood fill)
regions ─► geology ─► [final verify/reject] ─► wildlife ─► good/evil ─► civs ─► history
```

---

## 2. The scalar fields and how they are generated

### 2.1 Field inventory

| Field | Range | How produced | Used for |
|---|---|---|---|
| Elevation | worldgen scale 0–400; ocean < 100, land 100–299, mountain ≥ 300, peak = 400 [W] | Fractal; then plains smoothing, erosion, re-smoothing [T] | Ocean/mountain cut, rivers, rain shadow, temperature |
| Rainfall | 0–100 [W][RE] | Fractal; then orographic/rain-shadow pass [T] | Biome x-axis, river count, vegetation |
| Temperature | param range −1000…1000, defaults 25–75 [W] | Fractal (default mesh "ignore") + latitude from poles + elevation + rainfall + forest damping [T] | Frozen/temperate/tropical biome variants, civ placement |
| Drainage | 0–100 [T][W] | "Just another fractal" [T]; never modified afterwards as far as known | Biome y-axis (wet vs. dry ground, flat vs. hilly), rain shadow, soil depth (reported) |
| Volcanism | 0–100 [W] | Fractal | Volcano sites (=100), igneous layer frequency |
| Savagery ("wildness") | 0–100; 0–32 calm, 33–65 neutral, 66–100 savage [RE] | Fractal | Creature sets, civ exclusion (≥ 66 blocks civs [W]) |
| Good/evil | 0–100; 0–32 good, 33–65 neutral, 66–100 evil [RE] | Assigned per region, by size bucket [W] | Creature/plant sets, evil weather |
| Vegetation | 0–100 [RE] | **Derived**: elevation, rainfall, temperature [T]; mutable at runtime (logging) [T] | Tree density |
| Salinity | 0–100 [RE] | **Derived**: 100 in ocean, quick taper inland [T] | Fresh/brackish/salt variants |

Notes:
- Bay 12's own name for drainage is `field_GEOLOGY` and for the good/evil field `field_EVIL`
  [RE] — drainage was conceived as a ground/rock property, not as hydrology.
- The tile struct reserves 25 int16 "field" slots; 9 (`UNUSED16..24`) are empty [RE].
  Other runtime slots: `AIRMASS_TEMP/X/Y`, `WEATHER`, `DAILY_WINDS`, `SNOWFALL` — the weather
  sim, not worldgen.
- Stored elevation after worldgen is rescaled: "0-99=Ocean, 150+=Mountains, 100-149: all
  other biomes. Note that PSV [preset] elevation uses 100-299 for normal biomes, with range
  later cut to 1/4, and Mountains shifted down" [RE]. So the 100–400 land band is compressed
  ×¼ into 100–175 once generation is done. [I] the compressed unit is what maps onto
  z-levels at embark.
- Worldgen temperature scale: `local °U = world_temp × 0.75 + 10000` (Toady, forum via wiki)
  — so 0 is freezing and 1 unit = 0.75 °F ≈ 0.42 °C [T via W].

### 2.2 The `min:max:x_variance:y_variance` token

`[ELEVATION:1:400:401:401]`, `[RAINFALL:0:100:200:200]`, `[TEMPERATURE:25:75:200:200]`,
`[DRAINAGE:0:100:200:200]`, `[VOLCANISM:0:100:200:200]`, `[SAVAGERY:0:100:200:200]`
(medium-region defaults [W]).

- min/max clamp the *initial* fractal; later passes (rain shadow, latitude) may exceed them [W].
- Variance (0–3200) controls how wildly adjacent tiles differ; X and Y are separate, so
  unequal values yield banded worlds. Variance 0 on both axes = one constant value for the
  whole map [W].
- Defaults scale with world side: elevation variance 52 / 202 / 401 / 800 for
  17 / 65 / 129 / 257-tile worlds; islands use 1600 on large maps [W, default `world_gen.txt`].
  [I] Doubling with the side length is exactly what you need if "variance" is the
  displacement amplitude at the *coarsest* subdivision level of a midpoint-displacement
  fractal: one more level of subdivision, one more doubling, same per-tile roughness.
- Wiki experiment: the visual effect of variance depends on the mesh *count*, not on cell
  size in tiles — "a large world with a 32×32 mesh will look essentially the same as a pocket
  world with a 2×2 mesh, only stretched" [W]. Consistent with the inference above.

### 2.3 Mesh sizes and weighted ranges (`*_FREQUENCY`)

`[ELEVATION_FREQUENCY:<mesh>:<w 0-20%>:<w 20-40%>:<w 40-60%>:<w 60-80%>:<w 80-100%>]`
— same shape for RAIN, DRAINAGE, TEMPERATURE, SAVAGERY, VOLCANISM.

Procedure as documented [W]:
1. Divide the world into `2^(mesh−1)` cells per axis (mesh 2 → 2×2 … 6 → 32×32). Minimum
   cell span is 9 world tiles with shared borders; world sizes are `2^n + 1` (17…257), the
   classic midpoint-displacement lattice.
2. At each lattice intersection, pick one of five equal-width bands of the field's
   [min,max] with probability ∝ the weights, then a value within the band.
3. "Smooth out the areas between the intersection points."
4. "Add noise according to the variance parameters."

Details:
- mesh = 1 ("Ignore") → only the four world corners get random values and everything
  between is fractal fill (1×1 grid; rectangular worlds use 1×k) [W]. **All six fields use
  Ignore with weights 1:1:1:1:1 in every default preset** [W, default `world_gen.txt`] — the
  mesh is a power-user shaping tool, not part of the stock look.
- A weight of 0 only stops *lattice points* landing in that band; interpolated tiles still
  pass through it [W].
- Weights shape the histogram non-linearly without making any band impossible — preferable
  to narrowing min/max, which can delete biomes outright [W].
- Too-high variance drowns the mesh; the wiki notes a 2×2 mesh is barely recognisable at
  default variance [W].
- [I] Steps 2–4 read as "replace the first `mesh−1` levels of midpoint displacement with
  band-weighted random draws, then continue the subdivision normally". Not confirmed.

How the ocean-edge settings ("oceans, island sizes" in Tarn's words) feed into seeding is
unknown; islands in the presets are obtained with `COMPLETE_OCEAN_EDGE_MIN:4`, elevation
variance 1600, and a rejection floor of 33,000 low tiles out of 66,049 [W].

### 2.4 Rejection parameters (large-region defaults [W])

| Token | Default (257²) | Meaning |
|---|---|---|
| `ELEVATION_RANGES` / `RAIN_` / `DRAINAGE_` / `SAVAGERY_` / `VOLCANISM_RANGES` | 8256 : 16512 : 8256 | min tiles in low / mid / high band = 12.5 % / 25 % / 12.5 % of the map |
| `REGION_COUNTS:MOUNTAINS` / `OCEAN` / `GRASSLAND` / `HILLS` | 8256 tiles; 9–13 initial regions; 6–12 final | tile floor + contiguous-region floors before and after erosion |
| `REGION_COUNTS:FOREST` | 4128 : 13 : 12 | |
| `REGION_COUNTS:SWAMP` / `DESERT` | 1032 : 7 : 6 | |
| `REGION_COUNTS:GLACIER` / `TUNDRA` | 0 : 0 : 0 | not required |
| `PEAK_NUMBER_MIN` | 50 | needs that many elevation-400 tiles |
| `VOLCANO_MIN` | 15 | needs that many volcanism-100 tiles |
| `PARTIAL_OCEAN_EDGE_MIN` / `COMPLETE_…` | 2 / 0 (islands: 0 / 4) | |
| `RIVER_MINS` | 400 : 400 | riverheads before / after erosion |
| `SUBREGION_MAX` | 2750 | cap on number of named regions |

Smaller presets scale these roughly with area (e.g. medium: `ELEVATION_RANGES
2080:4160:2080`, `PEAK_NUMBER_MIN 12`, `RIVER_MINS 100:100`) [W]. After 86 consecutive
rejections the game shows an error dialog; `map_rejection_log.txt` records each reason [W].

---

## 3. From fields to biomes

Tarn, 2008: "The biomes are not the basics, they arise, at least in DF, from several factors:
temperature, rainfall, elevation, drainage… So we can now query a square and get rainfall,
temp, elevation and drainage data. This is where the biome comes from." [T]

Evaluation order [W]: elevation → temperature (frozen override) → rainfall × drainage →
temperature/salinity variants.

### 3.1 Elevation gates
| Elevation (worldgen scale) | Result |
|---|---|
| 0–99 | Ocean (arctic / temperate / tropical by temperature) |
| 100–299 | Land biome from the table below |
| 300–400 | Mountain (400 = eligible peak) |

Lakes are a special case: generated by the river pass on tiles that would otherwise be
≥ 100, and they never merge into neighbouring land regions [W].

### 3.2 Rainfall × drainage table (land, non-frozen)

Read from the wiki's `DF_Biome_Distribution.png` chart [W, experimentally derived]:

| Rainfall → | 0–9 | 10–19 | 20–32 | 33–65 | 66–74 | 75–100 |
|---|---|---|---|---|---|---|
| **Drainage 0–32** | Sand desert | Grassland (flat) | Savanna (flat) | **Marsh** | **Swamp** | **Swamp** |
| **Drainage 33–49** | Rocky wasteland (flat) | Grassland (flat) | Savanna (flat) | Shrubland (flat) | Conifer forest | Broadleaf forest |
| **Drainage 50–65** | Rocky wasteland (hilly) | Grassland (hilly) | Savanna (hilly) | Shrubland (hilly) | Conifer forest | Broadleaf forest |
| **Drainage 66–100** | Badlands | Grassland (hilly) | Savanna (hilly) | Shrubland (hilly) | Conifer forest | Broadleaf forest |

- Thirds (33/66) and halves (50) of a 0–100 scale, plus 10/20/75 on rainfall. Nothing subtle.
- "Hilly" is a drainage ≥ 50 property, not a terrain measurement: the HILLS region type is
  "grassland-class biome with high drainage". The local terrain generator then makes those
  tiles bumpier [I from W].
- Tarn's 2008 example — "if rainfall is >=66/100 and drainage is less than 50, then you have
  a swamp" [T] — disagrees with the chart's < 33. Either the rule changed after 2008 or he
  was simplifying; the chart reflects current versions.
- The wiki's older "Biome Type Requirement Table" (marked *verify*) puts forest at drainage
  66–100 and the grassland/hills split at 66; the chart is the better-tested source [W].

### 3.3 Temperature, salinity and drainage variants [W]
| Condition | Effect |
|---|---|
| temp ≤ −5 | all land biomes → **Tundra** (drainage < 75) or **Glacier** (drainage ≥ 75); ocean → arctic |
| −4 ≤ temp ≤ 9 | conifer forest → **Taiga** |
| temp ≳ 85 | tropical variants (tropical conifer / dry broadleaf / moist broadleaf, tropical grassland…) |
| salinity 0 / 33 / 66 / 100 | freshwater / freshwater wetland but brackish lake / saltwater / ocean |
| tropical saltwater swamp and drainage ≤ 9 | **Mangrove swamp** |

Civ placement: elves need temp ≥ 10, humans ≥ 0 [W].

### 3.4 Region grouping (flood fill) [W]
| Region type | Member biomes |
|---|---|
| Wetland | swamp, marsh |
| Forest | broadleaf, conifer, taiga |
| Grassland / Hills | grassland, savanna, shrubland (split by flat / hilly) |
| Desert | badlands, rocky wasteland, sand desert |
| Lake, Tundra, Glacier, Ocean, Mountains | themselves |

A region is a contiguous set of world tiles of one type, has one name, one good/neutral/evil
alignment, and owns plant/animal populations. Savagery may vary inside a region. Temperature
differences do *not* split a forest region (taiga and jungle can share a name) but frozen
types (tundra/glacier) always split [T][W].

### 3.5 Surroundings
3 × 3 matrix of alignment (good / neutral / evil — per region) × savagery (calm / neutral /
savage — per tile, thirds of 0–100): Serene, Mirthful, Joyous Wilds / Calm, Wilderness,
Untamed Wilds / Sinister, Haunted, Terrifying [W][RE]. Pure gameplay overlay: it selects
creature and plant sets and never feeds back into terrain.

---

## 4. Scale hierarchy and how local detail is derived

### 4.1 Sizes
| Level | Size | Notes |
|---|---|---|
| World | 17², 33², 65², 129² or 257² world tiles (`DIM`, always 2ⁿ+1; non-square allowed but crash-prone) [W] | large = 66,049 tiles |
| World tile (wiki: "region tile"; struct: `region_squarest`) | 16 × 16 mid-level tiles | carries all the scalar fields |
| Mid-level tile (embark tile, "local area block", DFHack "MLT") | 48 × 48 tiles | default fortress embark is a few of these per side |
| Tile | ≈ 2 m × 2 m × 3 m (Toady, forum via wiki); movement maths implies ≈ 2.4 m [W] | |

Arithmetic: one world tile = 768 tiles ≈ 1.5–1.8 km per side. A large world = 197,376 tiles
per side ≈ 395–475 km, i.e. 156,000–225,000 km²; the wiki's "comparable to Minnesota"
(225,000 km²) matches the 2.4 m figure. **The entire simulated climate/erosion/river model
runs on at most a 257 × 257 grid** — every global pass is cheap, which is what makes
rejection sampling affordable.

### 4.2 The mid-level map ("midmap") [RE]
DF does not store local terrain. When a world tile is needed it builds a
`region_midmapst` (DFHack `world_region_details`) holding, per world tile:

| Member (Bay 12 name) | Shape | Role |
|---|---|---|
| `el_map` | 17 × 17 int16 | elevation lattice at mid-level-tile resolution; 17 = 16 + shared border, so neighbours agree on the seam |
| `map` (biome) | 17 × 17 int8, values 1–9 | **which of the 3 × 3 neighbouring world tiles this mid-level tile takes its biome from** (5 = own, 1 = SW, 9 = NE…) |
| `local_seed` | 16 × 16 uint32 | one RNG seed per mid-level tile — local terrain is regenerated deterministically on demand |
| `local_ud_edge_data`, `local_lr_edge_data`, `local_ul_corner`, `local_u_edge`, `local_l_edge` | per edge / corner | where a biome boundary crosses each embark-tile edge and which side owns each edge and corner; tile biomes inside the frame are "semi-randomly interpolated from this edge spec"; fixed priority rules (ocean/lake lose edges to mountain, both lose to anything else) |
| `river_data_ud_*`, `river_data_lr_*` | per edge | river crossing extent (`b1`,`b2`), flow flag and **elevation** at every mid-level edge |
| `feature_square`, `square_flag` | 16 × 16 | per-tile feature lists (caves, underground layers, river source/sink flags…) |
| `temp_el_map`, `path_map` | 16 × 16 | scratch used while routing |

World-level `region_edge_datast` additionally stores `lr_edge_seed` / `ud_edge_seed` on a
`(dimx+1) × (dimy+1)` lattice, `square_seed` per world tile and an `edging_value` byte
("holds ul,u,l edge values for midmaps") [RE].

What this tells us:
- **Biome borders are dithered, not interpolated.** The scalar fields are *not* bilinearly
  resampled and re-thresholded at fine scale; each mid-level tile picks a neighbouring world
  tile to copy, and the 48 × 48 tiles inside split along edge-owned break points. The wiki
  says the same in prose: "Biomes are calculated for each region tile… then noise is
  calculated and applied which causes the irregular region boundaries on the local map" [W].
- **Seams are solved with edge-owned seeds.** Anything on a shared edge is derived from a
  seed that belongs to the edge (or from the N/W neighbour by a fixed convention), never from
  either tile's own seed — so both sides compute identical results independently.
- **Rivers are pinned at edges.** Position, width and *elevation* are fixed where a river
  crosses each mid-level edge; the local generator only connects the pins. That is how a
  world-scale river stays continuous and downhill across independently generated blocks.
- The dev log shows this architecture dates from 2006: "started map generation based on
  region fields" (05-25), "made fake map squares respect the biome" (06-12) [T].

How the 17 × 17 lattice is refined to 48 × 48-tile heightmaps (interpolation kernel, local
fractal, how drainage "hilliness" scales the amplitude, cliffs/ramps) is **not publicly
known**. [I] Seeded midpoint displacement between lattice values is the obvious guess given
everything else in the pipeline, but it is a guess. One confirmed oddity: z-level budgets
apply per 16 × 16 world tile, so one tall peak adds empty air levels to every embark in the
tile [W].

---

## 5. Why the results look good

### 5.1 Tarn's own reasons [T]
1. **Generate causes, derive effects.** "A lot of initial attempts at a world generator will
   start with things like 'I need to lay down some forests, and some mountains, and some
   rivers, and some deserts...' and then when you end up with a jungle next to a desert, or a
   desert next to a swamp in an unlikely way, it's difficult to fix. So the idea is to go down
   to basic elements." (2008) Game AI Pro 2, Principle 2: handling the fields separately gave
   "a more natural, internally consistent solution" than spawning biomes or letting one
   fractal define them.
2. **Continuous fields give natural borders for free.** "The nice thing about having the
   fractally-generated basic fields is that the biome boundaries all look natural." Adjacent
   biomes are neighbours in (rainfall, drainage) space, so implausible adjacencies need a
   steep gradient in a smooth field — rare by construction.
3. **Rain shadows were the biggest single upgrade.** "The world maps improved greatly when
   rain shadows were taken into consideration." (Principle 4: base the model on real-world
   analogs; when it looks wrong, real geography tells you what is missing.)
4. **Drainage is the non-obvious second axis.** "Oddly enough, drainage was another
   nonobvious consideration that helped smoothly delineate forests from swamps." One
   moisture axis gives a desert→grass→forest gradient only; a second, independent ground
   axis yields swamp vs. forest, marsh vs. shrubland, sand vs. badlands, flat vs. hilly.
5. **Erosion exists to kill fractal artifacts.** Midpoint displacement "tends to like
   vertical and horizontal lines, so the next step is the erosion phase." The carving agents
   are an artifact remover first and a hydrology model second.
6. **Don't shoehorn.** "Just lots of basic information, so you don't try to shoehorn
   anything into place. Just let it happen and these fields can all potentially be altered."
   Vegetation is a mutable field (logging) and the biome is always re-derivable.
7. **Don't overplan / don't overcomplicate** (Principles 1 and 3): get something running,
   iterate, model "at the level of what the player sees or one layer below".

### 5.2 Structural reasons visible in the design [I, from the sources above]
- **Order matters, and it is causal.** Elevation is finalised *before* rainfall is corrected;
  rainfall before temperature; temperature before vegetation; all before biome
  classification; classification before region flood-fill. Every derived layer sees the
  finished version of its inputs, so there is never a desert in a river-cut wet valley left
  over from a pre-erosion pass.
- **Rivers carve first, flow second.** Hundreds of throw-away agents dig channels, then the
  real rivers route over terrain that already drains. Forced outlets + lake "bulges" handle
  the remainder. Result: every river reaches the sea or a lake, tributaries merge with
  accumulated flow, and valleys exist where rivers are.
- **Two art-direction smoothing passes** (mid-elevation plains, mountains-to-sea) fix the
  fractal's weak spot — too little flat land and no coastal plains — without touching the
  mountains' roughness.
- **Generate-and-test ("belt and braces" [W]).** Shaping parameters bias the fields;
  rejection parameters guarantee the outcome: enough ocean, mountains, every region type in
  several separate patches, enough riverheads, not too fragmented. The player never sees the
  degenerate rolls. Tarn adds a repair step before rejecting. The wiki warns that
  rejection is a poor *steering* tool — bias the generator, use rejection only as the
  guarantee.
- **Contiguity counts, not just area.** `REGION_COUNTS` asks for N *separate* forests, not
  X % forest — a direct proxy for "the map has varied geography" that area statistics miss.
- **Identity on top of physics.** Flood-filled, named regions ("The Silvery Forest") with
  their own alignment and fauna turn a scalar soup into places. Much of DF's perceived map
  quality is this layer plus history, not terrain fidelity.
- **Independent gameplay fields** (savagery, good/evil, volcanism) are decoupled from climate,
  so difficulty and resources vary independently of biome: a calm desert and a terrifying one.

### 5.3 Honest limits
- Base terrain is plain midpoint displacement; no tectonics, no ridged noise. Mountain
  *ranges* come from fractal luck + rejection + erosion, and DF maps do show blobby mountain
  masses. Tarn calls the geology ordering wrong himself.
- No hydrological feedback into rainfall/vegetation (a river does not green a desert).
- Rain-shadow coupling to drainage ≥ 50 looks like a hack to make "hills" matter.
- The world is tiny (≤ 257²) and generated all at once; the design leans on cheap global
  passes and rerolls, neither of which carries over directly to an unbounded streamed world.

---

## 6. Most transferable ideas (brief — the sibling adaptation report covers this in depth)

1. **Field stack → pure biome function.** Keep elevation, rainfall, temperature, drainage
   (+ salinity, vegetation derived) as separate low-frequency fields; classify with a small
   threshold table (section 3.2 is directly reusable). A drainage-like second ground axis is
   the cheapest big win if the generator currently has only moisture + temperature.
2. **Causal ordering of derived fields**: final elevation → orographic rainfall → temperature
   (latitude, altitude lapse, humidity, forest damping) → vegetation → biome.
3. **Do the expensive coupled work on a coarse grid** (DF: ~1.5 km cells) and treat fine
   terrain as a deterministic, seeded refinement of it. DF's world tile ≈ one and a half of
   our 1024-unit regions if a voxel is ~1 m.
4. **Edge-owned seeds and pinned crossings** for seam-free independent generation: shared
   lattice values, per-edge seeds, river position/width/elevation fixed on every block edge.
5. **Dither biome borders at fine scale** by letting fine cells adopt a neighbouring coarse
   cell's biome, rather than re-thresholding interpolated fields (no spurious sliver biomes
   from interpolation crossing a threshold).
6. **Generate-and-test on the coarse map** with contiguity-aware acceptance tests. For a
   fixed-seed world this is an offline seed scan (the repo already has
   `crates/worldgen/examples/seed_scan.rs` for ocean spawn); DF's criteria list is a good
   starting checklist.
7. **Art-direction passes are legitimate**: flatten the mid band for plains, smooth
   mountain-to-sea, erode specifically to remove the noise function's artifacts.
8. **Named flood-filled regions** as the unit for fauna, alignment and naming.

---

## 7. Known vs. unknown

**Solid (multiple sources, or [T] + [W]/[RE] agreeing)**
- The pipeline order in section 1 (Tarn 2019; consistent with 2008, Game AI Pro 2, dev logs).
- The field list, ranges, and that biomes are a pure function of fields.
- Elevation gates 100/300/400; rainfall × drainage chart; temperature/salinity variant rules
  (wiki, experimentally derived; elevation gates corroborated by [RE] comments).
- Savagery and evil thirds (0–32 / 33–65 / 66–100) [RE].
- All parameter tokens, ranges, defaults and rejection semantics (in-game UI + default
  `world_gen.txt`).
- Scale hierarchy 16 × 16 × 48 × 48 and the midmap data layout [W][RE].
- Base elevation is midpoint displacement; erosion = downhill agents from mountain bases
  that dig when stuck; real rivers afterwards with forced outlets, lakes, loop erasure, flow
  accumulation, tributary detection, brooks [T].

**Known only as one sentence of prose [T], algorithm unknown**
- The first-pass "adjust some altitudes to fit" repair step.
- Mid-elevation plains smoothing; mountains-to-sea smoothing; peak/volcano "local
  adjustments"; "small oceans are dried out".
- Rain shadow: wind direction(s), moisture budget, how much rain is added windward.
- Temperature formula: latitude curve, altitude lapse, rainfall and forest-damping terms.
  (Wiki guesses poles move temperature by "more than 75" units, flagged *research needed*.)
- Vegetation formula. Salinity taper distance.
- Lake growth ("bulges out some lakes", "at several points along the rivers").

**Community inference, unconfirmed**
- Mesh = pre-seeding coarse subdivision levels; variance = coarsest-level amplitude (2.2–2.3).
- Drainage ≥ 50 casting rain shadows (forum experiment, via wiki; not read directly).
- Low drainage → thicker soil (wiki: "reported").
- How the 17 × 17 midmap lattice becomes 48 × 48-tile heightmaps.
- The 2008 "drainage < 50 → swamp" vs. today's < 33: changed, or simplified in speech.

**Not consulted**
- *Procedural Generation in Game Design* (Short & Adams, 2017), "Worlds" chapter —
  paywalled; no excerpt found. Tarn is co-editor; whether it adds algorithmic detail beyond
  the interviews is unverified.
- Bay 12 forum threads cited by the wiki (topics 20685 — 40d biome analysis; 139916 — mesh /
  variance / rain-shadow experiments; 140685 — drainage and rain shadows; 169696 — Future of
  the Fortress temperature-scale answer): blocked to automated access, see the access note.
- Conference talks (Roguelike Celebration, PRACTICE, GDC): video only; not transcribed here.

---

## Sources

| Source | Contribution | Reliability |
|---|---|---|
| Game Developer (Gamasutra), "Q&A: Dissecting the development of Dwarf Fortress with creator Tarn Adams", 2019 — https://www.gamedeveloper.com/design/q-a-dissecting-the-development-of-i-dwarf-fortress-i-with-creator-tarn-adams | The ordered step list quoted throughout section 1; geology-ordering and "mineral types" self-criticism | Primary [T]; most complete single statement |
| Game Developer (Gamasutra), "Interview: The Making Of Dwarf Fortress", 2008 — https://www.gamedeveloper.com/design/interview-the-making-of-dwarf-fortress | Midpoint displacement; fields-not-biomes rationale; erosion agent rule; real rivers, forced outlets, loop erasure, flow, brooks; salinity; region formation; swamp example | Primary [T]; describes the 2008 codebase, one threshold differs from today |
| Tarn Adams, "Simulation Principles from Dwarf Fortress", *Game AI Pro 2*, ch. 41 (2015) — https://www.gameaipro.com/GameAIPro2/GameAIPro2_Chapter41_Simulation_Principles_from_Dwarf_Fortress.pdf | Pipeline summary; four principles; rain-shadow and drainage remarks | Primary [T]; short (3 pages), high-level |
| Bay 12 dev logs 2006 / 2007 — http://www.bay12games.com/dwarves/dev_2006.html , http://www.bay12games.com/dwarves/dev_2007.html | Dates and one-liners: loop erase, region-field-based local maps, salinity field, humidity→temperature, volcanism field, landmasses, tributaries by flow | Primary [T]; terse |
| DF wiki, "World generation" — https://dwarffortresswiki.org/index.php/World_generation | World sizes, 16×16×48×48 hierarchy, rejection overview, verbatim 2019 quote, 86-rejection dialog | Community, rated "Exceptional"; well maintained |
| DF wiki, "Advanced world generation" — https://dwarffortresswiki.org/index.php/Advanced_world_generation | Every token, range, mesh/weight procedure, variance behaviour, erosion/river/orographic parameters, rejection tables, progress strings | Community; parameter facts solid, mechanism prose is experiment-based |
| DF wiki, "Biome" + `DF_Biome_Distribution.png` — https://dwarffortresswiki.org/index.php/Biome | Elevation gates, rainfall×drainage chart, temperature/salinity/mangrove rules | Community, experimentally derived; best available threshold source |
| DF wiki, "Region", "Surroundings", "World rejection", "v0.31:World rejection", "Climate", "Temperature", "Tile", "Embark", "Volcano", "Mountain" — https://dwarffortresswiki.org/ | Region grouping table and size classes; surroundings matrix; rejection reasons; temperature scale conversion; tile size | Community; mixed quality, cross-checked where used |
| DF wiki, default `world_gen.txt` — https://dwarffortresswiki.org/index.php/World_gen.txt | Actual default presets per world size (variance scaling, rejection floors, all meshes = Ignore) | Dump of game data; reliable |
| DFHack `df-structures`: `df.region.xml`, `df.region_midmap.xml` — https://github.com/DFHack/df-structures | Tile field list with Bay 12 original names and value-range comments; savagery/evil thirds; post-gen elevation rescale; river/lake tile flags; full midmap layout (17×17 elevation, biome reference digits, seeds, edge/river pins) | Reverse-engineered but tracks the real binary layout, original names supplied by Bay 12 since v50; comments are DFHack contributors' |
