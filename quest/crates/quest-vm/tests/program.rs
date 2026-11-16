//! The instruction set, the program format, its validation and the assembler (design D5, D6, D9).

use quest_vm::asm::ProgramBuilder;
use quest_vm::bytecode::{BYTECODE_VERSION, Builtin, Instr, Op};
use quest_vm::fuel::*;
use quest_vm::host::HostFnKind;
use quest_vm::program::{Program, ProgramError};
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
    ];
    assert_eq!(ops.len(), Op::ALL.len());
    for &(op, n) in ops {
        assert_eq!(op as u8, n, "{op:?}");
        assert_eq!(Op::from_u8(n), Some(op));
    }
    assert_eq!(Op::from_u8(64), None);
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
        (StrConcatN, 16),
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
    ];
    assert_eq!(bs.len(), Builtin::ALL.len());
    for &(b, n) in bs {
        assert_eq!(b as u16, n, "{b:?}");
        assert_eq!(Builtin::from_u16(n), Some(b));
    }
    assert_eq!(Builtin::from_u16(41), None);
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
}

/// A valid program: `main` (fn 0) calls `callee` (fn 1); one `Nop` in each function is room for `poke`.
fn base() -> Program {
    let mut pb = ProgramBuilder::new(7);
    let int = pb.ty(TypeDesc::Int);
    let main = pb.declare("main", 0, 2);
    let callee = pb.declare("callee", 1, 1);
    let mut f = pb.body(main);
    f.line(1);
    f.emit(Op::Nop, 0, 0, 0);
    f.emit(Op::LoadSmall, f.local(0), 5, 0);
    let ok = f.label();
    f.jump(Op::JmpIfNot, f.local(0), ok);
    f.emit(Op::Copy, 0, 0, 0);
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
    pb.main(0);
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
    let init = pb.declare("m::init_g", 0, 1);
    let g = pb.global("m::g", list);
    pb.set_global_init(g, init);
    let handler = pb.declare("m::on_click", 1, 3);
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
    h.emit(Op::Ret, 0, 0, 0);
    h.end();
    pb.handler("PlayerClicked", en, handler);
    pb.main(handler);
    let _ = (dict, g);
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
fn from_bytes_rejects_cyclic_and_too_deep_type_tables() {
    let mut p = full();
    p.types.0.push(TypeDesc::List(p.types.0.len() as u16)); // itself
    let b = p.to_bytes();
    assert!(Program::from_bytes(&b).is_err());
    let mut p = full();
    let n = p.types.0.len() as u16;
    p.types.0.push(TypeDesc::List(n + 1));
    p.types.0.push(TypeDesc::List(n)); // two-cycle
    assert!(Program::from_bytes(&p.to_bytes()).is_err());
    // deeper than WIRE_DEPTH_MAX
    let mut p = full();
    let n = p.types.0.len() as u16;
    p.types.0.push(TypeDesc::Int);
    for i in 0..40 {
        p.types.0.push(TypeDesc::List(n + i));
    }
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
