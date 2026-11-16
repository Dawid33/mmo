//! Module loading and name resolution (plan Task 19).

mod common;

use common::{diagnostics, diags};

/// One expected diagnostic: code, file, line.
fn d(code: &str, file: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), file.to_string(), line)
}

/// [`diags`] without the `not-yet` every dialogue and quest gets until Phase 10.
fn errs(files: &[(&str, &str)]) -> Vec<(String, String, u32)> {
    diags(files)
        .into_iter()
        .filter(|(code, _, _)| code != "not-yet")
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
    let n = 1 else err:
        raise er
    return n
fn h() -> int:
    let n = 1 else err:
        match err:
            case NotEnough(have):
                return have
            case ChestEmpty:
                return 0
    return n
"#;
    let all = diagnostics(&[("main.qs", main)]);
    assert_eq!(all.len(), 1, "{all:#?}");
    assert_eq!((all[0].code, all[0].line), ("unresolved", 7));
    assert!(all[0].notes.iter().any(|n| n.contains("`err`")), "{all:#?}");
}

#[test]
fn dialogue_placement() {
    // `objective` outside a stage, `role` outside a quest.
    let main = "objective \"x\"\nrole r: Player\nquest Q:\n    objective \"y\"\n";
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
        objective "x"

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

#[test]
fn not_yet_until_phase_10() {
    assert_eq!(
        diags(&[("main.qs", FARMER)]),
        vec![
            d("not-yet", "main.qs", 13),
            d("not-yet", "main.qs", 28),
            d("not-yet", "main.qs", 36),
        ]
    );
    assert_eq!(
        diags(&[("main.qs", "quest Q:\n    let x = 1\n")]),
        vec![d("not-yet", "main.qs", 1)]
    );
}
