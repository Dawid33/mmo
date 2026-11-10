# Handoff: write the stud-chunks implementation plan (2026-11-09)

**Your job in this session:** turn the approved-in-conversation design into an implementation plan
with the `superpowers:writing-plans` skill. **Do not implement.** The plan goes to
`docs/superpowers/plans/2026-11-09-stud-chunks.md`.

**Read first, in this order:**
1. `CLAUDE.md`: binding rules.
   - No worktrees, no commits unless asked, `just test-one` only.
   - Rollback/determinism rules; the session-lifecycle rule for client resources.
2. **The spec:** `docs/superpowers/specs/2026-11-08-stud-chunks-design.md`. Every decision lives
   there; this file only adds context.
3. Background, skim only: `docs/superpowers/specs/2026-11-08-brick-world-prior-art.md`.

**State of the tree:**
- Nothing is implemented; no `stud` code exists.
- The user has many unrelated uncommitted edits (client, server roles, deploy, docs). Never touch,
  revert or reformat them.
- `crates/server/tests/roles.rs` is the user's work in progress.

## What the experiment is (one paragraph)

A second chunk content type, the **stud chunk**: toy bricks on a stud/plate grid, its own data type
next to voxel `game::Chunk`. It goes through the whole stack:
- sim: ECS component, fixed rapier body with `Voxels` colliders, `StateHash`, rollback-exact
  undo-safe edits, orders;
- server: a `studs` world source with a genesis hook;
- network: rides in the region snapshot, plus orders;
- client: bridge, LDraw-based mesher, height grid, build mode, `client debug studs`, `just run-studs`.

The test world is one region plus its 8 neighbours, all stud chunks, with no store. Voxel chunks
stay untouched.

## Decisions, and who made them

The user made these (keep them):
- Stud chunks live **alongside** voxel chunks; nothing voxel is removed.
- The test world is one region plus neighbours, all bricks.
- It is client + server + sim + network work, named **`stud`** ("brick" already means the 32³
  render unit).
- **Use the LDraw library** (`assets/ldraw/`, 623 MB, git-ignored, CC BY 2.0/4.0) as the reference.
  A Rust crate loads it.
- **People are minifigure-sized:** 4 bricks tall, 2 studs wide, 1 stud deep.
- **Studs are sized for physics convenience, and the player is fitted to them.**
- **Chunks are sized to fit bricks.** Regions and chunks are square in x/z; height is free.
- Asked "would our own rapier shape make sense": the answer, recorded in the spec, is no; use parry
  `Voxels` with anisotropic cells.

Claude made these, all recorded in the spec (the user has seen them and not objected):
- **Grid:** stud 0.5 m (8 sim units, exact), plate 0.2 m (3.2 units), brick 0.6 m: real LEGO 5:2
  proportions. The factor of 5 sits only in vertical plates, and height is free.
- **Stud chunk:** 32 × 32 studs (16 m, the same footprint as the voxel sim chunk, so it reuses x/z
  `ChunkCoords` and region tiling) × 75 plates tall (25 bricks, 15 m, 240 units).
  - The vertical layer grid is its own: `StudChunkCoords` = x/z plus a signed stud layer y.
  - Body origin is `(x·256, y·240, z·256)` units.
- **People are 2.4 m** (×4/3 of today's 1.8 m `PLAYER_HEIGHT`). Capsule, step, jump, eye height
  and gravity scale per world. Half scale (1.2 m people) was rejected for cost.
- **Data:** `StudBrick { min: [u8;3], shape: Part{part,rot} | Filler{size}, color: u16 (LDraw code) }`.
  `StudChunk` is a canonical `BTreeMap<min, StudBrick>`; overlapping placement is refused.
- **Colliders:**
  - parry **`Voxels`**: a fine grid of `(8, 1.6, 8)` cells, where a brick fills 2 cells per plate
    and the **stud cap** is one extra half-plate cell on exposed tops;
  - a coarse grid of `(32, 48, 32)` cells for solid interiors;
  - a convex hull per non-box part.
  - Edits are in-place `set_voxel` through `mutate_collider_safe`.
- **LDraw pipeline:**
  - Offline `tools/ldraw-bake` (crate **`weldr` 0.3.1**, MIT; tool-only because of cgmath 0.17 and
    nom 5).
  - It outputs a generated `crates/game/src/stud_catalog.rs` (part number → occupied cells and hull
    points), `assets/studs/parts.bin` (tagged triangles `Face(dir)`/`Stud(cell)`/`Inner`, two
    levels of detail, edges), `assets/studs/colors.ron` and `assets/studs/ATTRIBUTION.md`.
  - **glTF instead of the custom `parts.bin` is an open swap** the user may still pick (same
    content as named primitives).
- **Client mesher:** copies tagged baked triangles, culling hidden faces, covered studs and unexposed
  undersides. Studs go in a separate per-chunk mesh shown only near the camera focus. Seams come
  from the baked LDraw edges as thin dark quads.
- **Server:** `VOXEL_WORLD_FILE=studs` → `WorldSource::Studs` (no store, map or history). The
  column generator returns nothing; `ThreadRegionSpawner::with_genesis` seeds stud chunks into
  freshly built regions only. The LOD path builds nothing.
- **Hash shift:** a new `Ecs` field changes every state hash once. Re-record `physics_goldens` on
  purpose and run `just determinism-wasm`.

Open questions, which the plan should carry as decision points rather than resolve:
- Step height: LEGO-style controllers auto-step a plate and jump a brick. The spec auto-steps one
  brick (0.6 m). Decide at first playtest.
- Split-on-overlap placement, as LEGO Worlds does: a later upgrade to `place`.
- glTF vs custom `parts.bin`.

## Code map, verified by survey on 2026-11-08 (re-verify line numbers; the tree moves)

**Game crate**, `crates/game/src/`:
- **`state.rs`:**
  - `#[rollback]` `Ecs` (~:367, `chunk: Component<Chunk>`).
  - `GameData.chunk_index: #[undo(map)] BTreeMap<ChunkIndexKey, EntityKey>` (~:283).
  - `Component<T>` with `insert_safe`/`set_safe`/`remove_safe` (~:557-760).
  - `create_entity_safe` (:520) inserts a slot for **every** component; `remove_entity_safe`
    (:1318) removes every slot plus the `chunk_index` entry. Add stud to both lists by hand: the
    compiler will not force it.
  - `create_chunk_entity_raw` (:546) for genesis.
  - **`create_mesh` (:937)** is the runtime undo-safe template: entity, fixed body via
    `undo_scope_digest` + `revert_insert`, colliders via `attach_collider_safe` (:1105).
  - **`seed_terrain_genesis` (:984)** is the raw genesis template.
  - `Undo<ColliderSet>::mutate_collider_safe` (:676).
  - `GameDataUpdateKind` (:56-121).
- `edit.rs`:
  - `spawn_chunk_safe` (:156), the `AddTerrainChunk` emit pattern;
  - `write_block_safe` (:262), the `SetBlocks` + `emit_on_undo` pattern;
  - `edit_block_colliders_safe` (:313), which patches `Voxels` in place: the model for stud
    collider edits.
- `block.rs:351`: `ColliderCells` and its multi-scale `grids()`, the model for fine/coarse stud
  grids.
- `orders.rs:~131` `Order`; handled at `region.rs:~1033` next to `DevBreakBlock`.
- `region.rs`: `Region::from_chunks` (:1144) and `from_columns` (:1157). Add a `seed_studs`-style
  helper for the server hook.
- Exhaustive destructures that will fail to compile until the new field is classified:
  - `migrate.rs:76 carry_components`: terrain only, never travels;
  - `portable.rs:255 classify`: not persisted;
  - `portable.rs:72/184` only if the portable structs change.
- `region_runner.rs:227-276`: `RegionSeed`, `into_region_with`, `into_runner`.
- **Gotchas:**
  - `ChunkCoords` (`voxel.rs:341`) does **not** derive `StateHash` (`WorldChunkPos` in
    `protocol.rs:216` does), so check before putting coords in an `Order`.
  - `remove_safe` unwraps the slot, so raw genesis entities without a slot panic on removal.
  - `physics.rs:74` maps `user_data` only for active bodies.
- Movement constants: `movement.rs` (`PLAYER_HEIGHT` 1.8 at ~:87, `STEP_HEIGHT`, `JUMP_SPEED`,
  `GRAVITY`…). These are global `const`s. **Per-world player scaling needs a design step in the
  plan** (a world parameter threaded to the walker/capsule code); this has not been investigated.

**Parry/rapier forks:**
- `crates/parry/src/shape/voxels/voxels.rs`: `Voxels::new(voxel_size: Vector, &[IVector])` (:665);
  in-place edits in `voxels_edition.rs` (`set_voxel` :118, `set_voxels` :136).
- Contacts use `voxel_size()/2.0` as per-axis half extents (`query/contact_manifolds/contact_manifolds_voxels_*.rs`).
- **Anisotropic cells are untested upstream:** the plan's first sim task should be a
  `crates/physics-next` pin test covering capsule, ball and cuboid against `(8,1.6,8)` voxels, a
  flat cap floor, and no seam snag.
- Custom shapes cannot deserialize (`parry/src/shape/shared_shape.rs:706`), which is why `Voxels`
  was chosen.
- Shapes are `StateHash`ed via `dyn Shape` (`shape.rs:446`).

**Server** (`crates/server/src/`):
- `world_file.rs:30` `WorldSource`, with `resolve` taking the `procedural` keyword. Add `studs`
  the same way: an early return; the DB env is ignored with a warning.
- `lib.rs:~510-720` builds:
  - the world source → `install_world`;
  - the LOD builder (`lod_workers::ThreadLodBuilder`, trait `game::LodBuilder` in
    `game/src/lod_builder.rs:67`);
  - `region_generator` / `halo_generator` → `ThreadRegionSpawner::with_generator(..).with_prefetch(..)`;
  - `WorldManager::new(spawner, Box<generator>, ..)`.
- `region_threads.rs`: `ThreadRegionSpawner` (fields :22-51, `spawn` :111). A seed is "fresh" when
  it is `Deferred(None)` and the store load was `Ok(None)`. Add `with_genesis`.
- `just test-threads` is required after touching `region_threads`.

**Client** (`crates/client/src/`):
- **Bridge, `renderer/bridge.rs`:**
  - region roots (`spawn_region_roots` :69, terrain root vs bodies root);
  - `spawn_region_snapshot` (:920; `is_chunk` decides the parent at :944-962), where a stud entity
    needs `ChildOf(root)` and its Transform from the body pose;
  - `drain_region_updates` (:1024): an exhaustive match, so add `AddStudChunk`/`SetStudChunk` arms;
    `RemoveEntity` (:1096) already despawns.
  - `interpolate.rs:26` skips only `ChunkBricks` entities: give stud entities a marker or confirm
    no drift.
- **Mesh template:** `renderer/water/mod.rs` `tile_mesh` (312) and `poll_water_meshes` (429) for
  Mesh3d + material + `SessionScoped` + `ChildOf(root)`. Plain `StandardMaterial` works
  (`avatar.rs:40-97`). Register next to `WaterPlugin` in `SimBridgePlugin::build`
  (`renderer/mod.rs:~108`).
- **Height grid, `renderer/commander/height.rs`:** `ChunkTops` (per 1 m block column, voxel units),
  `insert_chunk` (:206), `remove_entity` (:216). **It must be fed from stud chunks**, or the camera
  never places, the loading cover loops (`frontend/loading.rs:99-145`) and `CursorPolicy.loading`
  sticks.
- `TileMirror` and water read ground recipes: stud regions have none, so nothing shows through.
- **Edit path:** `designate.rs` `cursor_ray` (:502) for the pick ray; send
  `GameEventKind::Order(client, order)` through `GameEvents` (pattern at :616). Prediction and the
  server round-trip are automatic (`instance.rs::handle_game_event` → `RegionCmd::Predict`).
  `CursorPolicy` is the only cursor writer; there is no bevy_picking.
- **Launch:**
  - `debug_launch.rs`: `DebugScenario` (:98), `parse_args`, `USAGE` (:75), `boot` (:384),
    `auto_embark` (:582).
  - `auto_embark` waits for a world map, which a store-less world never has, so the studs scenario
    sets `Embarked` itself (as the `PlayHere` arm at ~:670 does).
  - `PendingWorld { entry_dir, terrain, name }` (`session.rs:100`); `LocalHost::start_on`
    (`local_host.rs:137`) passes `terrain` as `VOXEL_WORLD_FILE` verbatim, so `terrain: "studs"`
    works. `entry_dir` is only used for `server.log`.
- **justfile:** add `run-studs` next to `run` (~:197). Play recipes use `play_blocks` (256);
  switching `region_blocks` rebuilds everything (one shared target dir), so keep 256.
- Fortress cutaway and level darkening are voxel-shader only: stud meshes are not clipped (spec
  non-goal).

**LDraw facts** (from the library on disk):
- Units: stud pitch 20 LDU, plate 8, brick 24; stud radius 6 × height 4. −Y is up. At our scale
  1 LDU = 0.025 m = 0.4 units.
- Hi-detail stud `p/stud.dat` (16 segments); lo-detail stud `p/8/stud.dat` (8 segments).
- Colours: `LDConfig.ldr` has 322 `!COLOUR` lines (CODE, VALUE, EDGE).
- Parts in the spec's bake list all exist. `3023` and `3040` are `~Moved to` aliases (→ 3023b,
  3040b).
- Parts use `0 BFC CERTIFY CCW` and sub-files (`s\3001s01.dat`).

## Verification the plan should schedule

- `just test-one <crate> "<args>"` per new test file. Game integration tests go in the `it` binary
  unless they touch process-global state.
- `physics-next` pin test for anisotropic `Voxels`, first.
- Rollback bar for every stud mutation (create, place, remove, remove-to-empty):
  `hash(before) == hash(after undo)`.
- `just test-threads` after the spawner hook; `just determinism-wasm` after the hash shift;
  re-record `physics_goldens` on purpose.
- `just smoke` after client work. **Client tests are opt-in: ask the user first.**
- The end check is a real `just run-studs` look. Ask before launching anything GPU-heavy; the user
  runs benchmarks on this box.

## Plan-shaping advice

- Step order from the spec: 0 LDraw bake, 1 game sim, 2 worldgen generator + server, 3 client
  render + launch, 4 build mode. Step 0 and the physics pin test can run in parallel. Everything
  else is sequential, because the game types gate both the server and the client.
- Keep each task small enough for one subagent, with exact files, test commands and expected output.
- Put the per-world player-scale design (movement constants) as its own early task. It is the
  least-explored part.
