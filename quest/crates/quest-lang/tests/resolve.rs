//! Module loading and name resolution (plan Task 19).

mod common;

use common::{diagnostics, diags};

/// One expected diagnostic: code, file, line.
fn d(code: &str, file: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), file.to_string(), line)
}

/// [`diags`] without the warning `missing-line-id` every line written without an id gets.
fn errs(files: &[(&str, &str)]) -> Vec<(String, String, u32)> {
    diags(files)
        .into_iter()
        .filter(|(code, _, _)| code != "missing-line-id")
        .collect()
}

/// A `Person` for dialogue signatures (the stub host has none).
const PERSON: &str = "struct Person:\n    name: str\n";

#[test]
fn spec_module_tree_resolves() {
    let main = r#"mod rewards
mod npc
use rewards::give_gold
use npc::dialogue::{greet, farewell}
use rewards::give_gold as pay

pub struct Ledger:
    entries: list[int]

fn main():
    give_gold(1)
    pay(2)
    greet()
    farewell()
    let l = Ledger(entries=[])
"#;
    let rewards = "pub fn give_gold(n: int):\n    print(f\"{n}\")\n";
    let npc = "pub mod dialogue\n";
    let dialogue = "pub fn greet():\n    print(\"hi\")\n\npub fn farewell():\n    print(\"bye\")\n";
    let files = [
        ("main.qs", main),
        ("rewards.qs", rewards),
        ("npc/mod.qs", npc),
        ("npc/dialogue.qs", dialogue),
    ];
    assert_eq!(diags(&files), vec![]);
}

#[test]
fn mod_errors() {
    assert_eq!(
        diags(&[("main.qs", "mod nope\n")]),
        vec![d("mod-not-found", "main.qs", 1)]
    );
    assert_eq!(
        diags(&[
            ("main.qs", "fn main():\n    pass\n\nmod x\n"),
            ("x.qs", "let a = 1\n"),
            ("x/mod.qs", "let b = 2\n"),
        ]),
        vec![d("mod-ambiguous", "main.qs", 4)]
    );
    // `self`, `crate` and `super` parse as module names but name no module.
    assert_eq!(
        diags(&[("main.qs", "mod self\nmod crate\nmod super\n")]),
        vec![
            d("syntax", "main.qs", 1),
            d("syntax", "main.qs", 2),
            d("syntax", "main.qs", 3)
        ]
    );
}

#[test]
fn visibility() {
    let main = r#"mod a
fn main():
    a::f()
    let s = a::S(x=1)
    let e = a::E::V
    let g = a::G
    a::pf()
    let ps = a::PS(x=1)
    let pe = a::PE::V
    let pg = a::PG
"#;
    let a = r#"fn f():
    pass
struct S:
    x: int
enum E:
    V
let G = 1
pub fn pf():
    pass
pub struct PS:
    x: int
pub enum PE:
    V
pub let PG = 2
"#;
    let files = [("main.qs", main), ("a.qs", a)];
    assert_eq!(
        diags(&files),
        vec![
            d("private", "main.qs", 3),
            d("private", "main.qs", 4),
            d("private", "main.qs", 5),
            d("private", "main.qs", 6),
        ]
    );
    let messages: Vec<String> = diagnostics(&files).into_iter().map(|d| d.message).collect();
    for name in ["`f`", "`S`", "`E`", "`G`"] {
        assert!(
            messages.iter().any(|m| m.contains(name)),
            "no message names {name}: {messages:#?}"
        );
    }

    // `crate::`, `super::`, `self::` and `use … as`; a child module sees its ancestors' private items.
    let main = "mod a\n\npub fn top():\n    pass\n\nfn main():\n    a::b::run()\n";
    let a = "pub mod b\n\nfn helper():\n    pass\n";
    let b = r#"use super::helper as h

pub fn run():
    crate::top()
    super::helper()
    self::local()
    h()

fn local():
    pass
"#;
    assert_eq!(
        diags(&[("main.qs", main), ("a.qs", a), ("a/b.qs", b)]),
        vec![]
    );

    // `pub use` cannot re-export a private item.
    let a = "fn secret():\n    pass\n\npub use self::secret as open\n";
    assert_eq!(
        diags(&[("main.qs", "mod a\n"), ("a.qs", a)]),
        vec![d("private", "a.qs", 4)]
    );
}

#[test]
fn use_cycles_between_modules_are_fine() {
    let main = "mod a\nmod b\nuse a::g2\n\nfn main():\n    a::f()\n    g2()\n";
    let a = "use crate::b::g\npub use crate::b::g as g2\n\npub fn f():\n    g()\n";
    let b = "use crate::a::f\n\npub fn g():\n    pass\n";
    assert_eq!(
        diags(&[("main.qs", main), ("a.qs", a), ("b.qs", b)]),
        vec![]
    );
}

#[test]
fn scopes() {
    let main = r#"fn main():
    if true:
        let inner = 1
    print(f"{inner}")
    let x = 1
    if true:
        print(f"{x}")
    let x = "s"
    y = 2
    match Some(1):
        case Some(v):
            print(f"{v}")
        case None:
            pass
    print(f"{v}")
    for i in range(3):
        print(f"{i}")
    print(f"{i}")
"#;
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("assign-undeclared", "main.qs", 9),
            d("unresolved", "main.qs", 4),
            d("unresolved", "main.qs", 15),
            d("unresolved", "main.qs", 18),
        ]
    );
}

#[test]
fn unresolved_suggests() {
    let all = diagnostics(&[("main.qs", "fn main():\n    pritn(\"x\")\n")]);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!((all[0].code, all[0].line), ("unresolved", 2));
    assert!(
        all[0].notes.iter().any(|n| n.contains("`print`")),
        "{all:#?}"
    );
}

#[test]
fn duplicates() {
    let main = "fn f():\n    pass\nfn f():\n    pass\nstruct S:\n    a: int\n    a: str\n";
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("duplicate-def", "main.qs", 3),
            d("duplicate-def", "main.qs", 7)
        ]
    );
    // One name bound twice in a pattern; `|` alternatives each bind it once.
    let main = r#"struct Pair:
    x: int
    y: int
fn m(p: Pair):
    match p:
        case Pair(a, a):
            pass
        case Pair(a, 1) | Pair(1, a):
            pass
        case _:
            pass
"#;
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![d("duplicate-def", "main.qs", 6)]
    );
}

#[test]
fn host_names() {
    let main = r#"@on(PlayerClicked)
fn h(e: PlayerClicked):
    let v: Promise[int] = world.get("k")
    say(e.player, "hi")

fn no_such():
    let w = world.nope

fn shadowed():
    let world = 1
    let w = world.nope
"#;
    // The shadowed `world` is the `int`: its `.nope` is a field the type checker refuses.
    assert_eq!(
        diags(&[("main.qs", main)]),
        vec![
            d("unknown-field", "main.qs", 11),
            d("unresolved", "main.qs", 7)
        ]
    );
}

#[test]
fn error_names_need_no_import() {
    let main = r#"mod a

fn main():
    match a::f():
        case Ok(_):
            pass
        case Err(ChestEmpty):
            pass
"#;
    let a = "pub fn f() -> int:\n    raise ChestEmpty\n";
    assert_eq!(diags(&[("main.qs", main), ("a.qs", a)]), vec![]);

    // An upper-case head of `raise` is an error name even when an item has that name; a lower-case one
    // must be a binding.
    // A bare caught error is matched by its error names; resolution leaves them to the type checker.
    let main = r#"struct Ledger:
    x: int
fn f() -> int:
    raise Ledger
fn g() -> int:
    let n = f() else err:
        raise er
    return n
fn h() -> int:
    let n = take() else err:
        match err:
            case NotEnough(have):
                return have
            case ChestEmpty:
                return 0
    return n
fn take() -> int:
    if true:
        raise NotEnough(have=1)
    raise ChestEmpty
"#;
    let all = diagnostics(&[("main.qs", main)]);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!((all[0].code, all[0].line), ("unresolved", 7));
    assert!(all[0].notes.iter().any(|n| n.contains("`err`")), "{all:#?}");
}

#[test]
fn dialogue_placement() {
    // `objective` outside a stage, `role` outside a quest.
    let main = "objective x \"x\"\nrole r: Player\nquest Q:\n    objective y \"y\"\n";
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("quest-only", "main.qs", 1),
            d("quest-only", "main.qs", 2),
            d("quest-only", "main.qs", 4),
        ]
    );

    // Nodes mixed with steps.
    let main = format!(
        "{PERSON}dialogue d(p: Player, npc: Person):\n    node a:\n        npc: \"x\"\n    npc: \"y\"\n"
    );
    assert_eq!(
        errs(&[("main.qs", &main)]),
        vec![d("mixed-dialogue-body", "main.qs", 6)]
    );

    // Jumps and detours.
    let main = format!(
        r#"{PERSON}dialogue d(p: Player, npc: Person):
    node a:
        -> nowhere
        detour nowhere
        -> a
        detour d

quest Q:
    stage One:
        @on(PlayerClicked)
        fn go(e):
            -> Two
    stage Two:
        objective x "x"

fn other():
    -> Two
"#
    );
    assert_eq!(
        errs(&[("main.qs", &main)]),
        vec![
            d("jump-target", "main.qs", 5),
            d("jump-target", "main.qs", 6),
            d("jump-target", "main.qs", 19),
        ]
    );

    // `emit`.
    let main = format!(
        r#"{PERSON}event Mine:
    n: int

dialogue d(p: Player, npc: Person):
    emit Nope()
    emit Mine(n=1)
    emit PlayerClicked(player=p)
"#
    );
    assert_eq!(
        errs(&[("main.qs", &main)]),
        vec![d("unresolved", "main.qs", 7)]
    );

    // Signatures.
    let main = format!(
        "{PERSON}dialogue d(p: Player):\n    \"x\"\ndialogue e(p: Player, npc: Person, x: int):\n    \"y\"\n"
    );
    assert_eq!(
        errs(&[("main.qs", &main)]),
        vec![
            d("dialogue-signature", "main.qs", 3),
            d("dialogue-signature", "main.qs", 5),
        ]
    );

    // A quest outside the crate root; two quests.
    let quest = "quest Q:\n    let x = 1\n";
    assert_eq!(
        errs(&[("main.qs", "mod a\n"), ("a.qs", quest)]),
        vec![d("quest-placement", "a.qs", 1)]
    );
    let two = "quest Q:\n    let x = 1\nquest R:\n    let y = 1\n";
    assert_eq!(
        errs(&[("main.qs", two)]),
        vec![d("quest-placement", "main.qs", 3)]
    );

    // Decorators.
    let main = format!(
        "{PERSON}@pick(best)\nfn f():\n    pass\n@on(PlayerClicked)\ndialogue d(p: Player, npc: Person):\n    \"x\"\n"
    );
    assert_eq!(
        errs(&[("main.qs", &main)]),
        vec![
            d("bad-decorator", "main.qs", 3),
            d("bad-decorator", "main.qs", 6),
        ]
    );

    // Line ids are unique across modules.
    let main = format!(
        "{PERSON}mod a\ndialogue d(p: Player, npc: Person):\n    npc: \"x\" @farmer.start\n"
    );
    let a =
        "use crate::Person\n\ndialogue e(p: Player, npc: Person):\n    npc: \"y\" @farmer.start\n";
    assert_eq!(
        errs(&[("main.qs", &main), ("a.qs", a)]),
        vec![d("duplicate-line-id", "a.qs", 4)]
    );
}

/// Spec §5.11's farmer and greeting dialogues, with what they name declared.
const FARMER: &str = r#"struct Person:
    name: str

let farmer = Person(name="Tom")
let innkeeper = Person(name="Ida")
let accepted = false
let dog_name = "Rufus"

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog? He ran off toward the hills."     @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
                npc: "Bless you. Bring him back before dark."          @farmer.thanks
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            once "Is that blood on your sleeve?" @farmer.blood:
                detour farmer_secret
            "Goodbye." @farmer.goodbye
        npc: "Safe roads."                                             @farmer.bye

dialogue farmer_secret(p: Player, npc: Person):
    npc: "It is not mine."                                             @farmer.secret

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "Weather's turning."                                   @greet.b
        => npc: "You again?"                                           @greet.c when once per p
"#;

/// The examples' Lost Dog (quests-examples §1) and Debt to Settle (§4), in one program, plus a read of
/// another stage's objective (`Search.find_dog`).
const LOST_DOG: &str = r#"let NIGHT_TICKS = 9000

pub dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Times are hard."                                              @trade.hard
    choice:
        "Half now, half at harvest?" @trade.half if p.skill("trade") >= 2:
            return agreed
        "Then I'll be back." @trade.back:
            return refused

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller and near(miller, farmer, regions=1)

    let attempts = 0

    event Accepted:
        player: Player
    event DogFound:
        player: Player
    event Nightfall:
        attempt: int
    event Reunited:
        player: Player

    stage Open:
        dialogue worry(p: Player, npc: Person) for farmer:
            node hi when once per p:
                npc: "Stranger! Have you seen a brown dog?"             @lostdog.worry.first
            node hi:
                => npc: "Still no sign of him."                         @lostdog.worry.a
                => npc: "I left the gate open."                         @lostdog.worry.b

        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills."                @lostdog.plea
            choice:
                "I'll find him." @lostdog.accept:
                    emit Accepted(player=p)
                "Not now." @lostdog.not_now

        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    stage Search:
        objective find_dog f"Find {dog.name}" @lostdog.find at dog
        objective feed_dog "Coax him with food" @lostdog.feed at dog count 2
        optional objective ask_miller "Ask at the mill" @lostdog.ask_mill at miller
        -> Returning when find_dog.done and feed_dog.done

        dialogue confront(p: Player, npc: Person) for miller topic "About the grain" @lostdog.grain exits paid, stalled:
            npc: "Oh. You."                                             @lostdog.oh
            detour haggle:
                agreed:
                    return paid
                refused:
                    return stalled

        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1

        @on(DogFound)
        fn found(e):
            if not find_dog.done:
                find_dog.finish()
                after(NIGHT_TICKS, Nightfall(attempt=attempts))

        @on(Nightfall)
        fn bolted(e):
            -> Search

    stage Returning:
        objective bring_home "Bring him home" @lostdog.return at farmer

        @on(Reunited)
        fn reunited(e):
            if Search.find_dog.done:
                bring_home.finish()
            for q in participants:
                give(q, Item(kind="carved_whistle", count=1))
            complete()

    @on(DialogueEnded)
    fn after_talk(e):
        if e.dialogue == confront and e.exit == confront::paid and e.reason == EndReason::Finished:
            join(e.player)

    @on(RoleLost)
    fn lost(e):
        match e.role:
            case Role::miller:
                pass
            case _:
                fail()

    part dog_watch on dog:
        let seen = 0

        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants and not find_dog.done:
                emit DogFound(player=e.player)
"#;

fn resolved(files: &[(&str, &str)]) -> quest_lang::hir::Crate {
    let (krate, _) = quest_lang::load_modules(&common::tree(files));
    quest_lang::resolve(&krate, &quest_stub::stub_api()).0
}

fn find(program: &quest_lang::hir::Crate, qualified: &str) -> Option<quest_lang::hir::DefId> {
    (0..program.defs.len())
        .map(|i| quest_lang::hir::DefId(i as u32))
        .find(|d| program.qualified_name(*d) == qualified)
}

#[test]
fn quest_names_resolve() {
    use quest_lang::hir::{DefKind, PartAnchor, RoleIdx, StageIdx, StepKind};
    let files = [("main.qs", LOST_DOG)];
    assert_eq!(errs(&files), vec![]);
    let program = resolved(&files);
    for name in [
        "LostDog::Stage",
        "LostDog::Role",
        "LostDog::stage",
        "LostDog::participants",
        "LostDog::Entered",
        "LostDog::RoleLost",
        "LostDog::Search.find_dog",
        "LostDog::Returning.bring_home",
        "LostDog::role.farmer",
        "LostDog::role.miller",
        "LostDog::attempts",
        "LostDog::Accepted",
        "LostDog::plea",
        "LostDog::accepted",
        "LostDog::dog_watch::seen",
        "LostDog::dog_watch::spotted",
    ] {
        assert!(find(&program, name).is_some(), "no item {name}");
    }
    let quest = find(&program, "LostDog").expect("the quest");
    let Some(DefKind::Quest(q)) = program.def(quest).map(|d| &d.kind) else {
        panic!("not a quest");
    };
    let stages: Vec<&str> = q.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(stages, ["Open", "Search", "Returning"]);
    let roles: Vec<(&str, bool)> = q
        .roles
        .iter()
        .map(|r| (r.name.as_str(), r.optional))
        .collect();
    assert_eq!(roles, [("farmer", false), ("dog", false), ("miller", true)]);
    assert!(q.roles.iter().all(|r| r.cond.is_some()));
    assert_eq!(q.requires.len(), 1);
    assert_eq!(q.home, RoleIdx(0));
    let search = &q.stages[1];
    let objectives: Vec<(&str, Option<i64>, bool)> = search
        .objectives
        .iter()
        .map(|o| (o.name.as_str(), o.count, o.optional))
        .collect();
    assert_eq!(
        objectives,
        [
            ("find_dog", None, false),
            ("feed_dog", Some(2), false),
            ("ask_miller", None, true)
        ]
    );
    assert_eq!(
        search.objectives[0].global,
        find(&program, "LostDog::Search.find_dog").unwrap()
    );
    assert_eq!(search.transitions.len(), 1);
    assert_eq!(search.transitions[0].0, StageIdx(2));
    assert_eq!(search.handlers.len(), 3);
    assert_eq!(q.handlers.len(), 2);
    assert_eq!(q.parts.len(), 1);
    assert_eq!(
        (q.parts[0].anchor, q.parts[0].role),
        (PartAnchor::On, RoleIdx(1))
    );
    // `-> Search` in a handler is a stage change.
    let bolted = find(&program, "LostDog::bolted").unwrap();
    let Some(DefKind::Fn(f)) = program.def(bolted).map(|d| &d.kind) else {
        panic!("not a fn");
    };
    assert!(matches!(
        f.body.as_slice(),
        [quest_lang::hir::Stmt {
            kind: quest_lang::hir::StmtKind::Stage(StageIdx(1)),
            ..
        }]
    ));
    // Exits: `done` first, then the declared ones; the detour's arms by the callee's exit index.
    let confront = find(&program, "LostDog::confront").unwrap();
    let Some(DefKind::Dialogue(dl)) = program.def(confront).map(|d| &d.kind) else {
        panic!("not a dialogue");
    };
    assert_eq!(dl.exits, ["done", "paid", "stalled"]);
    assert_eq!(dl.quest, Some(quest));
    assert_eq!(dl.stage, Some(StageIdx(1)));
    let haggle = find(&program, "haggle").unwrap();
    let detour = &dl.nodes[0].steps[1].kind;
    let StepKind::Detour(callee, arms) = detour else {
        panic!("{detour:?}");
    };
    assert_eq!(*callee, haggle);
    let arms: Vec<(u32, &StepKind)> = arms.iter().map(|(i, s)| (*i, &s[0].kind)).collect();
    assert_eq!(arms, [(1, &StepKind::Return(1)), (2, &StepKind::Return(2))]);

    // Another stage's objective that does not exist; a stage used as a value.
    let bad = LOST_DOG
        .replace("if Search.find_dog.done:", "if Search.lost_dog.done:")
        .replace("attempts = attempts + 1", "let s = Open");
    assert_eq!(
        errs(&[("main.qs", &bad)]),
        vec![
            d("unresolved", "main.qs", 64),
            d("unresolved", "main.qs", 81)
        ]
    );
}

/// Spec §5.11's farmer and greeting dialogues against the stub host, split over modules.
const FARMER_MOD: &str = r#"pub let farmer = Person(id=1)
let innkeeper = Person(id=2)
let accepted = false
let dog_name = "Rufus"

event Accepted:
    player: Player

@pick(best)
dialogue farmer_talk(p: Player, npc: Person) for farmer:
    node start:
        npc: "Have you seen my dog?"                                   @farmer.start
        choice:
            "I'll find him." @farmer.accept if not accepted:
                emit Accepted(player=p)
            "What does he look like?" @farmer.ask_dog:
                npc: f"Brown, one ear up. He answers to {dog_name}."   @farmer.dog_desc
                -> start
            "Goodbye." @farmer.goodbye

dialogue greeting(p: Player, npc: Person) for innkeeper:
    node hello when p.has_tag("outlaw"):
        npc: "Keep walking."                                           @greet.outlaw
    node hello when once per p:
        npc: "A new face! Welcome."                                    @greet.first
    node hello:
        => npc: "Morning."                                             @greet.a
        => npc: "You again?"                                           @greet.c when once per p
"#;

#[test]
fn synthesised_fns_are_named_stably() {
    let other = "pub fn helper() -> int:\n    return 1\n";
    let a = [
        ("main.qs", "mod farm\nmod other\n"),
        ("farm.qs", FARMER_MOD),
        ("other.qs", other),
    ];
    let b = [
        ("other.qs", other),
        ("farm.qs", FARMER_MOD),
        ("main.qs", "mod other\nmod farm\n"),
    ];
    assert_eq!(errs(&a), vec![]);
    // The spec's own farmer (its `Person` a script struct) checks clean too.
    assert_eq!(errs(&[("main.qs", FARMER)]), vec![]);
    let sigs = |files: &[(&str, &str)]| {
        quest_lang::signatures(&common::tree(files), &quest_stub::stub_api())
    };
    let (sa, sb) = (sigs(&a), sigs(&b));
    assert_eq!(sa, sb);
    let synthesised: Vec<(&str, &str)> = sa
        .iter()
        .filter(|(n, _)| n.contains('$'))
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        synthesised,
        [
            ("farm::farmer_talk$cond#0", "fn(Player, Person) -> bool"),
            (
                "farm::farmer_talk$emit_arg#0",
                "fn(Player, Person) -> Player"
            ),
            ("farm::farmer_talk$subst#0", "fn(Player, Person) -> str"),
            ("farm::farmer_talk$target#0", "fn(Player, Person) -> Person"),
            ("farm::greeting$cond#0", "fn(Player, Person) -> bool"),
            ("farm::greeting$per#0", "fn(Player, Person) -> Player"),
            ("farm::greeting$per#1", "fn(Player, Person) -> Player"),
            ("farm::greeting$target#0", "fn(Player, Person) -> Person"),
        ]
    );
}

#[test]
fn objective_shadowing_role_is_duplicate_def() {
    // Quests-examples §6.
    let main = "quest Q:\n    role dog: Creature\n    stage Search:\n        objective dog \"Find the dog\" at dog\n";
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![d("duplicate-def", "main.qs", 4)]
    );
    // An objective shadowing a `let` or another objective; two stages, parts or exits of one name.
    let main = r#"quest Q:
    role dog: Creature
    let found = 0
    stage A:
        objective found "x"
        objective seek "x"
    stage B:
        objective seek "y"
    stage A:
        objective other "z"
    part p on dog:
        let x = 1
    part p at dog:
        let y = 1
    dialogue d(p: Player, npc: Person) exits gone, gone, done:
        "x"
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("duplicate-def", "main.qs", 5),
            d("duplicate-def", "main.qs", 8),
            d("duplicate-def", "main.qs", 9),
            d("duplicate-def", "main.qs", 13),
            d("duplicate-def", "main.qs", 15),
            d("duplicate-def", "main.qs", 15),
        ]
    );
    // A name the quest generates.
    let main = "quest Q:\n    let stage = 1\n    event Entered:\n        n: int\n";
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("duplicate-def", "main.qs", 2),
            d("duplicate-def", "main.qs", 3)
        ]
    );
}

#[test]
fn entered_outside_stage() {
    let main = r#"quest Q:
    role dog: Creature
    @on(Entered)
    fn body(e):
        pass
    stage S:
        @on(Entered)
        fn ok(e):
            pass
    part watch on dog:
        @on(Entered)
        fn part_entered(e):
            pass
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("entered-outside-stage", "main.qs", 3),
            d("entered-outside-stage", "main.qs", 11),
        ]
    );
}

#[test]
fn unknown_exit_in_return_and_arm() {
    // Quests-examples §6, and a `return` of an undeclared exit.
    let main = r#"pub dialogue haggle(p: Player, npc: Person) exits agreed, refused:
    npc: "Well?"
    return agreed

dialogue offer(p: Player, npc: Person) exits sold:
    detour haggle:
        agreed:
            npc: "Done."
            return sold
        cheated:
            npc: "Hey!"
        done:
            return
    return nope
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("unknown-exit", "main.qs", 10),
            d("unknown-exit", "main.qs", 14)
        ]
    );
}

#[test]
fn scene_choice_and_follow_up() {
    let main = r#"scene quarrel(a: Person, b: Person):
    a: "Well?"
    choice:
        "Yes"

scene market(a: Person, b: Person) where a != b:
    b: "How much?"
    pause 20
    a: "Two coppers."                     (ticks=90)
    -> market_after

scene market_after(a: Person, b: Person):
    pause 40
    b: "...Fine. Two."

scene early(a: Person, b: Person):
    -> market_after
    a: "And then?"

scene alone(a: Person):
    a: "Hm."
    -> market_after

scene nested(a: Person, b: Person):
    when a != b:
        -> market_after

scene caster(a: Person, x: int):
    a: "x"
    detour quarrel
    p: "who?"

dialogue crier(npc: Person):
    npc: "Hear ye!"
    choice:
        "What?"
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("bark-choice", "main.qs", 35),
            d("dialogue-signature", "main.qs", 28),
            d("not-a-step", "main.qs", 30),
            d("scene-choice", "main.qs", 3),
            d("scene-follow-up", "main.qs", 17),
            d("scene-follow-up", "main.qs", 22),
            d("scene-follow-up", "main.qs", 26),
            d("unresolved", "main.qs", 31),
        ]
    );
    let program = resolved(&[("main.qs", main)]);
    let market = find(&program, "market").unwrap();
    let after = find(&program, "market_after").unwrap();
    let Some(quest_lang::hir::DefKind::Dialogue(m)) = program.def(market).map(|d| &d.kind) else {
        panic!("not a dialogue");
    };
    use quest_lang::hir::StepKind;
    let steps: Vec<&StepKind> = m.nodes[0].steps.iter().map(|s| &s.kind).collect();
    assert!(matches!(steps[1], StepKind::Pause(20)));
    assert!(matches!(steps[2], StepKind::Line(l) if l.ticks == Some(90)));
    assert_eq!(steps[3], &StepKind::FollowUp(after));
    assert!(m.stateless && m.cast_cond.is_some());
}

#[test]
fn bad_line_text() {
    use quest_lang::ast;
    let main = "dialogue d(p: Player, npc: Person):\n    npc: \"x\" @d.x\n";
    let (mut krate, diags) = quest_lang::load_modules(&common::tree(&[("main.qs", main)]));
    assert!(diags.is_empty(), "{diags:#?}");
    // The grammar takes only literals: an analysis tool's tree may hold anything.
    let item = &mut krate.modules[0].ast.items[0];
    let ast::ItemKind::Dialogue(dl) = &mut item.kind else {
        panic!("not a dialogue");
    };
    let ast::DialogueEntry::Step(step) = &mut dl.body[0] else {
        panic!("not a step");
    };
    let ast::StepKind::Line(line) = &mut step.kind else {
        panic!("not a line");
    };
    line.text.kind = ast::ExprKind::Int(3);
    let (_, diags) = quest_lang::resolve(&krate, &quest_stub::stub_api());
    let codes: Vec<(&str, u32)> = diags.iter().map(|d| (d.code, d.line)).collect();
    assert_eq!(codes, [("bad-line-text", 2)]);
}

#[test]
fn missing_line_id_is_a_warning() {
    let main = r#"quest Q:
    stage S:
        objective o "Find it"
        dialogue d(p: Player, npc: Person):
            npc: "a"
            npc: "b" @d.b
            choice:
                "c":
                    pass
"#;
    let main = main.replace(
        "                    pass\n",
        "                    npc: \"e\"\n",
    );
    let all = diagnostics(&[("main.qs", &main)]);
    let found: Vec<(&str, u32, &quest_lang::Severity)> =
        all.iter().map(|d| (d.code, d.line, &d.severity)).collect();
    let w = &quest_lang::Severity::Warning;
    assert_eq!(
        found,
        [
            ("missing-line-id", 3, w),
            ("missing-line-id", 5, w),
            ("missing-line-id", 8, w),
            ("missing-line-id", 9, w),
        ]
    );
    // The provisional ids: the owner's qualified name and the line's position among its lines.
    assert!(all[0].message.contains("`@Q.0`"), "{}", all[0].message);
    assert!(all[1].message.contains("`@Q.d.0`"), "{}", all[1].message);
    assert!(all[2].message.contains("`@Q.d.2`"), "{}", all[2].message);
    assert!(
        quest_lang::check(
            &common::tree(&[("main.qs", &main)]),
            &quest_stub::stub_api()
        )
        .is_ok()
    );
}

#[test]
fn bad_part_item() {
    let main = r#"quest Q:
    role dog: Creature
    part watch on dog:
        stage S:
            objective o "x" @q.o
        objective p "x" @q.p
        dialogue d(p: Player, npc: Person):
            "x" @q.x
        scene s(a: Person):
            a: "x" @q.y
        part inner at dog:
            let x = 1
        role cat: Creature
        requires act() >= 1
        let ok = 1
        event Fine:
            n: int
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("bad-part-item", "main.qs", 4),
            d("bad-part-item", "main.qs", 6),
            d("bad-part-item", "main.qs", 7),
            d("bad-part-item", "main.qs", 9),
            d("bad-part-item", "main.qs", 11),
            d("bad-part-item", "main.qs", 13),
            d("bad-part-item", "main.qs", 14),
        ]
    );
}

#[test]
fn bad_quest_option() {
    let quest = |header: &str| format!("{header}\n    role w: Person\n");
    let one = |header: &str| errs(&[("main.qs", &quest(header))]);
    assert_eq!(
        one("quest Q(cooldown=10):"),
        vec![d("bad-quest-option", "main.qs", 1)]
    );
    assert_eq!(
        one("repeatable quest Q(nope=1):"),
        vec![d("bad-quest-option", "main.qs", 1)]
    );
    assert_eq!(
        one("repeatable quest Q(10):"),
        vec![d("bad-quest-option", "main.qs", 1)]
    );
    assert_eq!(
        one("repeatable quest Q(cooldown=act()):"),
        vec![d("bad-quest-option", "main.qs", 1)]
    );
    assert_eq!(
        one("repeatable quest Q(exclusive=1, cooldown=1, cooldown=2):"),
        vec![
            d("bad-quest-option", "main.qs", 1),
            d("bad-quest-option", "main.qs", 1)
        ]
    );
    let ok = quest("repeatable quest Q(cooldown=72000, exclusive=true) at w:");
    assert_eq!(errs(&[("main.qs", &ok)]), vec![]);
    let program = resolved(&[("main.qs", &ok)]);
    let q = find(&program, "Q").unwrap();
    let Some(quest_lang::hir::DefKind::Quest(q)) = program.def(q).map(|d| &d.kind) else {
        panic!("not a quest");
    };
    assert_eq!(
        q.repeatable,
        Some(quest_lang::hir::RepeatOpts {
            cooldown: 72000,
            exclusive: true,
            once_per_player: false
        })
    );
}

#[test]
fn decline_outside_setup() {
    let main = r#"quest Q:
    role mine: Site
    @on(Setup)
    fn setup(e):
        decline()
    stage S:
        @on(PlayerClicked)
        fn clicked(e):
            decline()
    part depths at mine:
        @on(Setup)
        fn nest(e):
            place(prefab("nest"), Place(id=1)) else:
                decline()
                return
        @on(PlayerNear)
        fn near(e):
            decline()
"#;
    assert_eq!(
        errs(&[("main.qs", main)]),
        vec![
            d("decline-outside-setup", "main.qs", 9),
            d("decline-outside-setup", "main.qs", 18),
        ]
    );
}

#[test]
fn emit_statement_names_the_hosts_emit() {
    // A script's own `emit` does not capture the statement `emit Ev(…)`; a plain call still finds it.
    let main = r#"fn emit(n: int) -> int:
    return n

@on(PlayerClicked)
fn clicked(e):
    emit PlayerEntered(player=e.player)
    let x = emit(3)
"#;
    assert_eq!(errs(&[("main.qs", main)]), vec![]);
}
