//! Code generation for handlers, host calls, `await`, `await all`, detached promises and `emit`, run end to
//! end through the stub host (plan Task 29; spec §7, §8.2, §9).

mod common;

use common::{Out, compile, run, run_with};
use quest_stub::{FailureKind, FailureRule, Runner, StubConfig};
use quest_vm::host::{AbortCause, DropReason, FaultKind, Report};

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Fuel per tick of the default runs.
const FUEL: u64 = 2_000;

/// Runs `files`: no report, nothing leaked; what it printed.
fn printed(files: &[(&str, &str)]) -> Vec<String> {
    printed_with(files, StubConfig::default())
}

fn printed_with(files: &[(&str, &str)], cfg: StubConfig) -> Vec<String> {
    let out = run_with(files, cfg, FUEL);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    assert_eq!(out.leaked_words, 0, "leaked");
    out.lines
}

/// Runs `main.qs` and records the tick at which each line was printed.
fn timed(main: &str, cfg: StubConfig) -> Vec<(u64, String)> {
    let program = compile(&[("main.qs", main)]);
    let mut runner = Runner::new(
        std::sync::Arc::new(program),
        quest_vm::consts::MEM_BYTES_DEFAULT,
        cfg,
    )
    .expect("the program loads");
    runner.vm.start_main();
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let now = runner.now;
        runner.step(FUEL);
        let seen = out.len();
        out.extend(runner.host.output[seen..].iter().map(|l| (now, l.clone())));
        if runner.quiescent() {
            break;
        }
    }
    assert!(runner.host.reports.is_empty(), "{:?}", runner.host.reports);
    out
}

const SPEC_7: &str = r#"@on(PlayerClicked)
fn greet(e):
    say(e.player, "Well met.")

@on(PlayerEntered)
fn warn(e):
    if e.player.has_tag("outlaw"):
        say(e.player, "Begone.")
"#;

#[test]
fn spec_7_handlers() {
    let main = format!(
        r#"{SPEC_7}
fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
    await world.set("k", 1) else ()
    ana.add_tag("outlaw")
    emit(PlayerEntered(player=ana))
"#
    );
    assert_eq!(
        printed(&[("main.qs", &main)]),
        lines(&["ana: Well met.", "ana: Begone."])
    );
}

#[test]
fn handlers_are_plain_functions() {
    let main = format!(
        r#"{SPEC_7}
fn main():
    let ana = Player(id=1, name="ana")
    greet(PlayerClicked(player=ana))
    warn(PlayerEntered(player=ana))
    ana.add_tag("outlaw")
    await world.set("k", 1) else ()
    warn(PlayerEntered(player=ana))
"#
    );
    assert_eq!(
        printed(&[("main.qs", &main)]),
        lines(&["ana: Well met.", "ana: Begone."])
    );
}

#[test]
fn handler_order() {
    let main = r#"mod b
mod a

@on(PlayerClicked)
fn root(e):
    print("root")

fn main():
    emit(PlayerClicked(player=Player(id=1, name="ana")))
"#;
    let a = "@on(PlayerClicked)\nfn h(e):\n    print(\"a\")\n";
    let b = "@on(PlayerClicked)\nfn h(e):\n    print(\"b\")\n";
    assert_eq!(
        printed(&[("main.qs", main), ("b.qs", b), ("a.qs", a)]),
        lines(&["root", "a", "b"])
    );
}

#[test]
fn spec_8_2_ask() {
    let main = r#"@on(PlayerClicked)
fn ask(e):
    let slain: int = await world.get("dragons_slain")?
    let p = world.get(f"renown/{e.player.id}")
    say(e.player, "Let me check the ledgers...")
    let r: int = await p else 0
    say(e.player, f"{slain} dragons slain, renown {r}")

fn main():
    await world.set("dragons_slain", 4) else ()
    emit(PlayerClicked(player=Player(id=1, name="ana")))
"#;
    let cfg = StubConfig {
        latency: 2,
        ..StubConfig::default()
    };
    assert_eq!(
        printed_with(&[("main.qs", main)], cfg),
        lines(&[
            "ana: Let me check the ledgers...",
            "ana: 4 dragons slain, renown 0"
        ])
    );
}

#[test]
fn spec_8_2_claim_race() {
    let main = r#"@on(ItemDelivered)
fn claim(e):
    match await world.compare_and_set("wyrm_quest", "open", str(e.player.id)):
        case Ok(_):
            give(e.player, await chest.take("gold", 20)?)
        case Err(Changed(winner)):
            say(e.player, "Another claimed it first.")
        case Err(_):
            say(e.player, "The ledgers are silent.")

fn main():
    await world.set("wyrm_quest", "open") else ()
    let wyrm = Item(kind="wyrm", count=1)
    emit(ItemDelivered(player=Player(id=1, name="ana"), item=wyrm))
    emit(ItemDelivered(player=Player(id=2, name="bo"), item=wyrm))
"#;
    let cfg = StubConfig {
        chest: vec![("gold".to_string(), 50)],
        ..StubConfig::default()
    };
    assert_eq!(
        printed_with(&[("main.qs", main)], cfg),
        lines(&["bo: Another claimed it first.", "give ana 20 gold"])
    );
}

#[test]
fn state_changes_across_await_are_visible() {
    let main = r#"let count = 0

@on(PlayerClicked)
fn first(e):
    print(f"before {count}")
    await world.set("k", 1) else ()
    print(f"after {count}")

@on(PlayerEntered)
fn second(e):
    count = 5

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
"#;
    let cfg = StubConfig {
        latency: 2,
        ..StubConfig::default()
    };
    assert_eq!(
        printed_with(&[("main.qs", main)], cfg),
        lines(&["before 0", "after 5"])
    );
}

#[test]
fn await_in_a_helper_function() {
    let main = r#"fn fetch(key: str) -> int:
    let n: int = await world.get(key) else -1
    return n

@on(PlayerClicked)
fn h(e):
    let a = fetch("a")
    let missing = fetch("missing")
    say(e.player, f"{a} {missing}")

fn main():
    await world.set("a", 7) else ()
    emit(PlayerClicked(player=Player(id=1, name="ana")))
    print(str(fetch("a")))
"#;
    assert_eq!(printed(&[("main.qs", main)]), lines(&["7", "ana: 7 -1"]));
}

#[test]
fn await_all() {
    let main = r#"fn main():
    await world.set("a", 1) else ()
    let a = world.add("a", 2)
    let b = world.add("b", 10)
    let xs = await all([a, b]) else []
    print(str(xs))
    match await all([world.delete("a"), world.delete("nope")]):
        case Ok(_):
            print("deleted both")
        case Err(NotFound):
            print("not found")
        case Err(_):
            print("other")
    let n: int = await world.get("a") else -1
    print(str(n))
"#;
    assert_eq!(
        printed(&[("main.qs", main)]),
        lines(&["[3, 10]", "not found", "-1"])
    );
}

/// More detached promises than the VM has promise records: each record goes when its promise resolves.
#[test]
fn detached_promise() {
    let main = r#"fn main():
    for i in range(20):
        let _ = world.set("k", 42 + i)
        await world.set("round", i) else ()
    let v: int = await world.get("k") else -1
    print(str(v))
"#;
    assert_eq!(printed(&[("main.qs", main)]), lines(&["61"]));
}

#[test]
fn fault_after_await() {
    let main = r#"let g = 0

@on(PlayerClicked)
fn h(e):
    g = 1
    print("before")
    await world.set("k", 1) else ()
    g = 2
    print("after")
    let xs = [1]
    print(str(xs[5]))

@on(PlayerEntered)
fn show(e):
    print(f"g={g}")

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    await world.set("m", 1) else ()
    await world.set("m", 2) else ()
    emit(PlayerEntered(player=ana))
"#;
    let out = run(&[("main.qs", main)]);
    assert_eq!(out.lines, lines(&["before", "g=1"]));
    assert_eq!(out.leaked_words, 0, "leaked");
    match out.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, .. },
                ..
            },
        ] => assert_eq!(*kind, FaultKind::IndexOutOfRange),
        other => panic!("{other:?}"),
    }
}

#[test]
fn queue_overflow_from_scripts() {
    let main = r#"@on(PlayerClicked)
fn burst(e):
    for i in range(6):
        emit(ItemDelivered(player=e.player, item=Item(kind="k", count=i)))

@on(PlayerEntered)
fn busy(e):
    let i = 0
    while i < 8000:
        i = i + 1
    print("done looping")

@on(ItemDelivered)
fn got(e):
    print(f"got {e.item.count}")

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
"#;
    let out: Out = run(&[("main.qs", main)]);
    assert_eq!(
        out.lines,
        lines(&["done looping", "got 0", "got 1", "got 2", "got 3"])
    );
    let queued = out
        .reports
        .iter()
        .filter(|r| matches!(r, Report::EventQueued { event } if event == "ItemDelivered"))
        .count();
    let dropped = out
        .reports
        .iter()
        .filter(|r| {
            matches!(r, Report::EventDropped { event, reason: DropReason::QueueFull }
                if event == "ItemDelivered")
        })
        .count();
    assert_eq!((queued, dropped), (4, 2), "{:?}", out.reports);
    assert_eq!(out.reports.len(), 6, "{:?}", out.reports);
    assert_eq!(out.leaked_words, 0, "leaked");
}

#[test]
fn timeout_path() {
    let main = r#"@on(PlayerClicked)
fn h(e):
    print("asking")
    match await world.add("k", 1):
        case Ok(n):
            print(f"ok {n}")
        case Err(Timeout):
            print("timed out")
        case Err(_):
            print("other")

fn main():
    emit(PlayerClicked(player=Player(id=1, name="ana")))
"#;
    let cfg = StubConfig {
        failures: vec![FailureRule {
            op: Some("world.add".to_string()),
            nth: 1,
            kind: FailureKind::Timeout,
        }],
        ..StubConfig::default()
    };
    let out = timed(main, cfg);
    let at = |line: &str| out.iter().find(|(_, l)| l == line).map(|(t, _)| *t);
    let issued = at("asking").expect("asked");
    assert_eq!(at("timed out"), Some(issued + 200), "{out:?}");
    assert_eq!(out.len(), 2, "{out:?}");
}

#[test]
fn no_leaks_across_awaits() {
    let main = r#"@on(PlayerClicked)
fn resumed(e):
    let s = f"hello {e.player.name}"
    let xs = [s, "x"]
    let n = await world.add("n", 1) else 0
    print(f"{s} {xs.len()} {n}")

@on(PlayerEntered)
fn aborted(e):
    let s = f"bye {e.player.name}"
    let xs = [[1, 2], [3]]
    let n = await world.add("n", 1) else 0
    print(s)
    let bad = xs[9]

@on(ItemDelivered)
fn timed_out(e):
    let s = e.item.kind
    let xs = [s, s]
    match await world.delete("t"):
        case Ok(_):
            print("deleted")
        case Err(Timeout):
            print(f"timeout {xs.len()}")
        case Err(_):
            print("other")

fn main():
    let ana = Player(id=1, name="ana")
    emit(PlayerClicked(player=ana))
    emit(PlayerEntered(player=ana))
    emit(ItemDelivered(player=ana, item=Item(kind="gold", count=3)))
"#;
    let cfg = StubConfig {
        failures: vec![FailureRule {
            op: Some("world.delete".to_string()),
            nth: 1,
            kind: FailureKind::Timeout,
        }],
        ..StubConfig::default()
    };
    let out = run_with(&[("main.qs", main)], cfg, FUEL);
    assert_eq!(out.lines, lines(&["hello ana 2 1", "timeout 2"]));
    match out.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, .. },
                ..
            },
        ] => assert_eq!(*kind, FaultKind::IndexOutOfRange),
        other => panic!("{other:?}"),
    }
    assert_eq!(out.leaked_words, 0, "leaked");
}
