//! The assembler: builds a [`Program`] for hand-written test programs and for the compiler's code
//! generator.
//!
//! Slots are **symbolic** while code is emitted ([`SlotRef`]) and become absolute words in
//! [`ProgramBuilder::finish`]: frames laid out in function-id (declaration) order from slot 0, then the
//! globals in declaration order. A frame is `3 + params + locals` words (word 0 return link, word 1 return
//! value, word 2 error value, then the parameters, then locals and temporaries); `locals` is one more than
//! the highest local used, so nobody has to know a frame's size up front. A function is declared
//! ([`ProgramBuilder::declare`], giving a [`FnRef`]) before or after the code that calls it; its body is
//! emitted through a [`FnBuilder`] and closed with [`FnBuilder::end`].
//!
//! Each of `a`, `b`, `c` of an instruction is a slot ([`SlotRef`]) or an immediate (`u16`), as
//! [`Op::layout`] says; a mismatch is remembered as an error. Errors found while building (a static region
//! over 65 535 words, an unbound label, a misused slot) are returned by `finish`, which also runs
//! [`Program::validate`].

use std::collections::{BTreeMap, BTreeSet};

use crate::bytecode::{BYTECODE_VERSION, Builtin, Instr, Op, Operand as Kind, Wide};
use crate::dialogue::DialogueGraph;
use crate::host::HostFnKind;
use crate::program::{
    EventInfo, FnInfo, FnSig, GlobalInfo, HandlerInfo, HostSite, LineEntry, Program, ProgramError,
    QuestInfo, Shape,
};
use crate::types::{TypeDesc, TypeId, TypeTable};

/// Words before the parameters: return link, return value, error value.
pub const FRAME_HEADER_WORDS: u16 = 3;

/// A declared function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FnRef(pub u16);

/// A declared global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalRef(pub u16);

/// A static-region word, resolved to an absolute index by `finish`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotRef {
    Frame {
        func: FnRef,
        word: u16,
    },
    Global(GlobalRef),
    /// A misuse (a parameter index past the parameter count); `finish` reports it.
    Invalid,
}

/// An instruction operand: a slot or an immediate number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opnd {
    Slot(SlotRef),
    Imm(u16),
}

impl From<SlotRef> for Opnd {
    fn from(s: SlotRef) -> Opnd {
        Opnd::Slot(s)
    }
}

impl From<GlobalRef> for Opnd {
    fn from(g: GlobalRef) -> Opnd {
        Opnd::Slot(SlotRef::Global(g))
    }
}

impl From<u16> for Opnd {
    fn from(n: u16) -> Opnd {
        Opnd::Imm(n)
    }
}

/// The words of one function's frame, as slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRef {
    pub func: FnRef,
    pub params: u8,
}

impl FrameRef {
    fn word(&self, word: u16) -> SlotRef {
        SlotRef::Frame {
            func: self.func,
            word,
        }
    }
    pub fn link(&self) -> SlotRef {
        self.word(0)
    }
    pub fn ret(&self) -> SlotRef {
        self.word(1)
    }
    pub fn err(&self) -> SlotRef {
        self.word(2)
    }
    pub fn param(&self, i: u16) -> SlotRef {
        if i < u16::from(self.params) {
            self.word(FRAME_HEADER_WORDS + i)
        } else {
            SlotRef::Invalid
        }
    }
    /// Local or temporary `i`; the frame grows to hold it.
    pub fn local(&self, i: u16) -> SlotRef {
        match (FRAME_HEADER_WORDS + u16::from(self.params)).checked_add(i) {
            Some(w) => self.word(w),
            None => SlotRef::Invalid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label(usize, u16);

#[derive(Clone, Copy)]
enum Wideness {
    Two(Opnd, Opnd),
    T(u32),
}

#[derive(Clone, Copy)]
struct Pending {
    op: Op,
    a: Opnd,
    rest: Wideness,
}

pub struct ProgramBuilder {
    host_api_version: u32,
    code: Vec<Pending>,
    consts: Vec<u64>,
    const_ids: BTreeMap<u64, u32>,
    strings: Vec<String>,
    string_ids: BTreeMap<String, u32>,
    shapes: Vec<Shape>,
    shape_ids: BTreeMap<(u8, u32), u16>,
    types: Vec<TypeDesc>,
    /// Type ids reserved by [`ProgramBuilder::reserve_ty`] and not defined yet.
    reserved: BTreeSet<u16>,
    fns: Vec<FnInfo>,
    globals: Vec<GlobalInfo>,
    handlers: Vec<HandlerInfo>,
    events: Vec<EventInfo>,
    quest: Option<QuestInfo>,
    dialogues: DialogueGraph,
    main_fn: Option<u16>,
    host_sites: Vec<HostSite>,
    lines: Vec<LineEntry>,
    files: Vec<String>,
    cur_file: Option<u16>,
    error: Option<ProgramError>,
}

fn id16(n: usize) -> Option<u16> {
    u16::try_from(n).ok()
}

impl ProgramBuilder {
    pub fn new(host_api_version: u32) -> ProgramBuilder {
        ProgramBuilder {
            host_api_version,
            code: Vec::new(),
            consts: Vec::new(),
            const_ids: BTreeMap::new(),
            strings: Vec::new(),
            string_ids: BTreeMap::new(),
            shapes: Vec::new(),
            shape_ids: BTreeMap::new(),
            types: Vec::new(),
            reserved: BTreeSet::new(),
            fns: Vec::new(),
            globals: Vec::new(),
            handlers: Vec::new(),
            events: Vec::new(),
            quest: None,
            dialogues: DialogueGraph::default(),
            main_fn: None,
            host_sites: Vec::new(),
            lines: Vec::new(),
            files: Vec::new(),
            cur_file: None,
            error: None,
        }
    }

    fn fail(&mut self, why: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(ProgramError::Invalid(why.into()));
        }
    }

    /// A global. Its initialiser defaults to fn 0; set it with [`Self::set_global_init`].
    pub fn global(&mut self, name: &str, ty: TypeId) -> GlobalRef {
        let Some(i) = id16(self.globals.len()) else {
            self.fail("more than 65536 globals");
            return GlobalRef(0);
        };
        self.globals.push(GlobalInfo {
            name: name.to_string(),
            slot: 0, // resolved by finish
            ty,
            init_fn: 0,
        });
        GlobalRef(i)
    }

    pub fn set_global_init(&mut self, g: GlobalRef, init: FnRef) {
        match self.globals.get_mut(usize::from(g.0)) {
            Some(g) => g.init_fn = init.0,
            None => self.fail("set_global_init of an unknown global"),
        }
    }

    /// Declares a function with `params` parameter slots; usable (called, named) before its body exists.
    pub fn declare(&mut self, name: &str, params: u8) -> FnRef {
        let Some(id) = id16(self.fns.len()) else {
            self.fail("more than 65536 functions");
            return FnRef(0);
        };
        self.fns.push(FnInfo {
            name: name.to_string(),
            entry: 0,
            end: 0,
            params,
            frame_base: 0,
            frame_words: 0, // resolved by finish
            ref_slots: Vec::new(),
            sig: None,
        });
        FnRef(id)
    }

    /// Gives a declared function its signature (`FnInfo::sig`): one type per parameter, and the return
    /// type. Needed by a fn the host evaluates ([`crate::vm::Vm::eval_pure`]).
    pub fn set_sig(&mut self, f: FnRef, params: Vec<TypeId>, ret: TypeId) {
        match self.fns.get_mut(usize::from(f.0)) {
            Some(i) => i.sig = Some(FnSig { params, ret }),
            None => self.fail("set_sig of an undeclared fn"),
        }
    }

    /// `declare` and start its body.
    pub fn func(&mut self, name: &str, params: u8) -> FnBuilder<'_> {
        let f = self.declare(name, params);
        self.body(f)
    }

    /// Starts the body of a declared function.
    pub fn body(&mut self, f: FnRef) -> FnBuilder<'_> {
        if usize::from(f.0) >= self.fns.len() {
            self.fail("body of an undeclared fn");
        }
        let start = u32::try_from(self.code.len()).unwrap_or(u32::MAX);
        FnBuilder {
            pb: self,
            func: f,
            start,
            labels: Vec::new(),
            fixups: Vec::new(),
            refs: Vec::new(),
        }
    }

    /// The frame of a declared function.
    pub fn frame(&mut self, f: FnRef) -> FrameRef {
        match self.fns.get(usize::from(f.0)) {
            Some(i) => FrameRef {
                func: f,
                params: i.params,
            },
            None => {
                self.fail("frame of an undeclared fn");
                FrameRef { func: f, params: 0 }
            }
        }
    }

    pub fn const_u64(&mut self, v: u64) -> u32 {
        if let Some(&i) = self.const_ids.get(&v) {
            return i;
        }
        let i = u32::try_from(self.consts.len()).unwrap_or(u32::MAX);
        self.consts.push(v);
        self.const_ids.insert(v, i);
        i
    }

    pub fn string(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.string_ids.get(s) {
            return i;
        }
        let i = u32::try_from(self.strings.len()).unwrap_or(u32::MAX);
        self.strings.push(s.to_string());
        self.string_ids.insert(s.to_string(), i);
        i
    }

    /// `fields` fields, bit `i` of `ref_bits` set when field `i` holds a heap kind.
    pub fn shape(&mut self, fields: u8, ref_bits: u32) -> u16 {
        if let Some(&i) = self.shape_ids.get(&(fields, ref_bits)) {
            return i;
        }
        let Some(i) = id16(self.shapes.len()) else {
            self.fail("more than 65536 shapes");
            return 0;
        };
        self.shapes.push(Shape { fields, ref_bits });
        self.shape_ids.insert((fields, ref_bits), i);
        i
    }

    /// Interns a type descriptor (equal descriptors share an id; a reserved id is never shared).
    pub fn ty(&mut self, d: TypeDesc) -> TypeId {
        let found = self
            .types
            .iter()
            .enumerate()
            .find(|(i, t)| **t == d && id16(*i).is_some_and(|i| !self.reserved.contains(&i)));
        if let Some((i, _)) = found {
            return id16(i).unwrap_or(0);
        }
        let Some(i) = id16(self.types.len()) else {
            self.fail("more than 65536 types");
            return 0;
        };
        self.types.push(d);
        i
    }

    /// A type id of its own, defined later by [`Self::define_ty`]: a type that contains itself (decision
    /// 17) names its id before its descriptor is complete. `finish` fails while one is undefined.
    pub fn reserve_ty(&mut self) -> TypeId {
        let Some(i) = id16(self.types.len()) else {
            self.fail("more than 65536 types");
            return 0;
        };
        self.types.push(TypeDesc::Unit);
        self.reserved.insert(i);
        i
    }

    /// Defines a type id from [`Self::reserve_ty`].
    pub fn define_ty(&mut self, id: TypeId, d: TypeDesc) {
        match self.types.get_mut(usize::from(id)) {
            Some(slot) if self.reserved.remove(&id) => *slot = d,
            _ => self.fail(format!("type {id} was not reserved")),
        }
    }

    pub fn host_site(
        &mut self,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
    ) -> u16 {
        self.fallible_host_site(host_fn, kind, params, ret, Vec::new())
    }

    /// A host site that can fail with `raises` (empty: [`Self::host_site`]).
    pub fn fallible_host_site(
        &mut self,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
        raises: Vec<String>,
    ) -> u16 {
        let Some(i) = id16(self.host_sites.len()) else {
            self.fail("more than 65536 host sites");
            return 0;
        };
        self.host_sites.push(HostSite {
            host_fn,
            kind,
            params,
            ret,
            raises,
        });
        i
    }

    /// A handler of the home (or of a program without a quest), in no stage.
    pub fn handler(&mut self, event: &str, event_ty: TypeId, f: FnRef) {
        self.owned_handler(event, event_ty, f, 0, None);
    }

    /// A handler of instance `owner` (0 the home, `1 + i` part `i`) declared in `stage`.
    pub fn owned_handler(
        &mut self,
        event: &str,
        event_ty: TypeId,
        f: FnRef,
        owner: u16,
        stage: Option<u16>,
    ) {
        self.handlers.push(HandlerInfo {
            event: event.to_string(),
            event_ty,
            fn_id: f.0,
            owner,
            stage,
        });
    }

    /// A script event: its index, the operand of `Enqueue` and `After`.
    pub fn script_event(&mut self, name: &str, ty: TypeId) -> u16 {
        let Some(i) = id16(self.events.len()) else {
            self.fail("more than 65536 script events");
            return 0;
        };
        self.events.push(EventInfo {
            name: name.to_string(),
            ty,
        });
        i
    }

    /// The program's quest (its globals and fns are this builder's indices).
    pub fn set_quest(&mut self, q: QuestInfo) {
        self.quest = Some(q);
    }

    /// The program's dialogue graph (fn ids are this builder's; [`DialogueGraph::block`] lays out steps).
    pub fn set_dialogues(&mut self, g: DialogueGraph) {
        self.dialogues = g;
    }

    pub fn main(&mut self, f: FnRef) {
        self.main_fn = Some(f.0);
    }

    /// The file the following line entries belong to.
    pub fn set_file(&mut self, name: &str) -> u16 {
        let i = match self.files.iter().position(|f| f == name) {
            Some(i) => i,
            None => {
                self.files.push(name.to_string());
                self.files.len() - 1
            }
        };
        let i = match id16(i) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 files");
                0
            }
        };
        self.cur_file = Some(i);
        i
    }

    /// Lays out the static region, resolves every slot, runs `validate` and returns the program.
    pub fn finish(mut self) -> Result<Program, ProgramError> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        if let Some(t) = self.reserved.first() {
            return Err(ProgramError::Invalid(format!(
                "type {t} was reserved and never defined"
            )));
        }
        // frame sizes: the header, the parameters and the highest word any operand names
        let mut words: Vec<u32> = self
            .fns
            .iter()
            .map(|f| u32::from(FRAME_HEADER_WORDS) + u32::from(f.params))
            .collect();
        for p in &self.code {
            // ranges an op reads or writes beyond the words it names: the owning frame covers them
            let (first, extra) = match (p.op, p.rest) {
                (Op::Await | Op::AwaitAll, _) => (p.a, 2),
                // the ticks, then the event value
                (Op::After, Wideness::Two(b, _)) => (b, 2),
                (Op::CallBuiltin, Wideness::Two(b, _)) => {
                    let n = match p.a {
                        Opnd::Imm(id) => Builtin::from_u16(id).map(Builtin::arity),
                        Opnd::Slot(_) => None,
                    };
                    match n {
                        Some(n) => (b, u32::from(n)),
                        None => return invalid("CallBuiltin of an unknown builtin"),
                    }
                }
                (Op::CallHost, Wideness::Two(b, _)) => {
                    let n = match p.a {
                        Opnd::Imm(site) => self.host_sites.get(usize::from(site)),
                        Opnd::Slot(_) => None,
                    };
                    match n {
                        Some(s) => (b, s.params.len() as u32),
                        None => return invalid("CallHost of an unknown host site"),
                    }
                }
                _ => (Opnd::Imm(0), 0),
            };
            // A fallible host call's result: the error word follows the result word.
            let pair = match (p.op, p.a, p.rest) {
                (Op::CallHost, Opnd::Imm(site), Wideness::Two(_, c))
                    if self
                        .host_sites
                        .get(usize::from(site))
                        .is_some_and(HostSite::fallible) =>
                {
                    Some((c, 2))
                }
                _ => None,
            };
            for (first, extra) in std::iter::once((first, extra)).chain(pair) {
                match first {
                    Opnd::Slot(SlotRef::Frame { func, word }) => {
                        if let Some(w) = words.get_mut(usize::from(func.0)) {
                            *w = (*w).max(u32::from(word) + extra);
                        }
                    }
                    Opnd::Slot(SlotRef::Global(_)) => {
                        return invalid(
                            "an argument run or await result is in a global (it must be in a frame)",
                        );
                    }
                    _ => {}
                }
            }
            let opnds = match p.rest {
                Wideness::Two(b, c) => [Some(p.a), Some(b), Some(c)],
                Wideness::T(_) => [Some(p.a), None, None],
            };
            for o in opnds.into_iter().flatten() {
                if let Opnd::Slot(SlotRef::Frame { func, word }) = o {
                    match words.get_mut(usize::from(func.0)) {
                        Some(w) => *w = (*w).max(u32::from(word) + 1),
                        None => return invalid("a slot names an undeclared fn"),
                    }
                }
            }
        }
        for (f, w) in self.fns.iter().zip(words.iter_mut()) {
            if let Some(&top) = f.ref_slots.last() {
                *w = (*w).max(u32::from(top) + 1);
            }
        }
        let mut bases = Vec::with_capacity(words.len());
        let mut cursor = 0u32;
        for w in &words {
            bases.push(cursor);
            cursor = cursor.saturating_add(*w);
        }
        let frames_end = cursor;
        let total = frames_end.saturating_add(self.globals.len() as u32);
        if total > u32::from(u16::MAX) {
            return invalid("the static region is larger than 65535 words");
        }
        for (i, f) in self.fns.iter_mut().enumerate() {
            f.frame_base = bases[i] as u16; // total fits u16
            f.frame_words = words[i] as u16;
        }
        for (i, g) in self.globals.iter_mut().enumerate() {
            g.slot = (frames_end + i as u32) as u16;
        }
        let resolve = |o: Opnd| -> Result<u16, ProgramError> {
            match o {
                Opnd::Imm(n) => Ok(n),
                Opnd::Slot(SlotRef::Frame { func, word }) => bases
                    .get(usize::from(func.0))
                    .and_then(|b| u16::try_from(b + u32::from(word)).ok())
                    .map_or_else(|| invalid("a slot is outside the static region"), Ok),
                Opnd::Slot(SlotRef::Global(g)) => {
                    if usize::from(g.0) < self.globals.len() {
                        Ok((frames_end + u32::from(g.0)) as u16)
                    } else {
                        invalid("a slot names an undeclared global")
                    }
                }
                Opnd::Slot(SlotRef::Invalid) => {
                    invalid("a parameter index is past the function's parameter count")
                }
            }
        };
        let mut code = Vec::with_capacity(self.code.len());
        for p in &self.code {
            code.push(match p.rest {
                Wideness::Two(b, c) => Instr {
                    op: p.op,
                    a: resolve(p.a)?,
                    b: resolve(b)?,
                    c: resolve(c)?,
                },
                Wideness::T(t) => Instr::with_t(p.op, resolve(p.a)?, t),
            });
        }
        let p = Program {
            bytecode_version: BYTECODE_VERSION,
            host_api_version: self.host_api_version,
            code,
            consts: self.consts,
            strings: self.strings,
            shapes: self.shapes,
            types: TypeTable(self.types),
            fns: self.fns,
            globals: self.globals,
            handlers: self.handlers,
            events: self.events,
            quest: self.quest,
            dialogues: self.dialogues,
            main_fn: self.main_fn,
            host_sites: self.host_sites,
            lines: self.lines,
            files: self.files,
            static_words: total as u16,
        };
        p.validate()?;
        Ok(p)
    }
}

fn invalid<T>(why: &str) -> Result<T, ProgramError> {
    Err(ProgramError::Invalid(why.to_string()))
}

/// The body of one function. Close it with [`FnBuilder::end`].
pub struct FnBuilder<'a> {
    pb: &'a mut ProgramBuilder,
    func: FnRef,
    start: u32,
    labels: Vec<Option<u32>>,
    /// (index in the code, label) of jumps waiting for their target.
    fixups: Vec<(usize, Label)>,
    refs: Vec<u16>,
}

impl FnBuilder<'_> {
    pub fn fn_ref(&self) -> FnRef {
        self.func
    }

    /// [`ProgramBuilder::const_u64`], while this body is open.
    pub fn const_u64(&mut self, v: u64) -> u32 {
        self.pb.const_u64(v)
    }

    /// [`ProgramBuilder::string`], while this body is open.
    pub fn string(&mut self, s: &str) -> u32 {
        self.pb.string(s)
    }

    /// [`ProgramBuilder::shape`], while this body is open.
    pub fn shape(&mut self, fields: u8, ref_bits: u32) -> u16 {
        self.pb.shape(fields, ref_bits)
    }

    /// [`ProgramBuilder::ty`], while this body is open.
    pub fn ty(&mut self, d: TypeDesc) -> TypeId {
        self.pb.ty(d)
    }

    /// [`ProgramBuilder::reserve_ty`], while this body is open.
    pub fn reserve_ty(&mut self) -> TypeId {
        self.pb.reserve_ty()
    }

    /// [`ProgramBuilder::define_ty`], while this body is open.
    pub fn define_ty(&mut self, id: TypeId, d: TypeDesc) {
        self.pb.define_ty(id, d)
    }

    /// [`ProgramBuilder::host_site`], while this body is open.
    pub fn host_site(
        &mut self,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
    ) -> u16 {
        self.pb.host_site(host_fn, kind, params, ret)
    }

    /// [`ProgramBuilder::fallible_host_site`], while this body is open.
    pub fn fallible_host_site(
        &mut self,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
        raises: Vec<String>,
    ) -> u16 {
        self.pb
            .fallible_host_site(host_fn, kind, params, ret, raises)
    }

    /// This function's own frame.
    pub fn frame(&self) -> FrameRef {
        self.of(self.func)
    }

    /// A function's frame (a callee's parameter, return and error words).
    pub fn of(&self, f: FnRef) -> FrameRef {
        FrameRef {
            func: f,
            params: self.pb.fns.get(usize::from(f.0)).map_or(0, |i| i.params),
        }
    }

    pub fn ret(&self) -> SlotRef {
        self.frame().ret()
    }
    pub fn err(&self) -> SlotRef {
        self.frame().err()
    }
    pub fn param(&self, i: u16) -> SlotRef {
        self.frame().param(i)
    }
    pub fn local(&self, i: u16) -> SlotRef {
        self.frame().local(i)
    }

    fn pc(&self) -> u32 {
        u32::try_from(self.pb.code.len()).unwrap_or(u32::MAX)
    }

    fn check_kind(&mut self, op: Op, kind: Kind, o: Opnd) {
        let ok = match (kind, o) {
            (Kind::Slot, Opnd::Slot(_)) => true,
            (Kind::Slot, Opnd::Imm(_)) => false,
            (_, Opnd::Slot(_)) => false,
            (_, Opnd::Imm(_)) => true,
        };
        if !ok {
            self.pb.fail(format!(
                "{op:?}: an operand is a {kind:?} but was given the other kind"
            ));
        }
    }

    /// Appends an instruction; its pc.
    pub fn emit(
        &mut self,
        op: Op,
        a: impl Into<Opnd>,
        b: impl Into<Opnd>,
        c: impl Into<Opnd>,
    ) -> u32 {
        let (a, b, c) = (a.into(), b.into(), c.into());
        let lay = op.layout();
        if lay.wide != Wide::None {
            self.pb
                .fail(format!("{op:?} takes a wide operand: use emit_t"));
        }
        self.check_kind(op, lay.a, a);
        self.check_kind(op, lay.b, b);
        self.check_kind(op, lay.c, c);
        let pc = self.pc();
        self.pb.code.push(Pending {
            op,
            a,
            rest: Wideness::Two(b, c),
        });
        pc
    }

    /// Appends an instruction with a 32-bit operand `T` (`LoadConst`, `LoadStr`: an index from
    /// [`ProgramBuilder::const_u64`] / [`ProgramBuilder::string`]).
    pub fn emit_t(&mut self, op: Op, a: impl Into<Opnd>, t: u32) -> u32 {
        let a = a.into();
        let lay = op.layout();
        if lay.wide == Wide::None || lay.wide == Wide::Target {
            self.pb
                .fail(format!("{op:?} has no const or string operand"));
        }
        self.check_kind(op, lay.a, a);
        let pc = self.pc();
        self.pb.code.push(Pending {
            op,
            a,
            rest: Wideness::T(t),
        });
        pc
    }

    /// `Call` of a declared function.
    pub fn call(&mut self, f: FnRef) -> u32 {
        self.emit(Op::Call, f.0, 0u16, 0u16)
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1, self.func.0)
    }

    /// Binds the label to the next instruction.
    pub fn bind(&mut self, l: Label) {
        let pc = self.pc();
        match self.labels.get_mut(l.0) {
            Some(slot @ None) if l.1 == self.func.0 => *slot = Some(pc),
            _ => self
                .pb
                .fail("a label is bound twice or is not from this function"),
        }
    }

    /// A jump (`Jmp`, `JmpIf`, `JmpIfNot`) to `l`; `a` is the condition slot (an immediate 0 for `Jmp`).
    pub fn jump(&mut self, op: Op, a: impl Into<Opnd>, l: Label) -> u32 {
        let a = a.into();
        let lay = op.layout();
        if lay.wide != Wide::Target {
            self.pb.fail(format!("{op:?} is not a jump"));
        }
        if l.1 != self.func.0 {
            self.pb.fail("a jump to a label of another function");
        }
        self.check_kind(op, lay.a, a);
        let pc = self.pc();
        self.fixups.push((self.pb.code.len(), l));
        self.pb.code.push(Pending {
            op,
            a,
            rest: Wideness::T(0),
        });
        pc
    }

    fn line_entry(&mut self, line: u32, col: u32) {
        let file = match self.pb.cur_file {
            Some(f) => f,
            None => self.pb.set_file(""),
        };
        let pc = self.pc();
        let entry = LineEntry {
            pc,
            file,
            line,
            col,
        };
        match self.pb.lines.last_mut() {
            Some(last) if last.pc == pc => *last = entry,
            _ => self.pb.lines.push(entry),
        }
    }

    /// Source line of the instructions that follow.
    pub fn line(&mut self, line: u32) {
        self.line_entry(line, 0);
    }

    pub fn line_col(&mut self, line: u32, col: u32) {
        self.line_entry(line, col);
    }

    /// Marks a word of this function's own frame as holding a heap kind (`ref_slots`).
    pub fn mark_ref(&mut self, slot: SlotRef) {
        match slot {
            SlotRef::Frame { func, word } if func == self.func => self.refs.push(word),
            _ => self
                .pb
                .fail("mark_ref of a slot that is not in this function's frame"),
        }
    }

    /// Resolves the labels and records the function's code range.
    pub fn end(mut self) {
        let fixups = std::mem::take(&mut self.fixups);
        for (at, l) in fixups {
            match self.labels.get(l.0).copied().flatten() {
                Some(target) => {
                    if let Some(p) = self.pb.code.get_mut(at) {
                        p.rest = Wideness::T(target);
                    }
                }
                None => self.pb.fail("a jump to a label that was never bound"),
            }
        }
        let end = self.pc();
        let mut refs = std::mem::take(&mut self.refs);
        refs.sort_unstable();
        refs.dedup();
        let (id, start) = (self.func, self.start);
        if let Some(f) = self.pb.fns.get_mut(usize::from(id.0)) {
            f.entry = start;
            f.end = end;
            f.ref_slots = refs;
        }
    }
}
