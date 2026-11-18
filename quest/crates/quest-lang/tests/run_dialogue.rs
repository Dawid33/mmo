//! Code generation for dialogues and scenes (plan Task 43; spec §5.11): the dialogue graph
//! (`Program::dialogues`), the lines table (`Compiled::lines`) and a script `emit` lowered to `Enqueue`.
//! The graph is inspected directly: the runner comes later.

mod common;

use common::{run, tree};
use quest_lang::Compiled;
use quest_vm::dialogue::{DialogueDef, DialogueKind, Speaker, StepDef};

/// Compiles `files` against the stub host; panics with the rendered diagnostics on an error.
fn compiled(files: &[(&str, &str)]) -> Compiled {
    let t = tree(files);
    match quest_lang::compile(&t, &quest_stub::stub_api()) {
        Ok(c) => c,
        Err(diags) => {
            let text: Vec<String> = diags.iter().map(|d| quest_lang::render(d, &t)).collect();
            panic!("compile failed:\n{}", text.join("\n"));
        }
    }
}

fn dialogue<'a>(c: &'a Compiled, name: &str) -> &'a DialogueDef {
    c.program
        .dialogues
        .dialogues
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no dialogue {name}"))
}

/// The steps of a block.
fn block(c: &Compiled, b: quest_vm::dialogue::Block) -> &[StepDef] {
    let r = b.range(c.program.dialogues.steps.len()).expect("in range");
    &c.program.dialogues.steps[r]
}

/// Spec §5.11's farmer, with the dialogue it detours into.
const FARMER: &str = r#"let accepted = false
let dog_name = "Rufus"
let farmer = Person(id=1)

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
    node secret when dialogue_depth() < 3:
        npc: "It's nothing. Chickens."                                 @secret.chickens

@on(Accepted)
fn accept(e):
    accepted = true
    give(e.player, Item(kind="dog whistle", count=1))
"#;

#[test]
fn farmer_graph_shape() {
    let c = compiled(&[("main.qs", FARMER)]);
    let names: Vec<&str> = c
        .program
        .dialogues
        .dialogues
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["farmer_secret", "farmer_talk"]);
    let d = dialogue(&c, "farmer_talk");
    assert_eq!(d.kind, DialogueKind::Conversation);
    assert_eq!(d.cast, 2);
    assert!(d.target.is_some() && d.filter.is_none() && d.topic.is_none());
    assert_eq!(d.pick, quest_vm::dialogue::Pick::Best);
    assert_eq!(d.exits, ["done"]);
    assert!(!d.stateless);
    assert_eq!(d.nodes.len(), 1);
    assert_eq!(d.nodes[0].name, "start");
    let body = block(&c, d.nodes[0].body);
    assert_eq!(body.len(), 3, "{body:?}");
    let StepDef::Line { line, .. } = &body[0] else {
        panic!("{:?}", body[0]);
    };
    assert_eq!(line.id, "farmer.start");
    assert_eq!(line.speaker, Speaker::Cast(1));
    let StepDef::Choice {
        options, fallback, ..
    } = &body[1]
    else {
        panic!("{:?}", body[1]);
    };
    assert!(fallback.is_none());
    assert_eq!(options.len(), 4);
    let once: Vec<bool> = options.iter().map(|o| o.once.is_some()).collect();
    assert_eq!(once, [false, false, true, false]);
    assert!(options[0].guard.is_some());
    assert_eq!(options[0].text.speaker, Speaker::Cast(0));
    // The first option emits, then a line.
    let first = block(&c, options[0].body);
    assert!(
        matches!(&first[0], StepDef::Emit { script: true, args, .. } if args.len() == 1),
        "{first:?}"
    );
    // The second option jumps back to `start` (node 0).
    let second = block(&c, options[1].body);
    assert_eq!(second.len(), 2);
    assert_eq!(second[1], StepDef::Jump { group: 0 });
    // The third detours into `farmer_secret` (dialogue 0 in qualified-name order).
    assert!(
        matches!(block(&c, options[2].body), [StepDef::Detour { dialogue: 0, arms }] if arms.is_empty())
    );
    assert!(block(&c, options[3].body).is_empty());
    assert!(matches!(&body[2], StepDef::Line { line, .. } if line.id == "farmer.bye"));
}

#[test]
fn lines_table_has_text_and_holes() {
    let c = compiled(&[("main.qs", FARMER)]);
    assert_eq!(
        c.lines.get("farmer.dog_desc").map(String::as_str),
        Some("Brown, one ear up. He answers to {0}.")
    );
    assert_eq!(
        c.lines.get("farmer.accept").map(String::as_str),
        Some("I'll find him.")
    );
    assert_eq!(
        c.lines.get("secret.chickens").map(String::as_str),
        Some("It's nothing. Chickens.")
    );
    assert_eq!(c.lines.len(), 9, "{:?}", c.lines);
    // The hole's substitution is the line's only one; a line without an id gets its provisional one.
    let c = compiled(&[(
        "main.qs",
        "let n = 2\n\ndialogue d(npc: Person):\n    npc: f\"{n} and {n + 1}, {{braces}}\"\n",
    )]);
    assert_eq!(
        c.lines.get("d.0").map(String::as_str),
        Some("{0} and {1}, {{braces}}")
    );
    let d = dialogue(&c, "d");
    let body = block(&c, d.nodes[0].body);
    assert!(matches!(&body[0], StepDef::Line { line, .. } if line.subst.len() == 2));
}

#[test]
fn graph_independent_of_module_order() {
    let a = "mod b\n\nscene zed(x: Person):\n    x: \"z\" @a.z\n    -> later\n\nscene later(x: Person):\n    x: \"l\" @a.l\n\nevent Pong:\n    n: int\n";
    let b = "event Ping:\n    n: int\n\ndialogue alpha(p: Player, npc: Person):\n    emit Ping(n=1)\n    npc: \"a\" @b.a\n";
    // The same declarations in another order (`alpha` keeps its lines: its `emit` argument is a fn, and a
    // fn's source lines are in the program's line table).
    let a2 = "event Pong:\n    n: int\n\nscene later(x: Person):\n    x: \"l\" @a.l\n\nscene zed(x: Person):\n    x: \"z\" @a.z\n    -> later\n\nmod b\n";
    let b2 = "\n\n\ndialogue alpha(p: Player, npc: Person):\n    emit Ping(n=1)\n    npc: \"a\" @b.a\n\nevent Ping:\n    n: int\n";
    let one = compiled(&[("main.qs", a), ("b.qs", b)]);
    let two = compiled(&[("main.qs", a2), ("b.qs", b2)]);
    assert_eq!(one.program.to_bytes(), two.program.to_bytes());
    assert_eq!(one.lines, two.lines);
    let names: Vec<&str> = one
        .program
        .dialogues
        .dialogues
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["b::alpha", "later", "zed"]);
    let events: Vec<&str> = one.program.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(events, ["Pong", "b::Ping"]);
}

#[test]
fn scene_line_ticks_from_source() {
    let main = r#"scene chat(a: Person, b: Person):
    a: "Hello there." @chat.hello (ticks=60)
    b: "..." @chat.dots
    pause 5
"#;
    let c = compiled(&[("main.qs", main)]);
    let d = dialogue(&c, "chat");
    assert_eq!(d.kind, DialogueKind::Scene);
    assert_eq!(d.cast, 2);
    assert!(d.stateless);
    let body = block(&c, d.nodes[0].body);
    let ticks: Vec<(u32, Speaker)> = body
        .iter()
        .filter_map(|s| match s {
            StepDef::Line { line, .. } => Some((line.ticks, line.speaker)),
            _ => None,
        })
        .collect();
    assert_eq!(ticks, [(60, Speaker::Cast(0)), (33, Speaker::Cast(1))]);
    assert_eq!(body[2], StepDef::Pause { ticks: 5 });
}

#[test]
fn script_emit_in_handler_enqueues() {
    let main = r#"event Ping:
    n: int

@on(Ping)
fn pong(e):
    print(f"ping {e.n}")
    emit Pong(n=e.n + 1)
    print("pinged")

event Pong:
    n: int

@on(Pong)
fn last(e):
    print(f"pong {e.n}")

fn main():
    emit Ping(n=7)
    print("sent")
"#;
    let c = compiled(&[("main.qs", main)]);
    let events: Vec<&str> = c.program.events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(events, ["Ping", "Pong"]);
    let out = run(&[("main.qs", main)]);
    assert!(out.reports.is_empty(), "{:?}", out.reports);
    // A handler's `emit` is handled after the handler's own segment, as a host's delivery would be.
    assert_eq!(out.lines, ["sent", "ping 7", "pinged", "pong 8"]);
}

#[test]
fn scene_line_ticks_count_holes_by_source() {
    // "Hi " (3) + `{b.id}` (6, its braces included) + "!" (1) = 10 characters: 30 + 10 ticks.
    let main = "scene chat(a: Person, b: Person):\n    a: f\"Hi {b.id}!\" @chat.hole\n";
    let c = compiled(&[("main.qs", main)]);
    let d = dialogue(&c, "chat");
    let body = block(&c, d.nodes[0].body);
    assert!(
        matches!(&body[0], StepDef::Line { line, .. } if line.ticks == 40 && line.subst.len() == 1),
        "{body:?}"
    );
}

/// The `once` site of each option of the dialogue's first choice.
fn option_sites(c: &Compiled) -> Vec<(String, Option<u32>)> {
    let d = dialogue(c, "d");
    let body = block(c, d.nodes[0].body);
    let StepDef::Choice { options, .. } = &body[0] else {
        panic!("{body:?}");
    };
    options
        .iter()
        .map(|o| (o.text.id.clone(), o.once))
        .collect()
}

#[test]
fn once_site_survives_an_earlier_insertion() {
    let before = r#"dialogue d(p: Player, npc: Person):
    choice:
        once "Ask about the dog." @d.ask
        "Goodbye." @d.bye
"#;
    let after = r#"dialogue d(p: Player, npc: Person):
    choice:
        once "Ask about the weather." @d.weather
        once "Ask about the dog." @d.ask
        "Goodbye." @d.bye
"#;
    let old = option_sites(&compiled(&[("main.qs", before)]));
    let new = option_sites(&compiled(&[("main.qs", after)]));
    let ask =
        |v: &[(String, Option<u32>)]| v.iter().find(|(id, _)| id == "d.ask").and_then(|o| o.1);
    assert!(ask(&old).is_some());
    assert_eq!(ask(&old), ask(&new));
    assert_ne!(new[0].1, new[1].1);
}

#[test]
fn once_steps_sharing_an_anchor_collide() {
    // Both `once:` steps are anchored at their first line, `d.x`.
    let main = r#"dialogue d(p: Player, npc: Person):
    once:
        once:
            npc: "x" @d.x
"#;
    let codes: Vec<String> =
        match quest_lang::compile(&tree(&[("main.qs", main)]), &quest_stub::stub_api()) {
            Ok(_) => panic!("compiled"),
            Err(diags) => diags.into_iter().map(|d| d.code.to_string()).collect(),
        };
    assert_eq!(codes, ["site-collision"]);
}

#[test]
fn provisional_line_id_collides_with_a_written_one() {
    // An explicit id cannot end in a number (`@d.0` does not parse), but two provisional ids can meet:
    // module `farm`'s `talk` and quest `farm`'s `talk` are both `farm.talk.0`.
    let main = "mod farm\n\nquest farm:\n    dialogue talk(npc: Person):\n        npc: \"b\"\n";
    let farm = "dialogue talk(npc: Person):\n    npc: \"a\"\n";
    let errors: Vec<(String, String, u32)> =
        common::diagnostics(&[("main.qs", main), ("farm.qs", farm)])
            .into_iter()
            .filter(|d| d.severity == quest_lang::Severity::Error)
            .map(|d| (d.code.to_string(), d.file, d.line))
            .collect();
    // (The quest beside the module of its name is `duplicate-def` as well.)
    assert!(
        errors.contains(&("duplicate-line-id".to_string(), "farm.qs".to_string(), 2)),
        "{errors:?}"
    );
}
