# Dialogue examples — Yarn Spinner samples converted to quest script

Date: 2026-11-16, rewritten the same day for the declarative model. Companion to the language spec §5.11–5.13
and `2026-11-16-dialogue-and-quest-structure-research.md`. Nothing here compiles yet: the parser is Phase 4
of the MVP plan and dialogue is Phase 10. These examples are for judging the syntax.

**The model (spec §5.11, decided 2026-11-16).** A dialogue is a declarative description: nodes, lines,
choices, attached to NPCs with `for`. The VM never runs it. The region's Rust code runs the conversation when
a player talks to the NPC. Conditions (`when`, option `if`) and substitutions are pure functions the runner
calls. A dialogue changes nothing itself: it `emit`s events, and `@on` handlers do the work.

**Sources.** Examples 1–5 convert scripts from Yarn Spinner's official Unity samples,
[YarnSpinnerTool/YarnSpinner-Unity-Samples](https://github.com/YarnSpinnerTool/YarnSpinner-Unity-Samples) (MIT
licence), fetched 2026-11-16:

- `Samples~/BasicSaliency/BasicSaliency.yarn`
- `Samples~/BackgroundChatter/Dialogue/TownCrier.yarn`
- `Samples~/OptionsThatTimeout/Dialogue/Timeouts.yarn`
- `Samples~/AdvancedSaliency/Dialogue/Room-Interrogation.yarn`
- `Samples~/CustomActions/CustomActions-ItemVendors.yarn`

Quoted Yarn text is the samples' own, typos included. Example 6 is original.

**Assumed host API.** The stub has none of this; the game's host description would. These are only the
examples' assumptions:

- `struct Person { name: str }`; `Player` as in the stub.
- Queries `npc_named(name: str) -> Person`, `Player.inventory_count(kind: str) -> int`,
  `quest_open(id: str) -> bool`, `act() -> int`, `here() -> Site`.
- Commands `give(p, Item)` and `bark(npc, d)`.
- Event `Ambient { npc: Person }`, fired now and then while players are near.

**Conventions.**

- Yarn's `Name: text` becomes `name: "text"`. The NPC being talked to is `npc` and the player is `p`; any
  other speaker is a `Person` in scope.
- Yarn's multiple `when:` headers are AND-ed: `when a and b`. Scoring is the same — an expression counts 1
  plus 1 per boolean operator.
- Yarn's `<<set>>` inside dialogue becomes an `emit` plus a handler, since dialogue has no assignment.
  `<<declare>>` is a top-level `let`, `<<enum>>` is `enum`, and `//` comments are `#`.
- Yarn markup stays inside the line's text.
- Line ids (`@path`) are written out in example 2 only. Elsewhere tooling adds them; until then each line
  gets a provisional id and the warning `missing-line-id`.

---

## 1. Node groups and line groups — `BasicSaliency.yarn`

**Corpus:** `quest/corpus/run/dx1_saliency` (scores by conjuncts; `npc_named` and the clock adapted for the stub).

Four NPCs pick what to say from the day and the time of day.

**Yarn (excerpt)**

```yarn
title: Alice
when: $day == .Tuesday
when: $time == .Morning
---
Player: Hello
Alice: Hello, what a great Tuesday morning it is!
===
title: Alice
when: $day == .Wednesday
when: $time == .Evening
when: once
---
Player: Hello
Alice: the week is half over, what a relief.
===
title: Liz
when: once
when: always
---
Liz: I have this node which will only happen once, neato right?
===
title: Liz
when: always
---
Liz: Oh hi
===
title: George
---
=> George: oh hi
=> George: another Monday, I hate Mondays <<if $day == .Monday>>
=> George: Weird how tomorrow is Monday, right? <<if $day == .Wednesday && $time == .Evening>>
    Player: did you want the devs to have to write out even more sample content?
    George: it would be nice, yes
=> George: I wonder if I am in any other samples
===
```

**Quest script**

```python
enum Day:
    Monday
    Tuesday
    Wednesday

enum TimeOfDay:
    Morning
    Evening

let day = Day::Monday              # the current day (in game: a host query)
let time = TimeOfDay::Morning      # the current time of day

dialogue alice_talk(p: Player, npc: Person) for npc_named("Alice"):
    node talk when day == Day::Monday:
        p: "Hey"
        npc: "Hey, it's Monday"
        p: "yep"
        npc: "and I only have a single storylet for Monday regardless of the time of day"
        p: "cool"
        npc: "indeed"
    node talk when day == Day::Tuesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Hello, what a great Tuesday morning it is!"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Ah Tuesday evening, always the best time of Tuesday I say"
    node talk when day == Day::Tuesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Just between you and me I have some plans this evening."
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Humpday, am I right?"
    node talk when day == Day::Wednesday and time == TimeOfDay::Morning:
        p: "Hello"
        npc: "Ah Wednesday morning, definitely the most morning part of the day."
    node talk when once if day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "the week is half over, what a relief."
    node talk when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "Hello"
        npc: "Probably should go to bed, it is Wednesday evening after all."

dialogue barry_talk(p: Player, npc: Person) for npc_named("Barry"):
    # Three nodes with the same condition: best_least_recent cycles through them.
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Good morning"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "How are you this morning?"
    node talk when time == TimeOfDay::Morning:
        p: "Hi"
        npc: "Top of the morning"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Good evening"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "What a fine evening it is today"
    node talk when time == TimeOfDay::Evening:
        p: "Hi"
        npc: "Wait, is is it evening or afternoon?"

dialogue liz_talk(p: Player, npc: Person) for npc_named("Liz"):
    node talk when once:           # scores 1, so it wins once; a node with no condition scores 0
        npc: "I have this node which will only happen once, neato right?"
    node talk:
        npc: "Oh hi"
    node talk:
        npc: "What up?"
    node talk:
        npc: "Talk to anyone interesting lately?"

dialogue george_talk(p: Player, npc: Person) for npc_named("George"):
    => npc: "oh hi"
    => npc: "another Monday, I hate Mondays" when day == Day::Monday
    => npc: "I suppose I should make breakfast" when time == TimeOfDay::Morning
    => npc: "Weird how tomorrow is Monday, right?" when day == Day::Wednesday and time == TimeOfDay::Evening:
        p: "did you want the devs to have to write out even more sample content?"
        npc: "it would be nice, yes"
        p: "well they aren'that gonna do that"
        npc: "boo"
    => npc: "I wonder if I am in any other samples"
```

What the conversion showed:

- **`for` replaces Yarn's engine-side dispatch.** Yarn starts the node named after the NPC from C#. Here each
  dialogue says whom it belongs to, and the runner finds it when a player talks to that NPC. No handler is
  involved.
- **A `=>` member may carry steps** (George's Wednesday line).
- **Liz's memory is shared.** Yarn's `once` is per save, which in an MMO means per world, so Liz's line is
  "only once" for everyone together. `once per p` would make it once per player.

## 2. Barks, a hub and a detour — `TownCrier.yarn`

**Corpus:** `quest/corpus/run/dx2_town_crier`.

A town crier calls out news in passing. Talked to, they open a conversation that loops at a hub and detours
to a news node group.

**Yarn**

```yarn
title: TownCrier
---
=> TownCrier: Hear ye, hear ye!
=> TownCrier: The king has a new advisor! <<if $king_new_advisor>>
=> TownCrier: A dragon stalks the village! <<if !$dragon_dead>>
=> TownCrier: The fearsome dragon is slain! <<if $dragon_dead>>

<<declare $dragon_dead = false>>
<<declare $king_new_advisor = false>>
===
title: TownCrierTalkTo
---
Player: Hello.
TownCrier: Hello, citizen!
<<jump TownCrierHub>>
===
title: TownCrierHub
---
-> What's the latest news?
    <<detour TownCrierNews>>
-> Goodbye.
    TownCrier: Farewell!
    <<stop>>

<<jump TownCrierHub>>
===
title: TownCrierNews
when: $dragon_dead
---
TownCrier: The beast that stalked the land of late is slain by a valiant adventurer!
===
title: TownCrierNews
when: !$dragon_dead
---
TownCrier: A fearsome dragon stalks the village from the mountains!
TownCrier: Will a noble hero save us from this fell beast?
===
```

**Quest script** (with line ids, as a package build requires)

```python
let dragon_dead = false
let king_new_advisor = false
let crier = npc_named("Town Crier")

dialogue crier_bark(npc: Person):                   # a bark: no player, no choices
    => npc: "Hear ye, hear ye!"                     @crier.bark.hear_ye
    => npc: "The king has a new advisor!"           @crier.bark.advisor when king_new_advisor
    => npc: "A dragon stalks the village!"          @crier.bark.dragon when not dragon_dead
    => npc: "The fearsome dragon is slain!"         @crier.bark.slain when dragon_dead

dialogue crier_talk(p: Player, npc: Person) for crier:
    node start:
        p: "Hello."                                 @crier.hello
        npc: "Hello, citizen!"                      @crier.hello_back
        -> hub
    node hub:
        choice:
            "What's the latest news?"               @crier.ask_news:
                detour crier_news
            "Goodbye."                              @crier.goodbye:
                npc: "Farewell!"                    @crier.farewell
                return                              # Yarn's <<stop>>
        -> hub

dialogue crier_news(p: Player, npc: Person):
    node news when dragon_dead:
        npc: "The beast that stalked the land of late is slain by a valiant adventurer!"  @crier.news.slain
    node news when not dragon_dead:
        npc: "A fearsome dragon stalks the village from the mountains!"                   @crier.news.dragon
        npc: "Will a noble hero save us from this fell beast?"                            @crier.news.hero

@on(Ambient)
fn call_out(e):
    if e.npc == crier:
        bark(e.npc, crier_bark)                     # a dialogue's name is a Dialogue value
```

What the conversion showed:

- **Barks have no listener,** so a dialogue may take only `(npc: Person)`. A bark has no choices, and a
  handler starts it (`bark`).
- **`detour` and `return` are steps.** `crier_news` has no `for`, so it is reachable only by detour.
- **The hub loop is `-> hub` after the `choice:`.** Yarn needs three nodes and two jumps.
- **Walking away needs no handling in the script.** The runner ends the conversation and raises
  `DialogueEnded { reason: Left }`, which a script may handle if it cares.

## 3. Options that time out — `Timeouts.yarn`

**Corpus:** `quest/corpus/run/dx3_timeouts`.

**Yarn (excerpt)**

```yarn
title: FallbackExample
---
-> Option 1?
    Alice: option 1 was selected
-> Option 2?
    Alice: option 2 was selected
-> Option 3? #fallback
    Alice: The hidden fallback option was selected
===
title: DefaultExample
---
-> Option 1? #default
    Alice: option 1 was selected
    Alice: this is the default.
-> Option 2?
    Alice: option 2 was selected
===
```

**Quest script**

```python
dialogue timeouts(p: Player, npc: Person) for npc_named("Alice"):
    node start:
        npc: "This sample shows off three different ways of having an option be selected automatically after a fixed amount of time."
        npc: "Which would you like to see?"
        choice:
            "Hidden fallback option?":
                -> fallback
            "Default option?":
                -> default_example
    node fallback:
        npc: "In this example if no option is chosen before time runs out we chose the option with the fallback metadata."
        npc: "This differs from the default example as the fallback option is hidden from the user."
        choice(timeout=300):                    # ticks
            "Option 1?":
                npc: "option 1 was selected"
            "Option 2?":
                npc: "option 2 was selected"
            default:                            # hidden: never offered, runs on timeout
                npc: "The hidden fallback option was selected"
    node default_example:
        npc: "In this example if no option is chosen before time runs out we chose the option with the default metadata."
        npc: "this differs from the fallback example as the default is visible to the user."
        choice(timeout=300):
            default "Option 1?":                # offered, and picked on timeout
                npc: "option 1 was selected"
                npc: "this is the default."
            "Option 2?":
                npc: "option 2 was selected"
```

What the conversion showed:

- **The timeout lives in the dialogue.** Yarn uses option metadata (`#fallback`, `#default`) read by a
  custom presenter. Here time is sim state the runner tracks, so the timeout is part of the description:
  `choice(timeout=…)`, `default "text"` for a visible default, and a bare `default:` for a hidden fallback.
  A timeout with neither ends the conversation (`DialogueEnded { reason: Timeout }`).
- **"Last highlighted option" is not converted.** It is one client's menu state, which the sim never sees.

## 4. Speakers in scope, scenario state — `Room-Interrogation.yarn`

**Corpus:** `quest/corpus/run/dx4_interrogation`.

Two NPCs, `$primary` and `$secondary`, interrogate together. Alice gets a special opening when she is the
primary.

**Yarn (excerpt)**

```yarn
title: Primary
when: $scenario == .Interogation
when: $scenario_state == .NotStarted
---
{$primary}: ok you be good cop, I'll be bad
Player: yep
<<set $scenario_state = .Started>>
===
title: Primary
when: $scenario == .Interogation
when: $scenario_state == .Started
---
{$primary}: you are in real trouble now
{$secondary}: Oh am I now!?
=> Player: it's weird we're in a mansion for this right? <<if $Room == .Mansion>>
=> Player: anyone fancy a pint? <<if $Room == .Pub>>
<<set $scenario_state = .Complete>>
===
```

**Quest script**

```python
enum ScenarioState:
    NotStarted
    Started
    Complete

enum Room:
    Mansion
    Pub
    Office
    Church

let state = ScenarioState::NotStarted
let room = Room::Mansion
let primary = npc_named("Alice")
let secondary = npc_named("Bob")

event Advance:
    to: ScenarioState

@on(Advance)
fn advance(e):
    state = e.to

dialogue primary_talk(p: Player, npc: Person) for primary:
    # Alice's opening scores once 1 + two comparisons 2 + `and` 1 = 4 and wins over the generic opening (1).
    node talk when once if npc.name == "Alice" and state == ScenarioState::NotStarted:
        npc: f"So it's time to break {secondary.name}."
        p: "oh?"
        npc: "yeah they are real twisted and I will need your help"
        p: "oh cool"
        npc: "agreed, anyways, talk to me again please and we'll discuss strategy."
    node talk when state == ScenarioState::NotStarted:
        npc: "ok you be good cop, I'll be bad"
        p: "yep"
        emit Advance(to=ScenarioState::Started)
    node talk when state == ScenarioState::Started:
        npc: "you are in real trouble now"
        secondary: "Oh am I now!?"
        => p: "it's weird we're in a mansion for this right?" when room == Room::Mansion
        => p: "anyone fancy a pint?" when room == Room::Pub
        => p: "this feels like the right place for this" when room == Room::Office
        => p: "I reckon the symbolism here is confusing" when room == Room::Church
        npc: "quiet for a second please"
        p: "want to go see Les Mis?"
        secondary: "huh?"
        npc: "Yeah, what?"
        p: "anyone fancy an icecream?, I could go pick us some up."
        secondary: "I confess, just stop being so weird."
        emit Advance(to=ScenarioState::Complete)
    node talk when state == ScenarioState::Complete:
        npc: "look I am not sure what you were doing but wow did it ever work."
        p: "thanks"

dialogue secondary_talk(p: Player, npc: Person) for secondary:
    node talk when state == ScenarioState::Complete:
        => npc: "I promise I'll be good, please don't Henry the Train me."
        => npc: "I'm done hiding the corpses of my masters victims." when room == Room::Mansion
        => npc: "I'm all Jesused up now, I swear." when room == Room::Church
        => npc: f"I promise I'll never eat {primary.name}'s lunch again." when room == Room::Office
        => npc: "I promise I'll no more cheating at darts." when room == Room::Pub
    node talk:
        npc: "Why are you talking to me?"
```

What the conversion showed:

- **Yarn's `<<set>>` becomes `emit Advance(...)` and a three-line handler.** That is the cost of "dialogue
  changes nothing itself" — and its benefit: every state change in the script is in a handler, in one
  place.
- **The new state shows up after the handler runs.** `state` changes when `advance` runs, after the step.
  A condition later in the *same* conversation must not rely on it sooner (spec §5.11, "read-your-emit");
  this sample only reads it on the next talk.
- **A secondary speaker is just a `Person` in scope** (`secondary:`). `$scenario == .Interogation` is
  dropped: here a scenario is its own script — in game, a quest.

## 5. Dialogue that would be code — `CustomActions-ItemVendors.yarn`

**Corpus:** `quest/corpus/run/dx5_vendors`; a computed line text: `quest/corpus/fail/bad_line_text`.

**Yarn** (three copies of this node: apples, oranges, blueberries)

```yarn
title: ItemVendorApple
---
<<if inventory_count(FoodType.Apple) < 10>>
    <<add_inventory_item FoodType.Apple>>
<<else>>
    Player: That's too many apples.
<<endif>>
===
```

**Quest script**

```python
event Sell:
    player: Player
    kind: str

fn has_room(p: Player, kind: str) -> bool:      # pure: usable in conditions
    return p.inventory_count(kind) < 10

@on(Sell)
fn sell(e):
    give(e.player, Item(kind=e.kind, count=1))

dialogue apple_stall(p: Player, npc: Person) for npc_named("Apple Seller"):
    when has_room(p, "apple"):
        emit Sell(player=p, kind="apple")
    when not has_room(p, "apple"):
        p: "That's too many apples."

dialogue orange_stall(p: Player, npc: Person) for npc_named("Orange Seller"):
    when has_room(p, "orange"):
        emit Sell(player=p, kind="orange")
    when not has_room(p, "orange"):
        p: "I've already got enough oranges."
```

What the conversion showed:

- **The rule is written once and shared.** `has_room` is a pure `fn` that every stall's conditions call, and
  `sell` is one handler. Only the refusal lines differ, and they must: a line's text is a literal
  (`bad-line-text`), so a computed refusal like `p: refusal` could never be translated.
- **`when cond:` has no `else`.** The opposite condition is written out instead, which keeps a dialogue a
  flat description.

## 6. An Errantry innkeeper — original

**Corpus:** `quest/corpus/run/dx6_innkeeper` (its header notes the number of nested tellings).

This example shows:

- quest-aware rumours, from `quest_open`, a host query of world facts;
- an act-gated option;
- `once per p` greetings in a shared world;
- a hub;
- a dialogue that detours into **itself**, bounded by `dialogue_depth()`.

```python
let keeper = npc_named("Mara")

dialogue innkeeper(p: Player, npc: Person) for keeper:
    node greet when once per p:
        npc: f"New face! Welcome to the {here().name}."
        -> menu
    node greet when p.has_tag("outlaw"):
        npc: "Coin first. Then we talk."
        -> menu
    node greet:
        => npc: "Back again?"
        => npc: "Mind the step, it's loose."
        => npc: "Sit anywhere that isn't wet."
        -> menu
    node menu:
        choice:
            "Heard any rumours?":
                detour rumours
            "Tell me a story.":
                detour tale
            once "Who really runs this town?" if act() >= 2:
                npc: "Not the mayor. Ask about the ledger at the mill."
            "Goodbye.":
                npc: "Safe roads."
                return
        -> menu

dialogue rumours(p: Player, npc: Person):
    => npc: "A farmer up the hill lost his dog. Offering a reward, too." when quest_open("lost_dog")
    => npc: "They sealed the old mine again. Third time this year." when quest_open("sealed_mine")
    => npc: "Quiet week. Too quiet."

dialogue tale(p: Player, npc: Person):
    npc: "Once, a traveller sat in an inn much like this one, and the keeper told a tale..."
    when dialogue_depth() < 4:
        choice:
            "And what was that tale?":
                detour tale                  # the dialogue detours into itself
            "Skip to the end."               # no steps: continue after the choice
    npc: "...and when the tale was done, the traveller went on their way."
```

Asking for the tale again and again nests up to three tellings inside the first. Each ending is then said
once on the way back out, in reverse order — the runner's return stack unwinding. Without the
`dialogue_depth()` bound, the runner would end the conversation at `DIALOGUE_STACK_MAX` (`TooDeep`).

## 7. Two scripts, one innkeeper — attached dialogues combine

**Corpus:** `quest/corpus/run/dx7_two_scripts`; module order: `quest/corpus/run/d_commutative`.

The innkeeper of example 6 belongs to the site's script. The lost-dog quest — a different script instance,
from a different package — adds a topic to the same NPC while its stage is `Open`. Neither script knows the
other.

```python
# Package `errantry/lost-dog`, quest program
quest LostDog:
    role farmer: Person
    role dog: Creature where dog.has_tag("pet")
    role inn: Person where inn.has_tag("innkeeper")

    let pointed_to_mill: list[Player] = []

    event AskedAround:
        player: Player

    stage Open:
        dialogue dog_rumour(p: Player, npc: Person) for inn topic f"Seen a dog called {dog.name}?" priority 1:
            npc: "Muddy little thing? Ran past at dawn, toward the mill."
            emit AskedAround(player=p)
        @on(AskedAround)
        fn asked(e):
            pointed_to_mill.append(e.player)     # the mill scene can check this later
```

A player talking to Mara while the quest is `Open` gets:

1. **One greeting**, picked from the pooled greetings: here only `innkeeper`'s `greet` group, since the quest
   attached a topic, not a greeting.
2. **The menu**, from the topics: `Seen a dog called Rufus?` (priority 1) first, then any priority-0 topics
   sorted by qualified name.

Whichever package loaded first, and whichever script attached first, the result is the same: it depends only
on the set (spec §5.11). When the stage moves on, the topic disappears from the menu, and Mara's own
dialogue is untouched.

One thing this shows about example 6: `innkeeper` is a greeting with its own hub (`node menu` with
`choice:`), so the quest's topic is offered only after that hub returns. A site NPC that expects quests to
add topics should leave the menu to the runner instead: a short greeting, and its own entries written as
topics (`dialogue rumours(...) for keeper topic "Heard any rumours?"`, `dialogue tale(...) for keeper topic
"Tell me a story."`). Then every package's topics share one menu.

## 8. What the conversions changed in the spec

Each item below was found converting the examples above and is now in spec §5.11 / §5.13:

1. **A line-group member may carry steps** (`=> line [when cond]:` + steps) — George, example 1.
2. **Timeouts:** `choice(timeout=ticks)`, a visible `default "text"` option, and a hidden `default:`
   fallback — example 3.
3. **Barks:** a dialogue taking only `(npc: Person)`, with no choices, started by `bark(npc, d)` — example 2.
4. **A dialogue's name is a `Dialogue` value** for `attach_dialogue`, `detach_dialogue` and `bark` — example 2.
5. **`dialogue_depth()`** in conditions, to bound a self-detour — example 6.
6. **Effects are `emit` plus a handler**, including what Yarn does with `<<set>>` — examples 4 and 5.
7. **A line's text is a literal** (`bad-line-text`), with markup passed through — example 5.
8. **Attached dialogues combine commutatively:** pooled greetings, a shared topic menu sorted by priority and
   qualified name — example 7.
