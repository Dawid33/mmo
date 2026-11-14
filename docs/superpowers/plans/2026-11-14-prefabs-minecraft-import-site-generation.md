# Prefabs, Minecraft import and generated sites — roadmap

Date: 2026-11-14. Status: **draft for approval — nothing is built.** This is a roadmap in the shape of
`2026-11-14-adventure-gameplay-roadmap.md`: it fixes the design, the order and the seams between three
pieces of work. Each stage in §8 becomes its own task-level plan when it starts.

The three things asked for:

1. **Prefabs** — saved groups of blocks placed into the world as a unit; by worldgen first (MVP), at
   runtime later.
2. **Minecraft import** — turning Minecraft worlds, or boxes cut out of them, into prefabs.
3. **Site generation** — composing prefabs procedurally into towns, fortresses and dungeons.

Everything marked **(call)** is Claude's proposal. §9 lists the calls that change the plan if wrong.

**Scope (decided 2026-11-14): towns first; dungeon generation is skipped for now.** The sample library
has no dungeon pieces and plenty of town material. Fortresses follow towns (they reuse the lots and add
a wall ring). What is written below about dungeons, lairs and the jigsaw technique is kept as a sketch
for later and is in no stage.

## 1. What already exists

| Thing | State | Use here |
|---|---|---|
| Prefab spec `specs/2026-11-11-prefab-system-design.md` | Built for the stud world, removed with it; code in git at `cc728a58` (`crates/game/src/prefab/`, `worldgen/src/prefab_world.rs`) | Its decisions and structure carry over (§2); the brick layout, the `Prop` kind and LDraw do not |
| Import research `specs/2026-11-11-minecraft-import-research.md` | Research only | §2 (formats), §8 (crates, licences), §9 (legal) hold unchanged; §3–§6 (studs, plates, plastic) are replaced by §5 below |
| Adventure roadmap §6 | Contract | Sites must expose typed **fixtures** saved in `RegionMeta`; `Site.kind`; layout a pure function of site and seed. Its stage 0 (test town + cave) is delivered by stage C here |
| `docs/game-design.md` §5–§6 | Direction | Pre-generated towns, dungeons, dwarven fortresses; Minecraft builds as "another source of places" |
| Trees (`worldgen/src/trees/`, `lib.rs` `tree_blocks`, `coarse_from_patch`) | Working | The only multi-block worldgen feature: the pattern for window-independent placement, the column band, and the far-LOD pass |

Facts about today's engine that shape the design (read from the code 2026-11-14):

- A block is 1 m, so **1 Minecraft block = 1 game block**. A block may carry a **chisel**: 16³ voxels as
  canonical cuboids with a palette (`game::ChiselData`). Worldgen already writes chisels (slopes).
- `blocks.ron` has 47 blocks: rock, soil, trees, snow, a chest. **No planks, brick, cobble, glass.**
  `BlockId` is `u16`; the client's texture array allows 2048 layers. Render modes are `Opaque | Cutout`
  only — no blended glass, no light sources.
- `generate_region` / `generate_columns` return terrain columns only and must give identical columns for
  any window. Genesis (`Region::from_columns`) creates no containers or entities.
- Far rings at play size (256 m regions) are built by `coarse_tile`, which **never runs
  `generate_region`**: a structure is invisible from afar unless it has its own coarse pass, as trees do.
- A saved region never regenerates. A generator change reaches never-saved regions only.
- `history::Site` has a region and no position inside it, no kind; history is bootstrapped at first server
  start (`server/src/world_file.rs`) and then changes during play. Worldgen cannot read it.
- Runtime edits are per-block `Copy` intents; `set_chunk_blocks_safe` is the batched, chisel-carrying
  write. Bedrock sits `UNDERGROUND_DEPTH` (60) blocks below the surface.

## 2. Decisions carried over

From the 2026-11-11 prefab spec (the user's): prefabs live on disk, are held in memory and can be
stamped; one layout routine serves worldgen and runtime; files name materials, never ids; the built-in
set is embedded and installed like block props; in a running game prefabs are per player like Factorio
blueprints (library, export strings, in-game editor). From the import research (the user's): the importer
is a **developer tool**, not a player feature.

Changed by the voxel return: there is one kind, the block prefab (the `Prop` kind was LDraw assemblies;
furniture and dressing come back later as their own design). "Plastic first" has no meaning any more —
see §5.3. The per-player library, registration and the editor are **deferred** to after stage H; nothing
here blocks them.

## 3. Goals, non-goals, anti-goals

**Goals**

1. A prefab file a person or a tool can write, independent of chunk, region and table growth.
2. Worldgen places prefabs whole across chunk and region edges, near and far, and stays a pure function.
3. A cut from a Minecraft world becomes a usable prefab with one command, with a report of what was lost.
4. Towns, fortresses and dungeons generated from a kit of prefabs, different per site, fitted to terrain,
   exposing the fixtures the adventure roadmap needs.
5. A review loop that needs no game session: pictures of prefabs and of generated site plans.

**Non-goals**: construction by workers, blueprint ghosts, item cost; the player library and editor;
working doors, furniture entities, lights, blended glass; Minecraft entities, redstone, Bedrock worlds,
pre-1.13 worlds; site simulation (who lives in which house); adding sites to worlds already forged.

**Anti-goals**

- Worldgen and runtime stamping give different blocks from the same prefab, anchor and turn.
- A structure that is visible near and missing from the far rings, or cut at a region edge.
- A site whose layout depends on which region generated first, or on the query window.
- A third-party build shipped without its author's permission.
- A prefab or an import that can crash or stall a region.

## 4. Prefabs

### 4.1 The file

```text
Prefab  { format: u16, name, meta: { author, source, licence, note, import: Option<ImportInfo> },
          size: [u32;3],                         // blocks; the anchor is a cell inside or on the box
          anchor: [i32;3],
          palette: [String],                     // block names from blocks.ron
          boxes:  [{ min, size, mat }],          // whole blocks of one material
          shaped: [{ at, shape }],               // one chiselled block each
          carve:  [{ min, size }],               // cleared before anything is laid
          markers:[{ at, kind: String, turn, props: {String: String} }],
          ground: Flat | Follow | Buried,        // how the placer fits it to terrain (§4.4)
          tags:   [String] }                     // free words for the generators ("house", "small", …)
shape   = Named { kind: Slab|Stairs|Post|Pane|Layer|…, facing, half, corner, mats: [palette idx] }
        | Cuboids { cuboids: [..], mats: [palette idx] }      // anything else, e.g. from an editor
```

- A cell not covered by `boxes`, `shaped` or `carve` is **keep**: the world shows through. That is the
  three-state cell the import research asks for.
- `Named` shapes come from a small catalogue in `game` (`prefab/shapes.rs`) that produces canonical
  `ChiselData`; a file stays readable and a shape fix reaches every prefab.
- Two encodings of the same file form **(call)**: `.prefab.ron` (text, hand-written and small ones) and
  `.prefab.bin` (`MMOPF2` + lz4(bincode); imports — a large build is 10⁵ boxes). One loader reads both.
- Placement turns a prefab by quarter turns about Y and may mirror it in X (the generators want
  variety; mirror swaps `facing` in `Named` shapes).
- **Resolved form**: palette → `BlockId`, shapes → `ChiselData`, bounds and caps checked, plus two things
  computed once: the **column profile** (per (x, z): top height and top material — for the far pass and
  for previews) and the **footprint mask**. Resolving returns a list of problems and never panics.
- **No size cap on built-in prefabs** (decided 2026-11-14: the library has monuments of 672 × 237 × 665).
  Size is paid for by structure instead: the resolved form buckets its boxes and shaped blocks on a
  chunk-sized grid, so a chunk's layout reads only what overlaps it. Caps (extent, box count) exist only
  for **untrusted** prefabs — a player's library and runtime stamps (stage H), where one stamp must not
  stall a region.
- Built-in set: `assets/prefabs/**`, embedded by `game/build.rs`, installed into a process global
  (`install_prefabs`); native prefers the copy on disk. Referred to by name; the key in state is the
  crc32 of the name, with a test that no two collide. `worldgen::cache::cache_key` gains the set's hash.

### 4.2 One layout routine

`prefab::layout_chunk(prefab, anchor: world block, turn, mirror, chunk: ChunkKey) -> Option<ChunkStamp>`
(and `chunks_touched(..)` for the callers that want them all) — per chunk, never the whole prefab at
once, so a monument costs a region only its own share. `ChunkStamp { carve: Vec<BlockIndex>, writes: Vec<(BlockIndex, Block, Option<ChiselData>)> }` in fixed
order. Both paths consume it: worldgen writes it into columns, the runtime order feeds it to
`set_chunk_blocks_safe`. A test asserts both give the same blocks, across a chunk edge and a region edge.

### 4.3 Worldgen placement

A **placement source** is anything that answers `placements_in(world box) -> Vec<Placement { key,
anchor, turn, mirror, pad }>` as a pure function, independent of the box asked. Sources: the dev gallery
(stage C), the site plans (stage G). `columns_from_patch` gains a structure pass after trees:

1. Ask every source for placements whose **bounds** touch the window. A source keeps its placements in a
   spatial index by bounds (pad skirt included), so there is no fixed margin and no limit on how far away
   an anchor may be. Placements anchored in other regions are clipped here — a prefab on a region edge, or
   one spanning several regions, comes out whole from every side (the old spec's rule; trees dodge seams
   instead, which a town cannot). A pad's level is fixed in the placement, not sampled by each window, so
   `prefetch_region`'s pinned tile rectangle does not grow.
2. **Pad**: a placement may level the ground under its footprint (`pad: { level, skirt }`). The pad edits
   the sampled surface before columns are filled: inside the footprint the surface is `level`, across
   `skirt` blocks it blends back. Slopes, soil depth and water see the padded surface. Where pads meet,
   any placement's own box wins before any skirt (then list order). No tree stands within reach of a
   footprint (a crown from just outside may overhang), and no pad column under a box is sloped.
3. Carve, then write. Overlap between two placements resolves by a fixed rule (the LATER placement in
   the source's order wins — as built); a generator is expected not to overlap.
4. The column band grows: `hi` to the top of the stamp, `lo` to its bottom, so cellars and dungeon rooms
   are real chunks, not implied ground (the band reaches one block below the stamp, and neighbouring
   columns' bands follow). Nothing is written at or below bedrock (`surface − UNDERGROUND_DEPTH`); a
   placement that would is clipped, silently for now.
5. Water: a prefab carries none. A block written into recipe water stands in it. A pad whose level is
   below the water top is flooded — as built, the pass does not prevent it; the placer must not choose
   such a level. A pad above the water top removes the water there.

**Far rings**: `coarse_from_patch` gets the same pass in coarse form — the pad on the heights, then each
placement's column profile rasterised into the cells, the way tree crowns are. Sub-block rings and saved
or running regions need nothing.

**Fixtures at genesis**: `worldgen::fixtures_in(region) -> Vec<Fixture>` reads the same placements and
turns markers into fixtures (region coordinates). The fresh-genesis path calls it beside
`Region::from_columns` and seeds `RegionMeta`: a `chest`/`Loot` marker gets its `Container`, the rest are
stored as the roadmap's typed fixtures. Layout is derived; only fixture state is saved.

### 4.4 Ground fit

`Flat` — wants a pad at one level (a house). `Follow` — laid column by column on the surface, each
column shifted to the ground under it (roads, palisades, field walls). `Buried` — anchored relative to
the surface but carved in (cellars, dungeon pieces). A `Flat` prefab gets a **foundation**: its lowest
solid layer is extended down to the padded ground, so nothing floats.

### 4.5 Runtime stamping (stage H, not MVP)

Order `StampPrefab { key, anchor, turn, mirror }`, developer-grade as in the old spec. The owner applies
its chunks with one batched write each. Neighbours get a new **batched intent** carrying a `ChunkStamp`
— `Intent` is `Copy` today, so this is a new non-`Copy` carrier beside it, not a new variant **(call)**.
It closes the three gaps `TODO.md` lists for the stud version (per-block intents, the 256 cap, seed/
runtime drift). Rollback bar and `just test-threads` apply. This is what later gives the dev order, sites
founded during play, and player blueprints.

## 5. Minecraft import

### 5.1 Shape of the tool

`crates/mc-import` (library: readers → `BlockVolume` → mapping → prefab file form; depends on `game` for
names and shapes only) and `tools/mc-import` (CLI, `just mc-import`), as the research proposed. Our own
reader on `fastnbt`; no GPL schematic crate. Native only.

Because the samples will arrive as **worlds**, the reader order changes: **Anvil world + box first**,
then Sponge `.schem`, then `.litematic` and structure `.nbt`. The research's format notes (2026 palette
rename, both bit-packings, height range from the data) are the reader's checklist.

Commands:

| Command | Does |
|---|---|
| `mc-import map <world> [--dim] [--box] -o map.png` | Top-down picture with a coordinate grid and a height shade — how boxes get chosen without opening Minecraft |
| `mc-import cut <world|file> --from x y z --to x y z --name n [--anchor] [--air auto\|carve\|keep] -o …` | One prefab + a report |
| `mc-import batch <cuts.ron>` | Every cut in a cut list |
| `mc-import report <world|file> [--box]` | Block census and what would not map, writing nothing |

A **cut list** (`tools/mc-import/cuts/<world>.cuts.ron`, in git) is the durable record: source world
(path + hash), then per cut a name, box, anchor, ground fit, tags and markers added by hand (connectors,
fixtures). Re-running `batch` after a mapping or material change rebuilds every prefab; nobody edits an
imported prefab by hand.

### 5.2 Blocks to blocks

Mapping is `base material × form`, from `assets/mc_import/materials.ron` (name pattern → our block name,
with the alias list for renames) and form rules by suffix and block-state properties.

| Minecraft | Becomes |
|---|---|
| Full cube | Whole block; same-material cubes greedy-merged into boxes in fixed x, z, y order |
| Slab, stairs (all `shape`s, both halves), wall, fence, pane, bars, carpet, snow layers, trapdoor, path, farmland | `Named` shape — exact at 16 voxels per block (slab 8, post 4 centred, pane 2, carpet 1, layer 2 each) |
| Door, gate | Opening left empty + `door` marker with facing |
| Chest, barrel | Our `chest` + `loot` marker (contents do not map) |
| Torch, lantern, bed, sign, banner, ladder, plants, redstone, rails | Dropped + marker by kind |
| Water, lava, `waterlogged` | Dropped; counted in the report |
| Logs, leaves | Our log / leaves blocks with `data = 0` — never a tree id, so felling ignores them |
| Air | Flood fill from the box faces: reachable air is **keep**, enclosed air is **carve** |
| `structure_void` | keep |
| Unmapped | Nearest of our blocks by a hand-picked colour, class before colour; listed in the report |

Deterministic throughout (`BTreeMap`, fixed scan order): same input and table → byte-identical prefab.
The prefab's `meta.import` records source hash, box, data version and mapping version.

### 5.3 The building set (stage A)

Imports are only as good as the block list, and so is anything a generator builds. Add a **building
set** to `blocks.ron` **(call: about 90 blocks)**: planks ×7 species, cobblestone, stone bricks (plain,
mossy, cracked), brick, dressed sandstone, mud brick, thatch, roof tiles ×3, plaster ×4, 16 dyed cloth,
16 fired clay, iron, gold, copper, glass (Cutout: a frame with clear panes), bars, bookshelf, hay,
packed earth, cobbled road. Textures are ours, 16 × 16, from `tools/textures/gen.py` — **no Mojang
texture and no colour table derived from one ships**. Ids append; `EMBEDDED_BLOCK_TEXTURES` grows with
them. The report from the first real import decides what else joins.

### 5.4 Legal

Unchanged from the research §9. In practice: a prefab carries `author`, `source`, `licence`; `batch`
writes to **`prefab-imports/` at the repo root** (git-ignored, and outside `assets/`, which the web release copies
whole and `game`'s build script embeds whole — decided at the plan-1 final review). A prefab is *promoted* into
`assets/prefabs/` only once its cut list records a licence; the loader refuses a built-in prefab that names
a `source` without a `licence`. Read the EULA and Usage Guidelines before any imported build ships.

## 6. Sites in the world

The hard question is how worldgen learns where sites are, since history is runtime state.

**Decision (call): frozen site plans.** When a world is created — after the forge and the history
bootstrap, before any region generates — the coordinator writes a `site_plans` table once: `{ plan id,
kind: Town | Fortress | Dungeon | Lair, anchor (world block), radius, seed, size tier, site: Option<SiteId>
}`. It is immutable afterwards, loaded by every process at start and installed for worldgen like the
forged world (`sites::install`). Generation reads the installed plans, never history.

- Towns and fortresses come from the bootstrapped civ sites (tier from population at the end of the
  bootstrap); the anchor is chosen inside the site's region by a suitability scan (flat, dry, near
  water). The site's people then spawn at the town's `Spawn`/`RestPoint` fixtures, not at `landing`.
- *(Later, with dungeons.)* Dungeons and lairs come from a seeded scatter over the world map (steep or
  remote tiles, a minimum spacing), with no civ. Stage F writes town plans only; the `kind` column leaves
  room.
- `Site` gains `kind` and `plan: Option<PlanId>`; the world map's site markers show the kind.
- Sites founded later (a player embark, a migration) have no plan: players build their own; generated
  buildings for AI-founded sites wait for runtime stamping (stage H).
- Written once by one process, read-only after: it is not a table two hosts' parts write. A world without
  the table (forged before this) simply has no generated sites; getting them means a re-forge.

Alternatives considered: sites as a pure function of the world map with history snapping to them (no
table, but history and worldgen then share a second placement rule and a code change moves every town of
an existing world); stamping at founding by runtime orders (needs stage H first, changes saved regions
only, and far rings of unvisited towns stay bare).

## 7. Site generators

One output type for all three, in `worldgen/src/sites/`:

```text
SiteLayout { placements: [Placement], pads, strokes: [Stroke { kind: Road|Wall|Ditch, path, width, mat }],
             fixtures: [Fixture], bounds }
```

`layout_for(plan) -> Arc<SiteLayout>` is a pure function of the plan, the forged world and the prefab
set, cached per process. It reads terrain through the surface profile only (no generated chunks), uses
integer math and stateless rolls (`roll(seed, subject, salt)`), and is capped in radius per kind so the
set of plans touching a window is found from the plans' bounds. Strokes are procedural (a road is a
ribbon of `cobbled road` following the ground, not a prefab).

Two techniques **(call)**:

- **Lots** (towns): a road skeleton from the centre along the gentlest ground → lots along roads, sized
  in classes → each lot filled by a prefab picked by tag and footprint class, turned to face the road,
  padded to its own level. Required pieces are placed first (a square with a well and a notice board, an
  inn = `RestPoint`), then houses by tier.
- **Jigsaw** (*later* — dungeons, fortress interiors): pieces carry `connector` markers (`pool`, facing, size); grow
  from a start piece, breadth-first, picking from the pool by roll, rejecting a piece whose bounds hit one
  already placed or leave the allowed volume, capping open ends with a dead-end piece. A dungeon is an
  entrance on the surface, a descent, rooms and corridors within the 60-block ground, `Spawn`, `Loot`,
  `Boss` fixtures by depth.
- **Fortress** (after towns) = a wall ring (a `Wall` stroke along a perimeter polygon with tower and gate
  prefabs at its corners and on the road) + lots inside + a keep (one prefab until jigsaw exists). A
  walled town is the same thing with a town inside, so the wall ring is built once for both.

Kit needed before a generator can be judged (what the sample worlds are for):

| Kind | Pieces |
|---|---|
| Town | 8–12 houses in 3 footprint classes, inn, smithy, market stalls, well, a few fillers (shed, cart, haystack) |
| Fortress | Wall segment, corner tower, gatehouse, keep pieces (hall, stair, chamber), barracks, storehouse |
| Dungeon *(later)* | Entrance, stair down, corridor straight / turn / T / cross, rooms S / M / L, boss room, dead end — none in the library today |

Review without a client: `just site-preview <kind> <seed> [--at x z]` writes a top-down PNG of a layout
on real terrain (footprints, roads, fixtures); `just prefab-view <name>` writes four elevations and a plan
of one prefab; `client debug gallery` shows every built-in prefab on a flat strip.

## 8. Stages

| # | Stage | Delivers | Main touch points | Needs |
|---|---|---|---|---|
| A | Building set | ~90 blocks with textures; the shape catalogue producing canonical chisels | `assets/blocks/`, `tools/textures/gen.py`, `game/src/prefab/shapes.rs`, client `EMBEDDED_BLOCK_TEXTURES` | — |
| B | Prefab core | File form (RON + bin), resolve, caps, column profile, `layout`, built-in set install, `just prefab-view`, `just prefab-check` | new `game/src/prefab/`, `game/build.rs`, `assets/prefabs/`, justfile | A |
| C | Worldgen stamping | Placement sources, the structure pass (margin, pad, band, carve, seams), the coarse pass, genesis fixtures + containers, `Fixture` types in `RegionMeta`, the dev gallery and a hand-placed **test town** (half of the adventure roadmap's stage 0; its test cave waits for dungeons or is dug by hand) | `worldgen/src/lib.rs` (`columns_from_patch`, `coarse_from_patch`, `prefetch_region`), new `worldgen/src/structures.rs`, `game/src/region.rs`, `portable.rs`, `cache.rs`, `debug_launch.rs` | B |
| D | Minecraft import | `mc-import` library and CLI: Anvil + box, `.schem`; mapping table, forms, air fill, report, `map`, cut lists | new `crates/mc-import`, `tools/mc-import`, `assets/mc_import/`, justfile | B |
| E | Sample kit | Cut lists for the supplied worlds; connectors, tags, fixtures marked; gaps in the building set filled; the kit of §7 reviewed in the gallery | `tools/mc-import/cuts/`, `assets/prefabs/`, `blocks.ron` | C, D, the worlds |
| F | Sites in the world | `Site.kind`, `site_plans` table and migration (SQLite + Postgres), written at world creation, installed at start in every role; map markers by kind; people spawn at fixtures | `history/records.rs`, `bootstrap.rs`, `world-store/migrations`, `server/src/world_file.rs`, `worldgen/src/sites/` | C |
| G | Town generator | `SiteLayout`, strokes, `site-preview`; the lot-based town, in a style per climate (desert towns from the desert packs first — the library's bulk); then the wall ring → walled towns and fortresses | `worldgen/src/sites/{layout,strokes,town,walls}.rs` | E, F |
| — | *Deferred* | Dungeons, lairs, the jigsaw technique | | a dungeon kit |
| H | Runtime stamping | `StampPrefab`, the batched stamp carrier, rollback and seam tests; dev order | `orders.rs`, `intents.rs`, `edit.rs`, `world_manager/` | B |

D runs beside C; H is independent of C–G and can wait. The first thing visible in the game is the end of
C (a hand-built test town in the debug world, near and far); the first imported building is the end of D.

### Order around the R4 takeover work

R4 (in progress, 2026-11-14) lives in `world_manager/`, the world store (`handle.rs`, `rows.rs`,
migrations `0012`/`0013`), the server's roles / claims / cuts, the coordinator and the multi-host tests.
It touches nothing in `crates/worldgen`, `assets/` or a prefab module. So:

- **Run now, beside R4:** A, B, D, E, **C1** (the worldgen half of C: structure pass, coarse pass, dev
  gallery and test town as terrain only) and **G**, fed by a **dev placement source** (towns at fixed
  spots in the debug world) instead of the site table.
- **Wait for R4 to land:** **C2** (fixtures and containers at genesis — changes `RegionMeta`, which cuts
  save and an heir adopts), **F** (a migration and the store's busiest files; take the next free migration
  number then) and **H** (a message crossing host seams).
- **The structure pass is a no-op unless a placement source is installed**, and only debug presets
  install one, so worldgen output, goldens and the go/no-go runs are unchanged for R4.
- Shared-checkout care: commits name only this work's files; `justfile` (hot, with staged and unstaged
  changes) takes additive recipes only; one-line touches to `game/src/lib.rs` and the root `Cargo.toml`.

Checks every stage plan must carry, where they apply:

- Window independence: `generate_columns` over any split equals `generate_region` with placements
  present, including one anchored in the neighbouring region. At region 64 a prefab can span three.
- Near/far agreement: a coarse tile over a placement shows it; no structure is in one path only.
- Layout equality between the worldgen and runtime paths (H).
- `just determinism-wasm` for C, F, G, H; `scene_hash_golden` re-recorded on purpose in C;
  `just test-threads` for C (halo seeds) and H; the `forge_quality` gate stays green.
- Hostile input: truncated, oversized and nonsense prefab files and Minecraft files are refused with a
  message; nothing panics.
- Importer: fixtures written by the tests, a golden per form, a byte-identical re-run.
- `WORLD_MAP_VERSION` bumps if F changes what `build_world_map` returns.
- Client tests are ask-first; the gallery and the test town get one look on a real adapter.

### Looking at a prefab in the game

```
just run --reset --prefab dev-hut
VOXEL_PREFAB_DIRS=prefab-imports/statuen just run --reset --prefab "statuen-06;statuen-07"
```

- `--prefab NAME[:DX,DZ[:TURN]]` (repeatable; the client joins the flags with `;`) makes the client start
  its server with `VOXEL_DEV_STRUCTURES=home=<spec>`: the prefabs stand beside the landing column (else the
  centre) of the HOME region only, the first region to start running on this server and be generated fresh (the server's
  region-thread generator announces it; a region restored from the store does not), which in the debug
  launch is the embark region; laid out as a row along +x without offsets, each on a pad levelled to the
  ground at its anchor. Other regions are untouched. The embark is chosen as without the flag.
- `VOXEL_DEV_STRUCTURES` by hand has these forms (besides `town=` above): `home=<spec>` (above), `all=<spec>` (the same beside
  EVERY region's landing, laid out lazily per region) and `<rx>,<rz>=<spec>` (one named region, an
  explicit list; the server logs the placement count). `home` is order-dependent by design: a region or far
  tile generated before the home is fixed shows no prefab. With any of the three the server turns the
  worldgen disk cache off (a cached region would carry or lack the prefab independently of the run).
- A town: `just town medieval` (or `just town medieval:2:64`; `client debug embarked --reset --town
  STYLE[:ARMS[:LEN]]`, refused with `--prefab` and `--join`) starts the server with
  `VOXEL_DEV_STRUCTURES=town=<style>[:<arms>[:<arm_len>]]`: one town (`sites::town::layout_town`: a plaza,
  1..=4 streets of 16..=256 blocks, houses of the style's kit on lots facing them) beside the landing of the
  HOME region, as `home=` does. The kit comes from the installed prefab set, so the recipe adds every
  sub-directory of `prefab-imports/` to `VOXEL_PREFAB_DIRS`; a style with fewer than three tagged houses is
  a startup error naming it. The road and plaza blocks are `cobbled_road` and `smooth_stone`.
- `VOXEL_PREFAB_DIRS` (`:`-separated, relative to the current directory) adds prefab directories to
  `assets/prefabs`; the built-in set is always installed. A missing directory is a startup error.
- It needs a region never saved before: use `--reset` (a saved region is never regenerated, so an
  existing world shows no structure). With neither variable set generation is untouched.

## 9. Decisions that change the plan if wrong

1. **Frozen site plans (§6)** rather than sites derived from the map alone, or stamped at runtime.
2. **A building set of about 90 new blocks with our own procedural textures (§5.3)**, glass as cutout,
   no lights — rather than importing onto today's 47 blocks first.
3. **Imported builds stay out of git until a licence is recorded (§5.4).** If the sample worlds are
   yours or freely licensed, this is a formality; if not, they are references for the generators' look
   and the shipped kit has to be rebuilt by hand.
4. **Doors, furniture and light sources are markers only** for now: openings stay open, interiors are
   lit by sky light alone. Dungeons will want light sooner than towns.
5. **Worldgen first, runtime last (H)**, and the player library and editor after that. **Towns first,
   dungeons deferred** (decided).
6. **Depth**: built-in prefabs have no size cap (decided), but anything underground lives in the 60
   blocks above bedrock. A deeper dungeon means raising `UNDERGROUND_DEPTH`, a terrain change of its own.
7. **Existing worlds get no sites**; a re-forge is the way to see them.

## 10. What is needed from the sample worlds

**First sample (2026-11-14):** `~/Downloads/Statuen/statuen.schem` — Greek statues spaced out on a flat
world, **bought by Dawid** (author "Timtenth", made with Axiom 4.7.1). Sponge schematic v2, data version
3465 (1.20.1), 729 × 164 × 296 blocks, 376 block states, 195 block entities (135 signs, 45 lecterns, 15
campfires — likely labels per statue, not yet read). Consequences: the Sponge v2 reader comes **before**
the Anvil reader in stage D; `mc-import` should offer an automatic split of a spaced-out schematic into
one cut per connected build, named from the nearest sign. Its `licence` field is "purchased"; check the
seller's terms on redistribution inside a game before the first release that ships them.

**The sample library (2026-11-14):** `mc-sources/` at the repo root (git-ignored; it must never sit under
`assets/`, which `scripts/web_release.sh` copies whole into the public build). 39 archives: 390 unique
Sponge v2 schematics (data versions 2730 = 1.17.1, 3465 = 1.20.1, one 4671) and three Java world folders
(`TestKingdom` — 1.7 GB, modded: `easy_npc`, `voxy`; `Pyramide`; `Trees`), not yet censused. Themes:
desert / Egyptian (the bulk), Asian, temples, medieval and stacked houses, walls, gates, towers, a castle,
harbours, arenas, statues, 75 decorations. No dungeon pieces.

Block census of the schematics (`mc-sources/_census/{blocks,states,files,block_entities}.tsv`): 48.5 M
non-air blocks, 549 block names, 3,696 states, all vanilla. What it says for the plan:

- **Forms**: 87 % whole blocks, then stairs 4.5 %, slabs 3.5 %, walls 3.4 %, fences 0.7 %, trapdoors
  0.4 %; everything else under 0.1 %. Stage A's shape catalogue is those five plus panes and carpets.
- **Base materials**: 388, but 20 cover 88 %, 60 cover 97 %, 100 cover 99 %. The building set of §5.3 was
  drawn for a medieval kit and is wrong for this library: it needs smooth / cut / plain sandstone, smooth
  quartz and quartz pillar, stripped woods (birch, spruce, oak, dark oak, and a teal one for
  `stripped_warped_hyphae`), concrete and terracotta colours, stained glass, bone, andesite / diorite,
  deepslate brick, prismarine, gold. Draw the set from `blocks.tsv` (weigh the `files` column, not only
  the count — five giant pyramids are a third of all blocks).
- **Sizes**: a dozen schematics are over 256 blocks on a side (the harbour is 672 × 237 × 665). Layouts
  of many builds are split by the importer into connected builds; single monuments import whole — there
  is no prefab size cap (§4.1).
- **Not blocks**: water is 8 % (harbours) and has no place in a prefab yet; `barrier` (0.4 %) maps to
  keep; 15,844 signs, 8,733 campfires, 6,064 banners, 4,236 barrels are the markers that matter.

- Java Edition worlds (any version from 1.13; newer is fine), as world folders or `.schem` files, with
  where each came from and under what licence.
- Per world, a rough note of what is worth cutting ("the harbour town at about x, z"); `mc-import map`
  takes it from there.
- Variety matters more than size: several small houses beat one cathedral, and pieces that repeat (wall
  runs, corridor sections) are what the generators live on.

## 11. Carried over from plan 1 (blocks and core — done 2026-11-14)

Plan `2026-11-14-prefabs-1-blocks-and-core.md` is built: 121 building blocks, the shape catalogue, the
prefab file (RON and binary), resolve, `layout_chunk`, the built-in set with `dev-hut`, `prefab view` /
`prefab check`. Open items, in the order they start to matter:

**First step of the next plan**
- `PrefabSet::from_sources` (`crates/game/src/prefab/builtin.rs`) keys its sources by file name: two files
  of one name in different sub-directories silently lose one. Refuse a repeated name; test
  `a.prefab.ron` + `more/a.prefab.ron`.

**Left uncommitted for the owner of those files** (they held another session's work in progress)
- `crates/client/src/renderer/mod.rs`: 124 `EMBEDDED_BLOCK_TEXTURES` entries. Until committed, the client
  tests `embedded_block_textures_*` fail on a clean checkout and the wasm client draws the building set as
  missing textures. The list was checked by script against `assets/blocks/*.png` (182 = 182); the client
  tests themselves were not run.
- `justfile`: recipes `prefab-view` and `prefab-check`.

**Before the importer (stage D)**
- `deny_unknown_fields` does not reach the struct variants of `FileShape` and `Shape`; a typo there still
  passes.
- `Cuboids` chisels are rebuilt per cell in `layout_chunk`; intern identical shapes at resolve.
- `layout_chunk` allocates a `BTreeMap` per call; `cell()` scans a bucket's boxes linearly. Measure on the
  first real import before changing either.
- `prefab view`: elevations look through door gaps (add depth shading), are slow on 100 M-cell prefabs
  (per-slab `Vec` + `BTreeMap`), `--scale` is unbounded above, no test pins panel orientation, and a name
  is only looked up at the top level of `assets/prefabs`.

**Before any `Trust::Untrusted` caller (stage H)**
- `occupied_cells` is outside both untrusted work budgets (65 536 thin tall boxes pass and cost ~10⁹ cell
  writes); `max_bytes` is ignored for RON input.

**Whenever**
- `install_prefabs` is process-global: the first test that installs a set needs its own `[[test]]` target
  or must install exactly the built-in set.
- Plain `i32` arithmetic on anchor + offset in `layout.rs` panics in debug at coordinates near 2³¹.
- Shape extents in `shapes.rs` are literals rather than named consts; an open trapdoor ignores `half`.
- `build.rs` and the disk loader differ on a `read_dir` error (skip vs fail); Cargo's directory watch
  re-runs the script for dotfiles it then ignores.
- `tools/textures/gen.py`: `mix` aliases `lerp`.

## 12. Carried over from plan 2 (Minecraft schematic import — done 2026-11-14)

Plan `2026-11-14-prefabs-2-minecraft-import.md` is built: `crates/mc-import` (NBT + Sponge v2/v3 reader,
classification of block states through `data/materials.ron`, volume → prefab with trim, air flood fill and
box merge, a splitter for showroom files) and the `mc-import` tool (`report`, `import`, `split`). On the
sample library: **482 of 482 schematics import and resolve, zero unmapped blocks**, the whole run in about
20 s. Output goes to the git-ignored `prefab-imports/`. The step-1 item of §11 (repeated file names) is
fixed.

What changed against §5 as first written: only Sponge `.schem` is read (worlds, litematic, structure NBT,
cut lists and the map picture are not built); sign labels are opt-in (`--labels`) because real signs are
decoration; closed fence gates are fence shapes, barrels are planks; a door cell stops the air flood and is
carved; `split` with default ground falls back to a whole-file import when it finds fewer than two builds.

**Left uncommitted for the owner of that file:** `justfile` — recipe `mc-import`.

**For the kit stage (E), in the order they will bite**
- **Interiors.** A house whose windows or doorless openings are open still lets outside air in, so many
  real houses carve 0–30 cells; stamped on uneven ground, terrain would stay inside. Import those with
  `--air carve` on a tight box, or have the stamper clear under roofs — decide per pack.
- **`split` is for showrooms only.** It cut `stadium`, `castle` and `haven` into 15–17 pieces; those are
  single compositions and should be `import`ed. `report`'s build count is the cue. Overlapping build boxes
  are warned about, not masked (2 real cases each in `statuen` and `Pyramide2`).
- **Anchor and ground fit** are fixed (box centre, y = 0, `Flat`) with no flag; a build imported with its
  plate has its anchor under the plate. The tool needs `--anchor` and `--ground-fit` (or the cut list).
- **Names.** File stems repeat across packs (`a2`, `c2`, …): give `--name` / `--prefix` per pack. A prefab
  is promoted into `assets/prefabs/` only by re-importing with `--licence`.
- **Mapping calls worth a look** (one table line each, `MAPPING_VERSION` 2): magma → basalt, moss →
  green_terracotta, dead coral → andesite, obsidian → basalt, blackstone → the deepslate family, crimson →
  stripped_mangrove, dark_oak log → stripped_dark_oak, nether bricks → deepslate_bricks, lapis →
  blue_concrete, purpur → magenta_terracotta, oxidized copper → prismarine, weathered copper →
  cyan_terracotta; furnaces and the like are plain blocks; water (3.9 M cells) and plants are dropped.
- Two markers for one bed or double chest; split boxes are tight to solids, so a torch or sign on an outer
  face is cut off; lit state of lamps and campfires ignored; `dirt_path` / `farmland` are whole blocks.

**Smaller, whenever**
- One air cell above a door with no upper half is carved even outdoors; a sign inside a build dropped for
  size can still label a neighbour (labels are opt-in).
- A `.prefab.bin` whose report file fails to write is left without its report.
- A number or boolean inside a sign's `extra`, and `translate` / `keybind` components, lose that line.
- `*_copper_bulb`, `*_copper_chest` fall back to `smooth_stone` (none in the library).
- No fixture test pins "resolve before write"; several tests could not be seen failing before the code
  existed (noted in the task reports).

## 13. Carried over from plan 3 (worldgen stamping — built 2026-11-14)

Plan `2026-11-14-prefabs-3-worldgen-stamping.md` is built: `worldgen::structures` (placements, the
`StructureSource` trait, `StructureList`, the ground pad), the structure pass in full generation and its
far-terrain twin, `ground_y`, a dev source and the `--prefab` debug flag (see "Looking at a prefab in the
game" in §8). With no source installed generation is byte-identical to before, pinned by checksums that
were verified against the pre-plan generator.

**Constraints on the site plans and generators (stages F, G) that this stage found**

- **A source must not sample terrain while generating.** The dev source calls `ground_y` on demand, also
  for neighbouring regions, outside the forged-tile rectangle a region pins; a failed load there panics a
  region or LOD thread. Tolerable for a dev flag. The real source takes every pad level from the frozen
  site table (heights sampled once, when the plan is written at world creation), so a placement query is
  pure data.
- **Far rings need a cheap answer.** A far key spanning many regions asks for every placement in its
  span; with towns of hundreds of buildings that is too much per tile. Expect a per-site silhouette (or a
  per-site bounds check that skips sites too small to show at that factor) rather than per-building work.
- **Pads are per placement.** There is no shared terrace for a group of lots and no road grading; a town
  on a slope gets one pad per building, each with its own skirt. The generator either sizes skirts to the
  height difference, picks sites by relief, or this stage's pad grows a "site pad".
- **Per-sample cost.** `pad_at` and `blocks_trees` scan every nearby placement for every sampled column
  and recompute its bounds; fine for a handful, not for a town. Precompute bounds per query, or rasterise
  each pad over its own rectangle.
- **Not built:** `GroundFit::Follow` (roads, walls following ground), a foundation for a placement
  without a pad (far terrain draws such a prefab as a pillar down to the ground), fixtures and containers
  at genesis (stage C2, waits for R4), structures in water.

**Smaller**

- `ColumnGround.surface` stays at pad level over a carved cellar; the bedrock clip is silent.
- A skirt over glacier ground strips the ice and can leave an ice cliff at its outer edge.
- On a hillside the dev skirt (4 blocks) leaves 2–3 block steps; a generator should not copy it.
- A broken built-in prefab file aborts every server start (kept loud on purpose).
- A world started once with `--prefab` keeps its saved huts; a later run without the flag generates
  neighbouring regions without them. Nothing records which source a world was generated with.

**Checks owed, and hunks left for their files' owners (plan 3)**

- `just test-threads` did not pass when this stage finished: 7 `game` `peer_schedule::` cut tests failed,
  in code the R4 work had open and uncommitted at the time; `handoff_schedule::` (28), `voxel_body` and
  `threaded_world` passed, and `just determinism-wasm` passed. Nothing of this stage touches the peer,
  coordinator or world-manager code — but the check is owed: re-run it once R4 settles.
- Nobody has launched the game with `--prefab` yet (it opens a window): `just run --reset --prefab
  dev-hut` is the first live look. Client tests were not run (ask-first), including the new
  `debug_embarked_collects_prefabs`.
- Left uncommitted (their files held other people's work): `crates/client/src/debug_launch.rs` (the
  `--prefab` flag, its USAGE text, its test, a warning when used on an existing world) and `justfile` (a
  comment on the `run` recipe) — alongside plan 1's `EMBEDDED_BLOCK_TEXTURES` entries in
  `crates/client/src/renderer/mod.rs` and the `prefab-view`, `prefab-check` and `mc-import` recipes.
