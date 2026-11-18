//! The host description of Phase 10 (plans Task 37 and 37b; spec §9.1): host enums, properties, facts, named
//! arguments with defaults, fallible commands (checked when issued) and fallible queries.

mod common;

use common::{compile, diags, run, run_with};
use quest_vm::host::{AbortCause, FaultKind, QueryError, Report};
use quest_vm::types::TypeDesc;

/// One expected diagnostic: code, file, line.
fn d(code: &str, line: u32) -> (String, String, u32) {
    (code.to_string(), "main.qs".to_string(), line)
}

fn main_diags(main: &str) -> Vec<(String, String, u32)> {
    diags(&[("main.qs", main)])
}

#[test]
fn host_enum_values_and_match() {
    let main = r#"fn describe(o: Occupation) -> str:
    match o:
        case Occupation::Smith:
            return "smith"
        case _:
            return "other"

fn main():
    print(describe(Occupation::Smith))
    print(describe(Occupation::None))
    let k = SiteKind::Mine
    print(str(k))
"#;
    assert_eq!(main_diags(main), vec![]);
    assert_eq!(
        run(&[("main.qs", main)]).lines,
        ["smith", "other", "SiteKind::Mine"]
    );
    // A host enum's value crosses the wire as `SiteKind::Mine`.
    let program = compile(&[("main.qs", main)]);
    let site_kind = program.types.0.iter().find_map(|t| match t {
        TypeDesc::Enum { name, variants } if name == "SiteKind" => Some(variants.clone()),
        _ => None,
    });
    let variants = site_kind.expect("SiteKind is a VM enum");
    assert_eq!(variants[2], ("SiteKind::Mine".to_string(), vec![]));

    // Exhaustive against the host's variants.
    let partial = r#"fn f(k: SiteKind) -> int:
    match k:
        case SiteKind::Village | SiteKind::Town:
            return 1
        case SiteKind::Mine:
            return 2
"#;
    assert_eq!(main_diags(partial), vec![d("non-exhaustive", 2)]);
    assert_eq!(
        main_diags("fn f() -> SiteKind:\n    return SiteKind::Castle\n"),
        vec![d("unresolved", 2)]
    );
}

#[test]
fn property_read() {
    let main = r#"fn fits(p: Person, c: Creature) -> bool:
    return p.alive and c.owner == p and p.occupation == Occupation::Farmer and p.name != c.name

fn main():
    pass
"#;
    assert_eq!(main_diags(main), vec![]);
    let sigs = quest_lang::signatures(&common::tree(&[("main.qs", main)]), &quest_stub::stub_api());
    assert_eq!(sigs["fits"], "fn(Person, Creature) -> bool");
    compile(&[("main.qs", main)]);
    // A property is read, never written.
    assert_eq!(
        main_diags("fn f(p: Person):\n    p.alive = false\n"),
        vec![d("not-assignable", 2)]
    );
}

#[test]
fn property_called_is_error() {
    assert_eq!(
        main_diags("fn f(p: Person) -> bool:\n    return p.alive()\n"),
        vec![d("not-callable", 2)]
    );
}

#[test]
fn method_without_call_is_error() {
    assert_eq!(
        main_diags("fn f(p: Person) -> bool:\n    let t = p.has_tag\n    return true\n"),
        vec![d("missing-call", 2)]
    );
}

#[test]
fn named_arguments_and_defaults() {
    let main = r#"fn f(a: Person, b: Creature, at: Place) -> bool:
    return near(a, b, blocks=8) and near(a, b) and near(a, b, regions=1, blocks=2) and at.open_ground(blocks=20)

fn main():
    pass
"#;
    assert_eq!(main_diags(main), vec![]);
    compile(&[("main.qs", main)]);
    // A parameter without a default must be given; a name must be a parameter's, once.
    assert_eq!(
        main_diags("fn f(at: Place) -> bool:\n    return at.open_ground()\n"),
        vec![d("arg-count", 2)]
    );
    assert_eq!(
        main_diags("fn f(a: Person) -> bool:\n    return near(a, a, miles=1)\n"),
        vec![d("arg-count", 2)]
    );
    assert_eq!(
        main_diags("fn f(a: Person) -> bool:\n    return near(a, a, blocks=1, blocks=2)\n"),
        vec![d("arg-count", 2)]
    );
}

#[test]
fn fallible_command_handled() {
    let main = r#"fn build(at: Place):
    place(prefab("nest"), at) else:
        print("no room")
        return
    print("placed")

fn main():
    pass
"#;
    assert_eq!(main_diags(main), vec![]);
    compile(&[("main.qs", main)]);
    assert_eq!(
        main_diags("fn build(at: Place):\n    place(prefab(\"nest\"), at)\n"),
        vec![d("unhandled-fallible", 2)]
    );
}

#[test]
fn quest_namespace_is_not_a_script_name() {
    let found = main_diags("fn f():\n    quest.fail()\n");
    assert_eq!(found, vec![d("unresolved", 2)]);
}

/// A host that records every query, check and command and answers the few queries the tests make;
/// `distance_to` raises `NoRoom`, which it does not declare, and `refuse` fails every check with `NoRoom`.
#[derive(Default)]
struct Recorder {
    calls: Vec<String>,
    output: Vec<String>,
    reports: Vec<quest_vm::host::Report>,
    refuse: bool,
}

fn no_room() -> W {
    W::Variant {
        name: "NoRoom".into(),
        fields: vec![],
    }
}

impl quest_vm::host::Host for Recorder {
    fn query(&mut self, f: u16, args: &[W]) -> Result<W, QueryError> {
        let hf = &quest_stub::stub_api().fns[usize::from(f)];
        self.calls.push(format!("query {} {args:?}", hf.name));
        Ok(match hf.name.as_str() {
            "distance_to" => return Err(QueryError::Raised(no_room())),
            "near" | "alive" => W::Bool(true),
            "occupation" => W::Variant {
                name: "Occupation::Smith".into(),
                fields: vec![],
            },
            "prefab" => W::Struct {
                name: "Prefab".into(),
                fields: vec![("name".into(), args[0].clone())],
            },
            _ => return Err(QueryError::Failed(format!("no answer for {}", hf.name))),
        })
    }

    fn check(&mut self, f: u16, args: &[W]) -> Result<(), QueryError> {
        let hf = &quest_stub::stub_api().fns[usize::from(f)];
        self.calls.push(format!("check {} {args:?}", hf.name));
        if self.refuse {
            return Err(QueryError::Raised(no_room()));
        }
        Ok(())
    }

    fn command(&mut self, f: u16, args: &[W]) {
        let hf = &quest_stub::stub_api().fns[usize::from(f)];
        self.calls.push(format!("command {} {args:?}", hf.name));
    }

    fn request(&mut self, _: quest_vm::host::PromiseId, _: u16, _: &[W]) {}

    fn print(&mut self, text: &str) {
        self.output.push(text.to_string());
    }

    fn report(&mut self, report: quest_vm::host::Report) {
        self.reports.push(report);
    }

    fn route(&mut self, event: &str, _: &W) {
        self.calls.push(format!("route {event}"));
    }
}

use quest_vm::wire::WireValue as W;

#[test]
fn host_calls_reach_the_host() {
    let main = r#"fn main():
    let p = Person(id=3)
    if near(p, p, blocks=8) and p.alive:
        match p.occupation:
            case Occupation::Smith:
                print("smith")
            case _:
                print("other")
    place(prefab("nest"), Place(id=9)) else:
        print("no room")
        return
    print("placed")
"#;
    let mut host = Recorder::default();
    record(main, &mut host, 2_000, 100);
    assert_eq!(host.reports, vec![]);
    assert_eq!(host.output, ["smith", "placed"]);
    let person = W::Struct {
        name: "Person".into(),
        fields: vec![("id".into(), W::Int(3))],
    };
    let place = W::Struct {
        name: "Place".into(),
        fields: vec![("id".into(), W::Int(9))],
    };
    let prefab = W::Struct {
        name: "Prefab".into(),
        fields: vec![("name".into(), W::Str("nest".into()))],
    };
    // `regions` takes its default; the property passes its receiver.
    let near = [person.clone(), person.clone(), W::Int(0), W::Int(8)];
    assert_eq!(
        host.calls,
        [
            format!("query near {near:?}"),
            format!("query alive {:?}", std::slice::from_ref(&person)),
            format!("query occupation {:?}", [person]),
            format!("query prefab {:?}", [W::Str("nest".into())]),
            format!("check place {:?}", [prefab.clone(), place.clone()]),
            format!("command place {:?}", [prefab, place]),
        ]
    );
}

/// Starts `main` of `main` against `host` and runs at most `ticks` ticks of `fuel`, stopping once idle.
fn record(main: &str, host: &mut Recorder, fuel: u64, ticks: u64) -> quest_vm::Vm {
    let api = quest_stub::stub_api();
    let mut vm = quest_vm::Vm::new(
        std::sync::Arc::new(compile(&[("main.qs", main)])),
        quest_vm::vm::VmConfig {
            mem_bytes: quest_vm::consts::MEM_BYTES_DEFAULT,
            epoch: 1,
            host_api_version: api.version,
            events: api.events.into_iter().map(|e| e.name).collect(),
            instance: 0,
            roles: vec![],
        },
    )
    .expect("the program loads");
    vm.start_main();
    for now in 0..ticks {
        vm.tick(now, fuel, host);
        if vm.is_idle() {
            break;
        }
    }
    vm
}

/// The stub's people: 1 and 2 are 5 blocks apart; nobody else is in reach.
fn people() -> quest_stub::StubConfig {
    quest_stub::StubConfig {
        people: vec![(1, [0, 0, 0]), (2, [3, -5, 1])],
        full_places: vec![9],
        ..quest_stub::StubConfig::default()
    }
}

#[test]
fn fallible_query_must_be_handled() {
    assert_eq!(
        main_diags("fn f(a: Person, b: Person) -> int:\n    return a.distance_to(b)\n"),
        vec![d("unhandled-fallible", 2)]
    );
    let main = "fn f(a: Person, b: Person) -> int:\n    return a.distance_to(b) else 0\n\nfn main():\n    pass\n";
    assert_eq!(main_diags(main), vec![]);
    compile(&[("main.qs", main)]);
}

#[test]
fn fallible_query_else_and_match() {
    let main = r#"fn far(a: Person, b: Person) -> int:
    return a.distance_to(b) else -1

fn twice(a: Person, b: Person) -> int:
    return a.distance_to(b)? * 2

fn main():
    let a = Person(id=1)
    let b = Person(id=2)
    let c = Person(id=9)
    print(str(far(a, b)))
    print(str(far(a, c)))
    match a.distance_to(c):
        case Ok(d):
            print(str(d))
        case OutOfReach:
            print("out of reach")
    print(str(twice(a, b) else 0))
    print(str(twice(c, a) else 0))
"#;
    assert_eq!(main_diags(main), vec![]);
    // The inferred set is exactly what the query declares.
    let sigs = quest_lang::signatures(&common::tree(&[("main.qs", main)]), &quest_stub::stub_api());
    assert_eq!(sigs["twice"], "fn(Person, Person) -> int raises OutOfReach");
    let out = run_with(&[("main.qs", main)], people(), 2_000);
    assert_eq!(out.reports, vec![]);
    assert_eq!(out.lines, ["5", "-1", "out of reach", "10", "0"]);
}

#[test]
fn query_raising_undeclared_error_faults() {
    let main = r#"fn main():
    let d = Person(id=1).distance_to(Person(id=2)) else -1
    print(str(d))
"#;
    let mut host = Recorder::default();
    record(main, &mut host, 2_000, 100);
    assert_eq!(host.output, Vec::<String>::new());
    match host.reports.as_slice() {
        [
            Report::HandlerAborted {
                cause: AbortCause::Fault { kind, message, .. },
                ..
            },
        ] => {
            assert_eq!(*kind, FaultKind::HostFault);
            assert!(message.contains("raised NoRoom"), "{message}");
        }
        other => panic!("expected one host fault, got {other:?}"),
    }
}

#[test]
fn fallible_command_error_takes_the_else_path() {
    let main = r#"fn build(at: Place) -> str:
    place(Prefab(name="nest"), at) else:
        return "no room"
    return "placed"

fn main():
    print(build(Place(id=9)))
    print(build(Place(id=4)))
"#;
    let out = run_with(&[("main.qs", main)], people(), 2_000);
    assert_eq!(out.reports, vec![]);
    // The refused `place` buffered nothing; the other reached the host when the segment committed.
    assert_eq!(out.lines, ["no room", "place nest at 4", "placed"]);
}

#[test]
fn fallible_command_ok_is_buffered_until_commit() {
    let main = r#"fn main():
    place(Prefab(name="nest"), Place(id=4)) else:
        return
    let i = 0
    while i < 200:
        i = i + 1
    print("done")
"#;
    let args = format!(
        "{:?}",
        [
            W::Struct {
                name: "Prefab".into(),
                fields: vec![("name".into(), W::Str("nest".into()))],
            },
            W::Struct {
                name: "Place".into(),
                fields: vec![("id".into(), W::Int(4))],
            },
        ]
    );
    // Checked when issued; the segment is still running after a tick, so nothing is handed over yet.
    let mut host = Recorder::default();
    let mut vm = record(main, &mut host, 200, 1);
    assert_eq!(host.calls, [format!("check place {args}")]);
    for now in 1..100 {
        vm.tick(now, 200, &mut host);
        if vm.is_idle() {
            break;
        }
    }
    assert_eq!(
        host.calls,
        [
            format!("check place {args}"),
            format!("command place {args}")
        ]
    );
    assert_eq!(host.output, ["done"]);

    // A segment that aborts after a passing check hands nothing over.
    let aborts = main.replace("print(\"done\")", "assert(false)");
    let mut host = Recorder::default();
    record(&aborts, &mut host, 2_000, 100);
    assert_eq!(host.calls, [format!("check place {args}")]);
    assert_eq!(host.reports.len(), 1);

    // A refused check takes the `else` path and hands nothing over either.
    let mut host = Recorder {
        refuse: true,
        ..Recorder::default()
    };
    record(main, &mut host, 2_000, 100);
    assert_eq!(host.calls, [format!("check place {args}")]);
    assert_eq!((host.output, host.reports), (vec![], vec![]));
}
