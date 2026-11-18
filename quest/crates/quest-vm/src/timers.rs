//! Timers (spec §5.12): `After`, `CancelTimer`, `CancelStageTimers`, and their firing at `tick`.
//!
//! Everything lives in the flat memory and goes through the store's logged write paths, so checkpoints,
//! rollback, saves and the state hash cover the timers. The table is a runtime region: a segment abort does
//! not restore it, [`Vm::abort_timers`] does what an abort must (as `promise` does for its records).
//!
//! # Records (the timers region: `TIMERS_MAX` records of `TIMER_REC_WORDS` words)
//! Word 0 `state: u8 | flags: u8 << 8 | event: u16 << 16` (the event an index into `Program::events`),
//! word 1 the id, word 2 the due tick, word 3 the event value (a `Raw` block of its wire bytes in the inbox
//! heap). A free record is all zero. Ids are `epoch << 32 | serial`; `H_TIMER_SERIAL` is the last serial
//! issued and is never given back, so an id never repeats within an epoch (and is never 0).
//! * `F_PENDING`: set by `After` in the open segment. The commit clears it; an abort frees the record.
//! * `F_CANCELLING`: cancelled in the open segment. The commit frees the record; an abort clears the flag.
//!   A pending timer cancelled in its own segment is freed at once.
//! * `F_SCOPED`: set by a task of a handler declared in a stage (`HandlerInfo::stage`):
//!   `CancelStageTimers` cancels every scoped timer.
//!
//! # `After a, b, c`
//! The ticks are the int in slot `b` (a negative count is 0), the event value is read from slot `b + 1`
//! (borrowed) against script event `c`'s type; the timer is due at `now + ticks` (`now` the latest tick's,
//! saturating) and its id goes to slot `a`. With `TIMERS_MAX` records in use (pending and cancelling ones
//! count) it faults `TooManyTimers`, and `HeapFull` when the inbox has no room for the value.
//!
//! # Firing
//! At the start of every tick, after the promise timeouts and before any task runs, each committed timer
//! (neither flag set) due at `now` or earlier is delivered to the instance's own queue, in (due, id) order,
//! as an `Enqueue` would deliver it (its value moves into the queue record). A timer set at tick T with `n`
//! ticks therefore runs its handlers at tick T + n (T + 1 for 0). When the queue is full the timer waits for
//! a later tick, and so does every timer after it (the order is kept). A timer whose event no handler of
//! this instance takes goes without a delivery, as an `Enqueue` of it would.

use std::sync::Arc;

use crate::consts::TIMERS_MAX;
use crate::fault::FaultKind;
use crate::interp::{Exec, Fault, Meter};
use crate::layout::{H_EPOCH, H_NOW, H_TASK, H_TASK_ARG, H_TIMER_SERIAL, TIMER_REC_WORDS};
use crate::object::ptr_of;
use crate::sched::{TASK_HANDLER, corrupt, word_off};
use crate::vm::Vm;
use crate::wire::{WireValue, fits, to_wire};

const T_FREE: u8 = 0;
const T_LIVE: u8 = 1;
const F_PENDING: u8 = 1;
const F_CANCELLING: u8 = 2;
const F_SCOPED: u8 = 4;
const F_ALL: u8 = F_PENDING | F_CANCELLING | F_SCOPED;

const REC_WORDS: usize = TIMER_REC_WORDS as usize;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TimerRec {
    state: u8,
    flags: u8,
    event: u16,
    id: u64,
    due: u64,
    payload: u64,
}

impl TimerRec {
    fn encode(&self) -> [u64; REC_WORDS] {
        let w0 = u64::from(self.state) | u64::from(self.flags) << 8 | u64::from(self.event) << 16;
        [w0, self.id, self.due, self.payload]
    }

    /// A record read from memory: no stray bits, a free record all zero.
    fn decode(w: [u64; REC_WORDS]) -> Exec<TimerRec> {
        let r = TimerRec {
            state: w[0] as u8,
            flags: (w[0] >> 8) as u8,
            event: (w[0] >> 16) as u16,
            id: w[1],
            due: w[2],
            payload: w[3],
        };
        let malformed = w[0] >> 32 != 0
            || r.flags & !F_ALL != 0
            || (r.state == T_FREE && r != TimerRec::default())
            || (r.state == T_LIVE && (r.id == 0 || r.payload == 0))
            || r.state > T_LIVE;
        if malformed {
            return corrupt("a timer record does not parse");
        }
        Ok(r)
    }

    /// Live and committed: neither created nor cancelled by the open segment.
    fn settled(&self) -> bool {
        self.state == T_LIVE && self.flags & (F_PENDING | F_CANCELLING) == 0
    }
}

/// A timer as an edit carries it (`migrate`): its event by name and its value as wire data.
pub(crate) struct CarriedTimer {
    pub id: u64,
    pub due: u64,
    pub scoped: bool,
    pub event: String,
    pub value: WireValue,
}

impl Vm {
    fn timer_at(&self, i: usize) -> Exec<u32> {
        word_off(
            self.store.layout.timers_base,
            i as u64 * u64::from(TIMER_REC_WORDS),
        )
    }

    fn read_timer(&self, i: usize) -> Exec<TimerRec> {
        let at = self.timer_at(i)?;
        let mut w = [0u64; REC_WORDS];
        for (k, x) in (0u64..).zip(w.iter_mut()) {
            *x = self.store.r(word_off(at, k)?)?;
        }
        TimerRec::decode(w)
    }

    fn write_timer(&mut self, i: usize, r: &TimerRec) -> Exec<()> {
        let at = self.timer_at(i)?;
        for (k, v) in (0u64..).zip(r.encode()) {
            self.store.wr(word_off(at, k)?, v)?;
        }
        Ok(())
    }

    /// Frees record `i` and its event value.
    fn free_timer(&mut self, i: usize) -> Exec<()> {
        let r = self.read_timer(i)?;
        if r.payload != 0 {
            self.inbox.free(&mut self.store, ptr_of(r.payload)?)?;
        }
        self.write_timer(i, &TimerRec::default())
    }

    /// The first free record.
    fn free_timer_slot(&self) -> Exec<Option<usize>> {
        for i in 0..TIMERS_MAX {
            if self.read_timer(i)?.state == T_FREE {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Whether the task in progress is a handler declared in a stage.
    fn task_is_staged(&self) -> Exec<bool> {
        if self.header(H_TASK)? != TASK_HANDLER {
            return Ok(false);
        }
        let i = self.header(H_TASK_ARG)?;
        Ok(usize::try_from(i)
            .ok()
            .and_then(|i| self.program.handlers.get(i))
            .is_some_and(|h| h.stage.is_some()))
    }

    /// `After a, b, c` (module docs).
    pub(crate) fn exec_after(&mut self, a: u16, b: u16, event: u16, m: &mut Meter) -> Exec<()> {
        let p = Arc::clone(&self.program);
        let Some(ev) = p.events.get(usize::from(event)) else {
            return corrupt("After of an unknown script event");
        };
        let ticks = self.rd(b)? as i64;
        let value_slot = b
            .checked_add(1)
            .ok_or_else(|| Fault::corrupt("After's value slot out of range"))?;
        let i = self
            .free_timer_slot()?
            .ok_or_else(|| Fault::from(FaultKind::TooManyTimers))?;
        let serial = self
            .header(H_TIMER_SERIAL)?
            .checked_add(1)
            .filter(|&s| s <= u64::from(u32::MAX))
            .ok_or_else(|| Fault::from(FaultKind::TooManyTimers))?;
        let epoch = self.header(H_EPOCH)?;
        if epoch > u64::from(u32::MAX) {
            return corrupt("epoch out of range");
        }
        let (w, words) = to_wire(
            &self.store,
            self.heap,
            &p.types,
            ev.ty,
            self.rd(value_slot)?,
        )?;
        m.words(words);
        let mut bytes = Vec::new();
        w.encode(&mut bytes);
        let payload = self.inbox_store(&bytes)?.ok_or_else(|| Fault {
            kind: FaultKind::HeapFull,
            message: "the inbox has no room for the timer's event".into(),
        })?;
        let due = self
            .header(H_NOW)?
            .saturating_add(u64::try_from(ticks).unwrap_or(0));
        let id = epoch << 32 | serial;
        let scoped = if self.task_is_staged()? { F_SCOPED } else { 0 };
        let r = TimerRec {
            state: T_LIVE,
            flags: F_PENDING | scoped,
            event,
            id,
            due,
            payload,
        };
        self.write_timer(i, &r)?;
        self.set_header(H_TIMER_SERIAL, serial)?;
        self.wt(a, id)
    }

    /// Cancels live record `i` in the open segment: a pending one is freed at once.
    fn cancel_timer_rec(&mut self, i: usize) -> Exec<()> {
        let mut r = self.read_timer(i)?;
        if r.flags & F_PENDING != 0 {
            return self.free_timer(i);
        }
        r.flags |= F_CANCELLING;
        self.write_timer(i, &r)
    }

    /// `CancelTimer a`: the live timer whose id is in slot `a`; an id that is not one (fired, cancelled,
    /// never issued) does nothing.
    pub(crate) fn exec_cancel_timer(&mut self, slot: u16) -> Exec<()> {
        let id = self.rd(slot)?;
        for i in 0..TIMERS_MAX {
            let r = self.read_timer(i)?;
            if r.state == T_LIVE && r.id == id && r.flags & F_CANCELLING == 0 {
                return self.cancel_timer_rec(i);
            }
        }
        Ok(())
    }

    /// `CancelStageTimers`: every stage-scoped timer.
    pub(crate) fn exec_cancel_stage_timers(&mut self) -> Exec<()> {
        for i in 0..TIMERS_MAX {
            let r = self.read_timer(i)?;
            if r.state == T_LIVE && r.flags & F_SCOPED != 0 && r.flags & F_CANCELLING == 0 {
                self.cancel_timer_rec(i)?;
            }
        }
        Ok(())
    }

    /// At a segment's commit: its timers are set, its cancels final.
    pub(crate) fn commit_timers(&mut self) -> Exec<()> {
        for i in 0..TIMERS_MAX {
            let mut r = self.read_timer(i)?;
            if r.flags & F_CANCELLING != 0 {
                self.free_timer(i)?;
            } else if r.flags & F_PENDING != 0 {
                r.flags &= !F_PENDING;
                self.write_timer(i, &r)?;
            }
        }
        Ok(())
    }

    /// After a segment's abort: the timers it set are gone, the ones it cancelled live on.
    pub(crate) fn abort_timers(&mut self) -> Exec<()> {
        for i in 0..TIMERS_MAX {
            let mut r = self.read_timer(i)?;
            if r.flags & F_PENDING != 0 {
                self.free_timer(i)?;
            } else if r.flags & F_CANCELLING != 0 {
                r.flags &= !F_CANCELLING;
                self.write_timer(i, &r)?;
            }
        }
        Ok(())
    }

    /// At the start of a tick: the committed timers due at `now` or earlier go to the queue (module docs).
    pub(crate) fn fire_timers(&mut self, now: u64) -> Exec<()> {
        let mut due = Vec::new();
        for i in 0..TIMERS_MAX {
            let r = self.read_timer(i)?;
            if r.settled() && r.due <= now {
                due.push((r.due, r.id, i));
            }
        }
        due.sort_unstable();
        let p = Arc::clone(&self.program);
        for (_, _, i) in due {
            let r = self.read_timer(i)?;
            let Some(ev) = p.events.get(usize::from(r.event)) else {
                return corrupt("a timer of an unknown script event");
            };
            let Some(first) = self.first_handler(&ev.name) else {
                self.free_timer(i)?;
                continue;
            };
            if !self.queue_push(first, r.payload)? {
                break;
            }
            // The queue record owns the value now.
            self.write_timer(i, &TimerRec::default())?;
        }
        Ok(())
    }

    /// The live timers as (due, id, event name), in (due, id) order; empty when the table does not parse.
    pub(crate) fn timers_list(&self) -> Vec<(u64, u64, String)> {
        let mut out = Vec::new();
        for i in 0..TIMERS_MAX {
            let Ok(r) = self.read_timer(i) else {
                return Vec::new();
            };
            if r.state != T_LIVE {
                continue;
            }
            let name = self
                .program
                .events
                .get(usize::from(r.event))
                .map_or_else(|| "?".into(), |e| e.name.clone());
            out.push((r.due, r.id, name));
        }
        out.sort();
        out
    }

    /// The timers an edit carries (`migrate`): those of committed segments, as of an abort of the open
    /// one (its new timers are not there, its cancelled ones are), in (due, id) order.
    pub(crate) fn carried_timers(&self) -> Exec<Vec<CarriedTimer>> {
        let p = Arc::clone(&self.program);
        let mut out = Vec::new();
        for i in 0..TIMERS_MAX {
            let r = self.read_timer(i)?;
            if r.state != T_LIVE || r.flags & F_PENDING != 0 {
                continue;
            }
            let Some(ev) = p.events.get(usize::from(r.event)) else {
                return corrupt("a timer of an unknown script event");
            };
            let value = WireValue::decode(&self.inbox_read(r.payload)?)
                .map_err(|_| Fault::corrupt("a timer's event does not decode"))?;
            out.push(CarriedTimer {
                id: r.id,
                due: r.due,
                scoped: r.flags & F_SCOPED != 0,
                event: ev.name.clone(),
                value,
            });
        }
        out.sort_by_key(|t| (t.due, t.id));
        Ok(out)
    }

    /// Sets a carried timer in this (fresh) instance; `false` when its event is gone or its value no longer
    /// fits, or there is no room.
    pub(crate) fn restore_timer(&mut self, t: &CarriedTimer) -> Exec<bool> {
        let p = Arc::clone(&self.program);
        let Some(event) = p.events.iter().position(|e| e.name == t.event) else {
            return Ok(false);
        };
        if !fits(&p.types, p.events[event].ty, &t.value) {
            return Ok(false);
        }
        let Some(i) = self.free_timer_slot()? else {
            return Ok(false);
        };
        let mut bytes = Vec::new();
        t.value.encode(&mut bytes);
        let Some(payload) = self.inbox_store(&bytes)? else {
            return Ok(false);
        };
        let r = TimerRec {
            state: T_LIVE,
            flags: if t.scoped { F_SCOPED } else { 0 },
            // a table of at most 65 536 events
            event: event as u16,
            id: t.id,
            due: t.due,
            payload,
        };
        self.write_timer(i, &r)?;
        Ok(true)
    }
}
