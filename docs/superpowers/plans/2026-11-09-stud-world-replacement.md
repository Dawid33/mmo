# Stud World: replacing voxel chunks with toy bricks — master plan (2026-11-09)

> **For agentic workers:** this is a **roadmap**, not a task-level plan. It fixes the target design,
> the phase order, each phase's interfaces, deletions and exit gate. Before starting a phase, write
> that phase's task-level plan with `superpowers:writing-plans` into
> `docs/superpowers/plans/<date>-stud-world-p<N>-<name>.md` (re-verify every file:line here first —
> the tree moves), then execute it with `superpowers:subagent-driven-development`.

**Goal:** stud chunks (toy bricks on a stud/plate grid) become the *only* terrain: forged-world
generation, persistence, seams, LOD, fortress gameplay and rendering all run on them, and the voxel
`Chunk` system (blocks, chisel, 32³ render bricks, voxel shaders, voxel LOD) is deleted.

**Architecture:** terrain is a per-stud-column **ground recipe** plus a **pure, per-chunk brick
decomposition** of it; a chunk that nobody has edited is never stored and never sent — server, client
and halo all derive it from the recipe. Edited chunks are explicit `StudChunk`s (canonical brick
lists, parry `Voxels` colliders). The sim keeps a coarse **tile** view (1 m × 1.2 m × 1 m) derived
from stud occupancy, so pathfinding, designations and jobs keep their shape. The client meshes chunks
in compute jobs into a quad-record pipeline forked from `voxel_pull`.

**Strategy:** strangler. The voxel world keeps working (`just run`) until the stud world passes it at
every layer; `just run-studs` grows from the 3×3 toy world into the real game; one cutover phase flips
the default and deletes the voxel code.

**Starting point (the experiment, done 2026-11-09):** spec
`docs/superpowers/specs/2026-11-08-stud-chunks-design.md`, plan `2026-11-09-stud-chunks.md`, open
points in `TODO.md` ("Stud chunks"). It has: data + catalog + LDraw bake, `Voxels` colliders,
rollback-exact place/remove orders, a toy generator, a main-thread mesher, height grid, build mode.
It lacks, by its own non-goals: persistence, seams/halo, LOD, creatures/pathing/tiles/mining, water,
fortress shading, wasm, real worldgen. Measured: 1.75–2.4 s genesis and ~5 MB snapshot per 256 m
region at 13 m of relief (debug build).

**Who decided what:** the user asked for the full replacement (2026-11-09) and answered no design
questions. Everything under "Target design" is **Claude's call** unless marked *(experiment spec)*.
The five calls most worth the user's veto are listed first.

## Status (2026-11-09)

D1–D5 accepted as recommended ("make it so", 2026-11-09).

- **P1 done.** 90-plate chunks, unbounded layers (`STUD_LAYER_LIMIT`), `TILE_STUDS`/`TILE_PLATES`,
  `StudBrick.material` + `data`, `stud_material.rs`, `stud_carve.rs` (`box_part`, `fill_box`,
  `StudChunk::carve`), `Rollback::carve_stud_safe`, `Order::CarveStud`, corridor pin tests.
  Deviations from the text below:
  - Materials are a const Rust table (`STUD_NATURALS`), not `materials.ron`: nothing to install,
    equal on every machine. An id below `NATURAL_BASE` (0x8000) is the plastic of that LDraw colour.
  - The stud body needed its own collider size: the 4/3-scaled capsule was 1.07 m wide and could not
    enter a 1-tile corridor. `BodyScale` gained `height_m`/`radius_m`; stud bodies are 2.2 m × 0.8 m.
  - Moved to P5 (they are wire/store work): `StudDelta` updates, the compact chunk codec.
  - Moved to the phase that first uses them: new baked parts (tiles, inverted slopes, round), the
    bake's Y-mirror fix.
- **P2 sim side done.** `stud_ground.rs` (`StudColumnGround`, `StudProfile`, `pristine_chunk`,
  `StudRegionSeed`), `GameData.stud_ground`, `seed_stud_ground_genesis`, `stud_chunk_or_pristine`,
  `stud_material_at`, carve materialises implied chunks (and the neighbours a hole opens onto),
  `Region::seed_stud_world`; the toy world (`worldgen::stud_world::region_seed`) and the server hook
  run on recipes. No `edited` flag: "edited" is `chunk != pristine_chunk(coords)`, checked when
  saving (P5).
  - **Still open in P2:** recipe water (bodies, `water_units_at`), slope parts on one-brick rises,
    and the exit gate — genesis cost against the P0 voxel baseline (needs benchmark runs).
  - **Watch:** the toy world's rough noise gives ~1000 bricks per surface chunk (16 chunks → 16 202
    bricks, 9 ms to derive). If real terrain is anywhere near that, lay only the exposed shell as
    parts and fill hidden soil with fillers.
  - The client still receives pristine chunks in the region snapshot; deriving them client-side is P5.
- **P2 additions (same day):** the recipe carries real rock strata (`geo` + `strata`, the voxel
  rule) instead of one rock per profile; hidden ground is laid as 4 × 4-stud fillers of the right
  material and only the visible shell (one course under the surface, deeper along chunk edges) is
  real parts. Flat ground ≈ 140 bricks per chunk. Steep procedural mountains still reach ≈ 1 400
  (46 424 bricks for a 64 m region of 34 chunks, 38 ms to derive, debug-opt test build): that is
  exposed cliff face, mostly 1 × 1 bricks. If forged mountains matter, let cliff courses run along
  the contour instead of the fixed strip direction.
- **P3 started.** `worldgen::stud_gen`: `generate_stud_columns` / `generate_stud_region` sample the
  forged world or the procedural pipeline at stud columns through the voxel generator's own
  `profile_sample` / `column_material`, fed `stud_materials()` (the worldgen material table in stud
  ids, resolved by block name; 14 natural materials added). Surfaces are plate-quantised, covers /
  soils / snow / glaciers / lake beds / strata carry over, sea and lake levels land in `water_top`.
  Window-independent and seam-continuous (tests). Server: `VOXEL_WORLD_FILE=studs-procedural`
  (`WorldSource::StudsProcedural`); client: `client debug studs --procedural`,
  `just run-studs --procedural`.
  - **Not yet in P3:** slope parts, water drawing (the sea floor shows dry). Trees: P6b below. **Forged** stud worlds run on the server since P5 persistence (below); the client has no
    way to draw one yet.
- **P5 persistence done (store side; the wire side is still open).** A forged world in the world
  store runs on stud terrain with `VOXEL_TERRAIN=studs` (`just terrain=studs … server`) and keeps its
  edits across restarts and parking, on SQLite and Postgres.
  - **Storage rule** (`portable.rs` module docs): per materialised chunk, compared with its
    recipe's `pristine_chunk` when saving — equal, in a surface layer → **no row**; equal, anywhere
    else (a neighbour a carve opened, a chunk dug and refilled) → a **`Pristine` marker** row
    (coordinates only); different (edited, built in the air, holding feature bricks) → its bricks.
    A restore seeds the recipes, every surface layer's pristine chunk, then the rows. So the
    restored region has *exactly* the saved set of materialised chunks, bricks and colliders (not
    a re-derived superset), and its terms equal the saved ones. Chosen over "re-derive the
    neighbours of stored chunks on load": that rule ties the loader to the carve's grow rule and
    materialises up to 26 chunks per edited one. A chunk edited back to pristine has its row
    deleted (surface layer) or turned into a marker (below).
  - `PortableRegion.stud_chunks: BTreeMap<StudChunkCoords, Option<StudChunkRecord>>` (`None` =
    delete the row; merges keep the newest) + `stud_ground` (first snapshot only);
    `StoredRegion.stud_chunks` / `stud_ground`. Saves diff against `SaveTerms { chunks, studs }`
    (was `ChunkTerms`); a stud term keeps the chunk's digest **and** how it was kept, so a save
    derives `pristine_chunk` only for chunks that changed since the last one. `restore_portable`
    sets `BodyScale::STUD` for a region with stud terrain. `classify` reclassified
    (`stud`, `stud_ground` persist; `stud_index`, `body_scale` derive); stud still never travels.
  - Store: migration `0004_stud_terrain` (both engines): `stud_chunks(rx, rz, cx, cy, cz, data)`,
    `regions.stud_ground` (nullable — its own column, not `regions.ground` as written below, so a
    voxel row and a stud row cannot be confused), `world.terrain`. `FORMAT_STUD_CHUNK`,
    `FORMAT_STUD_GROUND` (golden fixtures). `STORE_FORMAT_VERSION` stays 1: additive, like
    0002/0003 (a read-only handle is refused until a writer has migrated, as before). Stud rows are
    written only inside `commit_cut`'s fenced transaction. A row that does not decode, lies outside
    the region, or is a marker without a recipe makes the load `Corrupt` → `LoadFailed`; nothing
    regenerates over it.
  - **Terrain kind is durable**: `world.terrain` (NULL until first served; the migration marks
    worlds that already have saved regions as voxels). The host/all-in-one server calls
    `StoreHandle::bind_terrain` right after opening the store: set once, and a world tied to the
    other kind does not start (`StoreError::OtherTerrain`). It is a write to the world row outside
    the owner-epoch fence, like the world row's own insert at open: set-if-NULL, never changed.
  - Server: `world_file::TERRAIN_ENV` / `terrain_from`, logged with the world line. With studs on a
    stored world: region threads still pin their forged tiles, then seed fresh regions from
    `stud_gen::generate_stud_region`; voxel columns, halo generation and LOD are off as for the
    other stud sources.
  - **Left open in P5:** the wire (`StudDelta`, snapshot = recipe + edited chunks, compact chunk
    codec); `TerrainSnapshot` / `stored_terrain.rs` carry no stud terrain (halo seeds and LOD of a
    stored stud region see nothing — P4/P8); `just test-r2` with stud terrain was not run; recipes
    are stored as one zstd blob per region, not palette-compressed (size not measured against the
    voxel baseline); feature chunks (trees, once `stud_gen` makes them) are stored whole because
    they differ from the recipe — switch to "differs from recipe + features" when features land;
    no client recipe draws a stored stud world.
  - No hashed type or `GameData`/`Ecs` layout changed: `wasm_determinism`, `scene_hash_golden` and
    `physics_goldens` are untouched.
- **P4 seams done (sim, server, store; the client only replays it).** A seam changes nothing
  observable for walking, carving and placing on stud terrain.
  - **Stud halo** (`game/src/stud_halo.rs`): `GameData.stud_halo` (chunk + fixed body, by
    `WorldChunkPos` with `y` = stud layer) and `GameData.stud_halo_ground` (the neighbours' edge
    recipes). Halo chunks are bodies with `HALO_BODY_USER_DATA`, **not entities**: never in
    `stud_index` / `Ecs.stud`, so edits, saves (`portable::classify`: dropped) and the renderer's
    chunk mirror never see one; every start reseeds them.
  - **What is sent when.** It rides the existing `HaloUpdate` (`studs`, `stud_ground`), so
    stamping, T+1 application, the client's replay and reconcile are the voxel halo's. A *seed*
    carries the edge columns' recipes plus only the chunks the recipe cannot give back (edited, or
    materialised outside the surface layers — the P5 storage rule); the receiver derives
    `pristine_chunk` for every surface layer itself and drops any other chunk of that neighbour.
    A *delta* is every edge chunk the owner wrote in the slot, whole (`set_stud_chunk_safe` /
    `create_stud_chunk_safe` note it; also chunks a carve materialises).
  - **Seed sources**, all through `StudEdge` (`TerrainSnapshot.stud`, edge columns only): a running
    neighbour (`Rollback::stud_edge`, `HaloRequest`); a parked one (its final `TerrainSnapshot`);
    a stored one (`world-store` `load_terrain` → `StudEdge::from_stored`); a never-run one
    (`StudHaloGenerator` on the region thread — `generate_stud_columns` for the band windows on
    the server, the whole-region genesis filtered for the toy world; `RegionSpawner::stud_genesis`
    for the inline spawner).
  - **Reads**: `WorldView::stud_material_at(WorldStudCell)` (own ∪ halo, implied ground from the
    neighbour's recipe) and `WorldView::stud_raycast` → `StudWorldHit` (`placement()` gives the
    `Order::PlaceStudAt` body). `raycast_studs_within` / `placement_for_ray` are the signed-chunk
    forms of the old functions.
  - **Edits across a seam**: `Target::Stud`, `Intent::CarveStud { lo, hi }` (world stud cells) and
    `Intent::PlaceStud { chunk, brick }`. `Order::CarveStud` carves its own part at once and sends
    each neighbour its clipped part; the new world-addressed `Order::PlaceStudAt` places at once
    or sends an intent. Applied by the owner at its next tick by the ordinary intent rules (never
    wakes a region; refused `OwnerNotRunning` by the manager). A sender without a body (commander)
    sends `by` its region. **Decision:** what a neighbour's carve removes is dropped — there are
    no stud drops yet; when there are, they go into the owner's piles.
  - **Ground behind a neighbour's dig**: the owner's carve grows only inside its region, so the
    region across the face materialises its implied chunk when the *edited* halo chunk arrives
    (delta, or listed in a seed) — `materialize_behind_stud_halo_safe`. Derived pristine halo
    chunks never trigger it (in-region, a cliff between two chunk columns is not materialised
    either — still open from P2).
  - Tests: `game` `stud_halo::` (11: seed/delta/reseed, rollback bar on every event, intents,
    persistence without the halo, every seed source equal), `handoff_schedule::stud_` (walk level
    and grounded across a seam with no depenetration lift, a straddling carve, a placement across
    it, halos equal to owners, a replica fed each region's event stream ends in the server's state
    hash — lockstep and 3 seeds each of shuffled / late / jittery), `server`
    `threaded_world::stud_carve_crosses_a_seam_on_real_threads`, `world-store`
    `studs::stored_terrain_carries_the_stud_edge`.
  - **Left open:** the client build tool still aims with region-local `raycast_studs` /
    `Order::PlaceStud` (switch it to `stud_raycast` + `PlaceStudAt` in P7); the stud halo is not
    mirrored to the renderer (no `SetHalo*` update: the mesher cannot cull faces against a
    neighbour that is not loaded, and the tile mirror gets nothing — P6/P7); `Order::RemoveStud`
    is still region-local; a seed costs one `pristine_chunk` per edge surface chunk on the
    receiver, and `stud_edge()` one per materialised edge chunk on the sender, per neighbour
    (cache by `StudTerm` if region starts get slow at 256 m — not measured); whole-chunk deltas
    until `StudDelta` (P5 wire); `just test-r2` and the Postgres backend were not run.
- **P6a tiles / pathing / dig / build done (sim only).** A stud region runs the fortress loop on
  tiles: designate, validate, dev-complete, and walk.
  - **One switch, one conversion** (`game/src/tiles.rs`): `Rollback::terrain_kind()` →
    `TerrainKind::{Voxel, Stud}` (the `BodyScale::STUD` marker; `TerrainKind::of(scale)` where only
    the scale is at hand). Every tile ↔ unit conversion on the ported paths goes through
    `TerrainKind` (`tile_width_units`, `tile_height_units`, `feet_tile`, `on_level`,
    `column_centre`, `level_bottom_units`): voxel 1 m × 1 m, stud 1 m × 1.2 m. The old free
    functions (`feet_tile`, `feet_of`, `body_on_tile`, `steer`) keep their voxel meaning for the
    client and tests; the sim uses `Rollback::feet_of` / `body_centre_on` / `steer_on`.
  - **Tile view** — no cache in state. `StudTile` (per stud column: one past the highest filled
    plate in the tile; the top cell's material; a `partial` flag for non-box parts) is computed on
    demand from the materialised chunk, else the recipe, own ∪ stud halo
    (`Rollback::stud_tile`). `TileFill`: `Open` (nothing), `Solid` (all four columns reach the
    tile's top and no slope part is in it — a hollow under a full top still counts),
    `Floor(1..=6)` otherwise (the highest column; 6 = a partial footprint or a slope reaching the
    top). Dominant material = the topmost filled cell's, ties to the lowest stud index.
    `Rollback::tiles()` → `TileView` memoises whole chunks for the length of one `&Rollback`
    borrow (path searches, order validation): it cannot outlive an edit or a rollback, and is
    never hashed or stored. A tile never straddles a chunk, layer or region edge (const asserts).
  - **`TileSource` is the one abstraction**: three defaulted methods — `terrain()`, `tile_fill(p)`
    (voxel default: `Solid` for a colliding block), `tile_material(p)`. `pathfind`, `path_plan`
    (`OnMap` forwards them), `validate`, `current_tile`, `restore_translation` branch on
    `src.terrain()`; voxel code paths are the old expressions, untouched (voxel tests pass as they
    were). The client's `TileMirror` still answers voxel-only (P7).
  - **Standable rule (stud).** A body in tile `p` rests on a floor `o` plates above the tile's
    bottom: `Floor(k)` in `p` with `k ≤ 3` (`STUD_STEP_PLATES`) → `o = k`; `p` open over `Solid` →
    `o = 0`; `p` open over `Floor(k ≥ 4)` → `o = k − 6` (−2..0). So every floor height belongs to
    exactly one tile (`o ∈ −2..=3`; otherwise a third of plate-quantised terrain would be
    unwalkable). Room: every tile meeting the 11 plates (`STUD_BODY_PLATES`, 2.2 m) above the
    floor is `Open`, except the tile holding the floor. In tiles: the tile above is open, and for
    `o ≥ 2` the tile two above as well (11 + 1 ≤ 12 fits two tiles, 11 + 2 does not). Tile-grained,
    so conservative: anything in a tile the body reaches into blocks.
  - **Moves (stud).** 8 neighbours, level ±1, between standable tiles whose floors differ by at most
    3 plates up (walked — `step_is_walk_up` is true for every stud step, so walkers never plan a
    jump) or 6 plates down (`STUD_DROP_PLATES`; `CLIMB_COST` when deeper than a step). The lower
    column must have body room at the higher floor (the voxel rule's head check, both ways); a
    diagonal needs that room in both columns beside it. The A* heuristic drops its descent term
    on studs (a level can be descended in free steps). No wading/water on studs yet.
  - **Feet.** Feet rest half a plate over the floor (stud caps) or on it: a floor takes feet from
    0.25 plate under it (`STUD_FEET_SLACK_PLATES`) to a plate less that above; `body_centre_on`
    puts a body on the caps. People in a stud region now get the region's body
    (`BodyScale`: capsule, step, `walk_body_scaled`) — with `VOXEL` it is the old path bit for bit.
  - **Orders.** `validate` on studs: dig needs a non-open tile, not built, dominant material not
    bedrock; build needs a non-solid tile (it fills what is empty — so a build on a natural partial
    tile makes the whole tile "constructed", and deconstructing it carves all of it); deconstruct
    needs a built tile; chop is refused (no stud trees). A stud build may name any block but air.
    `dev_complete_all` on studs: builds bottom-up then removals top-down, each batch one write per
    chunk (`carve_stud_boxes_safe`, `fill_stud_boxes_safe`; `carve_stud_safe` is now the one-box
    form of the batch — same writes in the same order).
  - **Fill** (`Rollback::fill_stud_box_safe(lo, hi, material) -> cells`): the box's free cells as
    slabs of equal free-column masks, each covered by greedy rectangles and laid with `fill_box`
    parts; any chunk / layer; implied ground materialises. Across a seam: new
    `Order::FillStud` + `Intent::FillStud` (appended variants), routed like `CarveStud`.
    Designations themselves never cross a seam (clipped to the region, as on voxels).
  - **Materials.** `stud_material_of_block` (block registry name → stud material, else
    `STUD_BUILD_FALLBACK_MATERIAL` = plastic 71) and `block_of_stud_material` use the installed
    block props. `mining::stud_break_ticks` (hardness × cells × 15/64: stone tile 45 ticks = a
    stone block, dirt 12 vs 15) and `mining::stud_drops` (the same-named block's `drop_of`, one
    per 24 cells) exist and are tested, but **nothing deposits drops yet**: item piles are not
    ported, dev-complete drops nothing on voxels either, and survival mining on studs is not wired.
  - Tests: `game` `stud_tiles::` (16), `handoff_schedule::stud_dig_at_a_seam_*` (2).
  - **Found, not fixed (movement, P1's remit):** the real stud body does not reliably walk up a
    3-plate brick step: in `stud_tiles::bodies_walk…`'s world it settles 2 units short of the
    ledge and ends every tick where it began (1- and 2-plate steps work). Measured: replaying
    `handle_stairs`' three casts from the wall-hit pose, all pass (step 9.6 units). Not proven,
    my reading: the autostep nudges the capsule forward only by the tick's leftover motion, and
    the step-height "give back" then slides it off the ledge edge. `physics-next` `stud_voxels`
    walks into the step at one phase only, with the voxel capsule and no `depenetration_lift`.
    Until the controller is fixed (`handle_stairs` nudge, a fork change), a **stalled stud walk
    holds jump** (`steer_on`), which gets it over the step after ~1 s.
  - **Left open:** item drops into piles; survival mining / placing on stud tiles; trees/chop;
    stairs and ramps as slope parts (a slope tile reads as a plain partial floor); water and
    wading; `drop_at_feet` and chests still take the voxel feet tile; the client (`TileMirror`,
    picking, move-order preview, designation overlay) is voxel-only; a build does not check for
    bodies in the tile (voxel dev-complete does not either); tile reads outside `TileView` scan a
    chunk's bricks per tile (fine for single reads — measure before using in a loop).
  - No hashed type or `GameData`/`Ecs` layout changed (`Order`/`Intent` gained appended
    variants): `wasm_determinism`, `scene_hash_golden` and `physics_goldens` are untouched.
- **P6b trees / items / chests / detached bodies / water done (sim + worldgen; the client only
  compiles against it).**
  - **Brick step (task 0) — fixed in the game's controller parameters, no fork change.**
    Reproduced in `physics-next` `stud_voxels` with the real stud capsule through
    `movement::walk_body_scaled`: a 3-plate rise stalled for good from 7 of 16 sub-tick phases
    (1 and 2 plates never). Cause, as P6a read it: a brick (9.6 units) is taller than the capsule's
    radius (6.4), the autostep lifts the body but nudges it forward only by the tick's leftover
    motion, then gives the lift back — the round foot comes down on the ledge's EDGE, a contact
    steeper than rapier's default 45° `min_slope_slide_angle`, and slides back off (~2 units short,
    every tick). Fix: stud bodies get `min_slope_slide_angle = 90°`
    (`movement::STUD_EDGE_HOLD_ANGLE`): stud terrain is boxes, so a steep contact is always a brick
    edge, never a slope; held there, the next tick's step takes the rest of the rise. Voxel bodies
    keep 45° (`physics_goldens` unchanged). Side effect to look at in play: a stud body can perch on
    a brick edge instead of sliding off it. `STUD_STEP_PLATES` stays 3 and every stud path step is
    still a walk; the stalled-walk jump in `steer_on` stays as a safety net only. Pinned:
    `stud_voxels::stud_body_*` (rises from every phase, a 5-riser brick stair straight and
    diagonal at ≥ 80 % walking pace, 4 plates refused, walking off ledges),
    `stud_tiles::bodies_walk_up_a_brick_stair_without_jumping` (real bodies up six risers, never
    stalled long enough to jump, never rising more than a step per tick).
  - **Trees, worldgen** (`worldgen/src/stud_trees.rs`): `generate_stud_region` fills
    `StudRegionSeed::features` from the voxel generator's own placement (`trees_for_window` over
    `tree_ground` surfaces of the same patch — one patch with the tree margin now feeds recipes
    and trees; recipes stay bit-identical to `generate_stud_columns`) and skeletons. Only the
    shape step is new: integer rasterisation onto stud cells — wood along trunk and limb segments
    (skeleton levels 0–1; 0–2 for leafless species) in 1-stud x 3-plate courses, face-connected;
    leaves around the leaf clusters in 2-stud x 3-plate cells — greedy boxes, cut at chunk edges,
    tiled with `fill_box` parts. A tree stands on the highest of its base block's four stud
    columns (its foot reaches down to the others), keeps only cells in its region and above the
    recipe's ground; wood beats leaves, then the lower id. Materials: plain `wood` / `leaves`
    (species colours and a cactus green are open). **Tree id in `StudBrick.data` (u16)**:
    the low 8 bits of each axis of the voxel id's lattice cell (`stud_tree_id`), 0 remapped to
    0x8080 — unique within 1024 m on both axes, so within a region and its neighbours up to
    `region_blocks=512` (const assert). Measured (procedural seed 7, 64 m regions): 186–240
    bricks per tree; a region with 19 trees has 4 000 tree bricks beside 9 600 ground bricks —
    forests will dominate brick counts (next lever: hollow crowns, or leaves as fillers).
    The toy `stud_world` has no trees.
  - **Felling** (`game/src/stud_trees.rs`): `validate(Chop)` takes a tile holding tree WOOD
    (`stud_tree_at`). A brick records no limb, so the rule is by height and support: the cut is
    the tree's wood bricks meeting the chopped level within 4 tiles; everything of the tree
    starting at or above the lowest cut brick falls, plus whatever is then no longer joined to
    the standing wood of the tree's lowest level (nothing floats). A chop at the foot fells it
    all; a chop on a high branch fells the whole top from there. Bricks are found by scanning
    the chunks within 20 x 29 tiles for the id (leaves need not touch; pinned against the species
    table), own region only — as on voxels a generated tree never crosses a seam. One write per
    chunk; the cut's yield lies at the stump at once, the falling bricks are ONE stud body
    turning about the cut (`trees::tip_angvel`). Dev-complete chops last, with
    `SettlePolicy::DropItems`.
  - **Persistence of trees: left as P5 found it.** Feature chunks differ from their pristine
    chunk, so each is stored whole (a 64 m region with 19 trees: 21 chunk rows on its first save).
    "Pristine + features" needs the features at restore and at save, and `game` cannot call
    worldgen: it needs a regenerate hook on the region thread (or the features stored once per
    region) — not a contained `portable.rs` change, so not done.
  - **Item piles** (`item_piles.rs`): a pile cell is a tile on stud terrain. `pile_support`
    (a tile holds a pile up and out: `Solid`, or a floor over 3 plates) and the new
    `pile_rests_in` (also a low floor in the pile's own tile — a body's floor rule) replace the
    block reads; `pile_cell_seen` reads the stud halo; pickup reach is the region body's box in
    tiles; `drop_at_feet` and the chest reach use the stud feet tile. Every stud write
    (carve, fill, place, remove, detach) re-settles the piles it undermines or buries
    (`stud_piles_after_write_safe`).
  - **Drops**: `carve_stud_boxes_dropping_safe` puts `mining::stud_drops` down at the carved
    box's tile. Used by dev-complete digs and deconstructions, by `Order::CarveStud` and by the
    owner of an `Intent::CarveStud` — so a carve yields the same on both sides of a seam, each
    side into its OWN piles (nothing is sent back). Stud wood drops the `oak_log` item
    (`STUD_WOOD_DROP_BLOCK`); leaves and plastics drop nothing. `stud_material_of_block` maps
    every `*_log` / `*_branch` to wood, `*_leaves` to leaves.
  - **Chests** (`containers.rs`): the container stays tile-addressed; its terrain presence is the
    tile filled with bricks of plastic 70 tagged `STUD_CHEST_TAG` (1) in `data` (told from a tree
    by material). `place_stud_chest_safe` (an open tile) — reached by a `Build(chest)` order;
    opening by tile and the reach rule are unchanged; when the last tagged brick of the tile goes
    (carve, deconstruct, remove) the container spills into piles with the chest's own item. Saved
    and restored like any container (pinned).
  - **Stud bodies** (`stud_body.rs`): `Ecs.stud_body` — a brick model (offsets from the model's
    min cell), policy, settle counters, drops. One compound collider (a cuboid per brick), mass
    from `voxel_body::mass_properties_of_boxes` (no eigen solve), the voxel body's constants.
    `detach_studs_safe` (bricks out of their chunks, one write per chunk), `tick_stud_bodies_safe`
    (rest or timeout → removed, `DropItems` → piles at the tile it rests in), buoyancy, the
    `SetEntityPosition` stream and the seam-settle rule treat it like a voxel body.
    **Simpler than the voxel body on purpose**: no landed damping, no skirt snap, no
    `topple_angvel` raise — decide at a first look at falling trees. Renderer:
    `GameDataUpdateKind::AddStudBody` (appended; the client ignores it — P7).
    **Seams**: it travels in `Carried::stud_body` (drops in `Carried::drops`).
    **That changed a stored layout**: `FORMAT_ENTITY_BUNDLE` is now 2; format 1 rows are still
    read (`decode_entity_bundle` → `game::EntityBundleV1` / `CarriedV1`, golden
    `entity_bundle_v1.bin`) and rewritten as 2 by the next save. `STORE_FORMAT_VERSION` stays 1.
  - **Water** (`water.rs`): `water_units_at` on stud terrain is per tile and graded —
    `WATER_FULL` x the plates of recipe water standing over the tile's own solid top / 6
    (`stud_water_units`; the recipe's water lies between its ground and `water_top`, own ∪ stud
    halo). So wading is ≤ 2 plates (0.4 m) over the floor; deeper tiles are not standable
    (`stud_stand`), wading costs `WADE_COST`. `immersion_at` and `submerged_fraction` compare with
    the surface itself (plates), not tiles. Static as on voxels: a hole beside or under a lake
    stays dry, bricks built into water displace it. **Bodies are not labelled** (`water_bodies`
    stays empty on stud regions): nothing in the sim reads them; the renderer's water pass (P7)
    is their only user.
  - Tests: `game` `stud_gameplay::` (13, each mutation with the rollback bar),
    `stud_tiles::bodies_walk_up_a_brick_stair_without_jumping`,
    `handoff_schedule::stud_tree_*` (a tree felled two tiles from a seam falls across it: the
    body is handed over with model and drops, settles and drops its logs in the NORTH region,
    every log exactly once; a carve straddling the seam drops on both sides; replicas end in the
    server's hash — lockstep and 3 seeds of shuffled / late / jittery), `worldgen` `stud_trees::`
    (ids, every species, determinism, recipes unchanged, valid bricks inside their chunks, none
    overlapping the ground, trees standing, felling scan bounds, a generated tree felled),
    `server` `stud_world::stored_stud_region_keeps_its_trees_and_a_falling_one` (SQLite: trees,
    stump, the falling body's bundle and the pile restore), `world-store` `codec::` (format 1
    bundle still decodes), `physics-next` `stud_voxels::stud_body_*`.
  - **Left open:** survival mining / placing / throwing on stud tiles (the eye ray and
    `BreakBlock` are voxel-only, so chests cannot yet be placed or opened by a click);
    species materials; a branch-only chop; trees across region seams (none are generated);
    "pristine + features" storage; stud body look and feel (above); `settle_at_seam`'s drop cell
    is computed in blocks (lands a few tiles high and settles down); the client draws no trees'
    bodies, piles or water on stud terrain (P7).
  - Hashed layout changed (`Ecs.stud_body`): `wasm_determinism` and `scene_hash_golden`
    re-recorded (16-block values); `physics_goldens` did not move.
- **P7a client done (near field: meshing off-thread, edge culling, tiles and tools, falling
  bodies, water). Nothing was launched: every behaviour below is unit-tested, none was looked at.**
  - **Off-thread meshing.** `JobKind::StudMesh` (`stud/mesh.rs` `StudMeshJob`): `Chunk { chunk,
    neighbours: FaceSlabs }` or `Model(StudModel)`; output = body + stud-top vertex buffers
    (+ an offset), wrapped into `Mesh`es in the completion closure. A neighbour travels as the one
    layer of cells across the shared face (`Slab::{Free, Solid, Bits}`: 16 or 45 `u64` per face,
    one tag when empty or full) — all the mesher reads. No split parameters were needed (the
    body / tops split is by triangle tag). `StudAssets` is the process-global `stud_assets()`
    (`OnceLock`, decoded on first use in the page or a worker); the `Resource` is gone.
    `stud/world.rs` `StudWorld` (session-reset) is the incremental index: entity + boundary slabs +
    a dirty stamp per world chunk, the remesh queue, the stud halo, the recipes. A chunk is
    queued when it changes; a face neighbour only when the layer it sees changed (an interior
    edit remeshes one chunk); edits coalesce; one job per chunk at a time; a result whose stamp is
    stale is still shown and the chunk stays queued. Budgets: `STUD_MESH_JOBS_PER_FRAME` 8,
    `STUD_MESH_MAX_IN_FLIGHT` 16, `STUD_MESH_UPLOADS_PER_FRAME` 8.
  - **Region edges and the halo.** The index is keyed by `WorldChunkPos`, so a face on a region
    edge culls against the neighbouring region's chunk when it is loaded, else a stud-halo mirror
    of it, else implied ground (an unloaded chunk under its column's `surface_layers` is solid —
    which also drops the undersides of surface chunks), else nothing. The halo reaches the client
    through two appended render-only updates, `GameDataUpdateKind::SetStudHalo` /
    `SetStudHaloGround` (emitted beside the writes in `stud_halo.rs` only with a render sink, undo
    included — the voxel halo's `SetHaloTerrain` pattern; no state, hash or wire layout changed),
    plus the snapshot's `GameData::stud_halo_chunks()` / `stud_halo_recipes()` /
    `stud_ground_recipes()` (new read accessors). Halo chunks are data in the index, never
    entities, so they cannot be drawn.
  - **Tiles.** `TileMirror` answers `TileSource` for stud home regions by `Rollback::stud_tile`'s
    rule (materialised chunk → recipe; past the edge halo chunk → neighbour's recipe), with one
    `game::stud_chunk_tile_table` per mirrored chunk (rebuilt on change; ~54 KB per chunk, home
    region only), tree wood per tile, the stud column band and graded stud water. `game::tiles`
    gained three `pub` wrappers for that (`stud_chunk_tile_table`, `stud_tile_table_index`,
    `split_stud_tile`); the band and water rules are mirrored in the client and pinned against the
    sim tile for tile (`tiles::tests::stud::the_stud_mirror_agrees…`). `WithHalo` forwards on studs.
    **Picking runs in tile space**: `TileMirror::tile_ray` scales the ray's y by block height /
    tile height, so `pick_level_cell` / `pick_visible_cell` / `raycast_blocks` are reused unchanged
    and return stud tiles; `pick_solid` = any fill on studs. `WorkingLevel` carries the home
    terrain; every level ↔ height conversion (`level.rs`, `visible.rs`, move-order preview, gizmo
    boxes, underground / underwater eye tests) goes through `TerrainKind`. `surface_level` is
    made exact against `level_bottom_units` (19.2 does not divide exactly in `f32`).
  - **Tools.** Dig / build / erase / chop need no new orders: picking now yields stud tiles and
    `validate` runs on the mirror. Build still sends `Build(PLACE_BLOCK)` (dirt → `MAT_DIRT` by
    name). Designations on studs are shown by `stud/overlay.rs`: one translucent mesh of merged
    boxes per kind (the voxel overlay lives in the pull shader). `block_target` outlines the
    targeted BRICK on studs (`LookedAtStud`, the client's `stud_world_raycast`); **mining cracks
    are skipped** (survival mining on studs is not wired in the sim). `stud/build.rs` aims with a
    world ray over every loaded region ∪ the halo and sends `Order::PlaceStudAt`; a placement is
    valid only in the home region or its halo band. **Removal stays region-local**
    (`Order::RemoveStud` has no world form in the sim): a right click on a neighbour's brick sends
    nothing.
  - **Falling bodies.** `AddStudBody` (and a snapshot's `Ecs.stud_body`) → `stud/bodies.rs`: the
    model is meshed by the same job, unculled, into two children offset by minus the centre of
    mass; they ride the entity's interpolated pose and die with it. Tagged `VoxelBodyModel` so
    the avatar draws no placeholder box.
  - **Water.** `stud/water.rs`: per chunk column holding water, one surface mesh of merged quads
    at `water_top` where it is above `surface`, with the voxel water material (so it takes the
    cut). Recipe-only by design (static water, as the sim): no side faces, no dependence on
    edits; built once per region when its recipes arrive.
  - Tests (client `--lib`): `renderer::stud::` (24: job = the old main-thread mesher on a busy
    chunk + six neighbours, slabs = neighbour occupancy on every face cell, bincode round trip
    of chunk and model jobs, hand-built culling cases, the index's dirtying / coalescing /
    stale / replaced / budget / edge → halo → recipe rules, an ECS run through the job path,
    bodies, `PlaceStudAt` across a region edge, overlay, water), `commander::tiles::tests::stud::`
    (4), `commander::level::` (stud levels), `block_target::` (brick box), `bridge::` (halo
    updates and stud bodies); `game` `stud_halo::the_render_stream_mirrors…`. `just test-threads`
    passes (seam replicas still end in the server's hash).
  - **Left open:** everything visual is unseen (z-fighting of the overlay boxes, water alpha,
    the body mesh's look, stud-top LOD on bodies); the fortress cutaway / level darkening / x-ray /
    boundary line on stud materials, `stud_pull`, first-person stud tops, transparent materials,
    LOD (next slices); entering Adventure (`adventure::focus_tile`) still hands the sim a
    voxel-height level hint (the sim picks the nearest standable tile in the column); a body
    model is meshed unculled, hi-detail (an 8 000-brick model is a big mesh — cull inside the
    model if felled forests get heavy); implied-ground culling is not re-evaluated when a
    recipe arrives after its neighbours' chunks were meshed, except through the halo recipe
    path; the tile mirror keeps tables for the home region only, and a home flip re-derives
    them on the main thread (not measured at 256 m); the web build was not type-checked
    (no cheap wasm check recipe) — the new code uses only `compute::spawn`, `OnceLock` and serde.
  - Two client tests fail in code this phase did not touch: `block_target::stages_grow_the_same_cracks`
    and `commander::selection::click_anywhere_on_a_tall_body_hits_it` (both pure, both in
    unchanged functions).
- **P7b client + P9 UI done (fortress shading on the per-chunk path, glass, body sizes, the Slint
  palette). Nothing was launched: every line below is unit-tested or type-checked, none was looked at.**
  - **Fortress view on stud terrain — done.** `stud/material.rs`: stud meshes draw with
    `StudTerrainMaterial = ExtendedMaterial<StandardMaterial, StudFortressExt>` and
    `assets/shaders/stud_terrain.wgsl` (compiled in). The extension binds the SAME
    `FortressViewUniform` the voxel chunk material binds (copied by `sync_stud_materials`; its cut
    plane already comes through `TerrainKind`, so it is a tile boundary), plus
    `StudFortressParams` — the level height and `DEPTH_DARKEN_PER_LEVEL` / `DEPTH_DARKEN_FLOOR` passed
    from the Rust consts, so the shader restates no number — plus `BoundaryUniform`.
    - *Cut*: a per-fragment discard above the plane inside the home rect (no vertex clamp: stud meshes
      are baked part triangles). Forward AND prepass: the file has a `PREPASS_PIPELINE` half, and the
      opaque material is `AlphaMode::Mask` while the cut is on so Bevy runs it in the depth / shadow
      passes — cut-away bricks cast no shadow. With the cut off it is plain `Opaque`, culled, as before.
    - *Cap*: while the cut is on the opaque material is unculled (`StudFortressKey`) and a back face
      inside the home region is drawn as the cap (shaded at the eye ray's crossing of the plane, facing
      up, brick colour × (1 − `CAP_DARKEN`)) — the voxel fallback's rule. Seam-line quads carry vertex
      alpha `STUD_EDGE_ALPHA` (0.9, invisible) so their back sides never cap.
    - *Darkening*: the voxel curve, counted in stud levels (`material::depth_darkening` is the WGSL's
      Rust twin, tested).
    - *Applies to* chunk bodies, stud tops and falling bodies (one shared opaque material; the cut
      is by world position) and the glass material (cut + darkening, no cap). The **designation
      overlay** keeps its own unlit material and is cut on the CPU instead: with the cut on, tiles above
      the working level are left out of the mesh, those on it are drawn whole (their box stands 0.2
      units over the plane, which a fragment cut would shave).
    - *Slice cap for implied ground — done* (`slice_plane.rs` `stud_slice_colors`, one more texture
      binding and a mode flag in `slice_plane.wgsl`; the voxel path reads what it read): a colour per
      tile — the dominant material's LDraw colour where the working level's tile and the one above are
      both `Solid` (bricks or recipe alike), the water tint through recipe water. No designation tint
      on it on studs (the boxes stand over it).
    - *Home boundary line — done* as a shader addition: `apply_boundaries` copied from
      `voxel_pull.wgsl` (test-pinned against it), fed `BoundaryUniform` at binding 102. X-ray: not done
      (out of scope).
    - Shader test: `material::tests::stud_terrain_shader_is_valid_wgsl_in_every_variant` composes and
      validates seven variants (forward ± vertex colours, prepass with no output / depth emulation /
      normal / motion vectors) against stubs of Bevy's PBR modules, like the other shader tests.
  - **Stud tops in first person — done.** `stud_lod` centres on the local body's eyes while
    `FirstPerson::on` (`STUD_TOPS_FIRST_PERSON_RADIUS_M` = the commander radius, 40 m).
  - **Transparent colours — done.** `StudAssets::is_transparent` (colour alpha < 255). The job output
    gained `glass`; a chunk (and a body) gets a third mesh child with the blended glass material
    (`NotShadowCaster`). Rule: opaque faces / studs cull against OPAQUE cells only — `FaceSlabs` now
    hold opaque cells, so also across chunk and region faces — hence a face between glass and opaque
    stays on the opaque side and studs show under glass; a glass face culls against opaque cells and
    same-colour glass in its own chunk. Not culled: same-colour glass across a chunk face (one inner
    wall per chunk border). No transparent NATURAL material exists today (ice is an opaque colour).
  - **Sizes — done.** `cut_entities::feet_offset`, `selection::body_extent` / the selection ring and
    the camera fallback eye read `ClientBodyScale`; `adventure::focus_tile` / `toggle_order` go
    through `TerrainKind::feet_tile` (the P7a risk).
  - **The two failing client tests — both predate this plan; left as they are.**
    `commander::selection::click_anywhere_on_a_tall_body_hits_it`: `selection.rs` equals `HEAD`; test
    and `ScreenBody::distance` came together in `04ba4222` and disagree — a click 30 px under the feet
    of a 20 px half-width body is 10 px from its capsule outline, inside the 12 px slack, and the test
    wants `None`. `block_target::stages_grow_the_same_cracks`: `block_target.rs` is an untracked file
    of the user's mining work; `crack_order()` (fixed seed, no input) yields 28 texels where the test
    asks ≥ 40; the stud phases only added `LookedAtStud` code beside it.
  - **Slint UI (P9) — done.** `ui/stud-build.slint` `StudBuildPalette` (bottom right while build mode
    is on): the part list (catalog name + footprint), Rotate, the natural materials by name, a swatch
    grid of all 322 baked colours, the selection line, Close. `stud/palette.rs` is the pure model
    (`PaletteAction`, `StudBuild::apply`, `palette_view`), `stud/palette_ui.rs` the Slint glue (its own
    action queue; keys still work). `StudBuild` now carries a `material: u16` (was an index into eight
    colours). **Build tool material**: `DesignateState::build` (a `BlockId`, default `PLACE_BLOCK`);
    on stud terrain the context box lists the naturals a block can name (`palette::build_blocks`), and
    `palette_ui::tests::the_build_tool_builds_the_picked_material_in_the_sim` runs the order through a
    real stud `Region`. **Readouts**: the level text and the drag size count tiles and, on studs, add
    metres ("Level 52 · 6 below surface (7.2 m)", "4 × 3 × 2 (4 × 3 × 2.4 m)"). **Items**: icons are
    block items, nothing to change; the mining crack cube is now refused on stud terrain
    (`block_target::crack_target`). Headless Slint tests: `slint-ui` `stud_palette::` (4).
  - **Web — type-checks.** New `just web-check` (`cargo check -p client --target wasm32-unknown-unknown`);
    `just slint-wasm` passes.
  - **Unseen (needs a look on a real adapter, native and WebGL2):** the cut line on studs and slopes;
    the cap's look where it is a back face (its depth is the back face's, not the plane's — as on the
    voxel fallback); whether seam-line backs leak into caps; masked-vs-opaque switching when the cut
    toggles (pipeline respecialisation hitch?); shadows with the cut on; depth darkening strength at
    1.2 m levels; the slice plane's flat colours against lit bricks (z-fight where a brick top lies on
    the cut); the boundary line on bricks; glass sorting (per-mesh blend order, glass over water) and
    its 0.5 alpha; stud tops popping at 40 m in first person; the palette's size and placement at
    other window sizes / UI scales (tested at 1280 × 720 only), swatch size (11 px) and whether 322
    colours want grouping; the Build tool's list with 20-odd rows in the context box. The shader was
    validated against STUBS only: the first run on a real adapter is the first true compile of
    `stud_terrain.wgsl` against Bevy's `pbr_fragment` / `prepass_io` (whether `just pull-smoke`
    reaches a stud world was not checked).
  - **Left open:** x-ray on studs; the designation overlay as a shader tint (still boxes);
    `stud_pull`; LOD; a natural transparent material; `Order::RemoveStud` across regions; item
    hotbar building on studs (survival placing is not wired in the sim).
- **P8 far terrain done (data, builder, wire, client mesh + rings, defaults). Nothing was launched:
  every line below is unit-tested or type-checked, none was looked at.**
  - **Cell format** (`game/src/stud_lod.rs`): a `StudLodTile` per `LodKey` — the ground recipe
    point-sampled on the key's **cell-corner lattice**: `side + 1` samples per edge, `side =
    LodWater::side_for(key)` cells of `key.factor` sim units (factor 8 = one stud; 32 = 2 m). A
    sample is the recipe of the stud column whose min corner is the lattice point: `top` (**plates**,
    the recipe's frame, `i16`), `material` (stud material id of the top plate), `water` (plates, one
    past the top water plate where it stands over the ground, else `STUD_LOD_NO_WATER`). On the wire:
    tops as byte deltas (escape + 2 bytes past ±127), materials and water as `(run, value)` RLE —
    about a byte per sample before lz4. Corners, not cell centres, so tiles of one factor share their
    border samples and every coarser lattice is a subset of the finer ones: far terrain equals near
    terrain and its neighbours **exactly at the lattice points** (it can differ by a cell's relief
    between them).
  - **Generation** (`worldgen::stud_gen::stud_lod_tile`): the per-column sampling of
    `recipes_from_patch` was factored into `stud_column` / `stud_strata` (recipes are bit-identical —
    the `stud_gen` / `stud_trees` tests pass unchanged) and the tile reads the same function, with
    `game::stud_top_material` for the top plate. Procedural or forged (`forged::active`), like
    `coarse_tile`. **Not in it: trees, and anything edited or built** — far terrain is a function of
    the key alone. The voxel path's "top-down of a running region's chunks" was not a contained
    addition here (`TerrainSnapshot` carries only stud edge columns), so built structures do not
    show in the far field yet.
  - **Builder and manager — a sibling path, voxel types untouched.** `LodBuilder` gained three
    defaulted methods: `stud_terrain()`, `submit_stud(key)`, `poll_stud()`. For a stud builder the
    manager sends no `LodBuildJob` at all: `RequestLod` → `stud_lod_cache` hit or `submit_stud`;
    `pump_lod_results` also drains `poll_stud` → `handle_stud_lod_result` → `StudLodData` to every
    subscriber. Same in-flight dedupe, counters, LRU cap and subscriber pinning as voxel keys.
    `invalidate_region_lod` is a no-op (nothing a region reports changes a tile). Keys finer than a
    stud or wider than `STUD_LOD_MAX_SIDE` (256 cells) are dropped (`stud_lod_supported`).
    `game::InlineStudLodBuilder` (tests / an in-page server), `server::lod_workers::ThreadStudLodBuilder`
    (a thread pool; each worker pins the key's forged tiles with `prefetch_lod`, a failed load is
    retried with the voxel builder's backoff — its retry thread is now generic over the job).
    Server: the procedural and the stored (`VOXEL_TERRAIN=studs`) stud worlds use it; the toy world
    keeps `NullLodBuilder` (nothing beyond its 3x3). `LodResult`, `LodPayload`, `build_lod_job` and
    every voxel test are as they were.
  - **Wire**: `ServerPacket::StudLodData(LodKey, StudLodTile)` and `ServerPacket::StudTerrain`
    (sent once on connect by a world with a stud builder), both appended (pinned: `LodData` keeps
    variant index 6). `RequestLod` / `ReleaseLod` are shared. `ClientUpdateEvent::StudLodData`.
  - **Client rings.** A stud world subscribes on `game::stud_ring_table(table)`: the configured voxel
    table (default, `VOXEL_LOD_RINGS`, `view=`) with the same bounds and reach, every factor 4x
    coarser (`STUD_LOD_COARSEN`), raised to a supported one, capped at 512, equal rings merged —
    64 m regions: `(2,8) (3,16) (6,32) (13,64) (16,128)`; 256 m: `(2,32) (3,64) (6,128) (12,256)
    (60,512)`. The instance swaps its table (`use_stud_lod_table`) on `StudTerrain`, which arrives
    before its region does, so its first request is already on the stud table; a stud region
    snapshot swaps it too (the toy world sends no `StudTerrain`), and keys asked for on the voxel
    table before that are released by the next window diff. `join_timeline` counts expected keys on
    the same table.
    `LodEventMsg::StudData` takes the voxel slots: re-receipt, retirement, deadlines and the near
    takeover are `lod_rings`' own. **Hand-over**: a far tile of a region that joined the near window
    stays until every stud chunk of the region has its first mesh
    (`UnmeshedNearChunkFilter` gained `SimStudChunk` without `StudMeshChildren`), then goes; far
    keys never include near regions (`desired_lod`), so there is no double draw on the default
    tables. A custom table whose tile-keyed ring (a key spanning several regions) reaches into the
    near window overdraws it, as the voxel rings do.
  - **Client mesh — D-far decided: a smooth height field, not prisms** (`stud/far.rs`,
    `JobKind::StudLodMesh`): one vertex per lattice point (top in plates → units, the material's
    LDraw colour, opaque), two triangles per cell, normals from height differences; a **skirt** one
    cell edge deep on every border closes the gaps where a tile meets a coarser one or the near
    bricks between shared lattice points. Why not stepped prisms (the roadmap's wording): the Bevy
    mesh path pays ~40 bytes a vertex, a prism per cell is ~3 unshared quads against one shared
    vertex, plate-quantised heights almost never merge at 2 m+ cells, and stepped tops have only
    up-normals (the relief's shading would come from walls alone). Prisms want `stud_pull`'s 8-byte
    records. Water: a flat sheet over every cell with a wet corner at the highest of its corners'
    levels, alpha from the cell's mean depth, merged greedily, in the voxel water material
    (`LodWaterSurface`, so it never counts as terrain readiness). Terrain draws with the shared
    opaque `StudTerrainMaterial`, so cut / darkening / boundary line are the near bricks' (the cut
    applies inside the home rect only, as on the voxel LOD). Neither child casts shadows.
    `StudLodTasks` is session-reset; roots and children are `SessionScoped`; 4 uploads a frame.
  - **Defaults**: `just run-studs --procedural` no longer sets `VOXEL_LOD_RINGS=off` (the toy world
    still does); dials documented at the recipe.
  - Tests: `game` `stud_lod::` (5 unit: codec, malformed tiles, ring table) and `--test it stud_lod::`
    (6: manager round trip and cache, unsupported keys, LRU + pinning, the sibling trait path, wire,
    the connect announcement);
    `worldgen` `stud_lod::` (2: every lattice point equals the recipe column on an inland and a shore
    region incl. the sea level and sand floor; determinism, shared borders, coarser lattices are
    subsets); `server` `lod_workers::stud_*` (2: threads answer every key once and retry failed
    loads; the real builder = `stud_lod_tile`); client `stud::far::` (4: flat, a step, water, job
    round trip / malformed), `lod_rings::` (3: slots + re-receipt, near takeover, bad tiles),
    `instance::…a_stud_world_subscribes_on_the_stud_ring_table`. `just smoke`, `just web-check`,
    `just test-threads` pass. No hashed type or `GameData` / `Ecs` layout changed.
  - **Found, not fixed:** eight voxel `lod_rings::` tests fail in this tree (`lod_data_spawns_root…`,
    the re-receipt / removal / near-takeover ones): they wait for a `Mesh3d` child of the root, but
    cells with a `LodGroup` are merged into LOD tiles and `pipeline_app` runs no `rebuild_lod_tiles`.
    The code they exercise is the voxel `Data` path, which this phase did not change.
  - **Left open / unseen:** everything visual (LOD pop at the near edge, skirt visibility on steep
    seams, colour banding from per-vertex materials, water alpha against the near water, far terrain
    casting no shadows, whether 4x coarsening is the right budget); far trees; built structures;
    the cost of the full 15 km table
    (~1 200 keys at 256 m; nothing was measured); the recipe-only tile ignores frozen water (as the
    stud recipe does).
- **Stud renderer perf (2026-11-10, after the first launch ran at ~3 fps on an Iris Xe). Measured
  offline, fixed, NOT relaunched — fps is unknown until the next launch.**
  - **Measured** (`stud/measure.rs`, ignored tests that print a table:
    `just test-one client "--lib renderer::stud::measure:: -- --ignored --nocapture"`; one 256 m
    region = 4 x 4 test regions of the procedural world with trees, through the real job): 437
    chunks, 325 076 bricks. The first launch's mesher drew, per region, a **body of 8.82 M
    triangles / 21.3 M unshared 40-byte vertices (915 MB)** — seam-line quads 5.13 M (58 %), brick
    undersides (`Inner` tubes) 1.91 M, tree faces 1.25 M, ground faces 0.54 M — **plus 7.90 M stud
    triangles (995 MB) uploaded for EVERY chunk** although shown within 40 m only. Nine regions:
    79 M body triangles and ~17 GB of vertex buffers on an integrated GPU, all of the body a
    shadow caster in four cascades. That, not any per-frame system, was the 3 fps.
  - **Fixed.** (1) Terrain chunks are meshed as merged faces: per cell face exposure (was: a whole
    brick face if any cell across was free), coplanar faces of one colour merged greedily across
    box bricks into 4-vertex quads; non-box parts keep their baked triangles
    (`mesh::merged_box_faces`). Body per region now **610 k triangles / 1.22 M vertices (53 MB)**:
    14x fewer triangles, 17x fewer bytes. (2) Studs and seam lines are a **detail** mesh built
    only for chunks within `STUD_TOPS_RADIUS_M` (40 m) of the viewer — the job carries `detail`,
    `StudWorld::set_detail_centre` keeps the set (re-cut when the viewer moved 4 m, half a chunk
    of hysteresis; a gain is one job, a loss just drops the mesh). ~28 chunks, ~750 k triangles,
    82 MB around one viewer instead of 13 M / 1.4 GB per region. Detail casts no shadows.
    (3) Everything outside the home region's rect (the other regions' chunks, all far tiles)
    draws with a third material that never takes the cut (`StudMaterials::outside`,
    `retarget_stud_materials` on a home change): with the cut on, the shared material had made
    every region and ~1 200 far tiles alpha-masked and two-sided, fragment shader in the depth and
    shadow passes included. (4) The per-frame walk over every chunk's stud-top visibility
    (`stud_lod`) is gone. Launch totals now: 3 933 chunks, ~5.5 M body triangles (482 MB) + ~0.75 M
    detail.
  - **Gave up on purpose:** hollow brick undersides (tubes) on terrain — an underside is a flat
    quad now (leaves seen from below); brick boundaries beyond 40 m (flat colour there); glass
    bricks, falling bodies and the build ghost are still whole baked bricks.
  - **Found, not changed:** no per-frame mesh or material re-creation (`sync_stud_materials`
    writes only on a change; overlay, water and far tiles are change-gated) and no remesh loop in
    the code (dirtying is event-driven) — the launch log has no stud counter to prove it. The far
    field is ~2.7 M triangles in ~1 200 draws (60 % of them the 32 m-cell outer ring), no
    shadows; snapshots stay 6–10 MB a region (325 k bricks: real bricks under every surface
    column) — the next thing to cut if loading matters.
  - **Look for in the next launch:** one `[perf] event=stud_region_meshed region=… chunks=…
    body_tris=… body_verts=… detail_meshes=… detail_tris=… detail_set=…` line per region (expect
    ~600 k body triangles a region; `detail_tris` > 0 only around the camera); fps with the cut on
    and off; `VOXEL_SHADOW_CASCADES=1` / `VOXEL_SHADOW_DISTANCE_M=100` as an A/B if it is still
    slow (shadow passes re-draw the bodies); `VOXEL_LOD_RINGS=off` to see what the far field
    costs; T-junction pixel cracks on merged faces; seam lines popping at 40 m.
- Goldens re-recorded on purpose (16-block values): once for P6b (above); `wasm_determinism` and `scene_hash_golden` once
  more for P4 (`GameData.stud_halo`, `stud_halo_ground`; `physics_goldens` did not move); before
  that `wasm_determinism` twice (`BodyScale` fields,
  `GameData.stud_ground`), `scene_hash_golden` (it was already stale from the experiment's layout
  change, so that step was not separately proven layout-only). `physics_goldens` did not move.
- Not run: client unit tests (opt-in), `just run-studs` (needs a look), any benchmark.

## Decisions (confirmed 2026-11-09)

| # | Decision | Recommendation | Alternative |
|---|----------|----------------|-------------|
| D1 | Pristine terrain | Never stored or sent: derived per chunk from the ground recipe on every machine. | Store/send every chunk (5 MB/region measured — does not scale). |
| D2 | Sim tile + chunk height | Tile = 2×2 studs × 2 bricks (1 m × 1.2 m × 1 m); a person is 2 tiles tall, as today. Chunk height 75 → **90 plates** (18 m, 288 units, 15 tiles). | Tile = 1 brick tall, chunk stays 75 plates; corridors are 4–5 levels high and every designation is thinner. |
| D3 | Digging | `carve(box)`: remove the cells, re-decompose what is left of straddling bricks into catalog parts (LEGO Worlds behaviour). `place` still refuses overlaps. | Dig whole bricks only (ragged holes, tiles don't line up). |
| D4 | Material identity | `StudBrick.material: u16` into a material table (name, LDraw colour, hardness, drop, kind) replaces `color` as the sim identity and replaces `blocks.ron`. | Keep colour only; no hardness/drops/liquid — no mining economy. |
| D5 | Old worlds | No voxel→stud conversion. Store format bumps; existing worlds (incl. the deployed web world) are re-forged. | Write a converter (1 m block → 2×2×5-plate bricks; ugly, large). |

Also open, not blocking: step height (plate vs brick, `TODO.md`), far-terrain look (D-far in Phase 8),
glTF vs `parts.bin`.

## Target design

### Grid and scale
- *(experiment spec)* stud 0.5 m = 8 units, plate 0.2 m = 3.2 units, brick = 3 plates; chunk footprint
  32×32 studs = 16 m = the sim chunk footprint; x/z reuse `ChunkCoords`/`REGION_CHUNKS`.
- Chunk height `STUD_CHUNK_PLATES = 90` (D2): a multiple of 15 plates (whole units), of 6 plates
  (whole tiles), `u8`-safe. Fine collider grid 32×180(+cap)×32, coarse 8×6×8.
- Layers are unbounded: `STUD_LAYERS` goes away; `StudChunkCoords.y: i32` covers the forged range
  (−1024 m … +4096 m ≈ layers −57 … 228).
- One body scale. People are 2.4 m minifigures everywhere; `BodyScale` and the 1.8 m constants are
  deleted at cutover. **Collision capsule 2.2 m** so a person on floor studs (0.1 m cap) clears a
  2-tile (2.4 m) corridor — pinned by a `physics-next` test in Phase 1.
- The `chunk_blocks` dial dies with voxels (`CHUNK_UNITS` becomes a plain const 256); `region_blocks`
  stays, renamed in docs to metres.
- Naming: after cutover the 32³ render "brick" no longer exists; `stud` stays the name of the toy-brick
  code (no rename churn).

### Data
- `StudBrick { min: [u8;3], shape: Part{part,rot} | Filler{size}, material: u16, data: u16 }` —
  `data` is 0 for terrain, a tree id for wood/leaves (felling needs it), free for later.
- `assets/studs/materials.ron` → `game::StudMaterials` (installed like block props): `name`,
  `ldraw_color`, `hardness`, `drop`, `kind` (Solid / Cutout / Transparent). Player-built bricks pick
  a material whose colour they chose (a "plastic <colour>" family generated from the palette).
- `StudChunk` stays the canonical `BTreeMap<min, StudBrick>` with `place` (refuses overlap),
  `remove_at`, and new `carve(lo, hi) -> Vec<StudBrick /*removed remainders as drops*/>` (D3) using
  one shared deterministic `decompose(cells, material) -> bricks` routine.
- Bricks never cross a chunk boundary *(experiment spec)* — this is what makes per-chunk purity and
  seams cheap.

### Ground recipe and pristine chunks (D1)
- `StudColumnGround` per **stud column** (0.5 m; 512² per 256 m region): `surface` (plates, world),
  soil depth + cover material, strata reference, `bedrock`, `water_top` (plates), `water_body`.
  Stored run-length/palette-compressed per chunk column (surface deltas are small).
- `game::stud_pristine::pristine_chunk(coords, &GroundWindow) -> Option<StudChunk>`: a **pure
  function** of the chunk's own 32×32 columns (plus a 1-column apron only for slope choice). Fully
  buried chunk → one `Filler` per stratum run; fully air → `None`; surface chunks → `Filler` up to a
  few plates under the lowest surface, then running-bond courses and plates (the experiment's
  `Builder::lay`, generalised to 2-wide parts, slope parts on one-brick rises, and world-aligned
  course phase so bond patterns agree across chunk and region edges).
- Lives in `game` (engine-agnostic, used by server, client prediction, halo, LOD). Worldgen only
  produces recipes (+ features, below).
- A region's sim holds: the ground recipe, materialised pristine chunks for the band the sim needs
  (surface band + anything touched), and edited chunks. `Ecs.stud` gains an `edited: bool` beside the
  chunk (hashed). Reading below the band answers from the recipe ("implied ground", as today's
  `implied_block`); the first edit materialises the chunk (`materialize_stud_safe`, undo-exact).
- **Features** that are not a function of the column (trees, later sites/structures) are generated as
  explicit brick lists by worldgen and merged into the chunk at genesis; chunks containing features
  are flagged `edited` only if a player changes them — the feature list itself is regenerable
  (`worldgen::stud_features(region) -> Vec<(StudChunkCoords, Vec<StudBrick>)>`, pure).
- Water stays recipe data *(CLAUDE.md rule)*, now in plates; `water_units_at` reads it unchanged in
  spirit.

### Tiles (D2)
- `TilePos` keeps its meaning "one nav/designation cell", now 2×2 studs × 6 plates. `TILE_STUDS = 2`,
  `TILE_PLATES = 6`; every literal says which grid it is on.
- `TileSource` is reimplemented over stud occupancy + recipe: per tile a derived
  `TileFill { Open, Floor(plates), Solid }` (highest occupied plate in the tile's 4 columns, capped
  at the tile); `standable` = fill below is `Solid` or `Floor` within step height, two tiles of
  headroom `Open`. Derived per chunk into a cached `StudTiles` (invalidated by chunk edits, **not**
  hashed — it is a pure function of hashed state).
- Designations/constructions stay `TilePos` maps. Dig = `carve` the tile box; build = fill the tile
  box with decomposed bricks of the chosen material; stairs/ramps = slope parts.

### Seams
- Stud halo mirrors `halo.rs`: one band of neighbour chunks as fixed colliders. Pristine neighbours
  are derived locally from the neighbour's recipe edge (sent once, small); only **edited** neighbour
  chunks travel as halo updates/deltas.
- `WorldView` gets `stud_cell`, `tile_fill` and a stud raycast over own ∪ halo; `Intent::BreakBlock /
  PlaceBlock` become `Intent::Carve { box } / PlaceStud { brick }`. Intents never wake a region;
  drops from a carve go through the existing salvage path.

### Persistence and wire
- Store: migration `0004`: `stud_chunks(rx, rz, cx, cy, cz, data)` holds **edited chunks only**;
  `regions.ground` holds the stud recipe (`FORMAT_STUD_GROUND`), `FORMAT_STUD_CHUNK`; `Dials` gains the
  stud constants; `STORE_FORMAT_VERSION` 2, `WORLD_MAP_VERSION` bump. All writes stay fenced.
- `PortableRegion { stud_chunks: BTreeMap<StudChunkCoords, StudChunk> /*edited*/, ground }`; chunk
  terms diff as today. `portable::classify` / `carry_components` reclassify `stud` (persisted, never
  travels).
- Region snapshot to clients = recipe + edited chunks + entities. `SetStudChunk` (whole chunk) is
  replaced by `StudDelta { placed, removed }` updates. Target: snapshot ≤ voxel snapshot size.

### Rendering
- `JobKind::StudMesh`: input = chunk + 6 neighbour occupancy faces by value (workers have no ECS);
  `StudAssets` becomes a process-global `OnceLock` (already `include_bytes!`). Works in Web Workers.
- **`stud_pull`** pipeline, forked from `voxel_pull` (arena, indirect draws, cull): an 8-byte quad
  record for axis-aligned box faces in stud/plate cells (x,z 6 bits, y 8 bits, size, face, material
  index). Seam lines and edge bevels are drawn procedurally in the fragment shader from quad UV +
  brick extents (replacing baked edge quads). Non-box parts (slopes, round) go through a small
  triangle side-buffer. **Studs** are instanced lo-detail meshes near the camera, a normal-map-free
  shaded disc impostor at mid range, nothing far. `records.rs`' successor is locked to
  `assets/shaders/stud_pull.wgsl` the same way.
- The per-chunk Bevy `Mesh` path (today's) stays as the WebGL2 fallback, with an `ExtendedMaterial`
  carrying the fortress uniform.
- Fortress features move to the stud shaders: cutaway/level darkening (levels are tiles: 1.2 m),
  slice cap from the recipe, designation overlay (one byte per tile, 3D texture as today), home
  boundary line, x-ray (interior bit from the mesher).
- `TileMirror` is fed from stud chunks + recipe (`StudTiles`), which restores designation picking,
  `visible.rs`, move orders, `block_target` (first-person highlight/mining cracks on the targeted
  brick), and water meshing.
- Far terrain (D-far, Phase 8): server LOD cells derived from recipes (`worldgen::coarse_tile` is
  already column-derived), drawn by the raster LOD rings as coloured height prisms on the plate grid.
  `far_voxel/` (brick raymarch) is deleted, not ported.

## Phases

Dependencies: P1 → P2 → P3; P4, P5, P6, P7 each need P2 (P6/P7 can run in parallel; P7's tile work
needs P6's `StudTiles`); P8 needs P3 + P7; P9 needs P6 + P7; P10 is last.
Milestones: **M1** (after P3 + the P7 mesh job) walk a forged stud world · **M2** (P4 + P5) seamless
and persistent · **M3** (P6–P9) fortress parity · **M4** (P10) voxels gone.

Every phase: targeted `just test-one`, `just smoke`; rollback bar `hash(before) == hash(after undo)`
for every new mutation; `just determinism-wasm` when anything hashed changes (re-record goldens on
purpose, once per phase); `just test-threads` when a seam, `WorldManager` or `region_threads` is
touched; perf numbers in release, same scale, **ask before benchmarking**.

### P0 — Baseline and spec
- Record the voxel numbers the stud world must match (release, `region_blocks=256`): region genesis
  ms, snapshot bytes, client `world_ready`, steady frame time (`just fps-steady`), store bytes/region.
- Turn "Target design" into `docs/superpowers/specs/<date>-stud-world-design.md` once D1–D5 are
  answered.
- **Exit:** numbers in the spec; decisions signed.

### P1 — Data foundation (game, bake tool)
- `stud.rs`: 90-plate chunks, unbounded layers, `material` + `data` fields, `StudMaterials` +
  `assets/studs/materials.ron`, `decompose`, `carve`; compact codec (varint + palette) for chunks.
- `stud_ops.rs`: `carve_stud_safe`, delta update kinds (`StudDelta`), `Order::CarveStud`.
- `tools/ldraw-bake/parts.txt`: add tiles (smooth plates), inverted slopes, 1×1/2×2 round, 2-wide
  plates up to 2×8 if missing; fix the Y-mirror (`TODO.md`); `just ldraw-bake --check`.
- `physics-next`: pin the 2.2 m capsule in a 2-tile corridor over stud caps; carve never leaves a
  snagging edge.
- **Exit:** experiment world runs on the new layout; carve is rollback-exact; goldens re-recorded.

### P2 — Ground recipe and pristine chunks (game)
- New `stud_ground.rs` (`StudColumnGround`, compressed store, implied reads), `stud_pristine.rs`
  (`pristine_chunk`), `edited` flag, `materialize_stud_safe`, recipe water + body labelling in plates.
- `Region::from_stud_ground(recipe, features)` replaces `seed_studs`; band selection mirrors
  `materialized_sub_chunks`.
- Tests: purity (same chunk from any window), bond continuity across chunk/region edges, implied ==
  materialised occupancy, materialise/undo hash bar, genesis cost gate (ignored perf test).
- **Exit:** toy world generated as recipe → pristine; region genesis within 2× the P0 voxel number;
  snapshot (recipe + 0 edited chunks) under the voxel snapshot size.

### P3 — Forged worldgen (worldgen, server)
- `columns_from_patch` gains a stud output: sample the density/profile at **stud columns**, quantise
  the surface to plates, map `Materials`/strata/cover to stud materials, re-derive `SEA_LEVEL` and
  landing on the plate grid; `generate_region` → `StudRegionGround`; `stud_features` for trees
  (trunk/branch/leaf parts with tree ids, replacing voxel tree masks).
- Server: `WorldSource` forged/procedural paths feed stud recipes when `VOXEL_TERRAIN=studs` (the
  strangler switch; default still voxel); region prefetch reused; `with_genesis` hook removed.
- The fused `column()` ↔ `DensityFn` mirror rule is untouched (profiles, not blocks).
  `forge_quality` gate must still pass; bump `WORLD_MAP_VERSION` if `build_world_map` output changes.
- `debug_launch.rs`: `client debug studs` forges/reuses `worlds/debug-studs/`; `just run-studs`.
- **Exit (M1 with the P7 mesh job):** embark on a forged world rendered and collided as bricks.

### P4 — Seams
- `stud_halo.rs` (seed from running neighbour / store / recipe, edited-chunk deltas),
  `WorldView` stud reads + raycast, `Intent::Carve/PlaceStud`, `world_manager` halo seeding,
  client `regions/runner.rs` halo updates.
- Tests in the seeded layer (`handoff_schedule.rs`): walk/carve/place across a seam, conservation of
  drops; real threads in `threaded_world.rs`. `just test-threads`.
- **Exit:** a seam changes nothing observable for walking, building and digging.

### P5 — Persistence and protocol
- world-store migration `0004` (sqlite + pg), codecs, `PortableRegion`, `StoredRegion`,
  `TerrainSnapshot`, `stored_terrain.rs`, save cuts, `Dials`; ReadOnly handles; fence on every write.
- R2 roles: `just test-r2` with stud terrain; `LoadFailed` never regenerates.
- **Exit (M2):** edit, restart server, edits are back; parked regions restore; store bytes/region
  ≤ voxel baseline.

### P6 — Gameplay port (game)
- `StudTiles` + `TileSource` over studs; then port, each with its tests: `pathfind.rs`/`path_plan.rs`,
  `orders.rs` validation + dig/build/erase, `mining.rs` (hardness/drops from materials), `trees.rs`
  (felling by `data` tree id), `item_piles.rs`, `containers.rs` (chest = a part + material),
  `adventure.rs`, `people.rs`, `player.rs` respawn, `voxel_body.rs` → `stud_body.rs` (detached
  bricks as a dynamic body), `buoyancy.rs`.
- Movement constants become the stud values; `BodyScale` call sites collapse.
- **Exit:** dwarves path, dig, haul, build and chop in a stud fortress on the headless sim; every
  ported file's tests have a stud twin passing.

### P7 — Client near field
- Order: (a) `JobKind::StudMesh` + neighbour-across-region culling + delta updates (needed for M1);
  (b) `stud_pull` pipeline + shader + record lock test; (c) fortress uniform features;
  (d) `TileMirror`/picking/`block_target`/designation tools; (e) water; (f) stud tops in first
  person, transparent materials; (g) avatar/selection/cut sizes; (h) wasm (`just slint-wasm`,
  `just web`, `firefox-profile`).
- Session lifecycle: every new resource/`Local`/root registered or `SessionScoped`.
- **Exit:** frame time at the P0 scale within the agreed ratio of voxel (`just diag`,
  `gpu-measure`); `just pull-smoke` on a real adapter; fortress view complete.

### P8 — LOD and far terrain
- `lod.rs`/`lod_builder.rs`: `LodCell` becomes a height-prism grid from recipes (+ edited-chunk
  top-downs for built structures); `lod_workers.rs`; client `lod_rings.rs` payload; `set_lod_table`
  defaults for studs. Delete the `NullLodBuilder` special case.
- **Exit:** `world=full` view distance works on studs; LOD pop judged in a real look.

### P9 — UI and tools
- Slint: build palette (part, rotation, material) replacing the dev keys, tool wheel wording, level
  text in tiles, hotbar/inventory/item icons from materials and parts (`just slint-wasm`).
- Tools/examples: `cross_section`, `brick_census` → stud census, `snapshot_size`,
  `genesis_profile`, `collider_profile`, `world-map` landing units, `forge-ui` preview.

### P10 — Cutover and deletion
- Flip defaults: `just run/play/server/web` use stud terrain; remove `VOXEL_TERRAIN`,
  `WorldSource::Studs`, `STUDS_SUPPORTED`, the `chunk_blocks` dial.
- Delete: `voxel.rs` `Chunk`/`ChunkBlocks`, `block.rs` chisel/`ColliderCells`, `slope.rs`,
  `volume.rs`, `ground.rs` (voxel), `edit.rs` block paths, `halo.rs` (voxel), voxel `lod.rs`,
  `blocks.ron`/`registry.rs`/`block_props.rs`; client `bricks.rs`, `meshing/`, `voxel_pull/`,
  `far_voxel/`, `voxel_material.rs`, `voxel_bodies.rs`, voxel shaders; store `chunks` table
  (migration `0005`); voxel update kinds; ~60 game + ~14 worldgen voxel test files (each replaced by
  its stud twin in P2–P8 or deleted with its subject).
- Re-record once: `physics_goldens`, `wasm_determinism` digests, `scene_hash_golden`.
- Update `CLAUDE.md`, `justfile`, `TODO.md`; `just test-full`, `just test-threads`,
  `just determinism-wasm`, `just test-r2`; re-forge and redeploy the web world (D5) — plan first.
- **Exit (M4):** `grep -r "BLOCK_VOXELS\|ChiselData\|ChunkBricks"` is empty; all gates green.

## Review focus (what the phase plans must pin with tests)

1. **Purity of `pristine_chunk`** — a chunk derived on the server, on a client, and in a neighbour's
   halo must be byte-identical, or every unedited seam desyncs. (P2, wasm determinism too.)
2. **Carve remainders** — carving through a 2×8 brick or a `Filler` must leave canonical, overlap-free
   bricks and undo bit-exactly, including when the chunk was implied a tick earlier. (P1/P2)
3. **Deep and tall worlds** — digging 200 layers down or a 4 km mountain: implied ground answers
   without materialising; chunk-y far from 0 has no `u8`/f32 precision surprises (plate edges are
   computed from integers, never accumulated). (P2/P3)
4. **Store read errors** — a failed `stud_chunks` read must never fall back to "pristine"
   (that would silently erase a fortress): `LoadFailed`, retried. (P5)
5. **Tile/stud disagreement** — a half-carved tile, a lone plate on a floor, a slope: `standable`,
   the designation overlay and the physics capsule must agree, or dwarves path into walls. (P6)

## Risks

- **Cost**: brick count per region is the whole risk. D1 + `Filler` + delta wire are the mitigations;
  the P2 exit gate is where the plan is allowed to fail early.
- **Renderer scope**: `stud_pull` is a new GPU pipeline. The per-chunk mesh path is the fallback and
  is enough for M1–M2; do not block gameplay phases on it.
- **Hash churn**: every phase shifts state hashes. One deliberate golden re-record per phase, never
  ad hoc; the server's state-hash interval stays as is.
- **Concurrent work**: R3 (multi-host) and water plans touch `world_manager`, halo and the store;
  land P4/P5 between their phases, not across them.
- **LDraw**: only baked, attributed parts ship (`assets/studs/ATTRIBUTION.md`); LEGO names/logos stay
  out of the game (the stud top is a plain cylinder).
