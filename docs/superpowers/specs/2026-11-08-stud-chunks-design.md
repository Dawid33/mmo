# Stud chunks: a toy-brick chunk kind beside the voxel chunks (2026-11-08)

Experiment. A second chunk content type, the **stud chunk** (a canonical list of toy bricks on a
stud/plate grid), runs through the whole stack: sim (ECS, physics, rollback, hashing), server,
network, client rendering and editing. Voxel chunks are untouched and keep working. A debug world of
one region plus its 8 neighbours, built only of stud chunks, is the test bed.

Background: `2026-11-08-brick-world-prior-art.md`. Naming: "brick" already means the client's 32³
render unit (`BRICK_VOXELS`), so the toy-brick code is `stud` everywhere (`StudChunk`, `StudBrick`, …).

The user skipped design questions (2026-11-08) and asked for this as a client + server + sim + network
experiment. Every call below that is not "stud chunks alongside voxels, one region + neighbours, all
studs" is **Claude's call**, made to keep the experiment small and the voxel path untouched.

## Goals

- `just run-studs` launches straight into a 3×3-region world of stud terrain (single-player server,
  commander camera), with no forge and no menu.
- Stud terrain is real sim state: fixed rapier colliders, `StateHash`ed, rollback-exact, carried in the
  region snapshot, predicted on the client.
- Bricks can be placed and removed from the client. Edits go through the normal order path (predicted
  locally, confirmed by the server) and other clients see them.
- It reads as toy bricks: true 5:2 plate proportions, studs on exposed tops, seam lines between bricks.

## Non-goals (for this experiment)

- Persistence: the stud world has no store. Edits survive region parking (the serialized `Rollback`
  carries them) but not a server restart.
- Seams: no stud halo. Collision and rays stop at region edges, and the client draws a region's stud
  chunks without looking across the seam.
- Creatures, pathfinding, orders, mining, tiles and `WorldView` ignore stud chunks. The world has no
  dwarves; it is for looking and building.
- LOD and far terrain: none. Only loaded regions draw.
- Fortress cutaway/level darkening (voxel-shader only) does not apply to stud meshes.
- wasm/web: not targeted. Nothing here is native-only on purpose, but it is not tested on the web.

## Anti-goals

- Any change to voxel `Chunk`, its serialization, the store schema, worldgen columns or voxel
  rendering. A voxel world must play exactly as before, except for state-hash values (below).

## Units and data (game crate, `crates/game/src/stud.rs`)

- **Scale rules (user, 2026-11-08):**
  - People are minifigure-sized: 4 bricks tall, 2 studs wide, 1 stud deep.
  - Studs are sized for physics convenience, and the player is fitted to them.
  - Chunks are sized to fit bricks.
  - Regions and chunks must be square in x/z; their height is free.
- **Why true LEGO proportions still work:**
  - Regions must be a power of two in x/z (`lod.rs:404`, shared with voxel worlds; untouched). So
    the **stud** must be a power-of-two number of sim units, which makes it exact.
  - LEGO's 5:2 plate ratio puts its factor of 5 only in the **plate**, i.e. vertically, and nothing
    constrains height.
  - So the chunk height is a multiple of **15 plates** (5 bricks = 16 sim units × k): every chunk
    floor and ceiling is a whole number of units and lies on a whole brick.
  - Inside a chunk, plate heights are exact rationals (`plates * 16 / 5` units), exactly
    representable every 5th plate.
  - Every edge is computed from integer cells by that one formula, so adjacent bricks share
    bit-identical faces and the physics stays flush.
- **Grid, Claude's call (2026-11-08), true LEGO proportions:**
  - **1 stud = 8 sim units = 0.5 m** (exact).
  - **1 plate = 3.2 units = 0.2 m.**
  - **1 brick = 3 plates = 9.6 units = 0.6 m.**
  - Real 5:2 plates and 5:6 bricks, so LDraw geometry scales uniformly with no stretch.
- **Stud chunk:**
  - Footprint **32 × 32 studs = 16 m**: the classic 32×32 baseplate, and exactly the voxel sim
    chunk's footprint. It reuses `ChunkCoords`, the chunk-column grid and `REGION_CHUNKS` tiling
    (16 m divides every allowed region size).
  - Height **75 plates = 25 bricks = 15 m = 240 units**: a near-cube, whole units, whole bricks.
  - Body origin is `(x·CHUNK_UNITS, y·240, z·CHUNK_UNITS)`. The vertical stud-chunk layer grid
    differs from the voxel one (15 m vs 16 m), so `StudChunkCoords` = region-local x/z (shared
    with `ChunkCoords`) + its own signed y layer.
  - 76 800 cells, and every coordinate fits `u8`.
  - Tuning constants: `STUD_UNITS`, `STUD_CHUNK_STUDS`, `STUD_CHUNK_PLATES` (asserted to be a
    multiple of 15). Plate height in units is derived as `16/5`.
  - Bricks never cross a chunk boundary. A placement that would cross is refused (same as any
    chunked build).
  - Independent of the `chunk_blocks` dial (assert `CHUNK_UNITS == 256`, else the stud experiment
    is off).
- **Player, fitted:**
  - A minifigure is 4 bricks = **2.4 m** tall, 2 studs = 1 m wide, 1 stud = 0.5 m deep.
  - In the stud world the person capsule, step height (one brick, 0.6 m), jump and eye height scale
    from 1.8 m → 2.4 m (×4/3).
  - Gravity is scaled with it so jumps and falls feel the same relative to the body.
  - These are per-world parameters, not edits to the shared movement constants. Voxel worlds keep
    1.8 m.
- **Rejected alternatives:**
  - Half scale: 0.25 m studs, 0.1 m plates, 8 m chunks of 32 × 32 studs × 75 plates; people 1.2 m.
    About 4× the bricks per area, and its own x/z chunk grid.
  - Stretched whole-unit plates: 0.25 m studs, 0.125 m plates; bricks 25 % taller than real.
- **LDraw mapping.** LDraw units: stud pitch 20 LDU, plate 8 LDU, brick 24 LDU, and a stud is
  radius 6 × height 4 LDU.
  - 1 LDU = 0.025 m = 0.4 sim units, uniform.
  - A minifigure's LDraw geometry drops in at true scale next to the bricks and matches the 2.4 m
    capsule.
  - LDraw's −Y is up.
- **`StudBrick { min: [u8; 3], shape: StudShape, color: u16 }`:** `min` is the cell (stud x, plate y,
  stud z). The shape is one of:
  - `Part { part: u16, rot: u8 }`: an index into the part catalog baked from LDraw (below), with
    `rot` in quarter turns about Y. The catalog gives the cells the part occupies.
  - `Filler { size: [u8; 3] }`: a plain cuboid, used by generators for large hidden fill. Players
    cannot place it, and it has no LDraw part.
  - Players place only `Part`s.
- **`StudChunk`:** the bricks, canonical as a `BTreeMap<[u8;3] /*min*/, StudBrick>`, with no overlaps
  and every brick inside the chunk. The constructor and every edit validate this, so equal content
  means equal hash and equal bytes. It has queries (brick at a cell, occupancy), edits (place → `Result`,
  remove-at-cell) and a collider builder (see "Colliders" below).
  Collision uses each brick's solid bounding cells; LDraw parts are hollow underneath, but the sim
  ignores that. Slopes and other non-box parts use a convex hull baked from their LDraw geometry.
- **Colliders (2026-11-09): parry `Voxels` with non-cubic cells, not a compound of cuboids, and not
  a new custom shape.**
  - Collision only needs the union of occupied cells, which is a voxel grid. The vendored parry fork
    already has `Voxels` with a **per-axis** `voxel_size: Vector`; its contact code uses
    `voxel_size / 2` as per-axis half extents. It already has, in our fork:
    - serde (it rides in the region snapshot and in parking);
    - an incrementally maintained content `StateHash`;
    - in-place `set_voxel` edits that `mutate_collider_safe` can undo, the same path voxel terrain
      uses in `edit_block_colliders_safe`;
    - internal-edge handling through per-voxel neighbour states, so a capsule sliding across two
      adjacent bricks does not snag on the seam. A compound of separate cuboids has that
      ghost-collision problem.
  - **Fine grid:** `voxel_size = (8, 1.6, 8)` units = one stud × half a plate × one stud. A brick
    fills 2 cells per plate. The **stud cap** is simply one more half-plate cell (1.6 units ≈ the
    0.1 m stud height) on every exposed top cell: no separate cap shapes, and it updates with the
    same `set_voxel` edits.
  - **Coarse grid**, as voxel terrain's multi-scale `ColliderCells::grids()` does: fully solid
    4 × 4 studs × 15 plates blocks (32 × 48 × 32 units) go in a coarse `Voxels`, so buried
    interiors cost a few coarse cells instead of about 150k fine ones per chunk. An edit that
    breaks a coarse cell moves it to the fine grid.
  - **Non-box parts** (slopes, round parts): one convex-hull collider each, baked from LDraw,
    attached to the same body.
  - Things rest on the stud tops and the surface stays flat. Minifigure models are shifted down by
    the stud height so their feet sit over the studs, as on real LEGO. Step height is unaffected
    (both levels carry a cap).
  - Placement and occupancy read `StudChunk`, never the colliders. Colliders are derived from it,
    caps included.
  - **To pin before relying on it** (a `physics-next` test): anisotropic `Voxels` contacts against
    capsule, ball and cuboid; that a stud cap reads as a flat floor; and no seam snags at 8-unit
    cell edges.
  - **Why not a custom rapier/parry shape:**
    - Parry deserializes any `Custom` shape to an error ("Cannot deserialize custom shape"), which
      would break region snapshots. So it would have to be a new first-class shape in the fork:
      `ShapeType`/`TypedShape`/serde variants, `StateHash`, query-dispatcher arms or a
      `CompositeShape` impl, ray/point queries, internal-edge pseudo-normals, and pin tests.
    - Each of those is a `// mmo-fork:` hunk to re-apply on every upstream update.
    - It would buy nothing `Voxels` lacks for the experiment. Revisit only if profiling shows the
      voxel shapes are the bottleneck, or bricks need something cells cannot express.
- **Colours:** `color` is an **LDraw colour code** (`LDConfig.ldr`, 322 codes). The sim only checks it
  against the baked palette. The client reads RGB and edge colour from the baked colour table.

## Prior art: how LEGO Worlds and others do it (researched 2026-11-09)

**LEGO Worlds** (TT Games, 2017). The only primary source is a LEGO "Bits N' Bricks" podcast with
lead mechanics programmer Mark Engley (S4E44). There is no GDC talk, postmortem or datamine of the
terrain format.

- **A fine editable grid plus a separate "grid → bricks" layer.** They first tried a 2×2-brick grid;
  the artists pushed for "full LEGO brick" resolution, which Engley puts at about 100 of the smallest
  bricks per Minecraft block. Quote: "a really small world grid that you could go in and completely
  edit … a technology to kind of turn that grid into bricks … so that you could have bricks that
  could span, like, multiple points on the grid." This is the same split as ours: occupancy cells
  plus multi-cell brick entries.
- **Compressed storage, not one record per brick:** "it would be billions of bricks to store in
  memory … in-memory compressed representations." Ours: canonical brick lists per chunk, plus big
  hidden `Filler` cuboids.
- **Overlapping placement destroys and splits instead of refusing:** "you're splitting bricks … if
  you put one overlaps another … one can destroy the other effectively, and it can decompose down
  to some kind of different LEGO brick representation." (LEGOscape's "a better-fitting brick evicts
  the bricks it intersects" is the same idea.) **Open option for us:** our design refuses an
  overlapping placement. LEGO Worlds' behaviour (the old brick's cells minus the new brick,
  re-decomposed into smaller catalog bricks) is a later upgrade to `place`; the data model already
  allows it.
- **Real-time remeshing of edited areas; render culling was the hard part.** A transparent brick sea
  meant "10s of millions of LEGO bricks", so they had to choose "which bricks to render … when to
  treat it as air space". Our water is not bricks, but any transparent brick needs the same rule.
- **Deterministic, seed-generated terrain** ("if you do the same thing in the same order, it will
  create the same thing"), with designer-authored structures placed by the generator.
- **Two kinds of prefab models:**
  - functional prefabs: entities, often just "smash apart and yield studs";
  - landscape-brick models that "merge with the terrain", where every brick is then editable.
  Our `StudChunk` parts are the second kind; entity models would be the first.
- **Characters** reuse TT's LEGO character controller and can free-climb any brick structure. Placed
  bricks are always terrain and grid-aligned. A player discussion notes SNOT (studs not on top) is
  impossible "because of how the voxel system works". Ours has the same limitation: parts rotate
  only about Y.
- **Stud collision:** not described anywhere public.

**Stud collision elsewhere: nobody collides individual studs.**

- **Brickadia** (devlog 4): a brick is its bounding box in a custom PhysX compound over a BVH, and
  the player capsule sweeps against the per-brick boxes. Studs are never mentioned in collision.
- **LEGO bricks in UEFN** (Epic docs): kragled meshes get "heavily optimized collision volumes"; brick
  data is kept to validate placement, separately from physics.
- **Studworks** (a minifigure walk-through sandbox): a capsule; plates and tiles are auto-stepped, a
  full brick needs a jump, slopes collide as full blocks.

**What this means for the design:**
- The flat stud cap matches common practice (as a half-plate voxel layer since the colliders were reworked; see "Colliders").
- Per-brick boxes plus a capsule character is the standard.
- Open tuning question: step height. LEGO-style controllers auto-step a plate and jump a brick; ours
  currently auto-steps one brick (0.6 m). Decide at first playtest.

## LDraw as the reference library (added 2026-11-08)

The user downloaded the official LDraw parts library to `assets/ldraw/`. It is 623 MB with about 25k
parts, and it is git-ignored. Parts are CC BY 2.0/4.0: shippable, with attribution.

**Never at runtime.** Neither the game nor the client reads `assets/ldraw`. An offline bake step turns
a small chosen subset into committed assets, the same way `tools/creatures` + `just creatures` work:

- **`tools/ldraw-bake`** (a new tool crate; `just ldraw-bake`; `LDRAW_DIR` overrides `assets/ldraw`):
  - Parses with **`weldr` 0.3.1** (MIT, crates.io). It handles parsing and sub-file resolution;
    `ldraw.rs` is git-only, so not that one.
  - Pulls in cgmath 0.17 and nom 5, so it stays a tool-only dependency and never enters the
    game or client dependency tree.
  - The bake code itself does: flatten the sub-file tree with transforms, apply BFC
    (`0 BFC CERTIFY CCW`, `INVERTNEXT`, mirrored matrices), and resolve colour 16 (the part's own
    colour) and 24 (its edge colour).
  - Follows `~Moved to` alias parts.
- **Inputs:** a hand-written list of part numbers to bake. Start with plain bricks and plates:
  - bricks 3005 1×1, 3004 1×2, 3622 1×3, 3010 1×4, 3009 1×6, 3008 1×8, 3003 2×2, 3002 2×3,
    3001 2×4, 2456 2×6, 3007 2×8;
  - the matching plates (3024, 3023, 3623, 3710, 3666, 3460, 3022, 3021, 3020, 3795, 3034);
  - a few slopes (3040 2×1 45°, 3039 2×2 45°) to prove non-box parts.
  - The colour subset comes from `LDConfig.ldr`.
- **Outputs, committed:**
  - **`crates/game/src/stud_catalog.rs`**, generated: per part its number, name and occupied cells
    in stud × plate units. This is all the sim needs. It's a pure table, so its hash is stable.
  - **`assets/studs/parts.bin`**, framed with `encode_packet` and loaded by the client:
    - per part, triangles with positions in sim units, normals, and a per-triangle "own colour or
      fixed colour" flag;
    - its edge lines (LDraw type 2);
    - both at two levels of detail: hi uses `p/stud.dat` (16 segments), lo uses `p/8/stud.dat`
      (8 segments).
    - Every triangle is **tagged** by what it belongs to:
      - `Face(dir)`, lying on a face of the part's bounding box;
      - `Stud(cell)`, a stud on top of a given cell;
      - `Inner`, the underside tubes and hollow.
  - **`assets/studs/colors.ron`**: code → RGB, edge RGB, and finish (solid, transparent, chrome…).
  - **`assets/studs/ATTRIBUTION.md`**: the CC BY credit lines collected from the baked parts'
    `!LICENSE` and `Author:` headers.

**How the tags drive the terrain mesher.** It copies a part's baked triangles into the chunk mesh
(rotated and translated), keeping:
- `Face(dir)` only when that face is exposed;
- `Stud(cell)` only when the cell above is free;
- `Inner` only when the part's underside is exposed (an overhang or a removed brick below).

So stacked terrain costs about one box's worth of triangles per brick plus its visible studs, while
slopes and any later part still draw their real LDraw shape. A full LDraw 2×4 is about 414 polygons,
400 of them studs and underside tubes. Using it unculled in terrain would cost about 30× more.

**Seams:** LDraw's edge lines are the classic "built from bricks" outline. The baked edges are drawn
as thin dark quads inset on exposed faces (replacing the procedural border strips); a real line pass
is optional later. The edge colour comes from the colour table.

**Build palette and ghost:** these use the whole baked part, hi detail, untagged and unculled.

## Sim integration (game crate)

Use a **separate component and index**, following the sim-chunk survey's recommendation:

- `Ecs.stud: Component<StudChunk>` and `GameData.stud_index: #[undo(map)] BTreeMap<StudChunkCoords, EntityKey>`.
  A stud chunk is its own entity with one fixed body at `(x·CHUNK_UNITS, y·240, z·CHUNK_UNITS)` and its `Voxels` colliders
  (fine + coarse, plus a hull per non-box part; `user_data` = entity key). It never shares `chunk_index`, so no voxel `ecs.chunk` site
  ever sees it.
- Genesis: `Rollback::seed_stud_genesis(Vec<(StudChunkCoords, StudChunk)>)`, raw and unlogged like
  `seed_terrain_genesis`.
- Runtime, undo-safe:
  - `create_stud_chunk_safe` for the first brick in an empty chunk, mirroring `create_mesh`;
  - `set_stud_chunk_safe` for edits: `Component::set_safe` on the whole chunk (fine at this size)
    plus in-place `set_voxel` edits on the fine/coarse grids through `mutate_collider_safe` (hull
    colliders for non-box parts are attached or removed). An empty chunk keeps its entity, with
    its colliders disabled.
- Extend `create_entity_safe` and `remove_entity_safe`. Classify `stud` in `carry_components`
  (terrain only, never travels) and in `portable::classify` (not persisted).
- Edits are `Order::PlaceStud { chunk, brick }` and `Order::RemoveStud { chunk, cell }`, region-local,
  handled next to `DevBreakBlock`. An invalid edit (overlap, out of bounds, not in the catalog) is a
  no-op. Prediction and the server round-trip come free from the order path.
- Renderer updates: `GameDataUpdateKind::AddStudChunk(EntityKey, StudChunkCoords, Box<StudChunk>)` and
  `SetStudChunk(EntityKey, Box<StudChunk>)`. Removal is the existing `RemoveEntity`.
- **Hash cost:** a new `Ecs` field shifts every state hash once. Re-record `physics_goldens` on
  purpose and run `just determinism-wasm`.
- Tests:
  - stud data invariants and canonical form;
  - collider matches occupancy;
  - rollback bar: `hash(before) == hash(after undo)` for create, place, remove, empty;
  - order round trip on a `Region`.

## Test world (worldgen + server)

- `worldgen::stud_world::region_studs(RegionCoords) -> Vec<(StudChunkCoords, StudChunk)>`: a pure function.
  Regions with |x|, |z| ≤ 1 get terrain; every other region is empty air.
- **Terrain:** a gentle deterministic height field (integer/`libm` hash noise) between about 2 m and
  13 m (within stud-chunk layer y = 0, 15 m tall), quantized to plates. Colours: green on top, then tan and grey
  further down.
- **Decomposition:** a large hidden filler cuboid fills each chunk up to a few plates under its lowest
  surface. Above that, real bricks are laid in brick-height (3-plate) courses, with plates topping out
  odd heights. The fill uses catalog footprints and staggers seams between courses (running bond:
  alternate the orientation/offset per course) so it reads as built, not extruded.
- A small hand-made feature next to spawn shows off bonds: a staggered wall plus a short tower.
- **Server:** `VOXEL_WORLD_FILE=studs` resolves to `WorldSource::Studs`. Like `Procedural` it has no
  store, history or world map. The column generator returns no columns, so there is no voxel terrain,
  ground recipe or water. A post-genesis hook on `ThreadRegionSpawner` (`with_genesis`) seeds the stud
  chunks into freshly built regions only (never parked or restored ones). The LOD path builds nothing
  for this world.

## Client

- **Bridge:**
  - a stud entity becomes `ChildOf(region terrain root)`, with its Transform at the chunk origin
    (from its body pose) and a `SimStudChunk(StudChunk)` mirror;
  - `AddStudChunk` and `SetStudChunk` arms replace the mirror;
  - `RemoveEntity` already despawns it, and region roots clean up the rest.
- **Mesher** (`renderer/stud_mesh.rs`), main thread, on `Changed<SimStudChunk>`. It copies tagged
  baked part triangles (see "LDraw as the reference library"); `Filler` bricks are plain boxes.
  - **Body mesh:** a vertex-coloured `Mesh` of each brick's visible faces. A face is culled when the
    cells across it are all occupied, within the chunk and against loaded neighbour chunks; neighbours
    are remeshed when a chunk changes. Exposed faces carry the baked LDraw edges as thin dark quads,
    which give the seam/bevel lines.
  - **Stud mesh:** a separate child entity with the baked `Stud(cell)` triangles (lo detail) on every
    exposed top cell. It is shown only within a radius of the camera's focus (stud LOD); further out
    the tops stay flat.
  - Both use a `StandardMaterial` (plastic: low roughness) shared per session.
- **Height grid:** fed from stud chunks (per 1 m block-column top in voxel units, sampled from the stud columns under it), so the commander camera
  places, the loading cover clears, and the ground pick and minimap work.
- **Build mode** (dev-style keys, no Slint changes):
  - a key toggles stud build mode;
  - the cursor ray picks the nearest brick face in the loaded stud chunks;
  - a ghost brick previews the placement, red when invalid;
  - left click places, right click removes, and keys rotate, cycle the catalog size and cycle colour.
  - Orders go through `GameEvents` like the designation tools. `CursorPolicy` stays the only cursor
    writer.
- **Launch:**
  - `client debug studs` creates `worlds/debug-studs/` (used only for `server.log`) and sets
    `PendingWorld { terrain: "studs" }`, which `LocalHost` passes as `VOXEL_WORLD_FILE`;
  - once in the world it sets `Embarked` itself (there is no map or history to embark on);
  - `just run-studs` builds both binaries and runs it.

## Implementation order

0. `tools/ldraw-bake` plus `just ldraw-bake`: generate `stud_catalog.rs`, `assets/studs/*` and the
   attribution file. Check one baked 2×4 against LDraw's own numbers (8 studs, bounding box
   80×24×40 LDU).
1. Game: `stud.rs` data on the generated catalog, colliders, ECS/index, genesis and safe ops, orders,
   update kinds, classification, tests, golden re-record.
2. Worldgen generator plus server `WorldSource::Studs`, genesis hook, LOD opt-out; a server or game
   test that a stud region starts with its chunks.
3. Client: bridge, mesher, height grid, debug preset, `run-studs` recipe.
4. Client build mode: picking, ghost, orders.

Verification: targeted `just test-one` per touched test, `just test-threads` (spawner change),
`just determinism-wasm` (hash shift), `just smoke`, then a real `just run-studs` look.
