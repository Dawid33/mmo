# 00 — Making a Dwarf Fortress–style UI intuitive: synthesis

Research 2026-11-02. Question (Dawid): *how do well-received UIs make complex games intuitive,
and how does that apply to a game like Dwarf Fortress?*

Builds on, and does not repeat:
- `../2026-10-20-dwarf-fortress-research/12-ui-and-interaction.md` — DF's own screen layout.
- `../2026-10-29-voxel-selection-construction-ui/` — selection gestures, picking, cutaway,
  overlays/ghosts (the working-level and box-drag decisions there stand).

This set is about *legibility and learnability*: how players find commands, understand state,
learn why things happen, and are not buried as the simulation grows.

| file | covers |
|---|---|
| `01-colony-sims.md` | RimWorld, Oxygen Not Included, Prison Architect, Going Medieval, Timberborn, Against the Storm, Frostpunk 1/2, Songs of Syx, Manor Lords, Stonehearth, Kenshi (contrast) |
| `02-dense-systems-strategy.md` | Paradox nested tooltips (CK3, Vic3, EU V), Factorio, Old World, Civ VI/VII, Cities: Skylines 1/2, Into the Breach, Slay the Spire, Dyson Sphere Program, Shapez |
| `03-df-steam-and-roguelikes.md` | DF Steam redesign process + reception + post-release patches, DFHack / Dwarf Therapist / quickfort as gap evidence, Caves of Qud 1.0, CDDA, Gnomoria, Cogmind, URR |
| `04-ux-principles.md` | Nielsen heuristics, Hodent's *Gamer's Brain*, Hick/Fitts, marking menus, hotkey learning, command palettes, modes, notification tiers, onboarding studies, accessibility guidelines, playtesting |

Source tags: each file numbers its own sources; `[mem]` = from the researcher's knowledge, not
re-checked. Paradox forum dev diaries and a few reviews were only reachable through search
excerpts, and the files mark these.

## 1. The answer in one paragraph

The well-received games are not simpler than DF. They are just as deep, but they **answer
the player's questions where the player is looking**. Those questions are: *what can I do?*
(visible commands with their keys, search), *what is going on?* (an always-visible colony
strip, self-clearing alerts), *why?* (a breakdown behind every number, a reason behind every
refusal and every idle job), *what will happen if I do this?* (exact preview before commit),
and *where is it?* (every row, alert and notice jumps the camera to it, including the level).
DF's own Steam UI was praised for click-to-inspect and tooltips. The lasting complaints are
all about missing answers: hotkeys shown only on hover, no list search for 15 months, and
state the player could not see (unassigned labours, suspended jobs, settings silently reset).
Community tools (DFHack, Dwarf Therapist) exist to fill exactly those gaps.

## 2. Ten principles, in priority order

1. **Every "no" carries its reason, where the player is looking.** Across all four reports,
   the worst complaints are about missing or wrong reasons. Examples: Manor Lords' greyed
   button with no tooltip, RimWorld's silently reserved items, ONI's stale "unreachable",
   Factorio status icons that name the wrong cause, DF's suspended jobs. A wrong reason is
   worse than none. → The reason must be **an enum produced by the same sim code that
   decides**, never a UI-side guess (01 §13, 02 "for us", 03 §7.5).
2. **Explain every number as a ledger.** Paradox's breakdown tooltips (base value plus signed
   modifiers with their sources) and RimWorld's mood thoughts list each contribution with its
   value and timer. Victoria 3 patch 1.2's rule: *information where decisions are made, and
   every outcome with its explanation.* → A sim-side `Breakdown { base, terms }` for every
   derived value. Nested tooltips stop at about 2 levels (02).
3. **Alerts and events are two streams.** Alerts are *live conditions*: recomputed, cleared
   automatically, counted, and clicking cycles through the culprits (RimWorld, Timberborn).
   Events are a *log* (births, deaths, arrivals). Interrupts are reserved for decisions.
   EU V's alert flood (cut in patch 1.1) shows the cost of too many. (01 §12, 02, 04 §notif)
4. **Everything is a link that jumps, and the jump sets the level.** Rows, alerts, notices,
   inspector names, overlay hot spots. In a multi-level fort the jump must set the working
   level, not only the camera (Going Medieval's multi-floor complaint; DFHack made DF's stress
   icons do this). (01, 03 §7.10)
5. **Preview exactly before committing.** Into the Breach: *sacrifice cool ideas for the sake
   of clarity every time.* Slay the Spire went from opt-in previews to always-on intent icons,
   and exact numbers made play more engaging. → Every order drag shows tile count, invalid
   cells with their reason, materials needed vs in stock, and hazards uncovered. The adventure
   path preview (`move_order.rs`) already follows this pattern. (02, 04)
6. **Commands are found by recognition, then learned as hotkeys.** Hotkeys printed beside
   menu items are mostly ignored. Hotkey use roughly doubled (≈29% → 67–73%) only when picking
   from the menu cost something or the key was announced when the command was picked (Grossman
   et al., CHI 2007). Marking menus (Kurtenbach & Buxton): the novice's press-and-wait
   practises the expert's flick, which was ~3.5× faster in their case study (2 users).
   DF Steam's most specific veteran complaint was keys shown only on hover. → One binding
   table drives every surface; a command search (Blender F3 / VS Code style); the key printed
   on every wedge and button. (03, 04)
7. **Make modes visible.** NN/g: show the active mode with at least two cues. The cut, the
   active tool and first person each change what the same keys do, so each needs an indicator
   that stays on screen while active (Timberborn: a hidden cut state made buildings seem to
   "vanish"). (04, and 10-29 research)
8. **Aggregate before per-unit.** Per-colonist screens break past ~20 colonists (RimWorld,
   Frostpunk 2, Kenshi). Dwarf Therapist's whole-fort grid exists because DF was per-dwarf.
   → Colony strip, people × work grid, then per-dwarf sheets as views into these. Every list
   gets search + sort + keyboard (DF took 15 months to add them). (01, 03)
9. **Teach in context, by doing.** In Andersen et al. (CHI 2012, 45k+ players), tutorials
   only helped the *complex* game, where just-in-time help gave +16% play time and +40%
   progress, and locking the player's freedom bought nothing. Deep tooltips and encyclopedias
   do *not* replace a way in: Victoria 3, Old World and EU V all still read as overwhelming.
   (02, 04)
10. **Readability is a budget, not a polish item.** Small text and no UI scaling are the
    commonest complaint even in loved games: Frostpunk 2 and Against the Storm redid their UI
    late, Songs of Syx players asked for scaling in vain, and it was the main complaint in
    DF's Steam Deck review. XAG: ≥ 18 px at 1080p, scalable to 200%. Never use colour alone.
    Remappable keys, pause and game speed are the Game Accessibility Guidelines basics. (01,
    03, 04)

## 3. Where we stand

| area | today | gap against the principles |
|---|---|---|
| Commands | hotkeys G/X/B/T/Z, 1–7, C, PgUp/PgDn, Home, Ctrl+Z, F3/F5/F8/F9, M; hold-Q tool wheel; `[F5]` and `[1–7]` printed on buttons | most keys appear nowhere on screen; wheel wedges lack keys; no command search; wheel is not a flick-before-draw marking menu; no remapping |
| Modes | cut ON/OFF ("Hide above"), tool active, first person, adventure | no persistent mode/level indicator with what Space/Shift do now; no depth gauge relative to the surface |
| Why / refusals | sim validates designations (`tiles.rs` `TileMirror`), path preview red/amber cross, `EmbarkRefusal`, `PlayRefused` | invalid cells are shown red with no reason; embark tiles darken without a reason; refusals go only to the status line |
| Alerts / events | `Notice`s on the game status line; history `LogEntry` | one line, fades, can't be clicked, no log, no condition alerts |
| Inspection | `person_lines` for one selected dwarf, refreshed periodically | no tabs, names aren't links, no breakdowns |
| Overview | minimap, F9 panel | no colony strip, no aggregate lists |
| Time | wall-clock calendar | no pause / speed (open multiplayer question) |
| Readability | `VOXEL_UI_SCALE` env var, pixel-exact at 1× / 2× | not an in-game setting; no colour-blind check of designation tints |

## 4. Recommendations

**Now — cheap, and fixes UI that already exists:**
1. **Key binding table → printed everywhere**: on wheel wedges ("Dig · G"), panel buttons and
   the mode banner. After a pick from the wheel, flash "tip: G" by the cursor for the first
   few uses.
2. **Mode + level indicator**, always on screen: cut ON/OFF, level as a number *and* relative
   to the surface, what Space/Shift/PgUp/PgDn do right now, "Underground" when the backdrop
   swaps. While a tool is active: "Dig — corner 1 set · click corner 2 · Esc cancel".
3. **Refusal reasons at the cursor**: the rule that failed (`bedrock`, `region ring`, `not a
   tree`, …) in the drag readout ("312 tiles, 40 invalid: bedrock") and on hovered embark
   tiles. Invalid cells get a hatch pattern as well as the red tint.
4. **Notices become a clickable log**: the status line stays for the latest one; a scrollable
   log (e.g. key L) where every entry jumps the camera **and level** to its person or site.
5. **Marking-menu wheel**: Q + a flick + release before the wheel draws (~250–350 ms) picks
   the wedge; at most 8 per ring, at most one sub-ring.

**Next — the contracts to set before workers, jobs, needs and stockpiles land:**
6. **`Reason` enums in the sim** for every job that can't be assigned (`Unreachable`,
   `NoWorkerWithLabor`, `MaterialMissing{what}`, `Forbidden`, `Reserved{by}`). Show them on the
   designation hover, in a job's "who could take this, and why not", and in a dwarf's queue.
7. **`Breakdown` values in the sim** for mood, needs, skill rates and stock counts, shown as
   hover ledgers whose lines link to their sources.
8. **Two streams**: a right-edge self-clearing **alert list** (counts, click to cycle through
   culprits) beside the event log. It is useful even before workers exist ("N designations
   unreachable", "no dwarves in fortress").
9. **One Slint list widget** with search, sort, filter and keyboard navigation built in, and
   no other list widget. Then a **colony strip** (portrait, status icon, click to select,
   second click to jump), and a **people × labour grid** designed *before* per-dwarf labour
   sheets.
10. **Plan-first building** (DFHack buildingplan model): place now, claim materials later,
    with a visible "waiting for materials" marker. Designation **blueprints / copy-paste**,
    each one undo step.
11. **Overlays tied to the tool**, through the existing per-fragment tint path
    (`fortress_view.rs`, `voxel_pull.wgsl`): dig → reachability, build → materials. They dim
    rather than hide the terrain, and every hot spot is clickable to its cause.

**Later:**
12. A linked **fortress encyclopedia** (made-from / used-in / made-at, back/forward, Alt+click
    on anything), nested term tooltips limited to 2 levels.
13. **Production rates** (made/used per day, possible vs actual), not just stocks.
14. **Command search** (Ctrl+K or `/`) once there are more than ~30 commands, and a hold-F1
    controls overlay for the current mode.
15. **Accessibility pass**: in-game UI scale, key remapping, colour-blind-safe tints, text at
    least 18 px at 1080p.

## 5. Decisions for Dawid

- **Pause in multiplayer.** Every colony sim treats pause as a planning mode: orders are
  accepted while paused, and the game auto-pauses on threats. Our sim is shared, ticks to wall
  time, and drives the history calendar. The options are per-fortress pause (only if a region
  can stop while others run, which affects the history calendar), speed votes, or no pause
  with orders that are cheap to cancel. This decides how much of principle 9's planning-mode
  learning we can have.
- **Onboarding shape.** The reports disagree. 02 recommends a short scripted first fortress
  (CK3, where each step makes you open the screen being taught). 01 and 04 recommend
  contextual concept cards that tick off when you do the action (RimWorld learning helper),
  backed by the CHI 2012 data. Suggested: contextual cards as the default, plus an optional
  guided embark later. Both depend on there being workers to direct.
- **Reference vs discovered.** Ultima Ratio Regum asks which facts are always available in
  lists (stocks, people) and which the player must read off the world (history, culture). The
  person inspector and history notices are the first place this choice shows.
- **UX read with a non-DF player** before the labour and stockpile screens are built. Use the
  script from 04: embark, dig down, designate a room, chop, inspect a dwarf, read a notice.
  DF's reviewer came late and Tarn says he won't repeat that.

## 6. Anti-patterns to avoid (one line each)

- A disabled control with no explanation (Manor Lords).
- A status reason that can be wrong (Factorio icons, ONI "unreachable").
- Information shown only on one unit's sheet (Kenshi): anything that can go wrong for one
  dwarf must also reach the strip or the alert list.
- A wall of alerts (EU V before 1.1).
- An overlay that hides the map (Civ VI appeal lens).
- Hotkeys shown only on hover (DF Steam).
- Every system inventing its own menu style (CDDA).
- Menus inside menus to reach routine actions (DF stockpile Custom settings, Gnomoria).
- Hidden cut state (Timberborn).
- UI scale left until late (Frostpunk 2, Against the Storm).
