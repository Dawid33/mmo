//! [`StubHost`]: the `Host` the standalone runner talks to.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hasher;

use quest_vm::Fnv64;
use quest_vm::consts::PROMISE_TIMEOUT_TICKS;
use quest_vm::host::{Host, PromiseId, Report};
use quest_vm::wire::WireValue;

use crate::api::*;
use crate::namespace::{Answer, Namespace, error};

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

/// The `nth` (1-based) request of `op` (`"world.get"`, ...; `None` = any op) fails with `kind`.
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
}

#[derive(Clone, Debug)]
struct Pending {
    due: u64,
    f: u16,
    args: Vec<WireValue>,
    fate: Fate,
}

#[derive(Clone, Copy, Debug)]
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
    latency: u64,
    rules: Vec<(FailureRule, u32)>,
    namespace_gone: bool,
    tags: BTreeMap<i64, BTreeSet<String>>,
    chest: BTreeMap<String, i64>,
    namespace: Namespace,
    pending: BTreeMap<u64, Pending>,
}

fn field<'a>(v: &'a WireValue, name: &str) -> Option<&'a WireValue> {
    match v {
        WireValue::Struct { fields, .. } | WireValue::Variant { fields, .. } => {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
        }
        _ => None,
    }
}

fn str_of(v: Option<&WireValue>) -> Option<&str> {
    match v {
        Some(WireValue::Str(s)) => Some(s),
        _ => None,
    }
}

fn int_of(v: Option<&WireValue>) -> Option<i64> {
    match v {
        Some(WireValue::Int(i)) => Some(*i),
        _ => None,
    }
}

fn player_id(v: Option<&WireValue>) -> Option<i64> {
    int_of(v.and_then(|p| field(p, "id")))
}

impl StubHost {
    pub fn new(cfg: StubConfig) -> StubHost {
        let mut chest = BTreeMap::new();
        for (kind, n) in cfg.chest {
            *chest.entry(kind).or_insert(0i64) += n;
        }
        StubHost {
            output: Vec::new(),
            reports: Vec::new(),
            tick: 0,
            emitted: Vec::new(),
            latency: cfg.latency,
            rules: cfg.failures.into_iter().map(|r| (r, 0)).collect(),
            namespace_gone: false,
            tags: BTreeMap::new(),
            chest,
            namespace: Namespace::default(),
            pending: BTreeMap::new(),
        }
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
            if rule.op.as_ref().is_some_and(|o| *o != op) {
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
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, String> {
        let bad = || format!("bad arguments to host fn {f}");
        match f {
            F_HAS_TAG => {
                let id = player_id(args.first()).ok_or_else(bad)?;
                let tag = str_of(args.get(1)).ok_or_else(bad)?;
                Ok(WireValue::Bool(
                    self.tags.get(&id).is_some_and(|t| t.contains(tag)),
                ))
            }
            F_ROLL => {
                let (salt, sides) = (int_of(args.first()), int_of(args.get(1)));
                let (salt, sides) = salt.zip(sides).ok_or_else(bad)?;
                if sides <= 0 {
                    return Ok(WireValue::Int(0));
                }
                let mut h = Fnv64::default();
                h.write(&salt.to_le_bytes());
                // `sides > 0`, so the remainder is below `i64::MAX` and fits.
                Ok(WireValue::Int((h.finish() % sides as u64) as i64))
            }
            F_CHEST_COUNT => {
                let kind = str_of(args.first()).ok_or_else(bad)?;
                Ok(WireValue::Int(self.chest.get(kind).copied().unwrap_or(0)))
            }
            _ => Err(format!("host fn {f} is not a query")),
        }
    }

    fn command(&mut self, f: u16, args: &[WireValue]) {
        let line = match f {
            F_SAY => {
                let name = args.first().and_then(|p| field(p, "name"));
                str_of(name)
                    .zip(str_of(args.get(1)))
                    .map(|(n, t)| format!("{n}: {t}"))
            }
            F_LOG => str_of(args.first()).map(|t| format!("log: {t}")),
            F_GIVE => {
                let name = str_of(args.first().and_then(|p| field(p, "name")));
                let it = args.get(1);
                let kind = str_of(it.and_then(|i| field(i, "kind")));
                let count = int_of(it.and_then(|i| field(i, "count")));
                name.zip(kind)
                    .zip(count)
                    .map(|((n, k), c)| format!("give {n} {c} {k}"))
            }
            F_ADD_TAG | F_REMOVE_TAG => {
                if let Some((id, tag)) = player_id(args.first()).zip(str_of(args.get(1))) {
                    let tags = self.tags.entry(id).or_default();
                    if f == F_ADD_TAG {
                        tags.insert(tag.to_string());
                    } else {
                        tags.remove(tag);
                    }
                    return;
                }
                None
            }
            F_CHEST_PUT => {
                let it = args.first();
                if let Some((kind, n)) = str_of(it.and_then(|i| field(i, "kind")))
                    .zip(int_of(it.and_then(|i| field(i, "count"))))
                {
                    let slot = self.chest.entry(kind.to_string()).or_insert(0);
                    *slot = slot.saturating_add(n);
                    return;
                }
                None
            }
            F_EMIT => {
                if let Some(e @ WireValue::Struct { .. }) = args.first() {
                    self.emitted.push(e.clone());
                    return;
                }
                None
            }
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
}
