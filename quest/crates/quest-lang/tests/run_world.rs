//! The stub world (plan Task 47; spec §5.11 "The runner", §5.12, §9.1): compiled quests run as a home and its
//! parts, with people, talk, scenes, routed emits and mirrors.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::tree;
use quest_stub::{
    Input, Outcome, StubConfig, World, parse_people, parse_script, parse_wire, stub_api,
};
use quest_vm::Program;
use quest_vm::wire::WireValue;

const FUEL: u64 = 2_000;
const MEM: u32 = quest_vm::consts::MEM_BYTES_DEFAULT;

fn compiled(src: &str) -> (Arc<Program>, BTreeMap<String, String>) {
    compiled_files(&[("main.qs", src)])
}

fn compiled_files(files: &[(&str, &str)]) -> (Arc<Program>, BTreeMap<String, String>) {
    let t = tree(files);
    match quest_lang::compile(&t, &stub_api()) {
        Ok(c) => (Arc::new(c.program), c.lines),
        Err(diags) => {
            let text: Vec<String> = diags.iter().map(|d| quest_lang::render(d, &t)).collect();
            panic!("compile failed:\n{}", text.join("\n"));
        }
    }
}

fn roles(people: &str, text: &[(&str, &str)]) -> Vec<(String, WireValue)> {
    let people = parse_people(people).expect("people");
    text.iter()
        .map(|(n, v)| (n.to_string(), parse_wire(v, &people).expect("a role value")))
        .collect()
}

/// A world of `src` over `people` with `roles`, its lines table set.
fn world(src: &str, people: &str, role_text: &[(&str, &str)]) -> Result<World, String> {
    world_of(&[("main.qs", src)], people, role_text)
}

fn world_of(
    files: &[(&str, &str)],
    people: &str,
    role_text: &[(&str, &str)],
) -> Result<World, String> {
    let (p, lines) = compiled_files(files);
    let mut w = World::new(
        p,
        parse_people(people).expect("people"),
        roles(people, role_text),
        StubConfig::default(),
        FUEL,
        MEM,
    )?;
    w.set_lines(lines);
    Ok(w)
}

/// Runs `script` to the end and until the world is quiescent; the output.
fn run(w: &mut World, script: &str) -> Vec<String> {
    let script = parse_script(script).expect("script");
    assert!(
        w.run(&script, 50_000),
        "the world did not settle:\n{}",
        w.output().join("\n")
    );
    w.output().to_vec()
}

const LOST_DOG: &str = r#"# Package errantry/lost-dog — src/main.qs

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
"#;

const LOST_DOG_PEOPLE: &str = r#"
fact act=1
person 3 name="Hal" occupation=Farmer pos=0,0,0
creature 7 name="Rufus" tags=pet owner=3 pos=2,0,0
person 4 name="Mira" occupation=Miller
player 1 name="Ana" inv.meat=1
player 2 name="Bo"
"#;

const LOST_DOG_ROLES: &[(&str, &str)] = &[
    ("farmer", "Person(3)"),
    ("dog", "Creature(7)"),
    ("miller", "Person(4)"),
];

#[test]
fn lost_dog_end_to_end() {
    let mut w = world(LOST_DOG, LOST_DOG_PEOPLE, LOST_DOG_ROLES).expect("placed");
    assert_eq!(w.instances(), 2);
    let out = run(
        &mut w,
        "1 talk 1 3
2 pick 1 1
3 pick 1 1
5 leave 1
6 done 1
8 near 1 7
10 event ItemGiven player=Player(1) to=Creature(7) item=Item(kind=\"meat\",count=1) @dog_watch
20 talk 1 3
21 pick 1 1
23 leave 1
24 done 1
",
    );
    let expected = [
        "[1] Hal: Stranger! Have you seen a brown dog on the road?",
        "[1] menu: 1) About Rufus",
        "[2] Hal: Rufus ran off toward the hills two days ago.",
        "[2] Hal: He's never been gone a night before.",
        "[2] menu: 1) I'll find him. 2) What does he look like? 3) Not now.",
        "[3] Hal: Bless you. Take something to eat. He's skittish.",
        // The menu was built at tick 3, before the home handled `Accepted` (spec §5.11 read-your-emit).
        "[3] menu: 1) About Rufus",
        "[4] joined Ana",
        "[5] end Hal: Left",
        // The part saw Ana near the dog (`DogFound`), then the meat (`DogFed`, an `ItemGiven` handler).
        "[11] follow Rufus Ana",
        "[20] menu: 1) Rufus is back",
        "[21] Hal: Rufus! You daft mutt.",
        "[21] Hal: Take this. My father carved it. It's all I have of worth.",
        "[21] menu: 1) Rufus is back",
        "[22] give Ana 1 carved_whistle",
        "[22] completed Ana",
        "[23] end Hal: Left",
    ];
    assert_eq!(out, expected);
    assert_eq!(w.outcome(), Outcome::Completed);
    assert_eq!(w.completions().get(&1), Some(&1));
}

#[test]
fn lost_dog_role_lost_fails_it() {
    let mut w = world(LOST_DOG, LOST_DOG_PEOPLE, LOST_DOG_ROLES).expect("placed");
    // The optional miller lost: nothing; the dog killed: `RoleLost(dog)` → `fail()`.
    let out = run(&mut w, "2 lose 4\n4 kill 7\n");
    assert_eq!(out, ["[4] failed"]);
    assert_eq!(w.outcome(), Outcome::Failed);
}

#[test]
fn role_of_wrong_type_is_refused() {
    let mut roles = LOST_DOG_ROLES.to_vec();
    roles[0] = ("farmer", "Player(1)");
    let e = world(LOST_DOG, LOST_DOG_PEOPLE, &roles)
        .err()
        .expect("refused");
    assert!(e.contains("role farmer"), "{e}");
    let e = world(LOST_DOG, LOST_DOG_PEOPLE, &LOST_DOG_ROLES[1..])
        .err()
        .expect("refused");
    assert!(e.contains("role farmer"), "{e}");
    // The same shape (`{ id }`) of another kind: a site is no creature.
    let mut roles = LOST_DOG_ROLES.to_vec();
    roles[1] = ("dog", "Site(1)");
    let e = world(LOST_DOG, LOST_DOG_PEOPLE, &roles)
        .err()
        .expect("refused");
    assert!(e.contains("role dog"), "{e}");
    let e = world(PART_QUEST, PART_PEOPLE, &[("pet", "Site(1)")])
        .err()
        .expect("refused");
    assert!(e.contains("role pet"), "{e}");
}

#[test]
fn role_failing_where_is_refused() {
    // Mira is a miller, not a farmer.
    let mut roles = LOST_DOG_ROLES.to_vec();
    roles[0] = ("farmer", "Person(4)");
    let e = world(LOST_DOG, LOST_DOG_PEOPLE, &roles)
        .err()
        .expect("refused");
    assert!(e.contains("role farmer"), "{e}");
    // The dog's owner is not this farmer: `dog.owner == farmer` reads the earlier role.
    let people = LOST_DOG_PEOPLE.replace("owner=3", "owner=4");
    let e = world(LOST_DOG, &people, LOST_DOG_ROLES)
        .err()
        .expect("refused");
    assert!(e.contains("role dog"), "{e}");
    // `requires act() >= 1`.
    let people = LOST_DOG_PEOPLE.replace("fact act=1", "fact act=0");
    let e = world(LOST_DOG, &people, LOST_DOG_ROLES)
        .err()
        .expect("refused");
    assert!(e.contains("requirement 1"), "{e}");
}

const PART_QUEST: &str = r#"quest Q:
    role pet: Creature
    let count = 0
    event Seen:
        n: int
    event Bump:
        n: int
    @on(Bump)
    fn bump(e):
        count = count + e.n
    @on(Seen)
    fn seen(e):
        print(f"home saw {e.n}")
    stage Watching:
        @on(Entered)
        fn watching(e):
            pass
    part watch on pet:
        @on(PlayerNear)
        fn near(e):
            print(f"part sees {count}")
            emit Seen(n=count)
"#;

const PART_PEOPLE: &str = "creature 7 name=\"Rufus\"\nplayer 1 name=\"Ana\"\n";

#[test]
fn part_emit_reaches_home_next_tick() {
    let mut w = world(PART_QUEST, PART_PEOPLE, &[("pet", "Creature(7)")]).expect("placed");
    let out = run(&mut w, "3 near 1 7\n");
    assert_eq!(out, ["[3] part sees 0", "[4] home saw 0"]);
}

#[test]
fn part_reads_home_state_one_tick_old() {
    let mut w = world(PART_QUEST, PART_PEOPLE, &[("pet", "Creature(7)")]).expect("placed");
    let out = run(&mut w, "2 event Q::Bump n=5\n2 near 1 7\n3 near 1 7\n");
    assert_eq!(
        out,
        [
            "[2] part sees 0",
            "[3] home saw 0",
            "[3] part sees 5",
            "[4] home saw 5"
        ]
    );
}

#[test]
fn mirror_sent_only_on_change() {
    let mut w = world(PART_QUEST, PART_PEOPLE, &[("pet", "Creature(7)")]).expect("placed");
    let bump = parse_script("0 event Q::Bump n=1")
        .expect("script")
        .remove(0)
        .1;
    let mut installed = Vec::new();
    for t in 0..8 {
        let inputs = if t == 4 { vec![bump.clone()] } else { vec![] };
        let r = w.step(&inputs);
        if !r.mirrors.is_empty() {
            installed.push((r.tick, r.mirrors));
        }
    }
    // The first mirror, then only after the home's `count` changed at tick 4.
    assert_eq!(installed, [(1, vec![1]), (5, vec![1])]);
}

const SMITH: &str = r#"let smith = Person(id=3)

dialogue hello(p: Player, npc: Person) for smith:
    npc: f"Hello, {p.name}." @h.hi
    choice:
        "Bye." @h.bye
"#;

const SMITH_PEOPLE: &str = "person 3 name=\"Hal\"\nplayer 1 name=\"Ana\"\nplayer 2 name=\"Bo\"\n";

#[test]
fn busy_npc_ignores_second_talker() {
    let mut w = world(SMITH, SMITH_PEOPLE, &[]).expect("placed");
    let out = run(
        &mut w,
        "1 talk 1 3\n2 talk 2 3\n3 pick 2 1\n4 pick 1 1\n5 talk 2 3\n6 done 1\n7 talk 2 3\n",
    );
    assert_eq!(
        out,
        [
            "[1] Hal: Hello, Ana.",
            "[1] menu: 1) Bye.",
            // Bo at 2, 3 and 5 (Ana's conversation lingers until her `done`): nothing.
            "[4] end Hal: Finished",
            "[7] Hal: Hello, Bo.",
            "[7] menu: 1) Bye.",
        ]
    );
}

const REPEATABLE: &str = r#"repeatable quest R(cooldown=5):
    role watch: Person
    let runs = 0
    event Go:
        player: Player
    @on(Setup)
    fn setup(e):
        let n = await world.add("times", 1) else 0
        runs = runs + 1
        print(f"setup {n} runs {runs}")
    stage A:
        @on(Go)
        fn go(e):
            join(e.player)
            complete()
"#;

#[test]
fn repeatable_resets_after_cooldown_with_namespace_kept() {
    let mut w = world(REPEATABLE, SMITH_PEOPLE, &[("watch", "Person(3)")]).expect("placed");
    let out = run(&mut w, "3 event R::Go player=Player(1)\n");
    assert_eq!(
        out,
        [
            "[1] setup 1 runs 1",
            "[3] joined Ana",
            "[3] completed Ana",
            "[3] reaped",
            // `runs` re-initialised; the namespace's `times` kept.
            "[9] setup 2 runs 1",
        ]
    );
    assert_eq!(w.outcome(), Outcome::Running);
    assert_eq!(w.completions().get(&1), Some(&1));
}

#[test]
fn repeatable_whose_roles_fail_is_reaped_for_good() {
    // A `RoleLost` handler that does nothing: the kill does not fail the quest; the reset's check reaps it.
    let src = REPEATABLE
        .replace("role watch: Person", "role watch: Person where watch.alive")
        .replace(
            "    stage A:",
            "    @on(RoleLost)\n    fn lost(e):\n        pass\n    stage A:",
        );
    let mut w = world(&src, SMITH_PEOPLE, &[("watch", "Person(3)")]).expect("placed");
    let out = run(&mut w, "2 kill 3\n3 event R::Go player=Player(1)\n");
    assert_eq!(
        out[..4],
        [
            "[1] setup 1 runs 1",
            "[3] joined Ana",
            "[3] completed Ana",
            "[3] reaped"
        ]
    );
    assert!(out[4].starts_with("[3] reaped for good"), "{out:?}");
    assert_eq!(w.outcome(), Outcome::Reaped);
}

const FAILING: &str = r#"quest F:
    role watch: Person
    event Boom:
        n: int
    event Ping:
        n: int
    @on(Boom)
    fn boom(e):
        fail()
    @on(Ping)
    fn ping(e):
        print("ping")
    stage Waiting:
        @on(Entered)
        fn waiting(e):
            pass
"#;

#[test]
fn fail_stops_delivery() {
    let mut w = world(FAILING, SMITH_PEOPLE, &[("watch", "Person(3)")]).expect("placed");
    let out = run(
        &mut w,
        "1 event F::Ping n=1\n2 event F::Boom n=1\n3 event F::Ping n=1\n",
    );
    assert_eq!(out, ["[1] ping", "[2] failed"]);
    assert_eq!(w.outcome(), Outcome::Failed);
}

#[test]
fn world_hash_deterministic() {
    let script = parse_script("1 talk 1 3\n2 pick 1 1\n3 pick 1 1\n8 near 1 7\n").expect("script");
    let trace = || {
        let mut w = world(LOST_DOG, LOST_DOG_PEOPLE, LOST_DOG_ROLES).expect("placed");
        let mut hashes = Vec::new();
        for t in 0..30u64 {
            let inputs: Vec<Input> = script
                .iter()
                .filter(|(at, _)| *at == t)
                .map(|(_, i)| i.clone())
                .collect();
            w.step(&inputs);
            hashes.push(w.hash64());
        }
        hashes
    };
    let a = trace();
    assert_eq!(a, trace());
    let distinct: std::collections::BTreeSet<u64> = a.iter().copied().collect();
    assert!(distinct.len() > 5, "the hash follows the state");
}

const SEALED_MINE: &str = r#"# Package errantry/act-two — src/sealed_mine.qs

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
"#;

const MINE_PEOPLE: &str = r#"
fact act=2
person 3 name="Odo" occupation=Elder site=1
person 4 name="Brann" occupation=Miner site=1
site 9 kind=Mine
creature 20 name="lurker" ours
creature 21 name="lurker" ours
creature 22 name="lurker" ours
creature 23 name="lurker" ours
creature 24 name="lurker" ours
creature 25 name="lurker" ours
creature 26 name="wolf"
creature 27 name="lurker" ours
player 1 name="Ana"
"#;

const MINE_ROLES: &[(&str, &str)] = &[
    ("elder", "Person(3)"),
    ("foreman", "Person(4)"),
    ("mine", "Site(9)"),
];

#[test]
fn sealed_mine_end_to_end() {
    let mut w = world(SEALED_MINE, MINE_PEOPLE, MINE_ROLES).expect("placed");
    let out = run(
        &mut w,
        "1 talk 1 3
2 pick 1 1
3 leave 1
4 done 1
30 talk 1 4
500 kill 20 by 1
500 kill 26 by 1
501 kill 21 by 1
502 kill 27
502 kill 27 by 1
503 kill 22 by 1
504 kill 23 by 1
505 kill 24 by 1
506 kill 25 by 1
510 event BlockUsed player=Player(1) tag=\"seal_stone\" @depths
",
    );
    assert_eq!(
        out,
        [
            "[0] place lurker_nest at 9",
            "[1] menu: 1) The mine?",
            "[2] Odo: Brann wants it open. Stand here and listen to him lie.",
            "[2] menu: 1) The mine?",
            "[3] joined Ana",
            "[3] end Odo: Left",
            // The argument, timed: each line holds 30 + its characters ticks (64 → 94, 35 → 65, 39 → 69),
            // `pause 30` after the second, `(ticks=60)` for Brann's "...". The talk to Brann at 30 gets
            // nothing: the scene is uninterruptible.
            "[3] Odo: The seal held for forty years. You will not break it for silver.",
            "[97] Brann: Silver feeds this village, old man.",
            "[192] Odo: Silver did not feed the eleven we lost.",
            "[261] Brann: ...",
            "[321] Odo: Then let the stranger decide what they find down there.",
            // Six kills by Ana count; the wolf is not `ours`, the drowned lurker is no one's kill, and
            // killing it again raises nothing.
            "[502] kill: 27 is already dead",
            "[511] completed Ana",
        ]
    );
    assert_eq!(w.outcome(), Outcome::Completed);
}

#[test]
fn sealed_mine_declined_without_room() {
    let people = MINE_PEOPLE.replace("site 9 kind=Mine", "site 9 kind=Mine open_ground=0");
    let mut w = world(SEALED_MINE, &people, MINE_ROLES).expect("placed");
    let out = run(&mut w, "");
    assert_eq!(out, ["[0] declined"]);
    assert_eq!(w.outcome(), Outcome::Declined);
}

const TOWN: &str = r#"# Package errantry/town-life — src/main.qs   (no quest: package-level content)

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
"#;

const TOWN_PEOPLE: &str = r#"
fact act=1
person 3 name="Gunnar" occupation=Smith
person 5 name="Ulla" occupation=Merchant site=1
person 6 name="Pim" occupation=Farmer site=1
person 8 name="Crier" tags=town_crier
player 1 name="Ana"
"#;

#[test]
fn town_life_filters_and_ambient_scenes() {
    let mut w = world(TOWN, TOWN_PEOPLE, &[]).expect("placed");
    let out = run(
        &mut w,
        "1 talk 1 3\n2 leave 1\n3 done 1\n5 ambient 6 5\n600 ambient 8\n",
    );
    let text = out.join("\n");
    // The smith's filter dialogue: its `once per p` node first.
    assert!(text.contains("[1] Gunnar: Mind the sparks."), "{text}");
    // Ulla is the merchant `a`, Pim `b`, though Pim came first; then the follow-up.
    assert!(text.contains("Pim: How much for the turnips?"), "{text}");
    assert!(text.contains("Ulla: Two coppers."), "{text}");
    assert!(text.contains("Pim: ...Fine. Two."), "{text}");
    // The crier's act-1 line outscores "Hear ye".
    assert!(
        text.contains("Crier: The hollow road is closed by order of the reeve!"),
        "{text}"
    );
}

const DEBT: &str = r#"# Package errantry/old-debts — src/main.qs   (depends on errantry/trade)
mod trade
use trade::haggle

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
"#;

const HAGGLE: &str = r#"# Stands in for errantry/trade's haggle.qs.

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
"#;

#[test]
fn debt_exits_reach_dialogue_ended() {
    let people = "person 3 name=\"Lena\" occupation=Merchant site=1
person 4 name=\"Dov\" site=1
player 1 name=\"Ana\" skill.trade=2
";
    let files = [("main.qs", DEBT), ("trade.qs", HAGGLE)];
    let mut w = world_of(
        &files,
        people,
        &[("lender", "Person(3)"), ("debtor", "Person(4)")],
    )
    .expect("placed");
    let out = run(
        &mut w,
        "1 talk 1 4\n2 pick 1 1\n3 pick 1 1\n4 leave 1\n5 done 1\n6 talk 1 3\n7 pick 1 1\n8 leave 1\n9 done 1\n",
    );
    assert_eq!(
        out,
        [
            "[1] menu: 1) Lena sent me",
            "[2] Dov: Oh. Them.",
            "[2] Dov: Times are hard. You know how it is.",
            // Ana's trade skill offers the first option; nobody's intimidation the second.
            "[2] menu: 1) Half now, half at harvest? 2) Then I'll be back.",
            "[3] Dov: ...Half now. Fine.",
            "[3] Dov: Here. Tell them we're square.",
            "[3] menu: 1) Lena sent me",
            // `DialogueEnded { exit: confront::paid }` at 4: joined, `collect` done.
            "[4] joined Ana",
            "[4] end Dov: Left",
            "[6] menu: 1) About the debt",
            "[7] Lena: You got it? Then here's your cut.",
            "[7] menu: 1) About the debt",
            "[8] give Ana 10 silver",
            "[8] completed Ana",
            "[8] end Lena: Left",
        ]
    );
}

#[test]
fn edit_rebinds_and_ends_a_gone_dialogue() {
    let mut w = world(SMITH, SMITH_PEOPLE, &[]).expect("placed");
    let script = parse_script("1 talk 1 3\n").expect("script");
    assert!(w.run(&script, 10));
    let renamed = SMITH.replace("dialogue hello", "dialogue greet");
    let (p, lines) = compiled(&renamed);
    w.edit(p, lines).expect("migrates");
    assert_eq!(w.output()[2..], ["[2] edited", "[2] end Hal: Edited"]);
    // An `edit` input goes through the edit hook.
    let mut w = world(SMITH, SMITH_PEOPLE, &[]).expect("placed");
    w.set_edit(Box::new(|dir: &str| {
        assert_eq!(dir, "v2");
        Ok(compiled(&SMITH.replace("Hello", "Howdy")))
    }));
    let out = run(
        &mut w,
        "1 talk 1 3\n2 edit v2\n3 leave 1\n4 done 1\n5 talk 1 3\n",
    );
    // The offer out is withdrawn and offered again, evaluated over the edited program.
    assert_eq!(
        out[2..5],
        ["[2] edited", "[2] menu: 1) Bye.", "[3] end Hal: Left"]
    );
    assert_eq!(out[5], "[5] Hal: Howdy, Ana.");
}

#[test]
fn lost_dog_bolts_at_nightfall_and_is_found_again() {
    let mut w = world(LOST_DOG, LOST_DOG_PEOPLE, LOST_DOG_ROLES).expect("placed");
    // Found at 8, not fed: the stage's timer fires 9000 ticks later and `Search` is entered again.
    let out = run(
        &mut w,
        "1 talk 1 3\n2 pick 1 1\n3 pick 1 1\n5 leave 1\n6 done 1\n8 near 1 7\n9200 talk 1 3\n",
    );
    let tail: Vec<&str> = out.iter().skip(9).map(String::as_str).collect();
    // `attempts` counted both entries of `Search`; Ana, a participant, is offered no topic.
    assert_eq!(
        tail,
        [
            "[9200] Hal: Found and lost 2 times now. He's frightened.",
            "[9200] end Hal: Finished",
        ]
    );
}

const SLOW_PART: &str = r#"quest M:
    role pet: Creature
    let count = 0
    event Bump:
        n: int
    @on(Bump)
    fn bump(e):
        count = count + e.n
    stage S:
        @on(Entered)
        fn s(e):
            pass
    part watch on pet:
        @on(PlayerNear)
        fn near(e):
            let i = 0
            while i < 300:
                i = i + 1
            print(f"part sees {count}")
"#;

#[test]
fn mirror_waits_while_the_part_is_mid_segment() {
    let (p, _) = compiled(SLOW_PART);
    let people = parse_people(PART_PEOPLE).expect("people");
    let roles = roles(PART_PEOPLE, &[("pet", "Creature(7)")]);
    let mut w = World::new(p, people, roles, StubConfig::default(), 200, MEM).expect("placed");
    let script = parse_script("2 near 1 7\n2 event M::Bump n=1\n").expect("script");
    let mut installed = Vec::new();
    for t in 0..40u64 {
        let inputs: Vec<Input> = script
            .iter()
            .filter(|(at, _)| *at == t)
            .map(|(_, i)| i.clone())
            .collect();
        let r = w.step(&inputs);
        if !r.mirrors.is_empty() {
            installed.push(r.tick);
        }
    }
    // The handler ran over several ticks on the mirror it started with; the home's change waited for its end.
    let printed = w
        .output()
        .iter()
        .find(|l| l.ends_with("part sees 0"))
        .expect("the part's line");
    let at: u64 = printed[1..printed.find(']').expect("a tick")]
        .parse()
        .expect("a tick");
    assert!(at > 3, "{printed}");
    assert_eq!(installed, [1, at + 1]);
}

#[test]
fn open_event_fields_and_after_of_a_host_event_say_why() {
    let mut api = stub_api();
    api.events.push(quest_vm::host::HostStruct {
        name: "Odd".into(),
        fields: vec![("x".into(), quest_vm::host::HostTy::Value)],
    });
    let messages = |src: &str| -> Vec<String> {
        match quest_lang::compile(&tree(&[("main.qs", src)]), &api) {
            Ok(_) => Vec::new(),
            Err(d) => d.into_iter().map(|d| d.message).collect(),
        }
    };
    let m = messages("@on(Odd)\nfn odd(e):\n    pass\n");
    assert!(m.iter().any(|m| m.contains("open type (`Value`)")), "{m:?}");
    let m = messages(
        "quest Q:\n    role a: Person\n    stage S:\n        @on(Entered)\n        fn e(e):\n            after(5, Setup())\n",
    );
    assert!(
        m.iter()
            .any(|m| m.contains("a timer delivers a script `event` only")),
        "{m:?}"
    );
}

const ATTACHING: &str = r#"repeatable quest R(cooldown=5):
    role watch: Person
    event Go:
        player: Player
    dialogue chat(p: Player, npc: Person):
        npc: "Well met." @r.hi
        choice:
            "Bye." @r.bye
    @on(Setup)
    fn setup(e):
        attach_dialogue(watch, chat)
    stage A:
        @on(Go)
        fn go(e):
            complete([e.player])
"#;

#[test]
fn repeatable_reset_clears_attachments_and_conversations() {
    let mut w = world(ATTACHING, SMITH_PEOPLE, &[("watch", "Person(3)")]).expect("placed");
    let out = run(
        &mut w,
        "2 talk 1 3\n3 event R::Go player=Player(1)\n4 done 1\n5 talk 1 3\n6 done 1\n10 talk 2 3\n",
    );
    assert_eq!(
        out,
        [
            "[2] Hal: Well met.",
            "[2] menu: 1) Bye.",
            "[3] completed Ana",
            // The reset ends the open conversation (`Left`) and drops the run's attachment: nothing to
            // say at 5, until the next `Setup` (at 8) attaches it again.
            "[3] end Hal: Left",
            "[3] reaped",
            "[10] Hal: Well met.",
            "[10] menu: 1) Bye.",
        ]
    );
}

const SLOW_HOME: &str = r#"quest D:
    role keeper: Person
    let flag = 0
    event Go:
        n: int
    @on(Go)
    fn go(e):
        flag = e.n
        let i = 0
        while i < 300:
            i = i + 1
    stage S:
        dialogue hi(p: Player, npc: Person) for keeper:
            npc: f"flag is {flag}" @d.hi
"#;

#[test]
fn conversation_reads_the_dialogue_mirror_not_uncommitted_state() {
    let (p, lines) = compiled(SLOW_HOME);
    let people = parse_people(SMITH_PEOPLE).expect("people");
    let roles = roles(SMITH_PEOPLE, &[("keeper", "Person(3)")]);
    let mut w = World::new(p, people, roles, StubConfig::default(), 200, MEM).expect("placed");
    w.set_lines(lines);
    // `go` writes `flag` at 2 and runs on for several ticks: a talk meanwhile sees the committed 0.
    let out = run(
        &mut w,
        "2 event D::Go n=7\n3 talk 1 3\n4 done 1\n40 talk 2 3\n41 done 2\n",
    );
    assert_eq!(
        out,
        [
            "[3] Hal: flag is 0",
            "[3] end Hal: Finished",
            "[40] Hal: flag is 7",
            "[40] end Hal: Finished",
        ]
    );
}

const SLOW_SETUP: &str = r#"quest D:
    role keeper: Person
    let flag = 0
    @on(Setup)
    fn setup(e):
        let i = 0
        while i < 300:
            i = i + 1
        flag = 1
    dialogue hi(p: Player, npc: Person) for keeper:
        npc: f"flag is {flag}" @d.hi
    stage S:
        @on(Entered)
        fn s(e):
            pass
"#;

#[test]
fn talk_during_a_multi_tick_setup_reads_the_placed_mirror() {
    let (p, lines) = compiled(SLOW_SETUP);
    let people = parse_people(SMITH_PEOPLE).expect("people");
    let roles = roles(SMITH_PEOPLE, &[("keeper", "Person(3)")]);
    let mut w = World::new(p, people, roles, StubConfig::default(), 200, MEM).expect("placed");
    w.set_lines(lines);
    // `setup` runs over several ticks from tick 0: the talk at 1 reads the mirror taken at placement.
    let out = run(&mut w, "1 talk 1 3\n2 done 1\n");
    assert_eq!(out, ["[1] Hal: flag is 0", "[1] end Hal: Finished"]);
}

/// Review A2 (whole-branch, Task 52): an edit kept the mirrors pending for the parts (and what was sent)
/// from the old program, whose global indices mean other globals in the new one: one held back by a part
/// mid-segment was installed after the edit.
#[test]
fn an_edit_drops_the_old_programs_pending_mirrors() {
    let src = SLOW_PART.replace(
        "    stage S:\n",
        "    event Slow:\n        n: int\n    @on(Slow)\n    fn slow(e):\n        let i = 0\n        while i < 300:\n            i = i + 1\n    stage S:\n",
    );
    let (p, lines) = compiled(&src);
    let people = parse_people(PART_PEOPLE).expect("people");
    let roles = roles(PART_PEOPLE, &[("pet", "Creature(7)")]);
    let mut w = World::new(p, people, roles, StubConfig::default(), 200, MEM).expect("placed");
    w.set_lines(lines);
    let edited = src.replace(
        "    let count = 0\n",
        "    let extra = 7\n    let count = 0\n",
    );
    w.set_edit(Box::new(move |_: &str| Ok(compiled(&edited))));
    // The bump's mirror waits for the part (mid-segment from 2); the edit comes meanwhile, with the home
    // mid-segment at the end of its tick (no new mirror is taken then).
    let out = run(
        &mut w,
        "1 near 1 7\n2 event M::Bump n=1\n3 event M::Slow n=0\n3 edit v2\n40 near 1 7\n",
    );
    assert!(
        !out.iter().any(|l| l.contains("refused")),
        "{}",
        out.join("\n")
    );
    assert!(
        out.last().is_some_and(|l| l.ends_with("part sees 1")),
        "{}",
        out.join("\n")
    );
}
