//! Code generation (plan D10 stage `codegen`, design D4/D5): the typed program becomes a
//! [`quest_vm::Program`] through the VM's assembler.
//!
//! **Shape.** [`generate`] declares every function in fn-id order ([`crate::layout`]), then the
//! top-level `let`s in initialisation order, then emits each body through an [`Emit`]: `stmt.rs` lowers
//! statements, blocks, loops and function bodies, `expr.rs` expressions. A construct a later task compiles
//! is the error `not-yet` at its span ([`Emit::not_yet`]), so the code generator grows by adding cases.
//!
//! **Slots** (design D5, codegen carry-over 1). Every binding (`LocalId`) gets a frame word of its own
//! when its `let` (or `for`) is emitted, never reused. Temporaries come from a pool and are reused, but
//! only for a value of the same kind: a word is a heap kind or an immediate for its whole life, because
//! several instructions write an immediate without releasing what the word held. Heap-kind words are
//! listed in the function's `ref_slots`.
//!
//! **Ownership** (design D4). A heap-kind word owns one reference. A value is written into its
//! destination by an instruction that releases what the destination held (`Dup`, `Move`, `LoadStr`, a
//! builtin's result), so a destination never needs dropping first. A binding is dropped when its block
//! ends; on `break` / `continue` the blocks the jump leaves are dropped, on `return` every block, the
//! heap parameters and any temporary still holding a value. A caller writes the arguments into the
//! callee's parameter words, the callee owns them (it drops them as it returns) and the caller moves the
//! result out of the callee's word 1. So when a function is not running, its frame holds no reference.
//!
//! **Order of evaluation** is left to right. A value read straight from a variable's word (not copied)
//! is read when the instruction that uses it runs; when an operand to its right may run a call (which
//! may write that variable — a global, a later task's reference), the variable is copied into a
//! temporary first.

mod expr;
mod stmt;

use crate::ast::Span;
use crate::diag::Diagnostic;
use crate::hir::{Crate, DefId, DefKind, LocalId};
use crate::infer::Typed;
use crate::layout::{self, FnKind};
use crate::ty::{Ty, TyId};
use quest_vm::asm::{FnBuilder, FnRef, GlobalRef, SlotRef};
use quest_vm::host::HostApi;
use quest_vm::types::{TypeDesc, TypeId};
use quest_vm::{Program, ProgramBuilder};
use std::collections::{BTreeMap, BTreeSet};

/// The static region's words are addressed by 16-bit operands.
const STATIC_WORDS_MAX: u32 = u16::MAX as u32;

/// Words before a frame's parameters: return link, return value, error value.
const FRAME_HEADER_WORDS: u32 = quest_vm::asm::FRAME_HEADER_WORDS as u32;

/// Compiles a checked program (no error diagnostics were reported): the program, or the errors only
/// code generation finds (`too-many-slots`, `not-yet`).
pub(crate) fn generate(
    krate: &Crate,
    typed: &Typed,
    host: &HostApi,
) -> Result<Program, Vec<Diagnostic>> {
    let plan = layout::plan(krate, typed);
    let mut pb = ProgramBuilder::new(host.version);
    let mut g = Gen {
        krate,
        typed,
        fns: BTreeMap::new(),
        globals: BTreeMap::new(),
        type_ids: BTreeMap::new(),
        diags: Vec::new(),
    };

    let mut refs = Vec::with_capacity(plan.fns.len());
    for item in &plan.fns {
        let params = match item.kind {
            FnKind::Def(d) => g.param_count(d),
            FnKind::Init(_) => 0,
        };
        let params = match u8::try_from(params) {
            Ok(p) => p,
            Err(_) => {
                let (file, span) = g.def_site(def_of(item.kind));
                g.diags.push(Diagnostic::error(
                    "too-many-slots",
                    format!(
                        "`{}` has {params} parameters; at most 255 fit a frame",
                        item.name
                    ),
                    &file,
                    span.line,
                    span.col,
                    span.len,
                ));
                0
            }
        };
        let r = pb.declare(&item.name, params);
        refs.push(r);
        match item.kind {
            FnKind::Def(d) => g.fns.insert(d, r),
            FnKind::Init(d) => g.fns.insert(d, r),
        };
    }
    for d in &plan.globals {
        let ty = typed.globals.get(d).copied().unwrap_or(TyId::ERROR);
        let (file, span) = g.def_site(*d);
        let Some(tid) = g.type_id(&mut |d| pb.ty(d), ty, &file, span) else {
            continue;
        };
        let gr = pb.global(&krate.qualified_name(*d), tid);
        if let Some(init) = g.fns.get(d) {
            pb.set_global_init(gr, *init);
        }
        g.globals.insert(*d, gr);
    }
    if let Some(m) = plan.main.and_then(|m| g.fns.get(&m)) {
        pb.main(*m);
    }

    // Frame sizes, for `too-many-slots`: (words, the function's index in `plan.fns`).
    let mut frames: Vec<(u32, usize)> = Vec::with_capacity(plan.fns.len());
    for (i, (item, r)) in plan.fns.iter().zip(&refs).enumerate() {
        let d = def_of(item.kind);
        let (file, _) = g.def_site(d);
        pb.set_file(&file);
        let fb = pb.body(*r);
        let words = Emit::new(&mut g, fb, file).function(item.kind);
        frames.push((words, i));
    }

    let total = frames
        .iter()
        .fold(0u64, |acc, (w, _)| acc + u64::from(*w))
        .saturating_add(plan.globals.len() as u64);
    if total > u64::from(STATIC_WORDS_MAX) {
        // Named at the largest frame (the first of the largest, in fn-id order).
        let (words, i) = frames
            .iter()
            .copied()
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
            .unwrap_or((0, 0));
        let name = plan.fns.get(i).map_or("", |f| f.name.as_str());
        let (file, span) = plan
            .fns
            .get(i)
            .map_or((String::new(), Span::default()), |f| {
                g.def_site(def_of(f.kind))
            });
        g.diags.push(Diagnostic::error(
            "too-many-slots",
            format!(
                "the program needs {total} static words (frames and top-level variables), more than \
                 {STATIC_WORDS_MAX}; `{name}` alone takes {words}: split it into smaller functions"
            ),
            &file,
            span.line,
            span.col,
            span.len,
        ));
    }
    if !g.diags.is_empty() {
        return Err(g.diags);
    }
    pb.finish().map_err(|e| {
        vec![Diagnostic::error(
            "internal",
            format!("code generation produced an invalid program: {e}"),
            "",
            0,
            0,
            0,
        )]
    })
}

fn def_of(k: FnKind) -> DefId {
    match k {
        FnKind::Def(d) | FnKind::Init(d) => d,
    }
}

/// What every body's emitter shares.
pub(crate) struct Gen<'a> {
    krate: &'a Crate,
    typed: &'a Typed,
    /// Every function and initialiser, by its def.
    fns: BTreeMap<DefId, FnRef>,
    globals: BTreeMap<DefId, GlobalRef>,
    /// The VM type of each type lowered so far.
    type_ids: BTreeMap<TyId, TypeId>,
    diags: Vec<Diagnostic>,
}

impl Gen<'_> {
    /// A function's parameter words: the receiver, then the parameters.
    fn param_count(&self, d: DefId) -> usize {
        self.typed
            .sigs
            .get(&d)
            .map_or(0, |s| usize::from(s.receiver.is_some()) + s.params.len())
    }

    /// The file and span of an item.
    fn def_site(&self, d: DefId) -> (String, Span) {
        match self.krate.def(d) {
            Some(def) => (
                self.krate
                    .module(def.module)
                    .map_or(String::new(), |m| m.file.clone()),
                def.span,
            ),
            None => (String::new(), Span::default()),
        }
    }

    fn not_yet(&mut self, what: &str, file: &str, at: Span) {
        self.diags.push(Diagnostic::error(
            "not-yet",
            format!("{what} is checked but not compiled yet"),
            file,
            at.line,
            at.col,
            at.len,
        ));
    }

    /// Whether values of `ty` are heap kinds (design D4): a pointer, `0` or a field-less variant.
    fn is_heap(&self, ty: TyId) -> bool {
        matches!(
            self.typed.types.get(ty),
            Ty::Str
                | Ty::List(_)
                | Ty::Dict(_, _)
                | Ty::Option(_)
                | Ty::Adt(_)
                | Ty::HostStruct(_)
                | Ty::HostEvent(_)
                | Ty::Ref(_)
                | Ty::Caught(_)
        )
    }

    /// The VM type of `ty`, interned in the program's type table (children first, by a loop: a type may
    /// nest deeply). `None` (reported at `at`) for a type a later task lowers.
    fn type_id(
        &mut self,
        intern: &mut dyn FnMut(TypeDesc) -> TypeId,
        ty: TyId,
        file: &str,
        at: Span,
    ) -> Option<TypeId> {
        let mut stack = vec![ty];
        while let Some(&t) = stack.last() {
            if self.type_ids.contains_key(&t) {
                stack.pop();
                continue;
            }
            let shape = self.typed.types.get(t);
            let children = match shape {
                Ty::List(_) | Ty::Dict(_, _) | Ty::Option(_) | Ty::Promise(_) => shape.children(),
                Ty::Unit | Ty::Never | Ty::Int | Ty::Float | Ty::Bool | Ty::Str => Vec::new(),
                _ => {
                    self.not_yet(
                        "a value of a struct, enum, error or reference type",
                        file,
                        at,
                    );
                    return None;
                }
            };
            let missing: Vec<TyId> = children
                .iter()
                .copied()
                .filter(|c| !self.type_ids.contains_key(c))
                .collect();
            if !missing.is_empty() {
                stack.extend(missing);
                continue;
            }
            let id = |c: TyId| self.type_ids.get(&c).copied().unwrap_or(0);
            let desc = match shape {
                Ty::Int => TypeDesc::Int,
                Ty::Float => TypeDesc::Float,
                Ty::Bool => TypeDesc::Bool,
                Ty::Str => TypeDesc::Str,
                Ty::List(e) => TypeDesc::List(id(e)),
                Ty::Dict(k, v) => TypeDesc::Dict(id(k), id(v)),
                Ty::Promise(v) => TypeDesc::Promise(id(v)),
                Ty::Option(v) => TypeDesc::Enum {
                    name: "Option".to_string(),
                    variants: vec![
                        ("None".to_string(), Vec::new()),
                        ("Some".to_string(), vec![("value".to_string(), id(v))]),
                    ],
                },
                _ => TypeDesc::Unit,
            };
            let tid = intern(desc);
            self.type_ids.insert(t, tid);
            stack.pop();
        }
        self.type_ids.get(&ty).copied()
    }
}

/// The frame words of one function: bindings and temporaries, by index after the parameters.
#[derive(Default)]
struct Slots {
    /// By index: whether the word holds a heap kind.
    heap: Vec<bool>,
    /// Temporaries not in use.
    free: BTreeSet<u32>,
}

impl Slots {
    fn fresh(&mut self, heap: bool) -> u32 {
        let i = u32::try_from(self.heap.len()).unwrap_or(u32::MAX);
        self.heap.push(heap);
        i
    }

    fn kind(&self, i: u32) -> Option<bool> {
        self.heap.get(i as usize).copied()
    }

    /// A temporary of this kind: a free one, else a fresh word.
    fn temp(&mut self, heap: bool) -> u32 {
        let found = self
            .free
            .iter()
            .copied()
            .find(|&i| self.kind(i) == Some(heap));
        match found {
            Some(i) => {
                self.free.remove(&i);
                i
            }
            None => self.fresh(heap),
        }
    }

    /// Consecutive temporaries of these kinds (a builtin's argument run): free ones, else fresh words.
    fn run(&mut self, kinds: &[bool]) -> u32 {
        let fits = |s: &Slots, base: u32| {
            kinds.iter().enumerate().all(|(k, &heap)| {
                let i = base.saturating_add(k as u32);
                s.free.contains(&i) && s.kind(i) == Some(heap)
            })
        };
        let found = self.free.iter().copied().find(|&b| fits(self, b));
        match found {
            Some(base) => {
                for k in 0..kinds.len() as u32 {
                    self.free.remove(&(base + k));
                }
                base
            }
            None => {
                let base = u32::try_from(self.heap.len()).unwrap_or(u32::MAX);
                self.heap.extend_from_slice(kinds);
                base
            }
        }
    }

    fn release(&mut self, i: u32) {
        self.free.insert(i);
    }
}

/// A value an instruction can read: a word holding it, owned by this expression (a temporary, to be
/// released) or borrowed from a variable.
#[derive(Debug, Clone, Copy)]
struct Val {
    slot: SlotRef,
    heap: bool,
    /// The temporary's index when the value is one.
    temp: Option<u32>,
}

/// An enclosing loop: where `break` and `continue` go, and what they leave.
struct Loop {
    brk: quest_vm::asm::Label,
    cont: quest_vm::asm::Label,
    /// Open blocks outside the loop.
    scopes: usize,
    /// Live temporaries made before the loop's body.
    live: usize,
}

/// Emits one function's body.
pub(crate) struct Emit<'g, 'a, 'p> {
    g: &'g mut Gen<'a>,
    fb: FnBuilder<'p>,
    file: String,
    locals: BTreeMap<LocalId, SlotRef>,
    slots: Slots,
    params: u32,
    /// The heap-kind bindings of each open block, innermost last.
    scopes: Vec<Vec<SlotRef>>,
    /// The heap-kind parameters (the receiver included).
    heap_params: Vec<SlotRef>,
    /// Heap-kind temporaries holding a value, oldest first.
    live: Vec<u32>,
    loops: Vec<Loop>,
    /// The line of the statement being emitted.
    line: (u32, u32),
}

impl<'g, 'a, 'p> Emit<'g, 'a, 'p> {
    fn new(g: &'g mut Gen<'a>, fb: FnBuilder<'p>, file: String) -> Emit<'g, 'a, 'p> {
        Emit {
            g,
            fb,
            file,
            locals: BTreeMap::new(),
            slots: Slots::default(),
            params: 0,
            scopes: Vec::new(),
            heap_params: Vec::new(),
            live: Vec::new(),
            loops: Vec::new(),
            line: (0, 0),
        }
    }

    fn not_yet(&mut self, what: &str, at: Span) {
        let file = self.file.clone();
        self.g.not_yet(what, &file, at);
    }

    fn ty_of(&self, e: &crate::hir::Expr) -> TyId {
        self.g.typed.expr(e.id)
    }

    fn heap_ty(&self, ty: TyId) -> bool {
        self.g.is_heap(ty)
    }

    fn heap_expr(&self, e: &crate::hir::Expr) -> bool {
        self.g.is_heap(self.ty_of(e))
    }

    /// The word of local index `i` (after the parameters).
    fn word(&self, i: u32) -> SlotRef {
        // An index past what a frame can address is caught by `too-many-slots` before `finish`.
        self.fb.local(u16::try_from(i).unwrap_or(u16::MAX))
    }

    /// A temporary of the kind, in use until [`Self::free`].
    fn temp(&mut self, heap: bool) -> u32 {
        let t = self.slots.temp(heap);
        if heap {
            self.live.push(t);
        }
        t
    }

    /// Gives a temporary back; a heap kind must hold nothing any more (moved out or dropped).
    fn free(&mut self, t: u32) {
        if let Some(at) = self.live.iter().rposition(|&l| l == t) {
            self.live.remove(at);
        }
        self.slots.release(t);
    }

    /// The VM type of `ty` (`None`: reported).
    fn type_id(&mut self, ty: TyId, at: Span) -> Option<TypeId> {
        let file = self.file.clone();
        let fb = &mut self.fb;
        self.g.type_id(&mut |d| fb.ty(d), ty, &file, at)
    }

    /// Marks the heap words in `ref_slots` and closes the body; the frame's size in words.
    fn finish(mut self) -> u32 {
        for (i, heap) in self.slots.heap.clone().into_iter().enumerate() {
            if heap {
                let w = self.word(u32::try_from(i).unwrap_or(u32::MAX));
                self.fb.mark_ref(w);
            }
        }
        for p in self.heap_params.clone() {
            self.fb.mark_ref(p);
        }
        let locals = u32::try_from(self.slots.heap.len()).unwrap_or(u32::MAX);
        self.fb.end();
        FRAME_HEADER_WORDS
            .saturating_add(self.params)
            .saturating_add(locals)
    }

    /// The source position of the instructions that follow.
    fn at(&mut self, span: Span) {
        self.fb.line_col(span.line, span.col);
    }

    fn def_kind(&self, d: DefId) -> Option<&'a DefKind> {
        let krate: &'a Crate = self.g.krate;
        krate.def(d).map(|def| &def.kind)
    }
}
