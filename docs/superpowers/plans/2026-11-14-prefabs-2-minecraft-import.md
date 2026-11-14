# Prefabs 2 — Minecraft schematic import — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command turns a Sponge `.schem` file — whole, or split into the separate builds laid out inside it — into prefab files the game's prefab module reads, with a report of what did not map.

**Architecture:** A library crate `crates/mc-import` (NBT reader → `BlockVolume` → classification of every block state → `PrefabFile`) and a CLI `tools/mc-import`. The library depends on `game` only for the prefab file types and shapes. Output goes to the git-ignored `prefab-imports/` at the repo root as `.prefab.bin` plus a text report.

**Tech Stack:** Rust 2021; `game::prefab` (plan 1); `flate2` (already in `Cargo.lock`) for gzip; `ron` for the mapping table; our own NBT reader (no new crate).

**Spec:** `docs/superpowers/plans/2026-11-14-prefabs-minecraft-import-site-generation.md` (the roadmap; this plan is its stage D for `.schem`, with §5 as the design and §11 as carry-over). Format notes: `docs/superpowers/specs/2026-11-11-minecraft-import-research.md` §2.

**Out of scope (later):** Anvil world folders, `.litematic`, vanilla structure `.nbt`, Sponge v1, pre-1.13 numeric ids, entities, a `map` picture, cut lists and `batch`.

## Global Constraints

- Work directly on `develop` in this checkout. **No git worktrees.** Other sessions commit to this branch and the user has uncommitted edits.
- Commit with explicit paths only: `git add <paths>` then `git commit -m "<msg>" -- <paths>`. Never `git add -A`, `git add -u`, `git commit -a`, a bare `git commit`, or `--amend`.
- Before the first edit to any file that already exists, run `git status --short -- <file>`; if it is modified, make the edit but do NOT stage or commit that file, and list it under "left uncommitted". `justfile` and `crates/client/src/renderer/mod.rs` are always such files.
- Commit messages end with the line `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Deterministic output: `BTreeMap`/`BTreeSet` only, fixed scan orders; the same input and mapping table give a byte-identical prefab file.
- A schematic is untrusted input: no panic, no unbounded allocation from a length field, on any byte sequence. Errors are values.
- No Mojang asset, texture or colour table derived from one. Minecraft block and property *names* are facts and are fine.
- Nothing is ever written under `assets/`. Import output goes to `prefab-imports/`; `mc-sources/` (the bought schematics) is read-only and git-ignored; no file from either is committed (one exception: the block-name census of Task 3, which is counts of names, not content), and test fixtures are built by the tests, never copied from a real schematic.
- Conventions shared with `game::prefab`: +X east, +Z south, north is −Z, +Y up — the same as Minecraft's, so coordinates map 1:1. `Stairs { facing }` has its full-height back on the `facing` side (Minecraft's `facing` for stairs means the same). `Marker.turn`: quarter turns clockwise from above, 0 = north.
- Builds and tests go through `just` (CPU-capped): `just test-one <crate> "<args>"`. Several test-name filters need `--` first: `just test-one game "--test it -- a:: b::"`. Never run a whole suite as a routine check. A test slower than 100 ms is tagged `#[ignore = "slow: >100ms — run via just test-full"]`.

## Review Focus

- A truncated, non-gzip, or deliberately huge-length NBT file → an error, promptly, with no large allocation (Task 2).
- A schematic whose `BlockData` length disagrees with `Width × Height × Length`, or with a palette index beyond the palette → an error, not a panic or a wrong build (Task 2).
- A block state the table does not know → placed as the fallback block and counted in the report by name; never silently dropped, never a failed import (Tasks 3, 4).
- A house on a selection that includes outside air → the outside air is **keep**, the rooms are **carve**; stamping it against a hill must not erase the hill (Task 4).
- A schematic of many builds standing on a shared ground plate → one prefab per build, the plate excluded, nothing merged across the gaps (Task 5).

---

### Task 1: Refuse a repeated prefab file name (carry-over from plan 1)

**Files:**
- Modify: `crates/game/src/prefab/builtin.rs`
- Modify: `crates/game/tests/prefab_builtin.rs`

**Interfaces:**
- Produces: `PrefabSet::from_sources` returns `Err` naming the file when two sources have the same file name (today its `BTreeMap` collect keeps one silently). Signature unchanged.

- [ ] **Step 1: Failing test** `a_repeated_file_name_is_refused` in `prefab_builtin.rs`: two valid sources both named `a.prefab.ron` (different content) → `Err` containing `a.prefab.ron` and the word `twice`; and through the disk loader: a temp dir with `a.prefab.ron` and `more/a.prefab.ron` → the same error.
- [ ] **Step 2: Run** `just test-one game "--test it prefab_builtin::"` — the new test FAILS (the set loads with one prefab).
- [ ] **Step 3: Implement**: detect the duplicate while collecting (before hashing or resolving); error text `prefab <file name>: this file name appears twice`.
- [ ] **Step 4: Run** the same command — PASS.
- [ ] **Step 5: Commit**: `fix(prefab): a prefab file name that appears twice is refused, not silently dropped`.

---

### Task 2: NBT and the Sponge schematic reader

**Files:**
- Create: `crates/mc-import/Cargo.toml`, `crates/mc-import/src/lib.rs`, `src/nbt.rs`, `src/volume.rs`, `src/sponge.rs`, `crates/mc-import/tests/it.rs`, `tests/common/mod.rs` (an NBT **writer** for fixtures), `tests/sponge.rs`
- Modify: root `Cargo.toml` (member `crates/mc-import`), `Cargo.lock`

**Interfaces:**
- Produces, in `mc_import`:

```rust
// nbt.rs
pub enum Tag { Byte(i8), Short(i16), Int(i32), Long(i64), Float(f32), Double(f64), ByteArray(Vec<u8>), String(String),
               List(Vec<Tag>), Compound(BTreeMap<String, Tag>), IntArray(Vec<i32>), LongArray(Vec<i64>) }
pub const NBT_MAX_DEPTH: usize = 64;
/// Parses one uncompressed, big-endian (Java) named root tag. Every length is checked against the bytes
/// remaining before anything is allocated.
pub fn parse_nbt(bytes: &[u8]) -> Result<(String, Tag), ImportError>;
/// Gunzips (refusing more than `max_bytes` of output) then parses.
pub fn parse_gzip_nbt(bytes: &[u8], max_bytes: usize) -> Result<(String, Tag), ImportError>;
impl Tag { pub fn get(&self, key: &str) -> Option<&Tag>; pub fn as_i32(&self) -> Option<i32>; pub fn as_str(&self) -> Option<&str>; /* as_compound, as_list, as_bytes, as_ints */ }

// lib.rs
pub enum ImportError { NotGzip, TooLarge, Truncated, BadNbt(String), NotASchematic(String), Unsupported(String), Io(String) }  // Display + Error

// volume.rs
/// A block state: `minecraft:oak_stairs` + its properties.
pub struct BlockState { pub name: String, pub props: BTreeMap<String, String> }     // Ord, Clone, Debug
impl BlockState { pub fn parse(s: &str) -> Result<BlockState, ImportError>; }        // "minecraft:oak_stairs[facing=north,half=bottom]"
pub struct BlockEntity { pub pos: [i32; 3], pub id: String, pub nbt: Tag }
pub struct BlockVolume {
    pub size: [u32; 3],                       // x (Width), y (Height), z (Length)
    pub palette: Vec<BlockState>,
    pub blocks: Vec<u16>,                     // palette index, index = x + z * sx + y * sx * sz
    pub block_entities: Vec<BlockEntity>,
    pub data_version: i32,
    pub name: String, pub author: String,     // from Metadata, "" when absent
}
impl BlockVolume { pub fn at(&self, x: u32, y: u32, z: u32) -> &BlockState; pub fn index(&self, x: u32, y: u32, z: u32) -> usize; }

// sponge.rs
pub const SCHEM_MAX_BYTES: usize = 1 << 30;   // decompressed
pub const SCHEM_MAX_CELLS: u64 = 1 << 28;
/// Sponge schematic v2 (root `Schematic` with `Palette`, `BlockData`) and v3 (root "" holding `Schematic`
/// with `Blocks { Palette, Data, BlockEntities }`). v1 and anything else → Unsupported.
pub fn read_schem(bytes: &[u8]) -> Result<BlockVolume, ImportError>;
```

Format facts: `Width`/`Height`/`Length` are shorts, read as unsigned 16-bit; `Palette` is a compound mapping state string → int; `BlockData`/`Data` is a byte array of **varints**, one per cell, index `x + z * Width + y * Width * Length`; `BlockEntities` is a list of compounds with `Pos` (int array of 3) and `Id`. A palette of more than 65 536 entries is `Unsupported`.

- [ ] **Step 1: Failing tests** `tests/sponge.rs`, with fixtures built by `tests/common` (an NBT writer + gzip):
  - `reads_a_v2_schematic`: a 3 × 2 × 4 volume with a 3-state palette (one with properties), known cells and one sign block entity → size, `at()` for six probes, palette states parsed, entity position and id, data version, metadata name and author.
  - `reads_a_v3_schematic`: the same content in v3 layout → an equal `BlockVolume`.
  - `varints_above_127`: a palette of 200 states where cells use indices ≥ 128 (two-byte varints) reads back exactly.
  - `refuses_bad_input`: empty input and plain text → `NotGzip`; a valid file cut at every tenth length → an error, never a panic; `BlockData` one cell short and one cell long → `NotASchematic`; a cell index ≥ palette length → `NotASchematic`; `Version: 1` → `Unsupported`; a byte-array tag declaring length `i32::MAX` in a 40-byte file → `Truncated` without allocating; nesting 100 lists deep → `BadNbt`.
  - `block_state_parsing`: `"minecraft:stone"`, `"minecraft:oak_stairs[facing=north,half=bottom]"`, and malformed strings (`"a[b"`, `"a[b=c"`, `""`) → `Err`.
- [ ] **Step 2: Run** `just test-one mc-import "--test it sponge::"` — FAIL.
- [ ] **Step 3: Implement.** `Cargo.toml`: edition 2021, `autotests = false` with one `[[test]] it`; deps `game = { path = "../game" }`, `flate2` (the version in `Cargo.lock`, default features), `ron`, `serde`. `blocks` is `Vec<u16>`.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(mc-import): an NBT reader and the Sponge schematic reader`.

---

### Task 3: Classifying block states

**Files:**
- Create: `crates/mc-import/src/classify.rs`, `crates/mc-import/src/table.rs`, `crates/mc-import/data/materials.ron`, `crates/mc-import/data/census.tsv`, `crates/mc-import/tests/classify.rs`
- Modify: `crates/mc-import/src/lib.rs`, `tests/it.rs`

**Interfaces:**
- Consumes: `BlockState` (Task 2); `game::prefab::{Shape, Facing, Half, StairCorner, Sides}`.
- Produces:

```rust
// table.rs
pub const MAPPING_VERSION: u32 = 1;
pub struct Table { /* parsed data/materials.ron */ }
impl Table { pub fn builtin() -> &'static Table;  pub fn from_ron(src: &str) -> Result<Table, String>; }

// classify.rs
pub enum Class {
    Air,                                            // air, cave_air, void_air
    Keep,                                           // structure_void, barrier, light: "not part of the build"
    Block { block: String },                        // a whole block of ours, by name
    Shaped { shape: Shape, block: String },
    Marker { kind: String, turn: u8, solid: Option<String> },   // door, torch, chest…; `solid` = also place this block
    Dropped { why: &'static str },                  // water, lava, plants, redstone…: becomes air, counted
}
pub struct Classified { pub class: Class, pub mapped: bool }     // mapped = false when the fallback was used
pub fn classify(state: &BlockState, table: &Table) -> Classified;
```

`data/materials.ron` (hand-written, embedded with `include_str!`) has: `fallback: "smooth_stone"`; `aliases` (old name → new name); `materials`: Minecraft base name → our block name; `keep`, `dropped` (name or suffix → reason), `markers` (name or suffix → kind). **Form rules** are code, by suffix of the Minecraft name after the `minecraft:` prefix, applied to the base material:

| Suffix / name | Class |
|---|---|
| `_slab` | `type=double` → `Block`; `bottom`/`top` → `Slab { half }` |
| `_stairs` | `Stairs { facing, half, corner }`; `shape` = `straight`, `inner_left`, `inner_right`, `outer_left`, `outer_right` map to the same-named corner |
| `_wall` | `Wall { sides, post }`: a side is set when its property is `low` or `tall` (or `true`); `post` = `up` |
| `_fence`, `iron_bars`, `_pane`, `glass_pane` | `Fence { sides }` / `Pane { sides }` from `north/east/south/west=true` |
| `_trapdoor` | `Trapdoor { facing, half, open }` |
| `_carpet`, `moss_carpet` | `Carpet` |
| `snow` (with `layers`) | `Layer { layers }`; 8 → `Block` snow |
| `_door`, `_fence_gate` | `Marker { kind: "door" / "gate", turn from facing, solid: None }` |
| `chest`, `trapped_chest`, `barrel` | `Marker { kind: "loot", turn, solid: Some("chest") }` |
| torches, lanterns, campfire, candles, sea_lantern, glowstone… | `Marker { kind: "light", .. }` — a full-cube light source keeps a `solid` block |
| signs, banners, beds, ladders, buttons, levers, pressure plates, rails, redstone, flower pots, heads, item frames | `Marker` by kind (`sign`, `banner`, `bed`, `ladder`, `decor`) with `solid: None` |
| water, lava, bubble_column, kelp, seagrass, fire | `Dropped` |
| flowers, saplings, grass, ferns, vines, crops, mushrooms, coral fans, dead bushes | `Dropped { why: "plant" }` |
| `_log`, `_wood`, `_stem`, `_hyphae` | `Block` (our `<species>_log` or `stripped_<species>`; `axis` ignored) |
| `_leaves` | `Block` our `<species>_leaves` |
| anything else | `Block { materials[name] }`, or the fallback with `mapped: false` |

The base material of `oak_stairs` is `oak_planks`, of `stone_brick_stairs` is `stone_bricks`, of `smooth_sandstone_slab` is `smooth_sandstone`: resolve `<x>_stairs` by trying `x`, `x + "s"`, `x + "_planks"`, `x + "_block"` in `materials`. `waterlogged=true` is ignored (the block is kept, the water is not).

`data/census.tsv` is the block census of the sample library (names and counts — facts): **copy** `mc-sources/_census/blocks.tsv` (columns `block`, `count`, `share_of_non_air`, `files`, `properties`) into the crate. It is the worklist for the table and the fixture for the coverage test. `materials` must map every Minecraft name to a block that exists in `assets/blocks/blocks.ron`; pick the nearest of our 168 blocks by material and colour where there is no namesake (for example `stripped_warped_hyphae` → `stripped_warped`, `black_concrete_powder` → `black_concrete`, `end_stone_bricks` → `sandstone`-family or `quartz_bricks`, ores → their stone).

- [ ] **Step 1: Failing tests** `tests/classify.rs`:
  - `forms`: one assertion per row of the table above with concrete states, including `oak_stairs[facing=east,half=top,shape=outer_left]` → `Shaped { Stairs { East, Top, OuterLeft }, "oak_planks" }`; `smooth_sandstone_slab[type=double]` → `Block("smooth_sandstone")`; `sandstone_wall[east=low,north=none,south=tall,up=true,west=none]` → `Wall { sides: east+south, post: true }`; `spruce_trapdoor[facing=west,half=top,open=true]`; `snow[layers=3]`; `oak_door[facing=south,half=lower,…]` → `Marker { kind: "door", turn: 2, solid: None }`; `chest[facing=north]` → loot marker with `solid: Some("chest")`; `water[level=0]` → `Dropped`; `structure_void` and `barrier` → `Keep`; `air` → `Air`.
  - `unknown_block_falls_back`: `minecraft:definitely_not_a_block` → `Block("smooth_stone")`, `mapped: false`; `minecraft:definitely_not_stairs[facing=north,half=bottom,shape=straight]` → `Shaped` stairs of the fallback, `mapped: false`.
  - `every_target_block_exists`: every block name the table can produce (all `materials` values, the fallback, `chest`) is in `assets/blocks/blocks.ron` (parse it with `game::registry::BlockRegistry`).
  - `the_census_is_covered`: classify a default-property state of every name in `data/census.tsv` (build props from the `properties` column's first listed value of each key); the names with `mapped: false` account for **less than 0.5 %** of the non-air count, and the test prints them sorted by count when it fails.
- [ ] **Step 2: Run** `just test-one mc-import "--test it classify::"` — FAIL.
- [ ] **Step 3: Implement** the rules and write the table from the census, most frequent names first, until the coverage test passes; then map the remaining names you can place sensibly.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(mc-import): classify block states — materials table, forms, markers, a fallback`.

---

### Task 4: From a volume to a prefab file

**Files:**
- Create: `crates/mc-import/src/convert.rs`, `crates/mc-import/src/report.rs`, `crates/mc-import/tests/convert.rs`
- Modify: `crates/mc-import/src/lib.rs`, `tests/it.rs`

**Interfaces:**
- Consumes: `BlockVolume` (Task 2), `classify`, `Class`, `Table`, `MAPPING_VERSION` (Task 3); `game::prefab::{PrefabFile, FileBox, FileShaped, FileShape, FileCarve, Marker, PrefabMeta, ImportInfo, GroundFit, PREFAB_FORMAT}`.
- Produces:

```rust
pub enum AirMode { Auto, Carve, Keep }        // Auto = flood fill from the box faces
pub struct ConvertOptions {
    pub name: String, pub air: AirMode,
    pub region: Option<([u32; 3], [u32; 3])>,  // a sub-box of the volume (min inclusive, max exclusive); None = all
    pub author: String, pub source: String, pub licence: String, pub source_hash: String,
    pub tags: Vec<String>,
}
pub struct Report {
    pub cells: u64, pub blocks: u64, pub shaped: u64, pub carved: u64, pub kept_air: u64,
    pub boxes: usize,
    pub unmapped: BTreeMap<String, u64>,       // Minecraft name -> cells placed as the fallback
    pub dropped: BTreeMap<String, u64>,        // reason -> cells
    pub markers: BTreeMap<String, u64>,        // kind -> count
}
impl Report { pub fn to_text(&self, name: &str) -> String; }
/// `None` when the region holds nothing solid.
pub fn convert(volume: &BlockVolume, table: &Table, opts: &ConvertOptions) -> Option<(PrefabFile, Report)>;
```

Rules:
1. **Trim**: the prefab's box is the bounding box of the non-air, non-keep cells of the region (markers count). `size` is that box; cells are relative to its min corner.
2. **Air**: `Dropped` cells are air. With `AirMode::Auto`, air (and `Keep`) cells reachable from any face of the trimmed box through air/keep/marker-without-solid cells, 6-connected, are **keep** (no entry in the file); enclosed air is **carve**. `Carve` mode carves all air; `Keep` mode carves none. `Class::Keep` cells are never carved.
3. **Boxes**: whole blocks greedy-merged per block name — scan cells in (y, z, x) order; an unclaimed cell starts a box that grows along +x, then +z, then +y while every cell in the new layer is the same block and unclaimed. Carve cells are merged the same way into `carve` boxes. Output lists are in scan order of their min corner.
4. **Shaped**: one `FileShaped { at, shape: Named { shape, mat } }` per shaped cell, in scan order.
5. **Markers**: `Marker { at, kind, turn, props }` with `props` = the state's properties plus `"mc" = <minecraft name>`; a sign's text (block entity `front_text.messages`, or `Text1..Text4` in older files; strip the JSON to its plain text) goes in `props["text"]`. A marker with `solid` also writes that block.
6. **Palette**: our block names in order of first use.
7. **Anchor**: `[size.x / 2, 0, size.z / 2]`. **Ground**: `Flat`. **Meta**: `author`, `source`, `licence` from the options; `import: Some(ImportInfo { source_hash, source_box: the trimmed box in volume coordinates, data_version, mapping_version: MAPPING_VERSION })`. `format: PREFAB_FORMAT`.

- [ ] **Step 1: Failing tests** `tests/convert.rs` (volumes built in memory, no gzip needed):
  - `a_hollow_house`: a 7 × 5 × 7 stone-brick box with a 5 × 3 × 5 air room, one doorway cell open to the outside, set in a 11 × 7 × 11 volume of air → with `Auto`: size 7 × 5 × 7; the room and doorway are reachable from outside, so **no carve**; close the doorway with a door block (→ marker, no solid) and it is still reachable; replace the door with a glass block → the room is 75 carve cells in one carve box; the prefab resolves (`game::prefab::resolve` with a registry from `blocks.ron`) and `cell()` at a room cell is `Carve`, at a wall cell `Block`.
  - `greedy_boxes`: a 4 × 3 × 2 solid slab of one block is **one** box; two blocks side by side of different materials are two; an L-shape is two boxes; the union of boxes covers each solid cell exactly once (brute-force check on a pseudo-random 12³ volume, LCG-filled with three materials and air).
  - `shaped_and_markers`: stairs, a wall and a slab become `Named` shaped cells with the classified shape; a chest writes a `chest` block and a `loot` marker; a sign block entity's text lands in the marker's `props["text"]` (both the 1.20 `front_text` form and the old `Text1` form).
  - `unmapped_and_dropped_are_counted`: a volume with an unknown block, water and a flower → the unknown appears in `unmapped` by its Minecraft name and is placed as `smooth_stone`; water and the flower are in `dropped`; `to_text` mentions all three.
  - `region_and_trim`: converting a sub-region containing one of two builds yields only that build, trimmed; a region of pure air → `None`.
  - `deterministic`: converting twice gives byte-identical `prefab_to_bin` output.
- [ ] **Step 2: Run** `just test-one mc-import "--test it convert::"` — FAIL.
- [ ] **Step 3: Implement.** Flood fill with an explicit stack and a bitset (no recursion). Classify each **palette entry** once, then loop cells by index.
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(mc-import): a block volume becomes a prefab file — trimmed, air kept or carved, boxes merged, a report`.

---

### Task 5: Splitting a schematic into its builds

**Files:**
- Create: `crates/mc-import/src/split.rs`, `crates/mc-import/tests/split.rs`
- Modify: `crates/mc-import/src/lib.rs`, `tests/it.rs`

**Interfaces:**
- Consumes: `BlockVolume`, `classify`, `Class`, `Table`.
- Produces:

```rust
pub struct SplitOptions {
    pub gap: u32,          // builds closer than this many empty cells (in x/z) are one build; default 3
    pub min_blocks: u64,   // smaller components are dropped; default 30
    pub ground: GroundMode,
}
pub enum GroundMode { Auto, None, Layers(u32) }   // Layers(n): the lowest n layers of the volume are ground
pub struct Build { pub min: [u32; 3], pub max: [u32; 3], pub blocks: u64, pub label: Option<String> }  // max exclusive
pub struct Split { pub ground_layers: u32, pub builds: Vec<Build>, pub dropped_small: usize }
pub fn split(volume: &BlockVolume, table: &Table, opts: &SplitOptions) -> Split;
/// A file-name-safe name: lowercase ASCII letters, digits and `-`; "" when nothing is left.
pub fn slug(text: &str) -> String;
```

Rules:
1. **Ground** (`Auto`): starting from y = 0, a layer is ground while at least 60 % of its (x, z) cells are solid (a `Block` or `Shaped` class); stop at the first layer that is not. A volume with no such layer has 0 ground layers.
2. **Components** work on the 2D footprint above the ground: a column (x, z) is occupied when any cell above the ground layers is `Block`, `Shaped`, or a `Marker` with `solid`. Dilate the occupancy by `gap` (Chebyshev) and label 8-connected components; each component's build is the tight 3D bounding box of the solid cells in its (undilated) columns, from the first layer above ground to its highest solid cell. Builds are ordered by `(min.z, min.x)`.
3. **Label**: the text of the sign block entity nearest to the build's box (within 6 cells of it, measured in x/z), if any.
4. Components with fewer than `min_blocks` solid cells are dropped and counted.

- [ ] **Step 1: Failing tests** `tests/split.rs`:
  - `three_statues_on_a_plate`: a 40 × 12 × 14 volume, 2 full ground layers of grass, three 4 × 6 × 4 pillars at x = 3, 15, 30 → `ground_layers == 2`, three builds with the exact boxes (y from 2 to 8), in x order.
  - `a_gap_joins_or_separates`: two pillars 2 cells apart are one build with `gap: 3` and two with `gap: 1`.
  - `small_debris_is_dropped`: a lone 2-block lamp post is dropped with `min_blocks: 30` and counted in `dropped_small`.
  - `signs_label_builds`: a sign entity 2 cells from the middle pillar with text `Zeus` → that build's label is `Zeus`, the others `None`.
  - `no_ground`: a volume with no plate → `ground_layers == 0`, builds start at y = 0; `GroundMode::Layers(1)` forces one.
  - `slug`: `"Zeus Statue #2!"` → `"zeus-statue-2"`; `"Häuser"` → `"h-user"` or `"hauser"` — pin whichever the rule gives and keep it deterministic; `"!!!"` → `""`.
- [ ] **Step 2: Run** `just test-one mc-import "--test it split::"` — FAIL.
- [ ] **Step 3: Implement** (footprint as a `Vec<u32>` label grid; a queue-based flood, no recursion).
- [ ] **Step 4: Run** — PASS.
- [ ] **Step 5: Commit**: `feat(mc-import): split a schematic of spaced-out builds into one box per build`.

---

### Task 6: The `mc-import` command

**Files:**
- Create: `tools/mc-import/Cargo.toml`, `tools/mc-import/src/main.rs`, `tools/mc-import/tests/cli.rs`
- Modify: root `Cargo.toml` (member `tools/mc-import`), `Cargo.lock`
- Modify (left uncommitted): `justfile` — recipe `mc-import *args`: `{{cap}} {{cargo}} run --release -p mc-import-tool -- {{args}}`, with a comment above it, next to `prefab-view`

**Interfaces:**
- Consumes: `mc_import::{read_schem, convert, split, slug, Table, ConvertOptions, SplitOptions, AirMode, GroundMode, Report}`; `game::prefab::{prefab_to_bin, BIN_SUFFIX, resolve, Trust}`, `game::registry::BlockRegistry`.
- Produces: package `mc-import-tool`, binary `mc-import`:
  - `mc-import report <file.schem>…` — per file: size, data version, author, non-air cells, the number of builds `split` finds, then the unmapped Minecraft names and dropped reasons with counts. Writes nothing.
  - `mc-import import <file.schem> [--name N] [--out DIR] [--air auto|carve|keep] [--licence TEXT] [--author TEXT] [--tag T]…` — the whole file as one prefab.
  - `mc-import split <file.schem> [--prefix P] [--out DIR] [--gap N] [--min-blocks N] [--ground auto|none|N] [--air …] [--licence …] [--author …] [--tag T]…` — one prefab per build, named `<prefix>-<slug of the sign label>` or `<prefix>-<nn>` (two digits, in build order) when there is no label or the name is already taken.
  - Defaults: `--out prefab-imports/<file stem>/`; `--name`/`--prefix` = `slug` of the file stem; `--author` = the schematic's metadata author; `--licence` = `""`; `source` = the input file name; `source_hash` = crc32 of the file bytes as 8 hex digits.
  - For each prefab: write `<name>.prefab.bin` and `<name>.report.txt`, then **resolve it** against `assets/blocks/blocks.ron` (`Trust::Builtin`) and fail loudly if it does not resolve. Print one line per prefab: name, size, blocks, boxes, shaped, markers, unmapped cells.
  - Exit codes: 0 ok; 1 a file failed to read, convert or resolve (the others still run); 2 usage error or `assets/blocks/blocks.ron` not found (run from the repo root).

- [ ] **Step 1: Failing tests** `tests/cli.rs` (the binary via `env!("CARGO_BIN_EXE_mc-import")`, cwd = repo root; fixtures written to a temp dir by a small in-test NBT + gzip writer — copy the helper from `crates/mc-import/tests/common/mod.rs` with a comment saying so):
  - `import_writes_a_prefab_that_loads`: a small house schematic → exit 0; `<out>/<name>.prefab.bin` exists and `game::prefab::prefab_from_bin` reads it with the right name, `meta.source`, `meta.import.source_hash` of 8 hex digits and `mapping_version`; the report file exists.
  - `split_writes_one_prefab_per_build`: three pillars on a plate with a sign `Zeus` near one → three files, one named `<prefix>-zeus`, the others `<prefix>-01`/`-03` style by build order; none contains the plate (its `size.y` is the pillar height).
  - `report_writes_nothing`: exit 0, stdout names the file and the build count, the temp out dir stays empty.
  - `bad_input`: a non-schematic file → exit 1 and a message naming it; no arguments → exit 2 with usage; an unknown flag → exit 2.
- [ ] **Step 2: Run** `just test-one mc-import-tool "--test cli"` — FAIL.
- [ ] **Step 3: Implement** (argument parsing by hand; no dependency beyond `mc-import`, `game`, `crc32fast`, and `flate2` as a dev-dependency for the fixture writer).
- [ ] **Step 4: Run** — PASS. Add the recipe to `justfile` (do not stage it).
- [ ] **Step 5: Commit** `tools/mc-import/**`, root `Cargo.toml`, `Cargo.lock`: `feat(tools): mc-import — report, import and split Sponge schematics into prefabs`.
