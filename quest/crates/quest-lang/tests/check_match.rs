//! `match`: pattern typing, exhaustiveness, impossible and unreachable arms (plan Task 23; spec §5.6, §5.7).

mod common;

use common::{diagnostics, diags};
use quest_lang::{Diagnostic, Severity};

/// One expected diagnostic: code, file, line.
fn d(code: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), "main.qs".to_string(), line)
}

fn main_diags(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
}

/// The diagnostics with this code.
fn with_code(main: &str, code: &str) -> Vec<Diagnostic> {
    diagnostics(&[("main.qs", main)])
        .into_iter()
        .filter(|d| d.code == code)
        .collect()
}

/// The message of the one `non-exhaustive` of `main`.
fn missing(main: &str) -> String {
    let found = with_code(main, "non-exhaustive");
    assert_eq!(found.len(), 1, "{:#?}", diagnostics(&[("main.qs", main)]));
    found[0].message.clone()
}

const STAGE: &str = "enum Stage:
    Asked
    Accepted(tick: int)
    Done
";

#[test]
fn spec_5_6_is_exhaustive() {
    let arms = [
        "        case Stage::Asked:\n            print(\"Will you help?\")\n",
        "        case Stage::Accepted(tick) if tick > 100:\n            print(\"Still waiting on you.\")\n",
        "        case Stage::Accepted(_):\n            pass\n",
        "        case Stage::Done:\n            print(\"Thanks again.\")\n",
    ];
    let main = format!(
        "{STAGE}\nfn talk(stage: Stage):\n    match stage:\n{}",
        arms.concat()
    );
    assert_eq!(main_diags(&main), vec![]);
    let main = format!(
        "{STAGE}\nfn talk(stage: Stage):\n    match stage:\n{}",
        arms[..3].concat()
    );
    assert_eq!(main_diags(&main), vec![d("non-exhaustive", 7)]);
    assert!(
        missing(&main).contains("`Stage::Done`"),
        "{}",
        missing(&main)
    );
}

#[test]
fn guards_do_not_cover() {
    let main = format!(
        "{STAGE}
fn talk(stage: Stage):
    match stage:
        case Stage::Asked:
            pass
        case Stage::Accepted(t) if t > 100:
            pass
        case Stage::Done:
            pass
"
    );
    assert_eq!(main_diags(&main), vec![d("non-exhaustive", 7)]);
    assert!(
        missing(&main).contains("`Stage::Accepted(_)`"),
        "{}",
        missing(&main)
    );
}

const TAKE_GOLD: &str = "fn take_gold(c: int, n: int) -> int:
    if c == 0:
        raise ChestEmpty
    if c < 0:
        raise QuestClosed
    if c < n:
        raise NotEnough(have=c)
    return n
";

#[test]
fn fallible_match() {
    // §5.7: exhaustive against the inferred set.
    let main = format!(
        "{TAKE_GOLD}
fn give(c: int):
    match take_gold(c, 20):
        case Ok(gold):
            print(str(gold))
        case Err(NotEnough(have)):
            print(f\"Only {{have}} left.\")
        case Err(ChestEmpty | QuestClosed):
            pass
"
    );
    assert_eq!(main_diags(&main), vec![]);

    // Without the `ChestEmpty` arm.
    let main = format!(
        "{TAKE_GOLD}
fn give(c: int):
    match take_gold(c, 20):
        case Ok(gold):
            print(str(gold))
        case Err(NotEnough(have)):
            print(f\"Only {{have}} left.\")
        case Err(QuestClosed):
            pass
"
    );
    assert_eq!(main_diags(&main), vec![d("non-exhaustive", 11)]);
    assert!(
        missing(&main).contains("`Err(ChestEmpty)`"),
        "{}",
        missing(&main)
    );

    // `QuestClosed` cannot occur here.
    let main = "fn take(c: int) -> int:
    if c == 0:
        raise ChestEmpty
    return c

fn give(c: int):
    match take(c):
        case Ok(gold):
            pass
        case Err(ChestEmpty | QuestClosed):
            pass
";
    assert_eq!(main_diags(main), vec![d("impossible-arm", 10)]);

    // A caught error (`else err:`) is matched against its set by bare names.
    let main = format!(
        "{TAKE_GOLD}
fn give(c: int) -> int:
    let g = take_gold(c, 20) else err:
        match err:
            case NotEnough(have):
                print(str(have))
            case ChestEmpty | QuestClosed:
                pass
        return 0
    let h = take_gold(c, 20) else err:
        match err:
            case NotEnough(have):
                pass
            case ChestEmty:
                pass
        return 0
    return g + h
"
    );
    assert_eq!(
        main_diags(&main),
        vec![d("impossible-arm", 22), d("non-exhaustive", 19)]
    );

    // A pattern is a use of a generic host error field (`Changed(current: Infer)`): it fixes its type.
    let main = "fn take(c: int) -> int raises Changed:
    return c

fn give(c: int):
    match take(c):
        case Ok(_):
            pass
        case Err(Changed(x)):
            let s: str = x
            let n: int = x
";
    assert_eq!(main_diags(main), vec![d("type-mismatch", 10)]);
    let main = "fn take(c: int) -> int raises Changed:
    return c

fn give(c: int):
    match take(c):
        case Ok(_):
            pass
        case Err(Changed(true)):
            pass
";
    assert_eq!(main_diags(main), vec![d("non-exhaustive", 5)]);
    assert!(
        missing(main).contains("`Err(Changed(false))`"),
        "{}",
        missing(main)
    );

    // Against anything else, a name that names nothing is `unresolved`, with its suggestion.
    let main = format!(
        "{STAGE}
fn talk(stage: Stage):
    match stage:
        case Stage::Asked | Stage::Done | Stage::Accepted(_):
            pass
        case Stag:
            pass
"
    );
    let found = with_code(&main, "unresolved");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].line, 10);
    assert_eq!(found[0].notes, vec!["did you mean `Stage`?".to_string()]);
}

#[test]
fn bool_option_nested() {
    let main = format!(
        "{STAGE}
fn f(b: bool, o: Option[Stage]):
    match b:
        case true:
            pass
        case false:
            pass
    match o:
        case Some(Stage::Accepted(t)):
            print(str(t))
        case Some(_):
            pass
        case None:
            pass
"
    );
    assert_eq!(main_diags(&main), vec![]);
    let main = format!(
        "{STAGE}
fn f(b: bool, o: Option[Stage]):
    match b:
        case true:
            pass
    match o:
        case Some(Stage::Asked | Stage::Done):
            pass
        case None:
            pass
"
    );
    assert_eq!(
        main_diags(&main),
        vec![d("non-exhaustive", 7), d("non-exhaustive", 10)]
    );
    let found = with_code(&main, "non-exhaustive");
    assert!(found[0].message.contains("`false`"), "{found:#?}");
    assert!(
        found[1].message.contains("`Some(Stage::Accepted(_))`"),
        "{found:#?}"
    );
}

#[test]
fn literals_need_a_wildcard() {
    let main = "fn f(n: int, s: str):
    match n:
        case 1:
            pass
        case 2 | 3:
            pass
    match s:
        case \"a\":
            pass
";
    assert_eq!(
        main_diags(main),
        vec![d("non-exhaustive", 2), d("non-exhaustive", 7)]
    );
    let main = "fn f(n: int, s: str):
    match n:
        case 1:
            pass
        case _:
            pass
    match s:
        case \"a\":
            pass
        case other:
            print(other)
";
    assert_eq!(main_diags(main), vec![]);
}

#[test]
fn alternatives() {
    let main = "enum Shape:
    A(x: int)
    B(x: int)
    C
    S(x: str)

fn f(s: Shape) -> int:
    match s:
        case Shape::A(x) | Shape::B(x):
            return x
        case Shape::C | Shape::S(_):
            return 0
";
    assert_eq!(main_diags(main), vec![]);
    let main = "enum Shape:
    A(x: int)
    B(x: int)
    C
    S(x: str)

fn f(s: Shape):
    match s:
        case Shape::A(x) | Shape::B(y):
            pass
        case Shape::A(x) | Shape::S(x):
            pass
        case _:
            pass
";
    assert_eq!(
        main_diags(main),
        vec![d("type-mismatch", 9), d("type-mismatch", 11)]
    );
}

#[test]
fn struct_patterns() {
    let main = "struct Ledger:
    owner: str
    gold: int

fn f(l: Ledger) -> int:
    match l:
        case Ledger(owner=\"ana\", gold=g):
            return g
        case Ledger(_, g):
            return g + 1
";
    assert_eq!(main_diags(main), vec![]);
    let main = "struct Ledger:
    owner: str
    gold: int

fn f(l: Ledger):
    match l:
        case Ledger(owner=\"ana\"):
            pass
";
    assert_eq!(main_diags(main), vec![d("non-exhaustive", 6)]);
    assert!(
        missing(main).contains("`Ledger(_, _)`"),
        "{}",
        missing(main)
    );
}

#[test]
fn binding_types() {
    let main = format!(
        "{STAGE}
fn f(stage: Stage):
    match stage:
        case Stage::Accepted(tick):
            let n: int = tick
            let s: str = tick
        case _:
            pass
"
    );
    assert_eq!(main_diags(&main), vec![d("type-mismatch", 10)]);
}

#[test]
fn unreachable_arm_warning() {
    let main = format!(
        "{STAGE}
fn f(stage: Stage):
    match stage:
        case _:
            pass
        case Stage::Done:
            pass
"
    );
    let found = diagnostics(&[("main.qs", main.as_str())]);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "unreachable-arm");
    assert_eq!(found[0].severity, Severity::Warning);
    assert_eq!(found[0].line, 10);
}
