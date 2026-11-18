//! What the runners read and evaluate through: the people, the dialogue mirrors ([`DialogueWorld`]); and
//! the instances beside it ([`Core`]).

use super::*;

/// The instances, and what the runners read through.
pub(super) struct Core {
    pub(super) insts: Vec<Inst>,
    pub(super) env: DialogueWorld,
}

/// What the conversation and scene runners read and evaluate through ([`DialogueEnv`]): the program, the
/// people, the players who completed the quest, the clock, and each instance's **dialogue mirror** — the
/// world but its instances, its conversations and its scenes. The stub's [`World`] runs its runners over one;
/// a caller that drives `quest_vm::dialogue_run` itself (the fuzz targets) uses the same.
///
/// Conditions are evaluated over the mirrors on a scratch `MirrorVm` (spec §5.11 "Conditions run … over
/// mirrored script state"); a fault is counted as false by the runner and kept in [`DialogueWorld::faults`].
/// A clone keeps the mirrors (not their scratch instances, made again at the next evaluation).
#[derive(Clone)]
pub struct DialogueWorld {
    program: Arc<Program>,
    /// `program.content_hash()`.
    program_key: u64,
    pub people: People,
    /// `quest.completed_by` answers from it.
    pub completed_by: BTreeSet<i64>,
    /// The tick, as the queries of a condition see it.
    pub now: u64,
    /// The pure evaluations that faulted, by instance (the caller takes them).
    pub faults: Vec<(u32, PureFault)>,
    pub(super) api_events: Vec<HostStruct>,
    /// The quest's role values (a dialogue mirror's scratch instance is made with them).
    pub roles: Vec<(String, WireValue)>,
    /// Each instance's memory (and a scratch instance's).
    pub mem_bytes: u32,
    /// The instances there are (`DialogueEnv::graph` answers for these): set by [`DialogueWorld::take_mirrors`].
    instances: usize,
    /// Each instance's dialogue mirror, by instance id: what its conditions are evaluated over.
    mirrors: BTreeMap<u32, DialogueMirror>,
}

/// An instance's dialogue read set as last committed (spec §5.11 "Conditions run … over mirrored script
/// state"): the conditions, substitutions, `for` targets and filters of its dialogues, and the stage the
/// attached set depends on, are evaluated over it, never over a segment in progress.
struct DialogueMirror {
    mirror: Mirror,
    hash: u64,
    /// A scratch instance holding the mirror, made at its first evaluation.
    scratch: Option<MirrorVm>,
}

impl Clone for DialogueMirror {
    fn clone(&self) -> DialogueMirror {
        DialogueMirror {
            mirror: self.mirror.clone(),
            hash: self.hash,
            scratch: None,
        }
    }
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

impl DialogueWorld {
    /// The program the instances run (replaced by [`DialogueWorld::set_program`]).
    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    /// The program's content hash (`Program::content_hash`): what a record and a mirror are keyed on.
    pub fn key(&self) -> u64 {
        self.program_key
    }

    /// No mirror yet: [`DialogueWorld::take_mirrors`] takes the first.
    pub fn new(
        program: Arc<Program>,
        people: People,
        roles: Vec<(String, WireValue)>,
        mem_bytes: u32,
    ) -> DialogueWorld {
        DialogueWorld {
            program_key: program.content_hash(),
            program,
            people,
            completed_by: BTreeSet::new(),
            now: 0,
            faults: Vec::new(),
            api_events: stub_api().events,
            roles,
            mem_bytes,
            instances: 0,
            mirrors: BTreeMap::new(),
        }
    }

    /// The script edited: `program` from now on; the old program's mirrors are dropped (take them again).
    pub fn set_program(&mut self, program: Arc<Program>) {
        self.program_key = program.content_hash();
        self.program = program;
        self.mirrors.clear();
    }

    /// Every mirror dropped (a reset, a load: take them again).
    pub fn clear_mirrors(&mut self) {
        self.mirrors.clear();
    }

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

    /// Takes each instance's dialogue mirror from `vms` (in id order: the home first), except of one
    /// mid-segment or still initialising (it keeps its last); a mirror that did not change keeps its scratch
    /// instance. The instances the runners may name are those of `vms`.
    pub fn take_mirrors<'a>(&mut self, vms: impl IntoIterator<Item = &'a Vm>) {
        let set = self.dialogue_set();
        let mut count = 0;
        for (k, vm) in vms.into_iter().enumerate() {
            count = k + 1;
            let Ok(k) = u32::try_from(k) else { break };
            // Mid-segment or still initialising: it keeps its last.
            let Ok(mirror) = vm.mirror(&set) else {
                continue;
            };
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
        self.instances = count;
    }

    /// A global of instance `i`'s dialogue mirror.
    pub fn mirrored(&self, i: u32, global: u16) -> Option<&WireValue> {
        self.mirrors.get(&i)?.mirror.globals.get(&global)
    }

    /// Hashes the mirrors: their count, then each instance id and mirror.
    pub fn hash_mirrors<H: Hasher>(&self, h: &mut H) {
        (self.mirrors.len() as u64).state_hash(h);
        for (i, d) in &self.mirrors {
            i.state_hash(h);
            d.mirror.state_hash(h);
        }
    }

    /// The home's current stage (an index into `QuestInfo::stages`), read from the home's dialogue mirror,
    /// as the conditions are: never a stage a segment in progress set.
    pub fn stage(&self) -> Option<u16> {
        let q = self.program.quest.as_ref()?;
        match self.mirrored(0, q.stage_global)? {
            WireValue::Variant { name, .. } => q
                .stages
                .iter()
                .position(|s| s == name)
                .and_then(|k| u16::try_from(k).ok()),
            _ => None,
        }
    }

    /// What is attached to `npc` for `player` now (spec §5.11; the [`World`] module docs): the home's
    /// conversations of the current stage whose `for` target is the NPC or whose `where` filter holds, unless
    /// `detached` holds (0, npc, name); then each (instance, npc, name) of `attached` naming a conversation
    /// of the current stage.
    pub fn attachments(
        &mut self,
        player: &WireValue,
        npc: &WireValue,
        attached: &BTreeSet<(u32, i64, String)>,
        detached: &BTreeSet<(u32, i64, String)>,
    ) -> Vec<Attachment> {
        let Some(npc_id) = id_of(Some(npc)) else {
            return Vec::new();
        };
        let stage = self.stage();
        let program = Arc::clone(&self.program);
        let cast = [player.clone(), npc.clone()];
        let in_stage =
            |d: &quest_vm::dialogue::DialogueDef| d.stage.is_none_or(|s| Some(s) == stage);
        let mut out = Vec::new();
        for (k, d) in program.dialogues.dialogues.iter().enumerate() {
            let Ok(k) = u16::try_from(k) else { break };
            if d.kind != DialogueKind::Conversation || d.cast != 2 || !in_stage(d) {
                continue;
            }
            if detached.contains(&(0, npc_id, d.name.clone())) {
                continue;
            }
            // A fault counts as false and is reported, as in any condition.
            let holds = match d.target.or(d.filter) {
                Some(f) => match self.eval_at(0, f, &self.cast_args(f, &cast), 1) {
                    Ok(v) if d.target.is_some() => id_of(Some(&v)) == Some(npc_id),
                    Ok(v) => v == WireValue::Bool(true),
                    Err(fault) => {
                        self.faults.push((0, fault));
                        false
                    }
                },
                None => false,
            };
            if holds {
                out.push(Attachment {
                    instance: InstanceId(0),
                    dialogue: k,
                });
            }
        }
        for (i, n, name) in attached {
            if *n != npc_id {
                continue;
            }
            let found = program
                .dialogues
                .dialogues
                .iter()
                .position(|d| d.name == *name && in_stage(d))
                .and_then(|k| u16::try_from(k).ok());
            if let Some(k) = found {
                out.push(Attachment {
                    instance: InstanceId(*i),
                    dialogue: k,
                });
            }
        }
        out
    }

    /// Evaluates fn `f` of instance `i` over its dialogue mirror (on a `MirrorVm` holding it). No mirror, or one the scratch instance refuses, is `BadCall`.
    pub fn eval_at(
        &mut self,
        i: usize,
        f: u16,
        args: &[WireValue],
        depth: u32,
    ) -> Result<WireValue, PureFault> {
        let DialogueWorld {
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
            let vm = MirrorVm::new(Arc::clone(program), cfg, &d.mirror)
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

    /// The first parameters of fn `f` from `cast` (a target, a filter, a condition takes the cast).
    pub fn cast_args(&self, f: u16, cast: &[WireValue]) -> Vec<WireValue> {
        let n = self
            .program
            .fns
            .get(usize::from(f))
            .and_then(|x| x.sig.as_ref())
            .map_or(0, |s| s.params.len());
        cast.iter().take(n).cloned().collect()
    }
}

impl Core {
    /// Each instance's dialogue mirror taken again ([`DialogueWorld::take_mirrors`]).
    pub(super) fn take_mirrors(&mut self) {
        self.env
            .take_mirrors(self.insts.iter().map(|x| &x.runner.vm));
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
        let Core { insts, env } = self;
        let DialogueWorld {
            people,
            completed_by,
            now,
            ..
        } = env;
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
}

impl DialogueEnv for DialogueWorld {
    fn graph(&self, i: InstanceId) -> Option<&quest_vm::dialogue::DialogueGraph> {
        let k = usize::try_from(i.0).ok()?;
        (k < self.instances).then_some(&self.program.dialogues)
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
