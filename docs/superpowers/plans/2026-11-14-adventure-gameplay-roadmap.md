# Adventure gameplay roadmap: quests, mastery, combat, and what sites must offer

Date: 2026-11-14. Status: **proposal, not approved**. This is a design and a staged roadmap, not a
task-level plan: each stage below becomes its own implementation plan once the design is agreed.
Out of scope here: site generation (towns, dungeons, dwarven fortresses — next plan, another agent),
the Minecraft import step, the code rename, fortress-mode visits/ratings.

**Partly superseded 2026-11-16** (`docs/game-design.md`, revision of that date): quests are authored in
packages and instantiated by the coordinator (`specs/2026-11-16-quests-in-the-world-design.md`), not
generated as coordinator world objects (§5.1); renown is a place in history, not a number (§5.4); the
fortress is a settlement its mayor runs in a body, and embark is going (§7, §8's `debug embarked`).
The character, combat, site-fixture and challenge-tag sections still stand as proposals.

Everything marked **(call)** is a decision made while writing this, not something Dawid said. Section 9
lists the ones that change the most if they are wrong.

## 1. What is being built, and why

Two symbiotic loops in one world:

- **Adventure**: find a quest → attempt → fail → prepare → attempt again → succeed → renown. Renown is
  a score, not power.
- **Fortress**: keep a settlement alive. Some troubles (a monster nesting in the region) can only be
  solved by an adventurer — a player, or in single player an AI one.

The progression rule that shapes everything: **power is capped and re-shapeable, so preparation is the
game, not levelling.** A character at the cap who has prepared correctly can complete any quest sized
for one player. What persists across attempts is knowledge of the quest, the build you trained into,
and the kit you packed — never a bigger number.

Anti-goals: power creep; a quest that only an older character can do; respec through a menu (it must be
played); a retraining loop that feels like a grind (see 3.4).

## 2. Where the code is today

| Area | Exists | Missing |
|---|---|---|
| Adventure mode | `PlayMode::Adventure`, `Order::SetMode`, `StartAdventure(SiteId)` + `Claim::Adventure`, A* `Walker`, first person (F5), mining, placing (`game/src/adventure.rs`, `coordinator/mod.rs`) | character, stats, any goal |
| Inventory | 36 slots, 9-slot hotbar, chests, item piles (`inventory.rs`, `containers.rs`, `item_piles.rs`) | tools, equipment, crafting; items are blocks + "stick" |
| Bodies | `Ecs` components `kind, motion, walker, wander, animator, …`; `EntityKind { Player, Slime, Person, Body }` (`state.rs`) | health, damage, death, hostile AI |
| Creatures | ~763 `.bbmodel` creatures with colliders (`creature_shapes.rs`: "No entity uses them yet"); passive slimes | species data, spawning, behaviour |
| Sites | `history::Site` (name, civ, region, members), people who wander, legends log, world-map markers | site kinds, anything built in the terrain, fixtures |
| Quests, skills, levels, renown | nothing | everything |
| HUD | hotbar, mining bar, inventory screen, fortress panel (Slint, `UiUpdate`/`UiAction`) | health, abilities, character sheet, quest log, notice board |

The inventory/chest feature (`plans/2026-11-05-adventure-survival-items.md`) is the template for how a
feature crosses every layer: `Client` fields → `migrate.rs` `arrive` → `player.rs` `parked_client` →
`UiAction`/`UiUpdate` → `HomeView` → `project_ui` → `slint_overlay.rs` → `.slint`.

## 3. The character system

### 3.1 Names (suggestions)

The game is called Errantry, so the vocabulary can come from it:

| Concept | Proposed name | Why |
|---|---|---|
| The levelling phase | **Squirehood**; a level is a **step** | Reads as training, not as the game |
| Reaching the cap | **Being dubbed an Errant** | "The cap" becomes the moment the game starts, a title rather than a ceiling |
| Total mastery budget | **Focus** | "You only have so much focus" explains why a tree fades when another grows |
| Per-tree value | **Mastery** (as given) | |
| Skill point | **Knack** | A slot you fill, not a currency you bank |
| Score | **Renown**, with rank titles | As given |
| Level used for scaling | `effective_step` in code; shown as "weakened / emboldened" | Never shown as a level number after dubbing |
| Fortress player | **Genius loci** — the spirit of the place; in UI "the Warden of <site>" | A real folklore figure that guards and shapes a place without speaking |

### 3.2 Rules **(call — this is my reading of the mastery description; confirm it)**

- A handful of **trees** (start with five: *Blade*, *Bow*, *Ward* (defence), *Wayfaring* (climb, swim,
  sneak, light), *Craft* (traps, locks, tonics)). Each tree has a **mastery** value `0..=MASTERY_MAX`.
- **Mastery rises by doing** things that belong to the tree (hits landed with a blade, locks picked,
  metres climbed). Gains come from a table of `(action, tree, amount)`.
- The sum of all masteries may not exceed **Focus** = `focus_for(effective_step)`. At the cap, Focus
  holds about two and a half full trees out of five.
- When a gain would exceed Focus, the same amount is **taken from another tree**. The player marks each
  tree *held* or *released* (three-state arrow, as in Ultima Online's skill lock); the drain takes from
  released trees, lowest-first by a fixed order. If every other tree is held, the gain is refused.
- Each step grants **knacks**. A skill node has a mastery requirement in its tree and costs one knack.
  **Mastery decides which nodes are reachable (depth); knacks decide how many you hold (breadth).**
- When a tree's mastery falls below a node's requirement (minus a small hysteresis so it does not
  flicker), the node **lapses and its knack returns**. That is the only way to get a knack back —
  re-speccing is done by training something else.
- `effective_step` can differ from the real step (a curse, a blessing, a quest rule, party scaling). If
  Focus drops below the current sum, trees go **dormant** proportionally and return when it rises —
  nothing is lost to a temporary effect.
- Steps come from **first times** (first quest of a kind, first site discovered, first kill of a
  species), not from repetition, so the cap arrives in a few hours and cannot be farmed.

### 3.3 Skills

Nodes are data (`assets/skills.toml`, compiled like block props). Two kinds: **passive** (resist
poison, see in the dark, carry one more tonic) and **active** (a shield bash, a grapple line). Active
skills sit on a 4-slot ability bar beside the hotbar **(call)**. Start with about six nodes per tree;
every node should answer some challenge tag from section 5, otherwise it does not belong in the first set.

### 3.4 Keeping retraining from becoming a grind

The risk in "respec by playing" is that preparation turns into hitting a dummy for an hour. Guards:

- **Trainers and training grounds in towns** multiply mastery gain in their tree. A full swap of one
  tree should cost roughly 15–20 minutes at a trainer **(call, tunable)**.
- Mastery gained against real opposition counts for more than against a dummy.
- Lapsed nodes are remembered: re-reaching the requirement re-offers them in one click.
- All rates are named consts in one file, so the pacing can be tuned without touching logic.

### 3.5 Inventory

Adventurers carry **the 9-slot hotbar plus three worn slots** (armour, two trinkets); the 27-slot main
grid is fortress/creative only **(call)**. A **locker at an inn** stores spare kit, so "packing for the
quest" is a real choice made in town. Items gain a small `ItemKind` table (weapon, armour, tonic, tool,
quest item) next to the existing block items.

### 3.6 Where it lives in the code

- `game::character::Character { step, step_xp, mastery: BTreeMap<TreeId, u16>, hold: BTreeMap<TreeId, Hold>, nodes: BTreeSet<NodeId>, lapsed, renown, firsts, quest_log }` —
  a plain struct deriving `StateHash`, with all mutation in `*_safe` methods.
- Players: `Client.character`. That means classifying it in `Client::arrive` (it travels), keeping it in
  `parked_client`, and bumping `FORMAT_PLAYER_RECORD` to 2 with a reader for 1.
- AI adventurers: the same struct in a new `Ecs.character` component (classified in `carry_components`).
- `Vitals { hp, max_hp, … }` is a separate `Ecs.vitals` component because monsters need it too.

## 4. Combat and creatures

Combat is the first thing the skill system needs something to act on, so it comes before skills.

- **Vitals and defeat.** Health, damage with a **damage type** (cut, pierce, blunt, fire, poison, …),
  fall damage. A defeated player is **not dead**: they wake at their last rest point (an inn bed, a
  camp) with their kit, and the attempt is over **(call)**. Permadeath would fight the try-fail-retry loop.
- **Attacks.** An `Order::Attack`/ability input resolved in the sim: reach + arc test for melee, a
  stepped projectile for ranged, all in deterministic math (`libm`, no `f32` transcendentals), any
  chance through `roll(tick, subject, salt)`. Damage to something in a neighbour region is an
  `Intent::Damage`, never a direct write.
- **Creatures.** `EntityKind::Creature { species }` using the existing model roster. A species table
  (`assets/creatures.toml`): health, speed, attacks, **resistances and weaknesses by damage type**,
  senses, behaviour. Add models to the compiled set only as species are used.
- **Brain.** One small deterministic state machine per creature: idle → notice → chase (reusing
  `Walker`/`PathPlan`) → attack → leash home. No parallelism inside a region; neighbours are read through
  `WorldView`.
- **Spawning.** Creatures come from **spawn fixtures** (section 6), not from a global spawner, so a
  dungeon's population is a property of the site and can be saved, cleared and repopulated.
- Client prediction needs nothing new: the region sim is already predicted and rolled back.

## 5. Quests

### 5.1 Shape

```
Quest { id, giver: SiteId, target: QuestTarget, kind, tier, challenge: Vec<ChallengeTag>,
        renown, state: Open | Closed { by, tick } }
QuestTarget = Site(SiteId) | Lair { region, fixture } | Trouble { fortress: SiteId, trouble }
kind        = Slay | Retrieve | Clear | Escort(later) | Deliver(later)
```

- **Quests are world objects** owned by the coordinator next to `History` (they span regions and
  hosts, and a region must never write a table another host's part writes). They are generated
  deterministically from history, site data and fortress troubles.
- **A player's quest log** (accepted quests, objective progress, notes) is part of `Character`, so it
  travels with the player and is saved with them.
- Objectives are detected in the region (creature with a quest tag defeated, quest item picked up,
  fixture reached) and written to the log. **Turning in at the giver's notice board** closes the quest
  through the coordinator, awards renown and writes a legends entry (`LogKind::QuestCompleted`).
- World-changing quests (slay the nesting beast) close for everyone on first completion; everyone who
  holds the quest and took part is credited. Delves into a dungeon reopen after it repopulates **(call)**.

### 5.2 The puzzle: challenge tags

A quest is a puzzle because its target carries **challenge tags**, each with known counters:

| Tag | Example | Counters |
|---|---|---|
| `Resists(type)` / `WeakTo(type)` | a beast that shrugs off blades | another weapon tree, a coated blade |
| `Dark` | unlit depths | torches (a hotbar slot), Wayfaring night-sight |
| `Flooded` | a sunken passage | swimming node, a breathing tonic |
| `Poison` | gas pockets, venom | Ward resistance, antidotes |
| `Locked` / `Trapped` | sealed vault | Craft nodes, keys found elsewhere |
| `Heights` | a shaft with no stairs | climbing node, rope |
| `Swarm` / `Brute` | many weak / one strong | area skills / defence and burst |

With 9 slots and limited Focus you cannot counter everything, so each quest asks for a specific fit —
and harder quests ask for more than one character can hold, which is the reason to form a party.

### 5.3 Knowledge is the progress

The notice board shows only **rumours** (one or two tags). Each attempt **reveals what you met** and
records it as notes in the quest log. A failed attempt therefore always pays out information, and the
next preparation is better informed. This is the one thing that accumulates, and it is per quest.

### 5.4 Renown

`renown: u32`, rank titles at thresholds, awarded by quest tier with a first-completion bonus. It gates
nothing at first; a tier of quests may later ask for a rank.

## 6. What gameplay needs from sites (contract for the site-generation plan)

Gameplay does not care how a site is built, only that a generated site exposes **fixtures** — typed
points and volumes in region coordinates, saved with the region (`RegionMeta`):

| Fixture | Used by |
|---|---|
| `NoticeBoard` | taking and turning in quests |
| `RestPoint` (inn bed, camp) | where a defeated adventurer wakes; locker |
| `Trainer { tree }` / training ground | fast mastery gain |
| `Locker`, `Merchant` (later) | preparation |
| `Entrance`, `Waypoint` | travel and path targets for AI adventurers |
| `Spawn { table, count, respawn }` | creatures of a dungeon or lair |
| `Boss { species }` / `QuestItem` | Slay / Retrieve objectives |
| `Hazard { tag, volume }` | Dark, Flooded, Poison |
| `Door { lock }`, `Trap { kind }` | Locked, Trapped |
| `Loot { table }` | chests |

Plus `Site.kind: Town | Dungeon | Fortress | Lair` on the history record, and a site-level list of
challenge tags derived from its fixtures (what the rumours draw from). Site layout must stay a pure
function of site and seed, as `generate_region` is; only fixture *state* (cleared, looted, opened) is saved.

Until site generation exists, **a dev order stamps a test town and a test cave** (a board, a bed, a
trainer, three spawns, a boss) into the debug world, so every stage below is playable and testable first.

## 7. Fortress troubles and AI adventurers

- A **trouble** is a fortress problem only an adventurer can end. First one: a beast nests in the
  region (a `Lair` fixture with a boss and tags). It raises a `Trouble` quest on the world board.
- A player adventurer can take it like any quest. In single player an **AI adventurer** does: a history
  `Person` with a generated `Character` (a build drawn from the same trees), sent as a `Journey` with a
  new `Purpose::Quest`, given a body on arrival through the existing people handover.
- The AI adventurer runs the creature brain with an adventurer behaviour (walk to target, fight with
  its skills, retreat when low) through the **same combat code** as players.
- It can fail. The next one to come is better matched to the tags the last one revealed — the same
  try-fail-prepare loop, seen from the other side, and a reason for the fortress to make itself worth
  visiting (a bed, a locker, later an inn).
- Outcomes are only simulated while the fortress region runs; an unresolved trouble waits **(call)**.

## 8. Stages

Each is a vertical slice that ends playable in `client debug embarked --adventure`, and becomes its own
plan. Every stage that adds sim state runs `just determinism-wasm` and, where it crosses a seam,
`just test-threads`.

| # | Stage | Delivers | Main touch points |
|---|---|---|---|
| 0 | Test bed and site contract | `Site.kind`, `Fixture` types saved per region, dev order stamping a test town + cave | `history/records.rs`, `portable.rs`, `orders.rs`, `debug_launch.rs` |
| 1 | Vitals and defeat | `Ecs.vitals`, damage types, fall damage, rest point, waking after defeat, health bar | `state.rs`, `migrate.rs`, `hud.slint` |
| 2 | Hostile creatures | species table, `EntityKind::Creature`, brain, spawn fixtures, creatures hurt players | new `creature.rs`, `creature_shapes.rs`, client `bridge.rs` |
| 3 | Fighting back | attack order, melee and ranged resolution, `Intent::Damage`, weapon/armour items, adventurer inventory limits, worn slots | `orders.rs`, `intents.rs`, `items.rs`, `inventory.rs` |
| 4 | Character | `Character` on `Client`, steps, Focus, mastery gain and drain, hold/release, knacks, lapsing, first trees, character sheet, ability bar, record format 2 | new `character.rs`, `player.rs`, `world-store/codec.rs`, new `.slint` |
| 5 | Quests and renown | world quests in the coordinator, notice board, quest log, objectives, turn-in, renown, legends entry | `coordinator/`, `history/log.rs`, `protocol.rs`, new `.slint` |
| 6 | The puzzle layer | challenge tags, hazards, doors and traps, counters, tonics, rumours and revealed notes, trainers, locker | `character.rs`, fixtures, `quest.rs` |
| 7 | Troubles and AI adventurers | lair trouble, `Purpose::Quest` journeys, adventurer brain, repeat visits | `history/`, `people.rs`, `creature.rs` |
| 8 | Parties and travel | party id, shared credit, `effective_step` scaling, map travel between known sites | `Client`, coordinator, world map |

Stages 1–3 need nothing from site generation beyond the test bed; 5–7 become much richer once real
sites exist, so the site plan can run in parallel from stage 0 onward against the fixture contract.

## 9. Decisions that change the plan if wrong

1. **Mastery reading (3.2):** one shared Focus budget, use-based gain, gains drain released trees,
   nodes lapse and refund their knack. If instead mastery was meant to be spent rather than trained,
   stage 4 changes shape.
2. **Defeat is not death (4):** waking at a rest point with your kit.
3. **Quests are world objects, closed for everyone on completion (5.1),** rather than each player
   getting a private copy of every quest.
4. **Adventurer inventory is the hotbar plus three worn slots; abilities have their own 4-slot bar
   (3.3, 3.5).** The harsher alternative — abilities compete with items for the same nine slots — makes
   the fitting puzzle tighter and is worth a try once both exist.
5. **Troubles wait while the fortress region is not running (7).**
