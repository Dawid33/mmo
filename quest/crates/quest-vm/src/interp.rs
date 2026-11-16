//! The run loop (design D6) and the instruction groups of the interpreter core (design D5): loads,
//! integer and float arithmetic, bool, control flow and calls over static frames.
//!
//! The loop keeps the pc and the current function in locals while it runs and writes them, the fuel
//! debt and the status back to the header before it returns, so every run ends with the whole state in
//! memory. Everything read from memory (header fields, return links) is checked; static operands were
//! checked by [`Program::validate`] when the `Vm` was made.
//!
//! Fuel: each instruction's base charge ([`base_fuel`]) is taken before it executes, and its
//! size-dependent work (`ceil(words / 8)` of what a value-layer function reported) by the instruction
//! through the [`Meter`]; a charge larger than what is left takes the budget below zero and the deficit
//! becomes the debt, repaid from the next run's budget before anything executes.
//!
//! # The reaper
//! Each iteration of the loop first looks at the zombie list: when it is not empty, one reap step runs
//! (`FUEL_REAP` + its reported work) instead of an instruction. So the zombie list is always empty when an
//! instruction starts, and every free the program causes is paid for, a bounded step at a time.
//!
//! # A full heap, a broken reference
//! An instruction that fails with `HeapFull` is undone through the undo log (back to the checkpoint taken
//! just before it), so it leaves no partial effect, and the run faults `HeapFull` at it. It is never
//! retried after a reap step: the instruction began with an empty zombie list (above), and undoing it
//! also undoes any release it made, so after the undo there is nothing a reap step could give back. The
//! reference instructions (`refs`) are undone the same way on EVERY fault: a `RefTake` / `RefStore` that
//! made part of a path unique before meeting a step that no longer resolves leaves nothing behind. The
//! fuel charged stands.

use crate::bytecode::{Instr, Op};
use crate::consts::ERR_TRACE_MAX;
use crate::fault::FaultKind;
use crate::fuel::{
    FUEL_ALLOC, FUEL_AWAIT, FUEL_BUILTIN, FUEL_CALL, FUEL_HEAP, FUEL_HOST_COMMAND, FUEL_HOST_QUERY,
    FUEL_HOST_REQUEST, FUEL_KEY, FUEL_REAP, FUEL_SIMPLE,
};
use crate::host::{Host, HostFnKind, SrcLoc};
use crate::layout::{
    H_CURRENT_FN, H_FUEL_DEBT, H_PC, H_STATUS, H_TRACE_DROPPED, H_TRACE_LEN, LINK_SENTINEL,
    STATUS_IDLE, STATUS_RUNNING,
};
use crate::mem::MemError;
use crate::object::reap_step;
use crate::program::Program;
use crate::vm::{DebugRun, RunEnd, Vm};
use crate::wire::canonical_bits;

/// A fault raised by an instruction: its kind and a short human text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fault {
    pub kind: FaultKind,
    pub message: String,
}

impl Fault {
    pub fn corrupt(why: &str) -> Fault {
        Fault {
            kind: FaultKind::Corrupt,
            message: why.to_string(),
        }
    }
}

/// The fuel of one run: what is left of the budget, the debt an overshoot made and everything charged.
/// Every charge, of an instruction's base cost, of its size-dependent work or of a reap step, goes
/// through it, so the accounting lives in one place.
pub(crate) struct Meter {
    left: u64,
    debt: u64,
    used: u64,
}

impl Meter {
    fn new(budget: u64) -> Meter {
        Meter {
            left: budget,
            debt: 0,
            used: 0,
        }
    }

    /// Charges `n`; what the budget cannot cover becomes debt.
    pub fn charge(&mut self, n: u64) {
        self.used = self.used.saturating_add(n);
        if n > self.left {
            self.debt = self.debt.saturating_add(n - self.left);
            self.left = 0;
        } else {
            self.left -= n;
        }
    }

    /// Charges the "+ size" of design D6 for `words` words of reported work: `ceil(words / 8)`.
    pub fn words(&mut self, words: u32) {
        self.charge(u64::from(words.div_ceil(8)));
    }
}

impl From<FaultKind> for Fault {
    fn from(kind: FaultKind) -> Fault {
        Fault {
            kind,
            message: kind.describe().to_string(),
        }
    }
}

impl From<MemError> for Fault {
    fn from(e: MemError) -> Fault {
        FaultKind::from(e).into()
    }
}

pub(crate) type Exec<T> = Result<T, Fault>;

/// What an instruction does to the pc.
pub(crate) enum Flow {
    Next,
    Goto(u32),
    /// Into another function (a call or a return).
    Enter {
        fn_id: u16,
        pc: u32,
    },
    /// The started function returned.
    Done,
}

/// The charge taken before an instruction executes (design D6). Size-dependent extras are charged by
/// the instruction itself through the [`Meter`]. Every op is listed: a new one does not compile until
/// its cost is stated.
pub(crate) fn base_fuel(p: &Program, ins: Instr) -> u64 {
    use Op::*;
    match ins.op {
        Nop | LoadSmall | LoadConst | Copy | AddI | SubI | MulI | DivI | RemI | NegI | EqI
        | NeI | LtI | LeI | AddF | SubF | MulF | DivF | NegF | EqF | NeF | LtF | LeF
        | IntToFloat | FloatToInt | Not | Jmp | JmpIf | JmpIfNot | TracePush | GetTag | ListLen
        | PromiseDetach => FUEL_SIMPLE,
        // A comparison: simple, plus the words the walk compared.
        Eq => FUEL_SIMPLE,
        Call | Ret => FUEL_CALL,
        Dup | Move | Drop | GetField | TakeField | SetField | MakeUnique | GetIndex | TakeIndex
        | SetIndex | RefLoad | RefTake | RefStore => FUEL_HEAP,
        LoadStr | NewObj | NewList | NewDict | RefMake | RefField | RefIndex | RefKey => FUEL_ALLOC,
        GetKey | TakeKey | SetKey => FUEL_KEY,
        CallBuiltin => FUEL_BUILTIN,
        // `validate` checked the site; an unknown one faults when it executes.
        CallHost => match p.host_sites.get(usize::from(ins.a)).map(|s| s.kind) {
            Some(HostFnKind::Command) => FUEL_HOST_COMMAND,
            Some(HostFnKind::Promise) => FUEL_HOST_REQUEST,
            Some(HostFnKind::Query) | None => FUEL_HOST_QUERY,
        },
        Await | AwaitAll => FUEL_AWAIT,
    }
}

/// Whether a fault of `op` is undone back to the checkpoint taken just before it (module docs): every
/// `HeapFull`, and every fault of the reference instructions, whose walk may have cloned part of a path.
fn undone_on_fault(op: Op, kind: FaultKind) -> bool {
    kind == FaultKind::HeapFull
        || matches!(
            op,
            Op::RefMake
                | Op::RefField
                | Op::RefIndex
                | Op::RefKey
                | Op::RefLoad
                | Op::RefTake
                | Op::RefStore
        )
}

/// `-2^63` and `2^63` as f64: an int-convertible float lies in `[-2^63, 2^63)`.
const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;

/// Where a return link leads: the caller and the pc after its `Call`. `None` for the sentinel and for
/// anything that is not the return site of a `Call` of `callee` (the link is read from memory).
fn return_site(p: &Program, link: u64, callee: u16) -> Option<(u16, u32)> {
    if link == LINK_SENTINEL {
        return None;
    }
    let caller = u16::try_from(link >> 32).ok()?;
    let pc = link as u32;
    let f = p.fns.get(usize::from(caller))?;
    if pc <= f.entry || pc >= f.end {
        return None;
    }
    let call = p.code.get(usize::try_from(pc - 1).ok()?)?;
    (call.op == Op::Call && call.a == callee).then_some((caller, pc))
}

fn int(v: u64) -> i64 {
    v as i64
}

fn float(v: u64) -> f64 {
    f64::from_bits(v)
}

fn int_binary(op: Op, x: i64, y: i64) -> Result<u64, FaultKind> {
    use FaultKind::{DivByZero, IntOverflow};
    let r = match op {
        Op::AddI => x.checked_add(y).ok_or(IntOverflow)?,
        Op::SubI => x.checked_sub(y).ok_or(IntOverflow)?,
        Op::MulI => x.checked_mul(y).ok_or(IntOverflow)?,
        Op::DivI if y == 0 => return Err(DivByZero),
        Op::DivI => x.checked_div(y).ok_or(IntOverflow)?,
        Op::RemI if y == 0 => return Err(DivByZero),
        Op::RemI => x.checked_rem(y).ok_or(IntOverflow)?,
        Op::EqI => return Ok(u64::from(x == y)),
        Op::NeI => return Ok(u64::from(x != y)),
        Op::LtI => return Ok(u64::from(x < y)),
        Op::LeI => return Ok(u64::from(x <= y)),
        _ => return Err(FaultKind::Corrupt),
    };
    Ok(r as u64)
}

/// IEEE operations and comparisons only (bit-identical everywhere); a NaN result is canonical.
fn float_binary(op: Op, x: f64, y: f64) -> Result<u64, FaultKind> {
    Ok(match op {
        Op::AddF => canonical_bits(x + y),
        Op::SubF => canonical_bits(x - y),
        Op::MulF => canonical_bits(x * y),
        Op::DivF => canonical_bits(x / y),
        Op::EqF => u64::from(x == y),
        Op::NeF => u64::from(x != y),
        Op::LtF => u64::from(x < y),
        Op::LeF => u64::from(x <= y),
        _ => return Err(FaultKind::Corrupt),
    })
}

/// Truncates toward zero; NaN and anything outside `[-2^63, 2^63)` is `BadFloatToInt`.
fn float_to_int(x: f64) -> Result<u64, FaultKind> {
    if x.is_nan() || x >= TWO_POW_63 || x < -TWO_POW_63 {
        return Err(FaultKind::BadFloatToInt);
    }
    Ok(x as i64 as u64)
}

impl Vm {
    pub(crate) fn rd(&self, slot: u16) -> Exec<u64> {
        Ok(self.store.r(self.store.layout.slot_off(slot))?)
    }

    pub(crate) fn wt(&mut self, slot: u16, v: u64) -> Exec<()> {
        let off = self.store.layout.slot_off(slot);
        Ok(self.store.ws(off, v)?)
    }

    fn header(&self, field: u32) -> Exec<u64> {
        Ok(self.store.r(field * 8)?)
    }

    fn set_header(&mut self, field: u32, v: u64) -> Exec<()> {
        Ok(self.store.wr(field * 8, v)?)
    }

    /// The running function and pc, from the header.
    fn load_regs(&self) -> Exec<(u16, u32)> {
        let fn_id = u16::try_from(self.header(H_CURRENT_FN)?)
            .map_err(|_| Fault::corrupt("the current function is out of range"))?;
        let pc = u32::try_from(self.header(H_PC)?)
            .map_err(|_| Fault::corrupt("the pc is out of range"))?;
        Ok((fn_id, pc))
    }

    /// The instruction at `pc`, which must lie inside `fn_id`'s code.
    fn fetch(&self, fn_id: u16, pc: u32) -> Exec<Instr> {
        let p = &self.program;
        match p.fns.get(usize::from(fn_id)) {
            Some(f) if f.entry <= pc && pc < f.end => p
                .code
                .get(pc as usize)
                .copied()
                .ok_or_else(|| Fault::corrupt("the pc is outside the code")),
            Some(_) => Err(Fault::corrupt("the pc is outside the current function")),
            None => Err(Fault::corrupt("the current function does not exist")),
        }
    }

    pub(crate) fn run(&mut self, fuel: u64, _host: &mut dyn Host) -> DebugRun {
        match self.run_loop(fuel) {
            Ok(r) => r,
            // A header that cannot be read or written: no position to report.
            Err(f) => DebugRun {
                fuel_used: 0,
                end: RunEnd::Faulted {
                    kind: f.kind,
                    message: f.message,
                    stack: Vec::new(),
                },
            },
        }
    }

    /// `Err` only when the header itself fails; an instruction's fault is a `RunEnd::Faulted`.
    fn run_loop(&mut self, fuel: u64) -> Exec<DebugRun> {
        match self.header(H_STATUS)? {
            STATUS_IDLE => {
                return Ok(DebugRun {
                    fuel_used: 0,
                    end: RunEnd::Returned,
                });
            }
            STATUS_RUNNING => {}
            _ => return Err(Fault::corrupt("unknown status")),
        }
        let debt = self.header(H_FUEL_DEBT)?;
        if fuel <= debt {
            self.set_header(H_FUEL_DEBT, debt - fuel)?;
            return Ok(DebugRun {
                fuel_used: 0,
                end: RunEnd::OutOfFuel,
            });
        }
        let mut m = Meter::new(fuel - debt);
        let (mut fn_id, mut pc) = self.load_regs()?;
        let stop = loop {
            if m.left == 0 {
                break Stop::Fuel;
            }
            match self.reap_one(&mut m) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(f) => break Stop::Fault(f),
            }
            let ins = match self.fetch(fn_id, pc) {
                Ok(i) => i,
                Err(f) => break Stop::Fault(f),
            };
            m.charge(base_fuel(&self.program, ins));
            let before = self.store.mem.checkpoint();
            match self.exec(fn_id, pc, ins, &mut m) {
                Ok(Flow::Next) => pc = pc.saturating_add(1),
                Ok(Flow::Goto(t)) => pc = t,
                Ok(Flow::Enter { fn_id: f, pc: p }) => (fn_id, pc) = (f, p),
                Ok(Flow::Done) => break Stop::Returned,
                Err(f) => {
                    if undone_on_fault(ins.op, f.kind) {
                        // No partial effect (module docs). The checkpoint was taken a moment ago, so
                        // its history cannot have been dropped.
                        let _ = self.store.mem.rollback_to(before);
                    }
                    break Stop::Fault(f);
                }
            }
        };
        self.set_header(H_PC, u64::from(pc))?;
        self.set_header(H_CURRENT_FN, u64::from(fn_id))?;
        self.set_header(H_FUEL_DEBT, m.debt)?;
        let status = match stop {
            Stop::Fuel => STATUS_RUNNING,
            Stop::Returned | Stop::Fault(_) => STATUS_IDLE,
        };
        self.set_header(H_STATUS, status)?;
        let end = match stop {
            Stop::Fuel => RunEnd::OutOfFuel,
            Stop::Returned => RunEnd::Returned,
            Stop::Fault(f) => RunEnd::Faulted {
                kind: f.kind,
                message: f.message,
                stack: self.fault_stack(fn_id, pc),
            },
        };
        Ok(DebugRun {
            fuel_used: m.used,
            end,
        })
    }

    /// One reap step when the zombie list is not empty (`true`), charged `FUEL_REAP` plus its work.
    fn reap_one(&mut self, m: &mut Meter) -> Exec<bool> {
        match reap_step(&mut self.store, self.heap, &*self.program)? {
            Some(words) => {
                m.charge(FUEL_REAP);
                m.words(words);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The faulting instruction's location, then each caller's call site, found by walking the return
    /// links outwards. A link that does not lead to a call site ends the walk; the walk is bounded by
    /// the number of functions (the call graph is acyclic, so no valid chain is longer).
    fn fault_stack(&self, fn_id: u16, pc: u32) -> Vec<SrcLoc> {
        let p = &self.program;
        let mut stack = vec![p.loc(pc)];
        let mut cur = fn_id;
        for _ in 0..p.fns.len() {
            let Some(f) = p.fns.get(usize::from(cur)) else {
                break;
            };
            let Ok(link) = self.rd(f.frame_base) else {
                break;
            };
            let Some((caller, ret_pc)) = return_site(p, link, cur) else {
                break;
            };
            stack.push(p.loc(ret_pc - 1));
            cur = caller;
        }
        stack
    }

    /// Executes one instruction of `fn_id` at `pc`.
    fn exec(&mut self, fn_id: u16, pc: u32, ins: Instr, m: &mut Meter) -> Exec<Flow> {
        let Instr { op, a, b, c } = ins;
        match op {
            Op::Nop => {}
            Op::LoadSmall => self.wt(a, b as i16 as i64 as u64)?,
            Op::LoadConst => {
                let v = *self
                    .program
                    .consts
                    .get(ins.t() as usize)
                    .ok_or_else(|| Fault::corrupt("constant out of range"))?;
                self.wt(a, v)?;
            }
            Op::Copy => {
                let v = self.rd(b)?;
                self.wt(a, v)?;
            }
            Op::AddI
            | Op::SubI
            | Op::MulI
            | Op::DivI
            | Op::RemI
            | Op::EqI
            | Op::NeI
            | Op::LtI
            | Op::LeI => {
                let v = int_binary(op, int(self.rd(b)?), int(self.rd(c)?))?;
                self.wt(a, v)?;
            }
            Op::NegI => {
                let v = int(self.rd(b)?)
                    .checked_neg()
                    .ok_or(FaultKind::IntOverflow)?;
                self.wt(a, v as u64)?;
            }
            Op::AddF | Op::SubF | Op::MulF | Op::DivF | Op::EqF | Op::NeF | Op::LtF | Op::LeF => {
                let v = float_binary(op, float(self.rd(b)?), float(self.rd(c)?))?;
                self.wt(a, v)?;
            }
            Op::NegF => {
                let v = canonical_bits(-float(self.rd(b)?));
                self.wt(a, v)?;
            }
            Op::IntToFloat => {
                let v = canonical_bits(int(self.rd(b)?) as f64);
                self.wt(a, v)?;
            }
            Op::FloatToInt => {
                let v = float_to_int(float(self.rd(b)?))?;
                self.wt(a, v)?;
            }
            Op::Not => {
                let v = u64::from(self.rd(b)? == 0);
                self.wt(a, v)?;
            }
            Op::Jmp => return Ok(Flow::Goto(ins.t())),
            Op::JmpIf | Op::JmpIfNot => {
                let taken = (self.rd(a)? != 0) == (op == Op::JmpIf);
                if taken {
                    return Ok(Flow::Goto(ins.t()));
                }
            }
            Op::Call => return self.call(fn_id, pc, a),
            Op::Ret => return self.ret(fn_id),
            Op::TracePush => self.trace_push(pc, a == 1)?,

            Op::LoadStr
            | Op::Dup
            | Op::Move
            | Op::Drop
            | Op::Eq
            | Op::NewObj
            | Op::GetField
            | Op::TakeField
            | Op::SetField
            | Op::GetTag
            | Op::MakeUnique
            | Op::NewList
            | Op::ListLen
            | Op::GetIndex
            | Op::TakeIndex
            | Op::SetIndex
            | Op::NewDict
            | Op::GetKey
            | Op::TakeKey
            | Op::SetKey => self.exec_heap(ins, m)?,
            Op::CallBuiltin => self.call_builtin(ins, m)?,
            Op::RefMake
            | Op::RefField
            | Op::RefIndex
            | Op::RefKey
            | Op::RefLoad
            | Op::RefTake
            | Op::RefStore => self.exec_ref(ins, m)?,

            // ---- Not implemented yet: Tasks 12–13 replace this arm with their groups. ----
            Op::CallHost | Op::Await | Op::AwaitAll | Op::PromiseDetach => {
                return Err(Fault::corrupt("unsupported instruction"));
            }
        }
        Ok(Flow::Next)
    }

    /// Writes the return link (`return pc | caller << 32`) into the callee's frame word 0 and enters it.
    fn call(&mut self, fn_id: u16, pc: u32, callee: u16) -> Exec<Flow> {
        let f = self
            .program
            .fns
            .get(usize::from(callee))
            .ok_or_else(|| Fault::corrupt("call of an unknown function"))?;
        let (entry, frame) = (f.entry, f.frame_base);
        let ret_pc = pc
            .checked_add(1)
            .ok_or_else(|| Fault::corrupt("call at the last pc"))?;
        self.wt(frame, u64::from(ret_pc) | u64::from(fn_id) << 32)?;
        Ok(Flow::Enter {
            fn_id: callee,
            pc: entry,
        })
    }

    /// Returns along the link in the current frame's word 0: the sentinel ends the run.
    fn ret(&mut self, fn_id: u16) -> Exec<Flow> {
        let frame = self
            .program
            .fns
            .get(usize::from(fn_id))
            .ok_or_else(|| Fault::corrupt("return from an unknown function"))?
            .frame_base;
        let link = self.rd(frame)?;
        if link == LINK_SENTINEL {
            return Ok(Flow::Done);
        }
        match return_site(&self.program, link, fn_id) {
            Some((caller, pc)) => Ok(Flow::Enter { fn_id: caller, pc }),
            None => Err(Fault::corrupt("corrupt return link")),
        }
    }

    /// Appends `pc` to the error trace (`clear`: empties it first); entries past `ERR_TRACE_MAX` are
    /// counted in `H_TRACE_DROPPED`.
    fn trace_push(&mut self, pc: u32, clear: bool) -> Exec<()> {
        if clear {
            self.set_header(H_TRACE_LEN, 0)?;
            self.set_header(H_TRACE_DROPPED, 0)?;
        }
        let len = self.header(H_TRACE_LEN)?;
        let max = ERR_TRACE_MAX as u64;
        if len > max {
            return Err(Fault::corrupt("error trace length out of range"));
        }
        if len < max {
            // len < ERR_TRACE_MAX, so the entry lies inside the trace region
            let off = self.store.layout.trace_base + (len as u32) * 8;
            self.store.wr(off, u64::from(pc))?;
            self.set_header(H_TRACE_LEN, len + 1)
        } else {
            let dropped = self.header(H_TRACE_DROPPED)?;
            self.set_header(H_TRACE_DROPPED, dropped.saturating_add(1))
        }
    }
}

/// Why the loop stopped.
enum Stop {
    Fuel,
    Returned,
    Fault(Fault),
}
