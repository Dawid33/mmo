//! The assembler: builds a [`Program`] for hand-written test programs and for the compiler's code
//! generator.
//!
//! Static region layout: a shared cursor hands out words in declaration order. A function declared with
//! [`ProgramBuilder::declare`] / [`ProgramBuilder::func`] takes its frame (`3 + params + locals` words:
//! word 0 return link, word 1 return value, word 2 error value, then parameters, then locals and
//! temporaries) right there, and a global takes one word. Every slot is therefore known the moment it is
//! declared, so code can use absolute slot operands while it is emitted. Declaring all functions first
//! and the globals last gives "frames in declaration order, then the globals" as the design describes.
//!
//! Functions are declared (and callable) at any time; their bodies are emitted one at a time through
//! [`FnBuilder`], in any order, and appended to the code in that order. A body must be closed with
//! [`FnBuilder::end`]. Errors found while building (more than 65 535 words, an unbound label, a bad
//! reference) are remembered and returned by [`ProgramBuilder::finish`], which also runs
//! [`Program::validate`].

use std::collections::BTreeMap;

use crate::bytecode::{BYTECODE_VERSION, Instr, Op};
use crate::host::HostFnKind;
use crate::program::{
    FnInfo, GlobalInfo, HandlerInfo, HostSite, LineEntry, Program, ProgramError, Shape,
};
use crate::types::{TypeDesc, TypeId, TypeTable};

/// Words before the parameters: return link, return value, error value.
pub const FRAME_HEADER_WORDS: u16 = 3;

/// The absolute slots of one function's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub base: u16,
    pub params: u16,
    pub locals: u16,
}

impl Frame {
    /// Word `i` of the frame (absolute).
    pub fn slot(&self, i: u16) -> u16 {
        self.base.saturating_add(i)
    }
    pub fn link_slot(&self) -> u16 {
        self.base
    }
    pub fn ret_slot(&self) -> u16 {
        self.slot(1)
    }
    pub fn err_slot(&self) -> u16 {
        self.slot(2)
    }
    pub fn param(&self, i: u16) -> u16 {
        self.slot(FRAME_HEADER_WORDS.saturating_add(i))
    }
    pub fn local(&self, i: u16) -> u16 {
        self.slot(
            FRAME_HEADER_WORDS
                .saturating_add(self.params)
                .saturating_add(i),
        )
    }
    pub fn words(&self) -> u16 {
        FRAME_HEADER_WORDS
            .saturating_add(self.params)
            .saturating_add(self.locals)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label(usize);

pub struct ProgramBuilder {
    host_api_version: u32,
    code: Vec<Instr>,
    consts: Vec<u64>,
    const_ids: BTreeMap<u64, u32>,
    strings: Vec<String>,
    string_ids: BTreeMap<String, u32>,
    shapes: Vec<Shape>,
    shape_ids: BTreeMap<(u8, u32), u16>,
    types: Vec<TypeDesc>,
    fns: Vec<FnInfo>,
    frames: Vec<Frame>,
    globals: Vec<GlobalInfo>,
    handlers: Vec<HandlerInfo>,
    main_fn: Option<u16>,
    host_sites: Vec<HostSite>,
    lines: Vec<LineEntry>,
    files: Vec<String>,
    cur_file: Option<u16>,
    cursor: u32,
    error: Option<ProgramError>,
}

fn narrow<T: TryFrom<usize>>(n: usize) -> Option<T> {
    T::try_from(n).ok()
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
            fns: Vec::new(),
            frames: Vec::new(),
            globals: Vec::new(),
            handlers: Vec::new(),
            main_fn: None,
            host_sites: Vec::new(),
            lines: Vec::new(),
            files: Vec::new(),
            cur_file: None,
            cursor: 0,
            error: None,
        }
    }

    fn fail(&mut self, why: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(ProgramError::Invalid(why.into()));
        }
    }

    /// `n` more words of the static region; the first one's index.
    fn alloc_words(&mut self, n: u32) -> u16 {
        let at = self.cursor;
        self.cursor = self.cursor.saturating_add(n);
        match u16::try_from(at) {
            Ok(a) if self.cursor <= u32::from(u16::MAX) => a,
            _ => {
                self.fail("the static region is larger than 65535 words");
                0
            }
        }
    }

    /// A global's slot. Its initialiser defaults to fn 0; set it with [`Self::set_global_init`].
    pub fn global(&mut self, name: &str, ty: TypeId) -> u16 {
        let slot = self.alloc_words(1);
        self.globals.push(GlobalInfo {
            name: name.to_string(),
            slot,
            ty,
            init_fn: 0,
        });
        slot
    }

    pub fn set_global_init(&mut self, slot: u16, init_fn: u16) {
        match self.globals.iter_mut().find(|g| g.slot == slot) {
            Some(g) => g.init_fn = init_fn,
            None => self.fail(format!("no global in slot {slot}")),
        }
    }

    /// Declares a function and lays out its frame; its body comes later ([`Self::body`]). Returns its id.
    pub fn declare(&mut self, name: &str, params: u16, locals: u16) -> u16 {
        let words = u32::from(FRAME_HEADER_WORDS) + u32::from(params) + u32::from(locals);
        let base = self.alloc_words(words);
        let id = match narrow::<u16>(self.fns.len()) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 functions");
                0
            }
        };
        self.frames.push(Frame {
            base,
            params,
            locals,
        });
        self.fns.push(FnInfo {
            name: name.to_string(),
            entry: 0,
            end: 0,
            frame_base: base,
            frame_words: u16::try_from(words).unwrap_or(u16::MAX),
            ref_slots: Vec::new(),
        });
        id
    }

    /// `declare` and start its body.
    pub fn func(&mut self, name: &str, params: u16, locals: u16) -> FnBuilder<'_> {
        let id = self.declare(name, params, locals);
        self.body(id)
    }

    /// Starts the body of a declared function.
    pub fn body(&mut self, id: u16) -> FnBuilder<'_> {
        if usize::from(id) >= self.fns.len() {
            self.fail(format!("body of undeclared fn {id}"));
        }
        let start = u32::try_from(self.code.len()).unwrap_or(u32::MAX);
        let frame = self.frame(id);
        FnBuilder {
            pb: self,
            id,
            frame,
            start,
            labels: Vec::new(),
            fixups: Vec::new(),
            refs: Vec::new(),
        }
    }

    /// The frame of a declared function (an all-zero frame for an unknown id, which `finish` reports).
    pub fn frame(&mut self, id: u16) -> Frame {
        match self.frames.get(usize::from(id)) {
            Some(f) => *f,
            None => {
                self.fail(format!("frame of undeclared fn {id}"));
                Frame {
                    base: 0,
                    params: 0,
                    locals: 0,
                }
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
        let i = match narrow::<u16>(self.shapes.len()) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 shapes");
                0
            }
        };
        self.shapes.push(Shape { fields, ref_bits });
        self.shape_ids.insert((fields, ref_bits), i);
        i
    }

    /// Interns a type descriptor (equal descriptors share an id).
    pub fn ty(&mut self, d: TypeDesc) -> TypeId {
        if let Some(i) = self.types.iter().position(|t| *t == d) {
            return u16::try_from(i).unwrap_or(0);
        }
        let i = match narrow::<u16>(self.types.len()) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 types");
                0
            }
        };
        self.types.push(d);
        i
    }

    pub fn host_site(
        &mut self,
        host_fn: u16,
        kind: HostFnKind,
        params: Vec<TypeId>,
        ret: TypeId,
    ) -> u16 {
        let i = match narrow::<u16>(self.host_sites.len()) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 host sites");
                0
            }
        };
        self.host_sites.push(HostSite {
            host_fn,
            kind,
            params,
            ret,
        });
        i
    }

    pub fn handler(&mut self, event: &str, event_ty: TypeId, fn_id: u16) {
        self.handlers.push(HandlerInfo {
            event: event.to_string(),
            event_ty,
            fn_id,
        });
    }

    pub fn main(&mut self, fn_id: u16) {
        self.main_fn = Some(fn_id);
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
        let i = match narrow::<u16>(i) {
            Some(i) => i,
            None => {
                self.fail("more than 65536 files");
                0
            }
        };
        self.cur_file = Some(i);
        i
    }

    /// Runs `validate` and returns the program.
    pub fn finish(self) -> Result<Program, ProgramError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let static_words = u16::try_from(self.cursor).unwrap_or(u16::MAX);
        let p = Program {
            bytecode_version: BYTECODE_VERSION,
            host_api_version: self.host_api_version,
            code: self.code,
            consts: self.consts,
            strings: self.strings,
            shapes: self.shapes,
            types: TypeTable(self.types),
            fns: self.fns,
            globals: self.globals,
            handlers: self.handlers,
            main_fn: self.main_fn,
            host_sites: self.host_sites,
            lines: self.lines,
            files: self.files,
            static_words,
        };
        p.validate()?;
        Ok(p)
    }
}

/// The body of one function. Close it with [`FnBuilder::end`].
pub struct FnBuilder<'a> {
    pb: &'a mut ProgramBuilder,
    id: u16,
    frame: Frame,
    start: u32,
    labels: Vec<Option<u32>>,
    /// (index in the code, label) of jumps waiting for their target.
    fixups: Vec<(usize, Label)>,
    refs: Vec<u16>,
}

impl FnBuilder<'_> {
    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn frame(&self) -> Frame {
        self.frame
    }

    /// Word `i` of this function's frame, absolute.
    pub fn slot(&self, i: u16) -> u16 {
        self.frame().slot(i)
    }
    pub fn ret_slot(&self) -> u16 {
        self.frame().ret_slot()
    }
    pub fn err_slot(&self) -> u16 {
        self.frame().err_slot()
    }
    pub fn param(&self, i: u16) -> u16 {
        self.frame().param(i)
    }
    pub fn local(&self, i: u16) -> u16 {
        self.frame().local(i)
    }

    fn pc(&self) -> u32 {
        u32::try_from(self.pb.code.len()).unwrap_or(u32::MAX)
    }

    /// Appends an instruction; its pc.
    pub fn emit(&mut self, op: Op, a: u16, b: u16, c: u16) -> u32 {
        let pc = self.pc();
        self.pb.code.push(Instr { op, a, b, c });
        pc
    }

    /// Appends an instruction with a 32-bit operand `T` (`LoadConst`, `LoadStr`, ...).
    pub fn emit_t(&mut self, op: Op, a: u16, t: u32) -> u32 {
        let pc = self.pc();
        self.pb.code.push(Instr::with_t(op, a, t));
        pc
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Binds the label to the next instruction.
    pub fn bind(&mut self, l: Label) {
        let pc = self.pc();
        match self.labels.get_mut(l.0) {
            Some(slot @ None) => *slot = Some(pc),
            _ => self
                .pb
                .fail("a label is bound twice or is not from this function"),
        }
    }

    /// A jump (`Jmp`, `JmpIf`, `JmpIfNot`) to `l`; `a` is the condition slot (0 for `Jmp`).
    pub fn jump(&mut self, op: Op, a: u16, l: Label) -> u32 {
        let pc = self.emit_t(op, a, 0);
        self.fixups.push((self.code_index(pc), l));
        pc
    }

    fn code_index(&self, pc: u32) -> usize {
        usize::try_from(pc).unwrap_or(usize::MAX)
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

    /// Marks a frame word (absolute slot) as holding a heap kind: it goes into the function's `ref_slots`.
    pub fn mark_ref(&mut self, slot: u16) {
        let base = self.frame.base;
        match slot.checked_sub(base) {
            Some(rel) => self.refs.push(rel),
            None => self.pb.fail("mark_ref of a slot below the frame"),
        }
    }

    /// Resolves the labels and records the function's code range.
    pub fn end(mut self) {
        let fixups = std::mem::take(&mut self.fixups);
        for (at, l) in fixups {
            match self.labels.get(l.0).copied().flatten() {
                Some(target) => {
                    if let Some(i) = self.pb.code.get_mut(at) {
                        *i = Instr::with_t(i.op, i.a, target);
                    }
                }
                None => self.pb.fail("a jump to a label that was never bound"),
            }
        }
        let end = self.pc();
        let mut refs = std::mem::take(&mut self.refs);
        refs.sort_unstable();
        refs.dedup();
        let (id, start) = (self.id, self.start);
        if let Some(f) = self.pb.fns.get_mut(usize::from(id)) {
            f.entry = start;
            f.end = end;
            f.ref_slots = refs;
        }
    }
}
