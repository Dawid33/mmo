# 12 — The UI and interaction model

Part of the Dwarf Fortress research (2026-10-20); start at `00-synthesis.md`. Source tags as in
`01-world-and-embark.md`. Reference: the Steam/premium (v50+) mouse UI; classic was
keyboard-menu driven (`d`esignate, `b`uild, `q`uery, `v`iew unit, `z` status …) [mem].

## 1. Screen layout (premium)

[wiki Interface]:

- **A** announcements (pop up; important ones pause and centre the view).
- **B** bottom-left menu buttons: creatures (citizens, pets, others, dead), tasks, places
  (zones/locations/stockpiles/workshops), labor (work details, standing orders, kitchen, stone
  use), work orders, nobles, objects, justice.
- **C** FPS counter (sim ticks and graphical FPS).
- **D** centre-bottom **designation tools**: mining, logging, gathering, smoothing/engraving,
  remove designations, building, stockpiles, zones, burrows, minecart haul routes, traffic, item
  actions (melt, dump, hide, forbid).
- **E** bottom-right: military (squads) and world overview.
- **F** a **depth bar** showing how deep the viewed level is (blue = above ground).
- **G** a **minimap of the current level** with the visible area outlined and the level number.
- **H** game controls: pause/play (Space), settings, help, hotkeys, "move view to surface here",
  "move view to lowest revealed level here", zoom in/out (`[`/`]` or Ctrl+wheel), show fluid
  levels (`f`), show ramp indicators (`r`).
- **I** fortress status: name, happiness summary, food stock counts, calendar.

## 2. Z-level navigation

[wiki Z-level, Interface]: **mouse wheel steps one z-level** up/down (keyboard `<` `>` in
classic); Ctrl+wheel zooms. The view shows exactly one level; open space reveals the level below,
drawn darker ("dark-colored water symbols indicate the water is one z-level below") and the
`▼` slope marker shows ramps from above. Jump buttons go to the surface or the lowest revealed
level at the cursor. The depth bar and minimap always reflect the current level.

## 3. Designating and building

[wiki Stockpile, Zone, Stairs] + [mem]:

- Select a tool, then **click two corners** to drag a rectangle (stockpiles, zones, digging,
  constructions); a secondary toolbar switches paint/rectangle/erase; some tools take a box
  spanning several z-levels (v50 stairs: drag at least 2 levels).
- Placement shows validity per tile (green/red, "Blocked", "Needs soil or mud").
- Buildings: choose type → place footprint → choose materials (or "nearest") → a job appears.
- Everything placed can be selected later and edited (settings, links, deletion).

## 4. Information panels

Click a unit → a **unit sheet**: overview (job, mood face, needs summary), thoughts and
preferences, personality, skills, health (wounds by body part), items, relationships, rooms,
labor/work details. Click a tile → its contents (items, building, liquid, material). Workshops
show their task queue and linked stockpiles. Stocks screen lists all items by category with
counts [mem]. These panels are a large share of the UI work and are read-only views over sim
state.

## 5. What the view must support (requirements for our renderer/UI)

1. A **current z-level** with everything above it hidden (cutaway), mouse-wheel stepping,
   jump to surface / lowest revealed.
2. Levels below visible through open space, darkened by depth.
3. **Hidden (unrevealed) tiles** drawn as an opaque unknown — never leak the map.
4. **Designation overlays** per tile (dig/channel/stairs/chop/…, with priority), stockpile and
   zone tints, traffic colours, building footprints and placement validity.
5. **Job markers** (who is doing what where), carried items, sleeping/eating icons.
6. Fluid level display (0–7) toggle; ramp indicators.
7. **Picking**: tile under the cursor *at the current level* (not the terrain surface), unit and
   building picking.
8. Rectangle drags on one level and across levels.
9. Info panels and list screens (units, stocks, work details, stockpile settings).
10. Pause/speed, calendar, announcements with click-to-centre.

## 6. MVP tiers

| item | tier |
|---|---|
| Z-level cutaway + wheel stepping + surface jump | **MVP** |
| Hidden tiles | **MVP** |
| Rectangle drags for dig/channel/stairs/chop, stockpiles, zones, building placement | **MVP** |
| Designation and stockpile overlays | **MVP** |
| Unit sheet (job, needs counters, skills, wounds) | **MVP** (basic) |
| Tile inspector | **MVP** |
| Pause, speed, calendar, announcements | **MVP** |
| Minimap of the current level, depth bar | soon after (the commander minimap exists, per surface) |
| Stocks screen, work details screen, stockpile settings screen | soon after |
| Multi-level box drags | soon after |
| Thoughts/personality/relationships panels | later (as those systems land) |

## Sources

- https://dwarffortresswiki.org/index.php/Interface
- https://dwarffortresswiki.org/index.php/Z-level
- https://dwarffortresswiki.org/index.php/Water
- https://dwarffortresswiki.org/index.php/Stockpile
- https://dwarffortresswiki.org/index.php/Zone
- https://dwarffortresswiki.org/index.php/Stairs
