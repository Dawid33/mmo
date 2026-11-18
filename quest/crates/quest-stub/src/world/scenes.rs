//! Scenes and barks: `play`, `bark`, the ambient schedule, their lines and ends.

use super::*;

pub(super) fn scene_reason_name(r: SceneReason) -> &'static str {
    match r {
        SceneReason::Finished => "Finished",
        SceneReason::Interrupted => "Interrupted",
        SceneReason::ActorLost => "ActorLost",
    }
}

impl World {
    pub(super) fn start_scene(&mut self, i: u16, scene: &str, cast: Vec<WireValue>) {
        let found = self
            .core
            .env
            .program()
            .dialogues
            .dialogues
            .iter()
            .position(|d| d.name == scene && d.kind == DialogueKind::Scene)
            .and_then(|k| u16::try_from(k).ok());
        let Some(k) = found else {
            return self.say(format!("play: no scene {scene}"));
        };
        let now = self.core.env.now;
        let play = ScenePlay::start(&mut self.core.env, InstanceId(u32::from(i)), k, cast, now);
        self.scenes.push(play);
    }

    /// Who is in a conversation or a scene.
    pub(super) fn busy(&self, id: i64) -> bool {
        self.talks.contains_key(&id)
            || self
                .scenes
                .iter()
                .any(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(id)))
    }

    pub(super) fn ambient(&mut self, npcs: &[i64]) {
        if self.outcome != Outcome::Running {
            return;
        }
        let idle: Vec<WireValue> = npcs
            .iter()
            .filter(|id| !self.busy(**id))
            .filter_map(|id| self.core.env.people.handle(*id))
            .collect();
        let stage = self.core.env.stage();
        let candidates: Vec<Attachment> = self
            .core
            .env
            .program()
            .dialogues
            .dialogues
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                d.kind == DialogueKind::Scene && d.stage.is_none_or(|s| Some(s) == stage)
            })
            .filter_map(|(k, _)| {
                Some(Attachment {
                    instance: InstanceId(0),
                    dialogue: u16::try_from(k).ok()?,
                })
            })
            .collect();
        let now = self.core.env.now;
        let Some((att, cast)) =
            cast_ambient(&mut self.core.env, &candidates, &idle, &self.played, now)
        else {
            return;
        };
        let name = self
            .core
            .env
            .program()
            .dialogues
            .dialogues
            .get(usize::from(att.dialogue))
            .map(|d| d.name.clone())
            .unwrap_or_default();
        self.played.insert((Some(self.core.env.key()), name), now);
        let play = ScenePlay::start(&mut self.core.env, att.instance, att.dialogue, cast, now);
        self.scenes.push(play);
    }

    pub(super) fn tick_scenes(&mut self) {
        let now = self.core.env.now;
        let mut outs = Vec::new();
        for s in &mut self.scenes {
            if s.ended() {
                continue;
            }
            let lead = s.cast().first().and_then(|c| id_of(Some(c))).unwrap_or(0);
            let rec = self.records.entry(lead).or_default();
            outs.push(s.tick(&mut self.core.env, rec, now));
        }
        for out in outs {
            self.scene_out(out);
        }
        self.scenes.retain(|s| !s.ended());
    }

    /// Prints a scene's lines and sends what it raised.
    pub(super) fn scene_out(&mut self, out: SceneOut) {
        for line in &out.lines {
            let said = format!("{}: {}", self.speaker(&line.speaker), self.text(line));
            self.say(said);
        }
        for e in out.emits {
            self.send(e.instance.0, e.event, e.value);
        }
        if let Some(end) = out.ended {
            let value = unit_struct(
                "SceneEnded",
                vec![
                    ("scene", WireValue::Str(end.name.clone())),
                    ("reason", bare(scene_reason_name(end.reason))),
                ],
            );
            self.pending
                .push((end.instance.0, "SceneEnded".into(), value));
        }
    }
}
