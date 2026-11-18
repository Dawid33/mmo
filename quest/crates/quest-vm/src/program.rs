//! A compiled program: its tables, its canonical byte form and its validation (design D5).
//!
//! `to_bytes` layout (little-endian, fixed-width, no `usize`, no padding left undefined): the magic
//! `QSPROG01`, `bytecode_version: u32`, `host_api_version: u32`, `static_words: u16`, `main_fn` (a `u8`
//! flag, then a `u16` that is 0 when the flag is 0), then each table as a `u32` count followed by its
//! elements, in the order of the struct's fields: `code` (8 bytes per instruction, see [`Instr::encode`]),
//! `consts`, `strings` (`u32` length + UTF-8), `shapes`, `types`, `fns`, `globals`, `handlers` (each
//! with its owner and an optional stage), `events`, then `quest` (a `u8` flag, then the [`QuestInfo`] when
//! it is 1), `host_sites`, `lines`, `files`. Nothing may follow. Every flag and optional is a `u8` 0 or 1
//! (an optional's value follows a 1 only). The encoding is canonical: bytes that parse are exactly
//! `to_bytes` of the program they parse to.
//!
//! `from_bytes` never allocates from an untrusted count (every element takes at least one byte, so a
//! count larger than the input runs out of bytes first) and ends with [`Program::validate`]; a program
//! that validates can be handed to the interpreter.

use crate::bytecode::{BYTECODE_VERSION, Builtin, Instr, Op, Operand, Wide};
use crate::consts::STRUCT_FIELDS_MAX;
use crate::hash::Fnv64;
use crate::host::{HostFnKind, SrcLoc};
use crate::object::FrameMaps;
use std::hash::Hasher;

use crate::types::{ERROR_ENUM_NAME, TypeDesc, TypeId, TypeTable};

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
    /// The instance whose handler it is: 0 the home (or a program without a quest), `1 + i` part `i`.
    /// An instance registers only its own (`VmConfig::instance`).
    pub owner: u16,
    /// The quest stage (an index into `QuestInfo::stages`) whose handler it is: a timer its task sets is
    /// stage-scoped (`CancelStageTimers`). `None` in the quest body or a program without a quest.
    pub stage: Option<u16>,
}

/// A script event (a quest's `event`): `Enqueue` and `After` name one by index; `deliver` takes its name
/// like a host event's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventInfo {
    pub name: String,
    pub ty: TypeId,
}

/// The program's quest (spec §5.12). Every `global` is an index into `Program::globals`, every fn an index
/// into `Program::fns`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestInfo {
    pub name: String,
    pub roles: Vec<RoleInfo>,
    /// The stage names, in source order; the first is the initial one.
    pub stages: Vec<String>,
    pub stage_global: u16,
    pub participants_global: u16,
    /// The role (an index into `roles`) whose region is the home's.
    pub home_role: u16,
    pub repeat: Option<RepeatInfo>,
    pub parts: Vec<PartInfo>,
    /// The `requires` conditions: pure fns of no parameters.
    pub requires: Vec<u16>,
    /// Enters the first stage: run once, after the initialisers and before `main`, in the home only.
    pub start_fn: u16,
    /// The globals dialogues read (strictly increasing).
    pub read_set: Vec<u16>,
}

/// A role: [`crate::vm::Vm::new`] writes its value (`VmConfig::roles`) into `global` before the
/// initialisers run; the global's own initialiser never runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleInfo {
    pub name: String,
    pub global: u16,
    /// The role's type `T`; the global's is `T`, or `Option[T]` for an optional role.
    pub ty: TypeId,
    pub optional: bool,
    /// The `where` condition: a pure fn taking the earlier roles, then this one.
    pub cond: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatInfo {
    pub cooldown: u64,
    pub exclusive: bool,
    pub once_per_player: bool,
}

/// A part of the quest: its own instance (`HandlerInfo::owner` `1 + i`), anchored to a role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartInfo {
    pub name: String,
    pub anchor: Anchor,
    /// An index into `QuestInfo::roles`.
    pub role: u16,
    /// The globals the part reads through its mirror (strictly increasing).
    pub read_set: Vec<u16>,
}

/// `part name on role` (the role's entity) or `part name at role` (the region where it was bound).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    On,
    At,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSite {
    pub host_fn: u16,
    pub kind: HostFnKind,
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    /// The errors a fallible query or command declares (names of `$Error` variants); empty when it cannot
    /// fail. A fallible site's `CallHost` writes the error word `c + 1` too.
    pub raises: Vec<String>,
}

impl HostSite {
    /// Whether the call can fail (it declares errors).
    pub fn fallible(&self) -> bool {
        !self.raises.is_empty()
    }
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
    pub events: Vec<EventInfo>,
    pub quest: Option<QuestInfo>,
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
    fn flag(&mut self, b: bool) {
        self.u8(u8::from(b));
    }
    /// A `u8` 1 and the value, or a `u8` 0.
    fn opt_u16(&mut self, v: Option<u16>) {
        self.flag(v.is_some());
        if let Some(v) = v {
            self.u16(v);
        }
    }
    fn u16s(&mut self, v: &[u16]) {
        self.count(v.len());
        for x in v {
            self.u16(*x);
        }
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
    /// A `u8` 0 or 1.
    fn flag(&mut self) -> Parse<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => invalid("a flag is not canonical"),
        }
    }
    fn opt_u16(&mut self) -> Parse<Option<u16>> {
        Ok(if self.flag()? {
            Some(self.u16()?)
        } else {
            None
        })
    }
    fn u16s(&mut self) -> Parse<Vec<u16>> {
        self.vec(|r| r.u16())
    }
}

fn write_quest(w: &mut Writer, q: &QuestInfo) {
    w.str(&q.name);
    w.count(q.roles.len());
    for r in &q.roles {
        w.str(&r.name);
        w.u16(r.global);
        w.u16(r.ty);
        w.flag(r.optional);
        w.opt_u16(r.cond);
    }
    w.count(q.stages.len());
    for s in &q.stages {
        w.str(s);
    }
    w.u16(q.stage_global);
    w.u16(q.participants_global);
    w.u16(q.home_role);
    w.flag(q.repeat.is_some());
    if let Some(r) = &q.repeat {
        w.u64(r.cooldown);
        w.flag(r.exclusive);
        w.flag(r.once_per_player);
    }
    w.count(q.parts.len());
    for p in &q.parts {
        w.str(&p.name);
        w.u8(match p.anchor {
            Anchor::On => 0,
            Anchor::At => 1,
        });
        w.u16(p.role);
        w.u16s(&p.read_set);
    }
    w.u16s(&q.requires);
    w.u16(q.start_fn);
    w.u16s(&q.read_set);
}

fn read_quest(r: &mut Reader) -> Parse<QuestInfo> {
    let name = r.str()?;
    let roles = r.vec_ids(|r| {
        Ok(RoleInfo {
            name: r.str()?,
            global: r.u16()?,
            ty: r.u16()?,
            optional: r.flag()?,
            cond: r.opt_u16()?,
        })
    })?;
    let stages = r.vec_ids(|r| r.str())?;
    let stage_global = r.u16()?;
    let participants_global = r.u16()?;
    let home_role = r.u16()?;
    let repeat = if r.flag()? {
        Some(RepeatInfo {
            cooldown: r.u64()?,
            exclusive: r.flag()?,
            once_per_player: r.flag()?,
        })
    } else {
        None
    };
    let parts = r.vec_ids(|r| {
        Ok(PartInfo {
            name: r.str()?,
            anchor: match r.u8()? {
                0 => Anchor::On,
                1 => Anchor::At,
                a => return invalid(format!("unknown part anchor {a}")),
            },
            role: r.u16()?,
            read_set: r.u16s()?,
        })
    })?;
    Ok(QuestInfo {
        name,
        roles,
        stages,
        stage_global,
        participants_global,
        home_role,
        repeat,
        parts,
        requires: r.u16s()?,
        start_fn: r.u16()?,
        read_set: r.u16s()?,
    })
}

/// Strictly increasing and every id below `n`.
fn is_id_set(set: &[u16], n: usize) -> bool {
    set.windows(2).all(|w| w[0] < w[1]) && set.iter().all(|&g| usize::from(g) < n)
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
            w.u16(h.owner);
            w.opt_u16(h.stage);
        }
        w.count(self.events.len());
        for e in &self.events {
            w.str(&e.name);
            w.u16(e.ty);
        }
        w.flag(self.quest.is_some());
        if let Some(q) = &self.quest {
            write_quest(&mut w, q);
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
            w.count(s.raises.len());
            for e in &s.raises {
                w.str(e);
            }
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
        let handlers = r.vec_ids(|r| {
            Ok(HandlerInfo {
                event: r.str()?,
                event_ty: r.u16()?,
                fn_id: r.u16()?,
                owner: r.u16()?,
                stage: r.opt_u16()?,
            })
        })?;
        let events = r.vec_ids(|r| {
            Ok(EventInfo {
                name: r.str()?,
                ty: r.u16()?,
            })
        })?;
        let quest = if r.flag()? {
            Some(read_quest(&mut r)?)
        } else {
            None
        };
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
                raises: r.vec(|r| r.str())?,
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
            events,
            quest,
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
            self.handlers.len(),
            self.events.len(),
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
            if f.ref_slots.windows(2).any(|w| w[0] >= w[1]) {
                return invalid(format!(
                    "fn {i} has ref slots that are not strictly increasing"
                ));
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
        self.validate_events_and_quest()?;
        for h in &self.handlers {
            if usize::from(h.event_ty) >= ntypes || usize::from(h.fn_id) >= nfns {
                return invalid(format!("handler {} is out of range", h.event));
            }
            let (parts, stages) = self
                .quest
                .as_ref()
                .map_or((0, 0), |q| (q.parts.len(), q.stages.len()));
            if usize::from(h.owner) > parts || h.stage.is_some_and(|s| usize::from(s) >= stages) {
                return invalid(format!(
                    "handler {}: its owner or stage is out of range",
                    h.event
                ));
            }
            if let Some(e) = self.events.iter().find(|e| e.name == h.event)
                && e.ty != h.event_ty
            {
                return invalid(format!(
                    "handler {}: its event type is not the script event's",
                    h.event
                ));
            }
            // Dispatch writes the event into parameter 0 (frame word 3).
            let f = &self.fns[usize::from(h.fn_id)];
            if f.params != 1 {
                return invalid(format!(
                    "handler {} must take exactly one parameter",
                    h.event
                ));
            }
            if f.ref_slots.binary_search(&3).is_ok() != self.types.is_ref(h.event_ty) {
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
            if s.fallible() {
                if s.kind == HostFnKind::Promise {
                    return invalid(format!("host site {i} is a promise that declares errors"));
                }
                // Every declared error is a variant of the program's `$Error`.
                let errors = self.types.0.iter().find_map(|d| match d {
                    TypeDesc::Enum { name, variants } if name == ERROR_ENUM_NAME => Some(variants),
                    _ => None,
                });
                let known = |e: &String| errors.is_some_and(|v| v.iter().any(|(n, _)| n == e));
                if !s.raises.iter().all(known) {
                    return invalid(format!("host site {i} raises an error the program lacks"));
                }
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

    /// The script events (types in range, names unique) and the quest: every global, role, stage, part and
    /// fn it names in range; fns of the arity their use needs; role globals of the role's type.
    fn validate_events_and_quest(&self) -> Result<(), ProgramError> {
        let (ntypes, nglobals) = (self.types.0.len(), self.globals.len());
        let mut names: Vec<&str> = Vec::with_capacity(self.events.len());
        for e in &self.events {
            if usize::from(e.ty) >= ntypes {
                return invalid(format!("script event {} names a type out of range", e.name));
            }
            names.push(&e.name);
        }
        names.sort_unstable();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return invalid("two script events share a name");
        }
        let Some(q) = &self.quest else {
            return Ok(());
        };
        let fn_of = |f: u16, params: usize, what: &str| match self.fns.get(usize::from(f)) {
            Some(i) if usize::from(i.params) == params => Ok(()),
            _ => invalid(format!(
                "quest {}: {what} is not a fn of {params} parameters",
                q.name
            )),
        };
        let mut role_names: Vec<&str> = Vec::new();
        let mut role_globals: Vec<u16> = Vec::new();
        for (i, r) in q.roles.iter().enumerate() {
            let Some(g) = self.globals.get(usize::from(r.global)) else {
                return invalid(format!("role {}: its global is out of range", r.name));
            };
            if usize::from(r.ty) >= ntypes || !self.role_global_fits(r, g.ty) {
                return invalid(format!(
                    "role {}: its global's type is not the role's",
                    r.name
                ));
            }
            if let Some(c) = r.cond {
                fn_of(c, i + 1, "a role's condition")?;
            }
            role_names.push(&r.name);
            role_globals.push(r.global);
        }
        role_names.sort_unstable();
        role_globals.sort_unstable();
        if role_names.windows(2).any(|w| w[0] == w[1])
            || role_globals.windows(2).any(|w| w[0] == w[1])
        {
            return invalid("two roles share a name or a global");
        }
        if usize::from(q.stage_global) >= nglobals || usize::from(q.participants_global) >= nglobals
        {
            return invalid(format!("quest {}: a global is out of range", q.name));
        }
        if usize::from(q.home_role) >= q.roles.len().max(1) {
            return invalid(format!("quest {}: the home role is out of range", q.name));
        }
        if !is_id_set(&q.read_set, nglobals) {
            return invalid(format!("quest {}: the read set is malformed", q.name));
        }
        for p in &q.parts {
            if usize::from(p.role) >= q.roles.len() || !is_id_set(&p.read_set, nglobals) {
                return invalid(format!(
                    "part {}: its role or read set is malformed",
                    p.name
                ));
            }
        }
        for &f in &q.requires {
            fn_of(f, 0, "a requirement")?;
        }
        fn_of(q.start_fn, 0, "the start")
    }

    /// Whether a role's global holds its type: `T`, or for an optional role `Option[T]` (the enum
    /// `Option` of `None` and `Some` with one field of type `T`).
    fn role_global_fits(&self, r: &RoleInfo, global_ty: TypeId) -> bool {
        if !r.optional {
            return global_ty == r.ty;
        }
        match self.types.0.get(usize::from(global_ty)) {
            Some(TypeDesc::Enum { name, variants }) if name == "Option" => matches!(
                variants.as_slice(),
                [(none, n), (some, s)]
                    if none == "None" && n.is_empty() && some == "Some"
                        && matches!(s.as_slice(), [(_, t)] if *t == r.ty)
            ),
            _ => false,
        }
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
                Operand::Event => usize::from(v) < self.events.len(),
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
            // the event value follows the ticks
            Op::After if u32::from(ins.b) + 1 >= static_words => {
                Err("After's value slot is outside the static region".into())
            }
            Op::CallBuiltin => match Builtin::from_u16(ins.a).map(Builtin::arity) {
                Some(n) if u32::from(ins.b) + u32::from(n) > static_words => {
                    Err("CallBuiltin arguments run outside the static region".into())
                }
                _ => Ok(()),
            },
            Op::CallHost => {
                let site = &self.host_sites[usize::from(ins.a)];
                let n = site.params.len() as u64;
                if u64::from(ins.b) + n > u64::from(static_words) {
                    Err("CallHost arguments run outside the static region".into())
                } else if site.fallible() && u32::from(ins.c) + 1 >= static_words {
                    // the error word follows the result word
                    Err("CallHost's error slot is outside the static region".into())
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

/// Every id in range and field counts within `STRUCT_FIELDS_MAX`. Cycles and any nesting are allowed:
/// recursive data types are (decision 17).
fn validate_types(t: &TypeTable) -> Result<(), ProgramError> {
    let n = t.0.len();
    if n > usize::from(u16::MAX) + 1 {
        return invalid("too many types");
    }
    for (i, d) in t.0.iter().enumerate() {
        if type_children(d).iter().any(|&c| usize::from(c) >= n) {
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
    Ok(())
}
