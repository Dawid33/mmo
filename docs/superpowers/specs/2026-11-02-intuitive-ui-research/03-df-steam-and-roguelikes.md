# 03 — Dwarf Fortress's Steam UI, its community fixes, and DF-likes / roguelikes

Part of the intuitive-UI research (2026-11-02). The premium screen layout itself (toolbars, depth
bar, minimap, unit sheet) is already catalogued in
`docs/superpowers/specs/2026-10-20-dwarf-fortress-research/12-ui-and-interaction.md` and is not
repeated here. This file covers how the 2022 redesign was made, how players received it, what
still hurts, what the community built to patch it, and what other deep-sim roguelikes learned.

Tags: `[sN]` = numbered source at the end; `[mem]` = from general knowledge, not re-verified
this session. Where a search came back thin, that is said rather than papered over.

---

## 1. The redesign process

### Who designed it

- **Bay 12 designed the UI, not Kitfox.** Tarn: the new UI was "mostly Zach and I", with input
  from Mike Mayday and Meph (the two paid modders who made the 32×32 tileset and audio) [s1][s5].
  Kitfox's Tanya Short, in her two-months-later retrospective, says outright that Kitfox did no
  design or UX work and that every gameplay improvement is Tarn's and Zach's [s2] (summarised via
  search; the Medium page itself returned 403).
- **A professional UX reviewer came in, but too late.** Tarn: the reviewer looked at the UI later
  than he thinks was ideal; their input produced the **building menu** and smoother edges on the
  main screen, and he said they would not repeat that timing for adventure mode [s1]. In a
  pre-launch interview he also says "we have added tooltips everywhere" and brought in a UX
  specialist [s3].
- **Playtesting** was mostly Zach: "like a thousand hours of testing just on the Steam version"
  [s4]. No formal UI playtest programme is documented in anything found. One anecdote about a
  newcomer: a tester who had only played *The Sims* built an above-ground town instead of a
  fortress [s3], which says the game's premise (dig down) was not communicated by the UI.

### Constraints that shaped it

- **One UI, two skins.** Classic (ASCII) and premium share one grid; switching modes mostly
  swaps glyphs. Tarn: "We can't support two different UIs, that would be just too much work";
  Zach: "The interface changes as if we had never added graphics at all" [s1][s3]. So the free
  Classic version *also* lost the old keyboard UI: the last fully keyboard-controllable version
  is 0.47.05 [s6].
- **Text-grid layout.** Every panel still sits on the character grid, which limited layout
  options (Tarn calls it "some restrictions") but kept one codebase [s1].
- **Mouse-first, keyboard deferred.** Keyboard support was listed as classic functionality to
  bring back *after* launch QoL work [s1]. In practice keyboard came back piecemeal (section 3).

### Stated goals

- "It's like a glow-up. It just looks nice, and it's easier to use, but it's the same game"
  (Tarn) [s3]. Same sim, new presentation; tooltips, a tutorial, in-game guides and a
  "pixel-perfect minimap" (the old one was "mostly decorative") were the levers [s3].
- Zach expected the colony-sim genre (RimWorld etc.) to do much of the teaching: the genre "is
  really doing a lot of work for us now" [s3].

---

## 2. What worked

- **Click-to-inspect replaced memorised verbs.** Reviewers single out that "a simple mouseover
  [is] required to tell you exactly what you're looking at" (RPS) [s7]; the Guardian calls the
  bars with roll-over tooltips, tabbed menus and alert notifications "liberating" and says the
  Steam edition "feels effortless" next to the text version [s8]. PC Gamer (84/100): "slightly
  less impenetrable, just as engrossing"; clicking to designate / interact / inspect is "a
  much-needed and welcome change" [s9].
- **Legible at a glance.** The tileset lets you "distinguish dwarves at a glance rather than
  having to dive into their biography screens", and see details like whether a barrel is empty
  [s8]. Later patches extended this: container graphics show what is inside barrels and boxes
  (50.15) [s15].
- **The unit sheet's "Overview" tab** surfaces the highest-priority facts first, with
  Relations / Thoughts / Needs tabs behind it, and a tab per creature when several share a tile;
  it updates live while the game runs, so you can watch thoughts as they happen [s10].
- **Tutorials** — "rudimentary but effective", e.g. drafting a militia [s8]; RPS says they "go a
  long way to easing you in gently" [s7].
- **Automatic labour + work details.** The old per-dwarf labour toggles were replaced by
  automatic assignment plus named "work details" (mining, woodcutting, hunting need special
  equipment and stay manual) [s11]. Some Steam players praised it as a gentler on-ramp [s11].
- **Commercially** it landed: Kitfox's forecast of ~163k units in the first two months was hit
  in under 24 hours [s2]. Steam reviews have stayed "Overwhelmingly Positive" [mem].

## 3. What did not work

### Depth and discoverability

- **Nested menus.** PC Gamer: the new UI "struggles to accommodate every aspect of this
  bottomless game" and "something was lost in the UI translation"; the classic keyboard
  interface, once learned, "followed a reliable logic" [s9]. A reprinted review (publication not
  confirmed) has "within minutes you're three menus deep, trying to parse labour details and
  work duties" [s9]. A long-time poster on MetaFilter joked it should be called "Menu Quest"
  [s12].
- **Hidden affordances.** A Steam thread: "every detail is hidden away behind some tab"; making
  armour "requires going through a horrible menu"; a player "had no clue the magnifying glass
  under workorders" opened extra settings [s13]. Another: "the game got mouse support at the cost
  of no longer being able to handle the entirety of the game" [s13].
- **Tasks that are still not obvious.** RPS: "how to actually designate and assign bedrooms
  isn't nearly so obvious", and players still end up on the wiki [s7]. Guardian: you still need
  to look up "how to mollify a dwarf" [s8].
- **Z-levels remain the first wall.** Beginner guides still call imagining the 3D world from a
  2D slice one of the "two single greatest entrance barriers" [s14]. Premium added the depth bar,
  minimap and jump-to-surface, but no other view of the stack [mem].

### Keyboard

- **Hotkeys only appear on hover**, and shortcut letters are no longer underlined in menus;
  numpad did not work; some windows (raw material selection) could not be driven by keyboard at
  all; a keyboard cursor exists but only inside a mode that can use it (Settings › Game ›
  "Keyboard cursor enabled") [s16]. Quotes: "I really just want the old control scheme back :(";
  "100% kbd control is unfortunately not possible at this moment"; and from DFHack second-hand,
  "the old UI is their #1 most requested feature" [s16]. Some players stayed on 0.47.05 [s16].
- A Jan 2023 r/dwarffortress answer: Tarn plans "gradually putting keyboard support back in all
  the menus", no timeline [s17]. Oct 2023: "They still talk about adding improved keyboard
  support, but then they talk about a lot of things" [s16].

### Lists, labour, stockpiles, military

- **No search, no sort at launch.** Sorting for the creature and task lists, sorting + search in
  the kitchen and work-details screens, editable work details with restore-to-default, and
  "keyboard support for many unit lists" arrived only in **50.12, March 2024** — 15 months after
  launch [s18]. DFHack had shipped search widgets on 30+ screens in 50.11-r2 (Oct 2023) and
  deleted them once vanilla caught up [s19][s20].
- **Work details are hard to read.** Any job not in a work detail is done by everyone — a common
  surprise [s11]. Players asked for a Dwarf-Therapist-style view "to make dwarves easier to read
  by glance" [s11]. Bugs kept surfacing on the screen into 2024–2025 (checkbox pushed off-screen
  by a long detail list; scrollbar broken in 53.08; keyboard shortcuts dying on it) [s21][s22].
- **Stockpile settings.** No stockpile priorities [s13]; opening "Custom" on an existing pile
  reset its bin / barrel / wheelbarrow limits, so players learned "always check the barrels
  again after touching the custom settings"; the wiki talk page calls the menu "one of the
  least-intuitive" [s23].
- **Military** remains, in the wiki's own words, "one of the most confusing aspects of the game";
  Steam threads report uniforms assigned but not worn, and a squad only working if its uniform
  existed before the squad — order-of-creation rules the UI never states [s24].
- **Text size.** Steam Deck HQ gave controls/UI 3/5: no native gamepad, text "still hard to read
  at times", wants to resize text and UI elements [s25].

### Post-release UI changes (what Bay 12 fixed, in order)

| when | change |
|---|---|
| 50.0x (2023) | keyboard cursor setting; DFHack `Alt-K` toggle meanwhile [s16][s26] |
| 50.12, Mar 2024 | sort/search on creature, task, kitchen, work-detail lists; work details editable + restore defaults; keyboard for many unit lists; **burrows across multiple z-levels**; squad assignment lists assigned units last [s18] |
| 50.15, late 2024 | case-insensitive unit filters; list sort fixes; view worn clothing by dye/profession; container contents drawn [s15] |
| 51.05, Feb 2025 | **rectangle dimensions shown while dragging**; numbers/symbols in unit search [s27] |
| 51.11, Apr 2025 | keyboard scroll amount settings; assigning to a work detail now properly disables others [s28] |

Pattern: almost every fix is *finding and filtering* (search, sort, keyboard on lists) or
*feedback during an action* (drag dimensions), not new screens.

---

## 4. Community fixes, and why they exist

Each tool is a map of a gap the vanilla UI left.

- **DFHack `sort`** — search on info tabs (tasks, artifacts), noble candidates by skill, temple
  and guildhall pickers by deity/sphere, and **squad assignment ranked by melee/ranged
  effectiveness, potential, stress, training need**, with Include/Only/Exclude filters for
  nobles, mothers with infants, combat-haters, the maimed; the top-bar stress icons become
  clickable lists that **centre the map on the unit** [s29]. Gap: lists you can scroll but not
  ask questions of.
- **DFHack `buildingplan`** — place buildings and constructions *before the materials exist*;
  they wait suspended (clock marker) and claim items as they are made; filters by quality,
  material, decoration with 10 saved favourites; "Queue order" makes the missing items [s30].
  Gap: vanilla wants materials in hand at placement, forcing the player to sequence production
  manually.
- **DFHack `unsuspend` / `suspendmanager`** — resume jobs halted by fleeing wildlife,
  inaccessible materials or items in the way, and pre-emptively suspend constructions that would
  cave in, block other builds or wall a builder in [s31]. Gap: a suspended job is a silent
  failure the player must notice and hunt down one by one.
- **DFHack `quickfort` / `gui/quickfort` / `gui/blueprint`** — blueprints in CSV/XLSX (`#dig`,
  `#build`, `#place`, `#zone` modes); preview and reposition before applying; rotate, flip,
  repeat across z-levels; export a built area as a blueprint and stamp it elsewhere; `orders`
  queues the work orders it needs; `undo` [s32]. Origin: AutoHotkey macros for the classic
  keyboard UI, because typing the same designations every fort was tedious [mem]. Gap: no
  copy/paste, no templates, no undo of a designation batch.
- **DFHack overlay framework + `gui/control-panel`** — lets tools inject widgets into vanilla
  screens; DFHack's stated direction since 50.08 is making tools "more mouse-aware" [s20][s26].
  Stockpile overlay: auto-melt / trade / dump / train items brought to a pile [s19].
- **Dwarf Therapist** — a **spreadsheet of every dwarf × every labour**, read and written
  through game memory; bulk edits, custom professions/roles, and a labour optimiser [s33].
  Exists because the in-game labour screen is per-dwarf or per-detail and cannot show the whole
  fort's allocation at once; maintained for v50 too [s33].
- **Stonesense** — an isometric viewer running next to DF; Ctrl+wheel changes how many
  z-levels are drawn, and it can follow the DF window [s34]. Pitched from the start as giving the
  player "some visual orientation" the slice view lacks [s34].

Common thread: the community did not replace screens; it added **search, ranking, batching,
templates, auto-recovery of stuck work, and a whole-fort table** on top of them.

---

## 5. The classic keyboard UI: what veterans miss

- **Verb-first mnemonic chains** (`d` designate → `d` mine / `h` channel / `t` chop; `b`
  build; `q` query; `k` look; `u` units; `z` status; `p` stockpile) — every action two or three
  keys from the map, no pointer travel, same chain every time [mem]. PC Gamer: once acclimatised,
  it "followed a reliable logic" [s9].
- **Visible hotkeys.** Letters were printed next to every menu entry; players learned them by
  reading, not by hovering [s16].
- **Speed under load.** Designating a 20-level stair shaft or a room grid was a fixed keystroke
  sequence, which is exactly what quickfort automated [mem].
- **Whole-game reach.** Every function was keyboard-reachable; the premium build still has
  windows the keyboard cannot operate [s16].

What veterans do *not* miss: memorising unlabelled verbs before you can do anything, and opaque
per-dwarf labour toggles (`v` → `p` → `l`) — the reasons the redesign happened [mem][s9].

---

## 6. DF-likes and deep-sim roguelikes

### Caves of Qud — "Spring Molting" UI (June 2024) and 1.0 (Dec 2024)

- **Goal**: lower the barrier the console UI put in front of new players while keeping "the
  essence of what makes Qud Qud"; "fully mouseable, fully gamepaddable" [s35].
- **Changes**: combined equipment + inventory, search bars, a redesigned trade screen, quest
  givers shown on the world map instead of a text quest list, every journal tab, message log,
  character sheet, skills, tinkering, status effects, reputation, worldgen, "interact nearby",
  books, terminals; runs natively on Steam Deck [s35].
- **Process**: shipped incrementally over ~18 months on a beta branch with legacy and modern
  widgets side by side; readability at every zoom drove fixes (fullscreen "Fit" when zoomed so
  old elements never become unreadable; integer "Pixel Perfect" scale) [s36].
- **Escape hatch**: "Enable modern UI elements" can be unchecked to get the text UI back [s35].
- **Reception**: PC Gamer said Qud had "a near-impenetrable interface — until now" and the
  update made it "far more approachable" [s37]; GamingOnLinux called it much better for gamepad
  and Deck [s38]. The visible complaint: the new equipment list lacked hotkeys; the developer
  replied hotkeys were planned but missed the patch [s39]. Same lesson as DF: mouse-first
  rewrites ship before keyboard parity, and the keyboard players notice first.

### Cataclysm: Dark Days Ahead

- Recurring complaints: the pocket/container inventory is "a haphazard mess", the game becomes
  "an inventory management simulator" too fast; newer crafting flows add keystrokes and steps;
  the UI is the biggest obstacle on returning after a break, though "very intuitive once you
  figure out the mechanics and UI"; players ask for mouse-friendly menus to shorten the curve
  [s40]. The project's own UX design doc is about *game* principles ("if it works in reality, it
  works in the game"), not interface rules [s41] — the UI grew per feature without an
  interaction model, and it shows. Lesson: realism-driven systems multiply the number of
  screens; without a shared pattern each new system adds its own menu dialect.

### Ultima Ratio Regum

- Thin results this session: none of Mark R Johnson's UI posts came back in search. What was
  found: the game reveals its generated cultures through presentation, not encyclopedia entries
  — dialogue register ("flowery" vs "curt"), symbols on shields that cultures react to, clues in
  libraries, altars and sarcophagi [s42]; Johnson's planned encyclopedia would list concrete
  things (books, clothes, paintings) but deliberately *not* nations, religions or cultures,
  "since those are what the player pieces together" [s43]. Takeaway: in a deep sim, the UI can
  choose which facts are *reference* (looked up) and which are *discovered* (read from the
  world).
- Adjacent and better sourced: Cogmind's developer reports readability was "by far the top
  request" and that "readability trumps all" in a text-heavy game; his 80-cell minimum width was
  "one of the largest drawbacks" for visually impaired players; message-log colours had to be
  optional [s44].

### Gnomoria

- Split reception. Praise: mouse- and menu-driven, "soars beyond DF in terms of UI" [s45].
  Criticism: "a ton of nested menus and getting your gnomes to do anything is pretty tedious";
  no tutorial or in-game help [s45]. It shows that "mouse menus" alone do not fix depth: it
  merely moved DF's depth from keys into submenus.

### Stonesense

- See section 4. Its existence (and longevity, from ~2009 into the v50 era) is the strongest
  evidence that a single-slice top-down view leaves players without a mental model of the
  vertical structure [s34][mem].

### Songs of Syx (brief)

- Mixed: aggregated Steam feedback lists "steep learning curve with poor tutorials" and "UI
  issues" [s46]; one review credits it with letting you read logistics, worker paths and
  construction priorities without crowded menus [s46]. Its large-scale overlays (paths, service
  coverage) are the part reviewers praise [mem].

---

## 7. Lessons

1. **Bring the UX reviewer in before the screens exist.** DF's reviewer arrived late and still
   produced the building menu; Tarn says he won't repeat the timing [s1].
2. **Search and sort on every list, from day one.** DF needed 15 months and a community mod to
   get basic list search; DFHack deleted its widgets once vanilla caught up [s18][s20].
3. **Show hotkeys permanently, next to the label.** Hover-only hotkeys were the most specific
   veteran complaint [s16]; the classic UI taught itself by printing them.
4. **Full keyboard parity is a launch feature, not a patch.** Both DF and Qud shipped mouse-first
   and spent months answering keyboard players [s16][s39].
5. **Explain silent failures where they happen.** Suspended jobs, unworn uniforms, labours "done
   by everyone", bin limits reset by a button: each was invisible state the player had to
   discover from the wiki [s11][s23][s24][s31].
6. **Feedback during the gesture.** Drag dimensions (51.05) and placement validity cost little
   and were requested for years [s27].
7. **Plan before you can afford it.** buildingplan's "place now, materials later" is so
   preferred that the vanilla model looks like a missing feature [s30].
8. **Templates, copy/paste and undo for designations.** Quickfort exists because the same
   layouts are drawn every fort [s32].
9. **A whole-fort table beats N per-unit sheets** for allocation questions (Dwarf Therapist)
   [s33]; ranking units by fitness beats alphabetical lists (DFHack squad sort) [s29].
10. **Every list row and every alert should centre the camera on its subject** (DFHack made
    the stress icons do this) [s29].
11. **Give the vertical stack a second view**, or players build one (Stonesense) [s34].
12. **Order-of-operations rules must be enforced or shown** (uniform before squad) [s24].
13. **One UI, not two** — Bay 12 could only afford one, and lost the old one entirely [s3][s6];
    Qud kept a legacy toggle during migration, which softened the switch [s35].
14. **Text and UI scale** must be adjustable; it was the Deck review's main complaint [s25].
15. **A genre-literate player brings expectations** (Zach's "genre does the work") — match
    RimWorld-style conventions where they exist instead of inventing new ones [s3].

## 8. For us

Mapped to what exists today (commander camera, cutaway level, hold-Q tool wheel, designation
tools G/X/B/T/Z, F9 Fortress panel, F3 dev panel, Slint layers):

- **Print the key on every wheel wedge and panel button** (`Dig  G`), always, not in a tooltip.
  The tool wheel already has single-letter shortcuts; make them visible on the wedges.
- **Keyboard parity as a rule for every new Slint screen**: each list navigable with arrows,
  Enter, Esc, type-to-search. Put it in the widget layer so new screens inherit it.
- **One list widget with search + sort + filter built in**, used for units, stockpile
  contents, orders, people in the inspector. Make it the only list widget.
- **"Centre on it" everywhere**: clicking any unit row, notice or designation entry moves the
  commander camera *and the working level* to it. Notices already show on the status line, so
  make them clickable.
- **Drag readout already shows the box**; keep its width × depth × levels and add the tile
  count and the order's validity summary (e.g. "312 tiles, 40 invalid: bedrock").
- **Make why-not visible**: when designations exist and workers arrive, a designation nobody
  can reach or that is blocked should get its own tint and a reason on hover — the DF
  "suspended job" lesson before we have suspended jobs.
- **Plan-first building** (buildingplan model) as the default for Build: place now, materials
  claimed later, with a visible "waiting for materials" marker.
- **Blueprint / copy-paste of a designation box** across levels, plus the existing Ctrl+Z,
  extended to a whole stamped blueprint as one undo.
- **When labours land, design the whole-fort grid first** (dwarves × labours, bulk edit), then
  the per-dwarf view as a filter of it, not the reverse.
- **The vertical stack**: we have a true 3D camera and the cutaway — that is already
  Stonesense-in-the-game. Keep the "Show above" / x-ray toggle one key away and add a depth
  readout (level vs surface) like DF's depth bar.
- **UI scale**: `VOXEL_UI_SCALE` exists as an env var; promote it to an in-game setting before
  screens multiply.
- **Get a UX read early**: a short session with a non-DF player on the embark → first dig →
  first stockpile path, before the labour and stockpile screens are built, not after.
- **Reference vs discovered** (URR): decide per screen which facts are always available (stocks,
  people) and which the player reads off the world (history, culture) — the history notices
  and person inspector are the first place this choice shows up.

---

## Sources

- [s1] Game Developer — "How Tarn Adams upgraded and optimized Dwarf Fortress for its official Steam release": https://www.gamedeveloper.com/programming/how-tarn-adams-upgraded-and-optimized-dwarf-fortress-for-its-official-steam-release
- [s2] Tanya X. Short, "Dwarf Fortress: 2 Months Later" (Kitfox, Medium; content via search summary, page 403): https://medium.com/kitfox-games-shenanigans/dwarf-fortress-2-months-later-648d1c4f34d2 ; secondary: https://gamedeveloper.com/business/kitfox-breaks-down-i-dwarf-fortress-i-600k-copies-of-success
- [s3] Game Rant interview with Tarn and Zach Adams (Nov 2022): https://gamerant.com/dwarf-fortress-interview-evolution-after-release/
- [s4] PC Gamer — adventure mode plans interview (Zach's testing hours): https://pcgamer.com/dwarf-fortress-devs-on-big-adventure-mode-plans-this-is-just-gonna-blow-everyones-mind
- [s5] PC Gamer — "Dwarf Fortress is coming to Steam with graphics and mod support": https://pcgamer.com/dwarf-fortress-is-coming-to-steam-with-graphics-and-mod-support
- [s6] r/dwarffortress answer (DFHack contributor: last fully keyboard version 0.47.05), via search; DF wiki Release information 50.01: https://dwarffortresswiki.org/index.php/Release_information/50.01
- [s7] Rock Paper Shotgun review (mirror): https://wwwrockpapershotgos.pages.dev/posts/dwarf-fortress-review-the-legendary-colony-sim-gets-a-welcome-facelift-for-steam
- [s8] The Guardian review (via Yahoo): https://uk.finance.yahoo.com/news/dwarf-fortress-review-grand-chronicle-154232298.html
- [s9] PC Gamer review (quotes via search snippet; page truncated on fetch): https://www.pcgamer.com/dwarf-fortress-review/ ; "three menus deep" reprint: https://www.everand.com/article/630671450/Dwarf-Fortress
- [s10] PCGamesN — unit look screen: https://www.pcgamesn.com/dwarf-fortress/look-steam
- [s11] TechRaptor — work system upgrade: https://techraptor.net/gaming/news/dwarf-fortress-work-system-is-getting-major-upgrade ; Steam threads: https://steamcommunity.com/app/975370/discussions/0/3716062978749047381 , https://steamcommunity.com/app/975370/discussions/0/3716062978734789352 ; DF wiki Job: https://dwarffortresswiki.org/index.php/Job
- [s12] MetaFilter thread: https://metafilter.com/197458/Strke-the-earth
- [s13] Steam discussion on UI depth: https://steamcommunity.com/app/975370/discussions/0/3716063566027280813
- [s14] DF walkthrough / beginner material on z-levels: https://df-walkthrough.readthedocs.io/en/latest/chapters/chap01-setup-starting.html ; https://dwarffortresswiki.org/index.php/Z-level
- [s15] Kitfox devlog — Fortress Mode Update 50.15: https://kitfoxgames.itch.io/dwarf-fortress/devlog/849612/fortress-mode-update-5015
- [s16] Steam discussion on keyboard control: https://steamcommunity.com/app/975370/discussions/0/3709307511569514976/
- [s17] r/dwarffortress daily questions, Jan 2023: https://redlib.ssps.io/r/dwarffortress/comments/10eyl5c/daily_df_questions_thread/j4tyc3w/?context=3
- [s18] Kitfox devlog — Adventure Mode launch plans and patch 50.12 (Mar 2024): https://itch.io/devlog/693516/adventure-mode-launch-plans-and-patch-5012.amp
- [s19] DFHack 50.11-r2 highlights (search widgets on 30+ screens), via search: https://lr.in.psf.lt/r/dwarffortress/comments/17bm2lg/dfhack_5011r2_released_highlights_search_search ; DFHack releases: https://www.github.com/DFHack/dfhack/releases?page=4
- [s20] DFHack 50.12-r1 release notes: https://github.com/DFHack/dfhack/releases/tag/50.12-r1 ; 50.08-r2: https://github.com/DFHack/dfhack/releases/tag/50.08-r2
- [s21] DF bug tracker (work detail checkbox off-screen; keyboard shortcuts on Labor screen): https://dwarffortressbugtracker.com/view.php?id=12687 , https://dwarffortressbugtracker.com/view.php?id=13291
- [s22] Kitfox devlog — Hotfix 53.08: https://kitfoxgames.itch.io/dwarf-fortress/devlog/1155160/hotfix-patch-5308
- [s23] Steam threads + wiki talk on stockpile custom settings: https://steamcommunity.com/app/975370/discussions/0/3727324132821190964 , https://dwarffortresswiki.org/index.php/40d_Talk:Stockpile
- [s24] Steam threads on uniforms/squads: https://steamcommunity.com/app/975370/discussions/0/3716062978744094305 ; DF wiki Military: https://dwarffortresswiki.org/military
- [s25] Steam Deck HQ review: https://steamdeckhq.com/game-reviews/dwarf-fortress/
- [s26] DFHack changelog (keyboard cursor Alt-K), via search: https://steamdb.info/patchnotes/13661040 ; overlay docs: https://dfhack.readthedocs.io/en/latest/docs/tools/overlay.html
- [s27] Kitfox devlog — Next steps + patch 51.05: https://kitfoxgames.itch.io/dwarf-fortress/devlog/886081/next-steps-for-dwarf-fortress-patch-5105-dwarf-fortress-dev-news
- [s28] Kitfox devlog — Patch 51.11: https://kitfoxgames.itch.io/dwarf-fortress/devlog/929632/patch-5111
- [s29] DFHack `sort` docs: https://docs.dfhack.org/en/latest/docs/tools/sort.html
- [s30] DFHack `buildingplan` docs: https://docs.dfhack.org/en/latest/docs/tools/buildingplan.html
- [s31] DFHack `suspendmanager` docs: https://docs.dfhack.org/en/latest/docs/tools/suspendmanager.html
- [s32] DFHack `quickfort` docs: https://docs.dfhack.org/en/latest/docs/tools/quickfort.html
- [s33] DF wiki — Dwarf Therapist: https://dwarffortresswiki.org/index.php/Dwarf_Therapist ; Labor: https://dwarffortresswiki.org/DF2014:Labor
- [s34] Stonesense repo: https://github.com/dfhack/stonesense ; Destructoid: https://destructoid.com/?p=58891
- [s35] Freehold Games — "The Spring Molting update is now live": https://freeholdgames.itch.io/cavesofqud/devlog/744713/the-spring-molting-update-is-now-live ; Full Cleared: https://fullcleared.com/news/caves-of-qud-gets-ui-overhaul-in-spring-molting-update/
- [s36] Freehold Games — Feature Friday, Nov 11 2022: https://freeholdgames.itch.io/cavesofqud/devlog/450719/feature-friday-november-11-2022
- [s37] PC Gamer on Qud's UI update (quote via search snippet): https://pcgamer.com/games/roguelike/my-favorite-roguelike-where-you-can-grant-a-chair-the-gift-of-sentience-just-got-a-ui-update-that-makes-it-far-more-approachable
- [s38] GamingOnLinux: https://gamingonlinux.com/2024/06/roguelike-epic-caves-of-qud-gets-the-spring-molting-update-live-with-the-fancy-new-ui
- [s39] Steam discussion on Qud's new equipment screen: https://steamcommunity.com/app/333640/discussions/0/4338735089007210451
- [s40] CDDA forums + Steam: https://discourse.cataclysmdda.org/t/how-hard-was-the-learning-curve-for-you-guys/15698/11 , https://steamcommunity.com/app/2330750/discussions/0/6197556602757688644 , https://discourse.cataclysmdda.org/t/getting-used-to-crafting/11644
- [s41] CDDA `design-user-experience.md`: https://sources.debian.org/src/cataclysm-dda/0.H-1/doc/Lore/design-user-experience.md
- [s42] Game Developer — "The 10-year journey of Ultima Ratio Regum": https://gamedeveloper.com/design/the-10-year-journey-of-ultima-ratio-regum-the-culture-generating-roguelike
- [s43] URR ModDB page (developer comments on the encyclopedia), via search: https://www.moddb.com/games/ultima-ratio-regum
- [s44] Cogmind — "Readable text fonts for roguelikes": https://www.moddb.com/games/cogmind/news/readable-text-fonts-for-roguelikes ; r/roguelikedev FAQ Friday #16 UI design: https://reddit.birdcat.cafe/r/roguelikedev/comments/3cqsaq/faq_friday_16_ui_design/csyq5gr/?context=3
- [s45] Gnomoria reviews: https://www.backloggd.com/games/gnomoria , https://www.indiedb.com/members/codemanjones/reviews , https://gamebrain.co/game/gnomoria
- [s46] Songs of Syx: https://vaporlens.app/app/1162750/songs_of_syx , https://www.realityremake.com/articles/tag/Songs+of+Syx
