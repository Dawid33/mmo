//! The scheduler (design D7): tasks, abortable segments, the event queue and the command buffer.
//!
//! # State
//! Everything lives in the flat memory, written through the store's logged paths, so `checkpoint` /
//! `rollback_to` cover the scheduler too and `Vm::mem_bytes` stays the complete state:
//! * `H_INSTANCE`: ok or failed. A failed instance is inert: `tick` runs nothing, `deliver` drops.
//! * `H_TASK` / `H_TASK_ARG`: the task in progress (`TASK_*`) and its identity (the global index of an
//!   `Init` task, the handler index of a `Handler` task); `TASK_NONE` between tasks. The interpreter's
//!   own registers (`H_PC`, `H_CURRENT_FN`, `H_STATUS`) say where the task is.
//! * `H_INIT_CURSOR`: the next global to initialise; `H_QUEST_START`: whether the quest's start
//!   (`QuestInfo::start_fn`) is still to run — in the home only (`H_OWNER` 0); `H_MAIN`: whether `main`
//!   was asked for / started.
//!   `H_INIT_SKIP`: after a migration, the globals whose initialisers do not run (their values were
//!   carried over, `migrate`); the cursor steps over them.
//! * The queue region: a ring of `QUEUE_LEN` records (`H_QUEUE_HEAD`, `H_QUEUE_COUNT`) of
//!   `[arrival, first handler index, payload pointer, 0]`; the payload is the event's wire bytes in a
//!   `Raw` block of the inbox heap. `H_ARRIVAL` numbers deliveries.
//! * The event being dispatched (`H_EV_*`): taken off the queue when its first handler starts, so it
//!   does not hold a queue slot while its handlers run; `H_EV_HANDLER` is the index + 1 of its next
//!   handler (0 = none). Its payload is freed when its last handler starts.
//! * The command buffer (`cmds` region, `H_CMD_LEN` bytes used): one record per command, print or promise
//!   request, a header word (`kind: u8 | host fn: u16 << 16 | byte length: u32 << 32`) and the bytes
//!   packed into words (a command's arguments as the wire bytes of one `List`, a print's UTF-8 text, a
//!   request's id as 8 little-endian bytes followed by its arguments as a command's, an `Enqueue`'s event
//!   value as its wire bytes with the script event's index in the host-fn field). Bytes past `H_CMD_LEN`
//!   are stale and never read.
//! * The timers (`timers`).
//! * The promise and parked tables (`promise`).
//! * `H_NOW`: the latest `now` a tick was given.
//!
//! # Instances and events
//! Every instance of a program with parts runs the same program; `VmConfig::instance` (`H_OWNER`) says
//! which: only handlers whose `owner` is that instance register. [`Vm::deliver`] takes the host's events
//! and the program's script events alike. `Enqueue` buffers its event like a command; the commit delivers
//! it to the instance's own queue, with the overflow rules of a host delivery (a drop is reported as
//! `EventDropped`); an event no handler of this instance takes but another instance's does goes to
//! `Host::route` with its qualified name (one no instance takes goes nowhere).
//!
//! # Quest transitions
//! In the home of a quest program (`H_OWNER` 0), every committed segment of a handler, of `main` or of the
//! start sets `H_TRANSITIONS`; the next task to start is then `QuestInfo::transitions_fn` (after any
//! pending initialiser), in the same tick and on the same fuel. Its own segment sets it again only when it
//! entered a stage (the stage word changed), so a chain of transitions that already hold runs at once.
//!
//! # Tasks and segments
//! A task runs one function from its entry until its `Ret` on the sentinel link: the pending
//! initialisers in `program.globals` order, then the quest's start (the home only), then `main` if it
//! was asked for, then the handlers of the queued events and of the ready continuations by arrival (each
//! handler of an event its own task, in table order; a continuation resumes a parked task, `promise`). Every task opens a segment when it
//! starts or resumes. A segment ends at the task's end or at an `await`, which parks the task. At the end
//! the function's error word (frame word 2) is checked: non-zero is an escaping error. Otherwise the
//! task's result is stored (an initialiser's return value is moved into its global; a handler's event
//! parameter is released) and the segment commits: the buffered records are read out, the promise
//! records the segment issued are marked sent (their deadline set), the segment commits, the buffer and
//! the task are cleared, and only then are the commands, prints and requests handed to the host in
//! order (the host never sees an effect the memory has not committed). A fault or an escaping error
//! aborts the segment (the script regions return to the segment's start), clears the buffer, frees the
//! promise records the segment issued (`promise`), undoes its timers (`timers`) and reports
//! `Report::HandlerAborted`
//! (`Report::InitFailed` for an initialiser, which also fails the instance); error data too large to
//! convert for the report turns the cause into that fault. Only a `Corrupt` fault (memory that does not
//! parse) fails the instance without an abort.
//!
//! # Idle
//! An instance is idle when no task is in progress, nothing is runnable (no pending initialiser, no
//! wanted `main`, no event queued or being dispatched, no ready continuation; a task parked on a promise
//! that is not resolved yet is not runnable) and the zombie list of the script heap is empty:
//! `tick` reaps zombies between tasks (fuel-charged, like the run loop), and a tick that leaves zombies
//! reports `Busy`. So an idle instance is quiescent and reaped, which leak checks rely on.

use std::sync::Arc;

use crate::alloc::Kind;
use crate::bytecode::Instr;
use crate::consts::{ERR_TRACE_MAX, QUEUE_LEN};
use crate::fault::FaultKind;
use crate::fuel::FUEL_DISPATCH;
use crate::host::{
    AbortCause, DropReason, Host, HostFnKind, PromiseId, QueryError, Report, SrcLoc,
};
use crate::interp::{Exec, Fault, Meter};
use crate::layout::{
    H_ARRIVAL, H_CMD_LEN, H_CURRENT_FN, H_EV_ARRIVAL, H_EV_HANDLER, H_EV_PAYLOAD, H_FUEL_DEBT,
    H_INIT_CURSOR, H_INIT_SKIP, H_INSTANCE, H_MAIN, H_NOW, H_PC, H_QUEST_START, H_QUEUE_COUNT,
    H_QUEUE_HEAD, H_RESUME_PARKED, H_STATUS, H_TASK, H_TASK_ARG, H_TRACE_DROPPED, H_TRACE_LEN,
    H_TRANSITIONS, LINK_SENTINEL, QUEUE_REC_WORDS, STATUS_IDLE, STATUS_RUNNING,
};
use crate::object::{capacity, hdr, obj, ptr_of, read_slot, write_slot};
use crate::program::HostSite;
use crate::types::{ERROR_ENUM_NAME, TypeDesc, TypeId, TypeTable};
use crate::vm::{RunEnd, Vm};
use crate::wire::{self, FromWire, WireValue, from_wire, to_wire};

/// What [`Vm::deliver`] did with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Stored; no task was in progress.
    Accepted,
    /// Stored while a task was in progress: the owner's warning (decision 6).
    Queued,
    /// Not stored; nothing changed.
    Dropped(DropReason),
}

/// The outcome of one [`Vm::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickReport {
    /// Fuel charged during this tick; the last charge may exceed the budget (the excess is a debt the
    /// next ticks repay first).
    pub fuel_used: u64,
    pub status: Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing in progress, nothing runnable, nothing left to reap.
    Idle,
    /// More to run: a task in progress, a runnable task or zombies to reap.
    Busy,
    /// An initialiser failed or the memory is corrupt; inert.
    Failed,
}

// `H_TASK`. A resumed task keeps the kind and identity of the task that parked.
const TASK_NONE: u64 = 0;
const TASK_INIT: u64 = 1;
pub(crate) const TASK_MAIN: u64 = 2;
pub(crate) const TASK_HANDLER: u64 = 3;
/// The quest's start (`QuestInfo::start_fn`).
pub(crate) const TASK_START: u64 = 4;
/// The quest's transitions (`QuestInfo::transitions_fn`). Its argument (`H_TASK_ARG`) is the stage word it
/// started in, or [`STAGE_UNKNOWN`].
const TASK_TRANSITIONS: u64 = 5;
/// A transitions task's argument when the stage word does not fit a `u16` (an immediate is odd, so 0 is
/// never a stage).
const STAGE_UNKNOWN: u16 = 0;
// `H_QUEST_START`.
pub(crate) const QSTART_NONE: u64 = 0;
pub(crate) const QSTART_WANTED: u64 = 1;
pub(crate) const QSTART_DONE: u64 = 2;
// `H_MAIN`.
const MAIN_NONE: u64 = 0;
const MAIN_WANTED: u64 = 1;
const MAIN_STARTED: u64 = 2;
// `H_INSTANCE`.
const INSTANCE_OK: u64 = 0;
const INSTANCE_FAILED: u64 = 1;
// Command record kinds.
const CMD_COMMAND: u64 = 0;
const CMD_PRINT: u64 = 1;
pub(crate) const CMD_REQUEST: u64 = 2;
const CMD_ENQUEUE: u64 = 3;

const WORD: u64 = 8;
/// Frame words: the return link, the return value, the error value, the first parameter.
const FRAME_RET: u16 = 1;
const FRAME_ERR: u16 = 2;
const FRAME_PARAM0: u16 = 3;

/// A buffered effect, read back at commit.
enum Buffered {
    Command(u16, Vec<WireValue>),
    Print(String),
    Request(u64, u16, Vec<WireValue>),
    /// A script event (its index) and its value.
    Enqueue(u16, WireValue),
}

/// The task in progress, from the header.
#[derive(Clone, Copy)]
struct Task {
    kind: u64,
    arg: u16,
    fn_id: u16,
}

pub(crate) fn corrupt<T>(why: &str) -> Exec<T> {
    Err(Fault::corrupt(why))
}

/// The id of the program's error enum (the one `Enum` named [`ERROR_ENUM_NAME`]), if it has errors.
pub(crate) fn error_type(types: &TypeTable) -> Option<TypeId> {
    let i = types
        .0
        .iter()
        .position(|d| matches!(d, TypeDesc::Enum { name, .. } if name == ERROR_ENUM_NAME))?;
    TypeId::try_from(i).ok()
}

/// `base + i` words, as a byte offset.
pub(crate) fn word_off(base: u32, i: u64) -> Exec<u32> {
    u32::try_from(u64::from(base) + i * WORD).map_err(|_| Fault::corrupt("offset out of range"))
}

impl Vm {
    fn frame_slot(&self, fn_id: u16, word: u16) -> Exec<u16> {
        let f = self
            .program
            .fns
            .get(usize::from(fn_id))
            .ok_or_else(|| Fault::corrupt("a task of an unknown function"))?;
        f.frame_base
            .checked_add(word)
            .ok_or_else(|| Fault::corrupt("frame word out of range"))
    }

    pub(crate) fn failed(&self) -> bool {
        self.header(H_INSTANCE) != Ok(INSTANCE_OK)
    }

    pub(crate) fn sched_status(&self) -> Status {
        if self.failed() {
            return Status::Failed;
        }
        match self.busy() {
            Ok(false) => Status::Idle,
            Ok(true) => Status::Busy,
            Err(_) => Status::Failed,
        }
    }

    pub(crate) fn task_in_progress(&self) -> Exec<bool> {
        Ok(self.header(H_TASK)? != TASK_NONE)
    }

    /// A task in progress, a task to start, or zombies to reap.
    fn busy(&self) -> Exec<bool> {
        let init_pending = self.header(H_INIT_CURSOR)? < self.program.globals.len() as u64;
        Ok(self.task_in_progress()?
            || init_pending
            || self.header(H_QUEST_START)? == QSTART_WANTED
            || self.header(H_TRANSITIONS)? != 0
            || self.header(H_MAIN)? == MAIN_WANTED
            || self.header(H_EV_HANDLER)? != 0
            || self.header(H_QUEUE_COUNT)? != 0
            || self.next_ready()?.is_some()
            || self.heap.zombie_head(&self.store)? != 0)
    }

    pub(crate) fn sched_start_main(&mut self) -> bool {
        if self.program.main_fn.is_none() || self.failed() {
            return false;
        }
        match self.header(H_MAIN) {
            Ok(MAIN_NONE) => self.set_header(H_MAIN, MAIN_WANTED).is_ok(),
            _ => false,
        }
    }

    /// [`Vm::restart_main`]: asks for `main` again when it already ran and nothing is in progress.
    pub(crate) fn sched_restart_main(&mut self) -> bool {
        if self.program.main_fn.is_none() || self.failed() || self.status() != Status::Idle {
            return false;
        }
        match self.header(H_MAIN) {
            Ok(MAIN_STARTED) => self.set_header(H_MAIN, MAIN_WANTED).is_ok(),
            _ => false,
        }
    }

    // ---- deliver -------------------------------------------------------------------------------------

    /// [`Vm::deliver`]: a drop, or memory that does not parse, leaves the memory as it was.
    pub(crate) fn sched_deliver(&mut self, event: &str, payload: &WireValue) -> Delivery {
        let cp = self.store.mem.checkpoint();
        let d = match self.try_deliver(event, payload) {
            Ok(d) => d,
            // The next tick finds the corruption and fails the instance.
            Err(_) => Delivery::Dropped(DropReason::InstanceFailed),
        };
        if matches!(d, Delivery::Dropped(_)) {
            // Taken a moment ago: its history is still there.
            let _ = self.store.mem.rollback_to(cp);
        }
        d
    }

    fn try_deliver(&mut self, event: &str, payload: &WireValue) -> Exec<Delivery> {
        let p = Arc::clone(&self.program);
        let drop = |r| Ok(Delivery::Dropped(r));
        if !self.events.iter().any(|e| e == event) && !p.events.iter().any(|e| e.name == event) {
            return drop(DropReason::UnknownEvent);
        }
        let Some(first) = self.first_handler(event) else {
            return drop(DropReason::NoHandler);
        };
        if !wire::fits(&p.types, p.handlers[first].event_ty, payload) {
            return drop(DropReason::WrongShape);
        }
        if self.failed() {
            return drop(DropReason::InstanceFailed);
        }
        if self.queue_full()? {
            return drop(DropReason::QueueFull);
        }
        let mut bytes = Vec::new();
        payload.encode(&mut bytes);
        let Some(ptr) = self.inbox_store(&bytes)? else {
            return drop(DropReason::InboxFull);
        };
        if !self.queue_push(first, ptr)? {
            return corrupt("the queue filled while delivering");
        }
        Ok(if self.task_in_progress()? {
            Delivery::Queued
        } else {
            Delivery::Accepted
        })
    }

    /// The first handler of `event` this instance registers (its `owner` is this instance).
    pub(crate) fn first_handler(&self, event: &str) -> Option<usize> {
        self.program
            .handlers
            .iter()
            .position(|h| h.event == event && h.owner == self.instance)
    }

    fn queue_full(&self) -> Exec<bool> {
        let (head, count) = (self.header(H_QUEUE_HEAD)?, self.header(H_QUEUE_COUNT)?);
        let qlen = QUEUE_LEN as u64;
        if head >= qlen || count > qlen {
            return corrupt("queue head or count out of range");
        }
        Ok(count == qlen)
    }

    /// Queues an event whose first handler is `first` and whose payload is the inbox block `ptr` (the
    /// record owns it from now on), with the next arrival number. `false` when the queue is full (nothing
    /// changed).
    pub(crate) fn queue_push(&mut self, first: usize, ptr: u64) -> Exec<bool> {
        if self.queue_full()? {
            return Ok(false);
        }
        let (head, count) = (self.header(H_QUEUE_HEAD)?, self.header(H_QUEUE_COUNT)?);
        let arrival = self.header(H_ARRIVAL)?;
        let at = self.queue_rec((head + count) % QUEUE_LEN as u64)?;
        self.store.wr(at, arrival)?;
        self.store.wr(word_off(at, 1)?, first as u64)?;
        self.store.wr(word_off(at, 2)?, ptr)?;
        self.set_header(H_ARRIVAL, arrival.wrapping_add(1))?;
        self.set_header(H_QUEUE_COUNT, count + 1)?;
        Ok(true)
    }

    pub(crate) fn queue_rec(&self, i: u64) -> Exec<u32> {
        word_off(self.store.layout.queue_base, i * u64::from(QUEUE_REC_WORDS))
    }

    /// The payload in a new `Raw` block of the inbox heap; `None` when it does not fit.
    pub(crate) fn inbox_store(&mut self, bytes: &[u8]) -> Exec<Option<u64>> {
        let Ok(len) = u32::try_from(bytes.len()) else {
            return Ok(None);
        };
        let p = match self
            .inbox
            .alloc(&mut self.store, Kind::Raw, len.div_ceil(8))?
        {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let mut h = self.inbox.header(&self.store, p)?;
        h.len = len;
        self.inbox.set_header(&mut self.store, p, h)?;
        for (i, chunk) in (0u32..).zip(bytes.chunks(8)) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            write_slot(&mut self.store, self.inbox, p, &h, i, u64::from_le_bytes(w))?;
        }
        Ok(Some(u64::from(p)))
    }

    pub(crate) fn inbox_read(&self, v: u64) -> Exec<Vec<u8>> {
        let (p, h) = hdr(&self.store, self.inbox, v)?;
        if h.kind != Kind::Raw || u64::from(h.len) > u64::from(capacity(&h)) * WORD {
            return corrupt("an inbox block is not wire bytes");
        }
        let mut out = Vec::new();
        for i in 0..h.len.div_ceil(8) {
            out.extend_from_slice(&read_slot(&self.store, p, &h, i)?.to_le_bytes());
        }
        out.truncate(h.len as usize);
        Ok(out)
    }

    // ---- tick ----------------------------------------------------------------------------------------

    /// [`Vm::tick`].
    pub(crate) fn sched_tick(&mut self, now: u64, fuel: u64, host: &mut dyn Host) -> TickReport {
        let mut m = Meter::new(0);
        if fuel > 0
            && let Err(f) = self.tick_run(now, fuel, host, &mut m)
        {
            let cause = AbortCause::Fault {
                kind: f.kind,
                message: f.message,
                stack: Vec::new(),
            };
            // Only memory that does not parse fails the instance. Anything else that escapes the
            // scheduler aborts the task in progress (no task: it is only reported).
            if f.kind == FaultKind::Corrupt {
                self.fail(cause, host);
            } else if self.task_in_progress() == Ok(true) {
                if let Err(f) = self.abort_task(cause, host) {
                    self.fail(
                        AbortCause::Fault {
                            kind: f.kind,
                            message: f.message,
                            stack: Vec::new(),
                        },
                        host,
                    );
                }
            } else {
                host.report(Report::HandlerAborted {
                    task: "?".into(),
                    cause,
                });
            }
        }
        TickReport {
            fuel_used: m.used(),
            status: self.sched_status(),
        }
    }

    /// `Err` for memory that does not parse (the caller fails the instance), or a failure the scheduler
    /// did not route itself (the caller aborts the task in progress).
    fn tick_run(&mut self, now: u64, fuel: u64, host: &mut dyn Host, m: &mut Meter) -> Exec<()> {
        if self.failed() {
            return Ok(());
        }
        let last = self.header(H_NOW)?;
        let now = now.max(last);
        self.set_header(H_NOW, now)?;
        self.expire_promises(now)?;
        self.fire_timers(now)?;
        let Some(fresh) = self.meter_after_debt(fuel)? else {
            return Ok(());
        };
        *m = fresh;
        while m.left() > 0 && !self.failed() {
            if !self.task_in_progress()? {
                if self.reap_one(m)? {
                    continue;
                }
                if !self.start_next(m, host)? {
                    break;
                }
                continue;
            }
            match self.run_task(m, host)? {
                RunEnd::OutOfFuel => break,
                RunEnd::Returned => self.end_task(m, host)?,
                RunEnd::Parked => self.commit_segment(host)?,
                RunEnd::Impure => return corrupt("an impure stop outside a pure evaluation"),
                RunEnd::Faulted {
                    kind,
                    message,
                    stack,
                } => self.abort_task(
                    AbortCause::Fault {
                        kind,
                        message,
                        stack,
                    },
                    host,
                )?,
            }
        }
        self.set_header(H_FUEL_DEBT, m.debt())
    }

    /// Starts the next task: a pending initialiser, then `main`, then by lowest arrival number the next
    /// handler of the event being dispatched (or of the oldest queued one) or a ready continuation.
    /// `false` when nothing is runnable.
    fn start_next(&mut self, m: &mut Meter, host: &mut dyn Host) -> Exec<bool> {
        let p = Arc::clone(&self.program);
        let cursor = self.header(H_INIT_CURSOR)?;
        if let Some(g) = usize::try_from(cursor).ok().and_then(|i| p.globals.get(i)) {
            let arg = u16::try_from(cursor).map_err(|_| Fault::corrupt("init cursor"))?;
            self.begin_task(TASK_INIT, arg, g.init_fn)?;
            return Ok(true);
        }
        if cursor != p.globals.len() as u64 {
            return corrupt("init cursor out of range");
        }
        if self.header(H_TRANSITIONS)? != 0 {
            let Some(q) = &p.quest else {
                return corrupt("quest transitions in a program without a quest");
            };
            self.set_header(H_TRANSITIONS, 0)?;
            // The task's argument is the stage it starts in (`end_task` re-arms when it changed).
            let stage = self.stage_word(q.stage_global)?;
            self.begin_task(TASK_TRANSITIONS, stage, q.transitions_fn)?;
            return Ok(true);
        }
        if self.header(H_QUEST_START)? == QSTART_WANTED {
            let Some(q) = &p.quest else {
                return corrupt("a quest start in a program without a quest");
            };
            self.set_header(H_QUEST_START, QSTART_DONE)?;
            self.begin_task(TASK_START, 0, q.start_fn)?;
            return Ok(true);
        }
        if self.header(H_MAIN)? == MAIN_WANTED {
            let Some(f) = p.main_fn else {
                return corrupt("main wanted in a program without one");
            };
            self.set_header(H_MAIN, MAIN_STARTED)?;
            self.begin_task(TASK_MAIN, 0, f)?;
            return Ok(true);
        }
        let dispatching = self.header(H_EV_HANDLER)? != 0;
        let event = if dispatching {
            Some(self.header(H_EV_ARRIVAL)?)
        } else {
            self.queue_head_arrival()?
        };
        match (event, self.next_ready()?) {
            (Some(e), Some((_, r))) if e < r => {}
            (_, Some((k, _))) => {
                self.start_resume(k, m, host)?;
                return Ok(true);
            }
            (None, None) => return Ok(false),
            (Some(_), None) => {}
        }
        if !dispatching && !self.dequeue()? {
            return corrupt("the queue lost its head");
        }
        self.start_handler(m, host)?;
        Ok(true)
    }

    /// Whether a migration carried global `i` over (its initialiser does not run).
    pub(crate) fn init_skipped(&self, i: u64) -> Exec<bool> {
        let set = self.header(H_INIT_SKIP)?;
        if set == 0 {
            return Ok(false);
        }
        let (p, h) = hdr(&self.store, self.inbox, set)?;
        let n = self.program.globals.len() as u64;
        if h.kind != Kind::Raw || u64::from(h.len) < n.div_ceil(64) * WORD || i >= n {
            return corrupt("the skipped initialisers do not parse");
        }
        // i < n <= 65 536 globals
        let w = read_slot(&self.store, p, &h, (i / 64) as u32)?;
        Ok(w >> (i % 64) & 1 != 0)
    }

    /// Moves the init cursor past the globals a migration carried over, and frees the set of them once
    /// the cursor is past the last global. Reports the globals stepped over.
    pub(crate) fn skip_carried_inits(&mut self) -> Exec<u32> {
        let set = self.header(H_INIT_SKIP)?;
        if set == 0 {
            return Ok(0);
        }
        let n = self.program.globals.len() as u64;
        let mut cursor = self.header(H_INIT_CURSOR)?;
        let mut stepped = 0u32;
        while cursor < n && self.init_skipped(cursor)? {
            cursor += 1;
            stepped += 1;
        }
        self.set_header(H_INIT_CURSOR, cursor)?;
        if cursor >= n {
            self.inbox.free(&mut self.store, ptr_of(set)?)?;
            self.set_header(H_INIT_SKIP, 0)?;
        }
        Ok(stepped)
    }

    /// Makes `skip` (one bit per global, `init_skipped`) the set of initialisers that do not run, in place
    /// of any earlier set, and moves the init cursor past the leading ones. `false` when the inbox has no
    /// room for it (nothing changed then).
    pub(crate) fn set_init_skip(&mut self, skip: &[u64]) -> Exec<bool> {
        let set = if skip.iter().any(|&w| w != 0) {
            let bytes: Vec<u8> = skip.iter().flat_map(|w| w.to_le_bytes()).collect();
            match self.inbox_store(&bytes)? {
                Some(set) => set,
                None => return Ok(false),
            }
        } else {
            0
        };
        let old = self.header(H_INIT_SKIP)?;
        if old != 0 {
            self.inbox.free(&mut self.store, ptr_of(old)?)?;
        }
        self.set_header(H_INIT_SKIP, set)?;
        self.skip_carried_inits()?;
        Ok(true)
    }

    /// The arrival number of the oldest queued event.
    fn queue_head_arrival(&self) -> Exec<Option<u64>> {
        let (head, count) = (self.header(H_QUEUE_HEAD)?, self.header(H_QUEUE_COUNT)?);
        if head >= QUEUE_LEN as u64 || count > QUEUE_LEN as u64 {
            return corrupt("queue head or count out of range");
        }
        if count == 0 {
            return Ok(None);
        }
        Ok(Some(self.store.r(self.queue_rec(head)?)?))
    }

    /// Takes the oldest queued event off the queue and makes it the event being dispatched.
    fn dequeue(&mut self) -> Exec<bool> {
        let (head, count) = (self.header(H_QUEUE_HEAD)?, self.header(H_QUEUE_COUNT)?);
        let qlen = QUEUE_LEN as u64;
        if head >= qlen || count > qlen {
            return corrupt("queue head or count out of range");
        }
        if count == 0 {
            return Ok(false);
        }
        let at = self.queue_rec(head)?;
        let mut rec = [0u64; QUEUE_REC_WORDS as usize];
        for (i, w) in (0u64..).zip(rec.iter_mut()) {
            let off = word_off(at, i)?;
            *w = self.store.r(off)?;
            self.store.wr(off, 0)?;
        }
        let [arrival, first, payload, _] = rec;
        self.set_header(H_QUEUE_HEAD, (head + 1) % qlen)?;
        self.set_header(H_QUEUE_COUNT, count - 1)?;
        self.set_header(H_EV_HANDLER, first.saturating_add(1))?;
        self.set_header(H_EV_PAYLOAD, payload)?;
        self.set_header(H_EV_ARRIVAL, arrival)?;
        Ok(true)
    }

    /// Starts the next handler of the event being dispatched: advances the cursor (freeing the payload
    /// when this is the event's last handler), opens the task and loads the event into the handler's
    /// parameter inside its segment.
    fn start_handler(&mut self, m: &mut Meter, host: &mut dyn Host) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let idx = self.header(H_EV_HANDLER)?.wrapping_sub(1);
        let Some(h) = usize::try_from(idx).ok().and_then(|i| p.handlers.get(i)) else {
            return corrupt("the event's handler index is out of range");
        };
        // `idx` indexes a table of at most 65 536 handlers
        let (idx, arg) = (idx as usize, idx as u16);
        let ptr = self.header(H_EV_PAYLOAD)?;
        let bytes = self.inbox_read(ptr)?;
        let next = p
            .handlers
            .iter()
            .enumerate()
            .skip(idx + 1)
            .find(|(_, o)| o.event == h.event && o.owner == self.instance);
        match next {
            Some((j, _)) => self.set_header(H_EV_HANDLER, j as u64 + 1)?,
            None => {
                self.inbox.free(&mut self.store, ptr_of(ptr)?)?;
                self.set_header(H_EV_HANDLER, 0)?;
                self.set_header(H_EV_PAYLOAD, 0)?;
                self.set_header(H_EV_ARRIVAL, 0)?;
            }
        }
        m.charge(FUEL_DISPATCH);
        m.words(u32::try_from(bytes.len().div_ceil(8)).unwrap_or(u32::MAX));
        self.begin_task(TASK_HANDLER, arg, h.fn_id)?;
        let w = WireValue::decode(&bytes)
            .map_err(|_| Fault::corrupt("a queued payload does not decode"))?;
        let slot = self.frame_slot(h.fn_id, FRAME_PARAM0)?;
        let loaded = match from_wire(&mut self.store, self.heap, &p.types, h.event_ty, &w) {
            Ok((v, words)) => {
                m.words(words);
                self.put(slot, v, p.types.is_ref(h.event_ty))
            }
            Err(FromWire::WrongType) => Err(Fault {
                kind: FaultKind::HostFault,
                message: "the event does not fit the handler's event type".into(),
            }),
            Err(FromWire::Fault(k)) => Err(k.into()),
        };
        match loaded {
            Ok(()) => Ok(()),
            Err(f) if f.kind == FaultKind::Corrupt => Err(f),
            Err(f) => {
                let entry = p.fns.get(usize::from(h.fn_id)).map_or(0, |f| f.entry);
                self.abort_task(
                    AbortCause::Fault {
                        kind: f.kind,
                        message: f.message,
                        stack: vec![p.loc(entry)],
                    },
                    host,
                )
            }
        }
    }

    /// Opens the task's segment and starts its function with a sentinel return link.
    fn begin_task(&mut self, kind: u64, arg: u16, fn_id: u16) -> Exec<()> {
        let Some(entry) = self.program.fns.get(usize::from(fn_id)).map(|f| f.entry) else {
            return corrupt("a task of an unknown function");
        };
        if self.store.segment_is_open() {
            return corrupt("a segment is open between tasks");
        }
        self.store.segment_open();
        let link = self.frame_slot(fn_id, 0)?;
        self.wt(link, LINK_SENTINEL)?;
        for (field, v) in [
            (H_PC, u64::from(entry)),
            (H_CURRENT_FN, u64::from(fn_id)),
            (H_STATUS, STATUS_RUNNING),
            (H_TASK, kind),
            (H_TASK_ARG, u64::from(arg)),
            (H_TRACE_LEN, 0),
            (H_TRACE_DROPPED, 0),
        ] {
            self.set_header(field, v)?;
        }
        Ok(())
    }

    /// The stage global's word (a field-less variant: an immediate) as a transitions task's argument;
    /// [`STAGE_UNKNOWN`] when it does not fit one (then the run never re-arms).
    fn stage_word(&self, global: u16) -> Exec<u16> {
        let Some(g) = self.program.globals.get(usize::from(global)) else {
            return corrupt("the stage global is out of range");
        };
        let w = self.rd(g.slot)?;
        Ok(u16::try_from(w)
            .ok()
            .filter(|&w| w != STAGE_UNKNOWN)
            .unwrap_or(STAGE_UNKNOWN))
    }

    fn current_task(&self) -> Exec<Task> {
        let p = &self.program;
        let kind = self.header(H_TASK)?;
        let arg = u16::try_from(self.header(H_TASK_ARG)?)
            .map_err(|_| Fault::corrupt("task identity out of range"))?;
        let i = usize::from(arg);
        let fn_id = match kind {
            TASK_INIT => p.globals.get(i).map(|g| g.init_fn),
            TASK_MAIN => p.main_fn,
            TASK_HANDLER => p.handlers.get(i).map(|h| h.fn_id),
            TASK_START => p.quest.as_ref().map(|q| q.start_fn),
            TASK_TRANSITIONS => p.quest.as_ref().map(|q| q.transitions_fn),
            _ => None,
        };
        match fn_id {
            Some(fn_id) => Ok(Task { kind, arg, fn_id }),
            None => corrupt("unknown task"),
        }
    }

    /// The task's function returned: an escaping error aborts; otherwise its result is stored, the
    /// buffered effects reach the host and the segment commits.
    fn end_task(&mut self, m: &mut Meter, host: &mut dyn Host) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let t = self.current_task()?;
        let err = self.rd(self.frame_slot(t.fn_id, FRAME_ERR)?)?;
        if err != 0 {
            let cause = self.error_cause(err, m)?;
            return self.abort_task(cause, host);
        }
        let i = usize::from(t.arg);
        match t.kind {
            TASK_INIT => {
                let Some(g) = p.globals.get(i) else {
                    return corrupt("unknown global");
                };
                let is_ref = p.types.is_ref(g.ty);
                let ret = self.frame_slot(t.fn_id, FRAME_RET)?;
                let v = self.rd(ret)?;
                if is_ref {
                    self.wt(ret, 0)?;
                }
                self.put(g.slot, v, is_ref)?;
                self.set_header(H_INIT_CURSOR, u64::from(t.arg) + 1)?;
                let words = self.skip_carried_inits()?;
                m.words(words);
            }
            TASK_HANDLER => {
                let Some(h) = p.handlers.get(i) else {
                    return corrupt("unknown handler");
                };
                if p.types.is_ref(h.event_ty) {
                    let ev = self.frame_slot(t.fn_id, FRAME_PARAM0)?;
                    self.put(ev, 0, true)?;
                }
            }
            // A run that entered a stage re-arms: the new stage's own transition may already hold (a
            // chain). One that entered none does not, so a run never repeats itself; fuel bounds a cycle.
            TASK_TRANSITIONS => {
                if let Some(q) = &p.quest
                    && t.arg != STAGE_UNKNOWN
                    && self.stage_word(q.stage_global)? != t.arg
                {
                    self.set_header(H_TRANSITIONS, 1)?;
                }
            }
            _ => {}
        }
        self.commit_segment(host)
    }

    /// Ends the task's segment (at its end or at an `await`): the buffered records are read out, the
    /// requests marked sent, the segment committed and the task cleared; then the host gets the records in
    /// order. The memory is final before the host sees anything.
    fn commit_segment(&mut self, host: &mut dyn Host) -> Exec<()> {
        let buffered = self.read_cmds()?;
        let now = self.header(H_NOW)?;
        self.commit_promises(now)?;
        self.commit_timers()?;
        // The home's handlers, `main` and the start: the quest's transitions are due (spec §5.12).
        let kind = self.header(H_TASK)?;
        if self.instance == 0
            && self.program.quest.is_some()
            && matches!(kind, TASK_HANDLER | TASK_MAIN | TASK_START)
        {
            self.set_header(H_TRANSITIONS, 1)?;
        }
        self.store.segment_commit();
        self.clear_task()?;
        for b in buffered {
            match b {
                Buffered::Command(f, args) => host.command(f, &args),
                Buffered::Print(text) => host.print(&text),
                Buffered::Request(id, f, args) => host.request(PromiseId(id), f, &args),
                Buffered::Enqueue(e, value) => self.deliver_own(e, &value, host)?,
            }
        }
        Ok(())
    }

    /// A committed `Enqueue`: the event goes to this instance's queue as a host's delivery would (module
    /// docs).
    fn deliver_own(&mut self, e: u16, value: &WireValue, host: &mut dyn Host) -> Exec<()> {
        let Some(ev) = self.program.events.get(usize::from(e)) else {
            return corrupt("an Enqueue of an unknown script event");
        };
        let event = ev.name.clone();
        match self.sched_deliver(&event, value) {
            Delivery::Accepted | Delivery::Queued => {}
            // Another instance's: the host routes it (an event no instance handles goes nowhere).
            Delivery::Dropped(DropReason::NoHandler) => {
                if self
                    .program
                    .handlers
                    .iter()
                    .any(|h| h.event == event && h.owner != self.instance)
                {
                    host.route(&event, value);
                }
            }
            // Not failed while a segment commits: the memory did not parse.
            Delivery::Dropped(DropReason::InstanceFailed) => {
                return corrupt("the queue does not parse");
            }
            Delivery::Dropped(reason) => host.report(Report::EventDropped { event, reason }),
        }
        Ok(())
    }

    /// `Enqueue a, b`: the value in slot `b` (borrowed), of script event `a`, joins the segment's buffered
    /// effects; the commit delivers it.
    pub(crate) fn exec_enqueue(&mut self, e: u16, slot: u16, m: &mut Meter) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let Some(ev) = p.events.get(usize::from(e)) else {
            return corrupt("an Enqueue of an unknown script event");
        };
        let (w, words) = to_wire(&self.store, self.heap, &p.types, ev.ty, self.rd(slot)?)?;
        m.words(words);
        let mut bytes = Vec::new();
        w.encode(&mut bytes);
        self.buffer(CMD_ENQUEUE, e, &bytes, m)
    }

    /// The task failed with `cause`: the segment is undone, the buffer cleared and the failure reported;
    /// an initialiser's failure fails the instance. A `Corrupt` fault fails it without an abort.
    pub(crate) fn abort_task(&mut self, cause: AbortCause, host: &mut dyn Host) -> Exec<()> {
        if matches!(
            cause,
            AbortCause::Fault {
                kind: FaultKind::Corrupt,
                ..
            }
        ) {
            self.fail(cause, host);
            return Ok(());
        }
        let t = self.current_task()?;
        let report = self.report_for(t, cause);
        self.store.segment_abort();
        self.abort_promises()?;
        self.abort_timers()?;
        if t.kind == TASK_INIT {
            self.set_header(H_INSTANCE, INSTANCE_FAILED)?;
        }
        self.clear_task()?;
        host.report(report);
        Ok(())
    }

    /// Between tasks: no task, the interpreter idle, the buffer and the trace empty.
    fn clear_task(&mut self) -> Exec<()> {
        for field in [
            H_TASK,
            H_TASK_ARG,
            H_CMD_LEN,
            H_TRACE_LEN,
            H_TRACE_DROPPED,
            H_RESUME_PARKED,
        ] {
            self.set_header(field, 0)?;
        }
        self.set_header(H_STATUS, STATUS_IDLE)
    }

    /// Fails the instance (inert from now on) and reports `cause` against the task in progress.
    fn fail(&mut self, cause: AbortCause, host: &mut dyn Host) {
        let _ = self.set_header(H_INSTANCE, INSTANCE_FAILED);
        let report = match self.current_task() {
            Ok(t) => self.report_for(t, cause),
            Err(_) => Report::HandlerAborted {
                task: "?".into(),
                cause,
            },
        };
        host.report(report);
    }

    fn report_for(&self, t: Task, cause: AbortCause) -> Report {
        let p = &self.program;
        if t.kind == TASK_INIT {
            let global = p
                .globals
                .get(usize::from(t.arg))
                .map_or_else(|| "?".into(), |g| g.name.clone());
            return Report::InitFailed { global, cause };
        }
        let task = p
            .fns
            .get(usize::from(t.fn_id))
            .map_or_else(|| "?".into(), |f| f.name.clone());
        Report::HandlerAborted { task, cause }
    }

    /// The escaping error `err` (still in the frame, before the abort) with its trace. Data that cannot
    /// be converted for the report (too large to walk) makes the cause a fault of that kind instead, its
    /// stack the error's trace; only `Corrupt` is an `Err`.
    fn error_cause(&self, err: u64, m: &mut Meter) -> Exec<AbortCause> {
        let (trace, trace_dropped) = self.error_trace()?;
        let Some(t) = self.error_ty else {
            return Ok(AbortCause::Error {
                name: "?".into(),
                data: WireValue::Unit,
                trace,
                trace_dropped,
            });
        };
        let tag = obj::tag(&self.store, self.heap, err)?;
        let name = match self.program.types.desc(t)? {
            TypeDesc::Enum { variants, .. } => variants.get(usize::from(tag)).map(|v| v.0.clone()),
            _ => None,
        };
        let Some(name) = name else {
            return corrupt("an error whose tag is not a variant");
        };
        match to_wire(&self.store, self.heap, &self.program.types, t, err) {
            Ok((data, words)) => {
                m.words(words);
                Ok(AbortCause::Error {
                    name,
                    data,
                    trace,
                    trace_dropped,
                })
            }
            Err(FaultKind::Corrupt) => corrupt("an error value does not parse"),
            Err(kind) => Ok(AbortCause::Fault {
                kind,
                message: format!("error {name} carries data too large to report"),
                stack: trace,
            }),
        }
    }

    /// The error return trace: its locations and how many entries were dropped.
    fn error_trace(&self) -> Exec<(Vec<SrcLoc>, u32)> {
        let len = self.header(H_TRACE_LEN)?;
        if len > ERR_TRACE_MAX as u64 {
            return corrupt("error trace length out of range");
        }
        let mut trace: Vec<SrcLoc> = Vec::new();
        for i in 0..len {
            let pc = self.store.r(word_off(self.store.layout.trace_base, i)?)?;
            let pc = u32::try_from(pc).map_err(|_| Fault::corrupt("trace entry out of range"))?;
            trace.push(self.program.loc(pc));
        }
        let trace_dropped = u32::try_from(self.header(H_TRACE_DROPPED)?).unwrap_or(u32::MAX);
        Ok((trace, trace_dropped))
    }

    // ---- host calls and the command buffer -----------------------------------------------------------

    /// `CallHost a, b, c`: the arguments (types from site `a`) from the slots at `b`. A query is asked
    /// at once and its answer loaded into `c`; a command is buffered (it does not write `c`); a promise
    /// request is buffered and its id written to `c` (`promise`).
    ///
    /// A fallible site (one that declares errors) also writes the error word `c + 1`: 0 when the call
    /// succeeded, else the error, `c` left as it was. A fallible command is checked by the host first
    /// (`Host::check`) and buffered only when the check passes. An error the site does not declare, and a
    /// host's `Failed`, are the fault `HostFault`.
    pub(crate) fn call_host(&mut self, ins: Instr, m: &mut Meter, host: &mut dyn Host) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let Some(site) = p.host_sites.get(usize::from(ins.a)) else {
            return corrupt("unknown host site");
        };
        let mut args = Vec::with_capacity(site.params.len());
        for (i, &ty) in (0u16..).zip(&site.params) {
            let slot = ins
                .b
                .checked_add(i)
                .ok_or_else(|| Fault::corrupt("argument slot out of range"))?;
            let (w, words) = to_wire(&self.store, self.heap, &p.types, ty, self.rd(slot)?)?;
            m.words(words);
            args.push(w);
        }
        match site.kind {
            HostFnKind::Command => {
                if site.fallible()
                    && let Err(e) = host.check(site.host_fn, &args)
                {
                    return self.host_error(site, e, ins.c, m);
                }
                let mut bytes = Vec::new();
                WireValue::List(args).encode(&mut bytes);
                self.buffer(CMD_COMMAND, site.host_fn, &bytes, m)?;
                return self.host_succeeded(site, ins.c);
            }
            HostFnKind::Promise => return self.issue_request(site.host_fn, args, ins.c, m),
            HostFnKind::Query => {}
        }
        let answer = match host.query(site.host_fn, &args) {
            Ok(w) => w,
            Err(e) => return self.host_error(site, e, ins.c, m),
        };
        match from_wire(&mut self.store, self.heap, &p.types, site.ret, &answer) {
            Ok((v, words)) => {
                m.words(words);
                self.put(ins.c, v, p.types.is_ref(site.ret))?;
                self.host_succeeded(site, ins.c)
            }
            Err(FromWire::WrongType) => Err(Fault {
                kind: FaultKind::HostFault,
                message: "wrong result type".into(),
            }),
            Err(FromWire::Fault(k)) => Err(k.into()),
        }
    }

    /// The error word of a fallible site's call with result word `c`.
    fn host_err_slot(c: u16) -> Exec<u16> {
        c.checked_add(1)
            .ok_or_else(|| Fault::corrupt("CallHost's error slot out of range"))
    }

    /// A fallible site's call succeeded: its error word is 0. Nothing for an infallible site.
    fn host_succeeded(&mut self, site: &HostSite, c: u16) -> Exec<()> {
        if !site.fallible() {
            return Ok(());
        }
        self.put(Self::host_err_slot(c)?, 0, true)
    }

    /// The host refused a query or a fallible command's check with `e`: a declared error goes to the
    /// error word (`c` is left as it was); anything else is `HostFault`.
    fn host_error(&mut self, site: &HostSite, e: QueryError, c: u16, m: &mut Meter) -> Exec<()> {
        let fault = |message: String| Fault {
            kind: FaultKind::HostFault,
            message,
        };
        let w = match e {
            QueryError::Failed(message) => return Err(fault(message)),
            QueryError::Raised(w) => w,
        };
        match &w {
            WireValue::Variant { name, .. } if site.raises.contains(name) => {}
            WireValue::Variant { name, .. } => {
                return Err(fault(format!(
                    "host fn {} raised {name}, which it does not declare",
                    site.host_fn
                )));
            }
            _ => {
                return Err(fault(format!(
                    "host fn {} raised a value that is not an error",
                    site.host_fn
                )));
            }
        }
        let v = self.error_value(&w, m)?;
        self.put(Self::host_err_slot(c)?, v, true)
    }

    /// `Print`: the text joins the segment's buffered effects.
    pub(crate) fn buffer_print(&mut self, text: &str, m: &mut Meter) -> Exec<()> {
        self.buffer(CMD_PRINT, 0, text.as_bytes(), m)
    }

    /// Appends a record; a full buffer is the fault `TooManyCommands`.
    pub(crate) fn buffer(&mut self, kind: u64, f: u16, bytes: &[u8], m: &mut Meter) -> Exec<()> {
        let l = self.store.layout;
        let cap = u64::from(l.cmds_end - l.cmds_base);
        let len = self.header(H_CMD_LEN)?;
        if len > cap || !len.is_multiple_of(WORD) {
            return corrupt("command buffer length out of range");
        }
        let too_many = || Fault::from(FaultKind::TooManyCommands);
        let blen = u32::try_from(bytes.len()).map_err(|_| too_many())?;
        let words = u64::from(blen).div_ceil(WORD);
        let end = len + (1 + words) * WORD;
        if end > cap {
            return Err(too_many());
        }
        let at = word_off(l.cmds_base, len / WORD)?;
        self.store
            .wr(at, kind | u64::from(f) << 16 | u64::from(blen) << 32)?;
        for (i, chunk) in (1u64..).zip(bytes.chunks(8)) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            self.store.wr(word_off(at, i)?, u64::from_le_bytes(w))?;
        }
        m.words(u32::try_from(words).unwrap_or(u32::MAX));
        self.set_header(H_CMD_LEN, end)
    }

    /// The buffered records in order; anything that does not parse is `Corrupt`.
    fn read_cmds(&self) -> Exec<Vec<Buffered>> {
        let l = self.store.layout;
        let cap = u64::from(l.cmds_end - l.cmds_base);
        let len = self.header(H_CMD_LEN)?;
        if len > cap || !len.is_multiple_of(WORD) {
            return corrupt("command buffer length out of range");
        }
        let mut out = Vec::new();
        let mut at = 0u64; // words
        while at * WORD < len {
            let h = self.store.r(word_off(l.cmds_base, at)?)?;
            let (kind, reserved, f, blen) = (h & 0xff, (h >> 8) & 0xff, (h >> 16) as u16, h >> 32);
            let words = blen.div_ceil(WORD);
            if reserved != 0 || (at + 1 + words) * WORD > len {
                return corrupt("a command record does not parse");
            }
            let mut bytes = Vec::new();
            for i in 0..words {
                let w = self.store.r(word_off(l.cmds_base, at + 1 + i)?)?;
                bytes.extend_from_slice(&w.to_le_bytes());
            }
            // blen <= len <= cap, which fits a u32
            bytes.truncate(blen as usize);
            out.push(match kind {
                CMD_COMMAND => match WireValue::decode(&bytes)
                    .ok()
                    .and_then(WireValue::into_list)
                {
                    Some(args) => Buffered::Command(f, args),
                    None => return corrupt("a command's arguments do not decode"),
                },
                CMD_PRINT => match String::from_utf8(bytes) {
                    Ok(t) => Buffered::Print(t),
                    Err(_) => return corrupt("a print is not UTF-8"),
                },
                CMD_REQUEST => {
                    let Some((id, rest)) = bytes.split_first_chunk::<8>() else {
                        return corrupt("a request record has no id");
                    };
                    match WireValue::decode(rest).ok().and_then(WireValue::into_list) {
                        Some(args) => Buffered::Request(u64::from_le_bytes(*id), f, args),
                        None => return corrupt("a request's arguments do not decode"),
                    }
                }
                CMD_ENQUEUE => match WireValue::decode(&bytes) {
                    Ok(v) => Buffered::Enqueue(f, v),
                    Err(_) => return corrupt("an enqueued event does not decode"),
                },
                _ => return corrupt("unknown command record"),
            });
            at += 1 + words;
        }
        Ok(out)
    }
}
