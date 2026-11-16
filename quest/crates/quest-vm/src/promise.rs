//! Promises and parked tasks (design D7, spec §8.2): requests, `await`, resolution, resume and timeouts.
//!
//! Everything lives in the flat memory and goes through the store's logged write paths, so checkpoints,
//! rollback and the state hash cover it. The tables are runtime regions: a segment abort does not restore
//! them, the cleanup below does what an abort must.
//!
//! # Promise records (the promises region: `PROMISE_MAX` records of `PROMISE_REC_WORDS` words)
//! Word 0 `state: u8 | outcome: u8 << 8 | flags: u8 << 16 | host fn: u16 << 32`, word 1 the id, word 2
//! the deadline tick, word 3 the owner (index + 1 of the parked record awaiting it, 0 = none), word 4 the
//! answer (a `Raw` block of wire bytes in the inbox heap, for the outcomes `ok` and `err`), word 5 zero.
//! A free record is all zero. Ids are `epoch << 32 | serial`; `H_PROMISE_SERIAL` is the last serial
//! issued, so the first is 1 and an id is never 0.
//! * `PENDING`: issued in the open segment, its request in the command buffer next to the segment's
//!   commands (so the host sees both in issue order). The commit makes it `SENT` with the deadline
//!   `now + PROMISE_TIMEOUT_TICKS`; an abort frees it and gives its serial back.
//! * `SENT`: the host has the request. [`Vm::resolve`], or `tick` once the deadline is reached
//!   (`Timeout`), makes it `RESOLVED`. An `err` answer must be a variant of the program's error enum and
//!   any other is stored as `lost`, as is an answer the inbox has no room for. So the stored bytes always
//!   decode: a decode failure at resume is corrupt memory.
//! * `RESOLVED`: the answer waits for the task awaiting it, which frees the record when it resumes.
//! * `PromiseDetach` flags the record `DETACHING` in the open segment: `DETACHED` at its commit (and
//!   freed then when already resolved), cleared at its abort. A detached record is freed when it
//!   resolves; nobody awaits it.
//!
//! # Parked records (the parked region: `PARKED_MAX` records of `PARKED_REC_WORDS` words)
//! Word 0 `state: u8 | op: u8 << 8 | task kind: u8 << 16 | task arg: u16 << 32`, word 1 the `Parked`
//! object, word 2 the arrival number (once ready), word 3 `pc | fn id << 32` of the await. `WAITING`
//! until every promise it awaits is resolved, then `READY` with the next arrival number: the counter
//! events take theirs from (decision 5), so the scheduler resumes continuations and dispatches events
//! in one order, but a continuation takes no queue slot (the parked cap bounds them). A resumed task
//! runs as the task that parked (its kind and identity come back from the record; there is no separate
//! resume kind), so its end, its reports and its handler's event release are those of that task.
//!
//! # The `Parked` object (script heap)
//! The frames of the parked call chain, each as its frame words followed by a trailer word
//! `fn id | frame words << 16`; `len` is the payload words in use and `aux` the reaper's cursor (the
//! heap-kind words of the last frame released so far). Parking moves the frame words in (heap kinds are
//! not increfed: the object owns them now) and zeroes the frames, so other tasks may use them. Its parked
//! record owns the object. Resuming moves the words back (releasing whatever heap value a frame word holds
//! then) and frees the object, all inside the resumed task's segment: an abort of that segment brings the
//! object back, and the cleanup releases it to the reaper (`H_RESUME_PARKED` names it), which releases
//! the heap-kind words frame by frame, from the last, at most `REAP_CHILDREN_MAX` steps at a time.
//!
//! # `Await a, b, c` / `AwaitAll a, b, c`
//! Only a handler or `main` may park (an initialiser that awaits faults `HostFault`). Every promise is
//! checked first (a live record, not awaited, not detached, no duplicates: else `PromiseAlreadyAwaited`),
//! then that a parked record is free (`TooManyParked`); only then does anything change: the `Parked`
//! object is built (a `HeapFull` here is undone with the instruction), the promises point at the parked
//! record, and the task ends its segment, which the scheduler commits. An await always yields, even on a
//! resolved promise (decision 4): that continuation is ready at once with a fresh arrival number. On
//! resume the answer goes to slot `a` (loaded against type `c`) and slot `a + 1` gets the error (0 = none,
//! else an error value as `raise` builds it); a value that does not fit `c` is the error `WrongType`. For
//! `AwaitAll`, slot `b` holds a `list[Promise[T]]`, `c` is the type of the result (`list[T]`), the values
//! come in list order and the first failure in list order is the error (decision 2).

use std::sync::Arc;

use crate::alloc::{Heap, Kind, ObjHeader};
use crate::bytecode::{Instr, Op};
use crate::consts::{PARKED_MAX, PROMISE_MAX, PROMISE_TIMEOUT_TICKS};
use crate::fault::{FaultKind, VmResult};
use crate::fuel::FUEL_RESUME;
use crate::host::{AbortCause, Host};
use crate::interp::{Exec, Fault, Flow, Meter, return_site};
use crate::layout::{
    H_ARRIVAL, H_CURRENT_FN, H_EPOCH, H_PC, H_PROMISE_SERIAL, H_RESUME_PARKED, H_STATUS, H_TASK,
    H_TASK_ARG, H_TRACE_DROPPED, H_TRACE_LEN, LINK_SENTINEL, PARKED_REC_WORDS, PROMISE_REC_WORDS,
    STATUS_RUNNING,
};
use crate::list;
use crate::object::{
    FrameMaps, alloc, capacity, hdr, ptr_of, read_slot, release, write_header, write_slot,
};
use crate::sched::{CMD_REQUEST, TASK_HANDLER, TASK_MAIN, corrupt, word_off};
use crate::store::Store;
use crate::types::TypeDesc;
use crate::vm::Vm;
use crate::wire::{FromWire, WireValue, fits, from_wire};

/// What [`Vm::resolve`] did with an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// Stored; the task awaiting it (if any) resumes in arrival order.
    Accepted,
    /// Not an outstanding promise of this instance; nothing changed.
    Unknown,
}

// Promise record states, outcomes and flags.
const P_FREE: u8 = 0;
const P_PENDING: u8 = 1;
const P_SENT: u8 = 2;
const P_RESOLVED: u8 = 3;
const O_OK: u8 = 0;
const O_ERR: u8 = 1;
const O_LOST: u8 = 2;
const O_TIMEOUT: u8 = 3;
const F_DETACHING: u8 = 1;
const F_DETACHED: u8 = 2;
// Parked record states and ops.
const K_FREE: u8 = 0;
const K_WAITING: u8 = 1;
const K_READY: u8 = 2;
const OP_AWAIT: u8 = 0;
const OP_AWAIT_ALL: u8 = 1;

const PROM_WORDS: usize = PROMISE_REC_WORDS as usize;
const PARK_WORDS: usize = PARKED_REC_WORDS as usize;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PromRec {
    state: u8,
    outcome: u8,
    flags: u8,
    host_fn: u16,
    id: u64,
    deadline: u64,
    owner: u64,
    answer: u64,
}

impl PromRec {
    fn encode(&self) -> [u64; PROM_WORDS] {
        let w0 = u64::from(self.state)
            | u64::from(self.outcome) << 8
            | u64::from(self.flags) << 16
            | u64::from(self.host_fn) << 32;
        [w0, self.id, self.deadline, self.owner, self.answer, 0]
    }

    /// A record read from memory: no stray bits, every field in range.
    fn decode(w: [u64; PROM_WORDS]) -> Exec<PromRec> {
        let r = PromRec {
            state: w[0] as u8,
            outcome: (w[0] >> 8) as u8,
            flags: (w[0] >> 16) as u8,
            host_fn: (w[0] >> 32) as u16,
            id: w[1],
            deadline: w[2],
            owner: w[3],
            answer: w[4],
        };
        let ok = r.encode() == w
            && r.state <= P_RESOLVED
            && r.outcome <= O_TIMEOUT
            && r.flags & !(F_DETACHING | F_DETACHED) == 0
            && r.owner <= PARKED_MAX as u64
            && (r.state != P_FREE || r == PromRec::default());
        if ok {
            Ok(r)
        } else {
            corrupt("a promise record does not parse")
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct ParkRec {
    state: u8,
    op: u8,
    task_kind: u8,
    task_arg: u16,
    obj: u64,
    arrival: u64,
    pc: u32,
    fn_id: u16,
}

impl ParkRec {
    fn encode(&self) -> [u64; PARK_WORDS] {
        let w0 = u64::from(self.state)
            | u64::from(self.op) << 8
            | u64::from(self.task_kind) << 16
            | u64::from(self.task_arg) << 32;
        [
            w0,
            self.obj,
            self.arrival,
            u64::from(self.pc) | u64::from(self.fn_id) << 32,
        ]
    }

    fn decode(w: [u64; PARK_WORDS]) -> Exec<ParkRec> {
        let r = ParkRec {
            state: w[0] as u8,
            op: (w[0] >> 8) as u8,
            task_kind: (w[0] >> 16) as u8,
            task_arg: (w[0] >> 32) as u16,
            obj: w[1],
            arrival: w[2],
            pc: w[3] as u32,
            fn_id: (w[3] >> 32) as u16,
        };
        let task = u64::from(r.task_kind);
        let ok = r.encode() == w
            && r.state <= K_READY
            && r.op <= OP_AWAIT_ALL
            && match r.state {
                K_FREE => r == ParkRec::default(),
                _ => (task == TASK_MAIN || task == TASK_HANDLER) && r.obj != 0,
            };
        if ok {
            Ok(r)
        } else {
            corrupt("a parked record does not parse")
        }
    }
}

type Outcome = Result<WireValue, WireValue>;

fn already_awaited() -> Fault {
    FaultKind::PromiseAlreadyAwaited.into()
}

fn variant(name: &str) -> WireValue {
    WireValue::Variant {
        name: name.into(),
        fields: Vec::new(),
    }
}

/// A `Parked` object's frame trailer: the function and its frame words.
fn trailer(t: u64) -> VmResult<(u16, u16)> {
    if t >> 32 != 0 {
        return Err(FaultKind::Corrupt);
    }
    Ok((t as u16, (t >> 16) as u16))
}

/// One reap step's worth of a `Parked` zombie (see the module docs): releases the last frame's
/// heap-kind words (`aux` counts them), then drops the frame (`len` shrinks). Each release and each
/// dropped frame takes one unit of `budget`.
pub(crate) fn reap_parked(
    st: &mut Store,
    heap: Heap,
    p: u32,
    h: &mut ObjHeader,
    maps: &dyn FrameMaps,
    budget: &mut usize,
    work: &mut u32,
) -> VmResult<()> {
    if h.len > capacity(h) {
        return Err(FaultKind::Corrupt);
    }
    while *budget > 0 && h.len > 0 {
        let (f, fw) = trailer(read_slot(st, p, h, h.len - 1)?)?;
        let start = (h.len - 1)
            .checked_sub(u32::from(fw))
            .ok_or(FaultKind::Corrupt)?;
        let refs = maps.ref_slots(f);
        let done = h.aux as usize;
        if done < refs.len() {
            let w = *refs.get(refs.len() - 1 - done).ok_or(FaultKind::Corrupt)?;
            if w >= fw {
                return Err(FaultKind::Corrupt);
            }
            let child = read_slot(st, p, h, start + u32::from(w))?;
            release(st, heap, child)?;
            h.aux += 1;
        } else if done == refs.len() {
            h.len = start;
            h.aux = 0;
        } else {
            return Err(FaultKind::Corrupt);
        }
        *budget -= 1;
        *work += 1;
    }
    Ok(())
}

impl Vm {
    // ---- the tables ----------------------------------------------------------------------------------

    fn prom_at(&self, i: usize) -> Exec<u32> {
        word_off(
            self.store.layout.promises_base,
            i as u64 * u64::from(PROMISE_REC_WORDS),
        )
    }

    fn read_prom(&self, i: usize) -> Exec<PromRec> {
        let at = self.prom_at(i)?;
        let mut w = [0u64; PROM_WORDS];
        for (k, x) in (0u64..).zip(w.iter_mut()) {
            *x = self.store.r(word_off(at, k)?)?;
        }
        PromRec::decode(w)
    }

    fn write_prom(&mut self, i: usize, r: &PromRec) -> Exec<()> {
        let at = self.prom_at(i)?;
        for (k, v) in (0u64..).zip(r.encode()) {
            self.store.wr(word_off(at, k)?, v)?;
        }
        Ok(())
    }

    fn park_at(&self, k: usize) -> Exec<u32> {
        word_off(
            self.store.layout.parked_base,
            k as u64 * u64::from(PARKED_REC_WORDS),
        )
    }

    fn read_park(&self, k: usize) -> Exec<ParkRec> {
        let at = self.park_at(k)?;
        let mut w = [0u64; PARK_WORDS];
        for (j, x) in (0u64..).zip(w.iter_mut()) {
            *x = self.store.r(word_off(at, j)?)?;
        }
        ParkRec::decode(w)
    }

    fn write_park(&mut self, k: usize, r: &ParkRec) -> Exec<()> {
        let at = self.park_at(k)?;
        for (j, v) in (0u64..).zip(r.encode()) {
            self.store.wr(word_off(at, j)?, v)?;
        }
        Ok(())
    }

    /// The live record of promise `id`.
    fn find_promise(&self, id: u64) -> Exec<Option<usize>> {
        for i in 0..PROMISE_MAX {
            let r = self.read_prom(i)?;
            if r.state != P_FREE && r.id == id {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    fn next_arrival(&mut self) -> Exec<u64> {
        let a = self.header(H_ARRIVAL)?;
        self.set_header(H_ARRIVAL, a.wrapping_add(1))?;
        Ok(a)
    }

    /// Frees promise record `i` and its answer.
    pub(crate) fn free_promise(&mut self, i: usize) -> Exec<()> {
        let r = self.read_prom(i)?;
        if r.answer != 0 {
            self.inbox.free(&mut self.store, ptr_of(r.answer)?)?;
        }
        self.write_prom(i, &PromRec::default())
    }

    /// The parked tasks, waiting or ready: what an edit cancels (`migrate`).
    pub(crate) fn parked_count(&self) -> Exec<u32> {
        let mut n = 0;
        for k in 0..PARKED_MAX {
            if self.read_park(k)?.state != K_FREE {
                n += 1;
            }
        }
        Ok(n)
    }

    // ---- requests and the segment's end --------------------------------------------------------------

    /// `CallHost` of a promise site: takes a record (`TooManyPromises` when none is free), buffers the
    /// request and writes the new id to `dst`.
    pub(crate) fn issue_request(
        &mut self,
        f: u16,
        args: Vec<WireValue>,
        dst: u16,
        m: &mut Meter,
    ) -> Exec<()> {
        let mut free = None;
        for i in 0..PROMISE_MAX {
            if self.read_prom(i)?.state == P_FREE {
                free = Some(i);
                break;
            }
        }
        let too_many = || Fault::from(FaultKind::TooManyPromises);
        let i = free.ok_or_else(too_many)?;
        // A serial past 32 bits would repeat an id: the instance has issued all it can.
        let serial = self
            .header(H_PROMISE_SERIAL)?
            .checked_add(1)
            .filter(|&s| s <= u64::from(u32::MAX))
            .ok_or_else(too_many)?;
        let epoch = self.header(H_EPOCH)?;
        if epoch > u64::from(u32::MAX) {
            return corrupt("epoch out of range");
        }
        let id = epoch << 32 | serial;
        let mut bytes = id.to_le_bytes().to_vec();
        WireValue::List(args).encode(&mut bytes);
        self.buffer(CMD_REQUEST, f, &bytes, m)?;
        let r = PromRec {
            state: P_PENDING,
            host_fn: f,
            id,
            ..PromRec::default()
        };
        self.write_prom(i, &r)?;
        self.set_header(H_PROMISE_SERIAL, serial)?;
        self.wt(dst, id)
    }

    /// At a segment's commit: its requests are sent (deadline `now + PROMISE_TIMEOUT_TICKS`) and its
    /// detaches made final (a detached record already resolved is freed).
    pub(crate) fn commit_promises(&mut self, now: u64) -> Exec<()> {
        for i in 0..PROMISE_MAX {
            let mut r = self.read_prom(i)?;
            if r.state == P_FREE {
                continue;
            }
            let mut changed = false;
            if r.state == P_PENDING {
                r.state = P_SENT;
                r.deadline = now.saturating_add(PROMISE_TIMEOUT_TICKS);
                changed = true;
            }
            if r.flags & F_DETACHING != 0 {
                if r.state == P_RESOLVED {
                    self.free_promise(i)?;
                    continue;
                }
                r.flags = F_DETACHED;
                changed = true;
            }
            if changed {
                self.write_prom(i, &r)?;
            }
        }
        Ok(())
    }

    /// After a segment's abort (no segment open): the records it issued are freed and their serials
    /// given back, its detaches forgotten, and the `Parked` object of a resumed task (back in the heap
    /// with the abort) released.
    pub(crate) fn abort_promises(&mut self) -> Exec<()> {
        let mut issued = 0u64;
        for i in 0..PROMISE_MAX {
            let mut r = self.read_prom(i)?;
            if r.state == P_PENDING {
                self.free_promise(i)?;
                issued += 1;
            } else if r.flags & F_DETACHING != 0 {
                r.flags &= !F_DETACHING;
                self.write_prom(i, &r)?;
            }
        }
        let serial = self
            .header(H_PROMISE_SERIAL)?
            .checked_sub(issued)
            .ok_or_else(|| Fault::corrupt("promise serial below the records issued"))?;
        self.set_header(H_PROMISE_SERIAL, serial)?;
        let obj = self.header(H_RESUME_PARKED)?;
        if obj != 0 {
            release(&mut self.store, self.heap, obj)?;
            self.set_header(H_RESUME_PARKED, 0)?;
        }
        Ok(())
    }

    // ---- await ---------------------------------------------------------------------------------------

    /// `PromiseDetach a`: nobody will await the promise; its record goes when it resolves.
    pub(crate) fn exec_detach(&mut self, slot: u16) -> Exec<()> {
        let id = self.rd(slot)?;
        let i = self.find_promise(id)?.ok_or_else(already_awaited)?;
        let mut r = self.read_prom(i)?;
        if r.owner != 0 || r.flags != 0 {
            return Err(already_awaited());
        }
        r.flags = F_DETACHING;
        self.write_prom(i, &r)
    }

    /// The promise ids of the list in `slot` (`AwaitAll`). More than `PROMISE_MAX` cannot all be
    /// distinct live promises.
    fn promise_list(&self, slot: u16) -> Exec<Vec<u64>> {
        let l = self.rd(slot)?;
        if self.list_refs(l)? {
            return corrupt("a list of promises holds heap kinds");
        }
        let n = list::len(&self.store, self.heap, l)?;
        if n as usize > PROMISE_MAX {
            return Err(already_awaited());
        }
        let mut ids = Vec::with_capacity(n as usize);
        for i in 0..n {
            ids.push(list::get(&self.store, self.heap, l, i64::from(i))?);
        }
        Ok(ids)
    }

    /// `Await a, b, c` / `AwaitAll a, b, c` at `pc` of `fn_id`: parks the task (module docs).
    pub(crate) fn exec_await(
        &mut self,
        fn_id: u16,
        pc: u32,
        ins: Instr,
        m: &mut Meter,
    ) -> Exec<Flow> {
        let task = self.header(H_TASK)?;
        if task != TASK_MAIN && task != TASK_HANDLER {
            return Err(Fault {
                kind: FaultKind::HostFault,
                message: "await outside a handler or main".into(),
            });
        }
        let arg = u16::try_from(self.header(H_TASK_ARG)?)
            .map_err(|_| Fault::corrupt("task identity out of range"))?;
        let all = ins.op == Op::AwaitAll;
        let ids = if all {
            self.promise_list(ins.b)?
        } else {
            vec![self.rd(ins.b)?]
        };
        let mut recs: Vec<usize> = Vec::with_capacity(ids.len());
        for id in ids {
            let i = self.find_promise(id)?.ok_or_else(already_awaited)?;
            let r = self.read_prom(i)?;
            if r.owner != 0 || r.flags != 0 || recs.contains(&i) {
                return Err(already_awaited());
            }
            recs.push(i);
        }
        let mut slot = None;
        for k in 0..PARKED_MAX {
            if self.read_park(k)?.state == K_FREE {
                slot = Some(k);
                break;
            }
        }
        let k = slot.ok_or_else(|| Fault::from(FaultKind::TooManyParked))?;
        // Nothing has changed yet; from here a fault is undone with the instruction.
        let (obj, words) = self.park_frames(fn_id)?;
        m.words(words);
        let mut ready = true;
        for &i in &recs {
            let mut r = self.read_prom(i)?;
            r.owner = k as u64 + 1;
            ready &= r.state == P_RESOLVED;
            self.write_prom(i, &r)?;
        }
        let arrival = if ready { self.next_arrival()? } else { 0 };
        let rec = ParkRec {
            state: if ready { K_READY } else { K_WAITING },
            op: if all { OP_AWAIT_ALL } else { OP_AWAIT },
            task_kind: task as u8,
            task_arg: arg,
            obj,
            arrival,
            pc,
            fn_id,
        };
        self.write_park(k, &rec)?;
        Ok(Flow::Park)
    }

    /// Moves the frames of the call chain from `fn_id` out to the sentinel into a new `Parked` object
    /// and zeroes them: the object and the words moved.
    fn park_frames(&mut self, fn_id: u16) -> Exec<(u64, u32)> {
        let p = Arc::clone(&self.program);
        let mut chain = Vec::new();
        let mut cur = fn_id;
        loop {
            let f = p
                .fns
                .get(usize::from(cur))
                .ok_or_else(|| Fault::corrupt("a frame of an unknown function"))?;
            chain.push((cur, f.frame_base, f.frame_words));
            if chain.len() > p.fns.len() {
                return corrupt("the call chain is longer than the program");
            }
            let link = self.rd(f.frame_base)?;
            if link == LINK_SENTINEL {
                break;
            }
            cur = return_site(&p, link, cur)
                .ok_or_else(|| Fault::corrupt("corrupt return link"))?
                .0;
        }
        let total: u64 = chain.iter().map(|c| u64::from(c.2) + 1).sum();
        let total =
            u32::try_from(total).map_err(|_| Fault::corrupt("parked frames out of range"))?;
        let obj = alloc(&mut self.store, self.heap, Kind::Parked, total)?;
        let mut h = self.heap.header(&self.store, obj)?;
        h.len = total;
        write_header(&mut self.store, self.heap, obj, h)?;
        let mut at = 0u32;
        for (f, base, fw) in chain {
            for w in 0..fw {
                let slot = base
                    .checked_add(w)
                    .ok_or_else(|| Fault::corrupt("frame word out of range"))?;
                let v = self.rd(slot)?;
                write_slot(&mut self.store, self.heap, obj, &h, at, v)?;
                self.wt(slot, 0)?;
                at += 1;
            }
            write_slot(
                &mut self.store,
                self.heap,
                obj,
                &h,
                at,
                u64::from(f) | u64::from(fw) << 16,
            )?;
            at += 1;
        }
        Ok((u64::from(obj), total))
    }

    // ---- resolution and timeouts ---------------------------------------------------------------------

    /// [`Vm::resolve`]: `Unknown` leaves the memory as it was.
    pub(crate) fn promise_resolve(&mut self, id: u64, outcome: Outcome) -> Resolved {
        let cp = self.store.mem.checkpoint();
        match self.try_resolve(id, outcome) {
            Ok(true) => Resolved::Accepted,
            Ok(false) | Err(_) => {
                // Taken a moment ago: its history is still there.
                let _ = self.store.mem.rollback_to(cp);
                Resolved::Unknown
            }
        }
    }

    fn try_resolve(&mut self, id: u64, outcome: Outcome) -> Exec<bool> {
        if self.failed() || id >> 32 != self.header(H_EPOCH)? {
            return Ok(false);
        }
        let Some(i) = self.find_promise(id)? else {
            return Ok(false);
        };
        if self.read_prom(i)?.state != P_SENT {
            return Ok(false);
        }
        let known_error = |w: &WireValue| {
            matches!(w, WireValue::Variant { .. })
                && self
                    .error_ty
                    .is_some_and(|t| fits(&self.program.types, t, w))
        };
        let (o, answer) = match outcome {
            Ok(w) => (O_OK, Some(w)),
            Err(w) if known_error(&w) => (O_ERR, Some(w)),
            Err(_) => (O_LOST, None),
        };
        let bytes = answer.map(|w| {
            let mut b = Vec::new();
            w.encode(&mut b);
            b
        });
        self.settle(i, o, bytes)?;
        Ok(true)
    }

    /// `SENT` record `i` resolves with `outcome` (its wire bytes for `ok` / `err`): a detached record is
    /// freed; otherwise the answer is stored (`lost` when the inbox has no room) and a task awaiting it
    /// becomes ready once all it awaits is resolved.
    fn settle(&mut self, i: usize, outcome: u8, bytes: Option<Vec<u8>>) -> Exec<()> {
        let mut r = self.read_prom(i)?;
        if r.flags & F_DETACHED != 0 {
            return self.free_promise(i);
        }
        let (outcome, answer) = match bytes {
            Some(b) => match self.inbox_store(&b)? {
                Some(ptr) => (outcome, ptr),
                None => (O_LOST, 0),
            },
            None => (outcome, 0),
        };
        r.state = P_RESOLVED;
        r.outcome = outcome;
        r.answer = answer;
        self.write_prom(i, &r)?;
        if r.owner == 0 {
            return Ok(());
        }
        let k = (r.owner - 1) as usize;
        let mut pr = self.read_park(k)?;
        if pr.state != K_WAITING {
            return corrupt("a promise resolved for a task that is not waiting");
        }
        for j in 0..PROMISE_MAX {
            let o = self.read_prom(j)?;
            if o.owner == r.owner && o.state != P_RESOLVED {
                return Ok(());
            }
        }
        pr.state = K_READY;
        pr.arrival = self.next_arrival()?;
        self.write_park(k, &pr)
    }

    /// At the start of a tick: every sent record whose deadline is `now` or earlier resolves
    /// `Timeout`, in id order.
    pub(crate) fn expire_promises(&mut self, now: u64) -> Exec<()> {
        let mut due = Vec::new();
        for i in 0..PROMISE_MAX {
            let r = self.read_prom(i)?;
            if r.state == P_SENT && r.deadline <= now {
                due.push((r.id, i));
            }
        }
        due.sort_unstable();
        for (_, i) in due {
            self.settle(i, O_TIMEOUT, None)?;
        }
        Ok(())
    }

    // ---- resume --------------------------------------------------------------------------------------

    /// The ready continuation with the lowest arrival number, and that number.
    pub(crate) fn next_ready(&self) -> Exec<Option<(usize, u64)>> {
        let mut best: Option<(usize, u64)> = None;
        for k in 0..PARKED_MAX {
            let r = self.read_park(k)?;
            if r.state == K_READY && best.is_none_or(|(_, a)| r.arrival < a) {
                best = Some((k, r.arrival));
            }
        }
        Ok(best)
    }

    /// Resumes ready continuation `k`: opens a segment, frees the parked record, restores the frames
    /// and loads the answer; the task then runs on after its await. A fault while loading aborts it.
    pub(crate) fn start_resume(
        &mut self,
        k: usize,
        m: &mut Meter,
        host: &mut dyn Host,
    ) -> Exec<()> {
        let r = self.read_park(k)?;
        if r.state != K_READY {
            return corrupt("resuming a task that is not ready");
        }
        let ins = self.fetch(r.fn_id, r.pc)?;
        if !matches!(
            (ins.op, r.op),
            (Op::Await, OP_AWAIT) | (Op::AwaitAll, OP_AWAIT_ALL)
        ) {
            return corrupt("a parked task's pc is not its await");
        }
        let next =
            r.pc.checked_add(1)
                .ok_or_else(|| Fault::corrupt("await at the last pc"))?;
        if self.store.segment_is_open() {
            return corrupt("a segment is open between tasks");
        }
        self.store.segment_open();
        for (field, v) in [
            (H_PC, u64::from(next)),
            (H_CURRENT_FN, u64::from(r.fn_id)),
            (H_STATUS, STATUS_RUNNING),
            (H_TASK, u64::from(r.task_kind)),
            (H_TASK_ARG, u64::from(r.task_arg)),
            (H_TRACE_LEN, 0),
            (H_TRACE_DROPPED, 0),
            (H_RESUME_PARKED, r.obj),
        ] {
            self.set_header(field, v)?;
        }
        self.write_park(k, &ParkRec::default())?;
        m.charge(FUEL_RESUME);
        let words = self.unpark_frames(r.obj)?;
        m.words(words);
        let answers = self.take_answers(k, m)?;
        match self.load_answers(ins, answers, m) {
            Ok(()) => Ok(()),
            Err(f) if f.kind == FaultKind::Corrupt => Err(f),
            Err(f) => {
                let stack = self.fault_stack(r.fn_id, r.pc);
                self.abort_task(
                    AbortCause::Fault {
                        kind: f.kind,
                        message: f.message,
                        stack,
                    },
                    host,
                )
            }
        }
    }

    /// Moves the parked frames back (a heap-kind word a frame holds is released first) and frees the
    /// object: the words moved.
    fn unpark_frames(&mut self, obj: u64) -> Exec<u32> {
        let p = Arc::clone(&self.program);
        let (ptr, h) = hdr(&self.store, self.heap, obj)?;
        if h.kind != Kind::Parked || h.refcount != 1 || h.aux != 0 || h.len > capacity(&h) {
            return corrupt("a parked task's frames do not parse");
        }
        let mut end = h.len;
        while end > 0 {
            let (f, fw) = trailer(read_slot(&self.store, ptr, &h, end - 1)?)?;
            let info = p
                .fns
                .get(usize::from(f))
                .filter(|i| i.frame_words == fw)
                .ok_or_else(|| Fault::corrupt("a parked frame does not match its function"))?;
            let start = (end - 1)
                .checked_sub(u32::from(fw))
                .ok_or_else(|| Fault::corrupt("a parked frame does not fit"))?;
            for w in 0..fw {
                let v = read_slot(&self.store, ptr, &h, start + u32::from(w))?;
                let slot = info
                    .frame_base
                    .checked_add(w)
                    .ok_or_else(|| Fault::corrupt("frame word out of range"))?;
                self.put(slot, v, info.ref_slots.binary_search(&w).is_ok())?;
            }
            end = start;
        }
        self.heap.free(&mut self.store, ptr)?;
        Ok(h.len)
    }

    /// The answers of the promises parked task `k` awaited, by id; their records are freed.
    fn take_answers(&mut self, k: usize, m: &mut Meter) -> Exec<Vec<(u64, Outcome)>> {
        let owner = k as u64 + 1;
        let mut out = Vec::new();
        for i in 0..PROMISE_MAX {
            let r = self.read_prom(i)?;
            if r.owner != owner {
                continue;
            }
            if r.state != P_RESOLVED {
                return corrupt("a ready task awaits an unresolved promise");
            }
            let mut stored = || -> Exec<WireValue> {
                let bytes = self.inbox_read(r.answer)?;
                m.words(u32::try_from(bytes.len().div_ceil(8)).unwrap_or(u32::MAX));
                WireValue::decode(&bytes).map_err(|_| Fault::corrupt("an answer does not decode"))
            };
            let outcome = match r.outcome {
                O_OK => Ok(stored()?),
                O_ERR => Err(stored()?),
                O_LOST => Err(variant("Lost")),
                _ => Err(variant("Timeout")),
            };
            out.push((r.id, outcome));
            self.free_promise(i)?;
        }
        Ok(out)
    }

    /// Writes the result of the resumed await `ins`: the value to slot `a` and 0 to slot `a + 1`, or
    /// the error to slot `a + 1` (module docs).
    fn load_answers(
        &mut self,
        ins: Instr,
        answers: Vec<(u64, Outcome)>,
        m: &mut Meter,
    ) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let err_slot = ins
            .a
            .checked_add(1)
            .ok_or_else(|| Fault::corrupt("await's error slot out of range"))?;
        let result = if ins.op == Op::Await {
            let [(_, outcome)] = <[_; 1]>::try_from(answers)
                .map_err(|_| Fault::corrupt("an await owns other than one promise"))?;
            outcome
        } else {
            let elem = match p.types.desc(ins.c)? {
                TypeDesc::List(t) => *t,
                _ => return corrupt("AwaitAll's type is not a list"),
            };
            let ids = self.promise_list(ins.b)?;
            let mut answers = answers;
            if ids.len() != answers.len() {
                return corrupt("AwaitAll's list differs from the promises it awaits");
            }
            let (mut items, mut failed) = (Vec::with_capacity(ids.len()), None);
            for id in ids {
                let at = answers
                    .iter()
                    .position(|(a, _)| *a == id)
                    .ok_or_else(|| Fault::corrupt("AwaitAll's list differs from its promises"))?;
                match answers.swap_remove(at).1 {
                    Ok(w) if fits(&p.types, elem, &w) => items.push(w),
                    Ok(_) => {
                        failed.get_or_insert_with(|| variant("WrongType"));
                    }
                    Err(e) => {
                        failed.get_or_insert(e);
                    }
                }
            }
            failed.map_or(Ok(WireValue::List(items)), Err)
        };
        let error = match result {
            Ok(w) => match from_wire(&mut self.store, self.heap, &p.types, ins.c, &w) {
                Ok((v, words)) => {
                    m.words(words);
                    self.put(ins.a, v, p.types.is_ref(ins.c))?;
                    return self.put(err_slot, 0, true);
                }
                Err(FromWire::WrongType) => variant("WrongType"),
                Err(FromWire::Fault(k)) => return Err(k.into()),
            },
            Err(e) => e,
        };
        let v = self.error_value(&error, m)?;
        self.put(err_slot, v, true)
    }

    /// The program's error value for the wire variant `e`; a program without that error is `HostFault`.
    fn error_value(&mut self, e: &WireValue, m: &mut Meter) -> Exec<u64> {
        let missing = || Fault {
            kind: FaultKind::HostFault,
            message: match e {
                WireValue::Variant { name, .. } => format!("the program has no error {name}"),
                _ => "a promise failed with a value that is not an error".into(),
            },
        };
        let Some(t) = self.error_ty else {
            return Err(missing());
        };
        match from_wire(&mut self.store, self.heap, &self.program.types, t, e) {
            Ok((v, words)) => {
                m.words(words);
                Ok(v)
            }
            Err(FromWire::WrongType) => Err(missing()),
            Err(FromWire::Fault(k)) => Err(k.into()),
        }
    }
}
