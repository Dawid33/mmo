# 00 — Selection and construction UI for the voxel fortress: synthesis

Research 2026-10-29. Question (Dawid): *what UI systems apply to making selections in a 3D voxel
world, designating them for purposes, and building structures and blocks like Dwarf Fortress?*

Builds on the DF research (`../2026-10-20-dwarf-fortress-research/`): `12-ui-and-interaction.md`
(DF's own UI), `03-designations-and-jobs.md` (designation kinds, jobs) and `engine-mapping.md`
§8 (cutaway clip plane + cap, overlay texture, ghost previews, picking). Those decisions stand;
this set covers what other games and tools do and the techniques underneath.

- `01-colony-and-base-builders.md` — Going Medieval, Timberborn, Stonehearth, Gnomoria, Dungeon
  Keeper/WftO, RimWorld, Songs of Syx, Oxygen Not Included, Factorio, Satisfactory,
  Foundation/Manor Lords. Many UI details are `[mem]` (web sources thin); Going Medieval,
  Timberborn, Factorio, Satisfactory are sourced.
- `02-voxel-editors-and-building-tools.md` — WorldEdit, Axiom, Litematica, Create
  schematicannon, Building Gadgets, structure blocks, MagicaVoxel/Goxel, Space Engineers
  projectors, Hytale/Valheim/Townscaper. Litematica layer modes and Goxel's gesture are `[mem]`.
- `03-techniques-and-rendering.md` — 3D selection with a 2D mouse, picking under occlusion,
  selection data + sim event encoding, rendering overlays/ghosts, construction lifecycle under
  rollback, input conventions. Each section ends with a "for us" tied to code paths.

## 1. The answer in one picture

The UI is five systems, layered. Only the fourth touches the sim.

| # | system | what it is | where |
|---|---|---|---|
| 1 | **Working level (z-slice)** | an explicit, lockable current level; the cutaway shows it; *every* pick respects it | `engine-mapping` §8.1 cutaway + commander camera |
| 2 | **Picker** | ray → cut plane → voxel DDA; returns the hit cell + face *and* the level cell; tools choose which | client port of `game::edit::raycast_block_in` |
| 3 | **Selection builder** (client-local) | tool + shape gesture + CSG list of boxes, live preview, validity with reasons, size readout | `commander/selection.rs` grows into this |
| 4 | **Orders** (sim) | designations, zones, construction blueprints as persistent world objects with op ids, priorities, lifecycle states | `game` `#[rollback]` state, sim events |
| 5 | **Presentation** | per-cell overlay lookup in the voxel shader, dithered ghost voxels in the pulled-quad path, gizmo boxes, Slint toolbars/readouts/inspectors | `voxel_pull.wgsl`, `renderer/`, `crates/slint-ui` |

Everything below fills in each row.

## 2. Findings that decide the design

1. **The working level must be explicit and pick-binding.** Going Medieval, the closest 3D
   analogue, projected selection onto whatever was under the cursor. The result was its most
   persistent complaint: a dig dragged on level 4 lands on level 5, and clicks hit things behind
   the slice. It took a "Camera 2.0" rework (2025) to add a lockable layer with Shift+wheel
   stepping (01 §1). Timberborn adds the corollary: a hidden cut state confuses players
   ("my buildings vanished"), so the active slice must always be on screen (01 §2).
   → Corners lock to the *level floor plane*, never the terrain surface. Show the level
   permanently (depth bar plus a level number; DF has both).
2. **3D boxes are always two phases.** No game or editor does a 3D box in one drag (01 §13, 02
   patterns). The variants are: footprint drag then height drag (Stonehearth, Satisfactory
   vertical zoop, Goxel); footprint drag while the wheel changes level mid-drag (DF v50); or
   scroll/handle extrusion afterwards (Axiom). → **Decided (§6): slice-locked rectangle,
   Alt+wheel changes level while dragging**, which fits the cutaway since it is DF's own
   model and is the one gesture for every multi-level box (dig shafts, stairs, deconstruct a
   2 m structure). No separate Alt-drag height gesture. Later: face handles to edit an existing
   box. Always show a live `W × D × H` readout.
3. **Dig picks a cell; build picks the cell next to a face.** The picker returns both, plus the
   level cell (03 §2.3). That is one DDA walk starting at the cut plane, with hidden tiles solid.
4. **Selections are box lists with add/subtract**, Shift = add, Ctrl = subtract (WorldEdit,
   Axiom, ONI, Litematica multi-box). Nobody ships intersect. Flood fill
   ("magic select": dig this vein, chop this tree, Axiom's compare modes + limit + surface-only)
   is a separate tool, best done **in the sim** as seed + predicate + limit so it costs one
   small event (02 §2, 03 §3.3).
5. **Designations and blueprints are persistent world objects**, not commands. Across every
   game: tag → translucent ghost → job → built, and the ghost stays selectable to cancel,
   pause or re-prioritise. Missing edit-after-commit is a top complaint (Stonehearth rebuilds,
   Songs of Syx's one-click plan wipe; 01 §14).
6. **Undo works on orders, not on world time** (Factorio 2.0; 01 §9, 03 §5.2). Ctrl+Z removes
   the not-yet-started tiles of my last op id. Undoing a finished build is a new deconstruct
   order. This fits rollback netcode cleanly: no time travel in the UI.
7. **Validity is colour plus a reason**, aggregated: valid / hard-blocked / soft-blocked
   (Satisfactory blue/red/yellow), RimWorld-style reason text at the cursor. Over a box, show
   counts per reason ("14 blocked: not dug out, 3: no support"). Factorio's force-build idea
   (blockers become deconstruct orders) is worth a modifier later.
8. **Construction = workers filling a hologram.** Space Engineers projectors, the Create
   schematicannon and Litematica add up to the model (02 §3, §4, §9):
   - ghost blocks count as planned but unbuilt;
   - only cells adjacent to support are buildable now, with an option to highlight just those;
   - per-cell diff states: missing / wrong / done;
   - an overwrite policy: build into air only, or also dig what's in the way;
   - a bill of materials: needed / available / missing;
   - a one-line blocking reason per structure.
9. **Bulk tools need filters and painted priority** (ONI layer filters + a 1–9 priority brush,
   Factorio planner filters). Priority is a paint stroke, not a per-order dialog.
10. **Blueprints are clipboard + anchor + rotation/mirror**, picked from a thumbnail browser
    (Axiom, Hytale, Factorio). Marking cells "must be air" separately from "don't care"
    (structure void) lets a room blueprint carve its own interior, which matters for us because
    building underground means digging first.

## 3. Engine facts that constrain it

- **A chunk is 2×2×2 blocks** (`BLOCK_VOXELS = 16`, `CHUNK_BLOCKS = 2`,
  `crates/game/src/block.rs:15`). Designation storage and overlays must be indexed by block in
  their own sparse grid, e.g. 16×16×1 tile blocks (DF's layout), never per chunk.
- Sim designations: per-tile bytes in a `#[rollback]` tier-1 `UndoMap` (each tile write logs an
  exact inverse). The **event carries a capped box list + op id + priority + tool**, and the
  sim expands it in-tick, so the wire stays small and expansion is deterministic. The client
  keeps `Vec<(Op, IBox)>` and evaluates it to per-level bitsets only for preview/validity
  (03 §3.5).
- Rollback already gives pending / confirmed / rejected: a predicted designation is drawn
  dimmed until its tick confirms. Rejections arrive as emitted `GameDataUpdate`s → red flash +
  reason (03 §5.4).
- Rendering:
  - The overlay should be a **cell-indexed lookup** (3D or level-array R8 texture over the
    fortress box, ~4 MiB at 256×256×64) sampled in `voxel_pull.wgsl` from world position +
    face normal. One rule then covers floors, wall faces and levels below. This upgrades
    engine-mapping §8.3's per-level 2D texture.
  - Ghost voxel constructions: **a second record buffer in the pulled-quad path, dithered**
    (no alpha sorting, correct depth and cutaway clip). Ghost furniture: `BbModel` with a
    dithered material.
  - Drag boxes: Bevy `Gizmos`, two passes so hidden edges show dimmed.
  - Outline crates (`bevy_mod_outline`) don't fit: terrain has no `Mesh` assets, and the
    crate's docs list support only up to Bevy 0.16.
- Toolbars, priority selector, size/reason readouts, blueprint browser, inspectors: **Slint**
  (`crates/slint-ui`), per the 2026-10-20 UI rule. In-world overlays stay renderer work.

## 4. Input map (proposed)

| action | binding |
|---|---|
| confirm / drag | LMB |
| cancel drag → exit tool | RMB click (release < `DRAG_THRESHOLD_PX`; RMB *drag* stays orbit), Esc |
| add / subtract | Shift / Ctrl + LMB |
| level ±1 / ±10 | **Alt+wheel** / Alt+Shift+wheel (also PgUp/PgDn); **works mid-drag** — this is how a box spans levels |
| zoom | wheel (unchanged from today's commander camera) |
| layer lock | toggle (Going Medieval used Caps Lock); default *locked* |
| rotate placement | R / Shift+R, also Q/E |
| tool hotkeys | DF letters (d dig, h channel, u/j/i stairs, t chop, …), key shown on every button |
| priority | 1–9 while a tool is active (paints priority) |
| undo order | Ctrl+Z (my op ids, unstarted tiles) |
| pause | Space |

One client-side binding table feeds both the input systems and the Slint button labels.

## 5. Build order

1. **Working level + picker**: lockable `cut_y`, Alt+wheel stepping, level readout; `pick_cell`
   (DDA from the cut plane) next to `GroundPick`; hover-cell highlight. Needs the cutaway
   (`engine-mapping` §8.1).
2. **Rectangle designation tool** on the level, with multi-level via Alt+wheel mid-drag (required
   from the first version, §6.4), Shift/Ctrl
   CSG, size readout, RMB/Esc cancel; client-local preview with Gizmos.
3. **Designation sim state + event** (box list, op id, priority) and the **cell overlay** in
   `voxel_pull.wgsl` with pending dimming; dig first (dwarves/jobs consume it).
4. **Validity with reasons** (client pre-check, sim re-validates), Ctrl+Z on op ids,
   select-existing-designation → cancel / priority.
5. **Zones/stockpiles** reuse the same tool with area semantics (flat, per level, paint/erase).
6. **Construction ghosts**: wall/floor/ramp/stairs as ghost voxels in the second record buffer,
   material choice ("nearest" default), buildable-now highlight, bill of materials.
7. Later: magic select (sim-side flood), face handles, blueprints
   (copy/paste/rotate, air vs don't-care), priority brush, filters, force-build modifier, x-ray
   that changes pickability too (Timberborn's lesson).

## 6. Decisions (Dawid, 2026-10-29)

1. **Alt+wheel changes the working level**; the plain wheel stays zoom. Alt+wheel works while a
   drag is held, which is what makes a box span levels. Watch for: some Linux window managers
   bind Alt+drag (and a few bind Alt+wheel) to window actions; under a WM that eats it, PgUp/PgDn
   are the fallback, and the binding table (§4) keeps it rebindable.
2. **Walls hiding the floor: decided here, no occluder fade for the MVP.**
   - The cut sits at the **top of the working level** (floor + 1 m). Everything above it is
     clipped, so on the working level walls and furniture show as at most 1 m stubs. This is
     DF's "you see one level" and Going Medieval's half-height-walls view in one rule.
   - Box corners come from the level floor plane, not the first surface the ray hits. A wall
     stub in front of the floor therefore never steals the pick.
   - The drag box is drawn in two gizmo passes (hidden edges dimmed), so a selection behind a
     stub is still readable.
   - Units and `BbModel` furniture are not voxel-path geometry, so the voxel clip doesn't cut
     them. A 2 m character standing on the working level is drawn whole, its head poking above
     the cut. That is intended (you see who is there). Models whose *feet* are above the cut are
     hidden.
   - Revisit with a dither-fade of occluders near the cursor only if play shows floors being
     hard to read at steep pitch.
3. **Chisel / sub-block construction is out of scope.** Constructions are whole 1 m blocks
   (walls, floors, ramps, stairs) plus furniture models. The chisel data stays in the engine
   untouched.
4. **1 tile = 1 m; characters 1–2 m tall; housing structures generally 2 m (two levels), by
   race.** Consequences:
   - **Multi-level selection is MVP, not "soon after"** (moves it up from `12-ui…` §6). Removing
     a 2 m wall means designating both levels. Deconstruct, dig, channel and stairs all take the
     same multi-level box: drag on level N, Alt+wheel to N+1 (or more), release. The preview
     shows every affected level; the box readout shows `H` in levels.
   - **Room/house tools default to the race's height.** One race for now: **dwarves, 2 m
     (2 levels)** (Dawid, 2026-10-29) — a named constant `DWARF_HEIGHT_LEVELS = 2`, moving to a
     raws race table when a second race exists. A "build wall" footprint drag extrudes 2 levels
     without needing Alt+wheel; Alt+wheel during the drag overrides it.
   - **Furniture and doors span two levels.** A door or bed model belongs to one footprint cell
     but occupies the level above too. Picking a multi-level object from either level selects the
     whole object, and deconstructing it removes the whole object — selection resolves to objects
     first, tiles second (DF: buildings are picked before tiles).
   - **Deconstruction order is a sim concern.** A multi-level deconstruct must be scheduled
     top-down, and a tile is only workable from a standable neighbour, so a worker never removes
     the block under itself or under a block still waiting. This belongs to the job system
     (`03-designations-and-jobs.md`), not the UI. The UI shows "waiting: blocks above" as a
     validity reason.
   - **Headroom becomes a validity reason.** Placing a bed or workshop where a 2 m race can't
     stand shows "needs 2 levels of headroom". Pathing already has to know creature height
     (DF assumes 1 tile tall; we can't).
