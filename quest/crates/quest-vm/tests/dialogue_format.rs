//! The dialogue graph format (spec §5.11): its bytes, its validation and its hash.

use std::hash::Hasher;

use quest_vm::Op;
use quest_vm::asm::ProgramBuilder;
use quest_vm::dialogue::{
    Block, CondDef, DialogueDef, DialogueGraph, DialogueKind, LineDef, MemberDef, NodeDef, OnceDef,
    OptionDef, Pick, Speaker, StepDef, TopicDef, line_ticks,
};
use quest_vm::hash::Fnv64;
use quest_vm::program::{Program, ProgramError, QuestInfo};
use quest_vm::types::TypeDesc;
use state_hash::StateHash;

/// The pure fns a graph names: of two parameters (the conversation's cast) unless said otherwise.
struct Fns {
    cond: u16,
    text: u16,
    count: u16,
    who: u16,
    /// A `bool` fn of one parameter: the wrong arity for a conversation.
    cond1: u16,
    /// A fn without a signature.
    unsigned: u16,
    event: u16,
}

/// A program builder holding the fns, a script event `Found { kind: str, count: int }` and a quest with
/// two stages.
fn builder() -> (ProgramBuilder, Fns) {
    let mut pb = ProgramBuilder::new(1);
    let int = pb.ty(TypeDesc::Int);
    let st = pb.ty(TypeDesc::Str);
    let boolean = pb.ty(TypeDesc::Bool);
    let found = pb.ty(TypeDesc::Struct {
        name: "Found".into(),
        fields: vec![("kind".into(), st), ("count".into(), int)],
    });
    let pure = |pb: &mut ProgramBuilder, name: &str, params: u8, ret| {
        let f = pb.declare(name, params);
        pb.set_sig(f, vec![int; usize::from(params)], ret);
        let mut b = pb.body(f);
        b.emit(Op::Ret, 0, 0, 0);
        b.end();
        f.0
    };
    let fns = Fns {
        cond: pure(&mut pb, "d$cond#0", 2, boolean),
        text: pure(&mut pb, "d$subst#0", 2, st),
        count: pure(&mut pb, "d$emit_arg#0", 2, int),
        who: pure(&mut pb, "d$speaker#0", 2, int),
        cond1: pure(&mut pb, "b$cond#0", 1, boolean),
        unsigned: {
            let f = pb.declare("plain", 2);
            let mut b = pb.body(f);
            b.emit(Op::Ret, 0, 0, 0);
            b.end();
            f.0
        },
        event: pb.script_event("Found", found),
    };
    let start = pb.declare("$start::Q", 0);
    let mut b = pb.body(start);
    b.emit(Op::Ret, 0, 0, 0);
    b.end();
    let stage = pb.global("Q::stage", int);
    let parts = pb.global("Q::participants", int);
    pb.set_global_init(stage, start);
    pb.set_global_init(parts, start);
    pb.set_quest(QuestInfo {
        name: "Q".into(),
        roles: vec![],
        stages: vec!["Open".into(), "Search".into()],
        stage_global: stage.0,
        participants_global: parts.0,
        home_role: 0,
        repeat: None,
        parts: vec![],
        requires: vec![],
        start_fn: start.0,
        read_set: vec![stage.0],
        transitions_fn: start.0,
        objectives: vec![vec![], vec![]],
    });
    (pb, fns)
}

fn line(id: &str, speaker: Speaker, subst: Vec<u16>) -> LineDef {
    LineDef {
        id: id.into(),
        speaker,
        subst,
        ticks: 0,
    }
}

fn say(id: &str) -> StepDef {
    StepDef::Line {
        line: line(id, Speaker::Cast(1), vec![]),
        cond: CondDef::NONE,
    }
}

fn conversation(name: &str, nodes: Vec<NodeDef>) -> DialogueDef {
    DialogueDef {
        name: name.into(),
        kind: DialogueKind::Conversation,
        cast: 2,
        stage: None,
        target: None,
        filter: None,
        cast_cond: None,
        topic: None,
        exits: vec!["done".into()],
        pick: Pick::BestLeastRecent,
        stateless: false,
        interruptible: true,
        nodes,
        score: 0,
    }
}

fn node(name: &str, body: Block) -> NodeDef {
    NodeDef {
        name: name.into(),
        cond: CondDef::NONE,
        score: 0,
        body,
    }
}

/// Every step kind, both dialogue kinds, a topic, a node group, a detour with an arm, a follow-up.
fn graph(f: &Fns) -> DialogueGraph {
    let mut g = DialogueGraph::default();
    // `secret`: a topic with a named exit.
    let paid = g.block(vec![StepDef::Return { exit: 1 }]);
    let mut secret = conversation("Q::secret", vec![node("", paid)]);
    secret.exits.push("paid".into());
    secret.topic = Some(TopicDef {
        text: line("secret.topic", Speaker::Cast(0), vec![]),
        cond: Some(f.cond),
        priority: -3,
    });
    secret.stage = Some(1);
    // `farmer_talk`.
    let accept = g.block(vec![
        StepDef::Emit {
            event: f.event,
            script: true,
            args: vec![f.text, f.count],
        },
        say("farmer.thanks"),
    ]);
    let ask = g.block(vec![
        StepDef::Line {
            line: line("farmer.dog", Speaker::Cast(1), vec![f.text, f.text]),
            cond: CondDef::NONE,
        },
        StepDef::Jump { group: 0 },
    ]);
    let arm = g.block(vec![say("farmer.after_paid")]);
    let blood = g.block(vec![StepDef::Detour {
        dialogue: 1,
        arms: vec![(1, arm)],
    }]);
    let none = g.block(vec![]);
    let fallback = g.block(vec![say("farmer.silent")]);
    let member = g.block(vec![StepDef::Return { exit: 0 }]);
    let when = g.block(vec![StepDef::Emit {
        event: 3,
        script: false,
        args: vec![f.count],
    }]);
    let once = g.block(vec![say("farmer.once")]);
    let start = g.block(vec![
        StepDef::Line {
            line: line("farmer.start", Speaker::Fn(f.who), vec![]),
            cond: CondDef {
                once: Some(OnceDef {
                    site: 0,
                    per: Some(f.who),
                }),
                guard: Some(f.cond),
            },
        },
        StepDef::Choice {
            timeout: Some(600),
            options: vec![
                OptionDef {
                    text: line("farmer.accept", Speaker::Cast(0), vec![]),
                    guard: Some(f.cond),
                    once: None,
                    default: false,
                    body: accept,
                },
                OptionDef {
                    text: line("farmer.ask", Speaker::Cast(0), vec![f.text]),
                    guard: None,
                    once: None,
                    default: false,
                    body: ask,
                },
                OptionDef {
                    text: line("farmer.blood", Speaker::Cast(0), vec![]),
                    guard: None,
                    once: Some(1),
                    default: false,
                    body: blood,
                },
                OptionDef {
                    text: line("farmer.bye", Speaker::Cast(0), vec![]),
                    guard: None,
                    once: None,
                    default: false,
                    body: none,
                },
            ],
            fallback: Some(fallback),
        },
        StepDef::Group {
            site: 2,
            members: vec![
                MemberDef {
                    line: line("greet.a", Speaker::Cast(1), vec![]),
                    cond: CondDef::NONE,
                    score: 0,
                    body: member,
                },
                MemberDef {
                    line: line("greet.b", Speaker::Cast(1), vec![]),
                    cond: CondDef {
                        once: Some(OnceDef { site: 3, per: None }),
                        guard: None,
                    },
                    score: 1,
                    body: Block {
                        first: member.first + member.len,
                        len: 0,
                    },
                },
            ],
        },
        StepDef::When {
            cond: CondDef {
                once: None,
                guard: Some(f.cond),
            },
            body: when,
        },
        StepDef::Once {
            once: OnceDef { site: 4, per: None },
            body: once,
        },
        StepDef::Pause { ticks: 20 },
    ]);
    let again = g.block(vec![say("farmer.again"), StepDef::Jump { group: 1 }]);
    let mut farmer = conversation(
        "Q::farmer_talk",
        vec![
            NodeDef {
                name: "start".into(),
                cond: CondDef {
                    once: Some(OnceDef { site: 5, per: None }),
                    guard: Some(f.cond),
                },
                score: 2,
                body: start,
            },
            node("again", again),
            node(
                "start",
                Block {
                    first: again.first + again.len,
                    len: 0,
                },
            ),
        ],
    );
    farmer.pick = Pick::RandomBestLeastRecent;
    farmer.filter = Some(f.cond);
    // `gossip`: a scene of two, then a bark of one.
    let gossip_body = g.block(vec![
        StepDef::Line {
            line: LineDef {
                id: "gossip.a".into(),
                speaker: Speaker::Cast(1),
                subst: vec![],
                ticks: line_ticks(12),
            },
            cond: CondDef::NONE,
        },
        StepDef::Pause { ticks: 40 },
        StepDef::FollowUp { scene: 3 },
    ]);
    let mut gossip = conversation("gossip", vec![node("", gossip_body)]);
    gossip.kind = DialogueKind::Scene;
    gossip.cast_cond = Some(f.cond);
    gossip.score = 1;
    gossip.stateless = true;
    gossip.interruptible = false;
    let bark_body = g.block(vec![say_one("bark.hello")]);
    let mut bark = conversation("bark", vec![node("", bark_body)]);
    bark.kind = DialogueKind::Scene;
    bark.cast = 1;
    bark.pick = Pick::First;
    g.dialogues = vec![farmer, secret, gossip, bark];
    g
}

fn say_one(id: &str) -> StepDef {
    StepDef::Line {
        line: LineDef {
            id: id.into(),
            speaker: Speaker::Cast(0),
            subst: vec![],
            ticks: line_ticks(10),
        },
        cond: CondDef::NONE,
    }
}

fn program(g: DialogueGraph) -> Result<Program, ProgramError> {
    let (mut pb, _) = builder();
    pb.set_dialogues(g);
    pb.finish()
}

fn valid() -> Program {
    let (_, f) = builder();
    program(graph(&f)).expect("the graph is valid")
}

/// `graph(f)` changed by `edit`, built: the error's text.
fn refused(edit: impl FnOnce(&mut DialogueGraph, &Fns)) -> String {
    let (mut pb, f) = builder();
    let mut g = graph(&f);
    edit(&mut g, &f);
    pb.set_dialogues(g);
    match pb.finish() {
        Ok(_) => panic!("an invalid graph was accepted"),
        Err(e) => e.to_string(),
    }
}

fn hash(g: &DialogueGraph) -> u64 {
    let mut h = Fnv64::default();
    g.state_hash(&mut h);
    h.finish()
}

#[test]
fn graph_roundtrip() {
    let p = valid();
    assert_eq!(p.dialogues.dialogues.len(), 4);
    let bytes = p.to_bytes();
    let q = Program::from_bytes(&bytes).unwrap();
    assert_eq!(q, p);
    assert_eq!(q.to_bytes(), bytes);
    // The hash covers the graph: equal graphs hash equal, a changed duration does not.
    assert_eq!(hash(&q.dialogues), hash(&p.dialogues));
    let mut g = p.dialogues.clone();
    if let StepDef::Pause { ticks } = g
        .steps
        .iter_mut()
        .rev()
        .find(|s| matches!(s, StepDef::Pause { .. }))
        .unwrap()
    {
        *ticks += 1;
    }
    assert_ne!(hash(&g), hash(&p.dialogues));
    // A program without dialogues still round-trips.
    let empty = program(DialogueGraph::default()).unwrap();
    assert_eq!(Program::from_bytes(&empty.to_bytes()).unwrap(), empty);
}

#[test]
fn line_ticks_clamps() {
    assert_eq!(line_ticks(0), 30);
    assert_eq!(line_ticks(3), 33);
    assert_eq!(line_ticks(50), 80);
    assert_eq!(line_ticks(170), 200);
    assert_eq!(line_ticks(1000), 200);
    assert_eq!(line_ticks(u32::MAX), 200);
}

#[test]
fn overlapping_blocks_refused() {
    // Two nodes' bodies overlap.
    let why = refused(|g, _| {
        let b = g.dialogues[3].nodes[0].body;
        g.dialogues[1].nodes[0].body = b;
    });
    assert!(why.contains("in two blocks"), "{why}");
    // A body that runs past the steps.
    let why = refused(|g, _| g.dialogues[3].nodes[0].body.len = 2);
    assert!(why.contains("outside the steps"), "{why}");
    // A block that holds the step holding it: its steps are reached by no node.
    let why = refused(|g, _| {
        let i = g.dialogues[3].nodes[0].body.first;
        let b = g.block(vec![]);
        g.dialogues[3].nodes[0].body = b;
        g.steps.push(StepDef::When {
            cond: CondDef::NONE,
            body: Block { first: i, len: 2 },
        });
        g.steps[i as usize] = StepDef::Once {
            once: OnceDef { site: 9, per: None },
            body: Block {
                first: i + 1,
                len: 1,
            },
        };
    });
    assert!(why.contains("in no block"), "{why}");
    // Two records on one site.
    let why = refused(|g, _| {
        for s in &mut g.steps {
            if let StepDef::Once { once, .. } = s {
                once.site = 0;
            }
        }
    });
    assert!(why.contains("share a site"), "{why}");
}

#[test]
fn scene_with_choice_refused() {
    let why = refused(|g, _| {
        let i = g.dialogues[2].nodes[0].body.first as usize;
        let empty = g.block(vec![]);
        g.steps[i] = StepDef::Choice {
            timeout: None,
            options: vec![OptionDef {
                text: line("gossip.choice", Speaker::Cast(0), vec![]),
                guard: None,
                once: None,
                default: false,
                body: empty,
            }],
            fallback: None,
        };
    });
    assert!(why.contains("a scene has no `choice`"), "{why}");
    // Nor a detour, and a conversation has no follow-up.
    let why = refused(|g, _| {
        let i = g.dialogues[2].nodes[0].body.first as usize;
        g.steps[i] = StepDef::Detour {
            dialogue: 1,
            arms: vec![],
        };
    });
    assert!(why.contains("detour"), "{why}");
    let why = refused(|g, _| {
        let i = g.dialogues[0].nodes[1].body.first as usize;
        g.steps[i] = StepDef::FollowUp { scene: 3 };
    });
    assert!(why.contains("follow-up"), "{why}");
}

#[test]
fn wrong_arity_condition_refused() {
    let why = refused(|g, f| g.dialogues[0].nodes[0].cond.guard = Some(f.cond1));
    assert!(why.contains("a condition"), "{why}");
    // A `once per` key and a speaker take the cast too.
    let why = refused(|g, f| {
        g.dialogues[0].nodes[0].cond.once = Some(OnceDef {
            site: 5,
            per: Some(f.cond1),
        })
    });
    assert!(why.contains("`once per`"), "{why}");
    let why = refused(|g, f| {
        let i = g.dialogues[3].nodes[0].body.first as usize;
        if let StepDef::Line { line, .. } = &mut g.steps[i] {
            line.speaker = Speaker::Fn(f.cond);
        }
    });
    assert!(why.contains("a speaker"), "{why}");
    // A condition that is not `bool`, a substitution that is not `str`, an fn without a signature, an
    // `emit` argument of the wrong type.
    let why = refused(|g, f| g.dialogues[0].filter = Some(f.text));
    assert!(why.contains("the filter"), "{why}");
    let why = refused(|g, f| g.dialogues[1].topic.as_mut().unwrap().text.subst = vec![f.count]);
    assert!(why.contains("a substitution"), "{why}");
    let why = refused(|g, f| g.dialogues[0].nodes[0].cond.guard = Some(f.unsigned));
    assert!(why.contains("a condition"), "{why}");
    let why = refused(|g, _| {
        for s in &mut g.steps {
            if let StepDef::Emit {
                script: true, args, ..
            } = s
            {
                args.reverse();
            }
        }
    });
    assert!(why.contains("an `emit` argument"), "{why}");
}

#[test]
fn indices_out_of_range_refused() {
    let jump = |g: &mut DialogueGraph, group| {
        for s in &mut g.steps {
            if let StepDef::Jump { group: j } = s {
                *j = group;
            }
        }
    };
    // Node 2 is the second node of group `start`; 3 is out of range.
    for group in [2, 3] {
        let why = refused(|g, _| jump(g, group));
        assert!(why.contains("first node of a group"), "{why}");
    }
    let why = refused(|g, _| {
        g.steps.iter_mut().for_each(|s| {
            if let StepDef::Return { exit } = s {
                *exit = 2;
            }
        })
    });
    assert!(why.contains("no exit"), "{why}");
    let why = refused(|g, _| {
        g.steps.iter_mut().for_each(|s| {
            if let StepDef::Detour { arms, .. } = s {
                arms[0].0 = 2;
            }
        })
    });
    assert!(why.contains("exit"), "{why}");
    let why = refused(|g, _| g.dialogues[1].stage = Some(2));
    assert!(why.contains("stage"), "{why}");
    let why = refused(|g, _| g.dialogues[3].cast = 0);
    assert!(why.contains("cast"), "{why}");
    let why = refused(|g, _| g.dialogues[3].exits[0] = "end".into());
    assert!(why.contains("done"), "{why}");
}

#[cfg(not(target_arch = "wasm32"))]
mod props {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::Config;

    /// The bytes of `valid()` and the range its graph occupies (found against the same program without a
    /// graph, whose graph is two zero counts).
    fn bytes_and_graph() -> (Vec<u8>, std::ops::Range<usize>) {
        let full = valid().to_bytes();
        let bare = program(DialogueGraph::default()).unwrap().to_bytes();
        let start = full.iter().zip(&bare).take_while(|(a, b)| a == b).count();
        let end = full.len() - (bare.len() - start - 8);
        assert_eq!(
            full[start..start + 4],
            4u32.to_le_bytes(),
            "the graph starts with its count"
        );
        (full, start..end)
    }

    proptest! {
        #![proptest_config(Config::with_cases(2000))]

        #[test]
        fn hostile_graph_is_err(at in 0usize..1 << 16, byte in any::<u8>()) {
            let (mut bytes, graph) = bytes_and_graph();
            let i = graph.start + at % graph.len();
            bytes[i] = byte;
            if let Ok(p) = Program::from_bytes(&bytes) {
                p.validate().unwrap();
                prop_assert_eq!(p.to_bytes(), bytes);
            }
        }
    }
}
