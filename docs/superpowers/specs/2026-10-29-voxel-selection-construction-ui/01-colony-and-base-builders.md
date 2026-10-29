# 01 — Selection and construction in colony and base-building games

Part of the 2026-10-29 voxel selection/construction UI research (start at `00-synthesis.md`).
Dwarf Fortress itself is covered in `../2026-10-20-dwarf-fortress-research/12-ui-and-interaction.md`
and `03-designations-and-jobs.md` and is not repeated here.

Source tags: **[wiki X]** a wiki or guide page, **[dev]** developer patch notes / blog / dev reply,
**[forum]** player posts (Steam discussions, feature-request boards), **[review]** press,
**[mem]** my own recollection of the game, not re-verified for this report — treat [mem]
details (exact keys, version numbers) as likely-but-unconfirmed. Web search coverage for several
of these games was thin (official wikis are sparse on UI); the [mem] share is correspondingly
high for Gnomoria, Stonehearth, Dungeon Keeper and Clockwork Empires.

Our target, for orientation: 1 m blocks in 32³ chunks, a tilted top-down commander camera
(pan / rotate / zoom), indirect control — select + designate + place, AI workers execute.

---

## 1. Going Medieval (Foxy Voxel, 2021–; 3D block world, z-layers)

The closest analogue to us: a 3D voxel-ish (block-terrain) colony sim with a free perspective
camera, multi-storey buildings and diggable terrain.

**Vertical layers.**
- The world is cut at a **camera layer**; everything above the current layer is hidden. The
  layer moves in **half-level steps**: at layer N.5 walls of level N are shown half height —
  the most useful editing view, per players [forum 3055…, 4361…].
- Originally the layer followed the camera/cursor height automatically. **Camera 2.0**
  (experimental 0.22.34, Feb 2025) reworked it: multiple raycasts so the layer only changes when
  hovering over *larger* height changes; **Caps Lock locks the layer**, and while locked
  **Shift+wheel steps layers**; the same commands are icons in a top-left camera panel [dev
  patch notes via eprison.de].
- Complaints (2021, still raised 2024): at a half-layer, objects on the level *above* (furniture,
  floors) are still drawn semi-transparently, so "half-levels are extremely hard to use"; players
  want a clean cut and/or "hide the wall nearest the camera" [forum 3055…, 4361…].

**Selection.**
- Rectangle drag for dig, chop, stockpile zones, floors; walls are dragged as lines [mem].
- The selection is projected onto the terrain under the cursor, and that is the problem:
  **"dragging a mining selection on level 4 registers on level 5"**; clicking where only the
  bottom of an object shows on the current level selects the level *below*; clicks select
  objects *behind* the visible foreground (likely terrain and buildings using different
  collision). Players ask for a **"snap to level" hotkey** that confines all interaction to the
  current layer [forum 3055…]. Camera 2.0's layer lock is effectively the dev answer [dev].

**Designations and construction.**
- Base menu (F1): walls, floors, doors, roofs, beams, stairs; variants (other materials /
  looks / strength) in a sub-strip bottom-right [wiki eip.gg]. Zones (stockpiles) in a separate
  menu (F6/F7 depending on version) with per-zone item filters [wiki MGW].
- Placed pieces are translucent **blueprints** until hauled + built; a material shortage makes
  settlers idle silently [wiki eip.gg] [mem].
- **Structural support** is a first-class rule: roofs / upper floors need supports and the game
  warns when unsupported; fix is a beam between walls below; an integrity overlay colours
  stability [wiki eip.gg] [mem]. Rooms = enclosed by walls + floor + roof [wiki eip.gg].
- Pain points: adding beams under a collapsed floor is tedious; no way (in 2021) to fill holes
  with dirt (one-way terrain editing) [forum 3055…].

**Takeaways.** Auto-following layer + cursor-projected selection is the worst combination for a
3D colony builder; an explicit, lockable working layer that *all* picks respect is the minimum.

## 2. Timberborn (Mechanistry, 2021–; 3D voxel-column terrain, stacked platforms)

**Vertical layers.**
- A **layer-hiding tool** (a "maximum visible level" control with up/down steps and a red
  button to restore the unlimited default view) hides everything above a chosen height; it has
  a keybinding "Change the maximum visibility level" [wiki gamepressure keybinds] [dev reply].
- Players regularly think buildings vanished — the dev reply on the feature board is "the layer
  visibility tool is active, press the red button"; the request is an **on-screen warning when
  the layer cut is active** [forum featureupvote 221063]. Patch notes had to make Impermeable
  Floor see-through and visible under buildings with the tool on [dev steamdb 15600070].
- A popular **X-Ray mod** (Ctrl+P / hold P) makes terrain transparent so structures *under*
  terrain can be seen and selected, and makes surface buildings stop blocking selection of
  objects beneath; H toggles object transparency; optional grid/contour overlay [wiki X-Ray mod].
  Its existence is a signal that the built-in cut isn't enough for underground building.

**Selection / construction.**
- Paths, platforms, levees, floodgates etc. are dragged as lines/areas; single buildings are
  click-placed with rotation [mem]. Preview **green valid / red blocked** (water, steep slopes,
  occupied) [wiki timberborn.org].
- Marked buildings appear as a **translucent blueprint** until materials are delivered and
  builders finish [wiki timberborn.org]. Per-project **priority** setting; unfinished buildings
  can be paused [wiki timberborn.org] [mem].
- **Demolition** is a tool you click (or drag) over targets; builders tear down over time, ~50%
  refund [wiki timberborn.org]. Terrain is removed with **dynamite** (one block per charge,
  stackable down) — digging is itself a placed, built object [mem].
- Vertical: platforms stack; stairs/ramps for access; "stacking" rules per building (what can
  sit on what) are a wiki topic of their own [wiki timberborn.wiki.gg Stacking].

## 3. Stonehearth (Radiant, 2013–2018; voxel world, free 3D camera)

- **Mining zones** are 3D boxes: click-drag the footprint on the ground, then move the mouse
  vertically to set the depth/height, click to confirm [mem]. Zones persist as objects:
  with the mining tool active, right-click to drop the tool and click an existing zone to
  suspend / resume / remove it [forum 3202…]. Players advise splitting big mines into small zones
  (workers + pathing degrade on huge ones) [forum 3202…]. Harvest menu has an **eraser** that
  drag-deletes items in an area [forum].
- **Building editor** (reworked as "Building 2.0" late in development) [mem + forum]: rooms are
  the base unit; walls / floors made with the room tool belong to that room; standalone walls,
  slabs and roof pieces attach to rooms; floor/wall/roof **patterns are brushes** (data-driven,
  `building_brushes.json`) [wiki modding guide]. You design as a hologram, then press **Build**;
  the design becomes a construction job with scaffolding. Saved **templates** (and sharing)
  [forum].
- Complaint: once construction starts you cannot edit piece-by-piece — tear down and rebuild,
  templates help [forum 1728…]. Building UI widely described as powerful but fiddly [mem].
- Wall/roof occlusion: a toggle to hide or cut away building walls/roofs while editing [mem —
  unverified].

## 4. Gnomoria (Robotronic, 2016; isometric 2.5D, DF-like z-levels)

- **One level shown at a time** (DF model in isometric tiles); levels are numbered with
  underground negative (stone around −7 on many maps) [wiki Getting Started]. Level up/down by
  key/wheel; levels below shown through open space [mem].
- Designations from a **right-click context menu** on ground: category → task (e.g.
  Agriculture → Fell Trees) [wiki Getting Started]; then drag a rectangle [mem]. Mining jobs:
  mine, mine stairs down/up, ramps, channel-like digs [mem].
- Everything built can be deconstructed and rebuilt elsewhere (materials returned)
  [wiki/indiegamereviewer]. Ingnomia is an open-source reimplementation in the same style
  [store].
- Complaint recurring in reviews of 2.5D DF-likes: isometric single-level view hides overhangs
  and makes multi-level designs hard to read [mem].

## 5. Dungeon Keeper 1/2 and War for the Overworld (dungeon managers, "tag the dig")

- **Tag, don't command.** Left-click (or drag a box) on rock **tags** tiles for digging; imps /
  workers pick tagged tiles up automatically; right-click / second click untags [mem]. Tagged
  tiles get a coloured highlight/flag; gold/gem seams are distinct [mem]. Dug tiles become
  unclaimed floor, which workers then **claim** (convert to your colour) — claim is automatic,
  players complain it is half-hearted when far away; dev: working as intended [forum WftO
  support 1229].
- **Rooms are painted** tile-by-tile (or drag) onto claimed floor; room function and efficiency
  depend on size / shape / adjacency (e.g. WftO foundry bonus from adjacent walls) [LP archive
  WftO].
- **Possession**: drop to first-person control of one unit — the one direct-control escape
  hatch in an indirect game [review]. Obey/speed spells accelerate workers [forum].
- Single-level world: no z problem, which is part of why the tag-paint loop feels so clean.

## 6. RimWorld (2D baseline, single level)

- **Architect** menu: Orders (Deconstruct, Mine, Haul, Cut, Harvest, Chop, Hunt, Slaughter, Tame,
  Uninstall, Claim, Strip, Open, **Plan** + Remove plans), Zones/Areas (stockpile, dumping,
  growing, allowed area, home, build roof / remove roof area, snow clear) [wiki Architect].
- Gestures [mem]: designators drag rectangles; walls drag **lines with a right-angle elbow**
  (L-shaped) and show length; floors rectangle; area tools have expand/shrink variants (paint
  and erase as separate tools). Cancel designator erases any designation under the drag.
- Construction: pick building → **stuff (material) dropdown** on the build button → **blueprint**
  (blue ghost, free) → **frame** (materials hauled in) → built. Invalid placement shows a red
  ghost + **reason text at the cursor** ("Space already occupied", "Must be placed on…") [mem].
- **Plan** designation = free-form colour marking with no effect, used to sketch layouts before
  committing (players asked for this for years; added in 1.x) [wiki Architect].
- **Copy** gizmo on a selected building starts placing another with same type/material/rotation
  [mem]. Priorities via colonist work tab + right-click "Prioritize" forcing [mem].

## 7. Songs of Syx (2D, city-scale)

- Rooms are drawn as **areas** (rectangle + brush) and must be a **single connected region**;
  then **furniture** is placed inside; walls are generated/edited around [forum 2969…].
- **Copy footprint** of a room and place copies from the bottom of the buildings tab; you can copy
  an area for immediate reuse but (as of the posts) not save a multi-room area as a reusable
  blueprint — only single-room prints; workaround: build it somewhere and copy later [forum].
  A community browser **Room Layout Planner** exports to the game's `SavedPrints` [forum].
- Complaint: right-clicking during furniture placement wipes the whole furniture plan with no
  undo [forum 3784…]. QoL mod "Fixing the Syx" relaxes rock/door placement rules [mod page].

## 8. Oxygen Not Included (2D side view; added — strongest designation-tool UI in the genre)

All [mem]:
- Every designation tool (dig, mop, sweep, deconstruct, cancel, prioritize, disinfect, attack,
  capture) is a **rectangle drag**; tools that act on several *layers* in one cell
  (buildings / pipes / wires / automation / tiles / debris) have a **filter panel** ("All",
  "Liquid pipes", "Wires"…) so one drag cancels/deconstructs only the chosen layer.
- **Priority 1–9 + red "!!" emergency** chosen in a palette while dragging; a **prioritize
  tool** repaints priority on existing errands; the priority overlay shows numbers per cell.
- Build ghosts: translucent, with material selector (per-material stats) in the build panel;
  **drag-to-build** lines/areas for tiles, ladders, pipes, wires (pipes path-follow the drag).
- **Copy building** (B) re-arms placement with the selected building's type and material;
  **copy settings** brush applies one building's config to others.
- Overlays (oxygen, power, liquid, gas, temperature, rooms) change the whole view and keep the
  relevant build tools usable within them.

## 9. Factorio (2D; the reference for ghosts, blueprints, planners, undo)

- **Ghosts**: semi-transparent markers for entities *and tiles*; placed without range limit;
  carry configuration (recipes) that applies on build; player ghosts never expire (destroyed-
  entity ghosts do, with a purple timer bar); construction robots build them from the network
  [wiki Ghost].
- **Blueprints** [wiki Blueprint]: hold blueprint item, drag a box → entities highlight → setup
  dialog (icons, component count, Create); **Shift+select skips the dialog**. Placement: R
  rotate, H/V flip; **Shift = force build** (marks trees/rocks under it for deconstruction,
  landfill ghosts on water, places the unblocked parts of a blocked print); **Ctrl+Shift = super
  force** (also deconstructs colliding buildings). Things marked for deconstruction don't block.
  Blueprint **books** and a persistent, multiplayer-shared **library**; export/import as
  strings; **parameterised** blueprints with formulas.
- **Deconstruction / upgrade planners**: drag-box tools with entity/tile filters (whitelist /
  blacklist), Alt-drag to cancel [mem]. Copy/cut/paste: Ctrl+C / Ctrl+X / Ctrl+V make a
  temporary blueprint from a drag; cut = paste + deconstruct the source [mem].
- **Undo/redo** (2.0, FFF #412) [dev]: records build, deconstruct, upgrade orders, plus wires,
  copy-pasted settings and blueprint-over-existing settings; *not* manual R rotations (would flood
  the stack). Redo Ctrl+Y. Flying text says what was undone; button tooltip names the action and
  location; undoing something older than a few minutes asks for **confirmation with a ghost/
  deconstruction preview** of what will change.

## 10. Satisfactory (first-person, but the reference for 3D placement snapping)

[wiki Build Gun]:
- **Build modes** cycled with R (hold for radial): default, **zoop** (up to 10 pieces in a line
  *or* a vertical column — foundations, walls, ramps, pillars, beams), vertical zoop; multi-click
  pieces (first click sets location, then raise/rotate/angle).
- Rotation: wheel, snapping 45°/90° on foundations; Ctrl for 10° steps.
- **Snap**: Ctrl shows alignment guidelines (solid = side alignment, gradient = snap to ports)
  and auto clearance spacing.
- **Hologram colour: blue valid, red hard-clearance clash, yellow soft clearance**; colours
  user-customisable (accessibility).
- **Hologram lock** (H) then **nudge** with arrows (1 m horizontal), PgUp/PgDn (1 m vertical),
  Ctrl for 0.5 m — exact placement decoupled from the mouse.
- **Eyedropper** (middle-click) copies a building incl. recipe/clock; **mass dismantle**: hold
  Ctrl to multi-select up to 50, with a **type filter** (G); blueprint-aware dismantle selects a
  whole stamped blueprint from any part. In-place upgrade by aiming a higher tier at a piece.
- Blueprint Designer: a fixed-size cube where you build a module, save it, stamp it as a single
  hologram [mem].

## 11. Foundation and Manor Lords (organic, gridless placement)

- **Foundation**: a **brush** with square/circle modes paints both global zones (Residential,
  Forbidden) and per-building zones (crop field, reforestation); size with Ctrl+wheel or P/L;
  players still find painting large residential areas tedious [wiki Brush, forum]. Monuments are
  assembled from parts in a **monument editor** — modular, part snaps to part [mem].
- **Manor Lords**: no grid by design [forum dev quote]. Roads are dragged curves; residential
  **burgage plots** are drawn as a free quadrilateral (four points) which the game subdivides,
  with **+/- buttons to change the plot count** in the drawn area; fields likewise as polygons
  [forum, wiki]. Complaint: four points are too few; players use roads as guide lines to align
  plots [forum 4355…].

## 12. Clockwork Empires, Craft the World, and the 3D DF-likes

- **Clockwork Empires** (2016) [mem]: building = draw a **footprint** on the ground, then place
  **modules** (beds, ovens, machinery) inside it; workcrews (not individuals) get assigned to
  buildings; zones are rectangles. Reviewers liked the module-in-footprint idea, disliked
  opaque job assignment.
- **Craft the World** [mem]: 2D side view, Terraria-like; dig / build commands are
  drag-rectangles on the grid; walls and backgrounds placed as ghost blocks hauled by dwarves;
  a build queue panel. Direct-control spell actions mix in.
- **Embark** (2019, 3D voxel DF-like) advertises building "in multiple Z levels" — z-slicing
  over a voxel 3D view; development stalled (no update in 2+ years) [store]. **Odd Realm** uses
  top-down **2D cross-sections per z-level** (DF model) [press]. **Mountaincore** (re-release)
  and **Ingnomia** similar [store]. **Dawn Apart** (2025 EA) is a voxel factory/colony hybrid
  [press]. None found with documented UI worth copying beyond Going Medieval / Timberborn.
- Dropped as irrelevant/undocumented: Dig or Die (direct-control platformer), Valley of the
  Gnomes, Towngard, "Hearthfire", "Ever Kingdom" (no usable UI sources found).

---

## 13. Patterns that recur

1. **Tag/designate → ghost → job → built** is universal (DK tags, RimWorld blueprint → frame,
   Timberborn blueprint, Factorio ghost). The ghost is a persistent, selectable, cancellable
   world object with a priority, not a transient cursor preview.
2. **Rectangle drag is the default gesture**; lines (walls, paths, pipes, Satisfactory zoop)
   and brushes (zones: Foundation, Songs of Syx, RimWorld areas) are the two common extras.
   Paint and erase are usually *separate tools* or a modifier, never inferred.
3. **3D extent is set in two phases** where it exists: footprint drag, then vertical drag /
   wheel / mouse-up-down for height (Stonehearth mining, Satisfactory vertical zoop, DF v50
   multi-level stairs). Nobody does a single-gesture 3D box.
4. **The working layer is explicit and lockable** in the games that got it right(er): Going
   Medieval's Camera 2.0 lock + Shift+wheel; Timberborn's max-visible-level control. All picks
   must be confined to that layer (the "snap to level" request).
5. **Cutaway = hide everything above layer N**, ideally with a half-height-walls option for
   editing rooms. X-ray / transparency is a *second* tool for seeing *under* surfaces (Timberborn
   mod) and must also change what is pickable.
6. **Validity in colour + reason in text**: green/red (Timberborn), blue/red/yellow with soft vs
   hard clash (Satisfactory, colours configurable), RimWorld-style reason at the cursor; force
   modes that convert blockers into deconstruction orders (Factorio Shift / Ctrl+Shift).
7. **Priorities are painted**, not just set per job: ONI priority palette + prioritise tool,
   Timberborn per-project priority, RimWorld force-prioritise.
8. **Filters on bulk tools** (ONI layer filter, Factorio planner filters, Satisfactory dismantle
   type filter) — essential when one cell holds several things (terrain + floor + furniture +
   designation).
9. **Copy / eyedropper / templates**: copy-this-building (RimWorld, ONI B, Satisfactory
   middle-click), area copy (Songs of Syx, Factorio Ctrl+C), saved templates/libraries
   (Stonehearth, Factorio library + strings), parametrised prints (Factorio).
10. **Undo is order-level, not world-level**: Factorio undoes *orders* (builds, deconstructs,
    settings), with confirmation + preview for old ones; it excludes high-frequency trivial
    actions (rotations).
11. **Structural rules surface in the placement preview** (Going Medieval support warnings and
    integrity overlay; Timberborn stacking rules).
12. **Zones/designations are persistent objects you can re-select** to suspend / resume /
    remove (Stonehearth mining zones, DF, RimWorld zones) — not one-shot paint.

## 14. What players complain about

- **Layer picking leaks between levels** (Going Medieval): a drag on level 4 lands on level 5,
  clicks hit hidden or background objects, partial-visibility selects the wrong level [forum].
- **Ambiguous half-layer rendering**: upper-floor furniture drawn see-through at half levels;
  no clean cut; wish for "hide nearest wall" [forum GM 2021 & 2024].
- **Invisible mode state**: Timberborn players think buildings disappeared because the layer cut
  is on; ask for an on-screen indicator [forum, dev reply].
- **Auto-changing layers** with camera height (GM pre-2025) — fixed by making it lockable [dev].
- **Can't see / select underground** without a mod (Timberborn X-Ray) [mod].
- **No editing after commit** (Stonehearth buildings: tear down and rebuild) [forum].
- **Destructive cancel with no undo** (Songs of Syx right-click wipes furniture plan) [forum].
- **Tedium of large paint jobs** (Foundation residential brush, GM support beams) — wants
  bigger brushes, fill, or auto-support [forum].
- **Too-rigid freeform tools** (Manor Lords 4-point plots) and, conversely, gridless placement
  being hard to align [forum].
- **Big single jobs behave badly** (Stonehearth huge mines; WftO claiming far away) — players
  learn to chunk work manually [forum].
- **Silent stalls**: blueprints waiting on missing materials without a clear cause (GM idle
  settlers) [wiki eip.gg].

## Sources

- Going Medieval half-level view thread (2021): https://steamcommunity.com/app/1029780/discussions/0/3055111535922602464
- Going Medieval see-through upper floor thread (2024): https://steamcommunity.com/app/1029780/discussions/0/4361250086034818336
- Going Medieval Camera 2.0 patch notes (0.22.34): https://www.eprison.de/spiele/going-medieval/steam-news/1791580425999382/6460/88266.html
- Going Medieval construction guide: https://eip.gg/going-medieval/guides/construction
- Going Medieval beginner guide (zones): https://guides.magicgameworld.com/going-medieval-tips-and-tricks-a-beginners-guide/
- Timberborn layer visibility feature request + dev reply: https://timberborn.featureupvote.com/suggestions/221063/
- Timberborn patch notes (layer-hiding tool, Impermeable Floor): https://steamdb.info/patchnotes/15600070
- Timberborn construction / placement guide: https://timberborn.org/articles/construction-building-placement-guide
- Timberborn stacking: https://timberborn.wiki.gg/wiki/Stacking
- Timberborn keybinds: https://www.gamepressure.com/timberborn/keybinds/z7ff47
- Timberborn X-Ray mod: https://github-wiki-see.page/m/ihsoft/TimberbornMods/wiki/X-Ray
- Stonehearth mining zone thread: https://steamcommunity.com/app/253250/discussions/0/3202620911887101843
- Stonehearth building editor threads: https://steamcommunity.com/app/253250/discussions/0/1728701877459504586 , https://steamcommunity.com/app/253250/discussions/0/3112522283868854896
- Stonehearth modding guide, building parts: https://stonehearth.github.io/modding_guide/modding_guide/intermediate/building_parts/index.html
- Gnomoria wiki, Getting Started: https://gnomoria.fandom.com/wiki/Getting_Started?oldid=4200
- Gnomoria wiki, Hostile Creatures: https://gnomoria.fandom.com/wiki/Hostile_Creatures
- War for the Overworld claiming ticket: https://support.brightrockgames.com/communities/1/topics/1229-imps-to-finish-claiming-recently-dug-out-land?lang=lv
- War for the Overworld LP archive: https://lparchive.org/War-for-the-Overworld/Update%2004
- War for the Overworld review: https://staging2.eip.gg/reviews/war-overworld-review
- RimWorld wiki, Architect: https://rimworldwiki.com/wiki/Architect
- Songs of Syx room threads: https://steamcommunity.com/app/1162750/discussions/0/2969519680679214971 , https://steamcommunity.com/app/1162750/discussions/0/3784751116695130066
- Fixing the Syx mod: https://catalogue.smods.ru/archives/436816
- Factorio wiki, Blueprint: https://wiki.factorio.com/Blueprint
- Factorio wiki, Ghost: https://wiki.factorio.com/Ghost
- Factorio Friday Facts #412 (undo/redo): https://direct.factorio.com/blog/post/fff-412
- Satisfactory wiki, Build Gun: https://satisfactory.wiki.gg/wiki/Build_Gun
- Foundation wiki, Brush: https://wiki.polymorph.games/fr/foundation/Brush/history
- Foundation brush-size thread: https://steamcommunity.com/app/690830/discussions/0/3731826842440390705/
- Manor Lords burgage threads: https://steamcommunity.com/app/1363080/discussions/0/4355620138225704118 , https://steamcommunity.com/app/1363080/discussions/0/4362376876606662212
- Manor Lords burgage plot wiki: https://videogamer.com/wiki/Manor-Lords/Burgage-plot
- Embark (Steam): https://store.steampowered.com/app/1055090/
- Odd Realm press: https://st2.moderno.dk/odd-realm-is-dwarf-fortress-for-people-who-can-039-t-keep-houseplants-.html
- Dawn Apart release (Notebookcheck): https://www.notebookcheck.net/New-highly-anticipated-colony-sim-releases-on-Steam-today-after-raising-over-36-000-on-Kickstarter.1057721.0.html
- Ingnomia (Steam): https://store.steampowered.com/app/709240/Ingnomia/?l=english
- Mountaincore announcement: https://www.moddb.com/games/mountaincore/news/announcing-mountaincore-simstrategy-colony-builder
