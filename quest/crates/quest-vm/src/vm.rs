//! A program instance: the immutable program and its flat memory (spec §6.4). Every bit of interpreter
//! and scheduler state — pc, current function, status, fuel debt, the task, the queue, the command
//! buffer — lives in the memory, so after any call [`Vm::mem_bytes`] is the complete state; nothing is
//! kept in Rust fields but the program, the layout-derived allocator handles and what is derived from the
//! program alone.
//!
//! The block's life (design D7, `sched`): [`Vm::new`] leaves the initialisers pending; each [`Vm::tick`]
//! runs tasks under its fuel — the initialisers, then `main` once [`Vm::start_main`] asked for it, then
//! the handlers of the events [`Vm::deliver`] queued.

use std::sync::Arc;

use crate::alloc::Heap;
use crate::bytecode::BYTECODE_VERSION;
use crate::fault::FaultKind;
use crate::host::{Host, PromiseId, SrcLoc};
use crate::layout::{
    H_BYTECODE_VERSION, H_CURRENT_FN, H_EPOCH, H_HOST_API_VERSION, H_PC, H_PROGRAM_HASH, H_STATUS,
    LINK_SENTINEL, LoadError, STATUS_RUNNING,
};
use crate::mem::{Checkpoint, RollbackError};
use crate::program::Program;
pub use crate::promise::Resolved;
use crate::sched::error_type;
pub use crate::sched::{Delivery, Status, TickReport};
use crate::store::Store;
use crate::types::TypeId;
use crate::wire::{WireValue, to_wire};

/// What an instance is created with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmConfig {
    pub mem_bytes: u32,
    /// The instance epoch (promise ids carry it).
    pub epoch: u32,
    /// The host-API version the HOST implements; a program built against another is refused.
    pub host_api_version: u32,
    /// The host API's event names (`HostApi::events`): `deliver` of any other name is `UnknownEvent`.
    /// Configuration, like the host-API version: nothing about it is kept in memory.
    pub events: Vec<String>,
}

pub struct Vm {
    pub(crate) program: Arc<Program>,
    pub(crate) store: Store,
    /// The script heap.
    pub(crate) heap: Heap,
    /// The runtime heap for queued payloads and answers.
    pub(crate) inbox: Heap,
    /// The program's error enum (derived from the program at `new`).
    pub(crate) error_ty: Option<TypeId>,
    /// The host's event names (`VmConfig::events`).
    pub(crate) events: Vec<String>,
    /// `program.content_hash()`, computed once.
    pub(crate) program_hash: u64,
}

/// What [`Vm::new`] and [`Vm::load`] require of a program: this VM's bytecode version, the host's API
/// version, and a valid program (one built by hand may have skipped `finish`/`from_bytes`; the
/// interpreter relies on it).
pub(crate) fn check_program(program: &Program, host_api_version: u32) -> Result<(), LoadError> {
    if program.bytecode_version != BYTECODE_VERSION {
        return Err(LoadError::VersionMismatch {
            found: program.bytecode_version,
        });
    }
    if program.host_api_version != host_api_version {
        return Err(LoadError::HostApiMismatch {
            found: program.host_api_version,
        });
    }
    program
        .validate()
        .map_err(|e| LoadError::Invalid(e.to_string()))
}

/// The result of one [`Vm::debug_run`].
#[derive(Clone, Debug, PartialEq)]
pub struct DebugRun {
    /// Fuel charged during this call; may exceed the budget by the last instruction's overshoot.
    pub fuel_used: u64,
    pub end: RunEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunEnd {
    /// The budget is spent; the next run continues at the next instruction.
    OutOfFuel,
    /// The started function returned (or nothing was running).
    Returned,
    /// The task parked at an `await`: its frames wait in the heap for the promise (`promise`).
    Parked,
    /// `stack[0]` is the faulting instruction, then each caller's call site, innermost first.
    Faulted {
        kind: FaultKind,
        message: String,
        stack: Vec<SrcLoc>,
    },
}

impl Vm {
    /// Checks the versions, validates the program, plans the layout and writes a fresh memory: the
    /// header (layout, versions, program hash, epoch) and both allocators. The undo log starts empty.
    pub fn new(program: Arc<Program>, cfg: VmConfig) -> Result<Vm, LoadError> {
        check_program(&program, cfg.host_api_version)?;
        if !cfg.mem_bytes.is_multiple_of(8) {
            return Err(LoadError::Invalid(
                "mem_bytes is not a multiple of 8".into(),
            ));
        }
        let store = Store::new(u32::from(program.static_words), cfg.mem_bytes)?;
        let program_hash = program.content_hash();
        let mut vm = Vm::adopt(program, program_hash, store, cfg.events);
        let header = [
            (H_BYTECODE_VERSION, u64::from(BYTECODE_VERSION)),
            (H_HOST_API_VERSION, u64::from(cfg.host_api_version)),
            (H_PROGRAM_HASH, vm.program_hash),
            (H_EPOCH, u64::from(cfg.epoch)),
        ];
        let (heap, inbox) = (vm.heap, vm.inbox);
        let init = |store: &mut Store| {
            for (w, v) in header {
                store.wr(w * 8, v)?;
            }
            heap.init(store)?;
            inbox.init(store)
        };
        init(&mut vm.store)
            .map_err(|e| LoadError::Invalid(format!("writing a fresh memory: {e:?}")))?;
        vm.store.mem.clear_log();
        Ok(vm)
    }

    /// The instance of `program` (whose content hash is `program_hash`) with the state `store`, both
    /// checked by the caller; every Rust field is derived from the program, the layout or the
    /// configuration.
    pub(crate) fn adopt(
        program: Arc<Program>,
        program_hash: u64,
        store: Store,
        events: Vec<String>,
    ) -> Vm {
        let l = store.layout;
        let heap = Heap {
            base: l.heap_base,
            end: l.heap_end,
            script: true,
        };
        let inbox = Heap {
            base: l.inbox_base,
            end: l.inbox_end,
            script: false,
        };
        Vm {
            error_ty: error_type(&program.types),
            program_hash,
            program,
            store,
            heap,
            inbox,
            events,
        }
    }

    /// Asks for `main` to run once the initialisers are done (before queued events). `false` when the
    /// program has no `main`, it was already asked for, or the instance failed.
    pub fn start_main(&mut self) -> bool {
        self.sched_start_main()
    }

    /// Queues an event for its handlers (design D7). `Queued` is `Accepted` delivered while a task was in
    /// progress (the owner's warning). A dropped delivery changes nothing.
    pub fn deliver(&mut self, event: &str, payload: &WireValue) -> Delivery {
        self.sched_deliver(event, payload)
    }

    /// Runs tasks for up to `fuel` (less any debt an earlier tick left). `now` smaller than the latest
    /// `now` counts as the latest. Reports go to `host.report`; buffered commands and prints reach the
    /// host when their segment commits. With `fuel` 0 nothing changes. Promise timeouts fire only on
    /// ticks, so a host must keep ticking an instance that has parked handlers, even when it is idle.
    pub fn tick(&mut self, now: u64, fuel: u64, host: &mut dyn Host) -> TickReport {
        self.sched_tick(now, fuel, host)
    }

    /// Answers the promise `id` (design D7, spec §8.2): `Ok` with the value, `Err` with an error of the
    /// program's error enum (anything else, or an answer too large for the inbox, is stored as `Lost`).
    /// A continuation waiting on it becomes ready, ordered with events by arrival. An id that is unknown,
    /// of another epoch or not outstanding is `Unknown` and changes nothing. No host call is made.
    pub fn resolve(&mut self, id: PromiseId, outcome: Result<WireValue, WireValue>) -> Resolved {
        self.promise_resolve(id.0, outcome)
    }

    pub fn status(&self) -> Status {
        self.sched_status()
    }

    /// No task in progress, nothing queued or ready, no zombies left to reap (`Status::Idle`). A failed
    /// instance is not idle: it is `Status::Failed`.
    pub fn is_idle(&self) -> bool {
        self.status() == Status::Idle
    }

    /// Static slot `slot`; 0 when it is out of range (a test and debug accessor).
    pub fn slot(&self, slot: u16) -> u64 {
        if u32::from(slot) >= self.store.layout.static_words {
            return 0;
        }
        self.store.r(self.store.layout.slot_off(slot)).unwrap_or(0)
    }

    /// The whole memory: the complete state of the instance.
    pub fn mem_bytes(&self) -> &[u8] {
        self.store.mem.bytes()
    }

    /// The script regions: static slots and the heap.
    pub fn script_bytes(&self) -> &[u8] {
        let l = &self.store.layout;
        let (lo, hi) = (l.static_base as usize, l.heap_end as usize);
        self.store.mem.bytes().get(lo..hi).unwrap_or(&[])
    }

    /// Words in use in the script heap (0 when its header does not parse).
    pub fn heap_used_words(&self) -> u32 {
        self.heap.used_words(&self.store).unwrap_or(0)
    }

    /// The current position in the undo log (design D3).
    pub fn checkpoint(&self) -> Checkpoint {
        self.store.mem.checkpoint()
    }

    /// Undoes every memory write after `cp` — ticks, deliveries, resolutions alike; nothing changes on
    /// error. The state hash is then bit-exact what it was at `cp` (spec §6.6).
    pub fn rollback_to(&mut self, cp: Checkpoint) -> Result<(), RollbackError> {
        self.store.mem.rollback_to(cp)
    }

    /// Drops the undo history before `cp` (once the sim confirmed it); rolling back to `cp` or later
    /// still works. Never touches the state.
    pub fn forget_before(&mut self, cp: Checkpoint) {
        self.store.mem.forget_before(cp)
    }

    /// Entries in the undo log: one per changed word since the oldest checkpoint still remembered.
    pub fn log_len(&self) -> usize {
        self.store.mem.log_len()
    }

    /// The value in static slot `slot` read as type `ty` (a test and debug accessor).
    #[doc(hidden)]
    pub fn debug_value(&self, slot: u16, ty: TypeId) -> Result<WireValue, FaultKind> {
        if u32::from(slot) >= self.store.layout.static_words {
            return Err(FaultKind::Corrupt);
        }
        let v = self.store.r(self.store.layout.slot_off(slot))?;
        Ok(to_wire(&self.store, self.heap, &self.program.types, ty, v)?.0)
    }

    /// Starts `fn_id` with a sentinel return link: its `Ret` ends the run with [`RunEnd::Returned`]. An
    /// unknown id is not refused here; the next run faults `Corrupt` on it.
    #[doc(hidden)]
    pub fn debug_start(&mut self, fn_id: u16) {
        let (entry, link_off) = match self.program.fns.get(usize::from(fn_id)) {
            Some(f) => (f.entry, Some(self.store.layout.slot_off(f.frame_base))),
            None => (0, None),
        };
        // The header and a validated frame are always in range: these writes cannot fail.
        if let Some(off) = link_off {
            let _ = self.store.ws(off, LINK_SENTINEL);
        }
        let _ = self.store.wr(H_PC * 8, u64::from(entry));
        let _ = self.store.wr(H_CURRENT_FN * 8, u64::from(fn_id));
        let _ = self.store.wr(H_STATUS * 8, STATUS_RUNNING);
    }

    /// Runs the started function for `fuel` (less any debt left by the previous run).
    #[doc(hidden)]
    pub fn debug_run(&mut self, fuel: u64, host: &mut dyn Host) -> DebugRun {
        self.run(fuel, host)
    }
}
