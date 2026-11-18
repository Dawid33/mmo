//! What the runners read and evaluate through: the instances, the people, the dialogue mirrors.

use super::*;

/// What the runners read through ([`DialogueEnv`]): everything but the conversations and scenes.
pub(super) struct Core {
    pub(super) program: Arc<Program>,
    pub(super) program_key: u64,
    pub(super) insts: Vec<Inst>,
    pub(super) people: People,
    pub(super) completed_by: BTreeSet<i64>,
    pub(super) now: u64,
    pub(super) faults: Vec<(u32, PureFault)>,
    pub(super) api_events: Vec<HostStruct>,
    /// The quest's role values (a dialogue mirror's scratch instance is made with them).
    pub(super) roles: Vec<(String, WireValue)>,
    /// Each instance's memory (and a scratch instance's).
    pub(super) mem_bytes: u32,
    /// Each instance's dialogue mirror, by instance id: what its conditions are evaluated over.
    pub(super) mirrors: BTreeMap<u32, DialogueMirror>,
}

/// An instance's dialogue read set as last committed (spec §5.11 "Conditions run … over mirrored script
/// state"): the conditions, substitutions, `for` targets and filters of its dialogues, and the stage the
/// attached set depends on, are evaluated over it, never over a segment in progress.
pub(super) struct DialogueMirror {
    pub(super) mirror: Mirror,
    hash: u64,
    /// A scratch instance holding the mirror, made at its first evaluation.
    scratch: Option<Vm>,
}

/// The host a pure evaluation sees: the people's queries only.
pub(super) struct EvalHost<'a> {
    pub(super) people: &'a People,
    pub(super) completed_by: &'a BTreeSet<i64>,
    pub(super) tick: u64,
    pub(super) depth: u32,
}

impl Host for EvalHost<'_> {
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError> {
        let ctx = QueryCtx {
            tick: self.tick,
            depth: self.depth,
            completed_by: self.completed_by,
        };
        self.people.query(f, args, &ctx).unwrap_or_else(|| {
            Err(QueryError::Failed(format!(
                "the stub answers no query {} in a condition",
                op_name(f).unwrap_or_else(|| format!("(host fn {f})"))
            )))
        })
    }
    fn check(&mut self, f: u16, _: &[WireValue]) -> Result<(), QueryError> {
        Err(QueryError::Failed(format!(
            "host fn {f} checked in a condition"
        )))
    }
    fn command(&mut self, _: u16, _: &[WireValue]) {}
    fn request(&mut self, _: PromiseId, _: u16, _: &[WireValue]) {}
    fn print(&mut self, _: &str) {}
    fn report(&mut self, _: Report) {}
    fn route(&mut self, _: &str, _: &WireValue) {}
}

impl Core {
    /// The globals a dialogue mirror holds: the quest's dialogue read set and its stage; for a program
    /// without a quest, which has no read set, every global.
    fn dialogue_set(&self) -> Vec<u16> {
        let p = &self.program;
        match &p.quest {
            Some(q) => {
                let mut set = q.read_set.clone();
                set.push(q.stage_global);
                set.sort_unstable();
                set.dedup();
                set
            }
            None => (0..p.globals.len())
                .filter_map(|g| u16::try_from(g).ok())
                .collect(),
        }
    }

    /// Takes each instance's dialogue mirror, except of one mid-segment (it keeps its last); a mirror that
    /// did not change keeps its scratch instance.
    pub(super) fn take_mirrors(&mut self) {
        let set = self.dialogue_set();
        for (k, inst) in self.insts.iter().enumerate() {
            let Ok(k) = u32::try_from(k) else { break };
            if inst.runner.vm.in_segment() {
                continue;
            }
            let mirror = inst.runner.vm.mirror(&set);
            let hash = hash_of(&mirror);
            if self.mirrors.get(&k).is_none_or(|d| d.hash != hash) {
                self.mirrors.insert(
                    k,
                    DialogueMirror {
                        mirror,
                        hash,
                        scratch: None,
                    },
                );
            }
        }
    }

    /// A global of instance `i`'s dialogue mirror.
    pub(super) fn mirrored(&self, i: u32, global: u16) -> Option<&WireValue> {
        self.mirrors.get(&i)?.mirror.globals.get(&global)
    }

    /// Evaluates fn `f` of instance `i` over its dialogue mirror (`Vm::eval_pure` on a scratch instance
    /// holding it). No mirror, or one the scratch instance refuses, is `BadCall`.
    pub(super) fn eval_at(
        &mut self,
        i: usize,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        let Core {
            program,
            people,
            completed_by,
            now,
            roles,
            mem_bytes,
            mirrors,
            ..
        } = self;
        let (Ok(k), Ok(id)) = (u32::try_from(i), u16::try_from(i)) else {
            return Err(PureFault::BadCall);
        };
        let Some(d) = mirrors.get_mut(&k) else {
            return Err(PureFault::BadCall);
        };
        if d.scratch.is_none() {
            let cfg = vm_config(*mem_bytes, id, roles.clone());
            let mut vm = Vm::new(Arc::clone(program), cfg).map_err(|_| PureFault::BadCall)?;
            vm.install_mirror(&d.mirror)
                .map_err(|_| PureFault::BadCall)?;
            d.scratch = Some(vm);
        }
        let Some(vm) = d.scratch.as_mut() else {
            return Err(PureFault::BadCall);
        };
        let mut host = EvalHost {
            people,
            completed_by,
            tick: *now,
            depth,
        };
        vm.eval_pure(f, args, COND_FUEL, &mut host)
    }

    /// Evaluates fn `f` of instance `i` on its live VM (the placement's `requires` and `where`, checked
    /// between ticks).
    pub(super) fn eval_live(
        &mut self,
        i: usize,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        let Core {
            insts,
            people,
            completed_by,
            now,
            ..
        } = self;
        let Some(inst) = insts.get_mut(i) else {
            return Err(PureFault::BadCall);
        };
        let mut host = EvalHost {
            people,
            completed_by,
            tick: *now,
            depth,
        };
        inst.runner.vm.eval_pure(f, args, COND_FUEL, &mut host)
    }

    /// The first parameters of fn `f` from `cast` (a target, a filter, a condition takes the cast).
    pub(super) fn cast_args(&self, f: u16, cast: &[WireValue]) -> Vec<WireValue> {
        let n = self
            .program
            .fns
            .get(usize::from(f))
            .and_then(|x| x.sig.as_ref())
            .map_or(0, |s| s.params.len());
        cast.iter().take(n).cloned().collect()
    }
}

impl DialogueEnv for Core {
    fn graph(&self, i: InstanceId) -> Option<&quest_vm::dialogue::DialogueGraph> {
        let k = usize::try_from(i.0).ok()?;
        (k < self.insts.len()).then_some(&self.program.dialogues)
    }

    fn eval(
        &mut self,
        i: InstanceId,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        match usize::try_from(i.0) {
            Ok(k) => self.eval_at(k, f, args, depth),
            Err(_) => Err(PureFault::BadCall),
        }
    }

    fn report(&mut self, i: InstanceId, fault: PureFault) {
        self.faults.push((i.0, fault));
    }

    fn event(&self, _: InstanceId, event: u16, script: bool) -> Option<EventShape> {
        if !script {
            let e = self.api_events.get(usize::from(event))?;
            return Some(EventShape {
                name: e.name.clone(),
                fields: e.fields.iter().map(|f| f.0.clone()).collect(),
            });
        }
        let p = &self.program;
        let e = p.events.get(usize::from(event))?;
        match p.types.desc(e.ty).ok()? {
            TypeDesc::Struct { fields, .. } => Some(EventShape {
                name: e.name.clone(),
                fields: fields.iter().map(|f| f.0.clone()).collect(),
            }),
            _ => None,
        }
    }

    fn program_key(&self, i: InstanceId) -> Option<u64> {
        self.graph(i).map(|_| self.program_key)
    }
}
