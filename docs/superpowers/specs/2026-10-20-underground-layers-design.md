# Underground layers — design

2026-10-20. Status: **approved by Dawid (2026-10-20).** Implementation plan: `../plans/2026-10-20-underground-layers.md`.
Context: the Dwarf Fortress direction (CLAUDE.md "Direction"); features are built piece by piece.
Reference research: `2026-10-20-dwarf-fortress-research/02-map-and-z-levels.md` §4–5 and
`engine-mapping.md` §2. This is the first piece of the world map → embark → save/load line of
work, which needs a real underground first.

## Intent

A fortress is dug into the ground, so the ground has to exist. Today it doesn't: below a thin
surface band, the world is nothing (see "Current state"). This piece makes every region a solid
block of ground **60 levels deep**: soil at the top, then the **stone layers of the forge's
geology**, then a **bedrock floor**. It is the ground dwarves will later dig and that geology-aware
embark choices will be made on.

## Decisions (Dawid, 2026-10-20)

1. **Lazy, not stored.** The underground is a pure function of position and the world data;
   nothing below today's surface band is stored until it is exposed. (Option B of the discussion;
   full materialisation at region start was rejected.)
2. **Depth: 60 levels below the surface**, ending in a bedrock floor.
3. **Layers only**: soil, then stone layers from the forge geology, then bedrock. No caves,
   caverns, ores, gems, aquifers or water underground.
4. **Region size 256 m** (`REGION_CHUNKS = 128`) **for playing only**: the game recipes build at
   128; tests, the cargo env default and tooling stay at 32 (64 m). A full rebuild when switching
   between running and testing is accepted.
5. **Not a procedural world.** The world is the forge's pre-generated `.world` file; a region is a
   part of it generated at much higher resolution (what `forged.rs` already does for the surface).
6. **Checking it**: digging in first-person (the existing break/place), plus a cross-section image.
7. **One level = one block (1 m).** So the floor is 60 blocks below each column's surface.
8. **The forged world is the default; the procedural generator stays.** The server uses a forged
   world unless told otherwise, and **forges one on start if it doesn't exist on file yet**.

## Goals

- Digging down from any surface point reveals soil, then stone layers matching the forge
  geology, down to bedrock 60 levels below that column's surface.
- Breaking a block never opens a hole into nothing: the faces around it are real blocks you can
  stand on and see.
- A 256 m region costs about what it costs today at start-up, in memory and in snapshot size,
  however deep the ground goes. Cost grows only with what has been dug.
- Deterministic: the same world file gives bit-identical underground on server, native client and
  wasm; client prediction of a dig produces the same chunks the server does.
- A cross-section image shows exactly what the engine will generate.

## Non-goals

Caves, caverns, ores, gems, aquifers, magma, underground water; soil depth that varies with slope
or altitude; geology for the procedural generator; hidden/unrevealed tiles; any DF tile model;
cell mode beyond "keeps working" (see Constraints); LOD changes.

## Anti-goals (would make this a failure even if it works)

- Region start, memory or snapshot growing with the depth of the ground.
- Holes: a dig exposing a face of unmaterialised space that renders as a gap or lets a body fall.
- Client and server disagreeing about what a dug-out neighbour contains (desync).
- The cross-section image and the engine disagreeing.

## Current state (code, `develop` at `b58f6bb`)

- `worldgen::generate_region` (`crates/worldgen/src/lib.rs` ~180–245) materialises, per 2×2-block
  column footprint, only block Ys from `min surface − MAX_SOIL_DEPTH (3)` to the highest
  surface/water top. Below that the `Column` holds a constant `below = stone` sentinel.
- `Region::from_columns` (`crates/game/src/region.rs:688`) turns only the materialised sub-chunks
  into chunk entities (with rapier colliders). The sentinel is dropped: **in the sim, a missing
  chunk is air.** `set_block_uncounted` (`crates/game/src/edit.rs:195`) treats a missing chunk as
  air (breaking does nothing; placing creates a fresh air chunk).
- The client mesher treats missing neighbours as air too (`renderer/meshing/neighbors.rs:57`,
  "over-draws, never holes").
- So digging to the bottom of the band opens into empty space with no collider.
- `game` does not depend on `worldgen`; generation is injected into the `WorldManager` as a
  closure (`RegionGenerator`). The native client does not have the world file.
- The forge geology exists in the artifact but is unused by the engine: `WorldArtifact`
  (`crates/worldgen/src/forged.rs:99`) carries per-cell `uplifted` and `bedding` rasters and a
  formation `column` of `(top s, rock index)` with `rock_names` (sandstone, shale, limestone,
  conglomerate, basalt, granite — `examples/forge_proto/geology.rs`). Rock at a point is the
  formation containing `s = (y − uplifted) − bedding`, metres.
- `REGION_CHUNKS` default is 32 (`.cargo/config.toml` `[env]`); 128 is a legal value with its own
  LOD ring table and fixtures.

## Design

### 1. The underground function

For a block at region-local `(x, y, z)` in a column whose surface block is `surface(x, z)`:

| depth `d = surface − y` | block |
|---|---|
| `d < 0` | whatever the surface band says (air, water) — unchanged |
| `0 ≤ d ≤ MAX_SOIL_DEPTH` | today's soil rules (`solid_material`: grass/dirt/sand) — unchanged |
| `MAX_SOIL_DEPTH < d < UNDERGROUND_DEPTH` | the forge rock at that point |
| `d ≥ UNDERGROUND_DEPTH` | bedrock |

`UNDERGROUND_DEPTH = 60` (levels = blocks, decision 7), a named constant. The vertical range
worldgen scans (`SCAN_BOTTOM`, today block −64) must reach 60 blocks below the deepest surface in
the world, including the sea floor (~90 m below sea level around the island) — widen it or derive
it from `UNDERGROUND_DEPTH`. *(My call: derive it.)* The floor follows the terrain (each
column's own surface), not a flat level. *(Builder's call how the bedrock floor treats the blocks
below it: bedrock all the way down is the obvious choice.)*

Rock lookup: the existing formula, `s = (y_m − uplifted(x, z)) − bedding(x, z)`, formation by
`s`. To make it deterministic and cheap it is evaluated in **integers**: per block column,
precompute one `i32` offset (in blocks, or in a fixed sub-block unit) `= round(uplifted + bedding)`
from the artifact (sampled the same way `forged.rs` samples other rasters), and quantise the
formation tops to the same unit. Then `rock(x, y, z) = column_lookup(y − offset(x, z))` is integer
only. *(Builder's call on the unit and the interpolation of the rasters; the cross-section image
must use the same code.)*

### 2. The underground descriptor lives in region state

Every replica must be able to materialise a chunk the same way, including a client predicting a
dig — and the client has no world file. So the function's inputs travel with the region:

- At genesis, `generate_region` also produces an **`UndergroundDescriptor`** for the region: per
  block column the surface Y and the geology offset (two `i16`/`i32` per column — ~256–512 KiB for
  256×256 columns before compression *(estimate)*), plus the quantised formation table and the
  block ids for each rock and bedrock.
- It is stored in `GameData` (written at genesis, unlogged like the rest of genesis; serialized,
  hashed with `StateHash`), so it reaches clients in the region snapshot and parks/restores with the
  region.
- `game` gets a small pure function `underground_block(&descriptor, pos) -> Block` (soil rules
  included, so the band and the underground agree). `worldgen`'s band generation and the
  cross-section tool call the same function.

*(My call, recommended over the alternative of a server-authored "materialise" event like
`CellChanges`: a server-only event would make a predicted dig open onto unknown space on the
client until the server replies. Shipping the descriptor keeps digging fully predicted.)*

### 3. Missing chunk = implied ground

The meaning of "no chunk entity here" changes inside a region's ground:

- **Below the surface band** (a chunk coordinate under the column's surface): implied ground,
  content = `underground_block` per block.
- **Above it**: air, as today.

Every place that reads terrain must honour this when it finds no chunk: the block edit path
(`edit.rs` `set_block_uncounted`, the break/place raycast), and anything else that samples blocks
in the sim. *(Builder: find them all; `chunk_key_at` returning `None` is the seam.)*

### 4. Exposure materialisation

The invariant: **every air (or water) block that a body or the camera can reach is bordered only by
materialised chunks.** Then nothing outside the sim's materialised set can ever be touched, seen or
collided with, and implied ground needs no collider and no mesh.

- **On edit**: when a block becomes air, every chunk that is face-adjacent to that block and not
  yet materialised is materialised first, from the descriptor, as part of the same undoable edit
  (the chunk insert must have an exact inverse — like `create_mesh` today, which emits
  `RemoveEntity` on undo). Breaking the last block of a chunk boundary therefore pulls in the
  neighbouring 2×2×2-block chunk.
- **At genesis**: the band must already satisfy the invariant. A column next to a much lower
  neighbour exposes its side below its own band; today that rock is simply missing. Genesis
  materialises those chunks too (a chunk is materialised if any of its blocks is ground with an
  air/water face-neighbour). *(Builder: check whether today's band already leaves such holes on
  steep forged terrain; either way the rule above fixes it.)*
- Materialised chunks are ordinary chunk entities: render updates, colliders, chunk index,
  terrain fingerprint, snapshot — all as today.

### 5. Bedrock

A new `bedrock` block in `assets/blocks/blocks.ron` (appended id, own texture). It cannot be
broken: the edit path refuses to break it (and the raycast still hits it).

### 6. Rock blocks

Append one block per forge rock — sandstone, shale, limestone, conglomerate, basalt, granite — to
`assets/blocks/blocks.ron` with textures (a placeholder tinted texture each is fine for this
piece). The descriptor maps the artifact's `rock_names` to block ids by name; a world file naming
an unknown rock fails loudly at load.

### 7. The default world, and procedural worlds

**Forged by default (decision 8).**

- Today the forge is an example (`crates/worldgen/examples/forge_proto/`, ~2.5k lines, writes
  `<stem>.world`; ~30 s at 16 m cells) and the engine only *reads* world files
  (`forged.rs`, opt-in via `VOXEL_WORLD_FILE`). The forge's pipeline (world, geology, climate,
  erosion, post, hydrology, spawn, and the artifact writer) moves into library code the server can
  call — a `worldgen` module or its own crate *(builder's call; it must not pull new dependencies
  into `game` or the client)*. `just forge-proto`, `--search` and `just forge-ui` keep working as
  thin wrappers over it.
- The **server binary** resolves the world at start: an explicit `VOXEL_WORLD_FILE` /
  `just world_file=…` wins; otherwise it uses a default path (e.g. `worlds/default.world` at the
  repo root, git-ignored — *builder's call on the exact path*). If that file doesn't exist, the
  server forges it there with the forge's default knobs and default seed, logging progress (a
  `[perf] event=world_forged` line with the time), then loads it as usual.
- A file that exists but can't be read (wrong format version, corrupt) still fails loudly, as
  `forged.rs` does today; it is **not** silently regenerated *(my call: never overwrite a world
  someone may be playing)*. The error says to delete it to regenerate.
- Only the server forges and reads the world file. Clients never need it: terrain arrives in
  snapshots and the underground descriptor (§2) travels with the region.

**Procedural stays (decision 8)** — selectable explicitly (e.g. `just world_file=procedural …` or
an equivalent switch; *builder's call*), and it remains what the **tests** and the **wasm offline
world** (`LocalServer` in the browser) use: the library-level default is unchanged (no
`VOXEL_WORLD_FILE` → procedural), so no test ever triggers a 30 s forge. Procedural regions get
the same depth and bedrock with **plain stone** for all layers *(my call: one code path, and every
existing test exercises the lazy machinery)*.

### 8. Region size

Per decision 4: the **play recipes** (`just run`, `run-release`, `server`, `client`,
`client-release`, the wasm/web run, and the probes/benches that play the game) default to
`region_chunks=128`; `.cargo/config.toml` `[env]` keeps `VOXEL_REGION_CHUNKS = "32"`, so raw
cargo, rust-analyzer and every test/smoke/clippy recipe stay at 64 m and their fixtures and goldens
are untouched. An explicit `just region_chunks=… <recipe>` still wins. *(Builder's call how the
justfile expresses the two defaults.)* Update CLAUDE.md's region-size paragraph to match.
Consequence to handle: at 256 m regions the default `full` world scale (5×5 near window) is 1.28 km
of simulated terrain — far too much. When playing at 128 the default near window becomes **one
region** (near radius 0) with the LOD rings for the surroundings. *(My call — the fortress
direction plays one region; keep `full` available.)* All new tests in this piece must pass at 32;
the "done when" live run is at 128.

### 9. Cross-section image

A tool that renders a vertical slice of what the engine generates:
`just cross-section --world <file> [--region X,Z | --x … --z0 … --z1 …] [--out …]` (exact flags are
the builder's call), writing a PNG into `target/cross-section/`: one pixel per block, colour per
block (rock colours from the forge's `ROCKS` table, soil/grass/sand/water/air/bedrock colours),
surface line visible. It must call the same `underground_block` + band code the engine uses, not a
re-implementation. Also print the formation sequence crossed at the slice centre, so a test can
compare it with a dig.

## Constraints

- Determinism: integer-only underground evaluation; `just determinism-wasm` gets a pinned scene that
  materialises underground chunks by digging (native = wasm, state hash equal).
- `game` stays Bevy-free and worldgen-free; the descriptor is plain data in `game`.
- The undo log: materialisation inside an edit is a logged insert with an exact inverse
  (`hash(before) == hash(after undo)`).
- Cell mode (`VOXEL_CELL_CHUNKS > 0`) must keep working and its tests pass
  (`cell_activation`, `cell_manager`); `generate_cell` stays bit-identical to the region build.
  Whether cells also get the lazy underground is left to the builder (simplest: cells use the same
  descriptor per cell; acceptable: cell mode keeps today's band semantics behind the gate).
- The LOD rings see only the surface; pristine LOD output must not change.
- Tests: `just test-one` for the touched targets, `just smoke`, `just determinism-wasm`; client test
  binaries are a slow opt-in (ask first, CLAUDE.md).

## Verification ("done when")

0. With no world file on disk, `just run` forges `worlds/default.world` (or the chosen path) once,
   logs it, and starts on it; the next `just run` loads it without forging.
1. `just run` (now 256 m regions on the default forged world), `VOXEL_CAMERA=first-person`: dig a shaft
   from a hilltop and from a valley floor; see soil, then rock layer changes, then hit bedrock, which
   won't break; at every depth you can stand on the floor and the walls are solid (no gaps, no
   falling through).
2. The cross-section of the same spot shows the same layer sequence and the same bedrock depth.
3. Region start time, RSS and snapshot size at 256 m are within noise of a 256 m region built
   without the underground (one before/after measurement; ask before running timing runs).
4. Unit tests: the underground function against the forge formula; the exposure invariant after
   random digs (no air block with an unmaterialised ground neighbour); undo of a materialising dig
   restores the hash; descriptor round-trips through a snapshot; procedural regions get stone +
   bedrock at depth 60.

## Left to the builder

Fixed-point unit and raster interpolation for the geology offset; descriptor layout and
compression; where `underground_block` lives in `game`; the cross-section tool's flags and colours;
textures for the new blocks; how cell mode is handled within the constraint above; the order of
work.
