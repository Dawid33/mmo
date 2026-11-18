//! Talking: the attached set, conversations, picks, and how lines and offers print.

use super::*;

/// A player's conversation with an NPC.
pub(super) struct Talk {
    pub(super) player: i64,
    pub(super) conv: Conversation,
}

/// A value as a line shows it.
pub(super) fn show(v: &WireValue) -> String {
    match v {
        WireValue::Unit => "()".into(),
        WireValue::Bool(b) => b.to_string(),
        WireValue::Int(i) => i.to_string(),
        WireValue::Float(f) => f.to_string(),
        WireValue::Str(s) => s.clone(),
        WireValue::List(items) => {
            let items: Vec<String> = items.iter().map(show).collect();
            format!("[{}]", items.join(", "))
        }
        WireValue::Struct { name, fields } => match (field(v, "name"), field(v, "id")) {
            (Some(WireValue::Str(n)), _) => n.clone(),
            (_, Some(WireValue::Int(id))) => format!("{name}({id})"),
            _ => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|(n, v)| format!("{n}={}", show(v)))
                    .collect();
                format!("{name}({})", fs.join(", "))
            }
        },
        WireValue::Variant { name, fields } => match fields.as_slice() {
            [] => name.clone(),
            fs => {
                let fs: Vec<String> = fs.iter().map(|(_, v)| show(v)).collect();
                format!("{name}({})", fs.join(", "))
            }
        },
        other => format!("{other:?}"),
    }
}

/// `template`'s `{n}` holes filled from `args` (`{{` and `}}` are braces).
pub(super) fn fill(template: &str, args: &[WireValue]) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut n = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    n.push(d);
                }
                if let Some(v) = n.parse::<usize>().ok().and_then(|k| args.get(k)) {
                    out.push_str(&show(v));
                }
            }
            c => out.push(c),
        }
    }
    out
}

pub(super) fn end_name(r: EndReason) -> &'static str {
    match r {
        EndReason::Finished => "Finished",
        EndReason::Left => "Left",
        EndReason::Timeout => "Timeout",
        EndReason::TooDeep => "TooDeep",
        EndReason::Edited => "Edited",
        EndReason::OutOfRange => "OutOfRange",
    }
}

/// A conversation with nothing more to happen without input: closed, or waiting for a pick with no timeout.
pub(super) fn conv_quiet(c: &Conversation) -> bool {
    match c.state() {
        ConvState::Closed => true,
        ConvState::AwaitingPick => c.offer().is_none_or(|o| o.timeout_at.is_none()),
        _ => false,
    }
}

/// Over: ended or closed.
pub(super) fn conv_over(c: &Conversation) -> bool {
    matches!(c.state(), ConvState::Ended { .. } | ConvState::Closed)
}

impl World {
    /// What is attached to `npc` for `player` now (module docs): nothing once the quest is over.
    pub(super) fn attachments(&mut self, player: &WireValue, npc: &WireValue) -> Vec<Attachment> {
        if self.outcome != Outcome::Running {
            return Vec::new();
        }
        self.core
            .env
            .attachments(player, npc, &self.attached, &self.detached)
    }

    /// The NPC `player` is talking to (a conversation not over).
    pub(super) fn talking_to(&self, player: i64) -> Option<i64> {
        self.talks
            .iter()
            .find(|(_, tk)| tk.player == player && !conv_over(&tk.conv))
            .map(|(npc, _)| *npc)
    }

    pub(super) fn talk_input(&mut self, input: &Input) {
        match *input {
            Input::Talk { player, npc } => self.talk(player, npc),
            Input::Pick { player, option } => self.pick(player, option),
            Input::Leave { player } => {
                if let Some(npc) = self.talking_to(player) {
                    let now = self.core.env.now;
                    if let Some(tk) = self.talks.get_mut(&npc) {
                        let b = tk.conv.leave(now);
                        self.print_batch(npc, b);
                    }
                }
            }
            Input::Done { player } => {
                for tk in self.talks.values_mut().filter(|tk| tk.player == player) {
                    tk.conv.done();
                }
                self.talks
                    .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
            }
            Input::Ambient { ref npcs } => self.ambient(npcs),
            _ => {}
        }
    }

    pub(super) fn talk(&mut self, player: i64, npc: i64) {
        let (Some(p), Some(n)) = (
            self.core.env.people.handle(player),
            self.core.env.people.handle(npc),
        ) else {
            return self.say(format!("talk: no entity {player} or {npc}"));
        };
        // A cast member: an interruptible scene ends; an uninterruptible one keeps the NPC busy.
        if let Some(k) = self
            .scenes
            .iter()
            .position(|s| !s.ended() && s.cast().iter().any(|c| id_of(Some(c)) == Some(npc)))
        {
            match self.scenes[k].interrupt() {
                Some(out) => self.scene_out(out),
                None => return,
            }
        }
        if let Some(tk) = self.talks.get(&npc)
            && *tk.conv.state() != ConvState::Closed
            && (tk.player != player || !conv_over(&tk.conv))
        {
            return;
        }
        if self.talking_to(player).is_some() {
            return;
        }
        let attached = self.attachments(&p, &n);
        let talked = unit_struct("Talked", vec![("player", p.clone()), ("npc", n.clone())]);
        for i in self.owners_of("Talked") {
            self.pending
                .push((u32::from(i), "Talked".into(), talked.clone()));
        }
        let now = self.core.env.now;
        let rec = self.records.entry(npc).or_default();
        let (conv, batch) = Conversation::start(&mut self.core.env, &attached, rec, p, n, now);
        self.talks.insert(npc, Talk { player, conv });
        self.print_batch(npc, batch);
        self.talks
            .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
    }

    pub(super) fn pick(&mut self, player: i64, option: u32) {
        let Some(npc) = self
            .talks
            .iter()
            .find(|(_, tk)| tk.player == player && tk.conv.offer().is_some())
            .map(|(npc, _)| *npc)
        else {
            return;
        };
        let (Some(p), Some(n)) = (
            self.core.env.people.handle(player),
            self.core.env.people.handle(npc),
        ) else {
            return;
        };
        let attached = self.attachments(&p, &n);
        let now = self.core.env.now;
        let Some(tk) = self.talks.get_mut(&npc) else {
            return;
        };
        let Some(serial) = tk.conv.offer().map(|o| o.serial) else {
            return;
        };
        let rec = self.records.entry(npc).or_default();
        let result = tk.conv.pick(
            &mut self.core.env,
            &attached,
            rec,
            serial,
            option.saturating_sub(1),
            now,
        );
        match result {
            Ok(b) => self.print_batch(npc, b),
            Err(PickRefused::Stale { resent } | PickRefused::OutOfRange { resent }) => {
                self.print_offer(&resent)
            }
            Err(PickRefused::NoOffer) => {}
        }
    }

    pub(super) fn tick_talks(&mut self) {
        let now = self.core.env.now;
        let npcs: Vec<i64> = self.talks.keys().copied().collect();
        for npc in npcs {
            let Some(tk) = self.talks.get_mut(&npc) else {
                continue;
            };
            let rec = self.records.entry(npc).or_default();
            if let Some(b) = tk.conv.tick(&mut self.core.env, rec, now) {
                self.print_batch(npc, b);
            }
        }
        self.talks
            .retain(|_, tk| *tk.conv.state() != ConvState::Closed);
    }

    pub(super) fn speaker(&self, v: &WireValue) -> String {
        match (str_of(field(v, "name")), id_of(Some(v))) {
            (Some(n), _) => n.to_string(),
            (None, Some(id)) => self.core.env.people.name_of(id),
            _ => show(v),
        }
    }

    pub(super) fn text(&self, line: &SaidLine) -> String {
        match self.lines.get(&line.id) {
            Some(t) if !self.show_ids => fill(t, &line.args),
            _ if line.args.is_empty() => format!("@{}", line.id),
            _ => {
                let args: Vec<String> = line.args.iter().map(show).collect();
                format!("@{}({})", line.id, args.join(", "))
            }
        }
    }

    pub(super) fn print_offer(&mut self, offer: &Offer) {
        let options: Vec<String> = offer
            .options
            .iter()
            .enumerate()
            .map(|(k, o)| format!("{}) {}", k + 1, self.text(o)))
            .collect();
        self.say(format!("menu: {}", options.join(" ")));
    }

    /// Prints a conversation's batch and sends what it raised.
    pub(super) fn print_batch(&mut self, npc: i64, b: Batch) {
        for line in &b.lines {
            let said = format!("{}: {}", self.speaker(&line.speaker), self.text(line));
            self.say(said);
        }
        if let Some(offer) = &b.offer {
            self.print_offer(offer);
        }
        for e in b.emits {
            self.send(e.instance.0, e.event, e.value);
        }
        let player = self.talks.get(&npc).map(|tk| tk.player);
        let (p, n) = (
            player.and_then(|p| self.core.env.people.handle(p)),
            self.core.env.people.handle(npc),
        );
        for e in b.ended {
            let (Some(p), Some(n)) = (p.clone(), n.clone()) else {
                continue;
            };
            let value = unit_struct(
                "DialogueEnded",
                vec![
                    ("player", p),
                    ("npc", n),
                    ("dialogue", WireValue::Str(e.dialogue.clone())),
                    ("reason", bare(end_name(e.reason))),
                    (
                        "exit",
                        WireValue::Str(format!("{}::{}", e.dialogue, e.exit)),
                    ),
                ],
            );
            self.pending
                .push((e.instance.0, "DialogueEnded".into(), value));
        }
        if let Some(end) = b.end {
            let who = self.core.env.people.name_of(npc);
            self.say(format!("end {who}: {}", end_name(end)));
        }
    }
}
