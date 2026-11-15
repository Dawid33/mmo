# Minecraft → brick world conversion — research

Date: 2026-11-11. Status: research, nothing built. Dawid decided the five points in §10 on 2026-11-11;
other recommendations marked **[call]** are Claude's and open to change. Facts about our format were read from the code on this date; facts about Minecraft
come from minecraft.wiki, format specs and crates.io, and the ones that could not be confirmed from a
source are listed in §11.

Scope: turning a portion of a Minecraft world (or a schematic file) into our stud/brick format. A
separate plan builds the prefab system and in-game prefab editor; this document assumes it exists and
says what the converter needs from it (§7). How a prefab is stamped into a world, edited, or placed by
worldgen is that plan's business.

## 1. Summary

- **It fits well.** One Minecraft block is 2 × 2 studs wide, the same footprint as our sim tile. The
  only real geometry decision is height, and the answer is **1 block = 1 tile = 6 plates** (§3).
- **Convert to boxes, not bricks.** Bricks may not cross a chunk edge, so a brick list is only valid at
  one placement. The converter should emit material boxes plus a short list of shaped parts, and bricks
  get laid when the prefab is stamped, with the `merge_boxes` → `fill_box` code trees already use (§6, §7).
- **Mapping is `base material × form`.** A hand-written table of roughly 150 base materials, plus form
  rules driven by the block name suffix and block-state properties, covers the ~1,300 blocks and
  ~36,000 block states without a per-state table (§4, §5).
- **Read schematics first, worlds second.** Sponge `.schem` is the interchange format; `.litematic`,
  vanilla structure `.nbt` and a bounding box in a world folder follow. All are palette-of-block-states
  and normalise into one in-memory volume (§2).
- **Write our own reader on `fastnbt`.** The schematic crates are GPL, and Minecraft changed its chunk
  format three times in 2026, after the last `fastanvil` release (§8).
- **Our material list is the bottleneck, not the conversion.** We have 25 natural materials and 322
  plastic colours; there is no glass, planks, brick or cobblestone, no doors or ladders, and transparent
  colours draw opaque. Imports will look like colour-matched plastic until materials are added (§4).
- **Legal:** a tool that reads the user's files and ships no Mojang assets is the established pattern;
  shipping someone else's converted build needs that author's permission (§9).

## 2. Input: what "a portion of a Minecraft world" is

### Sources, in priority order **[call]**

| # | Source | How a user produces it | Notes |
|---|---|---|---|
| 1 | Sponge schematic `.schem` (v2, v3) | WorldEdit: `//wand`, `//copy`, `//schem save name` | Dense, full block-state strings, no size limit, written by many tools. |
| 2 | Litematica `.litematic` | Litematica area selection → save | Popular with builders; several named sub-regions per file. |
| 3 | Vanilla structure `.nbt` | Structure block in Save mode | Sparse block list; 48³ limit per file. |
| 4 | World folder + dimension + box | Nothing to export: `--world <dir> --from x y z --to x y z` | Needs the Anvil reader and the version handling below. |
| – | Bedrock worlds | Convert with Chunker (MIT, has a CLI) first | No native reader in v1; the Rust Bedrock crates are young. |
| – | Legacy MCEdit `.schematic`, pre-1.13 worlds | Open in a newer version, or re-save with WorldEdit | Numeric block ids; needs a pre-flattening table. Out of scope for v1. |

All four normalise to one structure, and everything after §2 only sees this:

```rust
struct BlockVolume {
    size: [u32; 3],                       // blocks, x y z
    palette: Vec<BlockState>,             // { name: "minecraft:oak_stairs", props: BTreeMap<String, String> }
    blocks: Vec<u32>,                     // palette index, y-major
    block_entities: Vec<([u32; 3], String)>, // position + id (chest, sign …); payload kept raw
    data_version: i32,
    source: SourceInfo,                   // file name, format, author / name fields if the file has them
}
```

### Format facts the reader must get right

- **Region files:** 4 KiB sectors, 1024 big-endian location entries, chunk payload = length + one
  compression byte (1 gzip, 2 zlib, 3 none, 4 LZ4; +128 means the data is in an external `.mcc` file).
- **Chunk sections (1.18+):** `sections[].block_states { palette, data }`; block index
  `y*256 + z*16 + x`; bits per index `max(4, ceil(log2(palette len)))`; indices do not span longs;
  `data` is absent when the palette has one entry. Read the height range from `yPos` and section `Y`,
  never assume it.
- **Older worlds:** 1.16–1.17 keep the same packing under `Level.Sections[].Palette/BlockStates`;
  1.13–1.15 pack indices across long boundaries. Pre-1.13 uses numeric ids.
- **2026 changes** (year-based version numbers since 26.1):
  - 26.1 moved the default dimensions under a `dimensions/` folder.
  - 26.3 renamed palette fields `Name` → `id` and `Properties` → `properties`, and allows a default
    state to be written as a bare string, so a palette can be a list of strings.
  - 26.4 snapshots rename `Status` → `status` and store biomes per block.
  - Branch on `DataVersion`, accept both casings and both palette shapes.
- **Sponge v2:** gzip NBT; `Width/Height/Length`; `Palette` maps `"minecraft:oak_stairs[facing=north,…]"`
  to an int; `BlockData` is a varint byte array indexed `x + z*Width + y*Width*Length`. **v3** nests the
  same under `Schematic.Blocks`.
- **Litematica:** `Regions{name → {Position, Size (may be negative), BlockStatePalette, BlockStates}}`;
  bits `max(2, ceil(log2(palette)))`; entries **do** span longs.
- **Vanilla structure:** `size`, `palette`, `blocks[{state, pos, nbt?}]`.
- Stair `shape` and fence/wall/pane connections are stored in the block state in every format, so the
  converter never has to infer them from neighbours.
- Block renames across versions (`grass` → `short_grass`, `grass_path` → `dirt_path` …) are handled by
  an alias list in the mapping table, not by running Mojang's data fixers.

## 3. Geometry: 1 block = 2 × 2 studs × 6 plates

Our units (`crates/game/src/stud.rs:17-28`, `tiles.rs`): a stud is 0.5 m, a plate 0.2 m, a brick course
3 plates. A sim tile is 2 × 2 studs × 6 plates (1 m × 1 m × 1.2 m), and a person is two tiles tall.

Horizontally there is no choice: 1 block = 2 × 2 studs. Vertically there are two candidates:

| | 5 plates per block | **6 plates per block (decided)** |
|---|---|---|
| Proportion | Exact cube (1.0 m) | 20 % taller (1.2 m) |
| Floors vs nav tiles | Drift: 6 blocks = 5 tiles, most floors land mid-tile | Every floor is a tile level |
| A 2-block doorway | 2.0 m — shorter than a person (2 tiles = 2.4 m) | 2.4 m — exactly a person |
| Slab (half block) | 2.5 plates, not representable | 3 plates = one brick course |
| Stair step | 2.5 plates | 0.6 m, under the 0.65 m auto-step |
| Full block | brick + 2 plates | 2 brick courses, what `fill_box` lays best |

Six plates is the only option under which an imported house is usable by the sim: units fit through
its doors, walk its floors and climb its stairs. The cost is that a 100-block tower becomes 120 m.
Official LEGO Minecraft sets and the `block2brick` tool make the opposite compromise (4 plates, 20 %
squat); no tool found uses 5.

Keep `--plates-per-block 5` as a dial for statues and scenery where proportion matters and nobody
walks inside. It costs nothing in the design: the block → cell step is one multiplication.

Stamping should snap to the tile grid (2 studs, 6 plates) so the alignment above survives placement.

## 4. Materials

One `u16` per brick (`stud_material.rs`): ids ≥ `0x8000` are the 25 `STUD_NATURALS`; ids below are
plastic in one of 322 LDraw colours (`assets/studs/colors.ron`, with RGB). There is no free RGB.

### Split every block into base material × form

Minecraft's block list is mostly a product: 16 dye colours × {wool, concrete, terracotta, glass, …},
a dozen wood types × {planks, stairs, slab, fence, door, log, …}, stone types × {block, stairs, slab,
wall}. So the table has two parts:

1. **Base materials** (~150 rows, hand-written, `assets/mc_import/materials.ron`): a name pattern →
   our material. `stone`, `dirt`, `grass_block`, `sand`, `gravel`, `clay`, `snow_block`, `ice`,
   `sandstone`, `granite`, `basalt`, `mud`, `podzol`, `red_sand` … map to the natural of the same
   meaning, so imported ground digs like ours. Everything else maps to a plastic colour chosen by
   hand: `{dye}_wool|concrete|terracotta` → 16 LDraw colours per family, `{wood}_planks` → a brown per
   species, `bricks`, `cobblestone`, `quartz_block`, ores, metals.
2. **Forms** (§5): derived from the name suffix (`_stairs`, `_slab`, `_wall`, `_fence`, `_pane`, …)
   and the block-state properties. `oak_stairs` is base `oak_planks` in form `stairs`.

### Fallback for unmapped blocks

- Nearest palette colour in Oklab from a representative block colour, with class before colour
  (leaves stay leaves, transparent stays transparent).
- The representative colours are the legal soft spot: an average-colour table computed from Mojang's
  textures is a derivative of assets we may not redistribute. Two clean sources **[call]**:
  - hand-pick colours for the base-material table (it is only ~150 rows, and they look better);
  - for anything left, compute averages at import time from the user's own installed Minecraft jar,
    the same arrangement `crates/slint-ui` has with a local Factorio install.
- Always write a report: every unmapped or approximated block id with its count. Every converter
  surveyed that people actually use does this (mc2brs, the Hytale importers, mcimport).
- The table is user-editable and the report names the rows to add, so coverage grows by use.

### What our side lacks

| Minecraft has | We have today | Effect on imports |
|---|---|---|
| Glass, stained glass, panes | 57 transparent LDraw colours, drawn opaque (TODO.md:159) | Windows are solid until transparency renders. |
| Planks, bricks, cobblestone, ores, metals | Only `wood` plus plastic colours | Right colour, wrong material: plastic hardness, no drops. |
| Light sources | Nothing | Dropped; interiors are dark. |
| Lava, flowing water | Static `water_top` per column, no other liquid | Dropped (§5). |

Decided: **import as plastic first**, add no materials with the importer, and let the import reports
show which are missed most. Adding materials later is an append to `STUD_NATURALS` (append-only, ids are stored). The mapping table
names materials, not ids, so imports made earlier can be re-converted to pick up new materials — one
reason to keep the source file path and the mapping version in the prefab's metadata.

## 5. Forms: what each kind of block becomes

A block is a 2 × 6 × 2 cell box (studs × plates × studs). "Quadrant" below means one of its four stud
columns. Bricks are axis-aligned and rotate only about Y.

| Block kind | Becomes | Notes |
|---|---|---|
| Full cube | 2 × 6 × 2 box | The bulk of any build; merged across blocks (§6). |
| Slab `type=bottom/top/double` | lower 3 plates / upper 3 plates / full | Exact. |
| Stairs, `half=bottom` | lower 3 plates full + upper 3 plates on the back half | `shape` picks the upper part: straight = 2 quadrants, outer corner = 1, inner corner = 3. `facing` rotates. Both rises are 0.6 m, walkable. |
| Stairs, `half=top` | mirror image vertically | Overhangs are fine as boxes. |
| Stairs as roof (option) | 3-plate course + `Slope 45 2x2` / `2x1` | Catalog parts 3039/3040 are exactly a 3-plate slope. Looks right for roofs, but the slope's 0.1 m lip puts the step at 0.7 m, above auto-step — so stepped is the default (decided) and slopes are a flag. No inverted slope in the catalog (3660 could be appended via `tools/ldraw-bake/parts.txt`). |
| Wall, fence, pane, iron bars | 1 stud thick, on a fixed side of the block | A centred post is impossible: the block centre lies between studs. Convention: a run along x uses the low-z stud row, a run along z the low-x column, a bare post the low-x/low-z quadrant, so runs and corners line up (decided; fattening to a full block was the alternative). |
| Snow layers 1–8, carpet | `layers × 6 / 8` plates rounded up / 1 plate | |
| Farmland, dirt path, soul sand | Full block | 15/16 height rounds to full. |
| Logs, leaves | `wood` / `leaves` boxes | `axis` is irrelevant for boxes. Our trees tag bricks with a tree id in `data`, and felling depends on it; whether imported trees should be registered as trees is open (§10). |
| Water, `waterlogged=true` | Dropped; listed as a water layer | Our water is recipe data per column, never bricks. Stamping water is a prefab/worldgen question. |
| Lava, fire, portals | Dropped | No counterpart. |
| Chest, barrel | Marker (optionally our chest) | We have one container: a tile of material 70 tagged `STUD_CHEST_TAG` plus a `Container`. Contents do not map (different items). |
| Door, trapdoor, gate | Empty + marker with facing | Keeps the opening; the prefab system can bind a door object later. |
| Ladder, torch, lantern, bed, sign, banner, rail, redstone, button, plant, crop, flower | Dropped + marker | Listed in the report by kind. |
| `air`, `cave_air` | Carve or leave (below) | |
| `structure_void` | Leave the world alone | Minecraft's own "not part of the structure" block. |

**Markers.** Everything without a counterpart is recorded as `(cell, kind, facing, raw block state)`
rather than silently lost. The prefab editor can show them, and they become doors, lights and
furniture as those arrive in the game — without re-importing.

**Air.** A prefab cell needs three states: solid, carve (remove what is there) and keep. A selection
box around a house is mostly air that should not erase the hill it is placed against, while the air
inside the rooms must be carved. Default **[call]**: flood-fill from the box faces; air reachable from
outside is *keep*, enclosed air is *carve*. Flags force either for the whole volume.

**Entities** (mobs, item frames, armour stands, paintings) are ignored in v1.

## 6. From blocks to bricks

### Constraints from our format

- A chunk is 32 × 90 × 32 cells; a brick never crosses a chunk edge; overlaps are refused, not split
  (`stud.rs:260-343`).
- Catalog box parts go up to 2 × 8 studs, as bricks (3 plates) and plates (1 plate). `Filler` is a
  stud-less box of any size, used for terrain interiors, and player orders may not place it.
- `fill_box` (`stud_carve.rs:42`) already tiles any box with catalog parts deterministically, and
  `stud_trees.rs` already does cells → `merge_boxes` → cut at chunk edges → `fill_box`.

### Pipeline **[call]**

1. **Classify** each palette entry once: (material, form, marker). The per-block loop is then a lookup.
2. **Full cubes, in block space.** Greedy-merge same-material full cubes into maximal boxes: scan in
   fixed x, y, z order, grow along x, then z, then y. Scale boxes to cells (× 2, × 6, × 2). Working in
   block space matters for size: a 256³-block selection is 400 million cells, but only 16.7 million
   blocks.
3. **Partial blocks, in cell space.** Slabs, stairs, walls and layers emit their small boxes into a
   sparse cell set per material, merged with the same greedy rule. Slope parts are emitted as explicit
   parts.
4. **Output boxes and parts in prefab-local cells** — not bricks (§7).
5. **At stamp time** (prefab system): translate, rotate, cut at chunk edges, lay with `fill_box`.

Notes:

- **Determinism.** Fixed scan order and `BTreeMap`/`BTreeSet` throughout, so the same input and mapping
  give byte-identical output. mc2brs iterates a hash map and is not reproducible; ours should be, both
  for tests and so a prefab can be content-addressed.
- **Brick count.** Merging gives few boxes, but `fill_box` re-tiles them with parts of at most 2 × 8
  studs: a solid 16-block cube (32 × 96 × 32 cells) is about 2,000 bricks. The client already struggles at 200–300 bricks landing
  per frame (TODO.md:16). Mitigation: lay the visible shell with studded parts and the fully enclosed
  interior with `Filler` boxes, as `pristine_chunk` does for rock (`stud_ground.rs:230`). The converter
  can mark a box as interior cheaply; whether stamps may contain fillers is a prefab-plan decision.
  The converter reports predicted brick counts either way.
- **No interior culling.** mc2brs and Mineways drop hidden blocks. Ours must not: bricks are diggable,
  and a hollow hill is a bug.
- **Structural layout** (Legolization, Testuz et al.) is not needed: our bricks do not have to
  interlock. Staggering seams between courses would be cosmetic and can be added inside `fill_box`
  later without changing converter output — another reason to emit boxes.
- **Biome tint.** Grass, leaves and water are tinted by biome in Minecraft. With semantic mapping
  (`grass_block` → our `grass`) this does not matter; it only affects the colour fallback.

## 7. What the converter needs from the prefab system

The converter's output, and so the minimum a prefab must be able to hold:

```rust
struct ImportedPrefab {
    size: [u32; 3],                               // cells
    boxes: Vec<PrefabBox>,                        // { min, size, material: name-or-id, data: u16, interior: bool }
    parts: Vec<PrefabPart>,                       // { min, part: LDraw number, rot, material, data } — slopes
    carve: Vec<([u32; 3], [u32; 3])>,             // air boxes to clear
    water: Vec<([u32; 3], [u32; 3])>,             // optional, for whoever can stamp water
    markers: Vec<Marker>,                         // doors, chests, lights, …
    meta: Meta,                                   // source file + hash, format, data version, mapping version,
                                                  // plates_per_block, author, licence, import report
}
```

Requirements this places on the prefab plan:

1. **Store placement-independent geometry, lay bricks at stamp time.** A stored `Vec<StudBrick>` is tied
   to one chunk alignment. The existing sim calls already take boxes: `fill_stud_boxes_safe(&[(lo, hi,
   material)])`, `carve_stud_boxes_safe`, and `place_stud_safe` for single parts (`stud_ops.rs`).
2. **`data` on fills.** `fill_stud_boxes_safe` takes no `data` tag; chests and tree ids need one.
3. **Three-state cells:** solid, carve, keep.
4. **A generic marker list** the editor can display and later bind to objects.
5. **Material and part references that survive table growth:** names / LDraw numbers in the file,
   resolved to ids at load. Both tables are append-only, so ids would also work, but names let an old
   import pick up a material added later.
6. **Y rotation in quarter turns** applied to boxes, parts and markers (parts already carry `rot`).
7. **Provenance metadata** (§9).
8. **A decision on fillers in stamps** (§6).

If prefabs end up placed by worldgen as `StudRegionSeed::features`, note that `feature_chunks`
silently drops any brick that overlaps (`stud_ground.rs:97`): a building stamped into a hillside must
carve first.

## 8. Where it lives and what it depends on

- This is a **developer tool** for authoring sites, ruins and set pieces (decided), not a player-facing
  import. So: native only, a hand-curated mapping table, the jar-on-disk colour fallback is acceptable,
  and failures can be terse.
- **`crates/mc-import`** (library): readers, `BlockVolume`, mapping, forms, merging →
  `ImportedPrefab`. Depends on `game` only for the catalog and material tables. No Bevy. Kept as a
  library so the prefab editor can call it directly later.
- **`tools/mc-import`** (CLI, like `tools/ldraw-bake`): `just mc-import <input> [--box …]
  [--plates-per-block 6] [--stairs stepped|sloped] [--air auto|carve|keep] -o <prefab>`, plus
  `--report` to print the unmapped list without writing anything. New `just` recipe in the same change.
- **Dependencies [call]:** `fastnbt` (MIT/Apache, maintained, serde-based) is the only new crate;
  `flate2` and `lz4_flex` are already in `Cargo.lock`. Our own ~300 lines for the region header, the
  two bit-packings and the three schematic layouts.
- **Not used, and why:**
  - `fastanvil` — last released 2025-08, before the 26.3 palette rename; pulls a renderer by default.
  - `rustmatica`, `mc_schem`, `mcdata` — GPL-3.0.
  - `nucleation` — MIT but very large (renderer, redstone simulator, storage backends).
  - `azalea-block` — typed block states, but trails releases; we only need names and string properties.
  - None of the recommended set brings TLS, so `aws-lc-rs` stays out.
- **Region size** is a build-time constant, but a prefab in local cells does not depend on it, so the
  tool needs no `VOXEL_REGION_BLOCKS` matching (unlike `worldctl`).
- **Tests:** tiny hand-built fixtures per format (a few blocks each, written by the test, not real
  builds), golden output for each form, and a byte-identical-rerun determinism test. These are plain
  unit tests in the new crate, outside the sim.

## 9. Legal

- **The tool.** Reading the save format and shipping no Mojang code, textures, models or sounds is what
  Mineways, Chunker, BlueMap and the other map renderers do. Block names and property names are facts.
  Do not ship Mojang textures or a colour table derived from them (§4). Do not name or brand the
  feature as if official; the Usage Guidelines ask for a "not an official Minecraft product" disclaimer.
- **The builds.** Mojang's terms leave user creations with their authors. Converting your own world is
  your business. Shipping a converted third-party build inside our game needs that author's permission:
  a Planet Minecraft page with no licence grants none, and CC BY-NC rules out commercial use. Build
  teams and map marketplaces use explicit written grants.
- **Consequence for the design:** the prefab carries `author`, `source`, `licence` fields, filled from
  the schematic's metadata where present and required (not defaulted) before a prefab can be marked
  shippable.
- The EULA and Usage Guidelines could not be fetched directly during this research; the above rests on
  search excerpts. Read both before shipping any imported content.

## 10. Decisions and open questions

Decided by Dawid, 2026-11-11:

1. **6 plates per block** is the default; 5 stays as a dial (§3).
2. **Stairs are stepped.** Slope bricks are an opt-in flag; no roof heuristic.
3. **Thin blocks** (fences, panes, walls) are 1 stud thick, off-centre by the fixed convention in §5.
4. **Developer tool**, not a player feature (§8).
5. **Plastic first**: no new materials with the importer (§4).

For the prefab plan:

1. Boxes-at-rest, bricks-at-stamp (§7.1) — accepted?
2. Fillers in stamped interiors (§6)?
3. How do imported trees relate to the tree registry (`data` = tree id, felling)? What does `wood`
   with `data = 0` do when cut?
4. Who stamps water, if anyone?

## 11. Not verified

- EULA / Usage Guidelines wording (§9).
- 26.4 chunk changes — unreleased, wiki only. The exact `dimensions/` path introduced in 26.1.
- Whether the 26.3 `id`/`properties` rename also reaches structure `.nbt`, and what palettes Sponge and
  Litematica exports from 26.3+ carry. Test against real files from a current version before trusting
  the readers.
- Sponge v3's palette key (`Palette` vs `BlockPalette` — the spec contradicts itself) and which Sponge
  version WorldEdit writes by default.
- Overworld height −64..320 and the 1.13–1.15 packing rule are from general knowledge, not a fetched
  source — which is why the reader takes the range from the data.
- On our side: that a slope's lip really blocks auto-step (computed from the hull, not tried in game),
  and how the sim treats overhanging bricks (assumed fine; nothing found that requires support).

## 12. Sources

- Formats: [Chunk format](https://minecraft.wiki/w/Chunk_format),
  [Region file format](https://minecraft.wiki/w/Region_file_format),
  [Structure file](https://minecraft.wiki/w/Structure_file),
  [26.3 Snapshot 7](https://minecraft.wiki/w/Java_Edition_26.3-snapshot-7),
  [Sponge schematic v2](https://github.com/SpongePowered/Schematic-Specification/blob/master/versions/schematic-2.md)
  / [v3](https://github.com/SpongePowered/Schematic-Specification/blob/master/versions/schematic-3.md),
  [Litematica format notes](https://litemapy.readthedocs.io/en/latest/litematics.html),
  [Bedrock level format](https://minecraft.wiki/w/Bedrock_Edition_level_format),
  [WorldEdit clipboard](https://worldedit.enginehub.org/en/latest/usage/clipboard/).
- Data: [misode/mcmeta](https://github.com/misode/mcmeta) (block/state counts computed from it:
  1,286 blocks, 35,723 states in 26.3), [PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data),
  [Oklab](https://bottosson.github.io/posts/oklab/).
- Tools: [Chunker](https://github.com/HiveGamesOSS/Chunker), [mc2brs](https://github.com/Kmschr/mc2brs),
  [block2brick](https://sourceforge.net/p/block2brick/wiki/Home/),
  [Mineways](https://erich.realtimerendering.com/minecraft/public/mineways/mineways.html),
  [jmc2obj](https://github.com/jmc2obj/j-mc-2-obj), [mcimport](https://github.com/fling-/mcimport),
  [greedy meshing](https://0fps.net/2012/06/30/meshing-in-a-minecraft-game/).
- Legal: [EULA](https://www.minecraft.net/en-us/eula),
  [Usage Guidelines](https://www.minecraft.net/en-us/usage-guidelines).
- Ours: `docs/superpowers/specs/2026-11-08-stud-chunks-design.md`,
  `docs/superpowers/specs/2026-11-08-brick-world-prior-art.md`,
  `docs/superpowers/plans/2026-11-09-stud-world-replacement.md`.
