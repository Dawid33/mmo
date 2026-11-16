//! A program instance: the immutable program and its flat memory (spec §6.4). Every bit of interpreter
//! state — pc, current function, status, fuel debt — lives in the memory's header, so after any run
//! [`Vm::mem_bytes`] is the complete state; nothing is kept in Rust fields but the program and the
//! layout-derived allocator handles.

use std::sync::Arc;

use crate::alloc::Heap;
use crate::bytecode::BYTECODE_VERSION;
use crate::fault::FaultKind;
use crate::host::{Host, SrcLoc};
use crate::layout::{
    H_BYTECODE_VERSION, H_CURRENT_FN, H_EPOCH, H_HOST_API_VERSION, H_PC, H_PROGRAM_HASH, H_STATUS,
    LINK_SENTINEL, LoadError, STATUS_RUNNING,
};
use crate::program::Program;
use crate::store::Store;

/// What an instance is created with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmConfig {
    pub mem_bytes: u32,
    /// The instance epoch (promise ids carry it).
    pub epoch: u32,
    /// The host-API version the HOST implements; a program built against another is refused.
    pub host_api_version: u32,
}

pub struct Vm {
    pub(crate) program: Arc<Program>,
    pub(crate) store: Store,
    /// The script heap.
    pub(crate) heap: Heap,
    /// The runtime heap for queued payloads and answers.
    #[expect(dead_code, reason = "the scheduler (Task 12) is its first reader")]
    pub(crate) inbox: Heap,
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
        if program.bytecode_version != BYTECODE_VERSION {
            return Err(LoadError::VersionMismatch {
                found: program.bytecode_version,
            });
        }
        if program.host_api_version != cfg.host_api_version {
            return Err(LoadError::HostApiMismatch {
                found: program.host_api_version,
            });
        }
        // A program built by hand may have skipped `finish`/`from_bytes`; the interpreter relies on it.
        program
            .validate()
            .map_err(|e| LoadError::Invalid(e.to_string()))?;
        if !cfg.mem_bytes.is_multiple_of(8) {
            return Err(LoadError::Invalid(
                "mem_bytes is not a multiple of 8".into(),
            ));
        }
        let mut store = Store::new(u32::from(program.static_words), cfg.mem_bytes)?;
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
        let header = [
            (H_BYTECODE_VERSION, u64::from(BYTECODE_VERSION)),
            (H_HOST_API_VERSION, u64::from(cfg.host_api_version)),
            (H_PROGRAM_HASH, program.content_hash()),
            (H_EPOCH, u64::from(cfg.epoch)),
        ];
        let init = |store: &mut Store| {
            for (w, v) in header {
                store.wr(w * 8, v)?;
            }
            heap.init(store)?;
            inbox.init(store)
        };
        init(&mut store)
            .map_err(|e| LoadError::Invalid(format!("writing a fresh memory: {e:?}")))?;
        store.mem.clear_log();
        Ok(Vm {
            program,
            store,
            heap,
            inbox,
        })
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
