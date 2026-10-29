# 02 — Selection and construction in voxel editors and building tools

Part of the 2026-10-29 voxel selection/construction UI research (start at `00-synthesis.md`).
Question: how do the best 3D voxel *editing* tools let a mouse pick 3D volumes, show what will
change, show ghosts/blueprints and verify them against the world, and account for materials —
and what of that carries over to a top-down/tilted-camera Dwarf Fortress–style game where the
player designates and AI workers build.

Source tags: **[docs]** official docs/wiki, **[mod page]** Modrinth/CurseForge/README,
**[guide]** third-party guide or article, **[mem]** own knowledge, not re-verified this session
(treat as likely but check before relying on a detail). URLs under **Sources**.

---

## 1. WorldEdit (Minecraft, server plugin / mod)

**Selection primitives.** `//sel <mode>` switches shape: `cuboid` (default, two corners),
`extend` (left-click first point, every right-click grows the box to include the new point),
`poly` (2D polygon of right-clicked points; vertical span = lowest..highest clicked Y),
`ellipsoid`, `sphere`, `cyl` (centre then radii; height again from clicked Y range), `convex`
(convex hull of all clicked points; also the ordered control points for `//curve`) [docs WE
selections]. Cuboids are always world-axis aligned [docs].

**Picking points / depth.** First-person aiming: the wand (`//wand`, wooden axe) — left-click
pos1, right-click pos2 on the block you hit; `//pos1`/`//pos2` = your feet; `//hpos1`/`//hpos2`
= ray-cast to the block you look at, for far-away corners; `//chunk` selects whole chunks
(full height) [docs]. Depth is therefore resolved *by the terrain hit*: you can only click a
corner that is on a visible surface, and an interior/air corner is reached by standing there or
by editing the box afterwards.

**Editing the selection numerically** — this is WorldEdit's real strength:
`//expand <n> [dir]` (N/S/E/W/U/D, relative F/B/L/R, `me` = look direction, `vert` = whole
column, two numbers = both ways, comma lists), `//contract`, `//inset`/`//outset` (`-h`/`-v` for
horizontal/vertical only), `//trim [mask]` (shrink to the blocks matching a mask), `//shift`
(move the box, not the blocks) [docs; `//shift` mem]. `//size` reports dimensions/volume,
`//count <mask>` and `//distr` give block counts/distribution of the selection [docs] — i.e. a
built-in bill of materials of an *existing* region.

**Operations.** `//set`, `//replace [mask] <pattern>`, `//walls` (hollow shell, no floor/ceiling),
`//faces`/`//outline`, `//hollow [thickness]`, `//line`/`//curve` (thick, `-h` hollow),
`//stack <count> [dir]` (repeat the selection, direction defaults to facing; `-m` source mask),
`//move <n> [dir] [fill]` with `-s` to carry the selection along [docs regionops]. Patterns
(`50%stone,50%cobble`) and **masks** (`#existing`, `!air`, `>stone` = "above stone",
`<`, `%50`, `//gmask` global mask) are a small expression language that filters every operation
and brush [mem]. Brushes (`//brush sphere`, `cyl`, `smooth`, …) apply an operation at the
ray-cast hit from range [mem].

**Clipboard / schematics.** `//copy` stores blocks relative to *where you stand* (the origin is
the player's offset) and `//paste` re-applies that offset; `-a` skip air, `-s` select the
pasted region, `-o` paste at original position; `//rotate`, `//flip`; `//schem save/load` [mem].

**Visual feedback.** Only via a client mod: WorldEdit CUI draws outlines + grid lines for every
selection shape; without it, `//drawsel` gives a server-side outline for cuboids ≤ 48³ in
creative only [docs]. There is no preview of an operation's result — you run it and undo.

**Undo.** Per-player session history, default last 15 actions (configurable), `//undo [n]
[player]`, `//redo`, `/clearhistory`; kept 10 min after logout; only *direct* changes are
recorded (flowing liquid, popped attachments are not undoable) [docs history].

**Feel.** Command precision is the praised part ("granularity and control that is much harder
via mouse") [guide sportskeeda]; the clunky part is that every corner is a first-person aim and
there is no live preview, so non-trivial work is "do, look, undo" [mem].

## 2. Axiom (Minecraft, modern in-game editor mod)

Two modes: **Builder Mode** (vanilla creative + tools in a 10th hotbar slot) and **Editor Mode**
(Right Shift: a Blender-like UI — top menu bar, bottom bar with context keybind hints + the
active selection + flight-speed slider, dockable/themable windows: Tools, Tool Options, Palette,
History, Clipboard, Blueprint Browser, Tool Masks, Keybinds, Views …) [docs editor intro]. The
editor combines "camera controls from conventional 3D software" with Minecraft tools; the
camera detaches from the player and the cursor is free [docs; mem for orbit/fly details].

**Selection is a first-class object.** "A set of highlighted blocks" that *constrains* brush
tools and defines what operations touch; Enter / Select→Clear clears it; Ctrl+F fill, Ctrl+R
replace, Ctrl+C/X copy/cut, Ctrl+J duplicate (cut leaving originals), "Move Selection" moves the
selection volume without its blocks [docs selections]. Selections can be saved/loaded (5.1.0)
[mod page changelog]. Tools:

- **Box Select** — click-drag between two points makes a cuboid; then refine by dragging **three
  gizmos**; or type two coordinates; or Pos1/Pos2 "Set" buttons; and uniquely an **"unconfirm"**
  button to reopen a confirmed box for adjustment [docs box select].
- **Magic Select** — click a block: flood-fills connected blocks of the same kind. Options:
  Compare = Block / BlockState / Solid / Any(non-air); Limit (default 100 000); Range (can jump
  air gaps, e.g. a bushy tree's leaves); Surface Only (only blocks adjacent to air); Up / Down
  (only at/above or at/below the clicked block's level — a free height clamp); Horizontal;
  Corners (diagonal adjacency). Drag across several block types to include them all [docs magic
  select].
- **Lasso Select** — hold right mouse, draw a closed screen-space loop; on release it is "cast to
  the blocks within your view" with a **Depth** slider for how far it extends; option to include
  non-solid blocks [docs lasso]. This is the screen-space → volume primitive (added later, see
  changelog) [mod page].
- **Freehand Select** — paint the selection with a sphere/cube/octahedron brush stroke; "rather
  basic" but good for odd shapes [docs].

**Gizmo.** Axis arrows (1-axis drag), plane nodes (2-axis), centre node (3-axis, keeps camera
distance), rotation rings, scale nodes; global vs local frame; **Ctrl** = coarse snap (1 block,
15°), **Shift** = snapping off; arrow keys nudge on X/Z [docs gizmos]. With Move Selection
active, scale nodes on the arrow tips resize the selection [docs selections]. Cut/paste,
blueprint placement and the path tool all reuse the same gizmo.

**Builder tools** (Builder Mode, alt+scroll to switch): Move, Clone, Stack, Smear, Extrude, Erase,
Setup Symmetry. Selection there is corner-based: left-click / right-click corners,
**middle-click selects a *face*** (grows the box to the clicked face — useful where there are no
edges to snap to); offset and size are shown above the hotbar [docs builder intro]. Stack: scroll
wheel nudges the repeat count one step with a **hologram preview**, direction from facing
(arrows drawn on the box faces), hold X/Y/Z to lock axis, middle-click to "stack to here",
right-click confirms [docs stack].

**Blueprints.** Clipboard → Ctrl+P → name/authors/tags + rotate for a thumbnail → `.bp` file;
Blueprint Browser: thumbnail grid, tag filters, recursive search, nested folders; clicking loads
into the clipboard, then place with the gizmo; server-shared blueprints [docs blueprints].
Ctrl+R / Ctrl+F rotate/flip, Enter confirms [docs editor intro].

**Masks.** A Tool Masks window filters which target blocks any tool may change; "Apply Tool
Mask" also applies to placement (5.1.0) [docs tools intro; mod page]. **Undo**: Ctrl+Z / Ctrl+Y
plus a History window [docs].

**Feel.** Users credit cursor-based selection + Magic Select for speed and the Blender-like UI as
the main draw; WorldEdit stays preferred for rigid city/structure work by some [guide]. [mem]:
Axiom's big win is that *every* operation previews before Enter.

## 3. Litematica (Minecraft, client-side schematic mod)

**Tool item** (default a stick; `M+T` toggles it) with modes cycled by **Ctrl+scroll**: Area
Selection, Schematic Placement, Fill, Replace Block, Paste Schematic, Grid Paste, Move, Delete,
Rebuild [guide; list partly mem]. Ctrl+M cycles a mode's sub-option (e.g. paste replace
behaviour) [guide].

**Area selection.** Left/right click set or move corners, middle-click picks a box or corner to
edit [guide]; **Alt+scroll nudges** the selected thing in the look direction [docs FAQ]. [mem]:
"Simple" mode = one box; "Normal" mode = one selection made of several named **sub-region
boxes** (an L-shaped house = 2 boxes), each savable into one schematic. Corners rendered as
coloured cubes with the box outline; a "Missing chunks" warning if the box extends into
unloaded terrain [docs FAQ].

**Placements.** A loaded schematic can have several placements; select by middle-click in world
or from the list (white outline), configure position/rotation/mirror in the placement menu
(Numpad −), move origin with the tool [docs FAQ]. Sub-region placements can be toggled/moved
individually [mem].

**Ghost rendering.** The schematic is drawn as a separate "schematic world" in translucent
blocks over the real one; `M+G` toggles ghost blocks, `M+R` all rendering;
`renderCollidingSchematicBlocks` draws ghosts even where another block occupies the cell
[docs FAQ]. **Mismatch overlay** colours per cell — config names: missing, extra, wrong block,
wrong block state [guide wiki]; [mem] colours: missing = light blue, extra = magenta, wrong block
= red, wrong state = orange.

**Render layers — the z-slice feature.** Render Layers menu: ALL, SINGLE_LAYER, ALL_BELOW,
ALL_ABOVE, LAYER_RANGE, on a chosen axis (X/Y/Z) [mem; screenshots of All Below / Single Layer on
the mod page]; **PgUp/PgDn move the layer, Shift/M+PgUp/PgDn cycle the mode** [guide]. Building
"layer by layer from the bottom" with All Below is the canonical workflow, and the material list
and verifier can be restricted to the rendered layers [mem; a Litematica add-on page advertises
per-layer material lists and highlighting missing/wrong blocks].

**Material list** (`M+L`): Total / Missing / Available columns, i.e. required, still unplaced,
and in your inventory [mod page]; can be pinned as a HUD [mem]. **Schematic Verifier** (`M+V`)
lists every mismatch by category and lets you highlight entries in-world [mod page; docs FAQ].
**Easy Place** [mem]: clicking a ghost places exactly that block in the right state regardless
of what you aim at; the community "Litematica Printer" fork auto-places nearby ghosts — an
automated-builder analogue.

**Feel.** Loved for survival megabuilds; layer modes + material list + verifier are what make a
large blueprint tractable. Clunky: menu-deep configuration and many chorded hotkeys [mem].

## 4. Create — Schematic & Quill + Schematicannon (automated builder)

Closest analogue to "workers build the blueprint". [mem] **Schematic and Quill**: click two
corners, Ctrl+scroll pulls the hovered face of the box in/out, then name and save; the
**Schematic Table** writes it onto an empty schematic item; holding the schematic gives
deploy tools (Deploy, Move, Move Y, Rotate, Flip, Print-in-creative) switched by Ctrl+scroll,
with a translucent preview.

**Schematicannon** [docs Printing]: placed near but *outside* the schematic bounds; needs the
schematic, **gunpowder fuel** (hover shows how many blocks it fuels), and adjacent inventories
holding the materials. A **Book in the material-list slot prints the required blocks**; run it
again to refresh. You don't need all materials up front — the cannon **stalls on the first
missing block** and resumes automatically when it appears.
- Replacement policy (4 buttons, least→most destructive): Don't Replace Solid; Replace Solid with
  Solid; **Replace Solid with Any (default)**; Replace Solid with Empty (clears everything inside
  the bounds, including where the schematic is air).
- **Skip Missing Blocks**: don't pause, leave gaps — doubles as a *filter* and lets several
  cannons split one schematic by material type.
- **Protect Tile Entities** (don't overwrite chests etc.).
- Status line: Idle / Invalid / Expired (red), Ready / Out of Gunpowder / **Missing Block** /
  Paused / Block not loaded (yellow), Running (green). Replaced blocks give no drops.

[mem]: blocks fly to their target as visible projectiles; the print order sweeps the bounds, so
progress is legible. The UX lesson: a *construction job* has explicit policies (overwrite,
skip-missing, protect) and a one-line *blocked reason*.

## 5. Building Gadgets / Effortless Building (Minecraft quick-build mods)

**Effortless Building** [mod page]: hold Alt for a radial menu of build modes — Single, Line,
Wall, Floor, Cube, Diagonal Line, Diagonal Wall, Slope Floor, Circle, Cylinder, Sphere. Shape
modes are **two clicks**: first fixes the start, then aim elsewhere and click again. [mem]:
Line works along the axis you are pointing, Wall locks X or Z, Floor locks Y, and the second point
is found by intersecting the look ray with the plane through the first point that is most
face-on to the camera — the standard "plane-lock" trick for the missing depth; Cube adds a third
click for height. Modifiers stack: Mirror (plane with position/size), Array (offset × count),
Radial Mirror; all update live. Replace modes: Only Air / Blocks and Air / Only Blocks / Filter by
offhand. Survival: actions **consume the exact blocks** from inventory, limits on reach and
blocks per action; Ctrl+Z / Ctrl+Y [mod page].

**Building Gadgets** [guide]: modes like Build to Me (from the aimed face to your feet), Vertical
Column (height = range; aiming at top/bottom/side of the face picks up/down/centred), Surface
(spread across connected same-type faces), with a **range** stepped by hotkey (Sneak = down). The
**Anchor** hotkey freezes the preview so you can walk around and inspect it before committing;
undo remembers ~10 builds; the Exchanging gadget swaps matching blocks in a range² square
[guide]. Energy cost per block (≈50 FE) [guide].

**Feel** [mem]: these are loved because a *single* parameter (range, or a second click on an
auto-chosen plane) gets you the third dimension without a gizmo; the "aim the second click"
plane lock is the clunky part at grazing angles.

## 6. Bedrock / Java structure blocks (vanilla Minecraft)

Modes Save / Load / Corner / Data (Bedrock adds **3D Export** to `.mcstructure`) [guide wiki;
mod page image]. Save: relative offset + size (Bedrock up to 64×384×64) with a bounding-box
outline; Corner mode auto-sizes between two corner blocks; `structure_void` marks cells that the
structure must *not* overwrite on load (vs air, which clears) [mem]. Load: rotation 0/90/180/270,
mirror, **integrity %** + seed (randomly omit blocks), include entities, and on Bedrock an
**animation mode — None / Layer by layer / Block by block, with a duration** [mem]. The
void-vs-air distinction is the same as Create's replacement policy, encoded in the data.

## 7. MagicaVoxel (dedicated editor)

[guide kodeco/A-Frame/older wiki; mem] Three orthogonal choices: **action** — Attach (T), Erase
(R), Paint (G); **brush** — Voxel (V), Face (F), Box (B), Line (L), Center (C), Pattern (P);
**mirror/axis lock** buttons per X/Y/Z. Box brush = drag a rectangle *on the plane of the face
you pressed on*; the 3rd dimension comes from **Face mode**: click a face to extrude the
connected same-colour region one voxel per click (or drag), which is how boxes become volumes
[guide kodeco]. Center brush grows a circle/square from a click. Selection mode: box select (N),
Shift add, Alt+Shift remove, region select = connected same colour; Ctrl+drag moves the
selection, Shift constrains to axis; Alt+click picks colour [guide]. Undo Ctrl+Z, deep [mem].
**Feel**: famously fast because the plane comes for free from the face under the cursor; the
grid ground plane is the face when nothing is hit [mem].

## 8. Goxel, Qubicle, Avoyd, vengi (other editors)

- **Goxel** [mem]: Brush, Shape, Selection, Laser (erase along the view ray through the whole
  model), Plane, Move tools; "snap" options (to volume surface, to a reference plane, to the
  selection box) decide where a 2D mouse lands in 3D. Shape/selection is drawn as a base
  rectangle by drag, then the mouse moves to set the height, click to confirm — the explicit
  **two-phase base-then-height** gesture. Selection box faces can be dragged to resize.
- **vengi voxedit** [docs]: Shape brush — click a face, drag to size; *orientation from the
  clicked face* (cylinders run along its normal). **Plane brush** — click a face to fill that
  whole plane region, then **drag to extrude it** to a thickness. Select brush — drag a box;
  later ops are confined to it until Select→None.
- **VOXL editor** [docs]: clicking a voxel places on that face, clicking the floor grid uses
  Y=0, clicking empty space uses "the closest grid plane perpendicular to the camera's view
  direction"; Shift/Alt add/remove selection; Face selection stops at occluded faces.
- **Qubicle** [mem]: matrices (named sub-volumes) as the unit of selection; rectangle select +
  extrude; strong slice/orthographic views. **Avoyd** [mem/guide]: large-world voxel editor with
  orthographic + isometric presets; edit shapes (box/sphere) placed at the ray hit with size
  sliders — notable mainly for orthographic presets, which suit a top-down camera.

## 9. Space Engineers projectors (workers fill the hologram)

[docs SE wiki; forum] A projector block shows a blueprint as a translucent, non-solid hologram,
positioned by **offset (H/V/forward) and pitch/yaw/roll sliders**. Projected blocks count as
**unwelded placed blocks**: a welder (hand or ship/station welder) fills them using the same
components as manual building. **Only "supported" blocks can be welded** — ones touching a real
block; welding one makes its neighbours buildable, so the build front grows from the contact
point. Options: **Show Only Buildable** (hide what can't be built yet), **Keep Projection**
(for repeat builds/repair), remaining-blocks panel ("built / total" + list), **Mark Missing
(red) / Mark Unfinished (yellow)** HUD markers within ~150 m. Complaints: automated welders skip
blocks, projections vanish after grind/reweld, sub-grids need separate projectors [forum].
[mem]: blocks pass through visible construction stages (frame → finished), so partial progress
is visible on the block itself.

## 10. Other games: Valheim, Enshrouded, Hytale, Townscaper, Teardown

- **Valheim** [mem]: hammer → piece menu showing each piece's **material cost** and greying
  unaffordable ones; ghost piece snaps to snap points; ghost tinted for valid/invalid; workbench
  radius required; hovering a built piece shows **structural support colour** (blue → green →
  yellow → red). Undo = deconstruct (full refund).
- **Enshrouded** [guide]: hold Tab for Building Mode; snapping toggle (X); left build / right
  remove; voxel blocks and terraforming; base = Flame Altar claim **40×40×40 m shown as an
  outline**; advanced building only inside a base.
- **Hytale creative tools** (official preview Nov 2025 [docs Hytale]; community wiki Dec 2025):
  a **Selection Tool** selecting 3D regions for saving prefabs, precise pasting, moving, defining
  regions, regenerating chunks, extruding; transforms via position/rotation/extrusion; modes Set,
  Wall, Fill, Replace; post-launch plan for arbitrary-shape selections and a same-material "magic
  wand". **Paste Tool** places a prefab exactly (arrow keys for fine nudging) or uses it as a brush
  — pasted relative to the prefab's **anchor** (set with a Set Anchor Tool in a separate
  **Prefab Editor** world); Prefab Browser; shape Paint brush with materials/density/wall
  thickness/**axis lock**/**masks**; node-graph **Scripted Brushes**; Ruler; undo. Unreleased
  details still "unknown" on the wiki.
- **Townscaper** [mem]: no selection at all — click a cell face to add, right-click to remove;
  the *system* resolves the visual (walls, arches, roofs, stairs) from neighbourhood rules. The
  lesson: let the player paint intent per cell and let rules fill in detail.
- **Teardown** [mem]: levels are authored in MagicaVoxel + an XML/in-game editor; no in-game
  volumetric building UI worth copying for this purpose.

---

## Cross-tool patterns that recur

1. **The third dimension is never free; every tool picks one of five answers.**
   (a) *Hit surface*: the corner is the block under the cursor (WorldEdit wand, Axiom box, Litematica,
   Create quill). (b) *Plane lock*: the drag lives on the plane of the first-clicked face, or the
   grid plane most face-on to the camera (MagicaVoxel box, vengi shape, VOXL empty-space rule,
   Effortless Line/Wall/Floor). (c) *Two-phase base then height*: drag a rectangle, then move to
   set height, click (Goxel, Effortless Cube's third click). (d) *Extrude after*: make a flat
   selection then push a face (MagicaVoxel Face, vengi Plane drag, Ctrl+scroll face pull in Create,
   Axiom middle-click face / gizmo). (e) *Parameter*: a range or depth number (Building Gadgets
   range, Axiom Lasso Depth, WorldEdit `//expand n dir`). Good tools offer (b) or (d) **plus** a
   numeric/scroll fallback.
2. **Scroll wheel = depth/nudge along the axis you face** (Axiom stack, Litematica Alt+scroll,
   Create Ctrl+scroll face, Building Gadgets range). Holding X/Y/Z locks the axis (Axiom).
3. **Selection is a persistent, editable object**, separate from the operation: it can be
   adjusted (handles/gizmo, "unconfirm"), saved/loaded, composed of several boxes
   (Litematica sub-regions), and it constrains tools (Axiom, vengi).
4. **Content-aware selection** beats geometry for natural shapes: flood/magic select with
   compare mode, limits, surface-only and **up/down level clamps** (Axiom), `//trim` to content
   (WorldEdit), region-by-colour (MagicaVoxel).
5. **Add/subtract via modifiers** (Shift add, Alt/Alt+Shift remove) is the editor norm; Minecraft
   mods mostly lack intersect — none of the researched tools surface intersect prominently.
6. **Always preview before commit**: hologram/ghost of the result, frozen with an anchor/confirm
   key (Axiom Enter, Building Gadgets Anchor, Effortless live preview). WorldEdit's lack of it is
   its most-cited weakness.
7. **Gizmo conventions are Blender/Unity's**: axis arrows, plane squares, centre handle, rings;
   Ctrl = coarse snap, Shift = free; arrow keys nudge.
8. **Dimensions readout** next to the selection (Axiom offset+size above hotbar, WorldEdit
   `//size`, Litematica box info) — always numeric, always visible while editing.
9. **Blueprints = selection + anchor + rotation/mirror + metadata (tags, thumbnail)** with a
   visual browser (Axiom, Hytale prefab browser, Create/Litematica files).
10. **Ghost-vs-world diffing with a four-state colour code** — missing / wrong / wrong state /
    extra (Litematica), missing red / unfinished yellow (Space Engineers) — plus a list view of the
    same mismatches that can be clicked to locate them.
11. **Bill of materials in three numbers: required, still missing, available** (Litematica), can
    be restricted to the visible slice; Create prints it to an item; Valheim greys unaffordable.
12. **Explicit overwrite policy** for placing into occupied space: only-air / solid-with-solid /
    any / clear-everything (Create), replace modes (Effortless), structure-void vs air (structure
    blocks), masks (WorldEdit/Axiom).
13. **Buildability frontier**: Space Engineers only lets you weld blocks touching real ones and
    can hide the rest; Create stalls on a missing block with a one-line reason. Builders need a
    visible "why is this not progressing".
14. **Layer slicing** is how 3D blueprints are made workable: Litematica's ALL_BELOW / single layer
    / range, Bedrock's layer-by-layer load animation.
15. **Undo is per-user, bounded and coarse-grained** (one operation = one entry; WorldEdit 15,
    Building Gadgets ~10, Axiom history window); indirect effects are not undone (WorldEdit).

## What transfers to an RTS-camera colony game (and what doesn't)

**Transfers well**

- **Plane lock from a z-level.** The DF camera already has a "current level". Make the default
  drag a rectangle *on the current slice plane* (pattern 1b) — no depth ambiguity — then add
  height by **(d) extrude**: drag a face handle up/down or scroll while dragging, with the
  dimensions readout `12 × 8 × 3` live (patterns 2, 8). Shift-drag on a terrain surface could
  instead project onto the hit surface (1a) for "dig this hillside".
- **Content-aware designation.** Magic select with Axiom's options maps directly: "select this
  ore vein" (compare = material, limit, range-jumps-gaps), "this tree" (connected logs + leaves),
  "surface only", and **up/down clamps from the clicked level**. WorldEdit `//trim`-to-content is
  "shrink designation to minable blocks".
- **Selection as an object** that can be re-opened (Axiom unconfirm), built from several boxes
  (Litematica sub-regions) and combined with Shift/Alt add/subtract — DF's designations are
  already per-cell sets, so add/subtract/intersect are cheap; offer intersect, which nobody does.
- **Preview before commit** for every designation and placement: ghost + numbers, Enter/click to
  confirm, Esc to cancel.
- **The construction job model is Create + Space Engineers + Litematica combined**: a placed
  blueprint is a ghost of "unbuilt placed blocks"; workers fill it (SE); it has an overwrite policy
  and a skip-missing option (Create); it renders the four-state diff (Litematica colours) and a
  per-job status line with the blocking reason (Create's "Missing Block", SE's "not supported");
  and a **material panel with required / missing / available-in-stockpiles**, filterable to the
  visible z-slice (Litematica). Buildability frontier (SE) maps to DF's "needs support / not
  reachable" and should be visualised, not just logged.
- **Layer views** — Litematica's single / all-below / range is exactly DF's z-slice and should be
  shared by world rendering, ghost rendering and material counts.
- **Blueprints** as clipboard + anchor + rotation/mirror + thumbnail + tags (Axiom/Hytale), placed
  with a 2-axis gizmo on the slice plane plus Q/E rotate, F flip, arrow nudge.
- **Shape modes from the quick-build mods** (line, wall, floor, hollow box, circle) as designation
  shapes, and **mirror/array modifiers** for symmetric fortresses.
- **Overwrite policy as data** (structure void vs air): a blueprint cell can say "must be air",
  "don't care", or "this block" — lets a room blueprint carve its own interior.

**Does not transfer (or needs rework)**

- **First-person aiming of corners** (WorldEdit wand, `//hpos`, Building Gadgets "build to me",
  Effortless's second click on a camera-chosen plane) — with a top-down camera the look ray is
  near-vertical, so plane-lock to the z-level replaces it; "build to me" has no "me".
- **Free-orbit 3D gizmos** are clumsy from a fixed pitch: the vertical axis arrow is
  foreshortened. Prefer slice-plane handles + scroll/keys for height.
- **Lasso projected along the view ray with a depth slider** (Axiom) is natural top-down (the ray
  is ~vertical, so depth = number of z-levels), but in a tilted camera it shears; clamp to
  whole columns × z-range.
- **Instant edits + undo as the main safety net** (WorldEdit, Axiom): in a colony game edits are
  *orders* executed later by workers, so "undo" is cancel/modify the pending designation; only
  executed work is irreversible (except by deconstruct, Valheim-style refund).
- **Creative-only power** (unlimited blocks, chunk regeneration, scripted terrain brushes) has no
  place in fortress mode beyond dev tools; survival constraints (Effortless consuming exact
  blocks, Create's fuel/inventories) are the relevant half.
- **Deep chorded hotkeys and menu stacks** (Litematica M+key chords) — acceptable for a
  power-user mod, not for a primary game UI; surface modes on a toolbar with hints, as Axiom's
  bottom bar does.

---

## Sources

- WorldEdit selections — https://worldedit.enginehub.org/en/latest/usage/regions/selections/
- WorldEdit region operations — https://worldedit.enginehub.org/en/latest/usage/regions/regionops/
- WorldEdit history — https://worldedit.enginehub.org/en/latest/usage/general/history/
- Axiom docs: selections — https://axiomdocs.moulberry.com/editor/selections.html
- Axiom docs: Magic Select — https://axiomdocs.moulberry.com/tools/selection/magicselect.html
- Axiom docs: Box Select — https://axiomdocs.moulberry.com/tools/selection/boxselect.html
- Axiom docs: Lasso Select — https://axiomdocs.moulberry.com/tools/selection/lasso.html
- Axiom docs: Freehand Select — https://axiomdocs.moulberry.com/tools/selection/freehandselect.html
- Axiom docs: Gizmos — https://axiomdocs.moulberry.com/editor/gizmos.html
- Axiom docs: Tools overview — https://axiomdocs.moulberry.com/tools/intro.html
- Axiom docs: Builder Mode — https://axiomdocs.moulberry.com/builder/intro.html
- Axiom docs: Stack — https://axiomdocs.moulberry.com/builder/buildertools/stack.html
- Axiom docs: Editor Mode — https://axiomdocs.moulberry.com/editor/intro.html
- Axiom docs: Blueprints — https://axiomdocs.moulberry.com/editor/windows/blueprints.html
- Axiom Modrinth + changelog — https://modrinth.com/mod/axiom , https://modrinth.com/mod/axiom/changelog?page=3 , https://modrinth.com/mod/axiom/version/5.1.0?dep=1
- Litematica repo — https://github.com/maruohon/litematica
- Litematica FAQ — https://github.com/maruohon/litematica/wiki/Frequently-Asked-Questions
- Litematica CurseForge / Modrinth — https://curseforge.com/minecraft/mc-mods/litematica , https://modrinth.com/mod/litematica
- Litematica wiki (third party) — https://wiki.9minecraft.net/litematica-mod/
- Litematica how-to guides — https://help.simplegamehosting.com/how-to-use-litematica-in-minecraft , https://www.technobezz.com/how-to-use-litematica-in-minecraft
- Litematica layer add-on — https://modrinth.com/mod/bEpr0Arc
- Create wiki: Printing a Schematic — https://github.com/Creators-of-Create/Create/wiki/Printing-a-Schematic
- Effortless Building — https://modrinth.com/mod/effortless-building , https://www.curseforge.com/projects/302113 , https://modrinth.com/mod/effortless-building-sophisticated
- Building Gadgets — https://github.com/SturdyFool10/BuildingGadgets , https://craftdownunder.co/guides/mods/building-gadgets , https://www.minecraft-guides.com/mod/building-gadgets/
- Structure Block — https://breezewiki.discard.no/minecraft/wiki/Structure_Block , https://minecraft.gamepedia.com/File:StructureLoadGUI.png
- MagicaVoxel — https://ephtracy.github.io/ , https://www.kodeco.com/375-magicavoxel-3d-art-tutorial , https://aframe.io/docs/0.8.0/guides/building-with-magicavoxel.html , https://magicavoxel.fandom.com/wiki/Introduction_to_MagicaVoxel_0.99.1_(tutorial)?oldid=138
- vengi voxedit brushes — https://vengi-voxel.github.io/vengi/voxedit/usage/Brushes
- VOXL editor — https://william-saxton.github.io/VOXL-Editor/editing-basics.html , https://william-saxton.github.io/VOXL-Editor/shapes-and-tools.html , https://william-saxton.github.io/VOXL-Editor/advanced.html
- Avoyd listing — https://www.sourceforge.net/software/product/Avoyd
- Space Engineers projector — https://spaceengineers.wiki.gg/wiki/Projector , https://steamcommunity.com/app/244850/discussions/0/3790380616234281940
- Hytale creative mode (official) — https://hytale.com/news/2025/11/hytale-creative-mode
- Hytale creative (community) — https://hytalemodding.dev/en/docs/established-information/gameplay/creative , https://docs.craftserve.com/en-US/hytale/commands/buildertools
- Enshrouded building — https://help.bisecthosting.com/hc/en-us/articles/41520916622107-How-to-Quickly-Terraform-in-Enshrouded , https://pcgamer.com/the-base-building-system-in-this-fantasy-co-op-survival-rpg-looks-pretty-incredible , https://supercraft.host/wiki/enshrouded/base-building-guide
- Axiom vs WorldEdit comparisons — https://sportskeeda.com/minecraft/axiom-vs-worldedit-which-minecraft-mod-better-you , https://premiumminecraft.com/?p=13697
