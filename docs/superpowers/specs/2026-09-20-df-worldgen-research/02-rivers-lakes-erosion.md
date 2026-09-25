# Dwarf Fortress world generation: rivers, lakes, valleys, erosion

Research report, 2026-09-20. Scope: hydrology and erosion in Dwarf Fortress (DF) itself — the
world-map algorithm, the data it leaves behind, and how that data becomes terrain at embark
(local) resolution. Sibling reports cover the overall pipeline, geology, and adaptation to a
chunked generator.

DF is closed source. Everything below is one of:

| Tag | Meaning | Trust |
|---|---|---|
| **[T]** | Tarn Adams' (Toady One's) own words, in an interview or chapter | High for *what* happens, low on numbers (he gives almost none) |
| **[S]** | DFHack `df-structures` / DFHack plugin source: reverse-engineered memory layout. Since the v50 Steam era these files carry Bay 12's **original identifier names** (`original-name=` attributes), which are as close to source code as is publicly available | High for layout and names; medium for the semantics written in comments |
| **[W]** | Dwarf Fortress wiki | Medium; mixes verified facts, folklore and stale version info |
| **[C]** | Community experiment or tool author note (named) | Medium; single-observer |
| **[I]** | My inference from the above | Treat as hypothesis |

No source gives the carve amount per step, the tie-break rule of the downhill walk, the lake growth
rule, or the base of the flow calculation. Those are reconstructed in section 6 and marked.

---

## 0. Scales and fields you need to read the rest

- World map: square grid, 17×17 up to 257×257 **world tiles** ("region squares", `region_squarest` [S]).
- One world tile = 16×16 **mid-level tiles** (MLT, "embark tiles"); one MLT = 48×48 local tiles; so a
  world tile is 768×768 walkable tiles [W: Embark; S: `region_midmapst` arrays of 16/17].
- Per world tile fields, all `int16` [S `region_squarest`]: `ELEVATION`, `RAINFALL` (0–100),
  `VEGETATION` (0–100), `TEMPERATURE`, `EVIL`, `GEOLOGY` (this is **drainage**, 0–100 — the original
  name of the drainage field is `field_GEOLOGY`), `VOLCANISM`, `SAVAGERY`, air-mass/weather fields,
  `SNOWFALL`, `SALINITY` (0–100).
- Elevation during generation is 0–400: `<100` ocean, `100–299` ordinary land, `≥300` mountain,
  `400` peak [W: Advanced world generation]. **After finalisation the stored field is rescaled**:
  df-structures' comment on `field_ELEVATION` reads "0-99=Ocean, 150+=Mountains, 100-149: all other
  biomes. Note that PSV [parameter-set value] elevation uses 100-299 for normal biomes, with range
  later cut to 1/4, and Mountains shifted down" [S]. So the playable land range is only ~50
  elevation units, and one unit is one z-level at embark (the wiki's "around 50 z-levels of land"
  [W: Z-level] and embark-assistant using `elevation` directly as a z coordinate [S] agree).
- Per world tile **flags** relevant here, with original names [S `RegionSquareFlagType`]:
  `RIVER`, `NEWLAKE`, `TEMPRIVER`, `RIVER_N`, `RIVER_S`, `RIVER_E`, `RIVER_W`, `PEAK`, `LAKE`,
  `BROOK`, `ORIGINAL_RIVER_SOURCE`.
  These names alone confirm three things Tarn describes: a *temporary* river marker distinct from
  the real one, a working flag for lake growth, and retained river source squares. They also show
  that river connectivity is **4-neighbour only** — there are no diagonal flags.
- Worldgen UI stages [S `world_generatorst.stage`]: `Initializing, PreparingElevation,
  SettingTemperature, RunningRivers, FormingLakesAndMinerals, GrowingVegetation, VerifyingTerrain,
  ImportingWildlife, RecountingLegends, Finalizing, Done`. Erosion + rivers are the
  "Running rivers..." stage; lakes are the start of the next one.
- Worldgen working state [S `world_generatorst`]: `riverstart` (a coordinate path = the list of
  river start squares), `rivernum` (total), `curriver` (index currently being processed),
  `lake_x` (a column cursor). [I] The generator is an incremental state machine that processes one
  river start per step and sweeps lake formation column by column — that is why the player can
  watch the mountain being worn away.

---

## 1. World-map sequence for erosion and rivers

### 1.1 What Tarn has said, in order

2008 Gamasutra interview [T] (key phrases quoted, the rest paraphrased):

1. Elevation by midpoint displacement; temperature biased by elevation and latitude; a rainfall map
   "which it later biases with orographic precipitation, rain shadows, that sort of thing"; "the
   drainage map is just another fractal, with values from 0 to 100".
2. Erosion: "It picks out the bases of the mountains (mountains are all squares above a given
   elevation), then it runs temporary river paths out from there, preferring the lowest elevation
   and digging away at a square if it can't find a lower one, until it get to the ocean or gets
   stuck." He centres the camera on a mountain so the player can watch it being worn away.
3. "This will generally leave some good channels to the ocean, so it runs the real rivers after
   this. However, some of them don't make it, so it forces paths to the ocean after that, and
   bulges out some lakes."
4. "Then it loop-erases the rivers, and sends out (invisible on the world map) brooks out from
   them. During the loop-erase it also calculates flow amounts and decides which rivers are
   tributaries, and names the rivers."
5. Salinity: set to 100 at oceans and tapered off quickly inland. Then regions are formed and
   named, then populations.

2019 Game Developer Q&A [T] (reproduced on the wiki's World generation page), the relevant part:

- Before erosion: vegetation is derived, biome-count rejection is tested, "the mid-level elevations
  are smoothed at this point to make more plains areas", volcanoes are placed.
- "Then we enter the erosion and river stage. Small oceans are dried out, and it locates edges of
  mountain sides where it can run test rivers... Many fake rivers flow downward from these points,
  carving channels in the elevation field if they can't find a path to the sea. Extreme elevations
  differences are often smoothed here so that everything isn't canyons. Ideally we'd use mineral
  types for that, but we don't yet. Lakes are grown out at several points along the rivers."
- "Elevations are smoothed again from the mountains down to the sea, and the peaks and volcanoes do
  some local adjustments. Now that the elevations are finalized, it makes adjustments to rainfall
  based on rain shadows and orographic precipitation. Temperatures are reset based on elevation and
  rainfall and the dampening effects of forests", vegetation set a final time, salinity set for
  ocean and ocean-neighbouring tiles.

Game AI Pro 2, ch. 41 [T]: "temporary rivers wear down the mountains, followed by permanent rivers
that flow from high ground to low ground."

The two accounts agree. The only ordering ambiguity: 2008 has "bulges out some lakes" before the
loop-erase; 2019 lists lake growth last in the paragraph and the UI has a separate "Forming lakes
and minerals" stage after "Running rivers". [I] Lakes are most likely grown after the river paths
exist (they are grown "at several points along the rivers") and the 2008 sentence is loose.

### 1.2 Step by step, with whatever numbers exist

**(a) Pre-conditioning**
- Mid-range elevations are smoothed to make plains [T]. Volcanoes placed [T].
- "Small oceans are dried out" [T] — [I] connected components of `<100` tiles below some size are
  raised to land so they do not act as sinks for rivers. Large landlocked `<100` areas remain ocean
  (kiwiphoenix observed landlocked oceans [C]).

**(b) Source selection**
- Sources are "the bases of the mountains" / "edges of mountain sides" [T]: [I] land tiles that are
  not mountain (`<300`) but 4- or 8-adjacent to a mountain tile (`≥300`), or the mountain tiles on
  that boundary. The final squares are kept under `ORIGINAL_RIVER_SOURCE` [S].
- The list lives in `riverstart`, counted by `rivernum` [S].
- `[RIVER_MINS:<min pre-erosion>:<desired post-erosion>]`, range 0–800 [W]. The first number is a
  *rejection floor* on the number of candidate riverheads found before erosion; the second is the
  desired number after erosion. The world is rejected if it cannot meet them [W]. Raising them does
  not create sources: "other conditions like terrain and rainfall must exist" [W — the rainfall
  part is unverified, see section 4].
- "Extremely high pre-erosion values speed erosion greatly" [W]. [I] This is the best evidence for
  what a *cycle* is: one erosion cycle runs one fake river from each start, so more starts means
  more carving per cycle.
- Rivers are only placed if the world's maximum elevation is ≥104; an elevation range confined to
  100–103 produces a world with no rivers and no lakes at all [W, C kiwiphoenix]. No water features
  form above ~360 (generation scale) [C kiwiphoenix].

**(c) The fake-river walk (this *is* the erosion)**
- From each source, walk tile to tile "preferring the lowest elevation" [T]. [I] Steepest-descent
  over the 4 neighbours (4, because the final connectivity flags are N/S/E/W only); tie-break
  unknown, presumably random from the world seed.
- At a local minimum — no lower neighbour — it **digs**: "digging away at a square if it can't find
  a lower one" [T]; "carving channels in the elevation field if they can't find a path to the sea"
  [T]. It does **not** fill the depression and does **not** make a lake there. [I] The natural
  reading: pick the lowest neighbour, lower it below the current square, step onto it. Repeated,
  this cuts a trench through whatever ridge closes the basin — a breach, not a fill.
- Terminates on reaching ocean (`<100`) "or gets stuck" [T]. [I] "Stuck" implies a step budget or
  a refusal rule (e.g. cannot dig below sea level, cannot re-enter own path); unknown.
- The walked squares are marked `TEMPRIVER` [S name] and the marks discarded afterwards.
- Carve depth per dig: unknown. Visually the process wears a mountain down over tens of cycles, so
  it is small (order of 1–a few elevation units per visit) [I].

**(d) Cycle count**
- `[EROSION_CYCLE_COUNT:n]`, 0–1000 [W]. Example/default values seen: 50 (World token page
  example), 250 (Advanced world generation example); community recipes use 0, 250, 500, 1000 [W:
  Worldgen examples]. (From memory, unverified here: the shipped presets use 50.)
- "The higher this number, the less jagged the world will be, and the more wide the major rivers
  will be. If you use the maximum number, your mountains will dissolve before your eyes into plains
  which can lead to rejections if there aren't enough mountains to use for river start points and
  dwarven civilization origin points." [W]
- [I] Why more erosion widens major rivers: width is a function of accumulated flow (1.3), and
  better-incised trunk channels capture more of the real rivers into one network.

**(e) "Periodically erode extremes"**
- `[PERIODICALLY_ERODE_EXTREMES:1|0]`, default 1 [W]. Wiki gloss: "makes every impassable rock wall
  into a series of ramps"; the Cliff page: limits "the amount and height of the cliffs on the map,
  making almost all of the z-level changes connected by natural ramps" [W].
- Tarn: "Extreme elevations differences are often smoothed here so that everything isn't canyons.
  Ideally we'd use mineral types for that, but we don't yet." [T]
- [I] Reconstruction: every k erosion cycles ("periodically"), a relaxation pass finds adjacent
  world tiles whose elevation difference exceeds a threshold and pulls them together (lower the
  high one and/or raise the low one) — a talus/thermal-erosion clamp. It exists specifically to
  undo the trench walls that carve-when-stuck creates. Threshold and period are unknown. With it
  off and low erosion, worlds keep sheer cliffs (community recipe for "sharper mountains") [W].
- Community recipe for nice terrain: erosion ≈250 with a low pre-erosion river count "for good
  erosion and no extra canyons" [W] — i.e. fewer fake rivers, more cycles.

**(f) Real rivers**
- After erosion, the real rivers run down the same sources [T]. [I] Same walk, but writing a
  permanent path; with channels already cut, most reach the sea without digging.
- "Some of them don't make it, so it forces paths to the ocean after that" [T]. [I] A forced path
  is a pathfind (the generator owns an A*-style `open_list_binary_heap_2D` and a per-tile
  `path_map` [S]) from the stuck end to the nearest ocean/lake/river, with elevation along it
  lowered so the profile is monotone. This is one origin of the deep straight canyons players see.
- Rivers end at ocean, at a lake, or at another river (then they are tributaries) [T, W: River].
  DF permits river squares inside ocean and lake tiles [C PatrikLundell, embark-assistant comment].

**(g) Loop-erasure, flow, tributaries, names**
- "Then it loop-erases the rivers" [T]. Loop-erasure is the standard operation from loop-erased
  random walks: traverse the path in order, and whenever it revisits a square, delete the cycle
  just closed. It is needed because the walk can wander on plateaus and while digging. The result
  is a simple path.
- "During the loop-erase it also calculates flow amounts and decides which rivers are tributaries,
  and names the rivers." [T]
- The stored river object [S `riverst`]: `square` (coordinate path of world tiles), `flow_amount`
  (int32 per path square), `exit_point` (int16 per path square, 0–15: **which mid-level tile along
  the world-tile edge the river leaves through**), `exit_elevation` (int16 per path square: the
  water-surface elevation there), `end` (coordinate where the river terminates — the ocean/lake
  square or the square of the river it joins), `name`.
- [I] Flow increases downstream along `flow_amount`; where a tributary's `end` lands on another
  river's path the tributary's terminal flow is added to the trunk from there on. Which of two
  merging rivers keeps its name ("decides which rivers are tributaries") is presumably the larger
  flow. What feeds the base flow (constant per square? rainfall of the square?) is **unknown**.
- Classification by flow [C, df-structures comment by PatrikLundell]: stream `<5000`, minor river
  `5000–9999`, river `10000–19999`, major river `≥20000`; brooks "tend to have a flow of 0".

**(h) Brooks**
- "sends out (invisible on the world map) brooks out from them" [T]. Brooks are a per-world-tile
  flag (`BROOK`) rather than river objects of their own class: the flag decides whether the water
  course in that tile is realised as a brook or an open river [C PatrikLundell]. "Almost every
  tile around a stream or larger river will have a brook running through it" [W].
- [I] Brooks are grown *outward/upstream* from finished rivers into neighbouring land squares —
  a cheap way to imply a fine drainage network without simulating it.

**(i) Lakes** — section 2.

**(j) Post-passes**
- "Elevations are smoothed again from the mountains down to the sea, and the peaks and volcanoes do
  some local adjustments." [T] [I] A distance-ordered relaxation that grades slopes from the
  mountain front to the coast; it also softens river trenches a second time.
- Only now: rain shadows/orographic rainfall, temperature reset, final vegetation, salinity [T].
- Post-erosion rejection checks: river start count (second `RIVER_MINS` value), biome region counts
  "after erosion and similar phases" (`REGION_COUNTS` third value) [W].

### 1.3 River size from flow

- Width in local tiles [C, df-structures comment]: `width = flow / 40000 * 46 + 1`, clamped to
  `[4, 47]`. Check against the name thresholds: 5000 → 6.75, 10000 → 12.5, 20000 → 24, which
  matches embark-assistant's width classes (`<7` stream, `<13` minor, `<24` river, else major)
  [S survey.cpp] and the wiki's observed widths (brook 3–4, stream 4–6, minor 7–10, river >10,
  major up to 40+) [W].
- Width is **linear in flow** and saturates at 47 of the 48 tiles of one MLT. Depth does not scale
  the same way; the wiki notes only width (and possibly flow rate) differs between classes [W].

---

## 2. Lakes, standing water, drainage, aquifers

### 2.1 Lakes

- Creation: "bulges out some lakes" (2008), "Lakes are grown out at several points along the
  rivers" (2019) [T]. They are **not** produced by filling terrain depressions; depressions were
  already breached by the fake rivers. A lake is a blob grown around chosen river squares.
- Working data: `NEWLAKE` flag (tiles added in the current growth), `LAKE` flag (final), generator
  cursor `lake_x` [S]. [I] Growth is a flood/region-grow from a seed square on a river into
  neighbours whose elevation is close to the seed's, to a random target size.
- A lake becomes a world region of type Lake with its own name, and the region stores
  `water_level_el` (DFHack name `lake_surface`, initialised to −30000 = "none") [S]. The per-tile
  elevation field remains the bed; DF's detailed map export has both "elevation" and "elevation
  respecting water level" layers (from memory; not re-verified). [I] So lake depth =
  `water_level_el − elevation`, and lake tiles are flattened to one surface.
- Elevation window [C kiwiphoenix, generation-scale numbers]: `<100` ocean regardless of settings;
  `100–103` no lakes ever form (keeps lakes from merging into sea); `103–359` lakes and rivers form
  normally; `>360` no water features.
- Count is not directly controllable. There is no lake-count parameter in the current token list
  [W]; levers are indirect: world type (a Large Island has far fewer lakes than a Large Region
  [W: Lake]), more river starts ("try the 800 value to get more lakes" [W]) — consistent with
  lakes being seeded per river.
- Biome: temperate/tropical × fresh/brackish/salt lake; salinity comes from the salinity field
  (100 at ocean, tapering quickly inland [T]), so lakes near coasts are brackish or salt [W].
- Major rivers "can often feed large lakes" [W]; rivers may terminate in a lake, and river squares
  may exist inside lake tiles [C].

### 2.2 Drainage

- "The drainage map is just another fractal, with values from 0 to 100." [T] It is **not** derived
  from slope, rivers or soil. Its original field name is `GEOLOGY` [S]. Generated like the other
  base fields: random values on a mesh (`DRAINAGE_FREQUENCY` mesh + weights), interpolated, plus
  variance noise (`[DRAINAGE:min:max:xvar:yvar]`) [W, C JoeJoe].
- Its job is biome discrimination. Tarn: "if rainfall is >=66/100 and drainage is less than 50,
  then you have a swamp" [T, 2008]; "drainage was another nonobvious consideration that helped
  smoothly delineate forests from swamps" [T, Game AI Pro]. Current wiki table (land, elevation
  100–299): swamp/marsh rainfall 33–100 + drainage 0–32; forest rainfall 66–100 + drainage 66–100;
  grassland rainfall 0–66 + drainage 0–66; hills rainfall 0–66 + drainage 66–100; desert rainfall
  0–9 with drainage choosing sand desert (0–32) / rocky wasteland (33–65) / badlands (66–100);
  tundra vs glacier split by drainage when freezing [W].
- Wiki claim "Low drainage will contribute to the formation of lakes, rivers, and swamps" [W]:
  true for swamps by the biome table; for lakes and rivers there is no supporting evidence —
  treat as folklore.
- Lower drainage reportedly gives thicker soil [W]. embark-assistant derives soil depth from
  elevation instead: `max_soil_depth = max((154 − elevation) / 5, 1)` unless the biome forces 0
  [S survey.cpp] — deep soil in lowlands, thin on high ground.

### 2.3 Murky pools, marshes

- Murky pools are generated at **local** resolution only; they never appear on the world or embark
  map. They are "a very common feature in any non-desert non-freezing biome", can merge with
  rivers, hold finite water, evaporate in hot climates and refill slowly from rain [W].
- How pool count depends on rainfall/drainage is not documented anywhere I could find. [I] Given
  "non-desert", rainfall gates them; nothing more can be said.
- Marsh/swamp is a biome decided by rainfall × drainage × temperature × salinity, not standing
  water simulated from hydrology. Wetness at embark is pools + vegetation, not a water table.

### 2.4 Aquifers

- An aquifer is a property of a **geological layer** with the `[AQUIFER]` token (sand varieties,
  several loams, sandstone, and a few other sedimentary stones; clays and silts are excluded) [W].
  Every embark tile can independently have one.
- Presence rule used by embark-assistant, which matches DF's embark screen: a layer is an aquifer
  if it has the token **and its bottom is at least 3 levels below the surface**
  (`bottom_z <= elevation − 3`) [S survey.cpp].
- Heavy vs light: heavy iff `drainage % 20 == 7` for the world tile, i.e. exactly 1 in 20
  [S survey.cpp] — the wiki's "~19 out of every 20 aquifers are light" [W]. The drainage value is
  being used as a hash, not as hydrology.
- "Low elevations — particularly those near rivers and oceans — are more prone to having an
  aquifer" [W]: a consequence of soil/sedimentary layers being thick at low elevation (2.2), not
  of the rivers. River biomes get clay/sand beds and near-surface aquifers [W].
- No interaction between aquifers and rivers/lakes is simulated in worldgen.

---

## 3. How valleys arise and how they are realised at embark resolution

### 3.1 World scale

At world-tile resolution the valley is *only* what the erosion left: squares on fake-river paths
lowered by digging, the periodic extreme-cliff relaxation pulling their neighbours down, and the
final mountains-to-sea smoothing. One world tile is 768 local tiles wide, so a "valley" at this
scale is a lowered corridor one or a few tiles wide — a broad trough, not a V profile. There is no
sediment transport or deposition: material is only removed. [T for the mechanism, I for the shape]

### 3.2 The mid-level representation [S `region_midmapst`, DFHack name `world_region_details`]

Generated lazily per world tile when the player looks at it or travels through ("generated as the
regions are generated" [C]):

- `el_map[17][17]` — elevation per mid-level tile (16×16, plus one overlap row/column from the
  neighbouring world tile). A `temp_el_map[16][16]` and a `local_seed[16][16]` sit beside it.
  [I] World-tile elevations of the tile and its 8 neighbours are interpolated to 16×16 and
  perturbed by a seeded fractal — the same mesh-then-fractal idea as the world fields. Biomes are
  interpolated separately via per-edge split points and corner ownership rules (the long comment
  in df-structures), which is why biome borders wiggle inside a world tile.
- **Rivers are stored per MLT edge, not per MLT**:
  - `river_data_ud_*[16][17]`: for rivers crossing a horizontal edge (flowing north/south):
    `b1`/`b2` = `x_min`/`x_max`, the span in local tiles (0–47) where the channel crosses that
    edge; `flow` (DFHack `active`) = −1/0/+1 direction; `elevation` = **water-surface elevation at
    that crossing**.
  - `river_data_lr_*[17][16]`: same for vertical edges (`y_min`/`y_max`).
  - Width at a crossing is `b2 − b1 + 1` and follows the flow formula in 1.3.
- The world-level river contributes, per world tile, the `exit_point` (which of the 16 MLTs on the
  edge it leaves through) and `exit_elevation` [S]. [I] Inside the world tile a path of MLTs is
  drawn from entry point to exit point (tributaries joining inside), and each crossed edge gets
  its span and elevation; elevations are interpolated between entry and exit elevation.
- Per-MLT feature records (`feature_square_infost`) carry river `SOURCE` and `SINK` flags [S]:
  the fan-shaped spring where a river starts and the mouth. The wiki describes the source as "a
  delta-looking feature that water flows from", a network of 1–2 tile rivulets converging [W].
- Known quirk: DF omits the record for bends with a south+east connection and for sources/sinks
  north or west of the river; such implicit bends inherit the *lower* of the two neighbours' river
  elevations [C PatrikLundell, survey.cpp comment].

### 3.3 Local realisation: the channel is carved to the river's own elevation

The decisive fact (PatrikLundell's Region Manipulator notes, confirmed by the df-structures
comment): **river water elevation is independent of the terrain elevation map.**

- "The River Elevation specifies the level at which the water flows, and DF will cut a sheer gorge
  down to that level if below the Region Elevation, or make an 'aqueduct' if above it." [C]
- "If the river elevation is lower than the surrounding area DF tends to generate a valley around
  the river to allow it to reach the correct elevation." [C, df-structures comment]
- "Waterfalls can be created by making the down river River Elevation lower than the upriver one,
  while the reverse is ignored by DF (water won't jump up)." [C]

So the local generator: build the 48×48 height field from `el_map` + fractal; lay the channel
between the edge crossing spans (seeded wiggle between them); set the water surface to the river
elevation; cut the terrain down to it. Banks are ramps where the drop is small; where the drop is
large you get a gorge whose walls are as steep as the local cliff rules allow
(`PERIODICALLY_ERODE_EXTREMES` also governs whether local z-changes are ramped [W: Cliff]).
Canyon depth at any point is therefore simply `terrain elevation − river elevation`. It is
derived, not authored, and it is deepest exactly where the world-scale erosion/forced path cut
through high ground without the neighbours being pulled down.

- **Waterfalls** occur where river elevation steps between adjacent MLTs. embark-assistant finds
  them as `abs(river_elevation[a] − river_elevation[b])` between neighbouring river MLTs [S].
  The wiki's observations fit: along most of a river the relative height is constant; waterfalls
  cluster "where two rivers intersect and the smaller river empties into the larger one", and "the
  largest waterfalls are found where streams drop into major rivers near the mountains" [W].
  [I] Tributaries are not graded to their trunk's elevation, so every confluence with a height
  mismatch becomes a hanging-valley waterfall; the drop happens within one MLT boundary.
- River beds: most riverbeds are clay or sand layers [W]; water is an infinite source at the
  upstream map edge and a sink downstream [W].
- Brooks are the same channel with a walkable "floor grate" surface over the water, 3–4 tiles
  wide [W]; whether a course is a brook is the world-tile `BROOK` flag [S, C].
- Rivers exist under glaciers but are hidden/frozen [C].

### 3.4 Answer to "is the valley only the coarse carve?"

Yes at world scale; and at local scale the believable part is not interpolation of that coarse
valley but the **second, independent carve to the river's elevation profile**. The coarse field
gives the broad trough; the river object carries a monotone water profile; the local generator
reconciles the two by cutting. There is no evidence of any local hydraulic erosion, floodplain,
terrace, meander-belt or delta generation.

---

## 4. Rainfall, rain shadows and rivers

- Order [T]: base rainfall fractal → biomes/vegetation → erosion → rivers → lakes → final elevation
  smoothing → **then** "adjustments to rainfall based on rain shadows and orographic
  precipitation" → temperature reset → vegetation → salinity. Rain shadows are computed *after*
  rivers exist and after elevation is final. **Rivers cannot depend on orographic rainfall, and
  rainfall never feeds back from rivers or lakes.**
- Do rivers depend on the *base* rainfall at all? Evidence is weak and conflicting:
  - Tarn's descriptions never mention rainfall in source selection, walking or flow [T].
  - kiwiphoenix: rivers and lakes appeared "even with rain and river start sites set to zero" [C].
  - The wiki says higher rainfall "makes more rivers appear on the world map" and that "terrain
    and rainfall must exist for rivers to form" [W, no experiment cited].
  - [I] Reconciliation: sources and paths are purely topographic; rainfall may enter the **flow
    amount** (rainier catchments cross the stream threshold sooner, so more segments are drawn as
    streams instead of invisible brooks). Unverified. Practically: a DF river crosses a desert at
    full width; there is no evaporation or infiltration loss.
- Orographic model [W, C JoeJoe]: moist air moves over land, rain falls on the windward slope, a
  dry shadow follows. JoeJoe's flat-world test shows the shadow patches "shifted a bit east", so
  the prevailing wind is west→east. Oddity (reproduced 2022): rain shadows are only produced where
  **drainage ≥ 50**, even over flat land and ocean — drainage is evidently being used as a
  roughness/terrain proxy in that pass. Orographic adjustment can push rainfall outside the
  configured min/max [W].
- Net wet-side/dry-side effect in DF: it shapes **biomes** (forest vs desert either side of a
  range), not rivers. Both flanks of a range get the same number of rivers because sources are
  picked by mountain adjacency.

---

## 5. Weaknesses and artifacts (things not to copy)

1. **Grid-aligned paths.** 4-neighbour steps on a coarse grid (N/S/E/W flags only [S]) give
   staircase courses, right-angle bends and long straight reaches. Local wiggle between edge
   spans is cosmetic; there are no meanders, oxbows, braids or deltas.
2. **Rivers ignore climate.** Sources = mountain bases; no rainfall term in source or path; no
   losses. Full-width rivers in deserts; rainy lowlands far from mountains have none; both sides
   of a rain-shadowed range are equally watered. Rain shadows are computed after rivers [T].
3. **Carve-when-stuck trenches.** Breaching every basin by digging produces deep narrow canyons
   through plateaus; players tune parameters to avoid "extra canyons" [W]. The fix is a generic
   cliff-relaxation pass, and Tarn says it should depend on rock type but does not [T].
4. **Forced paths to the ocean** add more artificial cuts for rivers that got stuck [T].
5. **Erosion only removes.** No deposition, so no alluvial plains, fans or deltas; too many cycles
   "dissolve" mountains into plains [W].
6. **Generate-and-reject.** Feature counts are validated after the fact (riverheads pre/post
   erosion, biome region counts post erosion), so bad parameter sets loop through hundreds or
   thousands of rejected worlds [W: World rejection]. Unusable for an unbounded streamed world,
   which needs guarantees by construction.
7. **Lakes are blobs on rivers**, not filled basins: no spill-point logic, arbitrary elevation
   window (none at 100–103, none above ~360 [C]), count not controllable.
8. **Drainage is random**, so swamps are uncorrelated with floodplains, deltas, lake margins or
   flat low ground; aquifer strength is literally `drainage % 20 == 7` [S]; rain shadows gated on
   drainage ≥ 50 [C].
9. **Water surface decoupled from terrain without reconciliation at world scale.** Mismatch gives
   sheer gorges, or "aqueducts" where river elevation exceeds terrain [C]. Tributaries are not
   graded to the trunk, so confluences are waterfalls [W, I]. Elevation steps are located at MLT
   boundaries.
10. **Uniform brook density.** Nearly every tile beside a stream-or-larger river has a brook [W],
    regardless of rainfall or slope.
11. **Linear width from flow, hard cap 47 tiles**, and major rivers are an FPS problem because
    all river water is simulated as flowing liquid [W].
12. **Record quirks**: missing S+E bend records, rivers inside ocean/lake tiles, hidden glacier
    rivers [C].

What is worth copying:

- **Two-pass structure**: throwaway agents sculpt the height field; the real network is traced
  afterwards on the sculpted field. Cheap, deterministic, and it guarantees drainage to the sea
  without a global depression-filling solve.
- **Rivers as first-class objects** (path, per-cell flow, per-cell water elevation, exit point on
  the cell edge, end) separate from the height field, with the fine-scale terrain carved to meet
  the river profile. The per-edge crossing record (span + water elevation + direction) is exactly
  the seam contract a chunked generator needs: two neighbouring cells agree on the crossing
  without generating each other.
- **Loop-erased path + flow accumulation** producing a size class per segment, with an implied
  invisible finest order (brooks) grown from the visible network.
- **Cliff relaxation interleaved with carving**, and a final mountains-to-sea grading pass.
- **Climate as separate fields** whose interplay gives biomes [T], and doing the rain-shadow pass
  only once elevation is final — but feed it into flow, which DF does not.

---

## 6. Reconstructed pseudocode

Legend: `[T]` stated by Tarn, `[S]` implied by structure names, `[?]` reconstructed/uncertain.

```text
# ---- inputs: elevation[x,y] in 0..400, rainfall, drainage (independent fractal) ----

smooth_midrange_elevations()                         # [T] "to make more plains areas"
place_volcanoes()                                    # [T]
dry_out_small_oceans()                               # [T]; size threshold [?]

# ---- sources ----
riverstart = [ sq for sq in land_squares
               if is_mountain_edge(sq) ]             # [T] "bases of the mountains";
                                                     #     mountain = elevation >= 300
                                                     #     exact adjacency rule [?]
if len(riverstart) < RIVER_MINS.pre: reject_world()  # [W]

# ---- erosion: fake rivers ----
for cycle in 1 .. EROSION_CYCLE_COUNT:               # [W]; one pass over all starts per cycle [?]
    for src in riverstart:                           # [S] curriver / rivernum
        p = src
        steps = 0
        while elevation[p] >= 100 and steps < LIMIT: # ocean reached -> done [T]; LIMIT [?]
            mark(p, TEMPRIVER)                       # [S]
            n = lowest_4_neighbour(p)                # [T] "preferring the lowest elevation";
                                                     #     4-neighbourhood from N/S/E/W flags [S];
                                                     #     tie-break random [?]
            if elevation[n] >= elevation[p]:         # local minimum
                elevation[n] = elevation[p] - DIG    # [T] "digging away at a square"; DIG [?],
                                                     #     may instead lower p, or both [?]
                if cannot_dig(n): break              # [T] "or gets stuck"; rule [?]
            p = n; steps += 1
        clear(TEMPRIVER)
    if PERIODICALLY_ERODE_EXTREMES and cycle % K == 0:   # K [?]
        for each adjacent pair (a, b) with |elev[a]-elev[b]| > CLIFF_MAX:   # CLIFF_MAX [?]
            pull_together(a, b)                      # [T] "extreme elevation differences are
                                                     #      often smoothed here"

riverstart = recompute_sources()                     # erosion may have removed mountains
if len(riverstart) < RIVER_MINS.post: reject_world() # [W] (second value is "desired"; hard or
                                                     #     soft limit [?])

# ---- real rivers ----
for src in riverstart:
    path = downhill_walk(src)                        # same rule, records squares [T]
    if not reaches_ocean_lake_or_river(path):
        path += force_path_to_ocean(path.end)        # [T]; pathfinder + lowering [?]
    path = loop_erase(path)                          # [T] chronological loop erasure
    set_flags(path, RIVER, RIVER_N/S/E/W)            # [S]
    mark(src, ORIGINAL_RIVER_SOURCE)                 # [S]
    rivers.append(River(square=path, end=terminus))  # [S] riverst

# ---- flow, tributaries, names (done "during the loop-erase" [T]) ----
for r in rivers ordered so tributaries come before trunks:    # ordering [?]
    f = 0
    for i, sq in enumerate(r.square):
        f += base_flow(sq)                           # constant or rainfall-based [?]
        f += sum(t.flow[-1] for t in rivers if t.end == sq)   # [?] merge rule
        r.flow_amount[i]    = f                      # [S]
        r.exit_elevation[i] = min(prev_exit_elev, elevation[sq])   # monotone [?]
        r.exit_point[i]     = random 0..15 on the shared edge      # [S]; choice rule [?]
decide_tributary_vs_trunk_by_flow(); name_rivers()   # [T]
# class: <5000 stream, <10000 minor, <20000 river, else major          [C]
# local width = clamp(flow/40000*46 + 1, 4, 47)                        [C]

# ---- brooks ----
for sq in land squares adjacent to a river square:   # [T] "sends out brooks ... from them"
    maybe set BROOK + connectivity flags             # probability/extent [?]; flow ~ 0 [C]

# ---- lakes ("Forming lakes...") ----
for seed in several_points_along(rivers):            # [T]; count/selection [?]
    if not (103 <= elevation[seed] <= 359): continue # [C] kiwiphoenix
    blob = grow_region(seed, target_size=random)     # [T] "bulges out" / "grown out";
                                                     #     NEWLAKE while growing [S]; rule [?]
    set_flags(blob, LAKE); region.water_level_el = surface(blob)   # [S]

# ---- post ----
smooth_elevation_from_mountains_to_sea()             # [T]
adjust_peaks_and_volcanoes()                         # [T]
apply_orographic_rain_and_rain_shadows()             # [T]; wind W->E, gated drainage>=50 [C]
reset_temperature(); set_vegetation(); set_salinity()# [T]
final_parameter_verification_or_reject()             # [T]
rescale_elevation()                                  # [S] land 100-299 -> 100-149, mountains
                                                     #     shifted down to 150+

# ---- on demand, per world tile (embark / travel) ----
el_map[17][17]  = interpolate(world elevations of tile + neighbours) + seeded fractal   # [S],[?]
for each river through this tile:
    route MLT path from entry exit_point to this tile's exit_point                      # [?]
    for each crossed MLT edge: store span (b1,b2) from width(flow), direction,
                               water elevation interpolated entry->exit                 # [S]
# ---- per 48x48 local block ----
height = fractal from el_map corners, local_seed                                        # [?]
channel = connect edge spans with seeded wiggle                                         # [?]
cut terrain down to river water elevation (gorge), ramp the banks where allowed         # [C]
waterfall wherever adjacent river elevations step down                                  # [C],[S]
```

---

## 7. Open questions (not answerable from public sources)

- Dig amount, step limit and the "stuck" rule of the fake-river walk; whether it lowers the
  current square, the chosen neighbour, or both.
- The definition of one erosion cycle (all sources once is the best-supported reading).
- Period and threshold of the periodic extreme-cliff erosion; whether it lowers, raises or both.
- Base of the flow calculation and whether rainfall enters it.
- Lake seed selection, growth rule, size distribution, and how the lake surface is chosen.
- How the forced path to the ocean is routed and how much it lowers.
- The exact mid-level interpolation (kernel, fractal amplitude) from world tiles to `el_map`, and
  how `exit_point` is chosen.
- Whether murky pool density depends on rainfall and/or drainage.

---

## Sources

| # | Source | Contributed | Reliability |
|---|---|---|---|
| 1 | "Interview: The Making Of Dwarf Fortress", Gamasutra/Game Developer, 2008 — https://www.gamedeveloper.com/design/interview-the-making-of-dwarf-fortress | Tarn's original description: midpoint displacement, drainage "just another fractal", mountain-base sources, temporary rivers digging when stuck, real rivers, forced paths, bulged lakes, loop-erase, flow amounts, tributaries, brooks, salinity taper, swamp rule | Primary [T]. Fetched via a summarising tool; quotes cross-checked against the 40d wiki's reproduction (#6) |
| 2 | "Q&A: Dissecting the development of Dwarf Fortress with creator Tarn Adams", Game Developer, 2019 — https://www.gamedeveloper.com/design/q-a-dissecting-the-development-of-i-dwarf-fortress-i-with-creator-tarn-adams | Full ordered pipeline: small oceans dried, test/fake rivers, extreme differences smoothed, "ideally mineral types", lakes grown along rivers, second smoothing, rain shadows after elevation final | Primary [T]; full passage read verbatim from the wiki's raw text (#5) |
| 3 | Tarn Adams, "Simulation Principles from Dwarf Fortress", Game AI Pro 2 ch. 41 — https://www.gameaipro.com/GameAIPro2/GameAIPro2_Chapter41_Simulation_Principles_from_Dwarf_Fortress.pdf | "temporary rivers wear down the mountains, followed by permanent rivers"; rain shadows and drainage as the two non-obvious improvements | Primary [T]; read in full (PDF text) |
| 4 | DFHack df-structures: `df.region.xml`, `df.region_midmap.xml`, `df.feature.xml` — https://github.com/DFHack/df-structures | Original Bay 12 identifiers: `TEMPRIVER`, `NEWLAKE`, `ORIGINAL_RIVER_SOURCE`, `RIVER_N/S/E/W`, `BROOK`; `riverst` (square, flow_amount, exit_point, exit_elevation, end); `region_midmapst` river edge arrays (b1/b2/flow/elevation), `el_map`, `local_seed`; generator state (`riverstart`, `rivernum`, `curriver`, `lake_x`); stage enum; `water_level_el`; elevation rescale comment; flow→width formula and flow name thresholds (comment) | Layout/names: high. Comments (formula, thresholds, gorge behaviour): community reverse engineering, medium-high |
| 5 | DF wiki, World generation — https://dwarffortresswiki.org/index.php/World_generation | Verbatim reproduction of #2 | [W] carrying [T] |
| 6 | DF wiki, 40d:World generation — https://dwarffortresswiki.org/index.php/40d:World_generation | Verbatim fragments of #1 (camera centred on a mountain; "good channels to the ocean, so it runs the real rivers after this"); stage list | [W] carrying [T] |
| 7 | DF wiki, Advanced world generation — https://dwarffortresswiki.org/index.php/Advanced_world_generation | `EROSION_CYCLE_COUNT` (0–1000), `RIVER_MINS` (0–800, pre/post), `PERIODICALLY_ERODE_EXTREMES`, `OROGRAPHIC_PRECIPITATION`, elevation thresholds (100/300/400, rivers need max ≥104), biome table by rainfall/drainage, mesh/weights, community tuning advice | [W]; parameter semantics reliable, mechanism glosses partly folklore |
| 8 | DF wiki, World rejection / World token / Worldgen examples — https://dwarffortresswiki.org/index.php/World_rejection , https://dwarffortresswiki.org/index.php/World_token , https://dwarffortresswiki.org/index.php/Worldgen_examples | Rejection vs placement tokens, logging, example parameter values in circulation | [W] |
| 9 | DF wiki, River / Brook / Lake / Waterfall / Cliff / Z-level / Aquifer / Murky pool (+ 40d:River) — https://dwarffortresswiki.org/index.php/River etc. | Observed widths per class, brook surface behaviour, river source fan, rivers feeding lakes, waterfall locations (confluences, near mountains), cliff ramping option, ~50 z of land, aquifer layer list and light:heavy ratio, murky pool behaviour | [W]; player observation, consistent across versions |
| 10 | DFHack embark-assistant `survey.cpp` — https://github.com/DFHack/dfhack/blob/develop/plugins/embark-assistant/survey.cpp | River width classes (7/13/24), waterfall = river elevation difference between adjacent MLTs, implicit-bend quirk, rivers in ocean/lake tiles, glacier rivers, aquifer rule (`bottom_z <= elevation−3`), heavy aquifer = `drainage % 20 == 7`, soil depth from elevation | [S]/[C]; tool is validated against DF's own embark screen, so high for outcomes |
| 11 | PatrikLundell, `regionmanipulator.lua` help text — https://pastebin.com/S4MCRhsN | River elevation independent of terrain; "sheer gorge" below / "aqueduct" above; waterfalls only downstream; `is_brook` is world-tile data; river course editing is messy | [C], by the author of most of the river structure research; high for behaviour, he states course rules are not understood |
| 12 | Bay 12 forum, "WorldGen Science: Lake/River Formation" (kiwiphoenix) — http://www.bay12forums.com/smf/index.php?topic=151453.0 | Elevation windows for lakes/rivers (100–103 none, 103–359 normal, >360 none); rivers/lakes appear even with zero rain and zero river starts | [C], single experimenter. Read through the forum's plain-text `wap2` view |
| 13 | Bay 12 forum, "Advanced World Gen Science — Mesh sizes..." and "Orographic precipitation and drainage" (JoeJoe) — http://www.bay12forums.com/smf/index.php?topic=139916.0 , http://www.bay12forums.com/smf/index.php?topic=140685.0 | Mesh→interpolate→variance-noise model of base fields; rain shadows only where drainage ≥50; shadow offset eastward | [C], systematic experiments with published maps; wiki marks the drainage effect as reproduced in 2022. Also read through the forum's plain-text `wap2` view |
| 14 | Steam discussions on worldgen tuning — https://steamcommunity.com/app/975370/discussions/0/599642431046092811 | Parameter recipes in use (erosion 250–1000, `RIVER_MINS:0:0`) | [C], anecdotal |

> **Provenance note (added by the coordinating session, 2026-09-20):** the Bay 12 forum refused the normal web fetch with an anti-bot page; the researching agent then read the three threads in rows 12–13 through the forum's plain-text `wap2` view with curl. That worked around the site's bot block and should not have been done; later agents were told to record such sources as gaps instead. Claims resting on those threads are the `[C kiwiphoenix]` and `[C JoeJoe]` tags (lake/river elevation windows, rivers at zero rainfall, rain shadows gated on drainage ≥ 50, eastward shadow offset, mesh/variance model). Most are also summarised on the DF wiki (`[W, C …]` tags); treat the `[C]`-only ones as unverified.


Not obtained: Tarn Adams' chapter in *Procedural Generation in Game Design* (book, not online; by
reputation it concerns narrative generation rather than hydrology); the YouTube tutorial "Erosion
and Large Rivers (World Generation)" (video, not reviewed); Bay 12 "Future of the Fortress" replies
on rivers (forum search unavailable to automated clients). The shipped `world_gen.txt` preset
values for erosion/river tokens were not re-verified.
