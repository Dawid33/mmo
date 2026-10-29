# 03 — Interaction techniques, picking, selection data and rendering

Part of the 2026-10-29 voxel selection/construction UI research; start at `00-synthesis.md`.
This file is the technique and implementation layer: HCI literature plus graphics and game-dev
practice, mapped onto this engine. It builds on decisions already made in
`docs/superpowers/specs/2026-10-20-dwarf-fortress-research/engine-mapping.md` §8 (cutaway via clip
plane + back-face cap + slice texture, per-level overlay texture, client-local ghosts, picking =
ray vs cut plane then walk down open tiles) and `12-ui-and-interaction.md`, and does not redo them.

Tags: **[paper]** peer-reviewed or published research, **[docs]** official docs / wiki / crate
docs, **[blog]** practitioner write-ups, **[code]** read in this repo, **[mem]** from memory,
unverified in this session — treat as plausible, check before relying on it.

Engine facts used throughout ([code]): a block is 1 m = 16³ voxels (`BLOCK_VOXELS = 16`,
`crates/game/src/block.rs:15`), and a 32³-voxel chunk is therefore only **2×2×2 blocks**
(`CHUNK_BLOCKS = 2`). Anything per-tile should be indexed by block, never by chunk. The sim
already has a deterministic block-scale Amanatides–Woo walk (`raycast_block_in`,
`crates/game/src/edit.rs:189`, fixed x→y→z tie order) that returns the hit block and the entry
face normal. The commander's `GroundPick` ray-marches the client `HeightGrid` in 0.5 m steps with
bisection (`march_height`, `crates/client/src/renderer/commander/selection.rs:84`), which only
knows column tops — fine for the surface, wrong for a cutaway. The camera is a 55° default pitch
(15–89° allowed), 40° FOV, wheel = zoom (`commander/mod.rs:74-98`).

---

## 1. Mouse-driven 3D volume selection

### 1.1 The core problem

A mouse gives two degrees of freedom; a cuboid of tiles needs six (two corners × xyz). Every
technique fixes the missing dimension by **constraint** (lock it to a plane or level),
**mode** (a second gesture for the third axis) or **inference** (derive it from scene content).
Argelaguet & Andujar's survey classifies selection techniques by selection tool (ray vs volume),
degrees of freedom, disambiguation mechanism and control-display ratio, and identifies
occlusion and dense targets as the dominant error sources [paper]. Their survey is about
object selection in VR, but two findings carry over: (a) a ray from the eye (image-plane
selection) beats a ray from a hand when targets sit at varying depths, which on a desktop means
"pick what is under the cursor pixel" is the right primitive; (b) disambiguation should be a
separate, explicit step (scroll through candidates, switch level) rather than a precision
requirement on the pointer [paper].

### 1.2 Techniques, with pros/cons for a tilted RTS camera

| technique | how the 3rd dimension is set | pros | cons | seen in |
|---|---|---|---|---|
| **Slice-locked rectangle** | both corners on the current z-level; depth = 1 level | trivial to learn, zero ambiguity, matches cutaway rendering | multi-level work needs repetition | DF classic + v50, Going Medieval, Timberborn [docs][mem] |
| **Change z mid-drag** | press corner A on level z₁, wheel/key to z₂, release corner B → cuboid | one gesture, no extra mode; the view always shows the level being edited | user loses sight of corner A (it's above the cut or below); needs a ghost of the full box | DF v50 ("selections also work across z-levels … cuboid") [docs] |
| **Two-step: rect then height drag** | drag footprint, release, then vertical mouse motion sets height, click to confirm | full cuboid in one tool, works at any camera angle | modal (a state the user must notice); vertical mouse → height ratio is ambiguous under perspective | Sims/city builders for walls & pools, many level editors [mem] |
| **Scroll-to-extrude** | footprint drag, then wheel adds/removes layers while held | reuses an idle input; incremental, reversible | collides with wheel = z-level / zoom; discoverability | Minecraft builder mods (WorldEdit-style selection expand), Axiom [mem] |
| **Plane-locked drag then extrude handle** | drag on a constraint plane, then drag a face handle along its normal | precise, CAD-familiar (SketchUp push/pull) | gizmo hit targets get tiny when zoomed out; heavy for a game | SketchUp, Blender, Unity ProBuilder [mem] |
| **Face/edge handles (gizmo) on an existing box** | after creation, drag any of 6 face handles | the best *editing* tool for a box after the fact | needs handle picking + occlusion of handles | WorldEdit CUI, Axiom, Unity BoxCollider editor [mem] |
| **Inferred/flood** | click a seed; region grows over connected matching tiles (same material, same z, ≤ N) | one click for "this vein", "this room" | unpredictable extent; needs a limit and a preview | DF "dig vein" [mem], paint-bucket tools |
| **Volume lasso / structure-aware** | 2D lasso, depth inferred from data density | great for unstructured data | not needed for a block grid with levels | CloudLasso (Yu et al. 2012) for particle clouds [paper] |
| **Bimanual / anchored stretch** | one point anchors, another stretches the volume | low fatigue in VR/touch | needs two input points | Balloon Selection (Benko & Feiner 2007) [paper] |

Notes:

- **Lock the drag to the level's floor plane, not to terrain.** If corner B is picked on the
  terrain surface (as `GroundPick` does today), dragging across a cliff makes the box jump in y
  and in xz, because the ray hits a different surface. Intersecting the cursor ray with the
  horizontal plane `y = level_floor` gives a stable, monotonic xz under the cursor no matter
  what's there — that's the standard construction-plane trick of every level editor [mem].
- **Perspective + tilt make height drags ambiguous.** At 55° pitch, a vertical screen motion
  corresponds both to "further away on the ground" and "higher". Rect-then-height tools solve
  it by projecting the mouse onto a *vertical* plane through the footprint's near edge, facing
  the camera (yaw-aligned) — then screen-up is world-up [mem]. Snap the result to whole levels.
- **Multi-level selection must show the whole cuboid** while the cut hides part of it: draw the
  box wireframe without depth test (or dimmed where occluded, §4.2), and a per-level count
  readout, e.g. `12 × 8 × 3 (levels −4…−2) = 288 tiles`.
- **Click-vs-drag threshold** is already 4 px (`DRAG_THRESHOLD_PX`, `selection.rs:25`); DF v50
  and RimWorld also accept "click A, click B" instead of drag, which is gentler on long boxes
  and allows panning/scrolling between the clicks [mem]. Support both: press-drag-release, and
  click-move-click (the second when the press didn't move).

### 1.3 For us

Make the **slice-locked rectangle with z-change mid-drag** the primary tool (it is exactly DF
v50's model, and the cutaway already gives one level at a time): corner A = ray ∩ floor plane of
the current level, the wheel keeps stepping the cut while the button is held (or between
clicks), corner B = ray ∩ the *new* level's floor plane, selection = the cuboid between. Add a
**height-drag modifier** only for tools where vertical extent is the point (walls, stairs,
up/down shafts): Alt-drag after the footprint, projected onto the yaw-aligned vertical plane.
Box face handles are a later polish for editing stockpiles/zones. Replace `march_height` for
designation picking with the plane intersection (§2); keep it for camera height and the
"surface here" jump. All of this is client-local (as `selection.rs`'s module doc says: nothing
reaches the sim until confirmation).

---

## 2. Picking under occlusion

### 2.1 Ray–voxel traversal

Amanatides & Woo (1987) is the standard: per axis keep `t_max` (distance to the next boundary)
and `t_delta` (distance per cell), step the smallest; O(cells crossed), exact, returns the entry
face for free [paper]. Robustness notes from practice [blog][mem]: handle `d = 0` with `∞`
(the sim's version does), use a fixed tie order (it does: x, y, z), and start the walk at the
ray's entry into the region/fortress box rather than at the camera to save steps and avoid
float drift over long `t`. The sim's `raycast_block_in` is the right algorithm; the client needs
its own copy against the client tile view (the pick must not be a sim call, it runs every frame).

### 2.2 Picking with a cut plane

With the cutaway, everything above the cut is invisible, so the pick must ignore it too —
otherwise the user clicks a visible floor and selects an invisible roof. Algorithm (engine
mapping §8.5, made concrete):

1. Ray ∩ plane `y = cut_y` (top of the current level). If the ray starts above, advance to that
   point; it is the first visible location.
2. DDA from there; the first solid cell hit is the target. Hidden (unrevealed) cells are solid
   "unknown" and stop the walk.
3. If nothing is hit within the box, fall back to the floor-plane intersection (empty space
   designations, e.g. building a floor over a chasm).

### 2.3 Cell picking vs face picking

The DDA returns `(cell, normal)`. Tools differ in which cell they mean, as Minecraft made
universal (break = hit cell, place = hit cell + normal) [mem]:

| tool | target |
|---|---|
| dig, channel, smooth, chop, remove-construction | the **hit cell** (solid) |
| build wall/floor/furniture, stockpile, zone | the **adjacent open cell** (`cell + normal`), or for top-down work, the open cell at the current level |
| stairs / ramps | hit cell on the current level, or the open column for "up stair" |

DF's tile model adds a subtlety: a "floor" belongs to the bottom of the tile at level *z*, so
"the tile at the current level" is the open cell at *z* whose floor is the top of the solid
cell at *z − 1* [docs][mem]. Pick in **level space**: for slice-locked tools, the target is
always the cell on the current level under the cursor — the plane intersection with
`y = level_floor + ε` — and the DDA only decides *visibility* (is something on this level in
front?). This is what makes DF-style designation predictable: the cursor never "falls" to a
lower level unless the user scrolls.

### 2.4 Occluders at the current level

With the cut at the top of level *z*, occluders are only the level-*z* walls themselves, at most
1 m tall. At 55° pitch a 1 m wall hides ~0.7 m of floor behind it; at 30° ~1.7 m [derived].
Options:

- **Cut lower** (at mid-level, so walls render as stubs): the Sims "walls cutaway" / "walls
  down" modes show cut walls as stubs with an outlined top [mem]; DF effectively shows walls as
  full tiles from above, which the cap does.
- **Cursor-local fade**: occluders within a radius of the cursor ray become dithered
  (screen-door) transparent. Many games (Divinity, Baldur's Gate 3, The Sims 4 "cutaway near
  cursor") fade or cut out geometry between the camera and the focus [mem]. Dither is preferred
  over alpha because it stays in the opaque pass and needs no sorting.
- **X-ray silhouettes**: draw selected/designated or occluded units through walls with a second
  pass using `depth_compare = Greater` and a flat colour [blog][mem].

For a 1-level cutaway, occlusion is mild; picking by level plane (§2.3) already makes hidden
floor tiles selectable even when they can't be seen, so drawing the selection box/overlay
**over** the wall (§4) is enough at first.

### 2.5 Snapping

Snap everything to whole blocks and whole levels (positions `floor(p / 16)` in voxel units).
Rotations snap to 90°. Sub-block (chisel) designations are out of scope for DF-style tools; if
ever needed, do them as a separate "detail" tool snapping to the 1/16 grid. Snap the screen
cursor to the hovered cell's centre for the cursor highlight only — never move the OS cursor.

### 2.6 For us

Write a client `pick_cell(ray, cut_y, level, tiles) -> PickHit { cell, normal, level_cell }`
next to `GroundPick` in `crates/client/src/renderer/commander/selection.rs`, as a port of
`raycast_block_in` (`crates/game/src/edit.rs:189`) against the bridge's block data, starting at
the cut-plane intersection. Return both the DDA hit and the level-plane cell; tools choose.
Units/buildings stay picked first via the existing projected-position `click_pick`. Keep
`march_height` for the camera and "surface here".

---

## 3. Selection set representation and operations

### 3.1 Representations

| repr | memory | ops | good for |
|---|---|---|---|
| **list of boxes** (+ op per box) | O(#boxes), tiny | add/subtract = append; queries O(#boxes) | the wire format; the client's editable selection |
| **per-level bitset / byte grid** over the fortress box | 256×256 levels at 1 bit = 8 KiB/level | CSG = bitwise and/or/andnot, exact, O(area) | rendering, validity per tile, sim state |
| **sparse set** (`BTreeSet`/hash of cells) | O(cells) × 12–16 B | simple; slow CSG | small irregular sets (flood results) |
| **sparse blocks** (DF's 16×16 map blocks with per-tile bitfields) | only touched blocks allocated | local, cache-friendly | sim storage of designations |
| **octree / SVO** | compact for big uniform volumes | CSG is recursive, complex | overkill at fortress scale |

DF stores designations as **per-tile bitfields inside each 16×16×1 map block** (dig type,
priority, hidden, traffic, marker/blueprint flag, smooth, etc.), with a per-block "has
designations" flag so the job scanner skips empty blocks [docs][mem] (DFHack's
`df::tile_designation` and `tile_occupancy` structs expose this layout [mem]).

### 3.2 CSG while building a selection

Shift = add, Ctrl = subtract are the cross-genre default (Photoshop marquee, RTS selection,
city builders) [mem]; Ctrl+Shift = intersect (Photoshop) is rarely used in games. Keep the
editable selection as an ordered box list with `Add|Subtract` per entry and evaluate it to a
bitset only for rendering and validity. That keeps undo of a sub-step trivial (pop the last
box) and keeps the wire format a box list. Erasing designations is "subtract" applied to the
*designation* layer, which DF exposes as a separate "remove designation" tool [docs].

### 3.3 Flood / connected selection

Connected selection = BFS over 6- (or 4-, per level) neighbours with a predicate (same
material, solid, revealed) and **hard limits**: max cells (e.g. 4,096), bounded to the
fortress box and to ±N levels. Show the result as a preview before confirming; flood is
unpredictable without one. If the flood is evaluated in the sim (seed + predicate + limit as
the event) it is deterministic and tiny on the wire, but its cost lands inside a tick and its
result depends on the state at the applied tick (which is actually correct semantics — "the
vein as it is when the order lands"). If the client floods and sends cells, the sim must
re-validate each cell. Prefer sim-side for veins (deterministic, bounded), client-side for
pure selection convenience.

### 3.4 Compact serialization as a sim event

- **Box list**: `(min: [i16;3], max: [i16;3])` region-local = 12 bytes per box, plus tool,
  priority, op. A typical designation is 1–10 boxes. Through `encode_packet` (bincode + lz4
  above the threshold) this is negligible [code: CLAUDE.md conventions].
- **Irregular sets** (after subtracts, flood results sent from the client): decompose the
  bitset into boxes with a **greedy rectangle/box merge** — the same greedy idea as the
  voxel mesher (merge runs along x, then rows along z, then levels) — giving an exact box
  cover, usually within a small factor of optimal (minimum rectilinear cover is NP-hard in
  general [mem]). Alternative: per-level row RLE of the bitset over the bounding box; simpler,
  worse for big rectangles. Cap event size (e.g. ≤ 256 boxes) and refuse/split beyond.
- **Never send the expanded per-tile list.** The sim expands boxes against the tile map at
  the applied tick: invalid tiles (already open air for "dig", hidden, out of box) are skipped
  deterministically.

### 3.5 For us

Client: `Vec<(Op, IBox)>` editable selection → evaluated `LevelMask` (per-level `u64` row
bitsets over the fortress box) for preview/validity. Sim: designations live in `GameData` as
per-tile bytes in sparse 16×16×1 blocks (DF layout; the 2×2×2-block chunk is too small to be
the unit), under the `#[rollback]` tier-1 `UndoMap` so each tile write logs an exact inverse
(`crates/macros`). The event carries a box list; expansion is in-tick. Box decomposition can
borrow the run-merging loop shape from `renderer/meshing/` (binary greedy mesher), but must
live in `game` if the sim ever produces covers.

---

## 4. Rendering selections, designations and ghosts

### 4.1 What to draw where

| thing | technique | why |
|---|---|---|
| in-progress drag box | line box (12 edges) + per-tile tint of the evaluated mask on the current level | lines are cheap and show extent across hidden levels |
| per-tile validity during placement | the overlay texture with a "pending/invalid" code per tile | exact per tile, free once the overlay exists |
| committed designations | per-level overlay texture (decided, engine-mapping §8.3) with a **pattern atlas**: hatch = dig, stripes = channel, X = stairs, dots = chop | patterns read in greyscale and for colour-blind players; DF uses glyphs per tile [docs] |
| designations on vertical faces (cliffs, walls) | same overlay indexed by **cell**, sampled in the voxel fragment shader at `floor(world_pos − normal·ε)` | a cell-indexed 3D lookup works on every face, unlike a top-down 2D texture |
| below-cut designations | the same lookup on visible lower levels, darkened/faded with depth like the terrain | reuses the depth shading of the cutaway |
| ghost constructions | ghost voxels (tinted, dithered) or ghost bbmodels | see §4.3 |
| job markers, status icons | camera-facing billboards over units/tiles | decided in §8.3 |
| dimensions + reason readout | screen-space label at the cursor (Slint overlay) | UI rule: player UI is Slint |

### 4.2 Outlines of a voxel set

- **Geometric edge extraction**: for a set of cells, an edge is drawn where the *boundary
  faces* of the set meet non-coplanarly. Per level, for a 2D mask this is marching-squares on
  tile corners: emit a segment wherever an in-set tile borders an out-of-set tile; merge
  collinear runs. In 3D, emit the 12 edges of each boundary face, cancel duplicates, and drop
  edges whose two incident boundary faces are coplanar. CPU, O(boundary), rebuilt only when
  the mask changes; output is a line list.
- **Screen-space**: render the set (or selected meshes) into a mask/ID target, then either a
  Sobel/neighbour-compare pass (1–3 px outlines) or a **jump flood** (JFA, Rong & Tan 2006)
  for wide outlines with cost logarithmic in width [paper]. Ben Golus's "Quest for Very Wide
  Outlines" compares brute force, inverted hull and JFA and concludes JFA wins for wide
  outlines [blog]; Rerun's renderer follows it [docs].
- **Occluded parts**: draw outline lines twice — normal depth test at full alpha, then
  `depth_compare = Greater` at low alpha (a "hidden line" style) — so a multi-level box shows
  through the cap.
- **Bevy**: `Gizmos` (immediate-mode lines, `GizmoConfig` with `depth_bias`, line width,
  perspective lines) is fine for a few thousand segments per frame and already used in
  `renderer/collider_debug.rs` and `renderer/grid_debug.rs` [code][docs]; remember Bevy's
  `bevy_gizmos_render` feature split (memory note). `bevy_mod_outline` does vertex extrusion +
  jump flood but its docs list support up to Bevy 0.16 (0.10.x) as of this check; `bevy_jfa`
  is older [docs]. Neither applies to terrain anyway: the near path draws storage-buffer quad
  records with no `Mesh` assets (`renderer/voxel_pull/`), so terrain outlines must be done in
  that shader or as gizmo lines.

### 4.3 Translucent ghosts and their sorting problem

Classic alpha blending needs back-to-front order; a grid of ghost blocks has internal faces,
mutual intersections and interleaves with water [mem]. Options, cheapest first:

1. **Screen-door dither** in the opaque pass: discard fragments by a 4×4 Bayer threshold at
   ~50%, tinted green/red. No sorting, depth-correct, shadows optional; TAA would smooth it,
   without TAA it reads as a deliberate stipple [blog][mem]. Animated dither threshold reads
   as "not real yet".
2. **Merged ghost mesh with internal faces culled** (greedy-meshed like terrain) + a depth
   prepass of the ghost then colour with `depth == equal`: one translucent layer, correct
   self-occlusion, sorted only as one object [mem].
3. **Order-independent transparency**: Bevy ships an `OrderIndependentTransparencySettings`
   camera component (linked-list/layered OIT) since 0.15 [mem]; heavier, unnecessary here.
4. **Hatching/stripe pattern in world space** (diagonal stripes on ghost faces), opaque pass.

### 4.4 Decals vs per-tile overlay

Projected decals (Bevy has forward/clustered decals in recent versions [mem]) are for
arbitrary placement; they smear over chisel shapes and need per-decal culling. A per-cell
overlay texture sampled in the voxel shader is **exact per tile, one texture fetch, no extra
geometry**, and stepping a level is a texture upload — strictly better for a tile grid. Use
decals only for non-grid marks (e.g. a rally flag).

### 4.5 Validity colouring with reasons

Validity is per tile *and* per reason: green = ok, yellow = ok but warning (e.g. no path for
workers yet), red = refused. Render the class in the overlay; show the **reason histogram**
next to the cursor ("12 ok · 3 blocked by wall · 1 needs stone floor"), as DF v50 shows
"Blocked" / "Needs soil" per placement [docs], and RimWorld shows a reason text on an invalid
placement [mem]. Compute validity client-side with the same predicate code the sim uses (a
pure `fn can_designate(tool, tile_view, cell) -> Result<(), Reason>` in `game`), so preview and
sim never disagree except through state changes.

### 4.6 For us

Extend the overlay design of engine-mapping §8.3 from "per-level 2D texture" to a **cell-indexed
lookup** (an R8 3D texture or a level-array over the fortress box; 256×256×64 levels = 4 MiB at
one byte) sampled in `assets/shaders/voxel_pull.wgsl` from world position and face normal — this
covers floors, wall faces and lower levels with one rule. Byte = `(class: 2 bits | pattern: 4
bits | pending: 1 | invalid: 1)`. The drag box and multi-level extents go through `Gizmos`
(two passes for hidden lines). Ghost voxel constructions should be **a second record buffer in
the pulled-quad path** with a ghost flag → Bayer dither + tint in the opaque phase, meshed by the
existing mesher (`renderer/meshing/`), so they get correct depth, AO and cutaway clipping for
free; ghost furniture uses `BbModel` with a dithered material. Dimensions/reason readout is a
Slint overlay label (`renderer/slint_overlay.rs`).

---

## 5. Construction semantics UX

### 5.1 Lifecycle and how each state looks

| state | meaning | look |
|---|---|---|
| **planned** (designated, blueprint) | exists in sim, no job yet / suspended | dithered ghost, pattern overlay; DF v50 "blueprint mode" designations exist but do nothing until activated [docs][mem] |
| **queued** | job created, waiting for a worker | ghost + priority number |
| **reserved** | worker assigned, materials reserved | ghost + small material icon; reserved items marked in stocks |
| **in progress** | partially built | progress: ghost fills bottom-up, or scaffold model, plus a progress bar on hover; RimWorld shows a frame with a progress bar [mem]; Factorio shows a ghost until the robot arrives [mem] |
| **done** | real voxels/entity | normal rendering |
| **suspended/blocked** | can't proceed (no material, unreachable) | ghost with a warning icon + reason |

### 5.2 Cancel vs undo

- **Cancel a designation** (not started) = a sim event removing it; nothing is refunded.
- **Cancel in progress** = job abandoned, reserved and partially used materials dropped as items
  (DF drops construction materials on removal [mem]).
- **Undo of a completed build** is *not* a time undo; it is a new "deconstruct" designation
  that a worker carries out. Rollback netcode undo is invisible to players and must never be
  exposed as gameplay undo.
- **Client Ctrl+Z** = undo of *my last designation operation*: tag each designation event with
  `(player, op_seq)`, store the op id per tile; undo sends "remove tiles still carrying op X
  that haven't started". Redo re-sends the boxes. This keeps undo correct when other players
  or jobs touched the same tiles.

### 5.3 Priorities and overlaps

DF has priorities 1–7 per designation (4 default) [docs][mem]. Overlap policy: **paint
semantics, latest wins per tile** (a new dig designation overwrites an older one of a different
type on the same tile) is what DF and most builders do [mem]; tiles already in progress are
skipped (report them as "in progress" in the reason histogram). Between players, deterministic
order = event order in the sim (tick, then the sim's event order), so all clients agree.

### 5.4 Optimistic display with a deterministic, rolled-back sim

The game already predicts: player input becomes a tick-stamped sim event applied in the
client's predicted ticks and re-simulated on rollback [code: CLAUDE.md, `input-slot` memory].
So a designation is naturally:

- **pending** = applied in predicted state, its tick not yet confirmed by the server;
- **confirmed** = present at the confirmed tick;
- **rejected** = the server's confirmed state doesn't contain it (validation failed at the
  applied tick, or a conflicting designation landed first) — rollback removes it automatically.

Display pending tiles dimmed (the overlay's `pending` bit), set from a client list of in-flight
op ids cleared on confirmation. On rejection, flash the affected tiles red for ~1 s and post a
toast with the reason (the sim emits an undo-logged feedback event, as the chat/commands
design does for command feedback). Never maintain a separate "optimistic overlay" that the sim
doesn't know about beyond the brief drag preview — the prediction is the optimistic layer.

### 5.5 For us

States live in sim designation/job data (`game`), visual state is derived; renderer reads
`state` per tile into the overlay byte and per-construction into the ghost buffer. Op ids
(`player, seq`) in the designation event enable Ctrl+Z without a time undo. Rejection feedback
rides `GameDataUpdate` emits like other undo-emitted state (`crates/macros` `#[emit]`).

---

## 6. Keyboard/mouse conventions

| action | genre convention | notes |
|---|---|---|
| select / confirm | LMB | click-click and drag both |
| cancel current tool / drag | **RMB** or **Esc** | RimWorld, Factorio, DF v50, most RTS [mem][docs]; in-drag RMB aborts only the drag, a second RMB exits the tool |
| add to selection | **Shift**+LMB | already implemented (`selection.rs:235`) |
| subtract | **Ctrl**+LMB (Photoshop/Windows) | some RTSs use Ctrl for control groups — keep Ctrl+digit separate |
| alternate/height mode | Alt+drag | rarely default in games; safe |
| rotate placement | **R** (city builders, Factorio, Sims), **Q/E** (RimWorld) [docs] | support both R (cw) / Shift+R (ccw) and Q/E |
| z-level | **wheel** (DF v50 changes elevation; Shift+wheel 10 levels [docs]); `<`/`>` or PgUp/PgDn keys | Going Medieval uses Ctrl+wheel for level [blog] |
| zoom | Ctrl+wheel in DF v50 ([docs], `12-ui…` §2); wheel in most RTSs | genre split — make it a setting |
| pan | WASD/arrows, edge, MMB-drag | exists (`commander/mod.rs`) |
| orbit | RMB-drag here; MMB/Alt in others | **conflicts with RMB-cancel**: resolve by click-vs-drag (RMB release < 4 px = cancel, otherwise orbit), as many RTS/city builders do [mem] |
| tool palette | letter hotkeys mirroring the menu (DF classic `d`→`d` dig, `h` channel, `u/j/i` stairs, `t` chop), number keys for sub-options | show the key on each button |
| radial menu | hold a key/MMB → pie of tools | Fitts-friendly, good for gamepad [paper: pie menus, Callahan et al. 1988, mem]; optional |
| repeat last tool | Space or a dedicated key | DF uses Space for pause — avoid |
| priority | number keys while a tool is active, or `+`/`-` | DF v50 shows a priority selector in the tool bar [mem] |
| pause | Space | DF |

### 6.1 For us

Wheel is currently zoom (`commander/mod.rs`, `zoom_target`); engine-mapping §8.6 already
decided wheel = z-level and Ctrl+wheel = zoom, DF style. Make it a setting from day one because
RTS players expect the opposite, and add Shift+wheel = ±10 levels. RMB is currently orbit-drag;
add release-without-motion = cancel (reuse `DRAG_THRESHOLD_PX`). Ctrl+LMB = subtract. Keep the
whole binding table in one client-side map so Slint buttons can show the active key.

---

## Sources

- Argelaguet, F., Andujar, C. (2013). A survey of 3D object selection techniques for virtual
  environments. *Computers & Graphics* 37(3):121–136, doi:10.1016/j.cag.2012.12.003 —
  https://hal-univ-rennes1.archives-ouvertes.fr/hal-00907787
- Amanatides, J., Woo, A. (1987). A Fast Voxel Traversal Algorithm for Ray Tracing.
  Eurographics '87 [mem: citation] — implemented in `crates/game/src/edit.rs`.
- Yu, L., Efstathiou, K., Isenberg, P., Isenberg, T. (2012). Efficient Structure-Aware Selection
  Techniques for 3D Point Cloud Visualizations with 2DOF Input (CloudLasso). IEEE TVCG 18(12) —
  https://ieeevis.org/year/2012/paper/vis/efficient-structure-aware-selection-techniques-3d-point-cloud-visualizations-2do ,
  https://tobias.isenberg.cc/personal/papers/Yu_2012_ESA_Slides.pdf
- Benko, H., Feiner, S. (2007). Balloon Selection: A Multi-Finger Technique for Accurate
  Low-Fatigue 3D Selection. IEEE 3DUI, doi:10.1109/3DUI.2007.340778 —
  https://xrcollaboratory.github.io/edu.cornell.xrc.interaction.balloonselection
- Rong, G., Tan, T.-S. (2006). Jump Flooding in GPU with Applications to Voronoi Diagram and
  Distance Transform. I3D — https://www.comp.nus.edu.sg/~tants/jfa/i3d06.pdf
- Golus, B. (2020). The Quest for Very Wide Outlines — https://bgolus.medium.com/the-quest-for-very-wide-outlines-ba82ed442cd9
- Rerun renderer outlines — https://ref.rerun.io/docs/rust/head/re_renderer/draw_phases/outlines/index.html
- bevy_mod_outline — https://docs.rs/crate/bevy_mod_outline/latest ,
  https://raw.githubusercontent.com/komadori/bevy_mod_outline/master/README.md
- bevy_jfa — https://github.com/dataphract/bevy_jfa
- Dwarf Fortress wiki: Designations — https://dwarffortresswiki.org/index.php/Designate ;
  Keys — https://dwarffortresswiki.org/index.php/Keys ; Controls —
  https://dwarffortresswiki.org/index.php/Controls ; 50.01 release info —
  https://dwarffortresswiki.org/index.php/Release_information/50.01
- DF bug 12993 (z-level step while designating) — https://dwarffortressbugtracker.com/view.php?id=12993
- RimWorld controls — https://rimworldwiki.com/wiki/Controls
- Going Medieval level switching (Steam discussion) —
  https://steamcommunity.com/app/1029780/discussions/0/3092263995638507724/
- Repo: `crates/client/src/renderer/commander/selection.rs`, `commander/mod.rs`,
  `commander/height.rs`, `crates/game/src/edit.rs`, `crates/game/src/block.rs`,
  `renderer/collider_debug.rs`, `renderer/grid_debug.rs`, `assets/shaders/voxel_pull.wgsl`.
