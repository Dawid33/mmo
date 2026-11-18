//! The scripted inputs delivered at once: events, `near`, `kill`, `lose`, `edit`.

use super::*;

impl World {
    pub(super) fn event_input(
        &mut self,
        name: &str,
        fields: &[(String, String)],
        part: &Option<String>,
    ) {
        let mut values = Vec::new();
        for (f, text) in fields {
            match parse_wire(text, &self.core.people) {
                Ok(v) => values.push((f.clone(), v)),
                Err(e) => return self.say(format!("event {name}: {e}")),
            }
        }
        let to = match part {
            None => Some(0),
            Some(p) => self
                .core
                .insts
                .iter()
                .position(|x| x.name == *p)
                .and_then(|k| u16::try_from(k).ok()),
        };
        let Some(to) = to else {
            return self.say(format!(
                "event {name}: no part {}",
                part.as_deref().unwrap_or("")
            ));
        };
        let value = WireValue::Struct {
            name: name.into(),
            fields: values,
        };
        self.deliver(to, name, &value, false);
    }

    /// The parts anchored on `entity` (`part … on role`, the role bound to it).
    pub(super) fn parts_on(&self, entity: i64) -> Vec<u16> {
        let Some(q) = self.core.program.quest.as_ref() else {
            return Vec::new();
        };
        q.parts
            .iter()
            .enumerate()
            .filter(|(_, p)| p.anchor == Anchor::On)
            .filter(|(_, p)| {
                q.roles.get(usize::from(p.role)).is_some_and(|r| {
                    self.core
                        .roles
                        .iter()
                        .any(|(n, v)| *n == r.name && id_of(Some(v)) == Some(entity))
                })
            })
            .filter_map(|(k, _)| u16::try_from(k + 1).ok())
            .collect()
    }

    pub(super) fn near(&mut self, player: i64, entity: i64) {
        let Some(p) = self.core.people.handle(player) else {
            return self.say(format!("near: no entity {player}"));
        };
        let value = unit_struct("PlayerNear", vec![("player", p)]);
        for i in self.parts_on(entity) {
            self.deliver(i, "PlayerNear", &value, true);
        }
    }

    pub(super) fn kill(&mut self, victim: i64, by: Option<i64>) {
        // A victim must be someone alive: a second kill raises nothing.
        match self.core.people.entities.get_mut(&victim) {
            Some(e) if e.alive => e.alive = false,
            Some(_) => return self.say(format!("kill: {victim} is already dead")),
            None => return self.say(format!("kill: no entity {victim}")),
        }
        let by = match by.map(|b| self.core.people.handle(b)) {
            Some(Some(p)) => WireValue::Variant {
                name: "Some".into(),
                fields: vec![("value".into(), p)],
            },
            Some(None) => return self.say("kill: an unknown killer".into()),
            None => bare("None"),
        };
        let value = unit_struct(
            "Killed",
            vec![
                (
                    "victim",
                    unit_struct("Entity", vec![("id", WireValue::Int(victim))]),
                ),
                ("by", by),
            ],
        );
        for i in self.owners_of("Killed") {
            self.deliver(i, "Killed", &value, true);
        }
        let ended: Vec<SceneOut> = self
            .scenes
            .iter_mut()
            .filter(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(victim)))
            .map(ScenePlay::actor_lost)
            .collect();
        for out in ended {
            self.scene_out(out);
        }
        self.lose(victim);
    }

    /// `RoleLost` to the home for each role bound to `entity`.
    pub(super) fn lose(&mut self, entity: i64) {
        let program = Arc::clone(&self.core.program);
        let Some(q) = program.quest.as_ref() else {
            return;
        };
        let Some(ev) = program
            .events
            .iter()
            .find(|e| e.name == format!("{}::RoleLost", q.name))
        else {
            return;
        };
        let field = match program.types.desc(ev.ty) {
            Ok(TypeDesc::Struct { fields, .. }) => fields.first().map(|f| f.0.clone()),
            _ => None,
        }
        .unwrap_or_else(|| "role".into());
        for r in &q.roles {
            let bound = self
                .core
                .roles
                .iter()
                .any(|(n, v)| *n == r.name && id_of(Some(v)) == Some(entity));
            if bound {
                let value = WireValue::Struct {
                    name: ev.name.clone(),
                    fields: vec![(field.clone(), bare(&r.name))],
                };
                self.deliver(0, &ev.name, &value, false);
            }
        }
    }

    pub(super) fn edit_input(&mut self, dir: &str) {
        let Some(mut f) = self.edit.take() else {
            return self.say(format!("edit {dir}: no compiler"));
        };
        let result = f(dir);
        self.edit = Some(f);
        match result {
            Ok((program, lines)) => {
                if let Err(e) = self.edit(program, lines) {
                    self.say(format!("edit {dir}: {e}"));
                }
            }
            Err(e) => self.say(format!("edit {dir}: {e}")),
        }
    }

    /// The script is edited (spec §6.9): every instance migrates to `program` (all or none: one refused
    /// leaves them all as they were), then every conversation and scene finds its place again.
    pub fn edit(
        &mut self,
        program: Arc<Program>,
        lines: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut vms = Vec::new();
        for (k, inst) in self.core.insts.iter().enumerate() {
            let id = u16::try_from(k).map_err(|_| "too many instances".to_string())?;
            let cfg = vm_config(self.core.mem_bytes, id, self.core.roles.clone());
            // `migrate` consumes the instance it migrates: a copy is migrated, so a later refusal
            // leaves every instance as it was.
            let copy = Vm::load(
                Arc::clone(&self.core.program),
                cfg.clone(),
                &inst.runner.vm.save(),
            )
            .map_err(|e| format!("the {} cannot be copied for the edit: {e:?}", inst.name))?;
            match copy.migrate(Arc::clone(&program), cfg) {
                Ok((vm, _)) => vms.push(vm),
                Err((_, e)) => return Err(format!("the {} refused the edit: {e:?}", inst.name)),
            }
        }
        for (inst, vm) in self.core.insts.iter_mut().zip(vms) {
            inst.runner.vm = vm;
        }
        self.core.program_key = program.content_hash();
        self.core.program = program;
        self.lines = lines;
        self.say("edited".into());
        // The mirrors of the old program name its globals: take them again before anything evaluates.
        self.core.mirrors.clear();
        self.core.take_mirrors();
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            if let Some(b) = tk.conv.rebind(&mut self.core) {
                self.print_batch(npc, b);
            }
        }
        let outs: Vec<SceneOut> = self
            .scenes
            .iter_mut()
            .filter_map(|s| s.rebind(&mut self.core))
            .collect();
        for out in outs {
            self.scene_out(out);
        }
        Ok(())
    }
}
