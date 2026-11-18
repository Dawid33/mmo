//! Editing a script and Reset (spec §6.9; decisions 13, 14).
//!
//! # Migration
//! [`Vm::migrate`] moves an instance's state into a recompiled program by NAME, never by slot: the old
//! instance is read out as self-describing values and a fresh instance of the new program is built from
//! them, so nothing of the old memory's layout, tables or heap survives. Every step below works on a
//! copy: the old instance is never changed, so when a migration fails ([`MigrateError`]) the edit is
//! refused and the block runs on with its old program and state, exactly as it was.
//! 1. A segment still open (a task out of fuel mid-run) is aborted: its writes are undone as if it
//!    faulted, and the task does not continue.
//! 2. Parked tasks are cancelled and every promise record is dropped with the old memory (the fresh
//!    instance has none), so a later answer to one of them is `Resolved::Unknown`. Writes the host
//!    already made stay: writes are final (§8.2). The fresh instance continues the old promise serial, so
//!    with the same epoch no id is issued twice.
//! 3. Every initialised global, and every queued event, is read out as a [`WireValue`].
//! 4. For each global of the new program, in order: a role's global takes the role's value from `cfg`
//!    as in [`Vm::new`] (an old value of that name is dropped: a role never re-binds) and is not
//!    reported. Otherwise an old global of the same qualified name whose value loads structurally into
//!    the new type is CARRIED; else an orphan of that name that loads is ADOPTED; either way its
//!    initialiser does not run (`H_INIT_SKIP`). Otherwise its initialiser runs at the next ticks
//!    (INITIALISED). An old global that was not carried becomes an orphan.
//! 5. The queued events are delivered again in their order; one with no handler in the new program, or
//!    whose shape no longer fits, is dropped. An event whose handlers had already started when the edit
//!    landed is not delivered again — re-running handlers that already ran is worse than losing the rest
//!    — and counts as dropped.
//! 6. The timers (those of committed segments: the open one was undone) are set again with their ids,
//!    due ticks and scopes; one whose script event is gone, or whose value no longer fits it, is dropped.
//!
//! The fresh instance has the old epoch, promise and timer serials and latest `now`, and a quest start
//! that already began does not run again; everything else — `main`'s state, the arrival counter, the
//! fuel debt — starts as in [`Vm::new`]. A failed instance migrates too: its initialised globals carry
//! over and the new instance is not failed.
//!
//! # Orphans
//! Kept outside the flat memory (the script cannot address them) in a list sorted by (name, type text),
//! one per pair: a newer orphan replaces an older one of the same name and type. They are part of the
//! state: saved, loaded and hashed (`save`).
//!
//! # Reset
//! [`Vm::reset`] replaces the instance with a fresh one of the same program at the next epoch: the
//! memory is byte for byte that of [`Vm::new`] with `epoch + 1` (the same instance and roles), no
//! orphans, no timers, an empty undo log.
//!
//! Both clear the undo log: rollback does not cross an edit or a Reset (decision 14).

use std::sync::Arc;

use crate::consts::QUEUE_LEN;
use crate::fault::FaultKind;
use crate::interp::Fault;
use crate::layout::{
    H_EPOCH, H_EV_HANDLER, H_INIT_CURSOR, H_NOW, H_PROMISE_SERIAL, H_QUEST_START, H_QUEUE_COUNT,
    H_QUEUE_HEAD, H_RESUME_PARKED, H_TASK, H_TIMER_SERIAL, LoadError,
};
use crate::program::{GlobalInfo, Program};
use crate::sched::{QSTART_DONE, QSTART_WANTED, TASK_START, word_off};
use crate::types::{TypeDesc, TypeId, TypeTable};
use crate::vm::{Delivery, Vm, VmConfig};
use crate::wire::{WireValue, from_wire, to_wire};

/// A value kept aside by an edit: its global is gone, or its new type cannot hold it.
#[derive(Clone, Debug, PartialEq)]
pub struct Orphan {
    /// The global's qualified name.
    pub name: String,
    /// The old type, as source writes it (`int`, `list[str]`, `Ledger`).
    pub ty: String,
    pub value: WireValue,
}

/// What [`Vm::migrate`] did. Names are qualified; `carried`, `adopted` and `initialised` in the new
/// program's order, `orphaned` in the old one's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Old globals whose values the new globals of the same name took.
    pub carried: Vec<String>,
    /// New globals whose initialisers run.
    pub initialised: Vec<String>,
    /// Old globals whose values became orphans.
    pub orphaned: Vec<String>,
    /// New globals that took an orphan's value.
    pub adopted: Vec<String>,
    /// Queued events not delivered to the new program.
    pub events_dropped: u32,
    /// Parked tasks cancelled (and a resumed one cut off mid-segment).
    pub handlers_cancelled: u32,
    /// Timers not set again: their script event is gone or their value no longer fits it.
    pub timers_dropped: u32,
}

/// Printing deeper than this shows `…` (as the compiler's type printer).
const TYPE_TEXT_DEPTH_MAX: u32 = 16;

/// `t` as source writes it; a corrupt id prints `?`.
fn type_text(types: &TypeTable, t: TypeId) -> String {
    let mut out = String::new();
    write_type(types, t, 0, &mut out);
    out
}

fn write_type(types: &TypeTable, t: TypeId, depth: u32, out: &mut String) {
    if depth > TYPE_TEXT_DEPTH_MAX {
        out.push('…');
        return;
    }
    let inner = |name: &str, args: &[TypeId], out: &mut String| {
        out.push_str(name);
        out.push('[');
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_type(types, *a, depth + 1, out);
        }
        out.push(']');
    };
    match types.desc(t) {
        Err(_) => out.push('?'),
        Ok(TypeDesc::Unit) => out.push_str("()"),
        Ok(TypeDesc::Bool) => out.push_str("bool"),
        Ok(TypeDesc::Int) => out.push_str("int"),
        Ok(TypeDesc::Float) => out.push_str("float"),
        Ok(TypeDesc::Str) => out.push_str("str"),
        Ok(TypeDesc::List(e)) => inner("list", &[*e], out),
        Ok(TypeDesc::Dict(k, v)) => inner("dict", &[*k, *v], out),
        Ok(TypeDesc::Promise(e)) => inner("Promise", &[*e], out),
        Ok(TypeDesc::Struct { name, .. }) => out.push_str(name),
        Ok(TypeDesc::Enum { name, variants }) => match variants.as_slice() {
            [(none, n), (some, s)] if name == "Option" && none == "None" && some == "Some" => {
                match (n.as_slice(), s.as_slice()) {
                    ([], [(_, v)]) => inner("Option", &[*v], out),
                    _ => out.push_str(name),
                }
            }
            _ => out.push_str(name),
        },
    }
}

/// Why [`Vm::migrate`] refused an edit. The old instance is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrateError {
    /// The new program or the memory size is refused, as by [`Vm::new`].
    Refused(LoadError),
    /// The old instance's memory does not parse.
    Corrupt(String),
    /// The value of global `name` cannot be read out (`ValueTooLarge`: the walk visits shared values
    /// once per reference, so a heavily aliased value can exceed the work bound).
    GlobalUnreadable { name: String, kind: FaultKind },
    /// The new instance's inbox has no room for the set of carried globals.
    InboxFull,
}

fn invalid(f: Fault) -> MigrateError {
    MigrateError::Corrupt(f.message)
}

/// Adds `o` to the sorted orphan list, replacing one of the same name and type.
fn add_orphan(orphans: &mut Vec<Orphan>, o: Orphan) {
    match orphans.binary_search_by(|x| (&x.name, &x.ty).cmp(&(&o.name, &o.ty))) {
        Ok(i) => orphans[i] = o,
        Err(i) => orphans.insert(i, o),
    }
}

impl Vm {
    /// The orphaned values (see the module docs), sorted by name and type.
    pub fn orphans(&self) -> &[Orphan] {
        &self.orphans
    }

    /// Re-initialises the instance (spec §6.9): afterwards it is exactly [`Vm::new`] of its program with
    /// the next epoch — the variables initialise again, the queue, parked handlers, promises and orphans
    /// are gone, a failed instance is no longer failed, and the undo log is empty. Promise ids then carry
    /// the new epoch, so none repeats (decision 13). At the last epoch the epoch stays `u32::MAX`.
    pub fn reset(&mut self) {
        let epoch = self
            .header(H_EPOCH)
            .ok()
            .and_then(|e| u32::try_from(e).ok())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let cfg = VmConfig {
            mem_bytes: self.store.mem.len(),
            epoch,
            host_api_version: self.program.host_api_version,
            events: self.events.clone(),
            instance: self.instance,
            roles: self.roles.clone(),
        };
        // The program and the memory size built this instance, so they build a fresh one.
        if let Ok(vm) = Vm::new(Arc::clone(&self.program), cfg) {
            *self = vm;
        }
    }

    /// A new instance of `new`, the edited script's program, holding this instance's state (the module
    /// docs give the steps), and what happened to each variable. `cfg` configures it as in [`Vm::new`],
    /// except that the epoch is this instance's (`cfg.epoch` is not used). Never changes `self`: on an
    /// error the edit is refused and this instance runs on as it was.
    pub fn migrate(
        &self,
        new: Arc<Program>,
        cfg: VmConfig,
    ) -> Result<(Vm, MigrationReport), MigrateError> {
        let epoch = u32::try_from(self.header(H_EPOCH).map_err(invalid)?)
            .map_err(|_| MigrateError::Corrupt("epoch out of range".into()))?;
        let mut next = Vm::fresh(new, VmConfig { epoch, ..cfg }).map_err(MigrateError::Refused)?;
        let mut skip = next.install_roles().map_err(MigrateError::Refused)?;
        let mut report = MigrationReport::default();

        // The old instance is read through a copy, whose open segment is undone (an abort restores
        // only the script words, so the copy could not run on; it is only read).
        let mut old_vm = Vm::adopt(
            Arc::clone(&self.program),
            self.program_hash,
            self.store.clone(),
            Vec::new(),
            self.instance,
            Vec::new(),
        );
        let this = &mut old_vm;

        // 1, 2: the open segment is undone; the parked tasks go with the old memory.
        let in_progress = this.task_in_progress().map_err(invalid)?;
        let resumed_mid_segment =
            in_progress && this.header(H_RESUME_PARKED).map_err(invalid)? != 0;
        // A start cut off mid-segment runs again; one that ended (or parked) does not.
        let started = this.header(H_QUEST_START).map_err(invalid)? == QSTART_DONE
            && !(in_progress && this.header(H_TASK).map_err(invalid)? == TASK_START);
        this.store.segment_abort();
        report.handlers_cancelled =
            this.parked_count().map_err(invalid)? + u32::from(resumed_mid_segment);

        // 3: the old state as wire values.
        let mut old = this.read_globals()?;
        let (events, in_dispatch) = this.read_queue().map_err(invalid)?;
        report.events_dropped = u32::from(in_dispatch);

        // 4: the new globals.
        let p = Arc::clone(&next.program);
        let mut orphans = self.orphans.clone();
        for (i, g) in p.globals.iter().enumerate() {
            if skip[i / 64] >> (i % 64) & 1 != 0 {
                // a role's, installed from `cfg`
                old.retain(|o| o.name != g.name);
                continue;
            }
            let carried = match old.iter().position(|o| o.name == g.name) {
                Some(k) if next.load_global(g, &old[k].value) => {
                    old.remove(k);
                    report.carried.push(g.name.clone());
                    true
                }
                _ => false,
            };
            let adopted = !carried
                && match orphans
                    .iter()
                    .position(|o| o.name == g.name && next.load_global(g, &o.value))
                {
                    Some(k) => {
                        orphans.remove(k);
                        report.adopted.push(g.name.clone());
                        true
                    }
                    None => false,
                };
            if carried || adopted {
                skip[i / 64] |= 1 << (i % 64);
            } else {
                report.initialised.push(g.name.clone());
            }
        }
        for o in old {
            report.orphaned.push(o.name.clone());
            add_orphan(&mut orphans, o);
        }
        next.orphans = orphans;
        if !next.set_init_skip(&skip).map_err(invalid)? {
            return Err(MigrateError::InboxFull);
        }

        // 5: the queued events, in order.
        for (name, payload) in events {
            if matches!(next.deliver(&name, &payload), Delivery::Dropped(_)) {
                report.events_dropped += 1;
            }
        }

        // 6: the timers.
        for t in this.carried_timers().map_err(invalid)? {
            if !next.restore_timer(&t).map_err(invalid)? {
                report.timers_dropped += 1;
            }
        }

        for field in [H_PROMISE_SERIAL, H_TIMER_SERIAL, H_NOW] {
            let v = this.header(field).map_err(invalid)?;
            next.set_header(field, v).map_err(invalid)?;
        }
        if started && next.header(H_QUEST_START).map_err(invalid)? == QSTART_WANTED {
            next.set_header(H_QUEST_START, QSTART_DONE)
                .map_err(invalid)?;
        }
        next.store.mem.clear_log();
        Ok((next, report))
    }

    /// Every global whose value exists (initialised, or carried by an earlier migration), as an orphan
    /// would hold it, in program order.
    fn read_globals(&self) -> Result<Vec<Orphan>, MigrateError> {
        let p = &self.program;
        let cursor = self.header(H_INIT_CURSOR).map_err(invalid)?;
        let mut out = Vec::new();
        for (i, g) in (0u64..).zip(&p.globals) {
            if i >= cursor && !self.init_skipped(i).map_err(invalid)? {
                continue;
            }
            let v = self.slot(g.slot);
            let (value, _) =
                to_wire(&self.store, self.heap, &p.types, g.ty, v).map_err(|kind| {
                    MigrateError::GlobalUnreadable {
                        name: g.name.clone(),
                        kind,
                    }
                })?;
            out.push(Orphan {
                name: g.name.clone(),
                ty: type_text(&p.types, g.ty),
                value,
            });
        }
        Ok(out)
    }

    /// The queued events (name, payload) in order, and whether an event's handlers are part-way through.
    fn read_queue(&self) -> Result<(Vec<(String, WireValue)>, bool), Fault> {
        let (head, count) = (self.header(H_QUEUE_HEAD)?, self.header(H_QUEUE_COUNT)?);
        let qlen = QUEUE_LEN as u64;
        if head >= qlen || count > qlen {
            return Err(Fault::corrupt("queue head or count out of range"));
        }
        let mut out = Vec::new();
        for k in 0..count {
            let at = self.queue_rec((head + k) % qlen)?;
            let first = self.store.r(word_off(at, 1)?)?;
            let payload = self.store.r(word_off(at, 2)?)?;
            let Some(h) = usize::try_from(first)
                .ok()
                .and_then(|i| self.program.handlers.get(i))
            else {
                return Err(Fault::corrupt("a queued event's handler is out of range"));
            };
            let bytes = self.inbox_read(payload)?;
            let w = WireValue::decode(&bytes)
                .map_err(|_| Fault::corrupt("a queued payload does not decode"))?;
            out.push((h.event.clone(), w));
        }
        Ok((out, self.header(H_EV_HANDLER)? != 0))
    }

    /// Loads `w` into global `g` of this (fresh) instance; false when it does not fit (or the heap has no
    /// room: whatever was built is reaped by the next ticks).
    fn load_global(&mut self, g: &GlobalInfo, w: &WireValue) -> bool {
        let p = Arc::clone(&self.program);
        match from_wire(&mut self.store, self.heap, &p.types, g.ty, w) {
            Ok((v, _)) => self.put(g.slot, v, p.types.is_ref(g.ty)).is_ok(),
            Err(_) => false,
        }
    }
}
