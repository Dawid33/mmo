# Prefab system — design

Date: 2026-11-11. Status: **draft for approval** — nothing is built.

A prefab is a saved, named group of LEGO parts that can be put into the world as a unit. This
document designs the mechanism: the data, the files, the per-player library, the editor, and the
ways a prefab enters the world (stamped at runtime, placed by worldgen). It does not design any
content or any player-facing fortress feature that uses it.

Related documents:

- `2026-11-11-lego-tree-prefabs-design.md` (parked) — the first customer; its "What trees need from
  the prefab mechanism" list is treated here as requirements for the prop kind.
- `2026-11-11-minecraft-import-research.md` §7 — what an importer needs a grid prefab to hold.
- `2026-11-08-stud-chunks-design.md` — the brick world this sits on.

## Decisions (the user's)

- Prefabs are stored on disk, held in game memory, and can be stamped into the world.
- They are edited in an **in-game editor that launches an empty world**. The editor uses the
  **normal commander camera** and feels like **Minecraft creative mode**.
- In **debug mode** the editor can open a prefab that lives on disk in the game's own data; the
  prefabs used by worldgen systems are edited this way.
- The first prefab will be a **tree**, made by the future tree redesign.
- Stamping prefabs in a normal fortress game through the UI is a **future direction**. For now it is
  a technical capability only: save / load, edit / view, register / deregister, stamp, and placement
  by worldgen that merges into the world — the last one as a capability, not wired to any generator.
- In a running game prefabs work **like Factorio blueprints: per player, shareable, importable and
  exportable.** A per-world library is not wanted for a long time.
- **Both kinds from the start**: grid-brick prefabs merged into terrain, and off-grid assemblies
  living as instances (what the tree design needs).

Everything below is my proposal.

## Goals, non-goals, anti-goals

**Goals**

1. One prefab concept with two kinds, one file format, one library, one editor.
2. One layout routine per kind, shared by runtime stamping and worldgen, so the two cannot drift.
3. A file that survives table growth and is independent of chunk alignment.
4. Prop instances cheap enough for forests: a few bytes each in state, snapshot and save.
5. Everything in the sim meets the rollback bar and stays deterministic on native and wasm.

**Non-goals**

- Any fortress-mode UI for stamping; construction by workers; a bill of materials or item cost.
- The trees themselves, their roster, placement, footing rule, felling yield.
- A per-world or shared server-side library.
- The Minecraft importer (the format leaves room for it).
- Thumbnails, mirroring, water inside prefabs, binding markers to game objects.
- The editor and the on-disk library in the browser build (the sim side stays platform-neutral).
- Undo of brick edits in the editor (none exists in the game today).

**Anti-goals** (it has failed if any of these is true, even if it works)

- Worldgen and runtime stamping produce different bricks from the same prefab, anchor and turn.
- A forest of prop trees makes join snapshots or region saves grow per tree by more than the instance.
- A prefab file stores raw table indices or chunk-local coordinates.
- A registered or imported prefab can crash, stall or desync a region (it is untrusted input).

## The two kinds

| | **Build** | **Prop** |
|---|---|---|
| Made of | Grid parts from `STUD_PARTS`, quarter turns about Y | Any baked LDraw part, free rigid transform per part |
| In the world | Becomes ordinary `StudBrick`s in terrain chunks; the prefab is forgotten | One instance with an id; never bricks |
| Colour | Per part | Per part: a fixed colour or one of the prefab's colour slots, filled per instance |
| Collision | The bricks' own | Compound of baked convex hulls, per part, solid parts only |
| Removed | Brick by brick | Whole |
| Typical use | Buildings, ruins, walls, player blueprints | Trees, bushes, dressing, furniture later |

## Data model (`crates/game`, engine-agnostic)

Two forms of the same thing:

- **File form** — what is on disk and in an export string. Refers to parts by **LDraw number** and to
  natural materials by **name**, so a file survives table growth.
- **Resolved form** — what the sim, the wire and worldgen use. Table indices, validated, capped.
  Resolving fails with a list of problems (unknown part, over a cap, part outside bounds) and never
  panics.

```text
Prefab        { format: u16, name, kind, meta: { author, note, source } }

Build         { parts:   [{ min: [i32;3] cells, part, rot: 0..4, material, data: u16 }],
                boxes:   [{ min, size, material, data: u16 }]   // solid fills, tiled into bricks at stamp time
                carve:   [{ min, size }]          // cleared before anything is laid
                markers: [{ at: [i32;3], kind: String, turn }] }   // stored and shown, not interpreted

Prop          { parts:   [{ part, pos: [i32;3] in LDU, rot: unit quaternion, colour: Fixed(code) | Slot(n),
                            solid: bool }],
                slots:   [{ name, default: colour code }],          // e.g. foliage, trunk
                foot:    [u8;2] studs,                              // footprint on the grid
                markers: [...] }
```

- The prefab frame's origin is the **anchor**: the cell (Build) or grid point (Prop) that lands on
  the stamp position. Build cells may be negative (cellars, roots).
- A prefab is turned by a quarter turn about Y at placement (four orientations), both kinds.
- **Parts versus boxes.** A part is one brick the author chose (its seams and studs matter). A box
  is a volume of one material whose tiling does not matter; it is tiled with `fill_box` when stamped,
  so it is independent of chunk alignment and large volumes stay small in the file. This is the
  "boxes at rest, bricks at stamp" the Minecraft import research asks for.
- **`data`** is `StudBrick::data` (today: the tree id felling keys on). Only built-in prefabs may
  carry a non-zero value; resolving a player's prefab forces it to 0, so a stamped brick can never
  pose as part of a tree.
- Caps are named constants, checked at resolve time. Starting values (my call): 4096 parts and 4096
  boxes; Build extent 96 x 270 x 96 cells; Prop solid extent within 16 m of the anchor (bounds the
  seam halo); 8 colour slots.
- A prefab's **key** is the `StateHash` of its resolved form (`u32`).

## Files, the built-in set and the player library

| Where | What | Who writes it |
|---|---|---|
| `assets/prefabs/<name>.prefab.ron` | The game's own prefabs (worldgen and later gameplay) | The editor in debug mode |
| `<data_dir>/prefabs/<name>.prefab.ron` | The player's library (`player_name::data_dir`) | The editor, import |
| Export string | `MMOPF1:` + base64(lz4(bincode(file form))) | Export, to the clipboard |

- RON is the file format (already used for game data, diffs well in git).
- **Built-in set**: `assets/prefabs/` is embedded into server and client at compile time and
  installed into a process global in `game` (`install_prefabs`, the `install_block_props` pattern), so
  `game` and `worldgen` read it without Bevy. Native builds prefer the on-disk copy, as `blocks.ron`
  does; it carries the same "client and server must agree" condition as `blocks.ron`.
- A built-in prefab is referred to from sim state by the crc32 of its name; a test asserts no two
  names collide.
- **Import** decodes a string under a size limit before decompressing, resolves it, and only then
  writes it to the library. A failed import reports the problem list.

## Per-player registration (the Factorio-blueprint part)

- The durable library is the player's own files. The game world never stores a library.
- To use a prefab in a running game the client **registers** it: an order carrying the resolved
  prefab. The server validates it again and stores it in that player's `Client` state, keyed by
  prefab key. **Deregister** removes it.
- Registered prefabs are ordinary rollback state: hashed, part of the join snapshot, and they
  **travel** with the player across seams (`Client::arrive` classifies the new field as travelling).
- Caps (my call): 8 registered prefabs and 8192 parts-plus-boxes in total per player, about 100 KB of
  state at worst. Region snapshots are already large (`TODO.md`), so this budget is deliberately
  small. Each definition is held behind a shared pointer with its hash computed once, so hashing and
  seam crossings do not re-walk it.
- Intended lifetime is the session: the client re-registers what it wants after connecting. Nothing
  relies on the registered set surviving a reconnect.
- Sharing between players is by export string outside the game. No in-game transfer.

## Stamping a Build prefab

One routine, `layout_build(prefab, anchor: world cell, turn) -> per-chunk { carve boxes, bricks }`:

1. Turn and translate every part, box and carve box into world cells; tile each box with `fill_box`
   per chunk.
2. Cut at chunk edges. A box-shaped part that straddles an edge is re-tiled across it with
   `fill_box` (same cells, same material). A non-box part (a slope) that straddles an edge cannot
   exist in the brick world and is **skipped and counted**.
3. Per chunk: apply the carve boxes, then place the bricks. A brick that overlaps something already
   there is **skipped and counted** (what `feature_chunks` does today). The result carries both
   counts, so a caller can treat a non-zero count as a refusal if it wants all-or-nothing.

**Runtime**: a new order `StampPrefab { key, anchor, turn }`, valid only for a key the player has
registered (or a built-in name key, for debug). The region that owns the anchor runs the layout,
applies its own chunks directly (one whole-chunk update per chunk, not per brick) and sends the rest
to neighbours as a new batched intent (`chunk` + carve boxes + bricks), applied at T+1 like every
intent. The neighbour does not need the prefab registered.

`StampPrefab` is a **developer-grade order**, in the same class as `CarveStud` / `FillStud`: no item
cost, no survival rules, no permission gate beyond those orders'. A brick that would overlap a body
is skipped and counted like any other overlap. There is no reply path from neighbours: if a
neighbour region is not running, its part of the stamp is not laid (intents never wake a region) and
the stamp is partial. That is accepted for this order. The future fortress feature is expected to be
blueprints built by workers from a ghost, which is a different mechanism on top of the same layout
routine.

**Worldgen**: `worldgen` gets `stamp_build_into_seed(seed, region, prefab, anchor, turn)`, which runs
the same layout and keeps only this region's chunks, writing carve boxes and bricks into
`StudRegionSeed` (which gains a carve list applied before features). A generator that places
prefabs must be a pure function of world coordinates and must also evaluate placements anchored in
neighbouring regions within the extent cap; then a prefab on a region edge comes out whole on both
sides. Nothing calls this yet.

Changing a built-in prefab changes the pristine chunks of existing worlds, exactly as changing tree
code does today.

## Prop prefabs

### Parts and bake

- A second list, `tools/ldraw-bake/prop_parts.txt`, with per-part flags (`solid`, `concave`).
  `just ldraw-bake` additionally emits:
  - `crates/game/src/prop_catalog.rs` — `PROP_PARTS` (append-only): number, name, bounding box,
    collision hulls in integer LDU, and a mass box.
  - `assets/studs/props.bin` — hi and lo meshes with per-triangle colour class, as `parts.bin` has.
- Hulls: the convex hull of **all** the part's vertices (the current bbox-plane method is wrong for
  organic moulds), simplified to a point cap; a part flagged `concave` gets a convex decomposition
  instead. Both run in the bake tool only; the sim sees integer data. Re-baking is a physics change.
- LDraw shortcut assemblies (`2518c01`) are not baked as one part. `just prefab-import-ldr <file>
  <name>` converts a `.ldr` or shortcut `.dat` into a Prop prefab file, one entry per sub-part with
  its matrix, so authored LDraw assemblies and LEGO's own joint tilts come in directly.
- Grid bricks can be used in a prop by listing them in `prop_parts.txt` too.

### Instances in the sim

- Region state gains `props: BTreeMap<PropId, PropInstance>`, rollback state, hashed.
  `PropInstance { prefab: Builtin(name key) | Local(prefab key), at: region-local grid point, turn,
  colours }`.
- `Local` definitions live in a small per-region table, reference-counted by the instances that use
  them and saved with the region. This is not a library: it exists only so a prop placed by a player
  outlives that player's session.
- `PropId` is unique in the world and stable: worldgen ids derive from the placement, runtime ids
  from the region's serial. Orders address a prop by id.
- **Pristine baseline**: `StudRegionSeed` gains `props`. The store saves only the difference from the
  seed (ids removed, instances added), in `RegionMeta`. An untouched forest costs nothing to save.
- **Colliders**: per instance, a compound of the solid parts' hulls on a fixed body, built from the
  catalog when the region loads. In the join snapshot they are sent as placeholders and rebuilt by
  the receiver, the way voxel shapes already are.
- **Seams**: an instance belongs to the region of its anchor. Instances whose solid extent reaches
  across a seam are mirrored to the neighbour through the halo (collision only), following removals.
  The 16 m cap bounds the mirror depth.
- **Tiles**: solid parts also block the tile grid, so workers path around a trunk instead of walking
  into its collider. Each instance marks the tiles its solid hulls' bounds cover; this is a derived
  index (rebuilt from `props` on load and on every add / remove, not separate hashed state), read by
  `tile_fill` and pathfinding next to bricks, and mirrored with the halo. It is a coarse blocker for
  path search only — collision and picking still use the hulls. Tree-specific order checks (what
  counts as choppable) stay with the tree work.
- Non-solid parts never collide, never block tiles, and may interpenetrate anything.
- Props and bricks do not check each other for overlap in v1 (a prop can be placed into a wall).
  Footing and spacing are the caller's rule.

**Runtime**: `StampPrefab` on a Prop prefab adds an instance (copying a registered definition into
the region table); a new order `RemoveProp { id }` deletes one.
**Worldgen**: `stamp_prop_into_seed(seed, prefab name, at, turn, colours, id)` appends to
`StudRegionSeed::props`.

### Becoming a falling body

`detach_prop_safe(id, pose, velocity)` removes the instance and spawns one dynamic entity with the
same compound (non-solid parts included, as the tree design asks). Mass is explicit, summed from the
parts' axis-aligned mass boxes, reusing the falling-brick-body code path (no eigen solve).
`ColliderSpec` gains a variant naming the prefab, so the body can cross seams and be saved, and is
rebuilt from the catalog on arrival. What triggers it, what it yields and how it settles belong to
the tree work.

### Drawing and picking

- The client builds one mesh per (prefab, colour set, level of detail), cached; each instance is one
  Bevy entity sharing that mesh and the existing stud material, so identical trees batch. Colour
  sets are few by design. Mesh building is a new `JobKind` so it works in Web Workers.
- Before this is accepted, measure a dense forest region on a real adapter; if entity count is the
  cost, merge instances per chunk instead. That choice is left to the builder, with the measurement.
- Picking: a ray test against instance bounds, then part hulls, in `game` (shared by the cursor and
  by order validation).

## The editor

**The world**: a new world source `prefab-editor` beside `studs` — flat baseplate, no store, nothing
persisted, no creatures. The anchor is marked on the baseplate.

**Getting in**

- Main menu → **Prefab editor** (native builds) → a library screen: the player's prefabs (name, kind,
  size, part count), with New (Build or Prop), Open, Duplicate, Delete, Import string, Export string.
- In debug builds (`dev_ui`) the screen has a second list, **Game prefabs**, showing
  `assets/prefabs/` on disk; opening one edits that file in place.
- `client debug prefab [--game] [<name>]` and `just prefab-editor [args]` go straight into the
  editor on a named prefab (or a new one).

**Editing a Build prefab** — creative mode as it exists today: the stud build mode is on, same keys,
same palette, no cost. Opening a prefab stamps it at the anchor with the runtime stamp order. **Save**
writes every brick that is not part of the pristine baseplate, relative to the anchor: a brick of a
plastic colour is written as a part exactly as placed; bricks of natural materials (soil, stone, …)
are merged into boxes. Carve boxes and markers already in a file are kept and shown; authoring them
is later work.

The editor always stamps at the same anchor with no turn, so the bricks it reads back sit exactly
where the file put them: **opening a prefab and saving it unchanged writes an identical file.**
(Stamping elsewhere in a game may split box-shaped parts at chunk edges; that result is never saved
back.) Saving a prefab that exceeds a cap is refused with the count.

**Editing a Prop prefab** — the prefab is a document on the client; the world shows it as one prop
instance at the anchor, replaced on each edit. Tools: add a part from a prop palette (dropped where
the cursor hits the prop or the baseplate, snapped to studs and plates), select a part, move it by
stud / plate or by fine steps (1 LDU), turn it about any axis in quarter turns or fine steps (1°),
set its colour or slot, toggle solid, delete. Slots are named and given defaults in a side panel,
with a preview of other colours. The document has its own undo stack.

**Leaving**: unsaved changes prompt. The editor world and its server are torn down like any session
(every new resource is session-registered).

## Build order (for the plan)

Each step is usable on its own; later steps do not change earlier formats.

1. Prefab types, resolve / validate, RON files, export strings, built-in set install.
2. Build layout; `stamp_build_into_seed`; the `StampPrefab` order and batched intent.
3. Per-player registration; the on-disk library; import / export.
4. The editor world, debug preset, `just` recipe, menu entry, library screen; Build editing.
5. Prop bake (list, hulls, meshes, catalog); `.ldr` import.
6. Prop instances in the sim: state, colliders, halo, snapshot placeholders, store diff, seed props.
7. Prop drawing and picking.
8. Prop editing tools.
9. Prop to falling body.

Step 2 onward edits `orders.rs`, `intents.rs`, `stud_ops.rs` and `state.rs`, which currently carry
someone's uncommitted survival-placement work. Step 1 is all new files and can start first; the rest
waits for that work to be committed.

After step 9 the tree design is revisited against what exists.

## Checks the plan must include

- Rollback bar (`hash(before) == hash(after undo)`) for register, deregister, stamp (both kinds),
  remove prop, detach prop.
- Layout equality: the same prefab, anchor and turn gives the same bricks through the runtime path
  and the seed path, including across a chunk edge and a region edge.
- `just test-threads` for stamps and prop halo across a seam.
- `just determinism-wasm` and the physics goldens after the hull bake and the prop collider work.
- Round trips: file ↔ resolved ↔ export string; editor open then save, unchanged, gives a
  byte-identical file.
- Hostile input: oversized, truncated and nonsense export strings and register orders are refused.

## Left to the builder

- Exact Rust names, module split, and the RON layout within the shapes above.
- The hull simplification method and point cap; the decomposition library used by the bake tool.
- `PropId` encoding.
- Instanced entities versus per-chunk merged meshes for props, by measurement.
- The Slint layout of the library screen and the prop tool panel (Slint only, `fortress` theme).
- Cap values, as named constants.
