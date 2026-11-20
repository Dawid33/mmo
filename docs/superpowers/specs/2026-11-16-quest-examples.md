# Quest examples — the accepted quest and dialogue constructs, worked through

Date: 2026-11-16. Companion to `2026-11-16-quest-construction-design.md` (accepted by Dawid the same day; "the
design" below) and to the language spec §5.11–5.13. Nothing here compiles yet: dialogue and quests are Phase 10
of the MVP plan, and the constructs the design adds were folded into the spec on 2026-11-17. These examples are
for judging the syntax and the semantics together.

The examples:

| # | Example | Kind | Shows |
|---|---|---|---|
| 1 | The Lost Dog | one-off, home + an entity part | `requires`, roles, `optional role`, stage greetings and topics, named objectives, parallel and optional objectives, `-> Stage when`, `Entered`, stage-scoped timers, a failed attempt by re-entering a stage, `participants` / `join` / `complete()`, `RoleLost`, full line ids |
| 2 | The Sealed Mine | world-line, multi-region | `quest … at role`, a region-anchored part, a part's own `Setup` and `decline`, a counter objective, an uninterruptible staged scene with `pause` and `(ticks=…)`, `SceneEnded`, the act that makes it world-line |
| 3 | The Bandit Camp | repeatable | `repeatable quest X(cooldown=…, exclusive=true)`, setup prefab, the namespace surviving resets, a failed attempt when every participant is down, what a reset does |
| 4 | A Debt to Settle | one-off, uses another package | a reusable dialogue with **exits**, `detour` arms, `DialogueEnded.exit` in a handler |
| 5 | Town life | package content, no quest | stateless package dialogue, `for npc where …`, ambient scenes with casting, a follow-up scene, a one-actor scene (a bark) |
| 6 | Refused by the compiler | — | the new diagnostics |
| 7 | One conversation, tick by tick | — | pacing: batches, a pick, the end acknowledgement |

## Assumed host API

The stub host has none of this; the game's host description and the coordinator's fact table would. Pure queries
may be used in conditions; commands only in handlers.

- **Types:**
  - `Person`, `Creature` and `Player` are entity handles;
  - `Site` (a forged site), `Place` (a spot the coordinator chose), `Prefab`, `Item(kind, count)`, `Timer`;
  - the enums `Occupation`, `SiteKind`, `EndReason` and `SceneEnd`.
- **Fact queries**, also allowed in `where` / `requires` (design §7.1):
  - `act()`, `completed(def)`;
  - `near(a, b, regions=n | blocks=n)`;
  - `x.alive`, `x.occupation`, `x.has_tag(t)`, `x.site`, `x.owner`;
  - `site_kind(s)`, `p.open_ground(blocks=n)`.
- **Other pure queries:**
  - `x.name`, `p.skill(name)`, `p.inventory_count(kind)`;
  - `ours(entity)`: spawned or placed by this quest instance;
  - `prefab(name)`: a prefab of this package.
- **Commands:**
  - `give(p, item)`, `take(p, kind, n)`;
  - `follow(creature, p)`, `place(prefab, at)` (fallible: `NoRoom`);
  - `play(scene, …cast)`, `after(ticks, event)`, `cancel(timer)`;
  - `join(p)`, `leave(p)`, `complete()`, `fail()`, `decline()`.
- **Namespace:** `world.get` / `world.set` / `world.add` (the quest's namespace; the rename to `ns.*` of design §8
  was declined 2026-11-17).
- **Host events:**
  - `Setup` — delivered once when an instance or a part is placed, and again after a repeatable reset;
  - `Entered` — a stage became current;
  - `PlayerNear { player }` — a player came within `NEAR_BLOCKS` of an entity-anchored instance's entity;
  - `ItemGiven { player, to, item }`;
  - `Killed { victim, by: Option[Player] }`;
  - `PlayerDefeated { player }`;
  - `BlockUsed { player, tag }`;
  - `RoleLost { role }`;
  - `DialogueEnded { player, npc, dialogue, reason: EndReason, exit }`;
  - `SceneEnded { scene, reason: SceneEnd }`.
- **Generated per quest:**
  - `Stage` (its stages);
  - `Role` (its roles, variants spelled as the role names);
  - `participants: list[Player]`.

Line ids are written out in example 1 only. Elsewhere tooling adds them; until then each line gets a provisional
id and the warning `missing-line-id`.

---

## 1. The Lost Dog — one-off, a home and an entity part

**Corpus:** `quest/corpus/run/qx1_lost_dog` (two players, a bolt and a retry); the spec's shorter version `quest/corpus/run/q_spec_lostdog`.

A farmer's dog has run off. Players who take the job find it, coax it with food before nightfall (or it bolts
again), and bring it home. The dog wanders, possibly across region seams, so the part that watches it is anchored
to the dog itself.

```python
# Package errantry/lost-dog — src/main.qs

let NIGHT_TICKS = 9000                  # from finding the dog to it bolting, unless fed

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller and near(miller, farmer, regions=1)

    let attempts = 0                    # quest state: how often the dog was found and lost again

    event Accepted:
        player: Player
    event AskedMiller:
        player: Player
    event DogFound:
        player: Player
    event DogFed:
        player: Player
    event Nightfall:
        attempt: int
    event Reunited:
        player: Player

    # ── Open: the farmer asks anyone who talks to him ─────────────────────────
    stage Open:
        dialogue worry(p: Player, npc: Person) for farmer:          # a greeting: no topic
            node hi when once per p:
                npc: "Stranger! Have you seen a brown dog on the road?"         @lostdog.worry.first
            node hi:
                => npc: "Still no sign of him."                                 @lostdog.worry.a
                => npc: "I left the gate open. My own fault."                   @lostdog.worry.b

        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills two days ago."           @lostdog.plea
            npc: "He's never been gone a night before."                         @lostdog.plea2
            choice:
                "I'll find him."                                                @lostdog.accept:
                    emit Accepted(player=p)
                    npc: "Bless you. Take something to eat. He's skittish."     @lostdog.hint
                "What does he look like?"                                       @lostdog.ask_look:
                    npc: "Brown, one ear up. Answers to his name, mostly."      @lostdog.look
                "Not now."                                                      @lostdog.not_now

        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    # ── Search: two objectives in parallel, one optional ─────────────────────
    stage Search:
        objective find_dog f"Find {dog.name}"                    @lostdog.find     at dog
        objective feed_dog f"Coax {dog.name} with food"          @lostdog.feed     at dog
        optional objective ask_miller "Ask at the mill"          @lostdog.ask_mill at miller
        -> Returning when find_dog.done and feed_dog.done

        dialogue join_in(p: Player, npc: Person) for farmer topic "Can I help look?" @lostdog.join if not (p in participants):
            npc: "Every pair of eyes helps."                                    @lostdog.join_ok
            emit Accepted(player=p)

        dialogue lost_again(p: Player, npc: Person) for farmer:
            node hi when attempts >= 2:
                npc: f"Found and lost {attempts} times now. He's frightened."   @lostdog.again
            node hi:
                npc: "Any luck?"                                                @lostdog.any_luck

        dialogue tip(p: Player, npc: Person) for miller topic f"Seen {dog.name}?" @lostdog.mill_topic if p in participants:
            npc: "Something's been at my grain sacks down by the weir."         @lostdog.mill_tip
            emit AskedMiller(player=p)

        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1

        @on(Accepted)
        fn joined_late(e):
            join(e.player)

        @on(AskedMiller)
        fn asked(e):
            ask_miller.finish()

        @on(DogFound)
        fn found(e):
            if not find_dog.done:       # two players may spot him in one tick
                find_dog.finish()
                after(NIGHT_TICKS, Nightfall(attempt=attempts))

        @on(DogFed)
        fn fed(e):
            if find_dog.done:
                feed_dog.finish()
                follow(dog, e.player)

        @on(Nightfall)
        fn bolted(e):
            -> Search                   # a failed attempt: re-entering re-opens both objectives

    # ── Returning: bring him home ────────────────────────────────────────────
    stage Returning:
        objective bring_home f"Bring {dog.name} home to {farmer.name}"   @lostdog.return at farmer

        dialogue reunion(p: Player, npc: Person) for farmer topic f"{dog.name} is back" @lostdog.back_topic if near(dog, farmer, blocks=8):
            npc: f"{dog.name}! You daft mutt."                                  @lostdog.reunion
            npc: "Take this. My father carved it. It's all I have of worth."    @lostdog.reward
            emit Reunited(player=p)

        @on(Reunited)
        fn reunited(e):
            bring_home.finish()
            for q in participants:
                give(q, Item(kind="carved_whistle", count=1))
            complete()                  # for every participant

    # ── In any stage ─────────────────────────────────────────────────────────
    @on(RoleLost)
    fn lost(e):
        match e.role:
            case Role::miller:
                pass                    # optional: the tip is simply gone
            case _:
                fail()                  # the farmer or the dog: the quest cannot be done

    # ── The part that rides with the dog ─────────────────────────────────────
    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants and not find_dog.done:
                emit DogFound(player=e.player)

        @on(ItemGiven)
        fn offered(e):
            if stage == Stage::Search and e.item.kind == "meat":
                emit DogFed(player=e.player)
```

What happens, and why it is written this way:

- **Spawn.** The coordinator binds `farmer`, `dog` and, if one is near, `miller`. `requires` is checked first, as
  the cheapest gate. The home is anchored to the farmer's region (the first role), and the part `dog_watch` to the
  dog entity.
- **One quest, several players.** Anyone may take the job: in `Open` the first `Accepted` joins and moves to
  `Search`. A second player who accepted in the same tick is handled by `Search`'s `joined_late`. Later players
  ask through `join_in`, offered only to non-participants.
- **Greetings are stage-scoped too.** `worry` belongs to `Open`, `lost_again` to `Search`. Both join the farmer's
  greeting pool, beside whatever greeting the farmer's site script attaches. The quest's `once per p` node (score 1)
  beats a site's unconditional greeting (score 0).
- **Parallel objectives** `find_dog` and `feed_dog` sit in one stage. `-> Returning when find_dog.done and
  feed_dog.done` is checked after every committed handler, so neither handler needs to know about the other.
- **The optional objective** `ask_miller` never gates anything. With no miller bound, it gets no marker, and
  `tip` attaches to nobody (`for` a `None`).
- **The timer belongs to the stage visit.** `after(NIGHT_TICKS, …)` set in `found` is cancelled automatically if
  the stage changes (the dog was fed in time → `Returning`). If it fires, `bolted` re-enters `Search`. Re-entering
  re-opens both objectives and fires `Entered` (so `attempts` counts up), and the farmer's `lost_again` greeting
  notices.
- **The part reads the home's state** (`stage`, `participants`, `find_dog.done`) through its mirror, one tick old.
  It writes nothing: it `emit`s `DogFound` / `DogFed` to the home. Two players near the dog in one tick send two
  `DogFound`s. `found` guards with `if not find_dog.done`, because `finish()` on a done objective does nothing, but
  a second `after` would start a **second timer**: timers stack, and authors guard where that matters.
- **Reward timing.** `reunion` says the reward line and emits `Reunited` in one batch (example 7), so the whistle
  can land in the bag while the line is still typing. That is acceptable here. A quest where it matters puts the
  `emit` after a `choice`.
- **Losing a role.** The dog dies → `RoleLost { role: Role::dog }` → `fail()`, and the reaper removes the part
  and anything tagged with the instance. Without the `lost` handler the default would do the same for `farmer`
  and `dog`, and nothing for the optional `miller`. The handler is written out to show the shape.

## 2. The Sealed Mine — world-line, multi-region

**Corpus:** `quest/corpus/run/qx2_sealed_mine`; no room at the mine: `quest/corpus/run/qx2_sealed_mine_no_room`.

In act 2, the village elder wants the old mine sealed again. The foreman wants it worked. Players hear the
argument, go down, clear what lurks there and set the seal stone. The mine can be a region or two away, so the
action runs in a part anchored there, while the home sits with the elder.

```python
# Package errantry/act-two — src/sealed_mine.qs

quest SealedMine at elder:
    requires act() == 2
    role elder: Person where elder.occupation == Occupation::Elder and elder.alive
    role foreman: Person where foreman.occupation == Occupation::Miner and foreman.site == elder.site
    role mine: Site where site_kind(mine) == SiteKind::Mine and near(mine, elder, regions=2)

    event HeardOut:
        player: Player
    event LurkerSlain:
        player: Player
    event SealSet:
        player: Player

    scene argument(a: Person, b: Person) uninterruptible:
        a: "The seal held for forty years. You will not break it for silver."
        b: "Silver feeds this village, old man."
        pause 30
        a: "Silver did not feed the eleven we lost."
        b: "..."                                                      (ticks=60)
        a: "Then let the stranger decide what they find down there."

    stage Council:
        objective hear_out "Hear the elder and the foreman out" at elder
        -> Depths when hear_out.done

        dialogue ask(p: Player, npc: Person) for elder topic "The mine?":
            npc: "Brann wants it open. Stand here and listen to him lie."
            emit HeardOut(player=p)

        @on(HeardOut)
        fn convene(e):
            join(e.player)
            play(argument, a=elder, b=foreman)

        @on(SceneEnded)
        fn heard(e):
            if e.scene == argument and e.reason == SceneEnd::Finished:
                hear_out.finish()

    stage Depths:
        objective lurkers "Clear the lurkers from the mine" at mine count 6
        objective seal "Set the seal stone" at mine

        dialogue foreman_sulk(p: Player, npc: Person) for foreman:
            => npc: "Go on then. Bury our living."
            => npc: "Mind the third shaft. It floods."

        @on(LurkerSlain)
        fn slain(e):
            join(e.player)              # anyone who fights down there takes part
            lurkers.progress(1)

        @on(SealSet)
        fn sealed(e):
            if lurkers.done:
                seal.finish()
                complete()

    part depths at mine:
        @on(Setup)
        fn nest(e):
            place(prefab("lurker_nest"), mine) else:
                decline()               # no room at this mine: the whole quest is not placed
                return

        @on(Killed)
        fn killed(e):
            if stage == Stage::Depths and ours(e.victim):
                match e.by:
                    case Some(p):
                        emit LurkerSlain(player=p)
                    case None:
                        pass            # drowned in the third shaft: no one's kill

        @on(BlockUsed)
        fn used(e):
            if stage == Stage::Depths and e.tag == "seal_stone":
                emit SealSet(player=e.player)
```

The package's manifest (a sketch: the act format is not designed yet) is what makes this quest **world-line**:

```toml
# errantry/act-two — package.toml (excerpt)
[[act]]
id      = "act-2"
title   = "@act2.title"                      # a line id: acts are translated too
quests  = ["errantry/act-two::SealedMine", "errantry/act-two::BrokenOath"]
min_days = 7                                  # multiplayer only: time-gated, not time-started
```

What happens:

- **Where things run.** `at elder` anchors the home to the elder's region. `part depths at mine` runs in the mine's
  region. The spawn job places both. The part's `Setup` places the nest and can `decline()`: then the job fails,
  the home is reaped too, and the bindings cool down (quests-in-the-world §5.4).
- **The argument is a staged scene.** `play(argument, a=elder, b=foreman)` starts it. It is `uninterruptible`, so
  a player who talks to either NPC meanwhile gets their `busy` bark or nothing; the scene cannot be broken off by
  accident.
  - **Its timing is in the sim:** each line's duration comes from its source text length. `pause 30` is
    silence, and `(ticks=60)` holds the foreman's "..." for the pause it deserves.
  - **Its end:** `SceneEnded { reason: Finished }` finishes the objective. If an actor dies mid-scene
    (`ActorLost`), the objective stays open, and the elder's topic can start the argument again.
- **The counter objective** `lurkers` counts `progress(1)` per kill reported by the part. `ours(e.victim)` keeps a
  wolf that wandered in from counting. The journal shows "Clear the lurkers from the mine (2/6)".
- **World-line.** Nothing in the quest says so. The act lists it. Completing it is recorded once for the world,
  with a legends entry naming the participants. When `BrokenOath` is also done (and, in multiplayer, seven days have
  passed since act 2 opened), act 3 opens. `requires act() == 2` keeps the quest from spawning in other acts. A
  second loop of the story would open act 2 again; `completed(def)` scoped to the loop is the design's (open) on
  later loops.

## 3. The Bandit Camp — repeatable

**Corpus:** `quest/corpus/run/qx3_bandit_camp` (four completions; its header notes the `exclusive` paragraph).

Bandits camp in a hollow near a village. A watchman pays to have them driven out. When they are gone, they come
back after a cooldown, a little better armed each time they have been beaten.

```python
# Package errantry/bandits — src/main.qs

repeatable quest BanditCamp(cooldown=72000, exclusive=true) at camp:
    role watch: Person where watch.occupation == Occupation::Guard and watch.alive
    role camp: Place where camp.open_ground(blocks=20) and near(camp, watch, regions=1)

    let veteran = false                 # re-initialised on every reset; set from the namespace in Setup
    let paid: list[Player] = []

    event Hired:
        player: Player
    event Paid:
        player: Player

    @on(Setup)
    fn pitch_camp(e):
        let beaten = await world.get("times_beaten") else 0     # the namespace survives resets
        veteran = beaten >= 3
        let camp_prefab = prefab("bandit_camp")
        if veteran:
            camp_prefab = prefab("bandit_camp_veteran")
        place(camp_prefab, camp) else:
            decline()
            return

    stage Notice:
        dialogue hire(p: Player, npc: Person) for watch topic "Any work going?":
            when veteran:
                npc: "Them again. Better blades this time, I hear."
            npc: "Bandits in the hollow. Clear them out and the village pays thirty silver."
            choice:
                "Consider it done.":
                    emit Hired(player=p)
                "Not today."

        @on(Hired)
        fn hired(e):
            join(e.player)
            -> Assault

    stage Assault:
        objective bandits "Drive the bandits from the hollow" at camp count 5
        optional objective banner "Take their banner" at camp
        -> Payday when bandits.done

        @on(Killed)
        fn killed(e):
            if ours(e.victim):
                bandits.progress(1)

        @on(PlayerDefeated)
        fn down(e):
            leave(e.player)
            if participants.len() == 0:
                -> Notice               # everyone is down: the attempt failed; the job is open again

        @on(BlockUsed)
        fn banner_taken(e):
            if e.tag == "bandit_banner":
                banner.finish()

    stage Payday:
        dialogue pay(p: Player, npc: Person) for watch topic "The hollow is clear." if p in participants and not (p in paid):
            npc: "Good work. Thirty, as promised."
            when banner.done:
                npc: "And the banner? The reeve will want that. Five more."
            emit Paid(player=p)

        @on(Paid)
        fn pay_out(e):
            let silver = 30
            if banner.done:
                silver = 35
            give(e.player, Item(kind="silver", count=silver))
            paid.append(e.player)
            if paid.len() == participants.len():
                await world.add("times_beaten", 1)
                complete()
```

What a completion does (design §9.2):

1. `complete()` records the completion for each participant, per (definition, player): a count and the last tick.
   The first completion per player gets a legends entry. So does a crossed milestone (10, 100); the rest are
   counted only.
2. The instance **resets**, exactly as an admin's Reset:
   - `veteran` and `paid` re-initialise;
   - the stage returns to `Notice`, objectives re-open, `participants` empties;
   - the queue, parked handlers and timers clear.
3. The reaper removes everything tagged with the instance (the bandits, their camp, a dropped banner left in the
   world).
4. The roles' `where` is re-checked. If the watchman died, the instance is reaped for good, and the coordinator may
   spawn a fresh one elsewhere.
5. After `cooldown` ticks, `Setup` is delivered again. It reads `times_beaten` from the **namespace**, which a
   reset keeps, and places the veteran camp from the fourth time on.

Other points:

- **`exclusive=true`:** while `participants` is non-empty, `join` refuses anyone else. A second party gets the
  topic, but `hire`'s `emit` leads to a refused `join` and they are told the job is taken (a host message). When
  the first party is all down, `-> Notice` reopens the job. That is the failed attempt, not a quest failure.
- **`at camp`:** the home sits where the fighting is, so `Killed` reaches it directly with no part. The watchman's
  dialogue runs in the watchman's region (maybe the next one), from the mirror of `veteran`, `paid`,
  `participants` and `banner.done`.
- **Terrain is not restored.** A player who digs a trench in the hollow leaves it there. A quest that needs
  pristine ground would declare a protected `area`, which stays (open) in the design (§9.2).

## 4. A Debt to Settle — a reusable dialogue with exits

**Corpus:** `quest/corpus/run/qx4_debt` (`errantry/trade` as a module).

A lender wants a debtor to pay. The player can talk the debtor round, browbeat them, or give up. The haggling is a
dialogue from a shared package that many merchants use. Its exits tell each caller how it went.

```python
# Package errantry/trade — src/haggle.qs   (a library: no quest)

pub dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Times are hard. You know how it is."
    choice:
        "Half now, half at harvest?" if p.skill("trade") >= 2:
            npc: "...Half now. Fine."
            return agreed
        "Pay, or I take it from your stock." if p.skill("intimidate") >= 3:
            npc: "All right! All right."
            return agreed
        "Then I'll be back.":
            return refused
    # falling off the end would be the implicit exit `done`; every option returns here
```

```python
# Package errantry/old-debts — src/main.qs   (depends on errantry/trade)
use trade::haggle::haggle          # `trade`: the dependency's crate name, as in Rust

quest OldDebt:
    role lender: Person where lender.occupation == Occupation::Merchant
    role debtor: Person where debtor.site == lender.site and debtor != lender and debtor.alive

    event Collected:
        player: Player

    stage Collect:
        objective collect f"Collect {lender.name}'s money from {debtor.name}" at debtor

        dialogue confront(p: Player, npc: Person) for debtor topic f"{lender.name} sent me" exits paid, stalled:
            npc: "Oh. Them."
            detour haggle:
                agreed:
                    npc: "Here. Tell them we're square."
                    return paid
                refused:
                    npc: "Come back when you're less... you."
                    return stalled

        dialogue thanks(p: Player, npc: Person) for lender topic "About the debt" if collect.done:
            npc: "You got it? Then here's your cut."
            emit Collected(player=p)

        @on(DialogueEnded)
        fn after_talk(e):
            if e.dialogue == confront and e.exit == confront::paid:
                join(e.player)
                collect.finish()

        @on(Collected)
        fn reward(e):
            give(e.player, Item(kind="silver", count=10))
            complete()
```

What happens:

- **`haggle` belongs to another package** and knows nothing about debts. It declares two exits and returns through
  one. Each caller decides what an exit means.
- **`detour haggle:` with arms:** after the detour returns, the runner runs the arm of the exit taken, then
  continues after the detour. Here each arm also leaves `confront` through one of *its* exits.
- **The quest learns the outcome from `DialogueEnded`**, whose `exit` is the outermost dialogue's (`confront::paid`).
  The dialogue `emit`s nothing for it. An exit is a label, `<dialogue>::<exit>`: the quest compares it, it never
  holds data. Walking away mid-haggle ends with `reason: EndReason::Left` and `exit: confront::done` (the implicit
  exit), so `after_talk` ignores it.
- **The `haggle` choice guards read the player's skills,** pure host queries. A player with neither skill sees
  only "Then I'll be back.", which is the "prepare and come back" loop of game-design §3.1.

## 5. Town life — package content with no quest

**Corpus:** `quest/corpus/run/qx5_town_life`.

Ambient flavour for every town: a smith's greetings, market banter between two merchants, a crier. There is no
quest instance, so all of it is **stateless**: conditions read the NPC, the player and world facts, never a global,
and nothing `emit`s.

```python
# Package errantry/town-life — src/main.qs   (no quest: package-level content)

dialogue smith_hello(p: Player, npc: Person) for npc where npc.occupation == Occupation::Smith:
    node hello when once per p:
        npc: "Mind the sparks. New faces always stand too close."
    node hello when p.inventory_count("broken_tool") > 0:
        npc: "That's a sorry tool you're carrying. Leave it with me."
    node hello:
        => npc: "Hot work, this."
        => npc: "Need something mended?"
        => npc: "Iron's dear since the mine closed." when act() >= 3

scene market_haggle(a: Person, b: Person) where a.occupation == Occupation::Merchant and b.site == a.site and a != b:
    b: "How much for the turnips?"
    a: "Two coppers."
    b: "Two? They're half rot."
    pause 20
    a: "Then it's one copper of turnip and one of rot."                     (ticks=90)
    -> market_haggle_after

scene market_haggle_after(a: Person, b: Person):
    pause 40
    b: "...Fine. Two."

scene crier(npc: Person) where npc.has_tag("town_crier"):                 # a one-actor scene: a bark
    => npc: "Hear ye, hear ye!"
    => npc: "The hollow road is closed by order of the reeve!" when act() == 1
    => npc: "The old mine is sealed again! Praise the stranger!" when completed("errantry/act-two::SealedMine")
```

What happens:

- **`smith_hello` applies to every smith**, wherever the package is enabled. When a player talks to a smith, its
  `hello` group joins that smith's greeting pool with whatever else is attached: the site's greeting, a quest's
  stage greeting. The pick is by the shared score, so a quest's specific greeting outscores this generic one. Its
  `once per p` record lives on the smith, keyed by `errantry/town-life::smith_hello`.
- **Scenes are cast by the runner's ambient schedule.** When a player is within `HEAR_RANGE_BLOCKS` of two idle
  people who satisfy `where`, `market_haggle` is a candidate. The runner:
  - binds `a` and `b` from those people;
  - picks among candidates by score, then least recently played, then the stable key;
  - plays the pick.

  `a` is the lead and owns the scene's state. If `a` crosses a seam mid-line, the scene goes with them.
- **Follow-up.** `-> market_haggle_after` hands the same cast, by position, to another scene: Valve's banter chain.
  The follow-up opens with a `pause`, so the reply comes after a beat.
- **Interruptible.** A player who talks to either merchant mid-scene ends it for both
  (`SceneEnded { reason: Interrupted }`), and the conversation starts.
- **A bark is a one-actor scene.** `crier` reads only world facts (`act()`, `completed`). Its `=>` group picks one
  line per cast, by score: the act-specific lines (score 1) beat "Hear ye" (score 0) while they hold.

## 6. Refused by the compiler

**Corpus:** the fragments in order: `quest/corpus/fail/bad_where`, `stateless_dialogue`, `unknown_exit`, `scene_choice`, `duplicate_def` (with `entered-outside-stage`).

Each fragment below is refused, with the diagnostic named.

```python
fn wealthy(x: Person) -> bool:
    return x.skill("trade") > 5

quest Q:
    role buyer: Person where wealthy(buyer)            # bad-where: a script fn is not a fact query
```

```python
let rumour = "wolves"                                  # a global in a package with no quest

dialogue gossip(p: Player, npc: Person) for npc where npc.has_tag("gossip"):
    npc: f"They say {rumour}."                         # stateless-dialogue: reads a script global
    emit Gossiped(player=p)                            # stateless-dialogue: no instance to emit to
```

```python
dialogue offer(p: Player, npc: Person) for trader:
    detour haggle:
        agreed:
            npc: "Done."
        cheated:                                       # unknown-exit: haggle exits agreed, refused
            npc: "Hey!"
```

```python
scene quarrel(a: Person, b: Person):
    a: "Well?"
    choice:                                            # scene-choice: a scene has nobody to choose
        "Yes"
```

```python
quest Q:
    role dog: Creature
    stage Search:
        objective dog "Find the dog" at dog            # duplicate-def: the objective `dog` shadows the role
    @on(Entered)                                       # entered-outside-stage: whose stage was entered?
    fn entered(e):
        pass
```

New diagnostics these introduce, beside the plan's list:

- `stateless-dialogue`;
- `unknown-exit`;
- `scene-choice`;
- `entered-outside-stage`.

`bad-where` and `duplicate-def` exist already, and only gain cases.

## 7. One conversation, tick by tick

**Corpus:** the opening of `quest/corpus/run/qx1_lost_dog`; the stale menu re-sent: `quest/corpus/run/d_read_your_emit`.

Player Ana talks to the farmer of example 1 at tick 1000, while the quest is in `Open`. Ana's client predicts all
of this; the server confirms it.

| Tick | Who | What |
|---|---|---|
| 1000 | Ana | **talk** to the farmer (stamped input to the farmer's region) |
| 1001 | runner | greeting pool: the site's `farmer_hello` (score 0) and `worry`'s `hi` nodes; `when once per p` (score 1) wins. It walks to the end of the greeting and opens the topic menu: `About Rufus` (priority 0) plus the site's own topics, by qualified name. **Batch 1:** one line and the menu. |
| — | Ana's client | types the line out at its own pace, then shows the menu. No sim input per line. |
| 1180 | Ana | **pick** `About Rufus` (stamped input) |
| 1181 | runner | detours into `plea`: two lines, then the choice. **Batch 2:** two lines and three options. |
| 1302 | Ana | **pick** "I'll find him." |
| 1303 | runner | queues `emit Accepted(player=Ana)` to the home (same region: next segment), then the hint line; `plea` ends and returns to the menu, now re-evaluated. **Batch 3:** one line and the menu. |
| 1303 | home | `accepted`: `join(Ana)`, `-> Search`; `Entered` queued; `plea` and `worry` detach, and `join_in` and `lost_again` attach (stage-scoped). |
| 1304 | runner | the menu Ana sees was built at 1303, so `About Rufus` is still on it. Picking it now would find `plea` detached; the runner refuses the pick and re-sends the menu. That is the read-your-emit latency of spec §5.11, here one tick wide. |
| 1390 | Ana | closes the menu: **leave** |
| 1391 | runner | the conversation ends; the farmer stays facing Ana until her **Done** or `CONVERSATION_LINGER_TICKS` |

Three stamped inputs in all (talk, pick, pick), plus the leave and the `Done`. None per line. If Ana stood across a
seam from the farmer, each of these would be one stamped message to the farmer's region, and each batch would
reach her as events of that region through her observer connection.
