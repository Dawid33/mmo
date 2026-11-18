//! Editing a script keeps its state by name; Reset re-initialises everything (plan Task 31; spec §6.9).
//! Each test compiles program A, runs it, migrates the instance to program B compiled from edited source
//! and runs that.

mod common;

use std::sync::Arc;

use common::compile;
use quest_stub::{RunOutcome, Runner, StubConfig, stub_api};
use quest_vm::host::{FaultKind, Host, PromiseId, QueryError, Report};
use quest_vm::migrate::{MigrateError, MigrationReport, Orphan};
use quest_vm::wire::WireValue;
use quest_vm::{Program, Resolved, Status, Vm, VmConfig};

const FUEL: u64 = 2_000;
const MEM: u32 = quest_vm::consts::MEM_BYTES_DEFAULT;

fn vm_cfg(epoch: u32) -> VmConfig {
    let api = stub_api();
    VmConfig {
        mem_bytes: MEM,
        epoch,
        host_api_version: api.version,
        events: api.events.into_iter().map(|e| e.name).collect(),
        instance: 0,
        roles: vec![],
    }
}

fn program(src: &str) -> Arc<Program> {
    Arc::new(compile(&[("main.qs", src)]))
}

/// A runner for `src` whose initialisers ran.
fn start(src: &str, cfg: StubConfig) -> (Runner, Arc<Program>) {
    let p = program(src);
    let mut r = Runner::new(Arc::clone(&p), MEM, cfg).expect("the program loads");
    settle_inits(&mut r);
    (r, p)
}

/// Steps until the runner is quiescent (at most 10 000 ticks).
fn settle(r: &mut Runner) {
    for _ in 0..10_000 {
        if r.quiescent() {
            return;
        }
        r.step(FUEL);
    }
    panic!("the run did not settle");
}

/// Migrates the runner's instance to `src`.
fn edit(r: Runner, src: &str) -> (Runner, Arc<Program>, MigrationReport) {
    let p = program(src);
    let Runner { vm, host, now } = r;
    let (vm, report) = vm.migrate(Arc::clone(&p), vm_cfg(1)).expect("migrates");
    (Runner { vm, host, now }, p, report)
}

/// The value of global `name` of `p` in `vm`.
fn global(vm: &Vm, p: &Program, name: &str) -> WireValue {
    let g = p
        .globals
        .iter()
        .find(|g| g.name == name)
        .unwrap_or_else(|| panic!("no global {name}"));
    vm.debug_value(g.slot, g.ty).expect("readable")
}

fn player(id: i64, name: &str) -> WireValue {
    WireValue::Struct {
        name: "Player".into(),
        fields: vec![
            ("id".into(), WireValue::Int(id)),
            ("name".into(), WireValue::Str(name.into())),
        ],
    }
}

fn event(name: &str, who: &str) -> WireValue {
    WireValue::Struct {
        name: name.into(),
        fields: vec![("player".into(), player(1, who))],
    }
}

fn click(r: &mut Runner, who: &str) {
    let d = r.vm.deliver("PlayerClicked", &event("PlayerClicked", who));
    assert!(
        matches!(d, quest_vm::Delivery::Accepted | quest_vm::Delivery::Queued),
        "{d:?}"
    );
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

const COUNTER: &str = "let count = 0
let gold = 7

@on(PlayerClicked)
fn c(e):
    count = count + 1
    gold = gold + 1
";

#[test]
fn same_name_compatible_is_carried() {
    let (mut r, _) = start(COUNTER, StubConfig::default());
    click(&mut r, "ana");
    click(&mut r, "bo");
    settle(&mut r);
    let edited =
        "let count = 100\nlet gold = 0\n\n@on(PlayerClicked)\nfn c(e):\n    count = count + 10\n";
    let (mut r, p, report) = edit(r, edited);
    assert_eq!(report.carried, names(&["count", "gold"]));
    assert!(report.initialised.is_empty() && report.orphaned.is_empty());
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "count"), WireValue::Int(2));
    click(&mut r, "ana");
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "count"), WireValue::Int(12));
    assert_eq!(global(&r.vm, &p, "gold"), WireValue::Int(9));

    // Regression (Task 31 review): a value too large to read out (1000 aliases of one list) refuses the
    // edit and leaves the old instance exactly as it was, still running.
    let aliased = "let a: list[int] = []
let b: list[list[int]] = []

fn main():
    let i = 0
    while i < 100:
        a.append(i)
        i = i + 1
    let j = 0
    while j < 1000:
        b.append(a)
        j = j + 1
";
    let (mut r, _) = start(aliased, StubConfig::default());
    r.run_main(FUEL, 10_000);
    assert!(r.host.reports.is_empty(), "{:?}", r.host.reports);
    let (before, saved) = (r.vm.hash64(), r.vm.save());
    let edited = program(&format!("{aliased}\nlet c = 1\n"));
    assert_eq!(
        r.vm.migrate(edited, vm_cfg(1)).err(),
        Some(MigrateError::GlobalUnreadable {
            name: "b".into(),
            kind: FaultKind::ValueTooLarge,
        })
    );
    assert_eq!((r.vm.hash64(), r.vm.save()), (before, saved));
    assert!(r.vm.restart_main());
    assert!(matches!(
        r.run_main(FUEL, 10_000),
        RunOutcome::Finished { .. }
    ));
    assert!(r.host.reports.is_empty(), "{:?}", r.host.reports);
}

#[test]
fn new_variable_is_initialised() {
    let (mut r, _) = start(COUNTER, StubConfig::default());
    click(&mut r, "ana");
    settle(&mut r);
    let edited = format!("{COUNTER}\nlet greeting = f\"hi {{count}}\"\n");
    let (mut r, p, report) = edit(r, &edited);
    assert_eq!(report.carried, names(&["count", "gold"]));
    assert_eq!(report.initialised, names(&["greeting"]));
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "greeting"), WireValue::Str("hi 1".into()));
    assert!(r.host.reports.is_empty(), "{:?}", r.host.reports);
}

#[test]
fn removed_variable_is_orphaned() {
    let (mut r, _) = start(COUNTER, StubConfig::default());
    click(&mut r, "ana");
    settle(&mut r);
    let (r, _, report) = edit(r, "let count = 0\n");
    assert_eq!(report.carried, names(&["count"]));
    assert_eq!(report.orphaned, names(&["gold"]));
    assert_eq!(
        r.vm.orphans(),
        &[Orphan {
            name: "gold".into(),
            ty: "int".into(),
            value: WireValue::Int(8),
        }]
    );
}

#[test]
fn retyped_variable_is_orphaned_and_reinitialised() {
    let (r, _) = start(COUNTER, StubConfig::default());
    let (mut r, p, report) = edit(r, "let count = 0\nlet gold = \"none\"\n");
    assert_eq!(report.orphaned, names(&["gold"]));
    assert_eq!(report.initialised, names(&["gold"]));
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "gold"), WireValue::Str("none".into()));
    assert_eq!(r.vm.orphans().len(), 1);
    assert_eq!(r.vm.orphans()[0].ty, "int");
    assert_eq!(r.vm.orphans()[0].value, WireValue::Int(7));
}

#[test]
fn struct_with_a_new_field_is_incompatible() {
    let a = "struct Ledger:\n    owner: str\n\nlet ledger = Ledger(owner=\"ana\")\n";
    let b = "struct Ledger:\n    owner: str\n    entries: list[int]\n\nlet ledger = Ledger(owner=\"bo\", entries=[])\n";
    let (r, _) = start(a, StubConfig::default());
    let (mut r, p, report) = edit(r, b);
    assert_eq!(report.orphaned, names(&["ledger"]));
    assert_eq!(report.initialised, names(&["ledger"]));
    assert_eq!(
        r.vm.orphans(),
        &[Orphan {
            name: "ledger".into(),
            ty: "Ledger".into(),
            value: WireValue::Struct {
                name: "Ledger".into(),
                fields: vec![("owner".into(), WireValue::Str("ana".into()))],
            },
        }]
    );
    settle(&mut r);
    let Some((_, fields)) = global(&r.vm, &p, "ledger").into_fields() else {
        panic!("not a struct");
    };
    assert_eq!(
        fields[0],
        ("owner".to_string(), WireValue::Str("bo".into()))
    );
}

#[test]
fn orphan_is_adopted_when_the_name_returns() {
    let (mut r, _) = start(COUNTER, StubConfig::default());
    click(&mut r, "ana");
    settle(&mut r);
    let (r, _, report) = edit(r, "let count = 0\n");
    assert_eq!(report.orphaned, names(&["gold"]));
    let (mut r, p, report) = edit(r, "let count = 0\nlet gold = 0\n");
    assert_eq!(report.adopted, names(&["gold"]));
    assert!(report.initialised.is_empty() && report.orphaned.is_empty());
    assert!(r.vm.orphans().is_empty());
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "gold"), WireValue::Int(8));
}

#[test]
fn queued_events_reach_the_new_handlers() {
    let a = "@on(PlayerClicked)\nfn c(e):\n    print(f\"old {e.player.name}\")\n";
    let b = "@on(PlayerClicked)\nfn c(e):\n    print(f\"new {e.player.name}\")\n";
    let (mut r, _) = start(a, StubConfig::default());
    click(&mut r, "ana");
    click(&mut r, "bo");
    let (mut r, _, report) = edit(r, b);
    assert_eq!(report.events_dropped, 0);
    settle(&mut r);
    assert_eq!(r.host.output, names(&["new ana", "new bo"]));
}

#[test]
fn events_without_a_handler_are_dropped() {
    let a = "@on(PlayerClicked)\nfn c(e):\n    print(\"clicked\")\n\n@on(PlayerEntered)\nfn n(e):\n    print(\"entered\")\n";
    let b = "@on(PlayerClicked)\nfn c(e):\n    print(\"clicked\")\n";
    let (mut r, _) = start(a, StubConfig::default());
    click(&mut r, "ana");
    let d =
        r.vm.deliver("PlayerEntered", &event("PlayerEntered", "ana"));
    assert_eq!(d, quest_vm::Delivery::Accepted);
    let (mut r, _, report) = edit(r, b);
    assert_eq!(report.events_dropped, 1);
    settle(&mut r);
    assert_eq!(r.host.output, names(&["clicked"]));
}

#[test]
fn parked_handlers_are_cancelled_but_writes_stay() {
    let a = "@on(PlayerClicked)\nfn c(e):\n    await world.set(\"k\", 1) else ()\n    print(\"after\")\n";
    let b = "fn main():\n    let v: int = await world.get(\"k\") else 0\n    print(f\"k={v}\")\n";
    let cfg = StubConfig {
        latency: 5,
        ..StubConfig::default()
    };
    let (mut r, _) = start(a, cfg);
    click(&mut r, "ana");
    // The handler runs, commits its request and parks.
    r.step(FUEL);
    assert!(r.vm.is_idle() && r.host.has_pending());
    let (mut r, _, report) = edit(r, b);
    assert_eq!(report.handlers_cancelled, 1);
    // The stub applies the write when it answers; the cancelled handler's answer is unknown now.
    let mut answered = 0;
    for _ in 0..10 {
        let now = r.now;
        for (id, answer) in r.host.due(now) {
            assert_eq!(r.vm.resolve(id, answer), Resolved::Unknown);
            answered += 1;
        }
        r.now += 1;
    }
    assert_eq!(answered, 1);
    r.run_main(FUEL, 10_000);
    assert_eq!(r.host.output, names(&["k=1"]));
    assert!(r.host.reports.is_empty(), "{:?}", r.host.reports);
}

#[test]
fn edit_mid_segment_aborts_it() {
    let a = "let n = 0

@on(PlayerClicked)
fn h(e):
    n = 5
    let i = 0
    while i < 1000:
        i = i + 1
";
    let b = format!("{a}\nlet m = 1\n");
    let (mut r, p) = start(a, StubConfig::default());
    click(&mut r, "ana");
    r.step(200);
    assert_eq!(r.vm.status(), Status::Busy);
    assert_eq!(
        global(&r.vm, &p, "n"),
        WireValue::Int(5),
        "the handler is mid-segment"
    );
    let (mut r, p, report) = edit(r, &b);
    assert_eq!(report.carried, names(&["n"]));
    settle(&mut r);
    assert_eq!(global(&r.vm, &p, "n"), WireValue::Int(0));
}

const BUSY: &str = "let gold = 7

@on(PlayerClicked)
fn c(e):
    await world.set(\"k\", 1) else ()
";

#[test]
fn reset_equals_fresh() {
    let cfg = StubConfig {
        latency: 50,
        ..StubConfig::default()
    };
    let (r, _) = start(BUSY, cfg);
    let b = "@on(PlayerClicked)\nfn c(e):\n    await world.set(\"k\", 1) else ()\n";
    let (mut r, p, _) = edit(r, b);
    assert_eq!(r.vm.orphans().len(), 1);
    settle_inits(&mut r);
    click(&mut r, "ana");
    r.step(FUEL);
    click(&mut r, "bo");
    click(&mut r, "cy");
    assert_eq!(r.vm.status(), Status::Busy);
    r.vm.reset().expect("reset");
    let fresh = Vm::new(p, vm_cfg(2)).expect("fresh");
    assert!(r.vm.orphans().is_empty());
    assert_eq!(r.vm.mem_bytes(), fresh.mem_bytes());
    assert_eq!(r.vm.hash64(), fresh.hash64());
    assert_eq!(r.vm.log_len(), 0);
}

/// Steps until the instance is not busy (its initialisers ran).
fn settle_inits(r: &mut Runner) {
    for _ in 0..10_000 {
        if r.vm.status() != Status::Busy {
            return;
        }
        r.step(FUEL);
    }
    panic!("the initialisers did not finish");
}

#[test]
fn reset_revives_a_failed_instance() {
    let src = "let per = 10 / chest.count(\"gold\")\n";
    let (mut r, p) = start(src, StubConfig::default());
    assert_eq!(r.vm.status(), Status::Failed);
    assert!(matches!(r.host.reports[..], [Report::InitFailed { .. }]));
    r.vm.reset().expect("reset");
    r.host = quest_stub::StubHost::new(StubConfig {
        chest: vec![("gold".into(), 5)],
        ..StubConfig::default()
    });
    settle(&mut r);
    assert_eq!(r.vm.status(), Status::Idle);
    assert_eq!(global(&r.vm, &p, "per"), WireValue::Int(2));
}

/// Records the ids of the requests it is given.
#[derive(Default)]
struct Ids(Vec<u64>);

impl Host for Ids {
    fn query(&mut self, _: u16, _: &[WireValue]) -> Result<WireValue, QueryError> {
        Err(QueryError::Failed("no queries".into()))
    }
    fn check(&mut self, _: u16, _: &[WireValue]) -> Result<(), QueryError> {
        Err(QueryError::Failed("no checks".into()))
    }
    fn command(&mut self, _: u16, _: &[WireValue]) {}
    fn request(&mut self, id: PromiseId, _: u16, _: &[WireValue]) {
        self.0.push(id.0);
    }
    fn print(&mut self, _: &str) {}
    fn report(&mut self, r: Report) {
        panic!("{r:?}");
    }
    fn route(&mut self, event: &str, _: &WireValue) {
        panic!("routed {event}");
    }
}

#[test]
fn promise_ids_after_reset_do_not_repeat() {
    let p = program(BUSY);
    let mut vm = Vm::new(p, vm_cfg(1)).expect("vm");
    let mut ids = Ids::default();
    vm.tick(0, FUEL, &mut ids);
    for t in 1..3 {
        vm.deliver("PlayerClicked", &event("PlayerClicked", "ana"));
        vm.tick(t, FUEL, &mut ids);
    }
    vm.reset().expect("reset");
    vm.tick(3, FUEL, &mut ids);
    for t in 4..6 {
        vm.deliver("PlayerClicked", &event("PlayerClicked", "ana"));
        vm.tick(t, FUEL, &mut ids);
    }
    assert_eq!(
        ids.0,
        vec![1 << 32 | 1, 1 << 32 | 2, 2 << 32 | 1, 2 << 32 | 2]
    );
}

#[test]
fn no_leaks_after_migrate_and_cancel() {
    let a = "let names = [\"a\", \"b\"]
let tally = {\"x\": 1}

@on(PlayerClicked)
fn c(e):
    let held = [e.player.name, f\"{e.player.id}\"]
    await world.set(\"k\", 1) else ()
    print(held[0])
";
    let b = "let names = [\"z\"]
let tally = {\"y\": 2}

@on(PlayerClicked)
fn c(e):
    print(e.player.name)
";
    let cfg = StubConfig {
        latency: 50,
        ..StubConfig::default()
    };
    let (mut r, _) = start(a, cfg);
    click(&mut r, "ana");
    r.step(FUEL);
    click(&mut r, "bo");
    r.step(FUEL);
    let (mut r, pb, report) = edit(r, b);
    assert_eq!(report.handlers_cancelled, 2);
    settle_inits(&mut r);
    // An instance of B holding the same globals, from an instance of A that never parked anything.
    let (fresh, _) = start(a, StubConfig::default());
    let (mut fresh, _, report) = edit(fresh, b);
    assert_eq!(report.handlers_cancelled, 0);
    settle_inits(&mut fresh);
    assert_eq!(global(&r.vm, &pb, "names"), global(&fresh.vm, &pb, "names"));
    assert_eq!(global(&r.vm, &pb, "tally"), global(&fresh.vm, &pb, "tally"));
    assert!(r.vm.is_idle() && fresh.vm.is_idle());
    assert_eq!(r.vm.heap_used_words(), fresh.vm.heap_used_words());
}
