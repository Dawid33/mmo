//! Types and the Phase 10 checks (plan Task 39; spec §5.11–5.12): the types of dialogue, scene and quest
//! code, purity (`impure-condition`), package-level content (`stateless-dialogue`), the query subset
//! (`bad-where`), quest state writes (`role-assign`, `part-write`), objectives (`bad-objective-call`),
//! scores, read sets and `mirror-size`.

mod common;

use common::{diagnostics, diags, tree};
use quest_lang::Severity;
use quest_lang::hir::{Crate, DefId, DefKind, StepKind};

/// One expected diagnostic: code, file, line.
fn d(code: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), "main.qs".to_string(), line)
}

/// The diagnostics of `main.qs`, without the warning `missing-line-id`.
fn errs(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
        .into_iter()
        .filter(|(code, _, _)| code != "missing-line-id")
        .collect()
}

/// The error diagnostics only (no warnings).
fn errors_only(main: &str) -> Vec<(String, String, u32)> {
    let mut out: Vec<(String, String, u32)> = diagnostics(&[("main.qs", main)])
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| (d.code.to_string(), d.file, d.line))
        .collect();
    out.sort();
    out
}

/// The messages of every diagnostic with `code`.
fn messages(main: &str, code: &str) -> Vec<String> {
    diagnostics(&[("main.qs", main)])
        .into_iter()
        .filter(|d| d.code == code)
        .map(|d| d.message)
        .collect()
}

fn resolved(main: &str) -> Crate {
    let (krate, _) = quest_lang::load_modules(&tree(&[("main.qs", main)]));
    quest_lang::resolve(&krate, &quest_stub::stub_api()).0
}

fn find(program: &Crate, qualified: &str) -> DefId {
    (0..program.defs.len())
        .map(|i| DefId(i as u32))
        .find(|d| program.qualified_name(*d) == qualified)
        .unwrap_or_else(|| panic!("no item {qualified}"))
}

fn dialogue<'a>(program: &'a Crate, qualified: &str) -> &'a quest_lang::hir::Dialogue {
    match program.def(find(program, qualified)).map(|d| &d.kind) {
        Some(DefKind::Dialogue(dl)) => dl,
        other => panic!("{qualified} is not a dialogue: {other:?}"),
    }
}

#[test]
fn condition_must_be_bool() {
    let main = r#"dialogue d(p: Player, npc: Person):
    node a when 1:
        npc: "x" @d.a
    node b when npc.alive:
        npc: "y" @d.b
"#;
    assert_eq!(errs(main), vec![d("type-mismatch", 2)]);
    assert!(
        messages(main, "type-mismatch")[0].contains("a condition is a `bool`, not `int`"),
        "{:?}",
        messages(main, "type-mismatch")
    );
}

#[test]
fn emit_arg_types() {
    let main = r#"event Paid:
    amount: int
    who: Player

dialogue d(p: Player, npc: Person):
    emit Paid(amount="lots", who=p)
    emit Paid(amount=1)
    emit Paid(amount=1, who=p, tip=2)
    emit Paid(2, p)
    emit Paid(2, p, 3)
"#;
    assert_eq!(
        errs(main),
        vec![
            d("arg-count", 10),
            d("missing-field", 7),
            d("type-mismatch", 6),
            d("unknown-field", 8),
        ]
    );
}

#[test]
fn timeout_must_be_constant_int() {
    let main = r#"let T = 30
let N = -2
let S = "x"

dialogue d(p: Player, npc: Person):
    choice(timeout=T):
        "a" @d.a
    choice(timeout=S):
        "b" @d.b
    choice(timeout=-1):
        "c" @d.c
    pause N
    pause T

dialogue crier(npc: Person):
    npc: "hey" @crier.hey (ticks=-5)

quest Q:
    role r: Person
    stage A:
        objective o "o" @q.o count 0
        objective k "k" @q.k count 1
"#;
    assert_eq!(
        errs(main),
        vec![
            d("not-constant", 8),
            d("not-constant", 10),
            d("not-constant", 12),
            d("not-constant", 16),
            d("not-constant", 21),
        ]
    );
    let all = messages(main, "not-constant");
    // `let N = -2` is a constant; its value is out of range.
    assert!(all[2].contains("0 or more"), "{all:?}");
    assert!(all[4].contains("1 or more"), "{all:?}");
}

#[test]
fn impure_condition_direct() {
    let main = r#"dialogue d(p: Player, npc: Person):
    choice:
        "gift" @d.gift if give(p, Item(kind="a", count=1)) == ():
            npc: "ok" @d.ok
        "bye" @d.bye
"#;
    assert_eq!(errs(main), vec![d("impure-condition", 3)]);
    let m = &messages(main, "impure-condition")[0];
    assert!(m.contains("`give`"), "{m}");
}

#[test]
fn impure_condition_transitive() {
    let main = r#"let calls = 0

fn b() -> bool:
    calls = calls + 1
    return true

fn a() -> bool:
    return b()

dialogue d(p: Player, npc: Person):
    node n when a():
        npc: "x" @d.x
"#;
    assert_eq!(errs(main), vec![d("impure-condition", 11)]);
    let m = &messages(main, "impure-condition")[0];
    assert!(m.contains("a → b"), "{m}");
    assert!(m.contains("`calls`"), "{m}");
}

#[test]
fn impure_condition_escaping_error() {
    let main = r#"fn risky() -> bool:
    raise Oops

dialogue d(p: Player, npc: Person):
    node a when risky()?:
        npc: "x" @d.a
    node b when risky() else false:
        npc: "y" @d.b
    node c when npc.distance_to(p)? > 3:
        npc: "z" @d.c
    node e when (npc.distance_to(p) else 0) > 3:
        npc: "w" @d.e
"#;
    assert_eq!(
        errs(main),
        vec![d("impure-condition", 5), d("impure-condition", 9)]
    );
    let all = messages(main, "impure-condition");
    assert!(all[0].contains("Oops"), "{all:?}");
    assert!(all[1].contains("OutOfReach"), "{all:?}");
}

#[test]
fn pure_fn_with_locals_is_fine() {
    let main = r#"fn score(n: int) -> int:
    let t = n
    t = t + 1
    let xs = [t]
    xs.append(2)
    return xs.len() + t

dialogue d(p: Player, npc: Person):
    node a when score(p.skill("trade")) > 2 and dialogue_depth() < 3:
        npc: f"You score {score(1)}." @d.a
"#;
    assert_eq!(errs(main), vec![]);
}

#[test]
fn stateless_dialogue_reads_global() {
    let main = r#"let mood = 1
let farmer = Person(id=1)

fn cheerful() -> bool:
    return mood > 0

dialogue d(p: Player, npc: Person) for npc where npc.alive and cheerful():
    npc: "x" @d.x

scene chat(a: Person, b: Person) where mood > 0:
    a: "hi" @chat.a

dialogue own(p: Player, npc: Person) for farmer:
    node n when mood > 0:
        npc: "mine" @own.x
"#;
    assert_eq!(
        errs(main),
        vec![d("stateless-dialogue", 7), d("stateless-dialogue", 10)]
    );
    let all = messages(main, "stateless-dialogue");
    assert!(all[0].contains("cheerful"), "{all:?}");
}

#[test]
fn stateless_dialogue_emits() {
    let main = r#"event Hi:
    n: int

dialogue d(p: Player, npc: Person) for npc where npc.alive:
    emit Hi(n=1)
    npc: "x" @d.x
"#;
    assert_eq!(errs(main), vec![d("stateless-dialogue", 5)]);
}

#[test]
fn bad_where_script_fn() {
    let main = r#"fn farmerish(x: Person) -> bool:
    return x.alive

quest Q:
    role farmer: Person where farmerish(farmer)
"#;
    assert_eq!(errs(main), vec![d("bad-where", 5)]);
}

#[test]
fn bad_where_global() {
    let main = r#"let wanted = Occupation::Farmer

quest Q:
    let n = 1
    requires act() >= n
    role farmer: Person where farmer.occupation == wanted
"#;
    assert_eq!(errs(main), vec![d("bad-where", 5), d("bad-where", 6)]);
}

#[test]
fn bad_where_non_fact() {
    let main = r#"quest Q:
    role farmer: Person where farmer.alive and farmer.name == "Tom"
    role smith: Person where smith.alive and smith.kind == "dwarf"
    requires act() + 1 >= 2
"#;
    assert_eq!(errs(main), vec![d("bad-where", 2), d("bad-where", 4)]);
    let all = messages(main, "bad-where");
    assert!(all[0].contains("fact"), "{all:?}");
    assert!(all[1].contains("arithmetic"), "{all:?}");
}

#[test]
fn bad_where_refusals() {
    let main = r#"let occs = [Occupation::Farmer]

quest Q:
    role a: Person where not (a.alive and a.has_tag("x"))
    role b: Person where b.occupation in occs
    role c: Person where c.occupation in [a.occupation]
    role d: Person where 1 == 1 and d.alive
    role e: Person where abs(act()) > 1
"#;
    assert_eq!(
        errs(main),
        vec![
            d("bad-where", 4),
            d("bad-where", 5),
            d("bad-where", 6),
            d("bad-where", 7),
            d("bad-where", 8),
        ]
    );
    let all = messages(main, "bad-where");
    assert!(all[0].contains("`not` applies to one atom"), "{all:?}");
    assert!(all[1].contains("written out"), "{all:?}");
    assert!(all[2].contains("lists constants"), "{all:?}");
    assert!(all[3].contains("two constants"), "{all:?}");
    assert!(all[4].contains("host's facts"), "{all:?}");
}

#[test]
fn bad_where_later_role() {
    let main = r#"quest Q:
    role inn: Person where inn.site == farmer.site
    role farmer: Person
    requires farmer.alive
"#;
    assert_eq!(errs(main), vec![d("bad-where", 2), d("bad-where", 4)]);
}

#[test]
fn where_subset_accepts_all_atoms() {
    let main = r#"quest Q:
    requires act() >= 2 and loop() == 0 and not completed("errantry/x::Y")
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive and farmer.occupation in [Occupation::Farmer, Occupation::Miller]
    role dog: Creature where dog.has_tag("pet") and dog.kind == "dog" and dog.owner == farmer and site_kind(dog.site) == SiteKind::Village
    role inn: Person where inn.site == farmer.site and inn != farmer and related(inn, farmer) and near(inn, farmer, regions=2)
    optional role miller: Person where (miller.has_tag("a") or miller.has_tag("b")) and not miller.has_tag("c") and miller.occupation != Occupation::None
"#;
    assert_eq!(errs(main), vec![]);
    // An `or` joins atoms about one role.
    let two = "quest Q:\n    role a: Person\n    role b: Person where b.alive or a.alive\n";
    assert_eq!(errs(two), vec![d("bad-where", 3)]);
}

#[test]
fn role_assign() {
    let main = r#"quest Q:
    role farmer: Person
    stage A:
        objective o "o" @q.o
        @on(PlayerClicked)
        fn click(e):
            stage = Stage::A
            participants = []
            participants.append(e.player)
            farmer = Person(id=1)
            o = Objective(state=1, count=0)
            o.finish()
            join(e.player)
"#;
    assert_eq!(
        errs(main),
        vec![
            d("role-assign", 7),
            d("role-assign", 8),
            d("role-assign", 9),
            d("role-assign", 10),
            d("role-assign", 11),
        ]
    );
}

#[test]
fn part_write_assign() {
    let main = r#"quest Q:
    role dog: Creature
    let found = 0
    stage A:
        objective o "o" @q.o
    part watch on dog:
        let seen = 0
        @on(PlayerNear)
        fn near_dog(e):
            seen = seen + 1
            found = found + 1
            o.finish()
            join(e.player)
            complete()
            -> A
"#;
    assert_eq!(
        errs(main),
        vec![
            d("part-write", 11),
            d("part-write", 12),
            d("part-write", 13),
            d("part-write", 14),
            d("part-write", 15),
        ]
    );
}

#[test]
fn part_write_top_level_global() {
    // A program's top-level `let`s are the home's state, as the quest's are.
    let main = r#"let top = 0

quest Q:
    role dog: Creature
    part watch on dog:
        @on(PlayerNear)
        fn near_dog(e):
            top = top + 1
"#;
    assert_eq!(errs(main), vec![d("part-write", 8)]);
}

#[test]
fn part_write_via_called_fn() {
    // A handler is also a plain function: the part calls the quest's.
    let main = r#"quest Q:
    role dog: Creature
    let found = 0
    event Ping:
        n: int
    @on(Ping)
    fn bump(e):
        found = found + e.n
    part watch on dog:
        @on(PlayerNear)
        fn near_dog(e):
            bump(Ping(n=1))
"#;
    assert_eq!(errs(main), vec![d("part-write", 12)]);
    let m = &messages(main, "part-write")[0];
    assert!(m.contains("bump"), "{m}");
}

#[test]
fn part_reads_are_fine() {
    let main = r#"quest Q:
    role dog: Creature
    let found = 0
    event Ping:
        n: int
    stage A:
        objective o "o" @q.o
    part watch on dog:
        @on(PlayerNear)
        fn near_dog(e):
            if stage == Stage::A and found > 0 and not o.done and e.player in participants:
                emit Ping(n=found)
"#;
    assert_eq!(errors_only(main), vec![]);
}

#[test]
fn objective_progress_without_count() {
    let main = r#"quest Q:
    role r: Person
    stage A:
        objective plain "p" @q.p
        objective many "m" @q.m count 3
        @on(PlayerClicked)
        fn click(e):
            many.progress(1)
            plain.progress(1)
            plain.finish()
            let n: int = many.count
            let b: bool = plain.done and plain.failed
            plain.explode()
"#;
    assert_eq!(
        errs(main),
        vec![d("bad-objective-call", 9), d("bad-objective-call", 13)]
    );
}

#[test]
fn scores_count_conjuncts() {
    let main = r#"dialogue d(p: Player, npc: Person):
    node n when npc.alive and p.has_tag("a"):
        npc: "a" @d.a
    node n when npc.alive or p.has_tag("a"):
        npc: "b" @d.b
    node n when once per p:
        npc: "c" @d.c
    node n when not (npc.alive and p.has_tag("a")):
        npc: "d" @d.d
    node n:
        => npc: "e" @d.e when npc.alive and p.has_tag("x") and npc.alive
        => npc: "f" @d.f
        => npc: "g" @d.g when once if npc.alive and npc.alive
"#;
    assert_eq!(errs(main), vec![]);
    let program = resolved(main);
    let dl = dialogue(&program, "d");
    let scores: Vec<u32> = dl.nodes.iter().map(|n| n.score).collect();
    assert_eq!(scores, [2, 1, 1, 1, 0]);
    let StepKind::Group(members) = &dl.nodes[4].steps[0].kind else {
        panic!("not a group: {:?}", dl.nodes[4].steps[0].kind);
    };
    let scores: Vec<u32> = members.iter().map(|m| m.score).collect();
    assert_eq!(scores, [3, 0, 3]);
}

#[test]
fn read_set_is_transitive_and_minimal() {
    let main = r#"let a = 1
let b = 2
let only_handler = 3
let unused = 4

fn helper() -> bool:
    return b > 0

dialogue d(p: Player, npc: Person):
    node n when a > 0 and helper():
        npc: "x" @d.x

@on(PlayerClicked)
fn click(e):
    only_handler = only_handler + 1

quest Q:
    role dog: Creature
    let found = 0
    let ignored = 0
    stage A:
        -> B when found > 2
    stage B:
        @on(PlayerClicked)
        fn other(e):
            ignored = ignored + 1
    part watch on dog:
        let seen = 0
        @on(PlayerNear)
        fn near_dog(e):
            seen = seen + 1
            if stage == Stage::A and seen > found + a:
                pass
"#;
    assert_eq!(errs(main), vec![]);
    let program = resolved(main);
    let (typed, _) = quest_lang::infer(&program, &quest_stub::stub_api());
    let names = |set: &std::collections::BTreeSet<DefId>| -> Vec<String> {
        let mut v: Vec<String> = set.iter().map(|d| program.qualified_name(*d)).collect();
        v.sort();
        v
    };
    assert_eq!(names(&typed.read_sets.dialogue), ["Q::found", "a", "b"]);
    assert_eq!(typed.read_sets.parts.len(), 1);
    // A top-level `let` is home state too: a part reads it through its mirror.
    assert_eq!(
        names(&typed.read_sets.parts[0]),
        ["Q::found", "Q::stage", "a"]
    );
}

#[test]
fn mirror_size_warns_on_list() {
    let main = r#"let names = ["a", "b"]
let n = 1

dialogue d(p: Player, npc: Person):
    node x when n > 0:
        npc: "x" @d.x
    node y when "a" in names:
        npc: "y" @d.y
"#;
    assert_eq!(errs(main), vec![d("mirror-size", 1)]);
    let all = diagnostics(&[("main.qs", main)]);
    let warn = all.iter().find(|d| d.code == "mirror-size").unwrap();
    assert_eq!(warn.severity, Severity::Warning);
    // Ints only: no warning.
    let small = main.replace("\"a\" in names", "n > 1");
    assert_eq!(errs(&small), vec![]);
}

#[test]
fn quest_builtins_and_values_are_typed() {
    let main = r#"event Ring:
    n: int

scene chat(a: Person, b: Person):
    a: "hi" @chat.a

quest Q:
    role r: Person
    stage A:
        @on(PlayerClicked)
        fn click(e):
            join(e.player)
            leave(e.player)
            let t: Timer = after(10, Ring(n=1))
            cancel(t)
            complete([e.player])
            play(chat, a=r, b=r)
            join(1)
            after(10, 3)
            play(chat, a=r)
            complete()

    @on(DialogueEnded)
    fn ended(e):
        if e.dialogue == chat and e.reason == EndReason::Finished:
            fail()
        if e.exit == 3:
            pass
"#;
    assert_eq!(
        errs(main),
        vec![
            d("arg-count", 20),
            d("type-mismatch", 18),
            d("type-mismatch", 19),
            d("type-mismatch", 27),
        ]
    );
}

#[test]
fn detour_arms_index_the_deduplicated_exits() {
    let main = r#"dialogue callee(p: Player, npc: Person) exits done, a, a, b:
    npc: "x" @c.x
    return b

dialogue caller(p: Player, npc: Person):
    detour callee:
        b:
            npc: "y" @c.y
"#;
    assert_eq!(
        errs(main),
        vec![d("duplicate-def", 1), d("duplicate-def", 1)]
    );
    let program = resolved(main);
    let callee = dialogue(&program, "callee");
    assert_eq!(callee.exits, ["done", "a", "b"]);
    let caller = dialogue(&program, "caller");
    let StepKind::Detour(_, arms) = &caller.nodes[0].steps[0].kind else {
        panic!("not a detour");
    };
    assert_eq!(arms[0].0, 2);
}

#[test]
fn stage_jump_ends_the_handler() {
    let main = r#"quest Q:
    role r: Person
    stage A:
        @on(PlayerClicked)
        fn click(e):
            let n = parse_int("3") else err:
                -> B
            print(str(n))
    stage B:
        @on(PlayerClicked)
        fn other(e):
            -> A
"#;
    assert_eq!(errs(main), vec![]);
    let sigs = quest_lang::signatures(&tree(&[("main.qs", main)]), &quest_stub::stub_api());
    assert_eq!(sigs["Q::click"], "fn(PlayerClicked) -> ()");
    assert_eq!(sigs["Q::other"], "fn(PlayerClicked) -> ()");
}
