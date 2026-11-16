//! A compiled program: its tables, its canonical byte form and its validation (design D5).
//!
//! `to_bytes` layout (little-endian, fixed-width, no `usize`, no padding left undefined): the magic
//! `QSPROG01`, `bytecode_version: u32`, `host_api_version: u32`, `static_words: u16`, `main_fn` (a `u8`
//! flag, then a `u16` that is 0 when the flag is 0), then each table as a `u32` count followed by its
//! elements, in the order of the struct's fields: `code` (8 bytes per instruction, see [`Instr::encode`]),
//! `consts`, `strings` (`u32` length + UTF-8), `shapes`, `types`, `fns`, `globals`, `handlers`,
//! `host_sites`, `lines`, `files`. Nothing may follow. The encoding is canonical: bytes that parse are
//! exactly `to_bytes` of the program they parse to.
//!
//! `from_bytes` never allocates from an untrusted count (every element takes at least one byte, so a
//! count larger than the input runs out of bytes first) and ends with [`Program::validate`]; a program
//! that validates can be handed to the interpreter.

use crate::bytecode::{Arity, BYTECODE_VERSION, Builtin, Instr, Op, Operand, Wide};
use crate::consts::{STRUCT_FIELDS_MAX, WIRE_DEPTH_MAX};
use crate::hash::Fnv64;
use crate::host::{HostFnKind, SrcLoc};
use crate::object::FrameMaps;
use std::hash::Hasher;

use crate::types::{TypeDesc, TypeId, TypeTable};

/// Most elements of a table indexed by a `u16` id.
const ID_TABLE_MAX: u32 = 65_536;

pub const MAGIC: [u8; 8] = *b"QSPROG01";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramError {
    Truncated,
    BadMagic,
    Version(u32),
    Invalid(String),
}

impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProgramError::Truncated => f.write_str("program bytes end early"),
            ProgramError::BadMagic => f.write_str("not a quest program"),
            ProgramError::Version(v) => write!(f, "bytecode version {v} is not supported"),
            ProgramError::Invalid(why) => write!(f, "invalid program: {why}"),
        }
    }
}

impl std::error::Error for ProgramError {}

fn invalid<T>(why: impl Into<String>) -> Result<T, ProgramError> {
    Err(ProgramError::Invalid(why.into()))
}

/// The object layout of one `NewObj` shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub fields: u8,
    /// Bit `i` set: field `i` holds a heap kind.
    pub ref_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnInfo {
    pub name: String,
    /// First instruction.
    pub entry: u32,
    /// One past the last instruction.
    pub end: u32,
    /// Parameter slots, at frame words `3 .. 3 + params`.
    pub params: u8,
    pub frame_base: u16,
    pub frame_words: u16,
    /// Frame-relative words holding heap kinds.
    pub ref_slots: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobalInfo {
    /// Qualified name.
    pub name: String,
    pub slot: u16,
    pub ty: TypeId,
    pub init_fn: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerInfo {
    pub event: String,
    pub event_ty: TypeId,
    pub fn_id: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSite {
    pub host_fn: u16,
    pub kind: HostFnKind,
    pub params: Vec<TypeId>,
    pub ret: TypeId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineEntry {
    pub pc: u32,
    pub file: u16,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub bytecode_version: u32,
    pub host_api_version: u32,
    pub code: Vec<Instr>,
    pub consts: Vec<u64>,
    pub strings: Vec<String>,
    pub shapes: Vec<Shape>,
    pub types: TypeTable,
    pub fns: Vec<FnInfo>,
    pub globals: Vec<GlobalInfo>,
    pub handlers: Vec<HandlerInfo>,
    pub main_fn: Option<u16>,
    pub host_sites: Vec<HostSite>,
    pub lines: Vec<LineEntry>,
    pub files: Vec<String>,
    pub static_words: u16,
}

impl FrameMaps for Program {
    fn ref_slots(&self, fn_id: u16) -> &[u16] {
        self.fns
            .get(usize::from(fn_id))
            .map_or(&[][..], |f| f.ref_slots.as_slice())
    }
}

// ---- bytes -------------------------------------------------------------------------------------------

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// A table count. A table cannot have more than `u32::MAX` elements and still exist in memory on
    /// the targets that run this; the saturated value would fail to parse, never alias another program.
    fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn str(&mut self, s: &str) {
        self.count(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

type Parse<T> = Result<T, ProgramError>;

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Parse<&'a [u8]> {
        let left = self.bytes.len() - self.pos;
        if n > left {
            return Err(ProgramError::Truncated);
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn array<const N: usize>(&mut self) -> Parse<[u8; N]> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u8(&mut self) -> Parse<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Parse<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Parse<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Parse<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn count(&mut self) -> Parse<u32> {
        self.u32()
    }
    fn str(&mut self) -> Parse<String> {
        let n = usize::try_from(self.u32()?).map_err(|_| ProgramError::Truncated)?;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).or_else(|_| invalid("a string is not UTF-8"))
    }
    /// `n` elements read by `f`. No capacity is reserved from `n`.
    fn vec<T>(&mut self, f: impl FnMut(&mut Self) -> Parse<T>) -> Parse<Vec<T>> {
        let n = self.count()?;
        self.vec_n(n, f)
    }
    fn vec_n<T>(&mut self, n: u32, mut f: impl FnMut(&mut Self) -> Parse<T>) -> Parse<Vec<T>> {
        let mut v = Vec::new();
        for _ in 0..n {
            v.push(f(self)?);
        }
        Ok(v)
    }
    /// Like `vec`, for tables whose ids are 16 bits wide: a longer table is refused before it is read.
    fn vec_ids<T>(&mut self, f: impl FnMut(&mut Self) -> Parse<T>) -> Parse<Vec<T>> {
        let n = self.u32()?;
        if n > ID_TABLE_MAX {
            return invalid("a table is longer than its ids can address");
        }
        self.vec_n(n, f)
    }
    fn named_ids(&mut self) -> Parse<Vec<(String, TypeId)>> {
        self.vec(|r| Ok((r.str()?, r.u16()?)))
    }
}

fn write_named_ids(w: &mut Writer, fields: &[(String, TypeId)]) {
    w.count(fields.len());
    for (n, t) in fields {
        w.str(n);
        w.u16(*t);
    }
}

fn write_type(w: &mut Writer, d: &TypeDesc) {
    match d {
        TypeDesc::Unit => w.u8(0),
        TypeDesc::Bool => w.u8(1),
        TypeDesc::Int => w.u8(2),
        TypeDesc::Float => w.u8(3),
        TypeDesc::Str => w.u8(4),
        TypeDesc::List(e) => {
            w.u8(5);
            w.u16(*e);
        }
        TypeDesc::Dict(k, v) => {
            w.u8(6);
            w.u16(*k);
            w.u16(*v);
        }
        TypeDesc::Promise(t) => {
            w.u8(7);
            w.u16(*t);
        }
        TypeDesc::Struct { name, fields } => {
            w.u8(8);
            w.str(name);
            write_named_ids(w, fields);
        }
        TypeDesc::Enum { name, variants } => {
            w.u8(9);
            w.str(name);
            w.count(variants.len());
            for (v, fields) in variants {
                w.str(v);
                write_named_ids(w, fields);
            }
        }
    }
}

fn read_type(r: &mut Reader) -> Parse<TypeDesc> {
    Ok(match r.u8()? {
        0 => TypeDesc::Unit,
        1 => TypeDesc::Bool,
        2 => TypeDesc::Int,
        3 => TypeDesc::Float,
        4 => TypeDesc::Str,
        5 => TypeDesc::List(r.u16()?),
        6 => TypeDesc::Dict(r.u16()?, r.u16()?),
        7 => TypeDesc::Promise(r.u16()?),
        8 => TypeDesc::Struct {
            name: r.str()?,
            fields: r.named_ids()?,
        },
        9 => TypeDesc::Enum {
            name: r.str()?,
            variants: r.vec(|r| Ok((r.str()?, r.named_ids()?)))?,
        },
        t => return invalid(format!("unknown type tag {t}")),
    })
}

fn kind_byte(k: HostFnKind) -> u8 {
    match k {
        HostFnKind::Query => 0,
        HostFnKind::Command => 1,
        HostFnKind::Promise => 2,
    }
}

impl Program {
    /// The canonical byte form (see the module docs).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(&MAGIC);
        w.u32(self.bytecode_version);
        w.u32(self.host_api_version);
        w.u16(self.static_words);
        match self.main_fn {
            Some(f) => {
                w.u8(1);
                w.u16(f);
            }
            None => {
                w.u8(0);
                w.u16(0);
            }
        }
        w.count(self.code.len());
        for i in &self.code {
            w.0.extend_from_slice(&i.encode());
        }
        w.count(self.consts.len());
        for c in &self.consts {
            w.u64(*c);
        }
        w.count(self.strings.len());
        for s in &self.strings {
            w.str(s);
        }
        w.count(self.shapes.len());
        for s in &self.shapes {
            w.u8(s.fields);
            w.u32(s.ref_bits);
        }
        w.count(self.types.0.len());
        for d in &self.types.0 {
            write_type(&mut w, d);
        }
        w.count(self.fns.len());
        for f in &self.fns {
            w.str(&f.name);
            w.u32(f.entry);
            w.u32(f.end);
            w.u8(f.params);
            w.u16(f.frame_base);
            w.u16(f.frame_words);
            w.count(f.ref_slots.len());
            for s in &f.ref_slots {
                w.u16(*s);
            }
        }
        w.count(self.globals.len());
        for g in &self.globals {
            w.str(&g.name);
            w.u16(g.slot);
            w.u16(g.ty);
            w.u16(g.init_fn);
        }
        w.count(self.handlers.len());
        for h in &self.handlers {
            w.str(&h.event);
            w.u16(h.event_ty);
            w.u16(h.fn_id);
        }
        w.count(self.host_sites.len());
        for s in &self.host_sites {
            w.u16(s.host_fn);
            w.u8(kind_byte(s.kind));
            w.count(s.params.len());
            for p in &s.params {
                w.u16(*p);
            }
            w.u16(s.ret);
        }
        w.count(self.lines.len());
        for l in &self.lines {
            w.u32(l.pc);
            w.u16(l.file);
            w.u32(l.line);
            w.u32(l.col);
        }
        w.count(self.files.len());
        for f in &self.files {
            w.str(f);
        }
        w.0
    }

    /// Parses `bytes` and validates the result. Untrusted input: any failure is an `Err`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Program, ProgramError> {
        let mut r = Reader { bytes, pos: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            return Err(ProgramError::BadMagic);
        }
        let bytecode_version = r.u32()?;
        if bytecode_version != BYTECODE_VERSION {
            return Err(ProgramError::Version(bytecode_version));
        }
        let host_api_version = r.u32()?;
        let static_words = r.u16()?;
        let main_fn = match (r.u8()?, r.u16()?) {
            (0, 0) => None,
            (1, f) => Some(f),
            _ => return invalid("main_fn is not canonical"),
        };
        let code =
            r.vec(|r| Instr::decode(r.array()?).map_or_else(|| invalid("not an instruction"), Ok))?;
        let consts = r.vec(|r| r.u64())?;
        let strings = r.vec(|r| r.str())?;
        let shapes = r.vec_ids(|r| {
            Ok(Shape {
                fields: r.u8()?,
                ref_bits: r.u32()?,
            })
        })?;
        let types = TypeTable(r.vec_ids(read_type)?);
        let fns = r.vec_ids(|r| {
            Ok(FnInfo {
                name: r.str()?,
                entry: r.u32()?,
                end: r.u32()?,
                params: r.u8()?,
                frame_base: r.u16()?,
                frame_words: r.u16()?,
                ref_slots: r.vec(|r| r.u16())?,
            })
        })?;
        let globals = r.vec_ids(|r| {
            Ok(GlobalInfo {
                name: r.str()?,
                slot: r.u16()?,
                ty: r.u16()?,
                init_fn: r.u16()?,
            })
        })?;
        let handlers = r.vec(|r| {
            Ok(HandlerInfo {
                event: r.str()?,
                event_ty: r.u16()?,
                fn_id: r.u16()?,
            })
        })?;
        let host_sites = r.vec_ids(|r| {
            let host_fn = r.u16()?;
            let kind = match r.u8()? {
                0 => HostFnKind::Query,
                1 => HostFnKind::Command,
                2 => HostFnKind::Promise,
                k => return invalid(format!("unknown host fn kind {k}")),
            };
            Ok(HostSite {
                host_fn,
                kind,
                params: r.vec(|r| r.u16())?,
                ret: r.u16()?,
            })
        })?;
        let lines = r.vec(|r| {
            Ok(LineEntry {
                pc: r.u32()?,
                file: r.u16()?,
                line: r.u32()?,
                col: r.u32()?,
            })
        })?;
        let files = r.vec(|r| r.str())?;
        if r.pos != bytes.len() {
            return invalid("trailing bytes");
        }
        let p = Program {
            bytecode_version,
            host_api_version,
            code,
            consts,
            strings,
            shapes,
            types,
            fns,
            globals,
            handlers,
            main_fn,
            host_sites,
            lines,
            files,
            static_words,
        };
        p.validate()?;
        Ok(p)
    }

    /// FNV-1a 64 of [`Program::to_bytes`].
    pub fn content_hash(&self) -> u64 {
        {
            let mut h = Fnv64::default();
            h.write(&self.to_bytes());
            h.finish()
        }
    }

    /// The source position of `pc` (the last line entry at or before it); empty when there is none.
    pub fn loc(&self, pc: u32) -> SrcLoc {
        let n = self.lines.partition_point(|l| l.pc <= pc);
        match n.checked_sub(1).and_then(|i| self.lines.get(i)) {
            Some(l) => SrcLoc {
                file: self
                    .files
                    .get(usize::from(l.file))
                    .cloned()
                    .unwrap_or_default(),
                line: l.line,
                col: l.col,
            },
            None => SrcLoc {
                file: String::new(),
                line: 0,
                col: 0,
            },
        }
    }

    /// The function whose code contains `pc`.
    pub fn fn_at(&self, pc: u32) -> Option<u16> {
        let i = self.fns.iter().position(|f| f.entry <= pc && pc < f.end)?;
        u16::try_from(i).ok()
    }

    /// Everything the interpreter relies on without checking again (see the module docs of `bytecode`
    /// for operand meanings): operand indexes in range, jumps inside their function, disjoint frames and
    /// code, no recursion, every function ends in `Ret` or `Jmp`, well-formed type table.
    pub fn validate(&self) -> Result<(), ProgramError> {
        if self.bytecode_version != BYTECODE_VERSION {
            return Err(ProgramError::Version(self.bytecode_version));
        }
        // Cheap bounds first: every table indexed by a 16-bit id, and the static region's size.
        let id_tables = [
            self.types.0.len(),
            self.fns.len(),
            self.shapes.len(),
            self.host_sites.len(),
            self.globals.len(),
        ];
        if id_tables.iter().any(|&n| n > ID_TABLE_MAX as usize) {
            return invalid("a table is longer than its ids can address");
        }
        validate_types(&self.types)?;
        let ntypes = self.types.0.len();
        let static_words = u32::from(self.static_words);
        let nfns = self.fns.len();
        let ncode = self.code.len();

        for (i, s) in self.shapes.iter().enumerate() {
            if usize::from(s.fields) > STRUCT_FIELDS_MAX
                || (s.fields < 32 && s.ref_bits >> s.fields != 0)
            {
                return invalid(format!("shape {i} is malformed"));
            }
        }

        // functions: ranges, frames
        let mut covered = 0u64;
        for (i, f) in self.fns.iter().enumerate() {
            if f.entry >= f.end || usize::try_from(f.end).map_or(true, |e| e > ncode) {
                return invalid(format!("fn {i} has no code range inside the code"));
            }
            covered += u64::from(f.end - f.entry);
            if u32::from(f.frame_words) < 3 + u32::from(f.params)
                || u32::from(f.frame_base) + u32::from(f.frame_words) > static_words
            {
                return invalid(format!("fn {i} has a frame outside the static region"));
            }
            if f.ref_slots.iter().any(|&s| s >= f.frame_words) {
                return invalid(format!("fn {i} has a ref slot outside its frame"));
            }
        }
        let mut by_code: Vec<(u32, u32)> = self.fns.iter().map(|f| (f.entry, f.end)).collect();
        by_code.sort_unstable();
        if by_code.windows(2).any(|w| w[0].1 > w[1].0) {
            return invalid("functions' code ranges overlap");
        }
        if covered != ncode as u64 {
            return invalid("code outside every function");
        }
        let mut by_frame: Vec<(u32, u32)> = self
            .fns
            .iter()
            .map(|f| {
                (
                    u32::from(f.frame_base),
                    u32::from(f.frame_base) + u32::from(f.frame_words),
                )
            })
            .collect();
        by_frame.sort_unstable();
        if by_frame.windows(2).any(|w| w[0].1 > w[1].0) {
            return invalid("frames overlap");
        }

        // Frames are the first words of the static region, then the globals (the interpreter treats
        // `[0, frames_end)` as the frames): every global lies at or above the end of the last frame.
        let frames_end = by_frame.last().map_or(0, |f| f.1);
        let mut slots = Vec::with_capacity(self.globals.len());
        for g in &self.globals {
            if u32::from(g.slot) >= static_words
                || u32::from(g.slot) < frames_end
                || usize::from(g.ty) >= ntypes
                || usize::from(g.init_fn) >= nfns
            {
                return invalid(format!(
                    "global {} is out of range or below the frames",
                    g.name
                ));
            }
            if self.fns[usize::from(g.init_fn)].params != 0 {
                return invalid(format!("the initialiser of {} takes parameters", g.name));
            }
            slots.push(g.slot);
        }
        slots.sort_unstable();
        if slots.windows(2).any(|w| w[0] == w[1]) {
            return invalid("two globals share a slot");
        }
        for h in &self.handlers {
            if usize::from(h.event_ty) >= ntypes || usize::from(h.fn_id) >= nfns {
                return invalid(format!("handler {} is out of range", h.event));
            }
            // Dispatch writes the event into parameter 0 (frame word 3).
            let f = &self.fns[usize::from(h.fn_id)];
            if f.params != 1 {
                return invalid(format!(
                    "handler {} must take exactly one parameter",
                    h.event
                ));
            }
            if f.ref_slots.contains(&3) != self.types.is_ref(h.event_ty) {
                return invalid(format!(
                    "handler {}: its event parameter's ref slot disagrees with the event type",
                    h.event
                ));
            }
        }
        if let Some(m) = self.main_fn {
            match self.fns.get(usize::from(m)) {
                Some(f) if f.params == 0 => {}
                _ => return invalid("main_fn is out of range or takes parameters"),
            }
        }
        for (i, s) in self.host_sites.iter().enumerate() {
            if usize::from(s.ret) >= ntypes || s.params.iter().any(|&p| usize::from(p) >= ntypes) {
                return invalid(format!("host site {i} names a type out of range"));
            }
        }
        for w in self.lines.windows(2) {
            if w[0].pc > w[1].pc {
                return invalid("line table is not sorted by pc");
            }
        }
        for l in &self.lines {
            if usize::from(l.file) >= self.files.len()
                || usize::try_from(l.pc).map_or(true, |p| p > ncode)
            {
                return invalid("line entry out of range");
            }
        }

        // code
        let mut calls: Vec<Vec<u16>> = vec![Vec::new(); nfns];
        for (fi, f) in self.fns.iter().enumerate() {
            let (lo, hi) = (f.entry as usize, f.end as usize);
            for (off, ins) in self.code[lo..hi].iter().enumerate() {
                let pc = lo + off;
                self.check_instr(ins, f)
                    .or_else(|why| invalid(format!("pc {pc}: {why}")))?;
                if ins.op == Op::Call {
                    calls[fi].push(ins.a);
                }
            }
            if !matches!(self.code[hi - 1].op, Op::Ret | Op::Jmp) {
                return invalid(format!("fn {fi} does not end in Ret or Jmp"));
            }
        }
        if has_cycle(&calls) {
            return invalid("the call graph has a cycle (frames are static: no recursion)");
        }
        Ok(())
    }

    fn check_instr(&self, ins: &Instr, f: &FnInfo) -> Result<(), String> {
        let lay = ins.op.layout();
        let static_words = u32::from(self.static_words);
        for (kind, v) in [(lay.a, ins.a), (lay.b, ins.b), (lay.c, ins.c)] {
            let ok = match kind {
                Operand::Unused | Operand::Imm => true,
                Operand::Slot => u32::from(v) < static_words,
                Operand::FnId => usize::from(v) < self.fns.len(),
                Operand::Shape => usize::from(v) < self.shapes.len(),
                Operand::Type => usize::from(v) < self.types.0.len(),
                Operand::Builtin => Builtin::from_u16(v).is_some(),
                Operand::HostSite => usize::from(v) < self.host_sites.len(),
            };
            if !ok {
                return Err(format!(
                    "{:?} operand {v} ({kind:?}) is out of range",
                    ins.op
                ));
            }
        }
        let t = ins.t();
        let ok = match lay.wide {
            Wide::None => true,
            Wide::Target => f.entry <= t && t < f.end,
            Wide::Const => usize::try_from(t).is_ok_and(|t| t < self.consts.len()),
            Wide::Str => usize::try_from(t).is_ok_and(|t| t < self.strings.len()),
        };
        if !ok {
            return Err(format!(
                "{:?} operand {t} ({:?}) is out of range",
                ins.op, lay.wide
            ));
        }
        match ins.op {
            // the error word follows the value word
            Op::Await | Op::AwaitAll if u32::from(ins.a) + 1 >= static_words => {
                Err("Await's error slot is outside the static region".into())
            }
            Op::CallBuiltin => match Builtin::from_u16(ins.a).map(Builtin::arity) {
                Some(Arity::Fixed(n)) if u32::from(ins.b) + u32::from(n) > static_words => {
                    Err("CallBuiltin arguments run outside the static region".into())
                }
                _ => Ok(()),
            },
            Op::CallHost => {
                let n = self.host_sites[usize::from(ins.a)].params.len() as u64;
                if u64::from(ins.b) + n > u64::from(static_words) {
                    Err("CallHost arguments run outside the static region".into())
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

/// Iterative three-colour DFS: a hostile program may have 65 536 functions in a chain.
fn has_cycle(edges: &[Vec<u16>]) -> bool {
    let mut state = vec![0u8; edges.len()]; // 0 new, 1 on the stack, 2 done
    for start in 0..edges.len() {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        let mut stack = vec![(start, 0usize)];
        while let Some(&mut (n, ref mut next)) = stack.last_mut() {
            if let Some(&m) = edges[n].get(*next) {
                *next += 1;
                let m = usize::from(m);
                match state.get(m) {
                    Some(1) => return true,
                    Some(0) => {
                        state[m] = 1;
                        stack.push((m, 0));
                    }
                    _ => {}
                }
            } else {
                state[n] = 2;
                stack.pop();
            }
        }
    }
    false
}

fn type_children(d: &TypeDesc) -> Vec<TypeId> {
    match d {
        TypeDesc::Unit | TypeDesc::Bool | TypeDesc::Int | TypeDesc::Float | TypeDesc::Str => vec![],
        TypeDesc::List(e) | TypeDesc::Promise(e) => vec![*e],
        TypeDesc::Dict(k, v) => vec![*k, *v],
        TypeDesc::Struct { fields, .. } => fields.iter().map(|f| f.1).collect(),
        TypeDesc::Enum { variants, .. } => variants
            .iter()
            .flat_map(|v| v.1.iter().map(|f| f.1))
            .collect(),
    }
}

/// Every id in range, field counts within `STRUCT_FIELDS_MAX`, no cycle (user types cannot be
/// recursive), nesting at most `WIRE_DEPTH_MAX`.
fn validate_types(t: &TypeTable) -> Result<(), ProgramError> {
    let n = t.0.len();
    if n > usize::from(u16::MAX) + 1 {
        return invalid("too many types");
    }
    let kids: Vec<Vec<TypeId>> = t.0.iter().map(type_children).collect();
    for (i, d) in t.0.iter().enumerate() {
        if kids[i].iter().any(|&c| usize::from(c) >= n) {
            return invalid(format!("type {i} names a type out of range"));
        }
        let too_many = match d {
            TypeDesc::Struct { fields, .. } => fields.len() > STRUCT_FIELDS_MAX,
            TypeDesc::Enum { variants, .. } => {
                variants.iter().any(|v| v.1.len() > STRUCT_FIELDS_MAX)
            }
            _ => false,
        };
        if too_many {
            return invalid(format!("type {i} has too many fields"));
        }
    }
    // post-order; height = nesting below
    let mut state = vec![0u8; n];
    let mut height = vec![0usize; n];
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        let mut stack = vec![(start, 0usize)];
        while let Some(&mut (x, ref mut next)) = stack.last_mut() {
            if let Some(&c) = kids[x].get(*next) {
                *next += 1;
                let c = usize::from(c);
                match state[c] {
                    1 => return invalid("the type table has a cycle"),
                    0 => {
                        state[c] = 1;
                        stack.push((c, 0));
                    }
                    _ => {}
                }
            } else {
                let h = kids[x]
                    .iter()
                    .map(|&c| height[usize::from(c)] + 1)
                    .max()
                    .unwrap_or(0);
                if h > WIRE_DEPTH_MAX {
                    return invalid("a type nests deeper than WIRE_DEPTH_MAX");
                }
                height[x] = h;
                state[x] = 2;
                stack.pop();
            }
        }
    }
    Ok(())
}
