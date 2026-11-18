//! Code generation for quests and parts (plan Task 44; spec §5.12): stages, objectives, transitions,
//! timers, participants, roles, parts and `QuestInfo`, run through the stub runner with roles given.

mod common;

use std::sync::Arc;

use common::compile;
use quest_stub::{Runner, StubConfig, stub_api};
use quest_vm::host::{DropReason, Report};
use quest_vm::program::Anchor;
use quest_vm::vm::{Delivery, VmConfig};
use quest_vm::wire::WireValue;
use quest_vm::{Program, Status, Vm};

const FUEL: u64 = 2_000;
const MEM: u32 = quest_vm::consts::MEM_BYTES_DEFAULT;

fn person(id: i64) -> WireValue {
    WireValue::Struct {
        name: "Person".into(),
        fields: vec![("id".into(), WireValue::Int(id))],
    }
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

/// The roles every test quest takes: `farmer` (required) and, when given, `miller` (optional).
fn roles() -> Vec<(String, WireValue)> {
    vec![("farmer".into(), person(1))]
}

fn program(src: &str) -> Arc<Program> {
    Arc::new(compile(&[("main.qs", src)]))
}

/// Steps until the runner is quiescent (at most 10 000 ticks).
fn settle(r: &mut Runner) {
    for _ in 0..10_000 {
        if r.quiescent() {
            return;
        }
        r.step(FUEL);
        let failed = r
            .host
            .reports
            .iter()
            .find(|x| matches!(x, Report::HandlerAborted { .. } | Report::InitFailed { .. }));
        assert!(failed.is_none(), "{failed:?}");
    }
    panic!("the run did not settle");
}

/// Instance `instance` of `p`, its initialisers and the quest's start run.
fn start_as(p: &Arc<Program>, instance: u16) -> Runner {
    let mut r = Runner::for_instance(Arc::clone(p), MEM, StubConfig::default(), instance, roles())
        .expect("the program loads");
    settle(&mut r);
    r
}

fn start(src: &str) -> (Runner, Arc<Program>) {
    let p = program(src);
    (start_as(&p, 0), p)
}

/// A script event `Q::<name>` with one int field `n`.
fn ev(r: &mut Runner, name: &str, n: i64) {
    let payload = WireValue::Struct {
        name: name.into(),
        fields: vec![("n".into(), WireValue::Int(n))],
    };
    assert_eq!(
        r.vm.deliver(&format!("Q::{name}"), &payload),
        Delivery::Accepted,
        "{name}"
    );
    settle(r);
}

/// A host event carrying a player.
fn host_ev(r: &mut Runner, name: &str, p: WireValue) -> Delivery {
    let payload = WireValue::Struct {
        name: name.into(),
        fields: vec![("player".into(), p)],
    };
    let d = r.vm.deliver(name, &payload);
    settle(r);
    d
}

fn global(vm: &Vm, p: &Program, name: &str) -> WireValue {
    let g = p
        .globals
        .iter()
        .find(|g| g.name == name)
        .unwrap_or_else(|| panic!("no global {name}"));
    vm.debug_value(g.slot, g.ty).expect("readable")
}

fn stage(vm: &Vm, p: &Program) -> String {
    match &global(vm, p, "Q::stage") {
        WireValue::Variant { name, .. } => name.clone(),
        other => panic!("stage is {other:?}"),
    }
}

/// An objective's (state, count).
fn objective(vm: &Vm, p: &Program, name: &str) -> (i64, i64) {
    let v = global(vm, p, &format!("Q::{name}"));
    let field = |f: &str| match &v {
        WireValue::Struct { fields, .. } => match fields.iter().find(|(n, _)| n == f) {
            Some((_, WireValue::Int(i))) => *i,
            _ => panic!("{name}.{f} in {v:?}"),
        },
        _ => panic!("{name} is {v:?}"),
    };
    (field("state"), field("count"))
}

fn lines(r: &Runner) -> Vec<&str> {
    r.host.output.iter().map(String::as_str).collect()
}

fn count(r: &Runner, line: &str) -> usize {
    r.host.output.iter().filter(|l| *l == line).count()
}

/// The quest most tests use: `Open` → (Go) → `Search`, whose two objectives lead to `Returning`.
const LOST_DOG: &str = r#"quest Q:
    role farmer: Person
    optional role miller: Person

    event Go:
        n: int
    event Find:
        n: int
    event Feed:
        n: int
    event Both:
        n: int
    event Back:
        n: int
    event Ping:
        n: int

    @on(Ping)
    fn any_ping(e):
        print("any ping")

    stage Open:
        @on(Entered)
        fn opened(e):
            print("entered Open")
        @on(Go)
        fn go(e):
            -> Search
        @on(Ping)
        fn open_ping(e):
            print("open ping")

    stage Search:
        objective find_dog "Find the dog" @q.find
        objective feed_dog "Feed the dog" @q.feed
        -> Returning when find_dog.done and feed_dog.done
        @on(Entered)
        fn searching(e):
            print("entered Search")
        @on(Find)
        fn found(e):
            find_dog.finish()
        @on(Feed)
        fn fed(e):
            feed_dog.finish()
        @on(Both)
        fn both(e):
            find_dog.finish()
            feed_dog.finish()
            find_dog.finish()
        @on(Back)
        fn back(e):
            -> Search
        @on(Ping)
        fn search_ping(e):
            print("search ping")

    stage Returning:
        @on(Entered)
        fn returning(e):
            print("entered Returning")
"#;

#[test]
fn starts_in_first_stage_with_entered() {
    let (r, p) = start(LOST_DOG);
    assert_eq!(r.vm.status(), Status::Idle);
    assert_eq!(lines(&r), ["entered Open"]);
    assert_eq!(stage(&r.vm, &p), "Open");
    let q = p.quest.as_ref().expect("a quest");
    assert_eq!(q.name, "Q");
    assert_eq!(q.stages, ["Open", "Search", "Returning"]);
    assert_eq!(p.globals[usize::from(q.stage_global)].name, "Q::stage");
    assert_eq!(
        p.globals[usize::from(q.participants_global)].name,
        "Q::participants"
    );
    let roles: Vec<(&str, &str, bool)> = q
        .roles
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                p.globals[usize::from(r.global)].name.as_str(),
                r.optional,
            )
        })
        .collect();
    assert_eq!(
        roles,
        [
            ("farmer", "Q::role.farmer", false),
            ("miller", "Q::role.miller", true)
        ]
    );
    assert_eq!(p.fns[usize::from(q.start_fn)].name, "$start::Q");
    // The transition reads both objectives: the dialogue read set.
    let mut read: Vec<&str> = q
        .read_set
        .iter()
        .map(|g| p.globals[usize::from(*g)].name.as_str())
        .collect();
    assert!(q.read_set.windows(2).all(|w| w[0] < w[1]));
    read.sort_unstable();
    assert_eq!(read, ["Q::Search.feed_dog", "Q::Search.find_dog"]);
    assert_eq!(q.home_role, 0);
    assert!(q.repeat.is_none() && q.parts.is_empty() && q.requires.is_empty());
}

#[test]
fn handler_jump_enters_stage() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Go", 0);
    assert_eq!(stage(&r.vm, &p), "Search");
    assert_eq!(lines(&r), ["entered Open", "entered Search"]);
}

#[test]
fn transition_when_both_objectives_done() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Go", 0);
    ev(&mut r, "Find", 0);
    assert_eq!(objective(&r.vm, &p, "Search.find_dog"), (1, 0));
    assert_eq!(stage(&r.vm, &p), "Search", "one objective done: stays");
    ev(&mut r, "Feed", 0);
    assert_eq!(stage(&r.vm, &p), "Returning");
    assert_eq!(count(&r, "entered Returning"), 1);
}

#[test]
fn two_finishes_one_entry() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Go", 0);
    // Both objectives (one twice) in one segment: the transition is taken once.
    ev(&mut r, "Both", 0);
    assert_eq!(stage(&r.vm, &p), "Returning");
    // Finishing again in another stage's time changes nothing and enters nothing.
    ev(&mut r, "Find", 0);
    ev(&mut r, "Both", 0);
    assert_eq!(count(&r, "entered Returning"), 1);
    assert_eq!(count(&r, "entered Search"), 1);
}

#[test]
fn reentering_reopens_objectives() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Go", 0);
    ev(&mut r, "Find", 0);
    assert_eq!(objective(&r.vm, &p, "Search.find_dog"), (1, 0));
    ev(&mut r, "Back", 0);
    assert_eq!(count(&r, "entered Search"), 2);
    assert_eq!(objective(&r.vm, &p, "Search.find_dog"), (0, 0));
    ev(&mut r, "Feed", 0);
    assert_eq!(stage(&r.vm, &p), "Search", "find_dog was re-opened");
}

const TIMERS: &str = r#"quest Q:
    role farmer: Person

    event Arm:
        n: int
    event Leave:
        n: int
    event Tick:
        n: int

    @on(Arm)
    fn arm_body(e):
        if e.n == 2:
            let _t = after(10, Tick(n=2))

    @on(Tick)
    fn ticked(e):
        print(f"tick {e.n}")

    stage A:
        @on(Arm)
        fn arm(e):
            if e.n == 1:
                let _t = after(10, Tick(n=1))
        @on(Leave)
        fn leave_a(e):
            -> B

    stage B:
        @on(Arm)
        fn arm_b(e):
            if e.n == 3:
                let t = after(5, Tick(n=3))
                cancel(t)
"#;

/// Steps `n` ticks.
fn run_ticks(r: &mut Runner, n: u64) {
    for _ in 0..n {
        r.step(FUEL);
    }
}

#[test]
fn stage_timer_cancelled_on_change() {
    let (mut r, _) = start(TIMERS);
    ev(&mut r, "Arm", 1);
    assert_eq!(r.vm.timers().len(), 1);
    ev(&mut r, "Leave", 0);
    assert!(r.vm.timers().is_empty(), "the stage change cancelled it");
    run_ticks(&mut r, 20);
    assert!(lines(&r).is_empty(), "{:?}", lines(&r));
    // `cancel(t)` cancels a timer the handler set.
    ev(&mut r, "Arm", 3);
    assert!(r.vm.timers().is_empty());
    run_ticks(&mut r, 20);
    assert!(lines(&r).is_empty(), "{:?}", lines(&r));
}

#[test]
fn quest_body_timer_survives_stage_change() {
    let (mut r, _) = start(TIMERS);
    ev(&mut r, "Arm", 2);
    ev(&mut r, "Leave", 0);
    assert_eq!(r.vm.timers().len(), 1);
    run_ticks(&mut r, 20);
    assert_eq!(lines(&r), ["tick 2"]);
}

#[test]
fn progress_counts() {
    let src = r#"quest Q:
    role farmer: Person

    event Got:
        n: int
    event Undo:
        n: int

    stage Gather:
        objective wood "Gather wood" @q.wood count 3
        -> Done when wood.done
        @on(Got)
        fn got(e):
            wood.progress(e.n)
            print(f"{wood.count} {wood.done}")
        @on(Undo)
        fn undo(e):
            wood.reset()

    stage Done:
        @on(Got)
        fn late(e):
            print("late")
"#;
    let (mut r, p) = start(src);
    ev(&mut r, "Got", 2);
    assert_eq!(objective(&r.vm, &p, "Gather.wood"), (0, 2));
    ev(&mut r, "Undo", 0);
    assert_eq!(objective(&r.vm, &p, "Gather.wood"), (0, 0));
    ev(&mut r, "Got", 1);
    ev(&mut r, "Got", 2);
    assert_eq!(objective(&r.vm, &p, "Gather.wood"), (1, 3));
    assert_eq!(stage(&r.vm, &p), "Done");
    // The transition is taken at the end of `got`'s segment: `Done` is current when the event's next
    // handler, `late`, is looked at, so it runs.
    assert_eq!(lines(&r), ["2 false", "1 false", "3 true", "late"]);
    // Progress on a done objective changes nothing.
    ev(&mut r, "Got", 5);
    assert_eq!(objective(&r.vm, &p, "Gather.wood"), (1, 3));
}

#[test]
fn stage_handlers_only_in_their_stage() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Ping", 0);
    ev(&mut r, "Go", 0);
    ev(&mut r, "Ping", 0);
    ev(&mut r, "Find", 0);
    ev(&mut r, "Feed", 0);
    ev(&mut r, "Ping", 0);
    assert_eq!(
        lines(&r),
        [
            "entered Open",
            "any ping",
            "open ping",
            "entered Search",
            "any ping",
            "search ping",
            "entered Returning",
            "any ping",
        ]
    );
    // Each row carries its stage.
    let staged = |name: &str| {
        p.handlers
            .iter()
            .find(|h| p.fns[usize::from(h.fn_id)].name == name)
            .map(|h| (h.owner, h.stage))
            .unwrap_or_else(|| panic!("no handler {name}"))
    };
    assert_eq!(staged("Q::any_ping"), (0, None));
    assert_eq!(staged("Q::open_ping"), (0, Some(0)));
    assert_eq!(staged("Q::search_ping"), (0, Some(1)));
}

#[test]
fn join_exclusive_refused() {
    let src = r#"repeatable quest Q(exclusive=true):
    role farmer: Person

    stage Open:
        @on(PlayerClicked)
        fn clicked(e):
            join(e.player)
        @on(PlayerDefeated)
        fn defeated(e):
            leave(e.player)
        @on(PlayerNear)
        fn near(e):
            complete()
"#;
    let (mut r, p) = start(src);
    host_ev(&mut r, "PlayerClicked", player(1, "Ann"));
    host_ev(&mut r, "PlayerClicked", player(2, "Bob"));
    host_ev(&mut r, "PlayerClicked", player(1, "Ann"));
    assert_eq!(
        global(&r.vm, &p, "Q::participants"),
        WireValue::List(vec![player(1, "Ann")])
    );
    host_ev(&mut r, "PlayerDefeated", player(2, "Bob"));
    host_ev(&mut r, "PlayerNear", player(1, "Ann"));
    host_ev(&mut r, "PlayerDefeated", player(1, "Ann"));
    // Nobody in it now: Bob may join.
    host_ev(&mut r, "PlayerClicked", player(2, "Bob"));
    assert_eq!(
        lines(&r),
        [
            "quest.joined Ann",
            "quest.join_refused Bob",
            "quest.complete Ann",
            "quest.left Ann",
            "quest.joined Bob",
        ]
    );
    assert_eq!(
        p.quest
            .as_ref()
            .and_then(|q| q.repeat)
            .map(|o| (o.exclusive, o.once_per_player)),
        Some((true, false))
    );
}

#[test]
fn role_lost_default_fails_required_only() {
    let (mut r, p) = start(LOST_DOG);
    let lost = |role: &str| WireValue::Struct {
        name: "RoleLost".into(),
        fields: vec![(
            "role".into(),
            WireValue::Variant {
                name: role.into(),
                fields: vec![],
            },
        )],
    };
    assert_eq!(
        r.vm.deliver("Q::RoleLost", &lost("miller")),
        Delivery::Accepted
    );
    settle(&mut r);
    assert_eq!(lines(&r), ["entered Open"], "an optional role: nothing");
    assert_eq!(
        r.vm.deliver("Q::RoleLost", &lost("farmer")),
        Delivery::Accepted
    );
    settle(&mut r);
    assert_eq!(lines(&r), ["entered Open", "quest.fail"]);
    assert!(p.fns.iter().any(|f| f.name == "$role_lost::Q"));

    // A quest with a `RoleLost` handler of its own gets no default one.
    let own = r#"quest Q:
    role farmer: Person

    @on(RoleLost)
    fn lost(e):
        print("lost")

    stage Open:
        @on(PlayerClicked)
        fn clicked(e):
            print("clicked")
"#;
    let (mut r, p) = start(own);
    assert!(!p.fns.iter().any(|f| f.name == "$role_lost::Q"));
    assert_eq!(
        r.vm.deliver("Q::RoleLost", &lost("farmer")),
        Delivery::Accepted
    );
    settle(&mut r);
    assert_eq!(lines(&r), ["lost"]);
}

#[test]
fn edit_removing_current_stage_falls_back_to_first() {
    let (mut r, p) = start(LOST_DOG);
    ev(&mut r, "Go", 0);
    assert_eq!(stage(&r.vm, &p), "Search");
    // `Search` is gone from the edited quest.
    let edited = LOST_DOG.replace("stage Search:", "stage Hunt:");
    let edited = edited.replace("-> Search", "-> Hunt");
    let p2 = program(&edited);
    let api = stub_api();
    let cfg = VmConfig {
        mem_bytes: MEM,
        epoch: 1,
        host_api_version: api.version,
        events: api.events.into_iter().map(|e| e.name).collect(),
        instance: 0,
        roles: roles(),
    };
    let Runner { vm, host, now } = r;
    let (vm, report) = vm.migrate(Arc::clone(&p2), cfg).expect("migrates");
    assert!(
        report.initialised.iter().any(|g| g == "Q::stage"),
        "{report:?}"
    );
    // The renamed stage's objectives start afresh.
    assert!(report.initialised.iter().any(|g| g == "Q::Hunt.find_dog"));
    let mut r = Runner { vm, host, now };
    settle(&mut r);
    assert_eq!(stage(&r.vm, &p2), "Open");
    // The quest's start does not run again: no second `Entered`.
    assert_eq!(lines(&r), ["entered Open", "entered Search"]);
    ev(&mut r, "Go", 0);
    assert_eq!(stage(&r.vm, &p2), "Hunt");
}

#[test]
fn part_handlers_compiled_with_owner() {
    let src = r#"quest Q:
    role farmer: Person

    let found = 0

    event Spotted:
        n: int

    stage Open:
        @on(Spotted)
        fn spotted(e):
            found = found + e.n

    part watch on farmer:
        let seen = 0
        @on(PlayerNear)
        fn near(e):
            seen = seen + 1
            if stage == Stage::Open and e.player not in participants:
                print(f"near {seen}")
"#;
    let p = program(src);
    let row = p
        .handlers
        .iter()
        .find(|h| p.fns[usize::from(h.fn_id)].name == "Q::watch::near")
        .expect("the part's handler");
    assert_eq!((row.owner, row.stage), (1, None));
    let q = p.quest.as_ref().expect("a quest");
    assert_eq!(q.parts.len(), 1);
    let part = &q.parts[0];
    assert_eq!(
        (part.name.as_str(), part.anchor, part.role),
        ("watch", Anchor::On, 0)
    );
    let mut read: Vec<&str> = part
        .read_set
        .iter()
        .map(|g| p.globals[usize::from(*g)].name.as_str())
        .collect();
    read.sort_unstable();
    assert_eq!(read, ["Q::participants", "Q::stage"]);
    assert!(p.globals.iter().any(|g| g.name == "Q::watch::seen"));

    // The home has no handler for it; the part's instance runs it.
    let mut home = start_as(&p, 0);
    assert_eq!(
        host_ev(&mut home, "PlayerNear", player(1, "Ann")),
        Delivery::Dropped(DropReason::NoHandler)
    );
    let mut part = start_as(&p, 1);
    host_ev(&mut part, "PlayerNear", player(1, "Ann"));
    host_ev(&mut part, "PlayerNear", player(1, "Ann"));
    assert_eq!(lines(&part), ["near 1", "near 2"]);
    assert!(part.host.reports.is_empty(), "{:?}", part.host.reports);
}

#[test]
fn play_and_dialogue_values() {
    let src = r#"quest Q:
    role farmer: Person

    scene argue(a: Person, b: Person):
        a: "You!" @q.you
        b: "Me?" @q.me

    dialogue shout(npc: Person):
        "Hey!" @q.hey

    stage Open:
        dialogue plea(p: Player, npc: Person) for farmer:
            npc: "Find my dog." @q.plea
        @on(PlayerClicked)
        fn clicked(e):
            play(argue, b=Person(id=2), a=farmer)
            let d = shout
            print(str(d == shout and d != argue))
"#;
    let (mut r, p) = start(src);
    host_ev(&mut r, "PlayerClicked", player(1, "Ann"));
    assert_eq!(lines(&r), ["quest.play Q::argue [1, 2]", "true"]);
    let names: Vec<(&str, Option<u16>)> = p
        .dialogues
        .dialogues
        .iter()
        .map(|d| (d.name.as_str(), d.stage))
        .collect();
    assert_eq!(
        names,
        [("Q::argue", None), ("Q::plea", Some(0)), ("Q::shout", None)]
    );
}

#[test]
fn quest_without_stage_is_refused() {
    let src = "quest Q:\n    role farmer: Person\n    @on(PlayerClicked)\n    fn c(e):\n        print(\"x\")\n";
    let t = common::tree(&[("main.qs", src)]);
    let codes: Vec<String> = match quest_lang::compile(&t, &stub_api()) {
        Ok(_) => panic!("compiled"),
        Err(diags) => diags.into_iter().map(|d| d.code.to_string()).collect(),
    };
    assert_eq!(codes, ["no-stage"]);
}
