# World editing (break / place blocks): investigation

Date: 2026-09-22. Status: v1 IMPLEMENTED the same day (see "What was built").

## What was built (v1)

Left click breaks the targeted block, right click places dirt
(`edit::PLACE_BLOCK`) against the face it hit, reach `edit::REACH` = 5 m.
Code: `crates/game/src/edit.rs`; tests: `crates/game/tests/block_edit.rs`.

It differs from the proposal below in one deliberate way. **The sim
raycasts, the client sends nothing new.** Mouse buttons already reach the
sim as tick-stamped `PlayerInput`, so `InputState` now tracks them like keys
(press edge survives a sub-tick tap). The region tick runs the edit after
the controllers and before the input step, from body + `PLAYER_EYE_OFFSET`
along `look_quat(yaw, pitch)`. That needs no new event, packet or routing,
and the ray is deterministic (`+ - * /`, `floor`, libm-forced quaternion).
The render camera's rotation snaps to the sim look, so the crosshair and
the sim ray agree. The cost is the trade-off the named-target design
avoided: if client and server disagree on the pose at that tick, the edit
mispredicts and is corrected by rollback like any other input.

As proposed:
- `set_block_safe` = chunk `set_safe` + `SetVoxelComponent` both ways +
  colliders swapped in place (`Undo::<ColliderSet>::mutate_collider_safe`,
  using new `ColliderSet::is_marked_modified` / `pop_modified` in the rapier
  fork). Spare colliders are disabled, missing ones attached; a genesis
  cuboid is reused with its offset cleared.
- Placing into a chunk slot with no entity creates it via `create_mesh`
  and sends its position + voxels (the bridge now snaps an entity spawned in
  the same drain to its first pose instead of gliding from the origin).
- Placing into any moving body's AABB is refused; nothing happens while the
  inventory is open or the cursor is released.
- Edits roll back bit-exact under `HashMode::Full` and replay identically,
  on partial chunks, genesis cuboids and newly created chunks.

Not done: cross-region edits (a ray that leaves the home region finds
nothing), hotbar-driven block choice, a target highlight, durable
persistence, and a coords → chunk index (the edit scans chunk bodies,
fine per click, too slow for a per-frame highlight). A new collider joins
the broad phase at the next physics step, so for one tick the character
controller can miss a just-placed block.

## Where things stand

Nothing edits terrain at runtime. `durable-world-persistence-design.md` says so
("No terrain-edit event exists… tests edit via `with_data`"), and the
block-abstraction spec (`2026-07-06-block-abstraction-data-model-design.md`
§ milestones) already sketches the chain this document fills in:
input → targeting → block edit → re-derive / re-mesh + collider update → new
`GameEventKind`.

Most of the downstream plumbing already reacts to a changed chunk. The work is
the event, one undoable sim mutation, and a collider inverse.

### Already there

| Piece | Where |
|---|---|
| Whole-block write (clears chisel) | `Chunk::set_block`, `game/src/voxel.rs` |
| Voxels derived from blocks + chisel | `derive_voxels`, `collider_cells` in `game/src/block.rs` |
| Undoable component write + slot digest | `Component::set_safe`, `game/src/state.rs` |
| Render events on undo | `emit_on_undo` |
| Client applies `SetVoxelComponent`, beats a late snapshot | `client/src/renderer/bridge.rs` `drain_region_updates` |
| Remesh with neighbour / region-seam re-dirty | `meshing/queue.rs` `mark_remesh_dirty` (world-keyed grid, 26 boundary-zone hashes) |
| LOD rings refresh on terrain change | `terrain_fingerprint` every `TERRAIN_HASH_INTERVAL_TICKS` → `TerrainChanged` → `invalidate_region_lod` |
| Edits survive parking (in memory) | `SerializedRegion` is the whole `Rollback` |
| Tick-stamped predicted events | `ClientPacket::GameEventAfterTick`, `UiAction` is the template |
| Mouse buttons captured and sent | `client/src/renderer/input.rs` |
| Editable voxels collider | parry fork `Voxels::set_voxel`; rapier `Collider::set_shape` / `shape_mut` |
| Eye position for view rays | `movement::PLAYER_EYE_OFFSET` (added with the 1.8 m body, 2026-09-22) |
| Crosshair | `renderer/hud.rs` |

### Missing

1. **An edit event.** `InputState::update` drops `MouseButton` / `MouseWheel`
   (`game/src/input.rs`). There is no hotbar slot and no edit `GameEventKind`.
2. **Targeting.** No sim-side raycast or voxel DDA. The client DDA in
   `renderer/far_voxel/dda.rs` is for far-LOD rendering.
3. **Chunk lookup.** Chunk entities don't store `ChunkCoords`; code recovers them
   from body translations by linear scan (`surface_voxel_y`, `terrain_snapshot`).
   There is no coords → `EntityKey` index and no global block-coordinate type.
4. **An undoable `set_block_safe`.** Nothing in `game` emits
   `SetVoxelComponent` today (only a client test does). `create_mesh` doesn't
   emit it either, so a chunk created at runtime would render nothing.
5. **A collider-edit inverse.** `ColliderSet` lacks `is_marked_modified` /
   `pop_modified` (only `RigidBodySet` has them). Genesis collapses fully solid
   chunks to one cuboid, which an edit must turn back into voxel shapes.
   Removing a collider has no surgical inverse (whole-`PhysicsState` snapshot).
6. **Cross-region edits.** Input routes only to the player's home region. A
   target across a seam (reach is ~5 m, so this happens at every seam) needs
   routing to the target region.
7. **Block highlight, durable persistence, edit tests.**

## Recommended design

### Event: the client names the target, the sim validates it

```rust
GameEventKind::BlockEdit(ClientId, BlockEdit)
struct BlockEdit { block: WorldBlock /* region + region-local block xyz */, action: Break | Place { id: BlockId, face: Face } }
```

The client raycasts from its rendered eye and sends the block it hit. The sim
checks the target instead of re-deriving it from yaw/pitch:

- the target lies within `REACH` (a named constant, 5 m) of the eye
  (body translation + `PLAYER_EYE_OFFSET`);
- Break: the block is solid. Place: the target cell is air, its `face`
  neighbour is solid, and the cell does not overlap any body's capsule;
- otherwise the event is a no-op (still logged, so client and server agree).

Why not raycast in the sim: the client's camera look is interpolated per frame
while the sim's yaw/pitch are per tick, so a sim ray can pick a different block
from the one the crosshair showed. A named target is also cheap to validate and
keeps float ray maths out of the determinism-critical path. Minecraft does the
same. The trade-off is that a cheating client can pick any block in reach
through walls; add a line-of-sight check later if that matters.

The event rides the existing `GameEventAfterTick` path, predicted locally, so
the block disappears on click without waiting for the server.

### Sim mutation: `set_block_safe(chunk_key, block_index, Block)`

One transaction, three parts, each with an exact inverse:

1. **Chunk data**: `ecs.chunk.set_safe(key, Some(edited))`. A whole-chunk
   clone per edit is small (8 blocks + sparse chisel), and `set_safe` already
   has a slot digest.
2. **Render**: send `SetVoxelComponent(key, Some(derive_voxels(new)))`, and
   `emit_on_undo` the old voxels.
3. **Collider**: rebuild this chunk's voxel shapes from `collider_cells` (at
   most 2³ block cells plus chisel, cheap) and `set_shape` on the existing
   collider. The inverse is `set_shape(old)` plus un-marking modified. That
   needs a `ColliderSet::pop_modified` / `is_marked_modified` in the rapier
   fork, mirroring the `RigidBodySet` pair. Prefer `set_shape` over
   `Voxels::set_voxel`: `shape_mut` copy-on-writes a shared `Arc` anyway, and
   a rebuilt shape is trivially bit-identical to what genesis would build.
   The same path handles a genesis cuboid chunk (replace cuboid with voxels).

Edge cases:
- **Empty chunk after a break**: keep the chunk entity and give it an empty
  shape, so the inverse never has to re-insert a collider.
- **Place into a chunk slot with no entity** (air above terrain): needs
  `create_mesh` plus a render emit. Simplest v1: genesis creates entities for
  every chunk in the loaded column range, or `set_block_safe` creates one via
  `create_mesh` and fixes its missing `SetVoxelComponent`.
- **Chisel**: `set_block` drops chisel detail, which is the right Break/Place
  semantics at block scale. Sub-block editing is a later feature.

Also add a `BTreeMap<ChunkCoords, EntityKey>` index in `GameData` (undo-logged
`UndoMap`, maintained by `create_mesh` / genesis / removal) so an edit finds
its chunk without scanning.

### Cross-region

v1: the `WorldManager` routes `BlockEdit` by the target block's region, not by
`homes`, and the client predicts it into that region's local sim (the client
already runs the near regions). Until then, reject edits whose target is
outside the home region; it's one check and keeps seams consistent.

### Persistence

Parking keeps edits in memory; a server restart loses them. That is the
durable-persistence spec's job and doesn't block a first playable version.

## Build order

Each step is testable on its own with `just test-one game …`.

1. `ColliderSet::is_marked_modified` / `pop_modified` in the rapier fork, with a
   `set_shape` round-trip test in the `hash_restore.rs` style
   (`hash(before) == hash(after undo)`).
2. Chunk-coords index + `set_block_safe` + rollback tests: break, place,
   cuboid chunk, emptied chunk; seeded random edits in `random_ops.rs`.
3. `BlockEdit` event with validation, handled in `Region::handle_event`;
   a test that a walking body falls into a broken hole and is stopped by a
   placed block.
4. Client: left/right mouse → raycast (reuse the far-LOD DDA shape over near
   chunks) → `BlockEdit`; wireframe highlight of the target block.
5. Client/server convergence harness test of an edit, plus a check that the LOD
   ring over the edit refreshes.
6. Cross-region routing.
