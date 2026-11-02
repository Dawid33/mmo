# 02 — How dense-systems games stay legible

Research for the fortress UI (Slint, commander camera). Question: how do well-received games
with deep, interlocking systems make them understandable without walls of menus? Per game:
the concrete mechanism, why it is praised, what is criticised. Then cross-game patterns, then
what that means for a Dwarf Fortress–style game.

Tags: `[sN]` = numbered source at the end (fetched or seen in search results on 2026-11-02);
`[mem]` = from prior knowledge, not re-verified today — treat as likely but check before quoting.

---

## 1. Paradox: Crusader Kings 3, Victoria 3, Europa Universalis V

### Mechanism

- **Nested tooltips.** Any highlighted term inside a tooltip opens its own tooltip on hover;
  that one can contain highlighted terms too. Players stack tooltips to build understanding
  of a concept without leaving the screen they are on [s1][s3]. Victoria 3 adopted it from CK3
  and covers core concepts ("pops", "dividend taxes", "market price") this way; Paradox's
  stated aim is "to allow you to learn even the most advanced concepts the game is based on
  as you play" [s6].
- **Lock modes** (a settings choice, because moving the cursor onto a tooltip normally closes
  it) [s3][s2]:
  - *Timer lock* (default): the tooltip freezes after ~1–2 s so the cursor can travel into it.
  - *Action lock*: middle-click locks/unlocks; experts pick it to avoid tooltip clutter.
  - *Mouse tendency*: stays while the cursor keeps moving toward it.
  Delays/lock times are sliders.
- **Hover-path geometry matters.** Victoria 3 patch 1.2 re-laid-out map interaction panels so
  the player can move toward a nested tooltip "without accidentally opening another
  tooltip" [s5]. (Classic menu-aim problem.)
- **Explain-the-number breakdowns.** Every derived value's tooltip lists base + each modifier
  with its source; Vic3 1.2 added battle breakdowns (troops each general brought), and
  "diagnostic" tooltips — e.g. *why* a state is not fully connected to the national market,
  *why* pops turn radical/loyal and which interest groups they belong to [s5]. Players
  explicitly asked for the additive "base value then modifiers" format [s7].
- **"Why can't I do this."** Vic3 1.2 started showing *all* diplomatic plays, not only the
  startable ones, so the player sees what blocks each one; AI acceptance is shown with the
  terms that would change it [s5]. CK3 character interactions are listed even when
  unavailable, greyed, with the failed conditions in red in the tooltip, and the AI's
  acceptance is a signed list of reasons ("+20 Opinion of you", "−50 Won't betray liege")
  [mem].
- **Information hierarchy inside tooltips.** Pop-need tooltips were reworked to "surface more
  of the relevant information higher up in the information hierarchy" [s5]. Paradox UX
  designers note the plain tooltip is "basically only a long text string"; richer content
  goes into a panel-style "cooltip" with real layout [s7].
- **Encyclopedia / game concepts.** CK3's encyclopedia collects concepts, mechanics and the
  whole tutorial lesson set, and uses the same highlighted links [s3]. A previewer solved a
  real succession puzzle there ("Tanistry, assigned on a per-title basis, takes precedence
  over other realm succession laws") [s3]. Coverage is clear it is a reference, not a
  way to learn the game up front [s4].
- **Tutorial + reactive tips.** CK3's tutorial is a small, forgiving start (Murchad of Munster,
  "Noob Island") whose windows force the player to *open* the screens being taught; reactive
  tips continue during play and can be reset [s3].
- **Issues / suggestions widget (CK3).** A top-bar list of situational advice ("a powerful
  vassal dislikes you", "you can create a title", "you hold too many duchies", "ransom
  prisoners"); each item dismissible with right-click, one button restores them [s3].
- **Notification triage (Vic3 1.2).** Per-notification display style and auto-pause in a
  Message Settings window; Paradox cut notifications by about half for most countries, moved
  low-value ones off by default, and only fires diplomacy/resource alerts that concern the
  player's own commitments [s5].
- **Map modes / heatmaps.** Literacy, population, infamy map modes; a "Practical Heatmaps" mod
  adopted into the base game; configurable fade between modes [s5].
- **Outliner with pins.** Pinnable goods and characters, approval values beside interest
  groups, available labour beside states [s5].
- **Actionable links.** Map-interaction panels link straight to the building details panel;
  Paradox's stated principle: information should appear where decisions are made, and
  explanations should accompany outcomes [s5].

### Praise

- Reviewers call CK3's tooltips "the star of the show" for understanding the simulation [s4].
- Vic3 reviewers: "every simulated calculation, keyword, and concept can be expanded using
  nested tooltips"; one said everything needed was in game, no wiki required [s8].
- Nested tooltips spread outside games: a designer wrote them up as a web pattern, noting
  they match how people learn by following supporting concepts [s2].

### Criticism

- **Depth limit.** Infinite chains make no sense; ~2–3 levels is a reasonable working depth;
  small cursor slips close the whole stack [s2].
- **Tooltips are not a tutorial.** Vic3 was criticised for "bad UI design" and being hard for
  newcomers to manage on screen [s7][s8]; Steam threads complain "the game doesn't tell you
  anything" despite thousands of tooltips [s7] — the information exists but the player
  does not know which question to ask.
- **EU V (2025):** "a classic case of information overload" for newcomers [s9]; players
  disliked the nested tooltips and asked for a way to turn them off [s9]; a reviewer noted
  menus don't make clear which elements are information and which are buttons leading
  deeper [s9]. Patch 1.1 targeted "UI chaos": the flood of top-of-screen alerts, repetitive
  orders (move-all by category) and automation [s10].
- Takeaway: nested tooltips answer "what does this mean / where does this number come from",
  but do nothing for "what should I look at now" — that needs alerts/issues, and alerts
  themselves need triage or they become the new wall.

---

## 2. Factorio

### Mechanism

- **Tooltips as structured cards (FFF #318).** Restyled to be "as compact as possible";
  properties grouped into named categories (electricity, fuel…) with a nameless root category
  for the entity's own type; placed entities show live *state* (fuel inside, fluid output);
  irrelevant information removed. Recipe and item info were split — the old recipe tooltip
  was "a Frankenstein's monster of recipe information and item information mashed
  together" — so hovering a recipe shows a recipe card plus one item card per product, and
  "an item tooltip looks the same wherever it appears" [s11].
- **Factoriopedia (FFF #397, 2.0).** Alt+click on almost anything (placed building, item in an
  inventory, recipe) opens its page: ingredients, **"made from"/"used in"** (every recipe
  consuming it), the unlocking technology, alternative recipes, machines that can craft it,
  fuels accepted. Item, recipe and entity merge into one entry when tightly linked. Entries
  are clickable, unlike tooltips. Browser-style **back/forward history** (Alt+←/→, mouse
  buttons) shared with the tech tree, blueprint library, train overview [s12]. Rationale:
  "we already had all the necessary information in the game" — just aggregate it [s12].
- **Search in every list.** Ctrl+F works in inventories, crafting, tech, statistics; when a
  panel had two lists, they rejected per-list boxes as inconsistent and put one search in
  the panel header that filters both [s13].
- **Production statistics.** Per-item production/consumption graphs over selectable time
  windows (5 s … 1000 h) [s14]; 2.0 added per-surface splits with a global toggle, a quality
  breakdown, an accumulator-charge graph ("otherwise players learn of a shortfall only when
  the factory blacks out") and a derived "science" total [s15].
- **Throughput where you decide.** 2.0 crafting-machine tooltips show what the machine will
  *actually* produce once modules, quality, beacons and productivity are applied — needed
  once the numbers stopped being "nice and round" [s16]. The assembler GUI split the recipe
  (click → Factoriopedia recipe) from the product slot (its own item tooltip, spoilage) [s16].
- **Resource search (2.0).** Search a resource, matching patches highlight on the map; pin a
  patch and its remaining amount updates live [s16].
- **Alt-mode overlay** [mem]: one key toggles a world overlay — recipe icon over each machine,
  chest contents, inserter filters, belt directions. Separately, every machine shows a
  blinking **status icon** in the world when it stops (no power, no fuel, output full,
  missing ingredients); the status names are a fixed enumeration exposed even to modders
  (`no-power`, `full-output`, `waiting-for-source-items`, …) [s17].
- **Pipette (Q) and quickbar** [mem]: Q over any entity puts that exact building (with its
  recipe/settings) in hand; the quickbar holds filtered slots; Shift-click copies settings
  between entities. Learn-by-pointing: the world itself is the palette.

### Praise

- Factorio is the reference case for "the UI never lies and never hides" [mem]. FFF readers
  praised the tooltip restyle and merging kill stats into the general statistics [s11][s13].
- Factoriopedia directly answers the two questions a production game raises — "what do I
  need for this?" and "what do I use this for?" [s12].

### Criticism

- Tooltip restyle drew "crowded" comments [s11].
- Status text is sometimes wrong (an inserter says "waiting for source items" when the
  destination is full; "output full" fires at 2× a recipe's output, before a stack fills)
  [s17] — a misleading reason is worse than none.
- Things the base game left to mods for years: per-machine rate calculator over a selection
  ("Rate Calculator"), hover-only alt-mode [s18]. Rates over a *selection* is a gap players
  keep filling.
- Alt as both "overlay toggle" and "open pedia" modifier worried players (resolved by not
  toggling when clicking) [s12].

---

## 3. Old World (Mohawk)

### Mechanism

- **Transparency as a design rule.** Soren Johnson: "Transparency is an important part of my
  design aesthetic"; he was "fairly uncomfortable with opaque triggers and effects which kept
  players in the dark, forcing them to play by feel" [s19].
- **Every event option shows its immediate effects**; downstream consequences (a trait that
  may later trigger events) are described in general terms only — a Mohawk developer: "always
  show any immediate effects, while we also say for concepts like traits that they can lead
  to many different situations via events" [s20]. Hovering an option that affects tiles
  highlights those tiles on the map [s20].
- **Context-filtered tooltips.** Character tooltips show the trait effects relevant to the
  character's current job (leader, general, governor); tile tooltips omit trait effects
  unless the player actually has a character with that trait [s20].
- **Cascading (nested) tooltips** with middle-click freeze, Paradox-style [s21][s22].

### Praise / criticism

- "A nice system of cascading tooltips" giving easy access to information [s21].
- But: tooltips "not an effective stand-in for a well-structured and in-depth tutorial"; the
  tutorial is "a series of pop-up messages"; the encyclopedia felt incomplete, some entries
  empty; UI "cramped" with many tiny icons; diplomacy screen singled out; few helpful map
  overlays [s21]. Same lesson as Vic3: hyperlinked depth without a guided entry point
  overwhelms.

---

## 4. Civilization VI / VII

### Mechanism (VI)

- **Civilopedia** with concept pages; terms in tooltips link into it [mem].
- **Lenses** — map overlays toggled from the minimap or auto-shown by context: Appeal (each
  tile's appeal), Settler (recommended city sites, water, loyalty penalty per tile),
  Continent, Tourism, Religion, Loyalty [s23]. Selecting a settler or builder switches the
  relevant lens on automatically [mem].
- **Placement preview:** while placing a district, every candidate hex shows its adjacency
  bonus before committing; tooltips list each contributing neighbour [mem].
- **Yield breakdowns:** the city panel's food/production/gold numbers expand into per-source
  lists (tiles, buildings, policies) [mem].

### Praise / criticism

- Lenses are well liked, but no built-in way to add lenses; the "More Lenses" mod (very
  popular) fakes the Appeal lens to draw others, and its settler lens activates on
  Ctrl+select [s24]. A mod author called the default Appeal transparency "atrocious… I was
  always having to switch away from my settler to figure out where to settle" [s24] —
  an overlay that hides the base map defeats its purpose.
- **Civ VII (Feb 2025) launch:** UI the dominant complaint; "feels ported from the console in
  the worst way possible"; information screens underdeveloped, QoL from older entries
  missing (quick move, auto-explore, rename); Firaxis acknowledged it and scheduled UI
  patches [s25][s26]. Lesson: a deep game can lose a lot of goodwill purely on missing
  information surfaces, even with liked mechanics.

---

## 5. Cities: Skylines 1 and 2

### Mechanism

- **Info views:** one button opens a colour-gradient overlay per system — pollution (ground,
  water, noise), education, happiness, crime, land value, wind, water/power coverage,
  traffic [s27][s28]. Happiness is broken down by zone type and its contributing factors
  [s27]. CS2 has 33 info views; pollution overlays use a blue–red ramp for the current level
  and white–orange for what each building/road *generates* — separating "state" from
  "source" [s28].
- **Building service radius preview** while placing (coverage drawn before you build) [mem].

### Praise / criticism

- CS1 reviewers praised the overlays as intuitive [s27]; CS2 reviewers called its information
  display a strength in a launch dominated by performance problems [s29].
- But: the simulation behind the overlays was hard to read — reviews of CS2 noted confusing
  swings in how the economy is presented [s29]; CS1 critics found traffic dominated
  everything and wanted more specific data [s27]. In CS2 the "high rent" / economy
  complaints were largely about numbers with no visible cause [mem].
- New developer Iceflake's first act was a UI overhaul: simpler onboarding, reworked icons
  "for clearer meaning", toolbar with more visible requirement indicators ("why is this
  locked") [s30].
- Lesson: an overlay tells you *where*; it does not tell you *why*. Overlays need a click-
  through to the cause (which building is the noise source, which factor drags happiness).

---

## 6. Into the Breach (Subset Games)

### Mechanism

- **Perfect information.** Every enemy's next attack is drawn on the board — direction,
  target tiles, damage — before the player moves; the player knows exactly what will
  happen if nothing changes [s31][s32].
- **Preview before commit** [mem]: hovering a weapon on a target shows the full result —
  damage, push arrows, which units/buildings will be hit, resulting enemy-attack changes,
  grid damage; hovering an enemy shows its attack order number. Moves can be undone; one
  "reset turn" per battle.
- **Animated tooltips:** a weapon's tooltip shows a tiny animation of the effect on a demo
  board, because text could not describe it — "showing that little animation … is a
  thousand times more effective" [s33].
- **One persistent stake:** the power grid bar is always visible and is the single thing to
  protect [s34].

### Why it works

- Justin Ma: "we would sacrifice cool ideas for the sake of clarity every time"; "If it can be
  shown in a clear way, [then] yes, we can have that"; "the player has to understand what's
  going on in any situation" [s33]. Weapons and firing patterns playtesters could not read
  were *cut* [s33]; damage indicators the devs found fine confused outside testers [s33].
- Telegraphing also sped play up — shorter battles than XCOM [s32].
- Reviewers: it feels like chess, not XCOM — randomness moved out of outcomes [s35].

### Criticism

- The constraint is real: the clarity rule limited design space [s33]. Perfect information is
  only cheap when the state is small; in a big sim you must choose which consequences to
  preview.

---

## 7. Slay the Spire (Mega Crit)

### Mechanism and history

- Enemies originally hid their next action, which fought the permadeath design. First fix: a
  "Next Turn" mode where the player selected an enemy to preview its action. Final: **intent
  icons** above every enemy, always on [s36]. Initially the icons did not show exact damage;
  playtesters found that showing numbers was *more* engaging and let them plan new
  strategies [s36]. Mega Crit has said revealing intent is what made the game come
  together [s37].
- Balance from telemetry: the client reported which cards were picked/won; rarely-picked
  cards were buffed, over-represented-in-wins cards nerfed [s36].
- StS2 places multiple intents side by side instead of stacked [s37].

### Lesson

- Showing the exact number did not remove decision-making — it moved the challenge from
  guessing to planning. Hidden information that the player cannot act on is just noise.
- Moving from opt-in preview (select to see) to always-on glanceable icon was the win.

---

## 8. Dyson Sphere Program

- **Statistics panel (late-2024 rework):** upstream/downstream lookup for an item ("where is
  this produced, what consumes it"), a "Reference Rate" = rate if every running facility
  were at full efficiency (actual vs theoretical side by side), import/export per planet and
  star system, histogram tooltips per interval, radial power chart, pin-to-dashboard
  charts [s38].
- Overall reception overwhelmingly positive; common complaints are lack of clear goals and
  guidance and a steep learning curve [s38] — the 2024 patch also added "Game Goals".
- Lesson: actual-vs-ideal comparison is the fastest bottleneck detector; the goals feature
  addresses "what now", which stats alone do not.

---

## 9. shapez / shapez 2

- Minimal, symbolic visuals: shapes are their own label (a quarter-shape *looks* like the
  item it is), so the factory reads at a glance [mem].
- Tobspr said they invested heavily in UI, onboarding and tutorial for shapez 2 [s39]; the
  1.0 review: readability is strong, but "readability is not the same thing as onboarding"
  [s39]. The 2025 update added **operator previews**, categorized shops, wire insights and
  **clearer throughput displays** [s40] — preview the output of a machine before building it.

---

## 10. Game UI Database

- gameuidatabase.com catalogues screenshots by screen type (tooltip, map, encyclopedia,
  inventory, notification…) across ~1,000+ games and is the practical place to collect
  reference screens for each pattern below [mem].

---

## Patterns

1. **Explain-the-number (provenance breakdowns).** Every derived value is hoverable and lists
   base + signed contributions with their sources, biggest first (Paradox [s5][s7], Civ yields,
   CS happiness [s27]). Sorting matters: Vic3 had to re-sort to put the relevant line on top [s5].
2. **Explain-the-refusal.** Unavailable actions stay visible (greyed) with the failing
   conditions listed (Vic3 showing unstartable plays [s5], CK3 interactions [mem], CS2
   requirement indicators [s30]). A hidden action teaches nothing; a disabled one with a
   reason teaches the rule.
3. **Hyperlinked glossary (nested tooltips + encyclopedia).** Terms link to definitions in
   place; the encyclopedia is the same graph, clickable, with history (CK3/Vic3 [s1][s3],
   Factoriopedia [s12]). Keep chains short (2–3) and give a lock mode [s2].
4. **Two-way production links.** "Made from / used in / made by / unlocked by" on every item
   (Factoriopedia [s12], DSP upstream/downstream [s38]). Production games live on this.
5. **Preview before commit.** Show the outcome on hover, on the map, before the click (Into
   the Breach, StS intents [s36], Old World event effects + tile highlight [s20], Civ
   adjacency, shapez operator previews [s40]). Show exact numbers [s36].
6. **Overlay lenses.** One-key world overlays per system, auto-selected by context (Civ
   settler lens [s23], CS info views [s27], Factorio alt-mode [mem]). They must keep the base
   map readable [s24] and should let you click from a hot spot to its cause [s28].
7. **In-world status glyphs.** Stalled things say why, on the thing (Factorio status icons
   [s17]); the reason must be correct [s17].
8. **Search everywhere, one way.** One consistent search per panel, same key everywhere [s13];
   map search for resources with pinned live counts [s16].
9. **Point at the world to learn/act.** Pipette, Alt+click → pedia from any object [s12][mem].
10. **Statistics with time windows and actual-vs-ideal** (Factorio [s14][s15], DSP [s38]).
11. **Progressive disclosure through triage, not hiding.** Issues/suggestions list with
    dismiss (CK3 [s3]), notification settings with auto-pause per type and fewer defaults
    (Vic3 cut ~50% [s5]); EU V's alert flood is the counter-example [s10].
12. **Guided entry point is still needed.** Every tooltip-heavy game criticised for its
    tutorial (Old World [s21], Vic3 [s7], EU V [s9]) shows that a hyperlinked reference
    answers questions but does not teach which questions to ask.
13. **Clarity as a cut criterion** (Into the Breach [s33]): if a mechanic cannot be shown
    clearly, simplify or cut it.
14. **Same card everywhere.** An item tooltip identical in every context; info split into
    composable cards rather than one mashed tooltip [s11].

---

## For us (DF-style fortress)

- **One "why" hover for every number and state.** Dwarf mood, need levels, stress, skill
  rate, job priority, stockpile counts, tile value: hover → signed contribution list sorted by
  magnitude, each line a link (e.g. stress −12 "ate without a table" → the event; +8
  "admired a fine statue" → the statue, click to fly the camera there). Build it as a
  sim-side `Breakdown { base, terms: Vec<(Source, i32)> }` produced by the same code that
  computes the value, never a parallel UI formula — DF's own failure is that thoughts are
  prose you must read in full [mem].
- **Every job has an in-world status and a "why idle / why not done" answer.** Designations
  (dig/chop/build already exist in `orders.rs`) should show a glyph per stalled order: no
  worker with the labor, unreachable, no material, forbidden, waiting. The answer must come
  from the actual job-assignment check, so it cannot lie (Factorio's wrong-reason bugs).
- **Refusals are visible.** Tools/orders that are invalid on the hovered tile already have
  sim-side validity rules — show the failing rule in the cursor readout rather than silently
  refusing. Same for embark (`EmbarkRefusal`): list every reason.
- **A linked "fortresspedia" over the same data:** item / material / workshop / creature
  pages with made-from, used-in, made-at, needs-labor, and back/forward history; Alt+click
  on anything in the world or a list opens it. Nested tooltips for terms (2 levels, timer lock
  default, middle-click action lock), rendered as Slint cards.
- **Lenses on the commander camera:** one key / wheel wedge per system — designations,
  stockpiles and their contents, traffic/pathing cost, room value, temperature/water later,
  danger/creature sight. Dim, don't hide, the terrain. Auto-pick the lens for the active
  tool (building → room/value lens; dig → designation lens). Each hot spot clickable to its
  cause.
- **Preview before commit for orders:** while dragging a dig/build box, show tile count,
  materials required vs in stock, estimated labour, and newly exposed hazards (water,
  caverns) — Into the Breach discipline, exact numbers. The move-order path preview
  (`move_order.rs`) is already this pattern; extend it to every order.
- **Production view, not just stocks:** per-item made/used per day with time windows and
  "possible vs actual" (workshops idle for lack of input vs lack of worker), plus a
  made-from/used-in chain view. DF's stocks screen only shows inventory, never rates [mem].
- **Triage, not alerts flood.** An issues list (CK3 style, dismissible, restorable) for
  standing problems — "no food stockpile", "3 dwarves thirsty, no drink" — separate from a
  short event feed (`Notice`s already exist) with per-type settings and auto-pause for the
  dangerous ones. Default to few. EU V's alert flood is the warning.
- **Search in every list and on the map:** one search field in each panel header (dwarves,
  items, workshops, stockpiles), same key; a map search for materials/ores with pinned live
  counts.
- **Unit inspector as composable cards:** the person inspector (`person_lines`) should split
  into identical-everywhere cards (needs, skills, relationships, current job + why) that also
  appear as tooltips in lists.
- **A guided first fortress.** A tiny scripted embark where each step makes the player open
  the screen being taught (CK3's approach), with reactive tips that can be reset. Do not rely
  on the pedia to teach.
- **Use clarity as a feature filter.** When porting a DF system, ask whether its state and
  causes can be shown with the patterns above; if not, simplify before building it.

---

## Sources

- [s1] Victoria 3 Dev Diary #29 – User Experience (search snippets; forum unreachable today) — https://forum.paradoxplaza.com/forum/developer-diary/victoria-3-dev-diary-29-user-experience.1506484/
- [s2] Philip Ardeljan, "Tooltips in tooltips" — https://philip.design/blog/tooltips-in-tooltips
- [s3] GameWatcher, CK3 tutorials, highlighted text, encyclopedia — https://www.gamewatcher.com/crusader-kings-3-tutorials-highlighted-text-encyclopedia
- [s4] CK3 Dev Diary #16 "Tutorials and Tooltips and Encyclopedias, Oh My!" + coverage — https://admin-forum.paradoxplaza.com/forum/developer-diary/ck3-dev-diary-16-tutorials-and-tooltips-and-encyclopedias-oh-my.1345581 ; https://www.pcgamer.com/the-crusader-kings-3-team-is-working-hard-to-make-the-game-easier-to-learn
- [s5] Victoria 3 Dev Diary #74 – UX Improvements — https://www.paradoxinteractive.com/games/victoria-3/news/dev-diary-74-ux-improvements
- [s6] PCGamesN, Victoria 3 nested tooltip system — https://www.pcgamesn.com/victoria-3/nested-tooltip-system
- [s7] Victoria 3 DD #117 (UX of Sphere of Influence, "cooltip"), eXputer review, Steam thread — https://admin-forum.paradoxplaza.com/forum/developer-diary/victoria-3-dev-diary-117-the-ux-of-sphere-of-influence.1677959/ ; https://exputer.com/reviews/victoria-3/ ; https://steamcommunity.com/app/529340/discussions/0/3549427890070729105
- [s8] Victoria 3 reviews (GameCritics, Destructoid) — https://gamecritics.com/mitch-zehe/victoria-3-review/ ; https://destructoid.com/reviews/review-victoria-3-pc-paradox-interactive-strategy
- [s9] EU V: Digital Chumps review, Steam discussion, Tobold's blog — https://digitalchumps.com/europa-universalis-v-pc-review ; https://steamcommunity.com/app/3450310/discussions/0/604155654554287067 ; https://tobolds.blogspot.com/2025/11/europa-universalis-v-first-impressions.html
- [s10] EU V 1.1 "Rossbach" — https://finalboss.io/europa-universalis-5-rossbach-1-1-tackles-ui ; https://www.gamewatcher.com/europa-universalis-5-rossbach-update-1-1-hidden-alerts-army-navy-templates-quality-of-life-changes
- [s11] Factorio FFF #318 – New Tooltips — https://www.factorio.com/blog/post/fff-318
- [s12] Factorio FFF #397 – Factoriopedia (+ forum thread) — https://www.factorio.com/blog/post/fff-397 ; https://forums.factorio.com/111179
- [s13] Factorio FFF #337 – Statistics GUI — https://www.factorio.com/blog/post/fff-337
- [s14] Factorio wiki, Production statistics — https://wiki.factorio.com/Production_statistics
- [s15] Factorio FFF #408 – Statistics improvements — https://www.factorio.com/blog/post/fff-408
- [s16] Factorio FFF #426 – Resource search & assembler GUI — https://www.factorio.com/blog/post/fff-426
- [s17] Factorio Lua API EntityStatus + bug reports on wrong status — https://lua-api.factorio.com/latest/types/EntityStatus.html ; https://forums.factorio.com/viewtopic.php?p=641232 ; https://forums.factorio.com/viewtopic.php?p=691043
- [s18] Factorio mods: Rate Calculator, Hover ALT Mode — https://mods.factorio.com/mod/RateCalculator/changelog ; https://mods.factorio.com/mod/hover-alt-mode
- [s19] Soren Johnson, Old World Designer Notes #9: Events — https://www.designer-notes.com/?p=1733
- [s20] Old World patch notes / dev Steam reply on event effects — https://steamcommunity.com/app/597180/discussions/0/3875967149491539006 ; https://steamdb.info/patchnotes/15741230 ; https://mohawkgames.com/?p=3805
- [s21] Cultured Vultures, Old World review — https://culturedvultures.com/old-world-pc-review/
- [s22] Quarter to Three Old World thread (tooltip freeze) — https://forum.quartertothree.com/t/old-world-pka-ten-crowns-from-soren-johnson/134746/1543
- [s23] Civilopedia (Civ VI lenses) — https://www.civilopedia.net/standard-rules/concepts/world_7
- [s24] Civ VI More Lenses mod; SiBr3's Transparent Appeal — https://forums.civfanatics.com/threads/more-lenses.606150 ; https://forums.civfanatics.com/resources/sibr3s-transparent-appeal.32000
- [s25] Civ VII launch UI — https://www.player.one/fans-rip-civilization-viis-ui-despite-new-gameplay-mechanics-this-game-we-expected-159784 ; https://www.cgmagonline.com/news/civilization-vii-quality-of-life-updates
- [s26] Aftermath, Civ VII UI update — https://aftermath.site/civilization-vii-civ-7-update-ui/
- [s27] Cities: Skylines reviews / info panel — https://www.pcgamer.com/uk/cities-skylines-review ; https://www.gamepressure.com/citiesskylines/information-panel/za7595
- [s28] CS2 info views guide — https://gameplay.tips/guides/cities-skylines-ii-info-views-guide.html
- [s29] CS2 reviews — https://gamesradar.com/cities-skylines-2-review ; https://techradar.com/gaming/consoles-pc/cities-skylines-2-review-road-to-success
- [s30] Iceflake CS2 UI overhaul — https://www.pcgamer.com/games/city-builder/cities-skylines-2s-new-developer-breaks-cover-on-its-plans-for-the-sequels-future-revealing-a-patch-that-will-include-changes-players-have-been-requesting-for-a-long-time/ ; https://www.eteknix.com/iceflake-giving-cities-skylines-2-ui-an-extreme-makeover-with-their-first-big-update/
- [s31] GDC 2019 "Into the Breach Design Postmortem" (vault, members) — https://gdcvault.com/play/1025772/-Into-the-Breach-Design
- [s32] Wikipedia, Into the Breach — https://en.wikipedia.org/wiki/Into_the_Breach
- [s33] Game Developer, "sacrifice cool ideas for the sake of clarity every time" — https://www.gamedeveloper.com/design/-i-into-the-breach-i-dev-on-ui-design-sacrifice-cool-ideas-for-the-sake-of-clarity-every-time-
- [s34] Game Developer, IGF / follow-up interviews (power grid clarity) — https://gamedeveloper.com/game-platforms/road-to-the-igf-subset-games-i-into-the-breach-i-
- [s35] "What can we learn from Into the Breach?" — https://sleepingbagworm.blogspot.com/2024/01/what-can-we-learn-from-into-breach.html
- [s36] Wikipedia, Slay the Spire (development) — https://en.wikipedia.org/wiki/Slay_the_Spire
- [s37] Ars Technica War Stories via summaries; StS2 intent layout — https://nintygamer.com/tag/slay-the-spire/ ; https://sts2.untapped.gg/guides/how-to-read-enemy-intent
- [s38] DSP patch notes 0.10.32 (statistics panel) + review summary — https://www.dsp-wiki.com/Patch_Notes:0.10.32.25496 ; https://vaporlens.app/app/1366540/dyson_sphere_program/stats/details
- [s39] shapez 2 EA/1.0 coverage — https://gamespress.com/shapez-2-arrives-in-Steam-Early-Access-on-August-15 ; https://finalboss.io/shapez-2-hits-version-1-0-new-manufacture-mode-mods-and-a-tutorial
- [s40] shapez 2 Dimension update press release — https://www.gamespress.com/shapez-2-Launches-Massive-Free-Dimension-Update-Today
