//! [`StubHost`]: the `Host` the standalone runner talks to.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use quest_vm::consts::PROMISE_TIMEOUT_TICKS;
use quest_vm::host::{Host, PromiseId, QueryError, Report};
use quest_vm::wire::WireValue;
use state_hash::StateHash;

use crate::api::*;
use crate::namespace::{Answer, Namespace, error};
use crate::people::{People, QueryCtx, field, id_of, int_of, str_of};

/// What goes wrong for a request a [`FailureRule`] picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Never answered: the VM's own deadline produces `Timeout`.
    Timeout,
    Lost,
    Changed,
    /// This and every later `world.*` call.
    NamespaceGone,
}

impl FailureKind {
    /// The error the request fails with.
    fn error_name(self) -> &'static str {
        match self {
            FailureKind::Timeout => "Timeout",
            FailureKind::Lost => "Lost",
            FailureKind::Changed => "Changed",
            FailureKind::NamespaceGone => "NamespaceGone",
        }
    }
}

/// The `nth` (1-based) request of `op` (`"world.get"`, ...; `None` = any op) fails with `kind`.
///
/// A rule injects only an error the request's op can produce: one its host fn declares, or a base promise
/// error (`Timeout`, `Lost`). For any other op the rule is skipped, and the request does not count toward
/// `nth` (`Changed` with `op: None` picks the `nth` `world.compare_and_set`, never a `chest.take`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureRule {
    pub op: Option<String>,
    pub nth: u32,
    pub kind: FailureKind,
}

#[derive(Clone, Debug, Default)]
pub struct StubConfig {
    /// Ticks between a request's commit and its answer (0 = the next tick).
    pub latency: u64,
    pub failures: Vec<FailureRule>,
    pub chest: Vec<(String, i64)>,
    /// Block positions by entity id (added to the host's [`People`]): `Person.distance_to` of anyone
    /// without one is `OutOfReach`.
    pub people: Vec<(i64, [i64; 3])>,
    /// The places (by id) with no room: `place(…, at)` of one fails its check with `NoRoom`.
    pub full_places: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Pending {
    due: u64,
    f: u16,
    args: Vec<WireValue>,
    fate: Fate,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fate {
    Normal,
    Fail(FailureKind),
    /// Never answered; forgotten once the VM's deadline has passed (`due` is that deadline).
    Never,
}

#[derive(Clone, Debug)]
pub struct StubHost {
    pub output: Vec<String>,
    pub reports: Vec<Report>,
    /// The tick being run: a request committed now is due at `tick + latency`. The runner sets it.
    pub tick: u64,
    /// Events emitted by scripts, delivered by the runner at the start of its next step.
    pub emitted: Vec<WireValue>,
    /// The players (by id) who completed the quest: `quest.completed_by` answers from it.
    pub completed_by: BTreeSet<i64>,
    /// Events routed to another instance (`Host::route`): the [`crate::World`] delivers them.
    pub routed: Vec<(String, WireValue)>,
    /// The world the queries read and the commands change (tags, inventories, …). A [`crate::World`] lends
    /// its own to the host of the instance it runs.
    pub people: People,
    /// Set by a [`crate::World`]: the commands it acts on (the `quest.*` builtins, dialogues, scenes,
    /// `follow`) are recorded in [`StubHost::world_cmds`] instead of printed here.
    pub(crate) in_world: bool,
    pub(crate) world_cmds: Vec<(u16, Vec<WireValue>)>,
    /// Set by a [`crate::World`] while the home's initialisers run at placement: every command is held in
    /// [`StubHost::held`], acted on only once the placement is accepted.
    pub(crate) holding: bool,
    pub(crate) held: Vec<(u16, Vec<WireValue>)>,
    pub(crate) namespace: Namespace,
    latency: u64,
    rules: Vec<(FailureRule, u32)>,
    namespace_gone: bool,
    chest: BTreeMap<String, i64>,
    full_places: BTreeSet<i64>,
    pending: BTreeMap<u64, Pending>,
}

impl StubHost {
    pub fn new(cfg: StubConfig) -> StubHost {
        let mut chest = BTreeMap::new();
        for (kind, n) in cfg.chest {
            *chest.entry(kind).or_insert(0i64) += n;
        }
        let people = People {
            pos: cfg.people.into_iter().collect(),
            ..People::default()
        };
        StubHost {
            output: Vec::new(),
            reports: Vec::new(),
            tick: 0,
            emitted: Vec::new(),
            completed_by: BTreeSet::new(),
            routed: Vec::new(),
            people,
            in_world: false,
            world_cmds: Vec::new(),
            holding: false,
            held: Vec::new(),
            namespace: Namespace::default(),
            latency: cfg.latency,
            rules: cfg.failures.into_iter().map(|r| (r, 0)).collect(),
            namespace_gone: false,
            chest,
            full_places: cfg.full_places.into_iter().collect(),
            pending: BTreeMap::new(),
        }
    }

    /// Hashes the host's own state: the namespace, the chest, the requests in flight and the failure
    /// rules' counts (not the output, the reports or the people, which a [`crate::World`] hashes itself).
    pub(crate) fn hash_state<H: Hasher>(&self, h: &mut H) {
        self.namespace.hash_state(h);
        self.chest.state_hash(h);
        self.namespace_gone.state_hash(h);
        for (_, seen) in &self.rules {
            seen.state_hash(h);
        }
        (self.pending.len() as u64).state_hash(h);
        for (id, p) in &self.pending {
            id.state_hash(h);
            p.due.state_hash(h);
            p.f.state_hash(h);
            p.args.state_hash(h);
            let fate: u8 = match p.fate {
                Fate::Normal => 0,
                Fate::Fail(FailureKind::Timeout) => 1,
                Fate::Fail(FailureKind::Lost) => 2,
                Fate::Fail(FailureKind::Changed) => 3,
                Fate::Fail(FailureKind::NamespaceGone) => 4,
                Fate::Never => 5,
            };
            fate.state_hash(h);
        }
        self.tick.state_hash(h);
        self.emitted.state_hash(h);
    }

    /// Whether a request is still waiting for its answer (a never-answered one counts until the VM's own
    /// deadline has passed).
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The answers due at `now` (every request with `due <= now`), in promise-id order. Each is computed
    /// and applied to the stub's state here, not when it was requested.
    pub fn due(&mut self, now: u64) -> Vec<(PromiseId, Answer)> {
        let ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.due <= now && !matches!(p.fate, Fate::Never))
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::new();
        for id in ids {
            if let Some(p) = self.pending.remove(&id) {
                let answer = self.answer(&p);
                out.push((PromiseId(id), answer));
            }
        }
        out
    }

    /// Forgets the never-answered requests whose VM deadline `now` has reached (call after the tick).
    pub fn expire(&mut self, now: u64) {
        self.pending
            .retain(|_, p| !(matches!(p.fate, Fate::Never) && p.due <= now));
    }

    fn answer(&mut self, p: &Pending) -> Answer {
        let key = str_of(p.args.first());
        match p.fate {
            Fate::Normal => self.apply(p.f, &p.args),
            Fate::Fail(FailureKind::Lost) => Err(error("Lost", vec![])),
            Fate::Fail(FailureKind::NamespaceGone) => Err(error("NamespaceGone", vec![])),
            Fate::Fail(FailureKind::Changed) => {
                let current = key
                    .and_then(|k| self.namespace.current(k))
                    .unwrap_or_default();
                Err(error("Changed", vec![("current", current)]))
            }
            // Never answered; `due` skips it.
            Fate::Fail(FailureKind::Timeout) | Fate::Never => Err(error("Lost", vec![])),
        }
    }

    fn apply(&mut self, f: u16, args: &[WireValue]) -> Answer {
        let lost = || error("Lost", vec![]);
        let key = str_of(args.first());
        match (f, key) {
            (F_CHEST_TAKE, Some(kind)) => {
                let n = int_of(args.get(1)).ok_or_else(lost)?.max(0);
                let have = self.chest.get(kind).copied().unwrap_or(0);
                if have <= 0 {
                    return Err(error("ChestEmpty", vec![]));
                }
                if have < n {
                    return Err(error("NotEnough", vec![("have", WireValue::Int(have))]));
                }
                self.chest.insert(kind.to_string(), have - n);
                Ok(item(kind, n))
            }
            (F_WORLD_GET, Some(k)) => self.namespace.get(k),
            (F_WORLD_SET, Some(k)) => {
                let v = args.get(1).cloned().ok_or_else(lost)?;
                self.namespace.set(k, v)
            }
            (F_WORLD_ADD, Some(k)) => {
                let n = int_of(args.get(1)).ok_or_else(lost)?;
                self.namespace.add(k, n)
            }
            (F_WORLD_CAS, Some(k)) => {
                let (expected, new) =
                    (args.get(1).ok_or_else(lost)?, args.get(2).ok_or_else(lost)?);
                self.namespace.compare_and_set(k, expected, new.clone())
            }
            (F_WORLD_DELETE, Some(k)) => self.namespace.delete(k),
            _ => Err(lost()),
        }
    }

    /// Counts the request against every rule and picks its fate: the first rule that fires wins.
    fn fate_of(&mut self, f: u16) -> Fate {
        let op = op_name(f).unwrap_or_default();
        let is_world = op.starts_with("world.");
        let mut fate = None;
        for (rule, seen) in &mut self.rules {
            if rule.op.as_ref().is_some_and(|o| *o != op)
                || !op_can_raise(f, rule.kind.error_name())
            {
                continue;
            }
            *seen += 1;
            if *seen == rule.nth && fate.is_none() {
                fate = Some(rule.kind);
            }
        }
        if fate == Some(FailureKind::NamespaceGone) && is_world {
            self.namespace_gone = true;
        }
        match fate {
            Some(FailureKind::Timeout) => Fate::Never,
            _ if self.namespace_gone && is_world => Fate::Fail(FailureKind::NamespaceGone),
            Some(kind) => Fate::Fail(kind),
            None => Fate::Normal,
        }
    }
}

fn item(kind: &str, count: i64) -> WireValue {
    WireValue::Struct {
        name: "Item".into(),
        fields: vec![
            ("kind".into(), WireValue::Str(kind.into())),
            ("count".into(), WireValue::Int(count)),
        ],
    }
}

impl Host for StubHost {
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError> {
        let ctx = QueryCtx {
            tick: self.tick,
            depth: 0,
            completed_by: &self.completed_by,
        };
        if let Some(answer) = self.people.query(f, args, &ctx) {
            return answer;
        }
        match f {
            F_CHEST_COUNT => {
                let kind = str_of(args.first())
                    .ok_or_else(|| QueryError::Failed(format!("bad arguments to host fn {f}")))?;
                Ok(WireValue::Int(self.chest.get(kind).copied().unwrap_or(0)))
            }
            _ => Err(QueryError::Failed(format!(
                "the stub answers no query {}",
                op_name(f).unwrap_or_else(|| format!("(host fn {f})"))
            ))),
        }
    }

    fn check(&mut self, f: u16, args: &[WireValue]) -> Result<(), QueryError> {
        match f {
            F_PLACE => {
                let at = id_of(args.get(1))
                    .ok_or_else(|| QueryError::Failed(format!("bad arguments to host fn {f}")))?;
                if self.full_places.contains(&at) || self.people.no_room(at) {
                    return Err(QueryError::Raised(error("NoRoom", vec![])));
                }
                Ok(())
            }
            _ => Err(QueryError::Failed(format!(
                "host fn {f} is not a fallible command"
            ))),
        }
    }

    fn command(&mut self, f: u16, args: &[WireValue]) {
        fn name_of(v: Option<&WireValue>) -> Option<&str> {
            str_of(v.and_then(|p| field(p, "name")))
        }
        fn item_of(v: Option<&WireValue>) -> Option<(&str, i64)> {
            str_of(v.and_then(|i| field(i, "kind"))).zip(int_of(v.and_then(|i| field(i, "count"))))
        }
        if self.holding {
            self.held.push((f, args.to_vec()));
            return;
        }
        if self.in_world
            && matches!(
                f,
                F_FOLLOW
                    | F_ATTACH_DIALOGUE
                    | F_DETACH_DIALOGUE
                    | F_BARK
                    | F_QUEST_JOINED
                    | F_QUEST_LEFT
                    | F_QUEST_JOIN_REFUSED
                    | F_QUEST_COMPLETE
                    | F_QUEST_FAIL
                    | F_QUEST_DECLINE
                    | F_QUEST_PLAY
            )
        {
            self.world_cmds.push((f, args.to_vec()));
            return;
        }
        let line = match f {
            F_SAY => name_of(args.first())
                .zip(str_of(args.get(1)))
                .map(|(n, t)| format!("{n}: {t}")),
            F_LOG => str_of(args.first()).map(|t| format!("log: {t}")),
            F_GIVE | F_TAKE => {
                let who = id_of(args.first());
                let what = if f == F_GIVE {
                    item_of(args.get(1))
                } else {
                    str_of(args.get(1)).zip(int_of(args.get(2)))
                };
                match (name_of(args.first()), who, what) {
                    (Some(n), Some(id), Some((kind, count))) => {
                        let (verb, sign) = if f == F_GIVE {
                            ("give", 1)
                        } else {
                            ("take", -1)
                        };
                        let line = format!("{verb} {n} {count} {kind}");
                        self.people.add_items(id, kind, count.saturating_mul(sign));
                        Some(line)
                    }
                    _ => None,
                }
            }
            F_ADD_TAG | F_REMOVE_TAG | F_TAG => {
                if let Some((id, tag)) = id_of(args.first()).zip(str_of(args.get(1))) {
                    self.people.set_tag(id, tag, f != F_REMOVE_TAG);
                    return;
                }
                None
            }
            F_CHEST_PUT => {
                if let Some((kind, n)) = item_of(args.first()) {
                    let slot = self.chest.entry(kind.to_string()).or_insert(0);
                    *slot = slot.saturating_add(n);
                    return;
                }
                None
            }
            F_PLACE => {
                let name = name_of(args.first());
                let at = id_of(args.get(1));
                name.zip(at).map(|(n, at)| format!("place {n} at {at}"))
            }
            F_EMIT => {
                if let Some(e @ WireValue::Struct { .. }) = args.first() {
                    self.emitted.push(e.clone());
                    return;
                }
                None
            }
            F_FOLLOW => id_of(args.first())
                .zip(name_of(args.get(1)))
                .map(|(c, p)| format!("follow {c} {p}")),
            F_ATTACH_DIALOGUE | F_DETACH_DIALOGUE | F_BARK => {
                let verb = match f {
                    F_ATTACH_DIALOGUE => "attach_dialogue",
                    F_DETACH_DIALOGUE => "detach_dialogue",
                    _ => "bark",
                };
                id_of(args.first())
                    .zip(str_of(args.get(1)))
                    .map(|(npc, d)| format!("{verb} {npc} {d}"))
            }
            // Outside a world, the host side of the quest builtins is printed only.
            F_QUEST_JOINED | F_QUEST_LEFT | F_QUEST_JOIN_REFUSED => {
                let what = match f {
                    F_QUEST_JOINED => "joined",
                    F_QUEST_LEFT => "left",
                    _ => "join_refused",
                };
                name_of(args.first()).map(|n| format!("quest.{what} {n}"))
            }
            F_QUEST_COMPLETE => match args.first() {
                Some(WireValue::List(ps)) => ps
                    .iter()
                    .map(|p| str_of(field(p, "name")))
                    .collect::<Option<Vec<&str>>>()
                    .map(|names| format!("quest.complete {}", names.join(", "))),
                _ => None,
            },
            F_QUEST_FAIL => Some("quest.fail".to_string()),
            F_QUEST_DECLINE => Some("quest.decline".to_string()),
            F_QUEST_PLAY => match (str_of(args.first()), args.get(1)) {
                (Some(scene), Some(WireValue::List(cast))) => cast
                    .iter()
                    .map(|c| int_of(field(c, "id")).map(|i| i.to_string()))
                    .collect::<Option<Vec<String>>>()
                    .map(|ids| format!("quest.play {scene} [{}]", ids.join(", "))),
                _ => None,
            },
            _ => None,
        };
        // A well-formed call returned above or wrote `Some`; anything else is a malformed call.
        self.output
            .push(line.unwrap_or_else(|| format!("stub: bad command {f}")));
    }

    fn request(&mut self, id: PromiseId, f: u16, args: &[WireValue]) {
        let fate = self.fate_of(f);
        let due = match fate {
            Fate::Never => self.tick.saturating_add(PROMISE_TIMEOUT_TICKS),
            _ => self.tick.saturating_add(self.latency),
        };
        self.pending.insert(
            id.0,
            Pending {
                due,
                f,
                args: args.to_vec(),
                fate,
            },
        );
    }

    fn print(&mut self, text: &str) {
        self.output.push(text.to_string());
    }

    fn report(&mut self, report: Report) {
        self.reports.push(report);
    }

    fn route(&mut self, event: &str, value: &WireValue) {
        self.routed.push((event.to_string(), value.clone()));
    }
}
