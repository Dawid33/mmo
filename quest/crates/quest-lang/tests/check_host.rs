//! The host description of Phase 10 (plan Task 37; spec §9.1): host enums, properties, facts, named
//! arguments with defaults, fallible commands.

mod common;

use common::{compile, diags, run};
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

/// A host that records every query and command and answers the few queries `host_calls_reach_the_host`
/// makes.
#[derive(Default)]
struct Recorder {
    calls: Vec<String>,
    output: Vec<String>,
    reports: Vec<quest_vm::host::Report>,
}

impl quest_vm::host::Host for Recorder {
    fn query(&mut self, f: u16, args: &[W]) -> Result<W, String> {
        let hf = &quest_stub::stub_api().fns[usize::from(f)];
        self.calls.push(format!("query {} {args:?}", hf.name));
        Ok(match hf.name.as_str() {
            "near" | "alive" => W::Bool(true),
            "occupation" => W::Variant {
                name: "Occupation::Smith".into(),
                fields: vec![],
            },
            "prefab" => W::Struct {
                name: "Prefab".into(),
                fields: vec![("name".into(), args[0].clone())],
            },
            _ => return Err(format!("no answer for {}", hf.name)),
        })
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
    let api = quest_stub::stub_api();
    let mut vm = quest_vm::Vm::new(
        std::sync::Arc::new(compile(&[("main.qs", main)])),
        quest_vm::vm::VmConfig {
            mem_bytes: quest_vm::consts::MEM_BYTES_DEFAULT,
            epoch: 1,
            host_api_version: api.version,
            events: api.events.into_iter().map(|e| e.name).collect(),
        },
    )
    .expect("the program loads");
    let mut host = Recorder::default();
    vm.start_main();
    for now in 0..100 {
        vm.tick(now, 2_000, &mut host);
        if vm.is_idle() {
            break;
        }
    }
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
            format!("command place {:?}", [prefab, place]),
        ]
    );
}
