# 01 — What makes colony / management sim UIs feel intuitive

Date: 2026-11-02. Scope: the information and control UI of well-received colony and
management sims — inspectors, needs, alerts, priorities, overlays, "why isn't X happening",
stock readouts, time controls, tooltips, onboarding, density, shortcuts. Box-drag and
designation gestures are NOT re-covered here (see
`../2026-10-29-voxel-selection-construction-ui/`), nor Dwarf Fortress's own screens (see
`../2026-10-20-dwarf-fortress-research/12-ui-and-interaction.md`).

Tags: **[src]** = backed by a source in the list at the end (numbers refer to it);
**[mem]** = from my own knowledge of the game, not re-verified this session — treat exact key
bindings and labels as likely but checkable. Review evidence for UI specifically is thin for
some titles (Stonehearth, Songs of Syx, Going Medieval); I say so where it applies.

---

## 1. RimWorld (Ludeon, 2013–) — the genre's reference layout

**Reception.** Paradoxical: the at-a-glance elements (colonist bar, alerts) are widely copied,
yet the UI as a whole is described as the game's most criticised aspect, with newcomers
"spending the first 5 to 10 hours mostly reading popup bubbles and menu trial-error" [src 3].
A design blog praises the colonist bar but calls commands buried under several menu layers
"one of the more annoying examples of UI design" [src 1]. Tynan Sylvester has said he could
not get into Dwarf Fortress because its interface was "too brutally difficult", which is part
of why RimWorld's is simpler [src 3]. The huge UI-mod scene is itself a signal of gaps:
RimHUD (richer inspect pane) [src 4], Fluffy's Work Tab and Numbers (custom tables),
Achtung, Dubs Mint Menus [mem].

**Mechanisms.**
- **Colonist bar** (top centre): one portrait per colonist, grouped by map/caravan. The
  portrait background darkens as mood drops, worn apparel is drawn on the portrait, and icons
  mark asleep / downed / mental break / drafted. Click selects, click again jumps the camera;
  right click opens the world map at a caravan [src 1, 2]. The one-row "health of the whole
  colony" summary is the praised part — you never open a per-colonist screen to know who is
  in trouble [src 1].
- **Alerts** (right edge, a vertical list): STATE, not events — each alert exists while its
  condition holds and vanishes when fixed ("Colonist needs rescue", "Major break risk",
  "Starvation", "Idle colonist", "Need warm clothes", "Low food", "Hunter lacks ranged
  weapon"). Red = critical, plain = minor. Hovering shows an explanation tooltip listing the
  culprits and highlights them on the map; clicking cycles the camera through the culprits
  [src 2, 5 partial; click-cycling detail mem].
- **Letters** (right edge, above the time controls): EVENTS — raids, trade ships, deaths,
  quests — as colour-coded envelopes (red threat, yellow neutral, blue good). Click opens a
  dialog with "Jump to location"; right click dismisses. Threat letters can auto-pause the
  game (configurable) [mem].
- **Messages**: transient lines top-left ("X cannot reach Y", "Construction failed")
  that fade; clickable to jump [mem].
- **Inspect pane** (bottom left): selected thing's summary; for a pawn, tabs Needs / Health /
  Gear / Social / Character / Log. The Needs tab shows a mood bar with the mental-break
  thresholds drawn as tick marks on it, plus the list of **thoughts** each with its signed
  mood value and remaining duration — the "why is she unhappy" answer is a ledger, not a
  number [mem]. Multi-select of one type shows the count [src 2]. With nothing selected,
  the bottom-left shows **cell info** under the cursor (roof, floor, filth, light) [src 2].
- **Float menu** (right click a target with a pawn selected): every action the pawn could do
  on it, with impossible ones listed greyed and the reason in parentheses ("Cannot haul: no
  storage", "Cannot reach", "Reserved by Ann", "Incapable of"). Players are told to force a
  job via this menu precisely because it is the place the game prints the refusal reason
  [src 6 for the advice; reason-format mem]. Common silent blockers: forbidden items (small
  red X on the item, auto-set on items dropped in fights), reservations, no accessible
  storage, allowed area [src 6, 7].
- **Work tab** (F1): colonist × work-type grid. Standard mode = checkboxes, left column beats
  right; manual mode = numbers 1–4 per cell, blank = never. Within a priority, leftmost
  first. Cells are outlined by skill colour (red → white → yellow), passion is shown as one or
  two flames, hovering gives the exact skill, an incapable colonist has no box at all, and
  assigning a low-skill colonist plays a "crunch" warning sound [src 7]. Pitfall the wiki
  itself warns about: Hauling at 1 makes a colonist haul everything first [src 7].
- **Schedule tab**: 24-hour strip per colonist painted with Anything / Work / Joy / Sleep
  (drag-paint, like a calendar) [mem]. **Assign tab**: per-colonist dropdowns for outfit,
  food, drug policies and allowed area; the policies themselves are reusable named filters
  [mem].
- **Resource readout** (top left): items in storage, collapsible by category (a play-settings
  toggle) [src 2].
- **Time controls** (bottom right): pause / 1× / 3× / 5×, Space and 1–3 [src 2]. Orders can be
  given while paused — the core of the genre's "plan, then unpause" rhythm [mem].
- **Info card** ("i" on anything): every stat with a breakdown — base value × each factor
  (skill, quality, health, bionics) — so numbers are explainable but one click deep [mem].
- **Learning helper** (top right): concept cards that appear when relevant ("you have a
  stockpile — here is how filters work"), replacing older yellow pop-ups; can be disabled and
  auto-hides when empty [src 2]. Some concepts tick off only when you DO the thing [mem].

**Criticised.** Commands and stats buried a click or two deep [src 1]; the info card is
undiscoverable [mem]; first hours are "reading popups" [src 3]; the work grid does not scale
past ~20 colonists without mods [mem]; silent job refusals (forbidden / reserved) that only
the float menu explains [src 6].

---

## 2. Oxygen Not Included (Klei, 2017–) — overlays and errand transparency

**Reception.** Overlays are the most consistently praised element ("let you see how
temperature or oxygen levels vary across the map", "per-block insight") [src 9]. Complaints
are about polish rather than structure: clipped pop-ups, forced scrollbars, "scrolling is
needed for nearly everything", tiny expand buttons, near-empty categories; one player: the UI
"offers compatibility with all, but optimization for none" (2020, later "improved") [src 10].
Text legibility on Steam Deck is a recurring theme [src 9].

**Mechanisms.**
- **Overlays**: F1 Oxygen, F2 Power, F3 Temperature, F4 Materials, F5 Light, F6 Plumbing,
  F7 Ventilation, F8 Decor, F9 Germs, F10 Farming, F11 Rooms, Shift+F1–F4 Exosuit /
  Automation / Conveyor / Radiation, P Priority. Some unlock with research [src 8]. Each
  overlay recolours the whole world and dims irrelevant buildings, and a legend panel on the
  right explains the colour scale and filters [mem]. Tool modes open their overlay
  automatically — Disinfect opens Germs, Harvest opens Farming [src 8] — so the overlay is
  tied to the verb, not something you must remember to toggle.
- **Tile hover tooltip**: element, mass, temperature, germs of the cell under the cursor —
  the world is fully inspectable without clicking [mem].
- **Two-axis priorities**: every errand carries a 1–9 priority (plus a red "!!" emergency
  tier) set by a priority brush; every duplicant has a per-category personal priority
  (Disabled … Very High) in the Priorities screen. A dupe picks by its personal category
  first, then errand priority, then distance [mem; tab description src 11]. Players request
  priority numbers drawn on top of every overlay [src 12].
- **Errands tab — the "why" panel**: select a dupe to see its queued errands in the order it
  will do them; select a building to see which dupes would take its errand, in what order, and
  why the others will not ("you'll get a more detailed description of why dupes aren't doing
  it") [src 11, 13].
- **Status items**: a building or designation shows red / yellow status lines in its tooltip
  and an icon in the world — "Missing materials", "Unreachable", "No power", "Pipe blocked"
  [mem]. Weak spots players report: "Unreachable dig" is stale after the path opens, or
  misleading when only some dupes can dig the material [src 13].
- **Notifications** (left edge, stacked, colour-coded): "X is suffocating", "Scalding",
  "Idle duplicants"; click to cycle the camera to them [mem; a fixed bug about click-jump
  going to the world corner confirms the behaviour, src 13]. The "unreachable" notification
  is often a false positive that players learn to ignore [src 13] — alarm fatigue.
- **Colony diagnostics + pinned resources** (top right): rows for Breathability, Food,
  Stress, Toilets, Beds, Idle, each green/yellow/red with a tooltip naming the failing
  criterion and a small trend graph; the resources panel lets you pin any item so its count
  stays on screen [mem].
- **Vitals / Consumables / Schedule screens**: tables of all dupes × needs (stress,
  calories, bladder), allowed foods, and shift schedules [mem].
- **Onboarding**: contextual tutorial messages tied to early milestones (build a toilet, a
  bed, an oxygen source) and the in-game Database (codex) with an entry per element, building
  and critter [mem].

**Takeaway.** ONI's "why" is answered at both ends — from the worker (its errand queue) and
from the job (who would take it and who will not) — and its overlays are bound to tools.

---

## 3. Prison Architect (Introversion, 2015)

**Reception.** Game Informer's launch review rated playability low: the interface is
"complicated and cumbersome" although it packs a lot of information [src 14]. The console
port removed the skeuomorphic clipboard look and split screens into tabs; the tablet port was
a full touch-first overhaul [src 14].

**Mechanisms.**
- **Reports** (one window, tabs Staff / Prisoners / Regime / Valuation / Jobs / …): the
  Needs report is a list of bar graphs of aggregate prisoner needs (food, hygiene, freedom,
  family, comfort…) so the warden sees which need drives unrest across the whole prison
  [src 14]. The Jobs tab lists active and queued work — the job queue is visible [src 14].
- **Per-prisoner inspector**: the same needs as bars, red where unmet, plus traits and
  reputation; thought bubbles above heads show what an inmate currently wants [mem].
- **Grants as onboarding**: early "grants" are paid objectives ("build a holding cell, a
  shower, a kitchen") that double as a checklist tutorial; a scripted story campaign teaches
  the rest [mem].
- **Overlays**: Logistics (drag lines from deliveries to rooms), Utilities, CCTV, and the
  Danger / Temperature / Water views [mem].
- **Planning tool**: paint a ghost layout before paying for it [mem] (designation-side, not
  re-covered).

**Takeaway.** Aggregate-need bars (which need, how many people, how bad) are what a manager
of many people actually needs; per-person sheets are second.

---

## 4. Going Medieval (Foxy Voxel, 2021–) — the closest 3D z-level analogue

**Reception.** Split. "Refreshingly straightforward… mostly stays out of your way" [src 15]
vs "dense, layered, somewhat cluttered" [src 16] and "UI / visibility can get fiddly once
you've built a complex multi-floor fortress" [src 17]. Player reviews single out task
prioritisation and the camera as clunky, and settlers staging goods in intermediate
stockpiles [src 18]. "Some things… aren't explained well enough" [src 19].

**Mechanisms.**
- **Settler priorities grid**: settler × job, numeric priority per cell; star markers show
  how much a settler LIKES a job (preferred jobs raise mood) — skill and preference in the
  same cell, like RimWorld's flames [src 20].
- **Layer view**: a slider / keys to cut the world at a floor, same role as our working level
  [mem]; the "fiddly" complaint is about finding things on hidden floors [src 17].
- **Settler panel** with mood, needs, skills, perks; alerts down the right edge in RimWorld
  style [mem].

**Takeaway.** The genre's problems get worse in 3D: once a fortress has several floors,
"where is the thing that's going wrong" needs a jump-to, not a hunt through levels.

---

## 5. Timberborn (Mechanistry, 2021–)

**Reception.** Generally well liked; UI scaling praised as rare even in big releases
[src 21]; "solid but could use refinement" [src 22]; requests for a simpler Distribution Post
UI and objections when an experimental 1.0 restyle "feels out of character" [src 22]. Its UI
is built on Unity UI Toolkit with game logic separated from UI logic [src 23].

**Mechanisms** (from the main-screen key [src 24]):
- **Inventory bar** (top): science, materials, ingredients, food, water, badwater. Hovering
  expands a breakdown — in storage, in production outputs, unused production inputs — with
  satisfied needs in green and the storage buildings holding each good. Inputs are excluded
  from totals (no double counting).
- **District focus**: cycle districts; stocks are per district.
- **Well-being**: the average score; click opens the needs list with the number of beavers
  satisfying each.
- **Alerts**: each problem type shows a COUNT in brackets; clicking it cycles through the
  affected objects. Not every problem produces one (e.g. storage with no good selected).
- **Weather forecast**: next week's days coloured temperate / drought / badtide — the threat
  calendar on screen at all times.
- **Journal**: a log of births, deaths, bot builds and breakdowns.
- **View layers**: storage overlay, hide water, max displayed build height (Timberborn's
  cutaway).
- **Toolbar**: tool groups with submenus centred above; Ctrl+wheel cycles groups,
  Shift+wheel cycles tools.
- **Building status icons**: floating icons over a building that lacks workers, input,
  power or a path [mem].

**Takeaway.** Counted, cyclable alerts ("Unconnected buildings (3)") and a forecast strip are
cheap and readable.

---

## 6. Against the Storm (Eremite, 2023) — strongest tooltip/onboarding work, and a UI rework

**Reception.** Reviews: the tutorial teaches the mechanics but "its onboarding for the
interface leaves a lot to be desired" [src 25]; tutorial missions "fit for purpose, if a little
by the numbers" and slow [src 25]; the Hooded Horse wiki added a tutorial walkthrough after
social-media confusion [src 26]; skipping covers only one tutorial city at a time [src 25].
Aggregated Steam complaints include "readability and UI issues" [src 25]. Eremite rebuilt the
UI because it had become "a patchwork of various ideas and styles" with readability
problems new players hit; the first new style drew backlash and a "we hear you" hotfix made
it more minimal [src 27, 28].

**Mechanisms.**
- **Every good is a hyperlink** [mem]: clicking or hovering any resource opens its recipe
  page — which buildings make it, from what, at what star rating, and who consumes it. The
  production chain is discoverable from wherever the resource appears.
- **Resolve per species** [mem]: each species' resolve bar with a tooltip ledger of every
  +/− (needs met, housing, services, hostility) — the RimWorld thought list at group level.
- **Hostility / Impatience bars** at the top [mem]: two pressure meters with thresholds; hover
  lists the sources and the next threshold's effect.
- **Worker overlay on ALT** [src 27]: holding ALT labels every building with its workers,
  species bonuses and glade-event progress bars — a hold-to-see overlay instead of a toggle.
- **Recipes window**: limits per recipe (a text box added after feedback) and bigger
  checkboxes in Orders and Recipes [src 27]; recipe icons reordered so the output aligns for
  scanning [src 28].
- **Alerts** for no fuel, starving villagers, unassigned workers, storm approaching [mem].
- **Onboarding**: a tutorial run of several short settlements, each introducing one system,
  plus an encyclopedia [src 25, 26].

**Takeaway.** Even the genre's best-received recent game needed a full UI rework for
readability, and its tutorial taught the systems but not the screen.

---

## 7. Frostpunk (11 bit, 2018) and Frostpunk 2 (2024)

**Frostpunk 1 — praised.** The **heat overlay** (Tab / view button) turns the city into an
infrared map — reds and yellows around the generator, blues at the edge — "shows the
problem more clearly than the city view alone"; snow on unheated roofs is the in-world cue to
open it [src 29]. The console port made the radial heat rings the basis of a **radial UI**
for build menus [src 30]. Other mechanisms [mem]: top bar resources with a hover projection
(e.g. food expected tomorrow vs consumption), the temperature forecast strip, Hope and
Discontent bars bottom centre (always visible, the two numbers the game is about), event
notices on the right that jump to the building ("sick people lack care", "homeless"), and
the Book of Laws with a cooldown timer.

**Frostpunk 2 — criticised.** Reviewers: "a labyrinth of menus and sub-menus", "clunky and
crowded", small text, hard-to-tell-apart resource icons, no visible district boundaries
unless a specific menu is open [src 31]. Before launch 11 bit delayed the game partly to
make the HUD "clearer and more intuitive", add a new construction menu and revamp the Idea
Tree's readability: "The game is challenging enough already" [src 32]. Players also objected
to the lighter palette [src 33]. The 2025 console redesign (radial build menus, quick
district cycling) "helps a lot" [src 31].

**Takeaway.** Frostpunk 1 wins with FEW always-visible numbers (hope, discontent, cold,
forecast) and one great overlay; Frostpunk 2 lost that when scale grew and information moved
into sub-menus.

---

## 8. Songs of Syx (Gamatron, 2020–) — scale vs readability

**Reception (thin evidence).** Mixed: one review calls the UI readable — logistics, paths
and construction priorities without cluttered menus [src 34]; aggregated player reviews list
"steep learning curve with poor tutorials" and UI issues [src 34]. Players asking for larger
fonts got "the best I've got": lower the game's resolution [src 35] — no UI scale.

**Mechanisms** [mem]: dense stat pages where every number has a hover breakdown (base,
modifiers by source); population and production tallied per species and profession;
overlays for paths, logistics and room quality; a one-person UI where depth beats polish.

**Takeaway.** Breakdown tooltips make density survivable; no UI scaling and no tutorial make
it inaccessible.

---

## 9. Manor Lords (Slavic Magic, 2024 EA) — the "information opacity" case

**Reception.** Praised: "contextual", fertility and water overlays "immediately useful and
easy to read" [src 36]. Criticised as OPACITY, not overload [src 36]: no consumption rate for
any resource but firewood; no decay rate on reputation penalties; undescribed policies;
an empty Production screen. A long player write-up lists: a greyed-out upgrade button with
"No error message. No warning. No tooltip."; a trading post silently set to 0; most workers
idle for want of tools with no alert; a logging camp with no trees in range and no message;
"these crises sneak up silently" [src 37]. A r/ManorLords thread: the new-player complaint
is weak documentation and alerts [src 38].

**Proposed fixes from players** [src 37]: idle-building messages with the cause ("no trees
in range"), a requirement CHECKLIST in place of a greyed button (each item met / missing),
warnings for a zero import target, wildlife zones shown red during placement, alerts when
workers idle or ore / fuel / food / tools hit zero.

**Takeaway.** Beautiful contextual UI does not make up for missing rates and missing
"blocked because". Every disabled control needs its reason.

---

## 10. Stonehearth (Radiant, 2013–2018) — limited evidence

**Mechanisms.** Top-bar buttons with rebindable shortcuts (C = Citizens); the Citizens
panel shows each hearthling's job and checkboxes for which task types it may perform;
clicking a name gives a **Promote** button (promotion needs a job talisman item), then a
shortcut to that crafter's workshop so you never hunt for it on the map [src 39]. Alpha 4
overhauled management into a central scoreboard plus citizen and crafter screens [src 39].
Early reviews reported UI error spam, later fixed [src 39]. Building editor and crafting
queues were frequent complaints [mem].

**Takeaway.** "Open the workshop from the person who works there" — navigation by
relationship, not by map search.

---

## 11. Kenshi (Lo-Fi, 2018) — contrast case

**Reception.** Loved despite its UI. PC Gamer 84/100: "Work through the presentational
ugliness and technical awkwardness, and you'll find an experience of frightening depth"
[src 40]; the UI "can get cumbersome" as your squad grows [src 40]; others call it "a wall of
text and tiny icons", "terrible UI", "read a walkthrough because the UI is not very
intuitive" [src 40]; GOG / RPG-site players found it clunky and slow, especially movement and
inventory [src 41]. Many replacement-UI mods exist [src 41].

**Mechanisms that hurt** [mem]: no colony-wide view of who is doing what (jobs are set per
character with a small job list and no explanation when a job silently fails); grid
inventories per character; status buried in per-character windows; few alerts — you
discover a starving or kidnapped squad member by looking.

**Takeaway.** A game can succeed with a bad UI if its world is compelling, but players then
pay with guides and mods; what Kenshi lacks (colony summary, job transparency, alerts) is
exactly what the well-received games have.

---

## 12. Patterns across games

1. **A colony-wide health strip that is always on screen.** RimWorld's colonist bar
   (portraits shaded by mood, status icons) [src 1], ONI's colony diagnostics [mem],
   Frostpunk's hope/discontent bars [mem], Timberborn's well-being average [src 24], AtS
   resolve per species [mem]. You learn "is anything wrong?" without opening anything.
2. **Alerts are conditions, not log lines.** RimWorld / Timberborn alerts exist while the
   condition holds and clear themselves [src 2, 24]; events (raids, births) are a separate
   stream (letters, journal). Both are clickable: click cycles the camera through the
   culprits; Timberborn shows a count [src 24].
3. **Every "no" has a reason, shown where the player is looking.** RimWorld's float menu
   ("Cannot haul: reserved by Ann") [mem, src 6], ONI's errand tab and status items
   [src 11, 13]. The biggest complaints in the set (Manor Lords' greyed buttons [src 37],
   RimWorld's silent forbidden items [src 6], ONI's stale "unreachable" [src 13]) are all
   missing or wrong reasons.
4. **Ledgers, not scores.** Mood = list of thoughts with signed values and timers (RimWorld,
   AtS resolve); stats = base × factors (RimWorld info card, Songs of Syx tooltips). A bare
   number invites "why?" questions the player cannot answer.
5. **Overlays bound to the verb.** ONI opens Germs when you pick Disinfect [src 8]; AtS's
   worker overlay is hold-ALT [src 27]; Frostpunk's heat map has an in-world cue (snow
   on roofs) [src 29]. Overlays recolour the world and dim the irrelevant, with a legend.
6. **Priorities as a grid of people × work, with skill and liking in each cell.** RimWorld
   (skill colour, passion flames, 1–4, blank = never) [src 7], Going Medieval (stars for
   preference) [src 20], ONI (personal priority per category + errand priority 1–9)
   [mem]. Common pitfall: one overloaded category (Hauling at 1) [src 7].
7. **Pause is a planning mode.** Every title lets you give orders while paused; RimWorld
   auto-pauses on threats (configurable) [mem]. Speed keys are Space + 1/2/3 nearly
   everywhere [src 2, mem].
8. **Stock readouts with provenance.** Timberborn's hover splits stock / outputs / unused
   inputs and names storages [src 24]; RimWorld's collapsible category list [src 2]; ONI pin
   any item [mem]; Frostpunk projects tomorrow's balance [mem]. Manor Lords is criticised for
   lacking rates [src 36].
9. **Hyperlinked information.** AtS goods link to their recipes [mem]; Stonehearth's
   citizen → workshop shortcut [src 39]; RimWorld's alert → culprit, letter → location
   [src 2, mem]. Players navigate by relationship, not by panning the map.
10. **Onboarding that is contextual and skippable.** RimWorld's learning helper (concept
    cards when relevant, ticked by doing) [src 2]; ONI milestone hints [mem]; Prison
    Architect's grants as a paid checklist [mem]. AtS shows the cost of teaching systems but
    not the screen [src 25].
11. **Readability failures are the commonest complaint even in loved games**: small text,
    no UI scaling (Songs of Syx [src 35], Frostpunk 2 [src 31], ONI on Deck [src 9]),
    similar icons [src 31], menus inside menus [src 1, 31]. AtS and Frostpunk 2 both did
    late UI reworks for this [src 27, 32].
12. **Scale breaks per-entity screens.** RimWorld's grid at 20+ colonists [mem], Frostpunk
    2's districts [src 31], Kenshi's squads [src 40], Going Medieval's floors [src 17] —
    the fix is aggregate views (Prison Architect needs report [src 14], Timberborn counts
    [src 24]) and jump-to.

---

## 13. For us (DF-like fortress game, Slint UI, commander camera, working level)

What we have: commander camera with a working level and cutaway, designation tools + tool
wheel (Q), F9 Fortress panel, minimap, world map with embark, event notices on the status
line, a one-dwarf inspector (`person_lines`), the F3 dev panel. Not built: workers, jobs,
stockpiles, needs, military.

- **Two streams from day one: conditions and events.** Keep the existing notices as the
  EVENT stream (births, deaths, arrivals — `history` `LogEntry`s; click jumps to the person
  or site). Add a separate right-edge ALERT list of live conditions, each recomputed from sim
  state, clearing itself, with a count and click-to-cycle through culprits. Even before
  workers exist it can carry "N designations unreachable", "N designations invalid", "no
  dwarves in fortress".
- **Click-to-jump must include the LEVEL.** Every jump (alert, notice, inspector link,
  minimap) moves the camera AND sets the working level to the target's level — the
  Going Medieval multi-floor complaint [src 17] is our default case. When the target is
  hidden by the cut, jump to its level rather than drawing it through rock.
- **Design jobs with a refusal reason as part of the data.** When workers land, every job
  the sim cannot assign should carry an enum reason (`Unreachable`, `NoSkilledWorker`,
  `MaterialMissing { what }`, `Forbidden`, `Reserved { by }`) produced by the same code that
  decides, so the UI never guesses (ONI's stale "unreachable" [src 13] is what guessing
  looks like). Show it in three places: the designation's hover, the job's panel ("who could
  take this, and why not", ONI errands [src 11]) and the dwarf's panel (its queue in order).
  The pathfinder already distinguishes "no path" from "search budget hit" in the move
  preview (red vs amber cross) — keep that distinction in job reasons too.
- **No unexplained disabled control.** Every greyed button / refused designation gets a
  tooltip or a met / missing checklist (Manor Lords [src 37]). We already validate
  designations against `TileMirror`; surface the rule that failed, not just red.
- **A colony strip.** One always-visible row: dwarves as small portraits with a mood shade
  and status icons (asleep, idle, hurt), click selects, second click jumps (RimWorld
  [src 1]). Before needs exist it can show idle / walking / in-region state.
- **Inspector as ledgers.** Extend `person_lines` into tabs (Overview, Needs, Thoughts,
  Skills, Relations — relations are already in `PersonInfo`); every number gets a hover
  breakdown; names are links (spouse → that dwarf, site → map) (AtS / Stonehearth
  navigation by relationship [mem, src 39]).
- **Labour grid when workers land.** Dwarves × labour categories; cell = priority (blank =
  never), outlined by skill, marked by liking; column header click sorts; one-key toggle to
  "simple checkbox mode" (RimWorld [src 7]). Plan for 50+ dwarves: group by squad/
  profession, and a per-labour "how many dwarves enabled" footer to spot gaps.
- **Overlays bound to tools, drawn through the existing fragment path.** We already tint
  designated cells (`fortress_view.rs`) and draw the region perimeter in `voxel_pull.wgsl`;
  the same per-fragment tint can carry overlays: designation priority numbers (ONI's
  requested "priorities on top of the overlay" [src 12]), reachability from the fortress
  (unreachable cells red), later stockpile zones, traffic, temperature. Picking a tool turns
  its overlay on (dig → reachability), and holding a key (Alt) shows the worker overlay
  (who is assigned where) like AtS [src 27].
- **Stock readout with rates.** When stockpiles land: top-bar counts for a few pinned goods
  with hover → in stockpiles / carried / in workshops and a trend (± per day, from the
  `CalendarTick` calendar). Manor Lords' missing rates are the anti-example [src 36].
- **Pause as planning mode.** Space pause, 1/2/3 speed, designations and orders allowed while
  paused (the sim must accept orders at a paused tick), optional auto-pause on threats and on
  first contact. A big "PAUSED" banner. This collides with multiplayer — decide whether pause
  is per-fortress or not before building it.
- **Onboarding by contextual concept cards**, not a scripted tutorial: a card appears the
  first time a concept is relevant (first embark: "the working level"; first dig: "dig
  designations and priority"; first refusal), ticks off when the player does it, and can be
  disabled (RimWorld learning helper [src 2]). Keep a codex entry per concept for later.
- **Readability budget up front.** We already have `VOXEL_UI_SCALE` (pixel-exact at 1 and 2);
  keep every screen usable at 1080p×1 and 4K×2 and test the smallest text against the
  fortress theme's umber palette. Frostpunk 2 and AtS both paid for late reworks [src 27, 32];
  Songs of Syx players asked in vain for scaling [src 35].
- **Keyboard parity.** One letter per tool (we have G / X / B / T / Z), F-keys or number row
  for screens (F9 fortress is a start), Space/1/2/3 time, Tab cycles alerts or idle dwarves,
  and every key shown in its tooltip. Keep the tool wheel for discovery, keys for speed.
- **Avoid Kenshi's traps.** No per-dwarf-only status: anything that can go wrong for one
  dwarf (stuck, starving, missing) must also surface in the colony strip or the alert list.

---

## Sources

1. Moonlit Development, "The Designer's Folder: RimWorld's at-a-glance UI elements" (2017) — https://moonlitdevelopment.squarespace.com/development-blog/2017/12/25/the-designers-folder-rimworlds-at-a-glance-user-interface-elements
2. RimWorld Wiki, "User interface" — https://rimworldwiki.com/wiki/User_interface
3. Search summaries of RimWorld reviews / Sylvester interviews: gamegeeker review https://gamegeeker.com/de/games/rimworld-294100/review ; GameSkinny interview https://www.gameskinny.com/news/bioshock-infinite-dev-talks-about-his-kickstarter-rimworld-in-this-exclusive-interview/ ; PC Gamer 1.0 article https://pcgamer.com/rimworld-creator-says-next-version-will-be-10-outlines-what-to-expect
4. RimHUD (mod) — https://www.github.com/Jaxe-Dev/RimHUD ; UberMicro (mod) — https://github.com/maarxx/UberMicro
5. RimWorld Steam discussion on UI — https://steamcommunity.com/app/294100/discussions/0/600779497610937366
6. RimWorld Steam discussions on colonists refusing to haul (forbidden, reserved, "No accessible storage") — https://steamcommunity.com/app/294100/discussions/0/366298942111489750 ; https://steamcommunity.com/app/294100/discussions/0/1744478429671201119 ; Ludeon forum https://ludeon.com/forums/index.php?msg=307822
7. RimWorld Wiki, "Work" — https://rimworldwiki.com/wiki/Work
8. Oxygen Not Included Wiki, "Overlay" — https://oxygennotincluded.wiki.gg/wiki/Overlay
9. ONI reviews: Gamereactor https://msn.gamereactor.eu/oxygen-not-included-review/ ; oprainfall https://operationrainfall.com/?p=240763 ; PC Gamer https://pcgamer.com/oxygen-not-included-is-a-deep-and-complex-sim-thats-also-easy-to-jump-into ; VaporLens summary https://vaporlens.app/app/457140/oxygen_not_included
10. ONI Steam discussion "UI sloppy" — https://steamcommunity.com/app/457140/discussions/2/1742267854809779347
11. ONI tabs/priorities guide (Gamepressure) — https://www.gamepressure.com/oxygen-not-included/tabsoverlays/z59cfc ; ONI Steam discussion on errands — https://steamcommunity.com/app/457140/discussions/0/3714937078841422039
12. Klei forums, "Overlay and interface feedback" (2018) — https://forums.kleientertainment.com/forums/topic/88446-overlay-and-interface-feedback/
13. ONI errands / unreachable discussions and bug tracker — https://steamcommunity.com/app/457140/discussions/0/1735508198293649845 ; https://forums.kleientertainment.com/forums/topic/169915-unreachable-build-is-not-displayed-as-unreachable-why/ ; https://oxygennotincluded.wiki.gg/wiki/Versions/R1-261394 ; https://kleiforums.com/forums/topic/98523-notify-the-player-if-a-duplicant-is-stuck/
14. Prison Architect: Game Informer review https://www.gameinformer.com/games/prison_architect/b/pc/archive/2015/10/06/prison-architect-review-game-informer.aspx ; Reports tab https://gamepressure.com/prisonarchitect/reports/zb51e0 ; mobile review https://www.touchtapplay.com/?p=17593 ; Tag Games tablet port https://tag-games.com/blog/tag-brings-prison-architect-to-tablet ; Xbox review https://www.thexboxhub.com/prison-architect-review/
15. Going Medieval, theGeek review — https://thegeek.games/?p=126332
16. Going Medieval, Virus.hr — https://virus.hr/en/news/going-medieval-rimworld-in-3d-but-finally-distinct-enough
17. Going Medieval, GameForce review — https://gameforce-czy.pages.dev/reviews/going-medieval
18. Going Medieval, Game Brain player-review summary — https://gamebrain.co/game/going-medieval
19. Going Medieval, Gaming Trend first impressions — https://gamingtrend.com/feature/impressions/i-lived-through-the-black-death-only-to-get-eaten-by-wolves-going-medieval-first-impressions
20. Going Medieval settlers guide (eip.gg) — https://eip.gg/going-medieval/guides/settlers/
21. GamingOnLinux on Timberborn — https://www.gamingonlinux.com/2023/07/goodbye-free-time-i-have-discovered-timberborn/page=1/
22. Timberborn GOG review https://www.gog.com/u/Felfyir/reviews ; feature-upvote board https://timberborn.featureupvote.com/?tag=UI ; https://timberborn.featureupvote.com/suggestions/690153/10-experimental-updated-ui-feels-out-of-character
23. Unity case study, "How Timberborn's complex runtime UI was built" — https://unity.com/en/resources/timberborn
24. Timberborn Wiki, main screen / alerts — https://timberborn.wiki.gg/wiki/Alert
25. Against the Storm reviews / complaints: Digitally Downloaded https://www.digitallydownloaded.net/?p=69346 ; inkl/TechRadar https://www.inkl.com/news/against-the-storm-review-here-be-monsters ; Steam skip-tutorial thread https://steamcommunity.com/app/1336490/discussions/2/3758852053574220100 ; VaporLens https://vaporlens.app/app/1336490/against_the_storm/stats/analysis
26. Hooded Horse wiki, Tutorial Walkthrough — https://hoodedhorse.com/wiki/Against_the_Storm/Tutorial_Walkthrough
27. Eremite, "Interface hotfix — we hear you" — https://eremitegames.com/interface-hotfix-we-hear-you/ ; QoL update coverage https://www.player.one/against-storm-quality-life-update-has-ton-noteworthy-features-156377
28. Eremite devlog, "Updated UI preview" — https://eremitegames.com/?p=236833
29. Frostpunk heat overlay: Windows Central https://windowscentral.com/best-frostpunk-xbox-one-review ; Checkpoint Gaming https://checkpointgaming.net/reviews/2018/04/frost-punk-review-not-winter-wonderland/ ; Pixel Twelve guide https://pixeltwelve.com/guides/frostpunk-beginner-guide-workers-heat-map-screen
30. Xbox Wire, "Frostpunk Console Edition: radial-driven design" — https://news.xbox.com/en-us/2019/09/20/frostpunk-console-edition-radial-driven-design-of-gameplay-and-controls/
31. Frostpunk 2 reviews: GeekChamp https://geekchamp.com/?p=220249 ; Thumb Culture https://www.thumbculture.co.uk/frostpunk-2-pc-review ; COGconnected https://cogconnected.com/?p=372715 ; Playday https://playday.one/2025/09/17/frostpunk-2-xbox-review/ ; Xbox Wire gamepad https://news.xbox.com/en-us/2025/09/18/adapting-frostpunk-2s-depth-to-a-gamepad/
32. PCGamesN, Frostpunk 2 UI improvements — https://www.pcgamesn.com/frostpunk-2/ui-improvements ; GamesRadar https://gamesradar.com/games/city-builder/the-game-is-challenging-enough-already-frostpunk-2-devs-reveal-user-friendly-changes-in-the-works-since-the-survival-city-builders-delay
33. Frostpunk 2 Steam discussion (dark mode) — https://steamcommunity.com/app/1601580/discussions/0/7845908683579905746
34. Songs of Syx: VaporLens https://vaporlens.app/app/1162750/songs_of_syx/stats/analysis ; Reality Remake https://www.realityremake.com/articles/tag/Songs+of+Syx ; Game Brain https://gamebrain.co/game/songs-of-syx-2020
35. Songs of Syx Steam discussion, UI scaling — https://steamcommunity.com/app/1162750/discussions/0/5755024051045850056
36. Gameluster, Manor Lords early access review — https://gameluster.com/manor-lords-early-access-review-hamfisted-hochadel/
37. "Surviving Manor Lords: a player's journey" (2026) — https://epic-fantasy-books.blogspot.com/2026/02/surviving-manor-lords-players-journey.html
38. r/ManorLords, "Simple and necessary improvements" — https://r.darklab.sh/r/ManorLords/comments/1kltit8/simple_and_necessary_improvements/ms5d4ns/?context=3 ; OpenCritic https://opencritic.com/game/16697/manor-lords
39. Stonehearth: wiki https://stonehearth.gamepedia.com/File:Bulletins.jpg ; Alpha 4 notes https://stonehearth.gamepedia.com/Alpha_4 ; modding guide (jobs) https://stonehearth.github.io/modding_guide/modding_guide/intermediate/jobs/index.html ; Qt3 forum https://forum.quartertothree.com/t/stonehearth-a-df-like-town-builder/72506?page=9 ; Steam page/reviews https://store.steampowered.com/app/253250/Stonehearth/
40. Kenshi: PC Gamer review https://pcgamer.com/kenshi-review ; Wikipedia https://www.wikipedia.org/wiki/Kenshi_(video_game) ; OpenCritic https://opencritic.com/game/7007/view/reviews ; Dafy review https://dafy.site/kenshi-review/
41. Kenshi player impressions: GOG forum https://www.gog.com/forum/kenshi/impressions ; RPG Site https://therpgsite.com/other-games/kenshi-anyone-here-playing-it ; Dark Vanilla UI mod https://catalogue.smods.ru/archives/51538
