# 04 — UX principles for an intuitive sim/strategy UI (mouse + keyboard)

Research note, 2026-11-02. Question: what UX/HCI research and game-UX practice say makes a UI
*intuitive* for a deep simulation game (Dwarf Fortress–style) played with mouse and keyboard,
and what that means for this game's existing UI: commander camera with z-level cutaway
("Hide above"), designation hotkeys (G dig, X deconstruct, B build, T chop, Z erase, 1–7
priority), the hold-Q tool wheel, the F9 fortress panel, M world map, Esc menu, event notices
on the status line, the unit inspector, F3 dev panel.

Tags: `[sN]` = source N in the list at the end (fetched or confirmed by search this session);
`[mem]` = from background knowledge, not re-verified today — treat as likely but check before
quoting a number.

---

## 0. The short version

1. **Clarity beats cleverness.** Into the Breach's designers cut mechanics they could not show
   clearly; Hodent's whole usability pillar is "the player must understand what they can do
   and what just happened" [s1][s3].
2. **Recognition over recall** is the single most transferable heuristic for a game with
   dozens of verbs: every hotkey must also be visible somewhere it is used [s2][s5].
3. **Menus should teach their own shortcuts.** Marking menus (radial, press-and-wait shows
   the menu, flick without waiting) are the best-studied novice→expert bridge; a mark is
   ~3.5× faster than the menu, experts still fall back to the menu to refresh memory [s7][s8].
4. **Showing a hotkey label next to a menu item is not enough** — users ignore it once the
   menu is open; hotkey use doubled only when the menu path cost something or the hotkey was
   said aloud [s9].
5. **Modes are fine if they are loud.** A tool mode (dig, build) needs two redundant cues
   (cursor + highlighted control) and no costly action may hinge on an invisible mode [s4].
6. **Tutorials only pay off in complex games — and this is one.** In-context instruction
   raised play time 16% / progress 40% in the complex game (Foldit) and did nothing in the
   simple ones; locking player freedom did not help [s10].
7. **Notifications need tiers.** Passive indicators for state, passive notices for events,
   action-required interrupts only when the player must decide; errors never only in a
   transient toast [s11].
8. **Accessibility basics are cheap if built in now:** ≥18 px text at 1080p (PC), scalable to
   200%, no information by colour alone, remappable keys, adjustable game speed [s12][s13].

---

## 1. Nielsen's heuristics, translated to games

**What the research says.** Nielsen's ten heuristics (1994, still NN/g's canonical list) are
a checklist for *inspection*, not a design method [s2]. NN/g's own games article (Kendrick,
2019) maps each to a game example [s5]:

| Heuristic | Game example [s5] | What it means for a sim |
|---|---|---|
| 1 Visibility of system status | BotW hearts update almost instantly | Every order, job, mode and level visible without asking |
| 2 Match real world | PUBG real gun names vs Apex "G7 Scout" | "Dig", "Chop", "Stockpile" — not engine words ("designation", "tile", "Order::Erase") |
| 3 User control & freedom | iOS Chess prominent undo | Undo of the last order; cancel any tool with Esc/right click |
| 4 Consistency & standards | jump on A across shooters; Mirror's Edge broke it | WASD pan, wheel zoom, Esc back/cancel, right click cancel — genre conventions |
| 5 Error prevention | Smash asks before quitting a match | Confirm only the irreversible (retire fortress, abandon) — sparingly, or "Yes" becomes reflex |
| 6 Recognition not recall | RDR2 shows horse controls only near a horse | Context key hints near the cursor / in the active tool panel |
| 7 Flexibility & efficiency | CS:GO binds | Hotkeys + remapping, wheel for mid-speed, menus for novices |
| 8 Aesthetic & minimalist | Mario Kart corner overlays | Progressive disclosure; corners for status, centre for the world |
| 9 Recover from errors | Subway Surfers "Not enough keys!" says how many | Refusals say *why* and *what to do*: "Can't dig: bedrock" |
| 10 Help & documentation | PUBG Mobile searchable help | A searchable in-game reference / command search |

**Where games differ.** Some games deliberately hide status (opponent hands in card games)
[s6]; challenge is the product, so the rule is "no *unintended* friction" — Hodent's framing:
the game should be hard because of its design, not because of its interface [s3][mem].
DF itself is the cautionary tale: two decades of "losing is fun" was in large part an
interface-difficulty tax, and the Steam edition (Kitfox, 2022) was explicitly a UX rework —
mouse menus, tutorial, readable tiles — aimed at accessibility [s14][s15].

**Do:** run a heuristic pass over every new screen; write refusal strings as cause + remedy.
**Don't:** use confirmations as the main safety net; invent vocabulary where DF/genre words exist.

---

## 2. Celia Hodent, *The Gamer's Brain* (2017) — usability and engage-ability

**What it says.** Hodent (ex-Epic, Fortnite UX director) builds game UX on three brain
limits [s3][s16]:

- **Perception is subjective** — players see what they expect; test what they actually
  perceive, not what you drew.
- **Attention is scarce and selective** — "inattentional blindness": anything outside the
  current task's focus is easily missed, including your tutorial pop-up.
- **Memory is fallible** — working memory holds a handful of items and decays in seconds;
  long-term learning needs spaced repetition and *doing*, not reading.

Her **usability pillars** (book Part II, "Usability" chapter) [s16][mem]: *signs and
feedback* (every interactive thing signals that it is interactive — the signifier — and
every action produces a response), *clarity* (the sign is understood: form follows
function), *form follows function*, *consistency*, *minimum workload* (cognitive and
physical: fewer clicks, less to remember), *error prevention / recovery*, and *flexibility*
(remapping, options). Her **engage-ability** side: motivation (competence, autonomy,
relatedness), emotion ("game feel", juice), and game flow (difficulty and **learning curve**
— onboarding is planned as a curve, not a front-loaded block).

Her onboarding talk (GDC 2016, "Gamer's Brain Part 2: UX of Onboarding") [s16]: teach by
**doing** in a meaningful context, one thing at a time, when the player needs it; players
skip text; test whether the thing was *learned*, not whether the tip was *shown*.

**Game example.** Fortnite (Save the World) iterated its build-mode signifiers through user
research: ghost previews of a wall before placing it are exactly "sign + feedback before
commitment" [mem].

**Do:** for every verb, ask "what is the sign that I *can*, the feedback that I *did*, and
the feedback that it *worked*?" (three different moments for a designation: hover preview,
placed marker, dwarf completed it). **Don't:** rely on memory of anything shown once.

---

## 3. Hick, Fitts, and the radial-menu vs toolbar vs hotkey question

**Hick–Hyman law**: decision time grows with log₂ of the number of *equally likely,
unfamiliar* choices [s17][mem]. It applies to scanning an unfamiliar menu; it mostly
vanishes for practised choices and for well-organised (grouped, alphabetic, searchable)
lists. Practical form: split large choice sets into meaningful categories, but don't over-nest
(NN/g: more than two disclosure levels gets users lost [s18]).

**Fitts's law**: time to hit a target grows with distance / size (log) [s19]. Consequences:
targets near the cursor are cheap (pop-up menus at the pointer beat a toolbar across the
screen), screen edges/corners are "infinitely deep" targets, and small icons far away are
the slowest thing on the screen.

**Pie / radial menus.** Callahan, Hopkins, Weiser & Shneiderman (CHI '88): pie menus beat
linear menus on seek time and errors (~15% faster per Hopkins' retrospective) because every
item is at the same small distance and the wedge is a large angular target [s20]. Radial
works best with **4, 8 or 12 items** — compass directions are easy to remember [s7].

**Marking menus** (Kurtenbach & Buxton, 1993–94) [s7][s8]:
- Press, **wait ~1/3 s** → the radial menu appears (novice). Press and **flick immediately**
  → the selection happens without drawing the menu (expert). Same gesture both times, so
  novice use *rehearses* expert use — "self-revelation, guidance, rehearsal".
- Field study: a mark was on average **3.5× faster** than selecting via the displayed menu;
  experts used marks heavily but **switched back to the menu to refresh memory** — expertise
  is not monotonic, so the novice path must stay available [s8] (n = 2, a case study).
- Hierarchic marking menus: accuracy holds to depth 2 at 8 items; breadth 12 or depth 3+
  gets error-prone [s7][mem].
- Later work (Bailly, Lecolinet, Nigay survey 2016; Cockburn, Gutwin et al. 2014 survey on
  novice→expert transitions): many users *never* make the transition on their own;
  **longer delays before the novice menu shows** increased expert selections [s21][s22][mem].

**Hotkeys.** Fastest for experts, invisible to novices. Grossman, Dragicevic & Balakrishnan
(CHI 2007) [s9]: showing the hotkey next to the menu item (standard practice) is largely
ignored "since once the menu is open they already have access to their desired command".
Two changes **more than doubled hotkey use** vs. the baseline (28.9% → 66.6% / 72.8% in
one experiment): **speaking the command name + hotkey on menu selection** (audio, since
vision is saturated) and **making menu selection cost something** (disabled menu item that
only tells you the key). Visual-only reinforcement did not help.

**How the three relate.** They are not competitors but rungs:
toolbar/menu (recognition, slow, discoverable) → radial at the pointer (spatial memory,
medium) → flick/hotkey (recall, fastest). The design goal is a *path* up the rungs where
each rung shows the next one.

**Game examples.** Blender's pie menus (hold key, flick) and its F3 menu search [s23];
Factorio/RimWorld bottom-bar architect menus with hotkeys in the tooltips [mem]; StarCraft /
AoE grid hotkeys laid out to match the on-screen command card (spatial mapping: the button's
position *is* its key) [mem]; DF Steam's bottom toolbar with letters on every button [s15].

**Do:** keep the same command in every rung with the same icon and the same key letter
visible; keep radial wedge count at 4/8; make the wheel work "flick-through" (press Q, move,
release → pick, without waiting for it to draw). **Don't:** nest wheels more than two deep;
put frequently used tools far from the pointer.

---

## 4. Recognition vs recall, and hotkey discoverability

**What the research says.** Recognition is easier than recall because the cue is in front
of you [s2]. Hotkeys are pure recall, so every hotkey needs a recognition surface:

- **Key shown on the control** (button label "Dig (G)", tooltip "Dig — G") — necessary,
  not sufficient [s9].
- **Contextual key hints** — RDR2 shows only the controls valid near the horse [s5];
  GAG intermediate: "Indicate / allow reminder of controls during gameplay" [s12].
- **Command search / palette** — Blender F3 searches operators by name and shows where each
  lives in the menus [s23]; VS Code's Command Palette (Ctrl+Shift+P) lists every command
  *with its keybinding beside it*, so searching teaches the shortcut [s24][mem]. In games:
  Factorio's searchable recipe/item GUIs, Oxygen Not Included's searchable build menu
  [mem]. Search turns a recall problem ("which menu was the Smelter in?") into recognition
  of a typed word.
- **Feedback in the moment of use** — when a command is used via the slow path, briefly say
  "tip: G" (the Grossman finding generalised) [s9].

**Do:** one source of truth for bindings, rendered everywhere a command appears (wheel
wedge, panel button, tooltip, search result, help overlay, notices that suggest an action).
**Don't:** ship a key that appears nowhere on screen (today: Ctrl+Z cancel-last-order, PgUp/PgDn,
Home, C, F4–F8 are only in docs).

---

## 5. Progressive disclosure

**What the research says.** Show the most important options first; reveal specialised ones
on request, on a secondary display; keep to **two levels** [s18]. The risk is discoverability
of the hidden layer — so the "more" affordance must itself be visible.

**Game examples.** RimWorld's architect menu: categories → items; the inspector shows a
summary card with tabs for the deep data (needs, health, social, log) [mem]. DF Steam: the
unit sheet is tabbed (Overview / Items / Health / Skills / Relations / Thoughts), so the
first view stays short [s15][mem]. Oxygen Not Included hides its deep simulation behind
**overlays** (oxygen, power, temperature, plumbing) the player toggles only when the problem
is relevant — disclosure by *question* rather than by depth [mem].

**Do:** a short default for every surface (inspector: name, job, mood, one alert), detail on
a tab; dev-only knobs live in F3 and never leak into player panels. **Don't:** hide the
*existence* of a feature in a submenu the player has no reason to open.

---

## 6. Feedback and undo

**What the research says.** Visibility of status needs feedback "within a reasonable time"
[s2]; for direct manipulation, ~0.1 s feels instantaneous, ~1 s keeps flow, >10 s loses
attention (Nielsen's response-time limits) [s25][mem]. Undo is the strongest form of error
tolerance: it lets people explore without fear (heuristic 3) [s2][s5].

Feedback for a *delegated* action (an order a dwarf will carry out later) has three beats:
**preview** (hover shows what will happen — Into the Breach's attack previews are the gold
standard: every enemy intent is drawn before it happens [s1]), **acknowledgement** (marker
appears instantly, sound), **completion/failure** (the job happened, or why not — RimWorld's
"no colonist can reach this" icon on a blueprint [mem]).

**Do:** previews for anything spatial (dig box, path, felling direction); instant local
acknowledgement even before the sim confirms; failure reasons attached *to the thing*, not
only in a log. **Don't:** let an order silently do nothing (today: designations nobody
consumes yet — fine for now, but the marker must later say "waiting / unreachable").

---

## 7. Modal vs modeless tools, mode errors

**What the research says** [s4] (Laubheimer, NN/g 2019): a mode = same input, different
result. Mode errors come from (a) not noticing the mode, (b) features only reachable in one
mode. Recommendations: **show the active mode with at least two redundant cues** (e.g.
highlighted tool + cursor change; Photoshop does tool icon + cursor + options bar); label
modes; **avoid modes where errors are costly**; give a helpful message when input looks
meant for another mode; **quasimodes** (spring-loaded: active only while a key is held) all
but eliminate mode slips but are less discoverable and tiring for long sequences.

**Game examples.** RTS build placement: the cursor turns into the building's ghost — the
mode *is* the cursor [mem]. DF Steam shows the active designation mode in a bar and a
cursor box [s15][mem]. Blender holds-to-transform (G then mouse) is a short, cancelable mode
with the header text replaced by the mode's instructions [mem].

**Do:** for every tool mode: cursor glyph/tint + highlighted wedge/button + a one-line mode
banner with its keys ("Dig — click two corners · Ctrl erase · Esc cancel · 1–7 priority");
Esc and right click always leave the mode; make the hold-Q wheel a quasimode (it already
is) and the tools themselves sticky modes with that banner. **Don't:** make the same click
mean "select unit" in one mode and "dig" in another without the cursor changing.

**The z-level cut is a mode too.** "Hide above" on vs off changes what Space/Shift, the wheel
and PgUp/PgDn do (level stepping vs free vertical camera; zoom vs no zoom). That is exactly
the hidden-state pattern NN/g warns about [s4]: it needs a persistent indicator *and* the
controls' current meaning shown.

---

## 8. Notifications and attention

**What the research says** [s11] (NN/g "Indicators, validations, notifications"):
- **Indicators**: passive, contextual, conditional (an icon on the thing). Use for state.
- **Validations**: tied to the user's own input, must be acted on, say how to fix.
- **Notifications**: system events the user didn't directly cause; **passive** (badge,
  corner toast — easy to miss, never for critical info) vs **action-required** (intrusive,
  must be dismissed — needs enough context because the user didn't trigger it).
- Errors must not live only in transient toasts; notifications unrelated to the current goal
  get ignored.

Alert fatigue is the known failure mode: when everything interrupts, players learn to
dismiss without reading [mem; widely documented in clinical-alarm research].

**Game examples.** RimWorld splits **messages** (transient top-left lines), **letters**
(persistent envelope icons, colour-coded by severity: blue good, grey neutral, yellow bad,
red threat; some pause the game and demand a choice) and **alerts** (persistent right-side
conditions such as "colonist needs rescue" that stay until the condition clears) — three
tiers matching NN/g's taxonomy, and every letter has a "jump to" target [s26]. Players
still mod-filter them, which shows the need for per-type control [s26]. DF has announcement
vs pause-and-recenter settings per event type in its `announcements` config [mem].
Stellaris/Crusader Kings 3 let you choose per message type: log, toast, or pause [mem].

**Do:** three tiers — *state indicators* (on the entity / minimap: hungry, idle,
unreachable job), *event log + transient notice* (birth, arrival; click to jump), *interrupt
with optional auto-pause* (siege, death of a named dwarf, cave-in) — with per-type settings;
colour + icon + word for severity; a persistent, scrollable log behind the status line.
**Don't:** put a refusal ("can't embark here") only in a fading status line; let a
low-value event push a critical one off a single-line display.

---

## 9. Onboarding

**What the research says.** Andersen et al., CHI 2012, 45,000+ players, three games of
different complexity [s10]:
- Tutorials raised play time up to **29%** and progress up to 75% in the **complex** game
  (Foldit); **negligible** effect in the two simpler games.
- **In-context, just-in-time** instructions vs an up-front manual: **+16% play time, +40%
  progress** in Foldit, no effect in the simple games.
- **Restricting freedom** (stencil tutorials that lock everything but the target) showed
  **no** learnability benefit.
- **On-demand help** helped in Foldit, did nothing or hurt in the simpler games.
- "Players … seem to learn more from experimentation than from reading text."

Hodent: learning by doing in a meaningful context, spread over time; test that it was
learned [s3][s16]. Into the Breach: **show, don't describe** — animated weapon previews were
"a thousand times more effective" than sentences that left testers confused [s1]. GAG
basic: "Include interactive tutorials"; intermediate: reminders of controls and objectives
during play [s12]. DF Steam shipped a tutorial and contextual help as the core of making the
game approachable [s14][s15].

**Game examples.** RimWorld's "Learning Helper": a small, dismissible checklist of concepts
that appears when relevant (first time you open the architect menu, first raid) and ticks
itself off when you *do* the thing [mem]. Factorio's interactive "tips and tricks" mini
simulations are opened from context [mem]. Portal's level design teaches without text [mem].

**Do:** a concept list that triggers in context and completes on *action* (first dig box
placed, first level change, first chop); short animated/diagram tips over text; nothing
modal at game start beyond the embark choice. **Don't:** a front-loaded multi-page tutorial
or a locked-freedom stencil sequence.

---

## 10. Information hierarchy and visual design for dense HUDs

**What the research says.**
- **Gestalt grouping** (proximity, similarity, common region, continuity): related controls
  share a container/alignment; unrelated ones are separated by space, not just lines [mem;
  NN/g Gestalt articles].
- **Hierarchy**: the world is the primary content; status at edges/corners; the player's
  focus (cursor) is where transient, task-relevant info belongs (Fitts + attention) [s5][s3].
- **Colour**: never the only carrier of meaning (GAG basic, XAG 103) [s12][s13];
  ~8% of men and ~0.5% of women have a colour-vision deficiency, red–green most common
  [mem]; Okabe–Ito is a categorical palette that stays distinguishable under protan/deutan/
  tritan vision [s27]. Pair hue with **shape/icon, pattern, or label**; check luminance
  contrast, not hue difference.
- **Icon + text**: icons alone are recognised reliably only for a handful of universal
  symbols; labels (or labels on hover with short delay) are what make icons usable [mem;
  NN/g "Icon usability"]. XAG 103: a text label next to a symbol lowers cognitive load [s13].
- **Readable text**: XAG 101 minimum default **18 px body height at 1080p on PC (36 px at
  4K)**, scalable to **200%**, at least one sans-serif option, sentence case for lines,
  outline/shadow for text over the world (Fenyx example) [s13]. GAG: readable default size,
  high contrast, "allow interfaces to be resized" [s12].
- **Readability over style in a stylised UI** — Hearthstone's team ("our game is UI") kept
  the immersive frame but put clarity first in the cards' information areas [s28].

**Game examples.** Into the Breach restricted the *design* (e.g. limited status effects per
move) so the UI could stay clear [s1][s29]. ONI/RimWorld use consistent per-resource colour
plus icons in overlays [mem]. DF Steam redrew everything as graphical tiles specifically to
make the map legible [s15].

**Do:** a type scale with ≥18 px body at UI scale 1 at 1080p; one semantic colour set
(good/warn/bad/info/selection) that is also encoded by icon; a single designation tint per
type *plus* a pattern/icon per type. **Don't:** tiny pixel-font labels at 1× scale; red vs
green as the only difference between "valid" and "invalid" previews.

---

## 11. Accessibility checklist (the relevant items)

From gameaccessibilityguidelines.com [s12] and the Xbox Accessibility Guidelines [s13]:

| Guideline | Level | Status here |
|---|---|---|
| Allow controls to be remapped | GAG basic (motor) | Not built — hotkeys are hard-coded |
| Easily readable default font size; allow font size adjust | GAG basic / advanced; XAG 101: 18 px @1080p PC, scale to 200% | `VOXEL_UI_SCALE` env only (1 or 2 pixel-exact) |
| No essential info by colour alone | GAG basic; XAG 103 | Check designation tints, civ marker colours, refusal-darkening |
| High contrast text/UI vs background; contrast option | GAG basic / intermediate; XAG 102 | Fortress palette (umber + gold) — measure ratios |
| Allow interfaces to be resized | GAG intermediate | Env var only; needs an in-game option |
| Reminder of controls during gameplay | GAG intermediate | Not built (see §4) |
| Include interactive tutorials | GAG basic | Not built (see §9) |
| Adjust game speed; progress through text at own pace | GAG basic | No pause/speed yet — DF-style pause is a core expectation |
| Use simple clear language | GAG basic | Watch engine words in notices/refusals |
| Avoid precise timing essentials | GAG advanced | Fine (commander play is untimed); hold-Q release timing is the one exception — keep the "release in middle = stay open" fallback |

---

## 12. Playtesting UI

**What the research says.** NN/g: **think-aloud** testing with ~5 users per round finds most
of the major usability problems; run several small rounds, not one large one [mem; NN/g
"Thinking aloud" and "Why you only need to test with 5 users"]. Hodent: game user research
separates *usability* tests (can they understand it — think-aloud, first-time users, task
based: "dig a room two levels down") from *engagement* measures (do they want to) [s3][s16].
Into the Breach's clarity changes came from handing the game to outside players and seeing
"who was doing what" confusion [s1]. Andersen et al. show the value of instrumented A/B
data at scale [s10].

**Do:** a standing first-time-user script (embark → dig a staircase down → designate a
room → chop trees → find a dwarf and read their info → read a notice) run with 3–5 people
who never saw the game, think-aloud, recorded; log every refused action and every
open/close of help as telemetry; fix, rerun. **Don't:** test with the developer or anyone
who read CLAUDE.md; ask "is it intuitive?" instead of watching a task.

---

## 13. For us — applying it to the existing UI

**Tool wheel (hold Q)**
- Keep it at ≤8 wedges per ring (5 tools + cut today = fine); a second ring only for a
  category (e.g. Build → materials), never a third [s7].
- Make it a true marking menu: press Q + move toward a wedge + release **before the wheel
  draws** should pick it (the drawn wheel appears only after ~250–350 ms of hesitation). That
  makes every novice selection rehearse the expert flick [s7][s8][s21].
- Show the direct hotkey on each wedge ("Dig · G") and, when a tool is picked through the
  wheel, flash "tip: G" by the cursor for the first N uses — the Grossman cost/feedback
  effect, without nagging forever [s9].
- Hold-Q is a quasimode → low mode-error risk; but it conflicts with "Q" conventions in
  some genres — make it remappable.

**Hotkeys (G/X/B/T/Z, 1–7, C, PgUp/PgDn, Home, Ctrl+Z, F3/F5/F8/F9, M, Esc)**
- One binding table in code; every surface that shows a command renders its key from it
  (wedge, F9 panel button, tooltip, help overlay, command search, mode banner) [s2][s9].
- Add a **command search** (e.g. `/` or Ctrl+K): every tool, panel, overlay and toggle by
  name, with its key beside it, Blender-F3 / VS Code style [s23][s24]. Cheap, and it scales as
  the sim grows (jobs, stocks, work orders).
- Add a held-key **controls overlay** (e.g. hold F1) listing the keys valid *in the current
  mode* (RDR2-style contextual hints) [s5][s12].
- Remapping before release (GAG basic) [s12].

**Designation tools as modes**
- Two-plus redundant cues per active tool: cursor glyph/tint by tool, highlighted
  wedge/button, and a one-line **mode banner** with that tool's keys and the first-click /
  second-click state ("Dig — corner 1 set · click corner 2 · Esc cancel") [s4].
- Previews before commit (box outline over exactly the cells that will change, with invalid
  cells marked by **pattern + colour**, not red alone) — the Into the Breach rule [s1][s12].
- Designation tints distinguishable without hue (hatch/icon per type) [s12][s13].
- Later: each designation shows its job state (waiting / claimed / unreachable + reason).

**Cutaway level indicator ("Hide above" / "Show above")**
- Treat the cut as a mode: a persistent, always-visible widget showing ON/OFF, the current
  level as a number *and* relative to the surface ("z −3 (3 below surface)"), and what
  Space/Shift/wheel do right now [s4].
- Pair it with a side-on depth gauge (DF Steam and ONI-style) so "where am I vertically" is
  recognition, not recall [s15][mem].
- When the camera goes underground (dark backdrop, closer fog), echo it in the indicator
  ("Underground — inside rock") so the look change is explained.

**Notices (status line)**
- Split into tiers: (1) indicators on entities/minimap, (2) a notice feed that is also a
  persistent, scrollable **event log** with click-to-jump, (3) interrupts (optional
  auto-pause) for the few events that need a decision [s11][s26].
- Per-type settings (log / notice / pause) once there are more than ~10 event kinds
  (DF announcements, CK3 message settings) [mem].
- Refusals (embark refused, can't dig bedrock, play refused off-map) are *validations*: show
  them next to the cursor/selection with cause + remedy, not only on the status line [s11][s5].
- Severity = icon + colour + word ("⚠ Death"), not colour alone [s12].

**Inspector (single selected dwarf)**
- Progressive disclosure: a short summary card (name, age, job, mood, one most-urgent
  issue), details behind tabs (relations, history, health) — two levels max [s18].
- Every name in it (spouse, parents, site) is a link that selects/jumps — recognition over
  recall for the history graph [s2].
- As needs/jobs arrive, surface the *why* ("idle: no reachable work") — feedback on
  delegated actions [s1][s3].

**World map (M) and embark**
- The embark choice is the first thing a new player meets: put the refusal reason on the
  hovered tile (tooltip "too close to Site X"), not only darkening — colour-alone rule and
  validation-near-input rule [s11][s12].
- Map layers (biome, relief, temperature, rain, regions) need legends with labels; site
  markers already use filled/hollow (shape) as well as colour — keep that pattern for every
  map encoding [s12][s27].
- Fit-to-view and zoom-at-cursor are right (Fitts / consistency with map conventions) [s19].

**Panels (F9 fortress, F3 dev, Esc menu)**
- F9/F3/M are recall-only toggles: give them a small always-visible launcher strip
  (icons + key letters), placed on an edge (Fitts: edge targets are easy) [s19].
- Keep F3 strictly dev; player features ("Complete all orders" analogues) never live there.

**Onboarding and testing**
- Contextual "learning helper" checklist that completes on action (first dig box, first
  level change, first chop, first embark), shown in the corner, dismissible — no
  front-loaded tutorial; this game is the "complex" case where in-context help paid off
  [s10].
- Run the §12 first-time-user script with 3–5 outsiders after each major UI change.

**Accessibility debt to schedule now**
- In-game UI scale option (not env-only), ≥18 px body text at 1080p at the default scale,
  remappable keys, pause + speed control, colour-independent encodings [s12][s13].

---

## Sources

- [s1] Game Developer, "Into the Breach dev on UI design: sacrifice cool ideas for the sake of
  clarity every time" — https://gamedeveloper.com/design/-i-into-the-breach-i-dev-on-ui-design-sacrifice-cool-ideas-for-the-sake-of-clarity-every-time-
  ; GDC 2019 "Into the Breach Design Postmortem" (Matthew Davis) — https://gdcvault.com/play/1025772/-Into-the-Breach-Design
- [s2] NN/g, "10 Usability Heuristics for User Interface Design" — https://www.nngroup.com/articles/ten-usability-heuristics/
  ; "Recognition and Recall" — https://www.nngroup.com/articles/recognition-and-recall/
- [s3] Celia Hodent, *The Gamer's Brain: How Neuroscience and UX Can Impact Video Game Design*
  (CRC Press, 2017) — https://routledge.com/9781498775502 (book; chapter list confirmed,
  checklist contents from memory [mem])
- [s4] Page Laubheimer, NN/g, "Modes in User Interfaces: When They Help and When They Hurt
  Users" (2019) — https://www.nngroup.com/articles/modes/
- [s5] Alita Kendrick, NN/g, "10 Usability Heuristics Applied to Video Games" (2019) —
  https://www.nngroup.com/articles/usability-heuristics-applied-video-games/
- [s6] UX Collective, "Usability heuristics in game design" — https://uxdesign.cc/usability-heuristics-in-game-design-29a324177d4e
- [s7] Kurtenbach & Buxton, "The Design and Evaluation of Marking Menus" (Kurtenbach PhD
  thesis, 1993) — https://research.autodesk.com/app/uploads/2023/03/the-design-and-evaluation.pdf_recCzWoTgz02K2Ubb.pdf
- [s8] Kurtenbach & Buxton, "User Learning and Performance with Marking Menus", CHI '94 —
  https://billbuxton.com/MMUserLearn.html
- [s9] Grossman, Dragicevic & Balakrishnan, "Strategies for Accelerating On-line Learning of
  Hotkeys", CHI 2007 — https://www.dgp.toronto.edu/~ravin/papers/chi2007_learninghotkeys.pdf
- [s10] Andersen et al., "The Impact of Tutorials on Games of Varying Complexity", CHI 2012 —
  https://grail.cs.washington.edu/projects/game-abtesting/chi2012/ (PDF:
  https://grail.cs.washington.edu/wp-content/uploads/2015/08/andersen2012tio.pdf)
- [s11] NN/g, "Indicators, Validations, and Notifications: Pick the Correct Communication
  Option" — https://www.nngroup.com/articles/indicators-validations-notifications/
- [s12] Game Accessibility Guidelines, full list — https://gameaccessibilityguidelines.com/full-list/
- [s13] Xbox Accessibility Guidelines 101 (text display) —
  https://learn.microsoft.com/en-us/gaming/accessibility/xbox-accessibility-guidelines/101 ;
  102 (contrast) — https://learn.microsoft.com/en-us/gaming/accessibility/xbox-accessibility-guidelines/102 ;
  103 (additional channels) — https://learn.microsoft.com/en-gb/gaming/accessibility/xbox-accessibility-guidelines/103
- [s14] PC Gamer, "Tutorials and mouse support could make Dwarf Fortress on Steam vastly
  easier to play" — https://pcgamer.com/tutorials-and-mouse-support-could-make-dwarf-fortress-on-steam-vastly-easier-to-play
- [s15] PC Gamer, "Dwarf Fortress Steam Edition threatens to make it playable for just about
  anyone" — https://pcgamer.com/take-a-good-long-look-at-how-accessible-dwarf-fortress-steam-edition-will-be
  ; PCGamesN, "Dwarf Fortress has a much better UI on Steam" — https://www.pcgamesn.com/dwarf-fortress/menus
- [s16] GDC talks: Hodent, "The Gamer's Brain: How Neuroscience and UX Can Impact Design"
  (GDC 2015) and "The Gamer's Brain Part 2: UX of Onboarding and Player Engagement"
  (GDC 2016) — https://www.gamedeveloper.com/design/attend-gdc-for-epic-insight-into-the-psychology-of-player-engagement
  ; https://gamedeveloper.com/design/video-making-better-games-through-neuroscience---and-good-ux
- [s17] Laws of UX, "Hick's Law" — https://lawsofux.com/hicks-law/ [mem: URL not re-fetched]
- [s18] NN/g, "Progressive Disclosure" — https://www.nngroup.com/articles/progressive-disclosure/
  [URL from memory; content confirmed via secondary quotes, incl. the two-level guidance]
- [s19] NN/g, "Fitts's Law and Its Applications in UX" — https://www.nngroup.com/articles/fitts-law/ [mem: URL not re-fetched]
- [s20] Callahan, Hopkins, Weiser & Shneiderman, "An Empirical Comparison of Pie vs. Linear
  Menus", CHI '88 — https://sites.umiacs.umd.edu/node/16962 ; Hopkins retrospective —
  https://www.osnews.com/story/30371/pie-menus-a-30-year-retrospective/
- [s21] Bailly, Lecolinet & Nigay, "Visual Menu Techniques", ACM Computing Surveys 2016 —
  https://www.academia.edu/69576829/Visual_Menu_Techniques
- [s22] Cockburn, Gutwin, Scarr & Malacria, "Supporting Novice to Expert Transitions in User
  Interfaces", ACM Computing Surveys 47(2), 2014 [mem: not fetched]
- [s23] Blender Manual, Menu Search (F3) — https://docs.blender.org/manual/en/3.4/interface/controls/templates/operator_search.html
- [s24] VS Code docs, Command Palette — https://code.visualstudio.com/docs/getstarted/userinterface#_command-palette [mem: not fetched]
- [s25] NN/g, "Response Times: The 3 Important Limits" — https://www.nngroup.com/articles/response-times-3-important-limits/ [mem]
- [s26] RimWorld Wiki, "Events" (letters, colours, pausing events) — https://rimworldwiki.com/wiki/Events
  ; "Block Unwanted Minutiae" mod (per-type blocking of messages/letters/alerts) — https://www.nexusmods.com/rimworld/mods/286
- [s27] Okabe & Ito, Color Universal Design palette — https://search.r-project.org/CRAN/refmans/see/html/palette_okabeito.html
- [s28] GDC 2015, Derek Sakamoto, "Hearthstone: How to Create an Immersive User Interface" —
  https://www.gdcvault.com/play/1022036/ ; coverage — https://gamedeveloper.com/design/video-designing-an-immersive-user-interface-for-i-hearthstone-i-
- [s29] 80.lv, "GDC 2019: An Inside Look at Into the Breach" — https://80.lv/articles/gdc-2019-an-inside-look-at-into-the-breach
