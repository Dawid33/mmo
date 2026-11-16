//! One program per fault kind a script can cause (spec §11 "Faults", plan D9; plan Task 33): the handler
//! `boom` sets the top-level `counter` from 5 to 6, prints, then faults. Afterwards `counter` is 5 again
//! (the segment was undone), nothing `boom` printed reached the host, and the one abort report names the
//! kind and the faulting line (marked `# fault` in the source).
//!
//! Not here: `Corrupt` (memory that does not parse) and `HostFault` (a host query that fails) — no valid
//! program causes either against the stub; the VM's own suites cover them.

use std::sync::Arc;

use quest_lang::{SourceTree, compile, render};
use quest_stub::{Runner, StubConfig};
use quest_vm::FaultKind;
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::host::{AbortCause, Report};
use quest_vm::wire::WireValue;

/// Fuel per tick: enough for the loops that fill a buffer or the heap to fault within a few ticks.
const FUEL: u64 = 1_000_000;

/// The most ticks a phase (main and its events; then the check) may take.
const TICKS_MAX: u64 = 1_000;

/// `boom`'s body (indented by the caller) for each kind, with the stub's request latency. Each sets
/// `counter = 6` and prints in the segment that faults.
const CASES: &[(FaultKind, u64, &str)] = &[
    (
        FaultKind::IndexOutOfRange,
        0,
        "counter = 6
print(\"never\")
let xs = [1]
let y = xs[5]  # fault",
    ),
    (
        FaultKind::MissingKey,
        0,
        "counter = 6
print(\"never\")
let d = {\"a\": 1}
let y = d[\"b\"]  # fault",
    ),
    (
        FaultKind::IntOverflow,
        0,
        "counter = 6
print(\"never\")
let big = 9223372036854775807
let y = big + counter  # fault",
    ),
    (
        FaultKind::DivByZero,
        0,
        "counter = 6
print(\"never\")
let y = 10 / (counter - 6)  # fault",
    ),
    (
        FaultKind::BadRef,
        0,
        "counter = 6
print(\"never\")
let xs = [1, 2, 3]
let r = &xs[2]
let p = xs.pop()
*r = 7  # fault",
    ),
    (
        FaultKind::HeapFull,
        0,
        "counter = 6
print(\"never\")
let s = \"abcdefgh\"
while true:
    s = s + s  # fault",
    ),
    (
        FaultKind::BadFloatToInt,
        0,
        "counter = 6
print(\"never\")
let y = int(sqrt(-1.0))  # fault",
    ),
    (
        FaultKind::AssertFailed,
        0,
        "counter = 6
print(\"never\")
assert(counter == 5)  # fault",
    ),
    (
        FaultKind::ValueTooLarge,
        0,
        // Two equal lists of 8 · 2^14 ints that share no storage (`source` builds them): comparing them
        // visits every word.
        "counter = 6
print(\"never\")
DOUBLINGS
let same = l14 == m14  # fault",
    ),
    (
        FaultKind::TooManyCommands,
        0,
        "counter = 6
print(\"never\")
while true:
    log(\"spam\")  # fault",
    ),
    (
        FaultKind::TooManyPromises,
        0,
        "counter = 6
print(\"never\")
let i = 0
while i < 100:
    i = i + 1
    let _ = world.set(\"k\", i)  # fault",
    ),
    (
        FaultKind::TooManyParked,
        // `hold` parks four handlers on requests answered 50 ticks later; `boom` would be the fifth.
        50,
        "counter = 6
print(\"never\")
await world.set(\"k\", 1) else ()  # fault",
    ),
    (
        FaultKind::PromiseAlreadyAwaited,
        0,
        "let p = world.set(\"k\", 1)
await p else ()
counter = 6
print(\"never\")
await p else ()  # fault",
    ),
];

/// `l0`/`m0` of 8 ints, then `l1 = [l0, l0]` … `l14`, and the same from `m0`.
fn doublings() -> String {
    let mut out = String::new();
    for x in ["l", "m"] {
        out.push_str(&format!("let {x}0 = [1, 2, 3, 4, 5, 6, 7, 8]\n"));
        for i in 1..=14 {
            let p = i - 1;
            out.push_str(&format!("let {x}{i} = [{x}{p}, {x}{p}]\n"));
        }
    }
    out
}

fn source(kind: FaultKind, body: &str) -> String {
    let body = body.replace("DOUBLINGS\n", &doublings());
    let body: Vec<String> = body.lines().map(|l| format!("    {l}")).collect();
    let mut main = "    emit(PlayerClicked(player=Player(id=1, name=\"ana\")))".to_string();
    let mut extra = String::new();
    if kind == FaultKind::TooManyParked {
        main = (0..4)
            .map(|i| {
                format!("    emit(ItemDelivered(player=Player(id=1, name=\"ana\"), item=Item(kind=\"k\", count={i})))")
            })
            .collect::<Vec<_>>()
            .join("\n");
        extra = "@on(ItemDelivered)
fn hold(e):
    if e.item.count == 3:
        emit(PlayerClicked(player=e.player))
    await world.get(\"held\") else 0
"
        .to_string();
    }
    format!(
        "let counter = 5

@on(PlayerEntered)
fn show(e):
    print(f\"counter {{counter}}\")

{extra}
@on(PlayerClicked)
fn boom(e):
{}

fn main():
{main}
",
        body.join("\n")
    )
}

/// Steps until the instance is quiescent (at least one step).
fn settle(runner: &mut Runner) {
    for _ in 0..TICKS_MAX {
        runner.step(FUEL);
        if runner.quiescent() {
            return;
        }
    }
    panic!("not quiescent after {TICKS_MAX} ticks");
}

#[test]
fn each_fault_kind() {
    let mut bad = Vec::new();
    for (kind, latency, body) in CASES {
        let text = source(*kind, body);
        let line = text
            .lines()
            .position(|l| l.ends_with("# fault"))
            .expect("a marked line") as u32
            + 1;
        let tree = SourceTree {
            files: [("main.qs".to_string(), text.clone())].into(),
        };
        let program = match compile(&tree, &quest_stub::stub_api()) {
            Ok(c) => c.program,
            Err(d) => {
                let d: Vec<String> = d.iter().map(|d| render(d, &tree)).collect();
                bad.push(format!("{kind:?}: does not compile:\n{}", d.join("\n")));
                continue;
            }
        };
        let stub = StubConfig {
            latency: *latency,
            ..StubConfig::default()
        };
        let mut runner = Runner::new(Arc::new(program), MEM_BYTES_DEFAULT, stub).expect("loads");
        runner.vm.start_main();
        settle(&mut runner);
        // Ask `show` for the counter.
        let player = WireValue::Struct {
            name: "Player".into(),
            fields: vec![
                ("id".into(), WireValue::Int(1)),
                ("name".into(), WireValue::Str("ana".into())),
            ],
        };
        runner.host.emitted.push(WireValue::Struct {
            name: "PlayerEntered".into(),
            fields: vec![("player".into(), player)],
        });
        settle(&mut runner);

        let aborts: Vec<&Report> = runner
            .host
            .reports
            .iter()
            .filter(|r| matches!(r, Report::HandlerAborted { .. }))
            .collect();
        let ok = match aborts.as_slice() {
            [
                Report::HandlerAborted {
                    task,
                    cause: AbortCause::Fault { kind: k, stack, .. },
                },
            ] => task == "boom" && k == kind && stack.first().is_some_and(|l| l.line == line),
            _ => false,
        };
        if !ok || runner.host.output != ["counter 5"] {
            bad.push(format!(
                "{kind:?} (line {line}): output {:?}, reports {:?}",
                runner.host.output, runner.host.reports
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
