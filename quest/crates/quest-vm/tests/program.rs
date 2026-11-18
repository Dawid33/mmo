//! The instruction set, the program format, its validation and the assembler (design D5, D6, D9).

use quest_vm::asm::ProgramBuilder;
use quest_vm::bytecode::{BYTECODE_VERSION, Builtin, Instr, Op};
use quest_vm::fuel::*;
use quest_vm::host::HostFnKind;
use quest_vm::program::{
    Anchor, FnInfo, FnSig, GlobalInfo, PartInfo, Program, ProgramError, QuestInfo, RepeatInfo,
    RoleInfo,
};
use quest_vm::types::TypeDesc;

fn instr(op: Op, a: u16, b: u16, c: u16) -> Instr {
    Instr { op, a, b, c }
}

#[test]
fn instr_encoding_pinned() {
    let i = instr(Op::AddI, 1, 2, 0x0304);
    assert_eq!(i.encode(), [Op::AddI as u8, 0, 1, 0, 2, 0, 4, 3]);
    assert_eq!(Instr::decode(i.encode()), Some(i));
    // a non-zero padding byte or an unknown op is not an instruction
    assert_eq!(Instr::decode([Op::AddI as u8, 1, 1, 0, 2, 0, 4, 3]), None);
    assert_eq!(Instr::decode([255, 0, 0, 0, 0, 0, 0, 0]), None);
    // T is b | c << 16
    assert_eq!(Instr::with_t(Op::Jmp, 0, 0x0102_0304).t(), 0x0102_0304);
}

#[test]
fn op_and_builtin_numbers_pinned() {
    use Op::*;
    let ops: &[(Op, u8)] = &[
        (Nop, 0),
        (LoadSmall, 1),
        (LoadConst, 2),
        (LoadStr, 3),
        (Copy, 4),
        (Dup, 5),
        (Move, 6),
        (Drop, 7),
        (AddI, 8),
        (SubI, 9),
        (MulI, 10),
        (DivI, 11),
        (RemI, 12),
        (NegI, 13),
        (EqI, 14),
        (NeI, 15),
        (LtI, 16),
        (LeI, 17),
        (AddF, 18),
        (SubF, 19),
        (MulF, 20),
        (DivF, 21),
        (NegF, 22),
        (EqF, 23),
        (NeF, 24),
        (LtF, 25),
        (LeF, 26),
        (IntToFloat, 27),
        (FloatToInt, 28),
        (Not, 29),
        (Eq, 30),
        (Jmp, 31),
        (JmpIf, 32),
        (JmpIfNot, 33),
        (Call, 34),
        (Ret, 35),
        (TracePush, 36),
        (NewObj, 37),
        (GetField, 38),
        (TakeField, 39),
        (SetField, 40),
        (GetTag, 41),
        (MakeUnique, 42),
        (NewList, 43),
        (ListLen, 44),
        (GetIndex, 45),
        (TakeIndex, 46),
        (SetIndex, 47),
        (NewDict, 48),
        (GetKey, 49),
        (TakeKey, 50),
        (SetKey, 51),
        (RefMake, 52),
        (RefField, 53),
        (RefIndex, 54),
        (RefKey, 55),
        (RefLoad, 56),
        (RefTake, 57),
        (RefStore, 58),
        (CallBuiltin, 59),
        (CallHost, 60),
        (Await, 61),
        (AwaitAll, 62),
        (PromiseDetach, 63),
        (Enqueue, 64),
        (After, 65),
        (CancelTimer, 66),
        (CancelStageTimers, 67),
    ];
    assert_eq!(ops.len(), Op::ALL.len());
    for &(op, n) in ops {
        assert_eq!(op as u8, n, "{op:?}");
        assert_eq!(Op::from_u8(n), Some(op));
    }
    assert_eq!(Op::from_u8(68), None);
    assert_eq!(Op::from_u8(255), None);

    use Builtin::*;
    let bs: &[(Builtin, u16)] = &[
        (ListPush, 0),
        (ListPop, 1),
        (ListInsert, 2),
        (ListRemove, 3),
        (ListSlice, 4),
        (ListContains, 5),
        (DictGet, 6),
        (DictRemove, 7),
        (DictContains, 8),
        (DictKeys, 9),
        (DictValues, 10),
        (DictLen, 11),
        (DictNext, 12),
        (DictKeyAt, 13),
        (DictValAt, 14),
        (StrLen, 15),
        (StrConcat, 16),
        (StrSplit, 17),
        (StrStartsWith, 18),
        (StrEndsWith, 19),
        (StrContains, 20),
        (StrTrim, 21),
        (StrUpper, 22),
        (StrLower, 23),
        (ParseInt, 24),
        (Fmt, 25),
        (AbsI, 26),
        (MinI, 27),
        (MaxI, 28),
        (AbsF, 29),
        (MinF, 30),
        (MaxF, 31),
        (Sqrt, 32),
        (Floor, 33),
        (Ceil, 34),
        (Round, 35),
        (Sin, 36),
        (Cos, 37),
        (Pow, 38),
        (Assert, 39),
        (Print, 40),
        (ModF, 41),
        (StrCmp, 42),
    ];
    assert_eq!(bs.len(), Builtin::ALL.len());
    for &(b, n) in bs {
        assert_eq!(b as u16, n, "{b:?}");
        assert_eq!(Builtin::from_u16(n), Some(b));
    }
    // argument slots read from `b` (design D5 / the Builtin::arity doc)
    let arity: &[(Builtin, u8)] = &[
        (ListPush, 2),
        (ListPop, 1),
        (ListInsert, 3),
        (ListRemove, 2),
        (ListSlice, 3),
        (ListContains, 2),
        (DictGet, 2),
        (DictRemove, 2),
        (DictContains, 2),
        (DictKeys, 1),
        (DictValues, 1),
        (DictLen, 1),
        (DictNext, 2),
        (DictKeyAt, 2),
        (DictValAt, 2),
        (StrLen, 1),
        (StrConcat, 2),
        (StrSplit, 2),
        (StrStartsWith, 2),
        (StrEndsWith, 2),
        (StrContains, 2),
        (StrTrim, 1),
        (StrUpper, 1),
        (StrLower, 1),
        (ParseInt, 1),
        (Fmt, 2),
        (AbsI, 1),
        (MinI, 2),
        (MaxI, 2),
        (AbsF, 1),
        (MinF, 2),
        (MaxF, 2),
        (Sqrt, 1),
        (Floor, 1),
        (Ceil, 1),
        (Round, 1),
        (Sin, 1),
        (Cos, 1),
        (Pow, 2),
        (Assert, 2),
        (Print, 1),
        (ModF, 2),
        (StrCmp, 2),
    ];
    assert_eq!(arity.len(), Builtin::ALL.len());
    for &(b, a) in arity {
        assert_eq!(b.arity(), a, "{b:?}");
    }
    assert_eq!(Builtin::from_u16(43), None);
    assert_eq!(Builtin::from_u16(u16::MAX), None);
}

#[test]
fn fuel_table_pinned() {
    assert_eq!(FUEL_SIMPLE, 1u64);
    assert_eq!(FUEL_CALL, 2u64);
    assert_eq!(FUEL_HEAP, 3u64);
    assert_eq!(FUEL_ALLOC, 5u64);
    assert_eq!(FUEL_KEY, 6u64);
    assert_eq!(FUEL_BUILTIN, 4u64);
    assert_eq!(FUEL_HOST_QUERY, 10u64);
    assert_eq!(FUEL_HOST_COMMAND, 10u64);
    assert_eq!(FUEL_HOST_REQUEST, 20u64);
    assert_eq!(FUEL_AWAIT, 20u64);
    assert_eq!(FUEL_RESUME, 20u64);
    assert_eq!(FUEL_DISPATCH, 10u64);
    assert_eq!(FUEL_REAP, 2u64);
    assert_eq!(FUEL_ENQUEUE, 10u64);
    assert_eq!(FUEL_AFTER, 10u64);
    assert_eq!(FUEL_CANCEL, 5u64);
}

/// A valid program: `main` (fn 0) calls `callee` (fn 1); one `Nop` in each function is room for `poke`.
fn base() -> Program {
    let mut pb = ProgramBuilder::new(7);
    let int = pb.ty(TypeDesc::Int);
    let main = pb.declare("main", 0);
    let callee = pb.declare("callee", 1);
    let mut f = pb.body(main);
    f.line(1);
    f.emit(Op::Nop, 0, 0, 0);
    f.emit(Op::LoadSmall, f.local(0), 5, 0);
    let ok = f.label();
    f.jump(Op::JmpIfNot, f.local(0), ok);
    f.emit(Op::Copy, f.local(1), f.local(0), 0);
    f.bind(ok);
    f.line(2);
    f.emit(Op::Ret, 0, 0, 0);
    f.end();
    let _ = int;
    let mut c = pb.body(callee);
    c.emit(Op::Nop, 0, 0, 0);
    c.emit(Op::AddI, c.local(0), c.param(0), c.param(0));
    c.emit(Op::Ret, 0, 0, 0);
    c.end();
    pb.main(main);
    pb.finish().expect("base program is valid")
}

fn poke(p: &mut Program, fn_id: usize, i: Instr) {
    let f = &p.fns[fn_id];
    let at = (f.entry..f.end)
        .find(|&pc| p.code[pc as usize].op == Op::Nop)
        .expect("a Nop to overwrite");
    p.code[at as usize] = i;
}

fn invalid(p: &Program) -> String {
    match p.validate() {
        Err(ProgramError::Invalid(why)) => why,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn base_validates() {
    base().validate().unwrap();
}

#[test]
fn validate_rejects_slot_outside_static_words() {
    let mut p = base();
    let n = p.static_words;
    poke(&mut p, 0, instr(Op::LoadSmall, n, 1, 0));
    invalid(&p);
    let mut p = base();
    poke(&mut p, 0, instr(Op::Copy, 0, n, 0));
    invalid(&p);
    // Await writes a + 1 as well
    let mut p = base();
    let n = p.static_words;
    poke(&mut p, 0, instr(Op::Await, n - 1, 0, 0));
    invalid(&p);
}

#[test]
fn validate_rejects_jump_outside_function() {
    let mut p = base();
    let at = p.code.iter().position(|i| i.op == Op::JmpIfNot).unwrap();
    // into the other function
    let target = p.fns[1].entry;
    p.code[at] = Instr::with_t(Op::JmpIfNot, p.code[at].a, target);
    invalid(&p);
    // past the end of the code
    let mut p = base();
    p.code[at] = Instr::with_t(Op::JmpIfNot, p.code[at].a, u32::MAX);
    invalid(&p);
}

#[test]
fn validate_rejects_fn_id_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, instr(Op::Call, 2, 0, 0));
    invalid(&p);
}

#[test]
fn validate_rejects_const_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, Instr::with_t(Op::LoadConst, 3, 0));
    invalid(&p);
}

#[test]
fn validate_rejects_string_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, Instr::with_t(Op::LoadStr, 3, 0));
    invalid(&p);
}

#[test]
fn validate_rejects_shape_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, instr(Op::NewObj, 3, 0, 0));
    invalid(&p);
}

#[test]
fn validate_rejects_type_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, instr(Op::Await, 3, 3, 99));
    invalid(&p);
    // in a table
    let mut p = base();
    p.types.0.push(TypeDesc::List(500));
    invalid(&p);
}

#[test]
fn validate_rejects_builtin_out_of_range() {
    let mut p = base();
    poke(
        &mut p,
        0,
        instr(Op::CallBuiltin, Builtin::ALL.len() as u16, 3, 3),
    );
    invalid(&p);
}

#[test]
fn validate_rejects_host_site_out_of_range() {
    let mut p = base();
    poke(&mut p, 0, instr(Op::CallHost, 0, 3, 3));
    invalid(&p);
}

#[test]
fn validate_rejects_builtin_arguments_past_the_static_region() {
    let mut p = base();
    let n = p.static_words;
    poke(
        &mut p,
        0,
        instr(Op::CallBuiltin, Builtin::ListPush as u16, n - 2, 0),
    );
    p.validate().unwrap(); // two argument slots fit exactly
    let mut p = base();
    poke(
        &mut p,
        0,
        instr(Op::CallBuiltin, Builtin::ListPush as u16, n - 1, 0),
    );
    invalid(&p);
}

#[test]
fn validate_rejects_handler_frame_without_room_for_its_event() {
    let p = full();
    let h = usize::from(p.handlers[0].fn_id);
    let mut q = p.clone();
    q.fns[h].frame_words = 3;
    invalid(&q);
    let mut q = p.clone();
    q.fns[h].params = 0;
    invalid(&q);
    // the event parameter's ref slot must agree with the event type
    let mut q = p.clone();
    q.fns[h].ref_slots.clear();
    invalid(&q);
    // main and initialisers take no parameters
    let mut q = p.clone();
    let m = usize::from(q.main_fn.unwrap());
    q.fns[m].params = 1;
    q.fns[m].frame_words += 1;
    invalid(&q);
}

#[test]
fn finish_rejects_argument_runs_in_globals() {
    let mut pb = ProgramBuilder::new(1);
    let int = pb.ty(TypeDesc::Int);
    let g = pb.global("g", int);
    let mut f = pb.func("f", 0);
    f.emit(Op::CallBuiltin, Builtin::AbsI as u16, g, f.local(0));
    f.emit(Op::Ret, 0, 0, 0);
    f.end();
    assert!(matches!(pb.finish(), Err(ProgramError::Invalid(_))));
}

#[test]
fn validate_rejects_unsorted_or_duplicate_ref_slots() {
    let p = full();
    for bad in [vec![2, 1], vec![1, 1]] {
        let mut q = p.clone();
        q.fns[0].ref_slots = bad; // fn 0 is the initialiser, 4 words
        invalid(&q);
    }
    let mut q = p.clone();
    q.fns[0].ref_slots = vec![1, 2];
    q.validate().unwrap();
}

#[test]
fn frames_cover_the_error_slot_of_await() {
    let mut pb = ProgramBuilder::new(1);
    let int = pb.ty(TypeDesc::Int);
    let mut f = pb.func("f", 1);
    f.emit(Op::Await, f.local(0), f.param(0), int);
    f.emit(Op::Ret, 0, 0, 0);
    f.end();
    let p = pb.finish().unwrap();
    // 3 header + 1 parameter + 1 local, plus the error word a + 1
    assert_eq!(p.fns[0].frame_words, 6);
}

#[test]
fn validate_rejects_global_below_a_frame_end() {
    let mut p = full();
    p.globals[0].slot = 0;
    invalid(&p);
}

/// 5 000 one-instruction functions and 50 000 globals (the most a 16-bit static region holds): validation
/// and parsing take time linear in the input, not quadratic.
#[test]
fn validate_is_fast_on_many_globals_and_frames() {
    let t = std::time::Instant::now();
    let mut p = Program {
        bytecode_version: BYTECODE_VERSION,
        host_api_version: 1,
        code: vec![],
        consts: vec![],
        strings: vec![],
        shapes: vec![],
        types: quest_vm::types::TypeTable(vec![TypeDesc::Int]),
        fns: vec![],
        globals: vec![],
        handlers: vec![],
        events: vec![],
        quest: None,
        main_fn: None,
        host_sites: vec![],
        lines: vec![],
        files: vec![],
        static_words: 65_000,
    };
    for i in 0..5_000u32 {
        p.code.push(instr(Op::Ret, 0, 0, 0));
        p.fns.push(FnInfo {
            name: String::new(),
            entry: i,
            end: i + 1,
            params: 0,
            frame_base: (i * 3) as u16,
            frame_words: 3,
            ref_slots: vec![],
            sig: None,
        });
    }
    for i in 0..50_000u32 {
        p.globals.push(GlobalInfo {
            name: String::new(),
            slot: (15_000 + i) as u16,
            ty: 0,
            init_fn: 0,
        });
    }
    p.validate().unwrap();
    let q = Program::from_bytes(&p.to_bytes()).unwrap();
    assert_eq!(q, p);
    assert!(t.elapsed().as_secs() < 5, "took {:?}", t.elapsed());
}

#[test]
fn validate_rejects_bad_signature() {
    // `callee` (fn 1) takes one int; type 0 is `int`.
    let sig = |params: Vec<u16>, ret: u16| {
        let mut p = base();
        p.fns[1].sig = Some(FnSig { params, ret });
        p
    };
    sig(vec![0], 0).validate().unwrap();
    assert!(invalid(&sig(vec![], 0)).contains("arity"));
    assert!(invalid(&sig(vec![0, 0], 0)).contains("arity"));
    assert!(invalid(&sig(vec![9], 0)).contains("out of range"));
    assert!(invalid(&sig(vec![0], 9)).contains("out of range"));
    // a heap-kind parameter in a word that is not a ref slot
    let mut p = sig(vec![0], 0);
    p.types.0.push(TypeDesc::Str);
    p.fns[1].sig = Some(FnSig {
        params: vec![1],
        ret: 0,
    });
    assert!(invalid(&p).contains("ref slot"));
}

#[test]
fn validate_rejects_overlapping_frames() {
    let mut p = base();
    p.fns[1].frame_base = p.fns[0].frame_base;
    invalid(&p);
}

#[test]
fn validate_rejects_call_cycle() {
    // mutual
    let mut p = base();
    poke(&mut p, 0, instr(Op::Call, 1, 0, 0));
    poke(&mut p, 1, instr(Op::Call, 0, 0, 0));
    assert!(invalid(&p).contains("cycle"));
    // direct
    let mut p = base();
    poke(&mut p, 1, instr(Op::Call, 1, 0, 0));
    assert!(invalid(&p).contains("cycle"));
    // a plain call is fine
    let mut p = base();
    poke(&mut p, 0, instr(Op::Call, 1, 0, 0));
    p.validate().unwrap();
}

#[test]
fn validate_rejects_function_not_ending_in_ret_or_jmp() {
    let mut p = base();
    let end = p.fns[1].end as usize;
    p.code[end - 1] = instr(Op::Nop, 0, 0, 0);
    invalid(&p);
}

#[test]
fn validate_rejects_other_bytecode_version() {
    let mut p = base();
    p.bytecode_version = BYTECODE_VERSION + 1;
    assert_eq!(
        p.validate(),
        Err(ProgramError::Version(BYTECODE_VERSION + 1))
    );
}

/// A program that uses every table.
fn full() -> Program {
    let mut pb = ProgramBuilder::new(3);
    let int = pb.ty(TypeDesc::Int);
    let st = pb.ty(TypeDesc::Str);
    let list = pb.ty(TypeDesc::List(st));
    let dict = pb.ty(TypeDesc::Dict(st, int));
    let prom = pb.ty(TypeDesc::Promise(int));
    let rec = pb.ty(TypeDesc::Struct {
        name: "Item".into(),
        fields: vec![("kind".into(), st), ("count".into(), int)],
    });
    let en = pb.ty(TypeDesc::Enum {
        name: "Option".into(),
        variants: vec![
            ("None".into(), vec![]),
            ("Some".into(), vec![("value".into(), rec)]),
        ],
    });
    let init = pb.declare("m::init_g", 0);
    let g = pb.global("m::g", list);
    pb.set_global_init(g, init);
    let handler = pb.declare("m::on_click", 1);
    let site = pb.host_site(5, HostFnKind::Promise, vec![st, int], prom);
    let shape = pb.shape(2, 0b01);
    let k = pb.const_u64(0xdead_beef_0000_0001);
    let s = pb.string("hello");
    pb.set_file("quests/a.quest");
    let mut f = pb.body(init);
    f.line(3);
    f.emit_t(Op::LoadConst, f.local(0), k);
    f.emit_t(Op::LoadStr, f.local(0), s);
    f.emit(Op::NewObj, f.local(0), shape, 1);
    f.emit(Op::CallHost, site, f.local(0), f.local(0));
    f.mark_ref(f.local(0));
    f.emit(Op::Ret, 0, 0, 0);
    f.end();
    let mut h = pb.body(handler);
    h.line_col(9, 4);
    h.mark_ref(h.param(0));
    h.emit(Op::Ret, 0, 0, 0);
    h.end();
    pb.handler("PlayerClicked", en, handler);
    pb.main(init);
    let _ = (dict, g);

    // A quest: script events, roles, stages, a part, owned and stage-scoped handlers, the timer ops.
    let opt_str = pb.ty(TypeDesc::Enum {
        name: "Option".into(),
        variants: vec![
            ("None".into(), vec![]),
            ("Some".into(), vec![("value".into(), st)]),
        ],
    });
    let found = pb.script_event("DogFound", rec);
    let stage = pb.global("Q::stage", int);
    let parts = pb.global("Q::participants", int);
    let farmer = pb.global("Q::role.farmer", int);
    let miller = pb.global("Q::role.miller", opt_str);
    for g in [stage, parts, farmer, miller] {
        pb.set_global_init(g, init);
    }
    let req = pb.declare("$requires::Q", 0);
    let cond = pb.declare("$where::Q::miller", 2);
    let start = pb.declare("$start::Q", 0);
    pb.set_sig(cond, vec![int, st], int);
    for f in [req, start] {
        let mut b = pb.body(f);
        b.emit(Op::Ret, 0, 0, 0);
        b.end();
    }
    let mut b = pb.body(cond);
    b.mark_ref(b.param(1));
    b.emit(Op::Ret, 0, 0, 0);
    b.end();
    let on_found = pb.declare("Q::found", 1);
    let mut b = pb.body(on_found);
    let ev = b.param(0);
    b.mark_ref(ev);
    b.emit(Op::Enqueue, found, ev, 0);
    b.emit(Op::Copy, b.local(1), ev, 0);
    b.emit(Op::After, b.local(0), b.local(1), found);
    b.emit(Op::CancelTimer, b.local(0), 0, 0);
    b.emit(Op::CancelStageTimers, 0, 0, 0);
    b.emit(Op::Ret, 0, 0, 0);
    b.end();
    pb.owned_handler("DogFound", rec, on_found, 0, Some(1));
    let spotted = pb.declare("Q::watch::spotted", 1);
    let mut b = pb.body(spotted);
    b.mark_ref(b.param(0));
    b.emit(Op::Ret, 0, 0, 0);
    b.end();
    pb.owned_handler("PlayerClicked", en, spotted, 1, None);
    pb.set_quest(QuestInfo {
        name: "Q".into(),
        roles: vec![
            RoleInfo {
                name: "farmer".into(),
                global: farmer.0,
                ty: int,
                optional: false,
                cond: None,
            },
            RoleInfo {
                name: "miller".into(),
                global: miller.0,
                ty: st,
                optional: true,
                cond: Some(cond.0),
            },
        ],
        stages: vec!["Open".into(), "Search".into()],
        stage_global: stage.0,
        participants_global: parts.0,
        home_role: 0,
        repeat: Some(RepeatInfo {
            cooldown: 9_000,
            exclusive: true,
            once_per_player: false,
        }),
        parts: vec![PartInfo {
            name: "watch".into(),
            anchor: Anchor::On,
            role: 0,
            read_set: vec![stage.0, parts.0],
        }],
        requires: vec![req.0],
        start_fn: start.0,
        read_set: vec![stage.0],
    });
    pb.finish().expect("full program is valid")
}

#[test]
fn roundtrip_bytes_identical() {
    let p = full();
    assert!(!p.consts.is_empty() && !p.strings.is_empty() && !p.shapes.is_empty());
    assert!(!p.types.0.is_empty() && !p.fns.is_empty() && !p.globals.is_empty());
    assert!(!p.handlers.is_empty() && p.main_fn.is_some() && !p.host_sites.is_empty());
    assert!(
        !p.lines.is_empty() && !p.files.is_empty() && p.fns.iter().any(|f| !f.ref_slots.is_empty())
    );
    let bytes = p.to_bytes();
    assert_eq!(&bytes[..8], b"QSPROG01");
    let q = Program::from_bytes(&bytes).unwrap();
    assert_eq!(q, p);
    assert_eq!(q.to_bytes(), bytes);
    assert_eq!(q.content_hash(), p.content_hash());
}

#[test]
fn program_v4_roundtrip() {
    let p = full();
    assert_eq!(p.bytecode_version, 4);
    assert!(p.fns.iter().any(|f| f.sig.is_some()) && p.fns.iter().any(|f| f.sig.is_none()));
    let q = p.quest.as_ref().expect("a quest");
    assert_eq!(p.events.len(), 1);
    assert_eq!(q.roles.len(), 2);
    assert_eq!(q.parts.len(), 1);
    assert!(p.handlers.iter().any(|h| h.owner == 1));
    assert!(p.handlers.iter().any(|h| h.stage == Some(1)));
    for op in [
        Op::Enqueue,
        Op::After,
        Op::CancelTimer,
        Op::CancelStageTimers,
    ] {
        assert!(p.code.iter().any(|i| i.op == op), "{op:?}");
    }
    let bytes = p.to_bytes();
    let back = Program::from_bytes(&bytes).unwrap();
    assert_eq!(back, p);
    assert_eq!(back.to_bytes(), bytes);
    // a plain program has neither
    let b = base();
    assert!(b.events.is_empty() && b.quest.is_none());
    assert_eq!(Program::from_bytes(&b.to_bytes()).unwrap(), b);
}

/// Each corruption of the quest tables is refused by `validate` and by `from_bytes` of its bytes, never a
/// panic.
#[test]
fn hostile_quest_info_is_err() {
    let refused = |p: &Program| {
        assert!(matches!(p.validate(), Err(ProgramError::Invalid(_))));
        assert!(Program::from_bytes(&p.to_bytes()).is_err());
    };
    let ok = full();
    let ng = ok.globals.len() as u16;
    let nf = ok.fns.len() as u16;
    type Edit = Box<dyn Fn(&mut Program)>;
    let edits: Vec<Edit> = vec![
        // a role global out of range
        Box::new(move |p| p.quest.as_mut().unwrap().roles[0].global = ng),
        Box::new(|p| p.quest.as_mut().unwrap().roles[0].global = u16::MAX),
        // a role whose global's type is not the role's (an optional role's is `Option[T]`)
        Box::new(|p| p.quest.as_mut().unwrap().roles[1].optional = false),
        Box::new(|p| p.quest.as_mut().unwrap().roles[0].optional = true),
        // two roles of one name, or one global
        Box::new(|p| p.quest.as_mut().unwrap().roles[1].name = "farmer".into()),
        // a part role out of range
        Box::new(|p| p.quest.as_mut().unwrap().parts[0].role = 2),
        // a handler's stage out of range, its owner past the parts, a stage outside a quest
        Box::new(|p| p.handlers[1].stage = Some(2)),
        Box::new(|p| p.handlers[1].owner = 2),
        Box::new(|p| {
            p.quest = None;
            p.handlers.retain(|h| h.owner == 0);
        }),
        // read sets and well-known globals out of range or unsorted
        Box::new(move |p| p.quest.as_mut().unwrap().read_set = vec![ng]),
        Box::new(|p| p.quest.as_mut().unwrap().parts[0].read_set.reverse()),
        Box::new(move |p| p.quest.as_mut().unwrap().stage_global = ng),
        Box::new(move |p| p.quest.as_mut().unwrap().participants_global = ng),
        Box::new(|p| p.quest.as_mut().unwrap().home_role = 2),
        // fns out of range or of the wrong arity
        Box::new(move |p| p.quest.as_mut().unwrap().start_fn = nf),
        Box::new(move |p| p.quest.as_mut().unwrap().requires = vec![nf]),
        Box::new(|p| {
            let c = p.quest.as_ref().unwrap().roles[1].cond.unwrap();
            p.quest.as_mut().unwrap().start_fn = c;
        }),
        Box::new(|p| {
            let s = p.quest.as_ref().unwrap().start_fn;
            p.quest.as_mut().unwrap().roles[1].cond = Some(s);
        }),
        // an event type out of range, two events of one name, a handler disagreeing with its event's type
        Box::new(|p| p.events[0].ty = u16::MAX),
        Box::new(|p| {
            let e = p.events[0].clone();
            p.events.push(e);
        }),
        Box::new(|p| p.handlers[1].event_ty = 0),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut p = ok.clone();
        edit(&mut p);
        assert_ne!(p, ok, "edit {i} changed nothing");
        refused(&p);
    }
    // instructions naming an event that does not exist
    let mut p = ok.clone();
    p.events.clear();
    p.handlers[1].event = "Gone".into();
    refused(&p);
    // a non-canonical flag byte: the miller's `optional` (1) becomes 2
    let bytes = ok.to_bytes();
    // the role's name (a `u32` length, then the bytes; the global's name is longer), then global: u16,
    // ty: u16, optional: u8
    let at = bytes
        .windows(10)
        .position(|w| w == b"\x06\x00\x00\x00miller")
        .expect("the role's name")
        + 10;
    let mut b = bytes.clone();
    assert_eq!(b[at + 4], 1);
    b[at + 4] = 2;
    assert!(Program::from_bytes(&b).is_err());
}

#[test]
fn from_bytes_rejects_bad_input() {
    let bytes = full().to_bytes();
    let mut b = bytes.clone();
    b.push(0);
    assert!(Program::from_bytes(&b).is_err(), "trailing byte");
    let mut b = bytes.clone();
    b[0] = b'X';
    assert_eq!(Program::from_bytes(&b), Err(ProgramError::BadMagic));
    assert_eq!(
        Program::from_bytes(&bytes[..bytes.len() - 1]),
        Err(ProgramError::Truncated)
    );
    assert_eq!(
        Program::from_bytes(&bytes[..3]),
        Err(ProgramError::Truncated)
    );
    let mut b = bytes.clone();
    b[8] = 9; // bytecode version
    assert_eq!(Program::from_bytes(&b), Err(ProgramError::Version(9)));
    // a count far beyond the bytes: an error, not an allocation
    let mut b = bytes[..8 + 4 + 4 + 2 + 3].to_vec();
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    assert!(Program::from_bytes(&b).is_err());
}

#[test]
fn from_bytes_accepts_cyclic_and_deep_type_tables() {
    // Recursive data is allowed (decision 17): cycles and any nesting load; ids must be in range.
    let mut p = full();
    p.types.0.push(TypeDesc::List(p.types.0.len() as u16)); // itself
    assert!(Program::from_bytes(&p.to_bytes()).is_ok());
    let mut p = full();
    let n = p.types.0.len() as u16;
    p.types.0.push(TypeDesc::List(n + 1));
    p.types.0.push(TypeDesc::List(n)); // two-cycle
    assert!(Program::from_bytes(&p.to_bytes()).is_ok());
    let mut p = full();
    let n = p.types.0.len() as u16;
    p.types.0.push(TypeDesc::Int);
    for i in 0..40 {
        p.types.0.push(TypeDesc::List(n + i));
    }
    assert!(Program::from_bytes(&p.to_bytes()).is_ok());
    let mut p = full();
    p.types.0.push(TypeDesc::List(u16::MAX)); // out of range
    assert!(Program::from_bytes(&p.to_bytes()).is_err());
}

#[test]
fn queries_by_pc() {
    let p = full();
    let h = p.handlers[0].fn_id;
    let f = &p.fns[usize::from(h)];
    assert_eq!(p.fn_at(f.entry), Some(h));
    assert_eq!(p.fn_at(f.end), p.fn_at(f.end)); // the next function or none, never a panic
    assert_eq!(p.fn_at(u32::MAX), None);
    let loc = p.loc(f.entry);
    assert_eq!(
        (loc.file.as_str(), loc.line, loc.col),
        ("quests/a.quest", 9, 4)
    );
}

#[cfg(not(target_arch = "wasm32"))]
mod props {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::Config;

    fn check(bytes: &[u8]) {
        if let Ok(p) = Program::from_bytes(bytes) {
            p.validate().unwrap();
            assert_eq!(p.to_bytes(), bytes, "accepted bytes are canonical");
        }
    }

    proptest! {
        #![proptest_config(Config::with_cases(64))]

        #[test]
        fn from_bytes_never_panics(cut in 0usize..4096, flip in 0usize..4096, bit in 0u8..8,
                                   random in proptest::collection::vec(any::<u8>(), 0..256)) {
            let bytes = full().to_bytes();
            check(&bytes[..cut.min(bytes.len())]);
            let mut b = bytes.clone();
            let i = flip % b.len();
            b[i] ^= 1 << bit;
            check(&b);
            check(&random);
            let mut r = b"QSPROG01".to_vec();
            r.extend_from_slice(&random);
            check(&r);
        }
    }
}
