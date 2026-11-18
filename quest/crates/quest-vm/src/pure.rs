//! Pure evaluation and mirrors (spec §5.11 "Purity" and "Across region boundaries", §5.12 parts).
//!
//! [`Vm::eval_pure`] runs one fn of the program — a condition, a substitution, a `where` — on the live
//! instance as a side calculation: no task, no segment, no queue, its own fuel (the instance's fuel debt
//! is neither paid nor added to). It runs on a checkpoint of the undo log and rolls back to it, so the
//! memory, the hash and the undo log afterwards are exactly what they were; an older checkpoint a caller
//! holds stays valid. Only a host query reaches the host: the run stops before any instruction with an
//! effect beyond the memory (`interp::impure`), and nothing is reported.
//!
//! A [`Mirror`] is the values of a set of globals (a read set: `QuestInfo::read_set`,
//! `PartInfo::read_set`), keyed by global index, as wire values, with the content hash of the program it
//! was taken from (an index means nothing in another program: every install and evaluation refuses a
//! mirror of another program). The region holding an NPC evaluates conditions against it — on a
//! [`MirrorVm`], the one scratch-instance path ([`Mirror::eval_pure`] makes one per call); a part's
//! instance takes it into its own globals ([`Vm::install_mirror`]). There is no change tracking: a sender
//! compares the mirror's [`StateHash`] with what it sent last. Neither a mirror is taken nor one installed
//! while the instance's initialisers are pending ([`LoadError::Busy`]): the globals are not set yet, and an
//! initialiser that ran after an install would overwrite it.

use std::collections::BTreeMap;
use std::hash::Hasher;
use std::sync::Arc;

use state_hash::StateHash;

use crate::consts::MEM_BYTES_DEFAULT;
use crate::fault::FaultKind;
use crate::host::Host;
use crate::interp::{Exec, Fault, Meter};
use crate::layout::{
    H_CURRENT_FN, H_INIT_CURSOR, H_PC, H_STATUS, H_TRACE_DROPPED, H_TRACE_LEN, LINK_SENTINEL,
    LoadError, STATUS_RUNNING,
};
use crate::object::obj;
use crate::program::{FnSig, Program};
use crate::types::TypeDesc;
use crate::vm::{RunEnd, Vm, VmConfig};
use crate::wire::{FromWire, WireValue, fits, from_wire, to_wire};

/// Frame words: the return value, the error value, the first parameter.
const FRAME_RET: u16 = 1;
const FRAME_ERR: u16 = 2;
const FRAME_PARAM0: u16 = 3;

/// Why a pure evaluation gave no value. The dialogue runner counts each as false (an empty substitution)
/// and reports it to the owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PureFault {
    /// The fn faulted (or its result could not be read out).
    Fault(FaultKind),
    /// The fuel ran out before the fn returned.
    OutOfFuel,
    /// The fn reached an instruction with an effect: a host command or request, an `await`, `Enqueue`,
    /// a timer.
    Impure,
    /// An error (named by its variant) escaped the fn.
    Raised(String),
    /// No such fn, a fn without a signature (`FnInfo::sig`), arguments that do not fit its parameters —
    /// or, for [`Mirror::eval_pure`], a mirror that does not fit the program.
    BadCall,
}

/// The values of a set of globals, by global index (`Program::globals`), of the program whose
/// `Program::content_hash` is `program`. Equal when their programs are and their values are equal by
/// representation ([`WireValue::bit_eq`]), as their hashes are: `-0.0` is a change from `0.0`.
#[derive(Clone, Debug)]
pub struct Mirror {
    pub program: u64,
    pub globals: BTreeMap<u16, WireValue>,
}

impl PartialEq for Mirror {
    fn eq(&self, other: &Mirror) -> bool {
        self.program == other.program
            && self.globals.len() == other.globals.len()
            && self
                .globals
                .iter()
                .zip(&other.globals)
                .all(|((i, a), (j, b))| i == j && a.bit_eq(b))
    }
}

impl Eq for Mirror {}

impl StateHash for Mirror {
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        self.program.state_hash(h);
        self.globals.state_hash(h);
    }
}

impl Mirror {
    /// Evaluates fn `f` against the mirror on a [`MirrorVm`] made for this one call (the home, default
    /// memory, no roles but those in the mirror). A mirror of another program, or one that does not fit
    /// it, is `BadCall`.
    pub fn eval_pure(
        &self,
        program: &Arc<Program>,
        f: u16,
        args: &[WireValue],
        fuel: u64,
        host: &mut dyn Host,
    ) -> Result<WireValue, PureFault> {
        let cfg = VmConfig {
            mem_bytes: MEM_BYTES_DEFAULT,
            epoch: 0,
            host_api_version: program.host_api_version,
            events: Vec::new(),
            instance: 0,
            roles: Vec::new(),
        };
        MirrorVm::new(Arc::clone(program), cfg, self)
            .map_err(|_| PureFault::BadCall)?
            .eval_pure(f, args, fuel, host)
    }
}

/// A scratch instance holding a mirror: where a pure fn of the program is evaluated over mirrored state
/// (spec §5.11 "Conditions run … over mirrored script state"), the one such path. Its globals in the
/// mirror hold its values, the roles of its `VmConfig` (if any are given) theirs; the others are never set
/// (a pure fn reads only its read set) — no initialiser ever runs on it, it runs nothing but
/// [`MirrorVm::eval_pure`], which leaves it as it was. Keep one while the mirror does not change.
#[derive(Debug)]
pub struct MirrorVm {
    vm: Vm,
}

impl MirrorVm {
    /// A scratch instance of `program` under `cfg` holding `m`. A mirror of another program, one that does
    /// not fit, or a refused program or role is the error.
    pub fn new(program: Arc<Program>, cfg: VmConfig, m: &Mirror) -> Result<MirrorVm, LoadError> {
        let mut vm = Vm::fresh(program, cfg)?;
        if !vm.roles.is_empty() {
            vm.install_roles()?;
        }
        vm.check_mirror(m)?;
        vm.write_mirror(m)?;
        vm.store.mem.clear_log();
        Ok(MirrorVm { vm })
    }

    /// [`Vm::eval_pure`] on the scratch instance.
    pub fn eval_pure(
        &mut self,
        f: u16,
        args: &[WireValue],
        fuel: u64,
        host: &mut dyn Host,
    ) -> Result<WireValue, PureFault> {
        self.vm.eval_pure(f, args, fuel, host)
    }
}

impl Vm {
    /// Runs fn `f` with `args` (wire values fitted to its params) under `fuel`; no task, no segment, no queue.
    /// The VM's state (memory, hash) afterwards equals before: it runs on a checkpoint and rolls back.
    /// A command, request, Enqueue or After is `Impure`; an escaping error `Raised`; a bad fn id/arity `BadCall`.
    pub fn eval_pure(
        &mut self,
        f: u16,
        args: &[WireValue],
        fuel: u64,
        host: &mut dyn Host,
    ) -> Result<WireValue, PureFault> {
        let p = Arc::clone(&self.program);
        let Some((frame, entry, sig)) = p
            .fns
            .get(usize::from(f))
            .and_then(|i| Some((i.frame_base, i.entry, i.sig.as_ref()?)))
        else {
            return Err(PureFault::BadCall);
        };
        if args.len() != sig.params.len()
            || !sig
                .params
                .iter()
                .zip(args)
                .all(|(&t, a)| fits(&p.types, t, a))
        {
            return Err(PureFault::BadCall);
        }
        let cp = self.store.mem.checkpoint();
        self.pure_eval = true;
        let out = self.pure_run(f, frame, entry, sig, args, fuel, host);
        self.pure_eval = false;
        // Taken a moment ago: its history cannot have been dropped.
        let _ = self.store.mem.rollback_to(cp);
        match out {
            Ok(r) => r,
            Err(fault) => Err(PureFault::Fault(fault.kind)),
        }
    }

    /// [`Vm::eval_pure`]'s run, its writes left for the caller to roll back. Frame words are written
    /// without releasing what they held: the rollback restores them.
    #[allow(clippy::too_many_arguments)]
    fn pure_run(
        &mut self,
        f: u16,
        frame: u16,
        entry: u32,
        sig: &FnSig,
        args: &[WireValue],
        fuel: u64,
        host: &mut dyn Host,
    ) -> Exec<Result<WireValue, PureFault>> {
        let p = Arc::clone(&self.program);
        let mut m = Meter::new(fuel);
        let slot = |w: u16| {
            frame
                .checked_add(w)
                .ok_or_else(|| Fault::corrupt("a frame word out of range"))
        };
        for (i, (&t, a)) in (0u16..).zip(sig.params.iter().zip(args)) {
            let v = match from_wire(&mut self.store, self.heap, &p.types, t, a) {
                Ok((v, words)) => {
                    m.words(words);
                    v
                }
                Err(FromWire::WrongType) => return Ok(Err(PureFault::BadCall)),
                Err(FromWire::Fault(k)) => return Err(k.into()),
            };
            self.wt(slot(FRAME_PARAM0 + i)?, v)?;
        }
        self.wt(slot(0)?, LINK_SENTINEL)?;
        self.wt(slot(FRAME_RET)?, 0)?;
        self.wt(slot(FRAME_ERR)?, 0)?;
        for (field, v) in [
            (H_PC, u64::from(entry)),
            (H_CURRENT_FN, u64::from(f)),
            (H_STATUS, STATUS_RUNNING),
            (H_TRACE_LEN, 0),
            (H_TRACE_DROPPED, 0),
        ] {
            self.set_header(field, v)?;
        }
        match self.run_task(&mut m, host)? {
            RunEnd::Returned => {}
            RunEnd::OutOfFuel => return Ok(Err(PureFault::OutOfFuel)),
            // An `await` is impure: it stops before it parks.
            RunEnd::Impure | RunEnd::Parked => return Ok(Err(PureFault::Impure)),
            RunEnd::Faulted { kind, .. } => return Ok(Err(PureFault::Fault(kind))),
        }
        let err = self.rd(slot(FRAME_ERR)?)?;
        if err != 0 {
            return Ok(Err(PureFault::Raised(self.error_name(err)?)));
        }
        let v = self.rd(slot(FRAME_RET)?)?;
        Ok(
            match to_wire(&self.store, self.heap, &p.types, sig.ret, v) {
                Ok((w, _)) => Ok(w),
                Err(k) => Err(PureFault::Fault(k)),
            },
        )
    }

    /// The variant name of the error value `err` of the program's error enum.
    fn error_name(&self, err: u64) -> Exec<String> {
        let Some(t) = self.error_ty else {
            return Err(Fault::corrupt("an error escaped a program without errors"));
        };
        let tag = obj::tag(&self.store, self.heap, err)?;
        match self.program.types.desc(t)? {
            TypeDesc::Enum { variants, .. } => variants
                .get(usize::from(tag))
                .map(|v| v.0.clone())
                .ok_or_else(|| Fault::corrupt("an error whose tag is not a variant")),
            _ => Err(Fault::corrupt("the error type is not an enum")),
        }
    }

    /// Whether an initialiser is still to run (a global not set yet).
    fn inits_pending(&self) -> bool {
        self.header(H_INIT_CURSOR)
            .map_or(true, |c| c < self.program.globals.len() as u64)
    }

    /// The globals of `set` (indices into `Program::globals`) as wire values. An index the program lacks,
    /// or a global whose value cannot be read out (too large), is left out. `Busy` while a task's segment
    /// is open (its writes are not committed) or an initialiser is pending (a global not set yet): take
    /// it again later.
    pub fn mirror(&self, set: &[u16]) -> Result<Mirror, LoadError> {
        if self.store.segment_is_open() || self.inits_pending() {
            return Err(LoadError::Busy);
        }
        let p = &self.program;
        let mut globals = BTreeMap::new();
        for &i in set {
            let Some(g) = p.globals.get(usize::from(i)) else {
                continue;
            };
            let Ok(v) = self.store.r(self.store.layout.slot_off(g.slot)) else {
                continue;
            };
            if let Ok((w, _)) = to_wire(&self.store, self.heap, &p.types, g.ty, v) {
                globals.insert(i, w);
            }
        }
        Ok(Mirror {
            program: self.program_hash,
            globals,
        })
    }

    /// Overwrites the globals of `m` with its values (a part's instance taking the home's mirror). Every
    /// write goes through the undo log, so a rollback undoes it. All or nothing: a mirror of another
    /// program, a global the program lacks, a value that does not fit its global's type, or a full heap is
    /// `Invalid` and changes nothing. `Busy` while a task's segment is open (a task paused on fuel): the
    /// task reads one mirror from its start to its end, and its abort would otherwise undo the install
    /// too; and while an initialiser is pending, which would overwrite it. The caller keeps the mirror and
    /// retries between tasks.
    pub fn install_mirror(&mut self, m: &Mirror) -> Result<(), LoadError> {
        self.check_mirror(m)?;
        if self.store.segment_is_open() || self.inits_pending() {
            return Err(LoadError::Busy);
        }
        self.write_mirror(m)
    }

    /// Whether `m` fits this instance's program: taken from it, naming its globals, each value fitting.
    fn check_mirror(&self, m: &Mirror) -> Result<(), LoadError> {
        let p = &self.program;
        if m.program != self.program_hash {
            return Err(LoadError::Invalid(format!(
                "the mirror is of program {:016x}, not of this one",
                m.program
            )));
        }
        for (&i, w) in &m.globals {
            let Some(g) = p.globals.get(usize::from(i)) else {
                return Err(LoadError::Invalid(format!(
                    "the mirror names global {i}, which the program lacks"
                )));
            };
            if !fits(&p.types, g.ty, w) {
                return Err(LoadError::Invalid(format!(
                    "the mirror's value of {} does not fit its type",
                    g.name
                )));
            }
        }
        Ok(())
    }

    /// [`Vm::install_mirror`]'s writes of a checked mirror (`check_mirror`), whatever the instance is
    /// doing; all or nothing.
    fn write_mirror(&mut self, m: &Mirror) -> Result<(), LoadError> {
        let p = Arc::clone(&self.program);
        let cp = self.store.mem.checkpoint();
        for (&i, w) in &m.globals {
            let g = &p.globals[usize::from(i)];
            let written = match from_wire(&mut self.store, self.heap, &p.types, g.ty, w) {
                Ok((v, _)) => self.put(g.slot, v, p.types.is_ref(g.ty)),
                Err(FromWire::WrongType) => Err(Fault::corrupt("a checked value does not fit")),
                Err(FromWire::Fault(k)) => Err(k.into()),
            };
            if let Err(f) = written {
                // Taken a moment ago: its history cannot have been dropped.
                let _ = self.store.mem.rollback_to(cp);
                return Err(LoadError::Invalid(format!(
                    "installing the mirror's value of {}: {}",
                    g.name, f.message
                )));
            }
        }
        Ok(())
    }
}
