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
    assert_eq!(api.version, 1);
    let mut names: Vec<String> = Vec::new();
    names.extend(api.structs.iter().map(|x| format!("struct {}", x.name)));
    names.extend(api.events.iter().map(|x| format!("event {x}")));
    names.extend(api.errors.iter().map(|x| format!("error {}", x.name)));
    for f in &api.fns {
        let owner = f
            .namespace
            .clone()
            .or(f.receiver.clone())
            .unwrap_or_default();
        let dot = if owner.is_empty() { "" } else { "." };
        names.push(format!("fn {owner}{dot}{}", f.name));
    }
    names.sort();
    let want = [
        "error Changed",
        "error ChestEmpty",
        "error Lost",
        "error NamespaceGone",
        "error NotEnough",
        "error NotFound",
        "error Timeout",
        "error WrongType",
        "event ItemDelivered",
        "event PlayerClicked",
        "event PlayerEntered",
        "fn Player.add_tag",
        "fn Player.has_tag",
        "fn Player.remove_tag",
        "fn chest.count",
        "fn chest.put",
        "fn chest.take",
        "fn emit",
        "fn give",
        "fn log",
        "fn roll",
        "fn say",
        "fn world.add",
        "fn world.compare_and_set",
        "fn world.delete",
        "fn world.get",
        "fn world.set",
        "struct Item",
        "struct Player",
    ];
    assert_eq!(names, want);
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
        failures: vec![rule(Some("world.set"), 2, FailureKind::Changed)],
        ..cfg()
    });
    one(&mut h, 1, F_WORLD_SET, &[s("k"), W::Int(1)]).unwrap();
    assert_eq!(
        one(&mut h, 2, F_WORLD_SET, &[s("k"), W::Int(2)]),
        Err(variant("Changed", vec![("current", W::Int(1))]))
    );
    assert_eq!(one(&mut h, 3, F_WORLD_GET, &[s("k")]), Ok(W::Int(1)));
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
