//! The stub host through the `Host` trait, and the runner with one tiny assembled program.

use std::sync::Arc;

use quest_stub::*;
use quest_vm::asm::ProgramBuilder;
use quest_vm::host::{DropReason, Host, HostFnKind, PromiseId, Report};
use quest_vm::program::Program;
use quest_vm::types::TypeDesc;
use quest_vm::wire::WireValue as W;
use quest_vm::{Op, Vm};

fn s(x: &str) -> W {
    W::Str(x.to_string())
}

fn player(id: i64, name: &str) -> W {
    W::Struct {
        name: "Player".into(),
        fields: vec![("id".into(), W::Int(id)), ("name".into(), s(name))],
    }
}

fn item(kind: &str, count: i64) -> W {
    W::Struct {
        name: "Item".into(),
        fields: vec![("kind".into(), s(kind)), ("count".into(), W::Int(count))],
    }
}

fn variant(name: &str, fields: Vec<(&str, W)>) -> W {
    W::Variant {
        name: name.into(),
        fields: fields.into_iter().map(|(n, v)| (n.into(), v)).collect(),
    }
}

fn host(cfg: StubConfig) -> StubHost {
    StubHost::new(cfg)
}

fn cfg() -> StubConfig {
    StubConfig {
        latency: 0,
        failures: vec![],
        chest: vec![],
        ..StubConfig::default()
    }
}

/// Sends request `f(args)` committed at tick `tick`, then collects the answers due at `now`.
fn ask(h: &mut StubHost, id: u64, tick: u64, f: u16, args: &[W]) {
    h.tick = tick;
    h.request(PromiseId(id), f, args);
}

fn answers(h: &mut StubHost, now: u64) -> Vec<(u64, Result<W, W>)> {
    h.due(now).into_iter().map(|(id, r)| (id.0, r)).collect()
}

fn one(h: &mut StubHost, id: u64, f: u16, args: &[W]) -> Result<W, W> {
    ask(h, id, 0, f, args);
    let mut a = answers(h, 1);
    assert_eq!(a.len(), 1, "one answer");
    a.remove(0).1
}

#[test]
fn stub_api_is_pinned() {
    let api = stub_api();
    assert_eq!(api.version, 6);
    assert_eq!(op_name(F_PLACE).as_deref(), Some("place"));
    for (f, name) in [
        (F_QUEST_JOINED, "quest.joined"),
        (F_QUEST_LEFT, "quest.left"),
        (F_QUEST_JOIN_REFUSED, "quest.join_refused"),
        (F_QUEST_COMPLETE, "quest.complete"),
        (F_QUEST_FAIL, "quest.fail"),
        (F_QUEST_DECLINE, "quest.decline"),
        (F_QUEST_PLAY, "quest.play"),
        (F_QUEST_COMPLETED_BY, "quest.completed_by"),
        (F_QUEST_DIALOGUE_DEPTH, "quest.dialogue_depth"),
        (F_ACT, "act"),
        (F_LOOP, "loop"),
        (F_COMPLETED, "completed"),
        (F_NEAR, "near"),
        (F_PERSON_ALIVE, "Person.alive"),
        (F_PERSON_OCCUPATION, "Person.occupation"),
        (F_PERSON_SITE, "Person.site"),
        (F_PERSON_OWNER, "Person.owner"),
        (F_PERSON_KIND, "Person.kind"),
        (F_PERSON_NAME, "Person.name"),
        (F_PERSON_HAS_TAG, "Person.has_tag"),
        (F_CREATURE_ALIVE, "Creature.alive"),
        (F_CREATURE_OCCUPATION, "Creature.occupation"),
        (F_CREATURE_SITE, "Creature.site"),
        (F_CREATURE_OWNER, "Creature.owner"),
        (F_CREATURE_KIND, "Creature.kind"),
        (F_CREATURE_NAME, "Creature.name"),
        (F_CREATURE_HAS_TAG, "Creature.has_tag"),
        (F_SITE_KIND, "site_kind"),
        (F_PLACE_OPEN_GROUND, "Place.open_ground"),
        (F_PLAYER_SKILL, "Player.skill"),
        (F_PLAYER_INVENTORY_COUNT, "Player.inventory_count"),
        (F_OURS, "ours"),
        (F_PREFAB, "prefab"),
        (F_TAKE, "take"),
        (F_FOLLOW, "follow"),
        (F_TAG, "tag"),
        (F_ATTACH_DIALOGUE, "attach_dialogue"),
        (F_DETACH_DIALOGUE, "detach_dialogue"),
        (F_BARK, "bark"),
        (F_RELATED, "related"),
    ] {
        assert_eq!(op_name(f).as_deref(), Some(name));
    }
    assert_eq!(
        op_name(F_PERSON_DISTANCE_TO).as_deref(),
        Some("Person.distance_to")
    );
    let mut names: Vec<String> = Vec::new();
    names.extend(api.structs.iter().map(|x| format!("struct {}", x.name)));
    names.extend(api.enums.iter().map(|x| format!("enum {}", x.name)));
    names.extend(api.events.iter().map(|x| format!("event {}", x.name)));
    names.extend(api.errors.iter().map(|x| format!("error {}", x.name)));
    for f in &api.fns {
        let owner = f
            .namespace
            .clone()
            .or(f.receiver.clone())
            .unwrap_or_default();
        let dot = if owner.is_empty() { "" } else { "." };
        let mut marks = String::new();
        if f.property {
            marks.push_str(" property");
        }
        if f.fact {
            marks.push_str(" fact");
        }
        names.push(format!("fn {owner}{dot}{}{marks}", f.name));
    }
    names.sort();
    let want = [
        "enum Occupation",
        "enum SiteKind",
        "error Changed",
        "error ChestEmpty",
        "error Lost",
        "error NamespaceGone",
        "error NoRoom",
        "error NotEnough",
        "error NotFound",
        "error OutOfReach",
        "error Timeout",
        "error WrongType",
        "event BlockUsed",
        "event ItemDelivered",
        "event ItemGiven",
        "event Killed",
        "event PlayerClicked",
        "event PlayerDefeated",
        "event PlayerEntered",
        "event PlayerNear",
        "event Setup",
        "fn Creature.alive property fact",
        "fn Creature.has_tag fact",
        "fn Creature.kind property fact",
        "fn Creature.name property",
        "fn Creature.occupation property fact",
        "fn Creature.owner property fact",
        "fn Creature.site property fact",
        "fn Person.alive property fact",
        "fn Person.distance_to",
        "fn Person.has_tag fact",
        "fn Person.kind property fact",
        "fn Person.name property",
        "fn Person.occupation property fact",
        "fn Person.owner property fact",
        "fn Person.site property fact",
        "fn Place.open_ground fact",
        "fn Player.add_tag",
        "fn Player.has_tag fact",
        "fn Player.inventory_count",
        "fn Player.remove_tag",
        "fn Player.skill",
        "fn act fact",
        "fn attach_dialogue",
        "fn bark",
        "fn chest.count",
        "fn chest.put",
        "fn chest.take",
        "fn completed fact",
        "fn detach_dialogue",
        "fn emit",
        "fn follow",
        "fn give",
        "fn log",
        "fn loop fact",
        "fn near fact",
        "fn ours",
        "fn place",
        "fn prefab",
        "fn quest.complete",
        "fn quest.completed_by fact",
        "fn quest.decline",
        "fn quest.dialogue_depth",
        "fn quest.fail",
        "fn quest.join_refused",
        "fn quest.joined",
        "fn quest.left",
        "fn quest.play",
        "fn related fact",
        "fn roll",
        "fn say",
        "fn site_kind fact",
        "fn tag",
        "fn take",
        "fn world.add",
        "fn world.compare_and_set",
        "fn world.delete",
        "fn world.get",
        "fn world.set",
        "struct Creature",
        "struct Entity",
        "struct Item",
        "struct Person",
        "struct Place",
        "struct Player",
        "struct Prefab",
        "struct Site",
        "struct Timer",
    ];
    assert_eq!(names, want);
}

#[test]
fn stub_api_v2_is_well_formed() {
    use quest_vm::host::HostTy;
    use std::collections::BTreeSet;
    let api = stub_api();
    // No duplicate names: types share one space; fns are unique per owner.
    let mut types = BTreeSet::new();
    for name in api
        .structs
        .iter()
        .map(|s| &s.name)
        .chain(api.enums.iter().map(|e| &e.name))
        .chain(api.events.iter().map(|e| &e.name))
    {
        assert!(types.insert(name.clone()), "type {name} twice");
    }
    let mut errors = BTreeSet::new();
    for e in &api.errors {
        assert!(errors.insert(e.name.clone()), "error {} twice", e.name);
    }
    let mut fns = BTreeSet::new();
    for f in &api.fns {
        let key = (f.namespace.clone(), f.receiver.clone(), f.name.clone());
        assert!(fns.insert(key), "fn {} twice", f.name);
    }
    for e in &api.enums {
        let distinct: BTreeSet<&String> = e.variants.iter().collect();
        assert_eq!(
            distinct.len(),
            e.variants.len(),
            "enum {} repeats a variant",
            e.name
        );
    }
    // Every named type exists.
    fn named(t: &HostTy, out: &mut Vec<(bool, String)>) {
        match t {
            HostTy::Struct(n) => out.push((false, n.clone())),
            HostTy::Enum(n) => out.push((true, n.clone())),
            HostTy::List(i) | HostTy::Option(i) => named(i, out),
            _ => {}
        }
    }
    let mut used = Vec::new();
    for s in api.structs.iter().chain(&api.events) {
        s.fields.iter().for_each(|(_, t)| named(t, &mut used));
    }
    for e in &api.errors {
        e.fields.iter().for_each(|(_, t)| named(t, &mut used));
    }
    for f in &api.fns {
        f.params.iter().for_each(|(_, t)| named(t, &mut used));
        named(&f.ret, &mut used);
        for r in &f.raises {
            assert!(errors.contains(r), "{} raises an unknown {r}", f.name);
        }
        if let Some(r) = &f.receiver {
            assert!(
                api.structs.iter().any(|s| &s.name == r),
                "{} on an unknown {r}",
                f.name
            );
        }
        // A property is a receiver query without parameters.
        if f.property {
            assert!(f.receiver.is_some() && f.params.is_empty(), "{}", f.name);
            assert_eq!(f.kind, HostFnKind::Query, "{}", f.name);
        }
        if f.fact {
            assert_eq!(f.kind, HostFnKind::Query, "{}", f.name);
        }
        for (d, _) in &f.defaults {
            assert!(
                f.params.iter().any(|(n, t)| n == d && *t == HostTy::Int),
                "{}: default {d} is no Int parameter",
                f.name
            );
        }
    }
    for (is_enum, n) in used {
        let found = if is_enum {
            api.enums.iter().any(|e| e.name == n)
        } else {
            api.structs.iter().chain(&api.events).any(|s| s.name == n)
        };
        assert!(found, "{n} is named but not declared");
    }
}

#[test]
fn commands_write_output() {
    let mut h = host(cfg());
    h.command(F_SAY, &[player(1, "ana"), s("hello")]);
    h.command(F_LOG, &[s("note")]);
    h.print("plain");
    h.command(F_GIVE, &[player(1, "ana"), item("gold", 4)]);
    assert_eq!(
        h.output,
        ["ana: hello", "log: note", "plain", "give ana 4 gold"]
    );
}

#[test]
fn namespace_semantics() {
    let mut h = host(cfg());
    let get = |h: &mut StubHost, k: &str| one(h, 1, F_WORLD_GET, &[s(k)]);
    assert_eq!(get(&mut h, "a"), Err(variant("NotFound", vec![])));
    assert_eq!(
        one(&mut h, 2, F_WORLD_SET, &[s("a"), W::Int(5)]),
        Ok(W::Unit)
    );
    assert_eq!(get(&mut h, "a"), Ok(W::Int(5)));
    assert_eq!(
        one(&mut h, 3, F_WORLD_ADD, &[s("a"), W::Int(2)]),
        Ok(W::Int(7))
    );
    assert_eq!(
        one(&mut h, 4, F_WORLD_ADD, &[s("fresh"), W::Int(2)]),
        Ok(W::Int(2))
    );
    one(&mut h, 5, F_WORLD_SET, &[s("t"), s("text")]).unwrap();
    assert_eq!(
        one(&mut h, 6, F_WORLD_ADD, &[s("t"), W::Int(1)]),
        Err(variant("WrongType", vec![]))
    );
    // compare_and_set: matching, differing, missing.
    assert_eq!(
        one(&mut h, 7, F_WORLD_CAS, &[s("a"), W::Int(7), W::Int(9)]),
        Ok(W::Unit)
    );
    assert_eq!(get(&mut h, "a"), Ok(W::Int(9)));
    assert_eq!(
        one(&mut h, 8, F_WORLD_CAS, &[s("a"), W::Int(7), W::Int(1)]),
        Err(variant("Changed", vec![("current", W::Int(9))]))
    );
    assert!(matches!(
        one(&mut h, 9, F_WORLD_CAS, &[s("nope"), W::Int(7), W::Int(1)]),
        Err(W::Variant { ref name, .. }) if name == "Changed"
    ));
    // delete.
    assert_eq!(one(&mut h, 10, F_WORLD_DELETE, &[s("a")]), Ok(W::Unit));
    assert_eq!(
        one(&mut h, 11, F_WORLD_DELETE, &[s("a")]),
        Err(variant("NotFound", vec![]))
    );
}

#[test]
fn latency_is_exact() {
    let mut h = host(StubConfig {
        latency: 3,
        ..cfg()
    });
    ask(&mut h, 1, 5, F_WORLD_SET, &[s("k"), W::Int(1)]);
    for now in 5..8 {
        assert!(answers(&mut h, now).is_empty(), "not yet at {now}");
    }
    assert_eq!(answers(&mut h, 8), vec![(1, Ok(W::Unit))]);
    assert!(answers(&mut h, 9).is_empty());
}

#[test]
fn failure_rules() {
    let rule = |op: Option<&str>, nth, kind| FailureRule {
        op: op.map(String::from),
        nth,
        kind,
    };
    // Lost on the 2nd world.get; a chest.take does not count toward it.
    let mut h = host(StubConfig {
        failures: vec![rule(Some("world.get"), 2, FailureKind::Lost)],
        ..cfg()
    });
    one(&mut h, 1, F_WORLD_SET, &[s("k"), W::Int(1)]).unwrap();
    assert_eq!(one(&mut h, 2, F_WORLD_GET, &[s("k")]), Ok(W::Int(1)));
    assert_eq!(
        one(&mut h, 3, F_WORLD_GET, &[s("k")]),
        Err(variant("Lost", vec![]))
    );
    assert_eq!(one(&mut h, 4, F_WORLD_GET, &[s("k")]), Ok(W::Int(1)));

    // Timeout: never answered, and not an answer ever.
    let mut h = host(StubConfig {
        failures: vec![rule(None, 1, FailureKind::Timeout)],
        ..cfg()
    });
    ask(&mut h, 1, 0, F_WORLD_GET, &[s("k")]);
    assert!(answers(&mut h, 1).is_empty());
    assert!(answers(&mut h, 1000).is_empty());

    // NamespaceGone sticks for this and every later world.* call.
    let mut h = host(StubConfig {
        failures: vec![rule(Some("world.set"), 2, FailureKind::NamespaceGone)],
        ..cfg()
    });
    assert_eq!(
        one(&mut h, 1, F_WORLD_SET, &[s("k"), W::Int(1)]),
        Ok(W::Unit)
    );
    let gone = Err(variant("NamespaceGone", vec![]));
    assert_eq!(one(&mut h, 2, F_WORLD_SET, &[s("k"), W::Int(1)]), gone);
    assert_eq!(one(&mut h, 3, F_WORLD_GET, &[s("k")]), gone);

    // Changed carries the current value.
    let mut h = host(StubConfig {
        failures: vec![rule(Some("world.compare_and_set"), 1, FailureKind::Changed)],
        ..cfg()
    });
    one(&mut h, 1, F_WORLD_SET, &[s("k"), W::Int(1)]).unwrap();
    assert_eq!(
        one(&mut h, 2, F_WORLD_CAS, &[s("k"), W::Int(1), W::Int(2)]),
        Err(variant("Changed", vec![("current", W::Int(1))]))
    );
    assert_eq!(one(&mut h, 3, F_WORLD_GET, &[s("k")]), Ok(W::Int(1)));
}

/// Regression (final review, Task 15 minor): a rule injects only errors its op can produce (its `raises`
/// and the base promise errors); for any other op it is skipped and does not count.
#[test]
fn failure_rules_fit_the_op() {
    let rule = |kind| FailureRule {
        op: None,
        nth: 1,
        kind,
    };
    let mut h = host(StubConfig {
        failures: vec![rule(FailureKind::Changed), rule(FailureKind::NamespaceGone)],
        chest: vec![("gold".into(), 3)],
        ..cfg()
    });
    // `chest.take` declares neither: both rules pass it by.
    assert_eq!(
        one(&mut h, 1, F_CHEST_TAKE, &[s("gold"), W::Int(1)]),
        Ok(item("gold", 1))
    );
    // `world.compare_and_set` raises both: the first rule fires on it.
    assert_eq!(
        one(&mut h, 2, F_WORLD_CAS, &[s("k"), W::Int(1), W::Int(2)]),
        Err(variant("Changed", vec![("current", W::Unit)]))
    );
    // A base error fits every promise.
    let mut h = host(StubConfig {
        failures: vec![rule(FailureKind::Lost)],
        chest: vec![("gold".into(), 3)],
        ..cfg()
    });
    assert_eq!(
        one(&mut h, 1, F_CHEST_TAKE, &[s("gold"), W::Int(1)]),
        Err(variant("Lost", vec![]))
    );
}

#[test]
fn chest_take() {
    let mut h = host(StubConfig {
        chest: vec![("gold".into(), 3), ("dust".into(), 0)],
        ..cfg()
    });
    let take =
        |h: &mut StubHost, id, kind: &str, n| one(h, id, F_CHEST_TAKE, &[s(kind), W::Int(n)]);
    assert_eq!(
        take(&mut h, 1, "silver", 1),
        Err(variant("ChestEmpty", vec![]))
    );
    assert_eq!(
        take(&mut h, 2, "dust", 1),
        Err(variant("ChestEmpty", vec![]))
    );
    assert_eq!(
        take(&mut h, 3, "gold", 5),
        Err(variant("NotEnough", vec![("have", W::Int(3))]))
    );
    assert_eq!(take(&mut h, 4, "gold", 2), Ok(item("gold", 2)));
    assert_eq!(h.query(F_CHEST_COUNT, &[s("gold")]), Ok(W::Int(1)));
    h.command(F_CHEST_PUT, &[item("gold", 4)]);
    assert_eq!(h.query(F_CHEST_COUNT, &[s("gold")]), Ok(W::Int(5)));
}

#[test]
fn queries_tags_and_roll() {
    let mut h = host(cfg());
    let p = player(7, "bo");
    assert_eq!(
        h.query(F_HAS_TAG, &[p.clone(), s("vip")]),
        Ok(W::Bool(false))
    );
    h.command(F_ADD_TAG, &[p.clone(), s("vip")]);
    assert_eq!(
        h.query(F_HAS_TAG, &[p.clone(), s("vip")]),
        Ok(W::Bool(true))
    );
    h.command(F_REMOVE_TAG, &[p.clone(), s("vip")]);
    assert_eq!(h.query(F_HAS_TAG, &[p, s("vip")]), Ok(W::Bool(false)));
    assert_eq!(h.query(F_ROLL, &[W::Int(3), W::Int(0)]), Ok(W::Int(0)));
    let a = h.query(F_ROLL, &[W::Int(3), W::Int(6)]);
    assert_eq!(a, h.query(F_ROLL, &[W::Int(3), W::Int(6)]));
    assert!(matches!(a, Ok(W::Int(0..=5))));
}

/// `main`: `log("hi")` through host fn `F_LOG`, then returns.
fn log_program() -> Arc<Program> {
    let mut pb = ProgramBuilder::new(stub_api().version);
    let str_ty = pb.ty(TypeDesc::Str);
    let unit = pb.ty(TypeDesc::Unit);
    let site = pb.host_site(F_LOG, HostFnKind::Command, vec![str_ty], unit);
    let text = pb.string("hi");
    let mut f = pb.func("main", 0);
    let tmp = f.local(0);
    f.mark_ref(tmp);
    f.emit_t(Op::LoadStr, tmp, text);
    f.emit(Op::CallHost, site, tmp, tmp);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    let main = f.fn_ref();
    f.end();
    pb.main(main);
    Arc::new(pb.finish().expect("program"))
}

fn runner() -> Runner {
    Runner::new(log_program(), 1 << 20, cfg()).expect("loads")
}

/// `main`: `world.get("k")`, awaited (its error, if any, kept), then `log("done")`.
fn await_program() -> Arc<Program> {
    let mut pb = ProgramBuilder::new(stub_api().version);
    let str_ty = pb.ty(TypeDesc::Str);
    let unit = pb.ty(TypeDesc::Unit);
    let int = pb.ty(TypeDesc::Int);
    pb.ty(TypeDesc::Enum {
        name: quest_vm::types::ERROR_ENUM_NAME.into(),
        variants: ["Lost", "Timeout", "NamespaceGone"]
            .iter()
            .map(|v| (v.to_string(), vec![]))
            .collect(),
    });
    let prom = pb.ty(TypeDesc::Promise(int));
    let get = pb.host_site(F_WORLD_GET, HostFnKind::Promise, vec![str_ty], prom);
    let log = pb.host_site(F_LOG, HostFnKind::Command, vec![str_ty], unit);
    let (key, done) = (pb.string("k"), pb.string("done"));
    let mut f = pb.func("main", 0);
    let (k, p, val, err) = (f.local(0), f.local(1), f.local(2), f.local(3));
    f.mark_ref(k);
    f.mark_ref(err);
    f.emit_t(Op::LoadStr, k, key);
    f.emit(Op::CallHost, get, k, p);
    f.emit(Op::Await, val, p, int);
    f.emit_t(Op::LoadStr, k, done);
    f.emit(Op::CallHost, log, k, k);
    f.emit(Op::Ret, 0u16, 0u16, 0u16);
    let main = f.fn_ref();
    f.end();
    pb.main(main);
    Arc::new(pb.finish().expect("program"))
}

/// Regression (MVP review, quest-stub): the runner forgot a never-answered request at its deadline even
/// on a tick without fuel, where `Vm::tick` runs no timeout pass: the run looked settled while the VM
/// still owed the `Timeout`.
#[test]
fn zero_fuel_tick_times_out_once() {
    let mut r = Runner::new(
        await_program(),
        1 << 20,
        StubConfig {
            failures: vec![FailureRule {
                op: Some("world.get".into()),
                nth: 1,
                kind: FailureKind::Timeout,
            }],
            ..cfg()
        },
    )
    .expect("loads");
    assert!(r.vm.start_main());
    r.step(10_000);
    assert!(!r.quiescent(), "main waits for its answer");
    while r.now <= quest_vm::consts::PROMISE_TIMEOUT_TICKS + 1 {
        r.step(0);
    }
    assert!(!r.quiescent(), "no tick has timed the request out yet");
    assert!(r.host.output.is_empty());
    r.step(10_000);
    assert_eq!(r.host.output, ["log: done"]);
    assert!(r.quiescent());
}

#[test]
fn runner_runs_main() {
    let mut r = runner();
    let out = r.run_main(10_000, 50);
    assert!(matches!(out, RunOutcome::Finished { .. }), "{out:?}");
    assert_eq!(r.host.output, ["log: hi"]);
    assert!(r.quiescent());
}

#[test]
fn emit_goes_through_the_queue() {
    let mut r = runner();
    r.host.command(
        F_EMIT,
        &[W::Struct {
            name: "PlayerClicked".into(),
            fields: vec![("player".into(), player(1, "ana"))],
        }],
    );
    assert!(!r.quiescent(), "an undelivered emit");
    assert!(r.host.reports.is_empty(), "not delivered within the tick");
    r.step(10_000);
    assert_eq!(
        r.host.reports,
        [Report::EventDropped {
            event: "PlayerClicked".into(),
            reason: DropReason::NoHandler
        }]
    );
    assert!(r.quiescent());
}

#[test]
fn clone_is_a_snapshot() {
    let mut r = runner();
    let saved_host = r.host.clone();
    let (saved_now, cp) = (r.now, r.vm.checkpoint());
    let run = |r: &mut Runner| {
        assert!(r.vm.start_main());
        for _ in 0..5 {
            r.step(10_000);
        }
        r.host.output.clone()
    };
    let first = run(&mut r);
    assert_eq!(first, ["log: hi"]);
    r.host = saved_host;
    r.now = saved_now;
    r.vm.rollback_to(cp).expect("rolls back");
    assert!(r.host.output.is_empty());
    assert_eq!(run(&mut r), first);
    let _: &Vm = &r.vm;
}

/// Review A6 (whole-branch, Task 52): a valid program was checked against the host API's version only, so
/// a host site's kind, parameters and errors were whatever the bytes said — a command called as a query,
/// a fallible fn without its check. The runner refuses a program whose sites do not fit the API.
#[test]
fn a_site_that_does_not_fit_the_api_is_refused() {
    let site_as = |kind: HostFnKind, params: usize, raises: Vec<String>| {
        let mut pb = ProgramBuilder::new(stub_api().version);
        let str_ty = pb.ty(TypeDesc::Str);
        let unit = pb.ty(TypeDesc::Unit);
        pb.ty(TypeDesc::Enum {
            name: quest_vm::types::ERROR_ENUM_NAME.into(),
            variants: vec![("NotFound".to_string(), vec![])],
        });
        pb.fallible_host_site(F_LOG, kind, vec![str_ty; params], unit, raises);
        let mut f = pb.func("main", 0);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        let main = f.fn_ref();
        f.end();
        pb.main(main);
        Arc::new(pb.finish().expect("program"))
    };
    assert!(Runner::new(site_as(HostFnKind::Command, 1, vec![]), 1 << 20, cfg()).is_ok());
    for bad in [
        site_as(HostFnKind::Query, 1, vec![]),
        site_as(HostFnKind::Command, 2, vec![]),
        site_as(HostFnKind::Command, 1, vec!["NotFound".into()]),
    ] {
        assert!(
            matches!(
                Runner::new(bad, 1 << 20, cfg()),
                Err(quest_vm::LoadError::Invalid(_))
            ),
            "a misfitting site loads"
        );
    }
}
