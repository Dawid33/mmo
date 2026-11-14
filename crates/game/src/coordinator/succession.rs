//! Successions (R4): which hosts lost their lease, and the successions the coordinator
//! declares and runs for them. The coordinator declares a host dead only from what it read
//! itself — the same `seen_ms` over [`HOST_LEASE_MS`] of its own clock, every read in
//! between a successful one — so a database outage never looks like every host dying. Pure:
//! the runtime reads the `hosts` rows, executes the declarations and rollbacks in the store
//! and passes its clock in.
//!
//! **The runs** ([`SuccessionRuns`], `CrashPolicy::Succession` only — `Static` never
//! declares, its crash policy is the world rollback):
//! - [`Coordinator::declare_wanted`] hands the runtime ONE batch of every lapsed host, each
//!   with its heir ([`crate::heir_of`] over the attached hosts that did not lapse), only
//!   between cuts (a running cut with a lapsed participant is decided abort first), with no
//!   succession open and none being declared. No live attached host: nothing (a total crash
//!   is the boots' business).
//! - [`Coordinator::declared`] takes the store's answer: `Done` sends each order to its
//!   survivors ([`CoordMsg::Succession`]; one not attached gets it with its next `Attach`)
//!   and takes the predecessor out of the coordinator's view (`host_succeeded`);
//!   `SuccessionOpen` asks for a rollback; `NotLapsed` / `Gone` are forgotten (the next reads
//!   observe again).
//! - While a succession is open and not every survivor answered
//!   [`HostMsg::SuccessionApplied`] and the heir [`HostMsg::SuccessionAdopted`], no world
//!   cut begins. The next cut after that names the open successions in its decision
//!   (`CutDecide::settles`); its commit settles them ([`CoordMsg::SuccessionDone`]).
//! - A lapse while a succession is open, the store answering `SuccessionOpen`, or
//!   [`SUCCESSION_DEADLINE_MS`] without every answer: [`Coordinator::rollback_wanted`] — the
//!   runtime rolls the world back and the core tells every host
//!   ([`Coordinator::rolled_back`] → [`CoordMsg::Generation`]).
//! - A restarted coordinator reads the open successions ([`Coordinator::successions_open`])
//!   and re-sends their orders; hosts answer again from memory (`Attach.successions`).

use std::collections::{BTreeMap, BTreeSet};

use super::{CoordMsg, Target};
use crate::{CrashPolicy, Declared, Succession, SuccessionId, SuccessionKind, HOST_LEASE_MS, HOST_LIVE, SUCCESSION_DEADLINE_MS};

/// One `hosts` row as the lease watch needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSeen {
    pub ordinal: u32,
    pub boot: u64,
    pub clean: bool,
    /// `HOST_LIVE` or `HOST_DEAD`.
    pub state: i64,
    /// The database clock at its last beat; 0 = vouched dead by a boot of its ordinal.
    pub seen_ms: u64,
}

/// What the watch remembers of one ordinal's row.
#[derive(Clone, Debug)]
struct Watched {
    row: HostSeen,
    /// When the first read showing this boot with this `seen_ms` was RECEIVED (the value
    /// was there by then at the latest).
    first_received_ms: u64,
    /// When the latest read still showing it was ISSUED (the value was still there then at
    /// the earliest).
    last_issued_ms: u64,
}

/// The coordinator's lapse watch over the `hosts` rows (fed every
/// `WORLD_CUT_HOSTS_REFRESH_MS` by the runtime).
///
/// A read is stamped twice on the coordinator's clock: when it was issued and when its
/// answer was received. The unchanged span counts from the first read's RECEIVE to the
/// latest read's ISSUE — a lower bound on how long the row really stood still. Stamping by
/// processing time alone would let a coordinator stall (an answer waiting in a channel)
/// stretch two ordinary reads of one beat over a whole lease.
#[derive(Clone, Debug, Default)]
pub struct LeaseWatch {
    rows: BTreeMap<u32, Watched>,
}

impl LeaseWatch {
    /// A successful read of every `hosts` row, issued at `issued_ms` and received at
    /// `received_ms` (the coordinator's clock). A row whose boot or `seen_ms` changed starts
    /// its count again; a row no longer read is forgotten.
    pub fn observe(&mut self, rows: &[HostSeen], issued_ms: u64, received_ms: u64) {
        let mut next = BTreeMap::new();
        for row in rows {
            let first_received_ms = match self.rows.get(&row.ordinal) {
                Some(w) if w.row.boot == row.boot && w.row.seen_ms == row.seen_ms => w.first_received_ms,
                _ => received_ms,
            };
            next.insert(row.ordinal, Watched { row: row.clone(), first_received_ms, last_issued_ms: issued_ms });
        }
        self.rows = next;
    }

    /// A read that failed: every observation is forgotten, so a lapse needs a whole lease
    /// of successful reads again (an outage is no evidence of anyone's death).
    pub fn observe_failed(&mut self) {
        self.rows.clear();
    }

    /// The `(ordinal, boot)` of every dirty live row that lapsed by the reads issued up to
    /// `now_ms`: vouched (`seen_ms == 0`), or showing one `seen_ms` from a read received at
    /// `first` to a later read issued at least [`HOST_LEASE_MS`] after `first`. Time without
    /// a read counts for nothing (`now_ms` never extends the span). By ordinal.
    pub fn lapsed(&self, now_ms: u64) -> Vec<(u32, u64)> {
        self.rows
            .values()
            .filter(|w| !w.row.clean && w.row.state == HOST_LIVE)
            .filter(|w| {
                w.row.seen_ms == 0 || w.last_issued_ms.min(now_ms).saturating_sub(w.first_received_ms) >= HOST_LEASE_MS
            })
            .map(|w| (w.row.ordinal, w.row.boot))
            .collect()
    }
}

/// A declaration of successions for the runtime to execute in the store
/// ([`Coordinator::declare_wanted`]): `dead[i]` (ordinal, boot) to heir `heirs[i]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declare {
    pub dead: Vec<(u32, u64)>,
    pub heirs: Vec<u32>,
    pub kind: SuccessionKind,
}

/// One open succession and its answers.
#[derive(Clone, Debug)]
struct Run {
    s: Succession,
    /// Survivors that answered `SuccessionApplied` (or listed it in an `Attach`).
    applied: BTreeSet<u32>,
    /// The heir answered `SuccessionAdopted`.
    adopted: bool,
    /// `clock_ms` past which a run not answered by everyone rolls the world back.
    deadline_ms: u64,
}

impl Run {
    /// Every survivor applied it and the heir adopted it: a cut may settle it.
    fn answered(&self) -> bool {
        self.adopted && self.s.survivors.iter().all(|h| self.applied.contains(h))
    }
}

/// The coordinator's successions (never saved: the store's open `successions` rows are
/// the truth a restarted coordinator reads again).
#[derive(Debug)]
pub(super) struct SuccessionRuns {
    /// `CrashPolicy::Succession`: the runs are on.
    enabled: bool,
    /// The store's open successions are known (a restarted coordinator reads them first:
    /// until then nothing is declared and no cut begins — "wait, never guess").
    known: bool,
    /// A declaration was handed to the runtime and is not answered yet.
    declaring: bool,
    open: BTreeMap<SuccessionId, Run>,
    /// A world rollback is wanted ([`Coordinator::rollback_wanted`]).
    rollback: bool,
    /// The predecessors declared, `(ordinal, boot)`: never lapsed or declared again.
    declared: BTreeSet<(u32, u64)>,
}

impl Default for SuccessionRuns {
    fn default() -> Self {
        SuccessionRuns {
            enabled: false,
            known: true,
            declaring: false,
            open: BTreeMap::new(),
            rollback: false,
            declared: BTreeSet::new(),
        }
    }
}

impl super::Coordinator {
    /// What a host crash costs the world this coordinator runs: `Succession` turns the
    /// succession runs on (several hosts under `claim`); `Rollback` (the default) never
    /// declares one.
    pub fn set_crash_policy(&mut self, policy: CrashPolicy) {
        self.successions.enabled = policy == CrashPolicy::Succession;
    }

    /// The store's open successions are not known yet (a coordinator starting): nothing is
    /// declared and no cut begins until [`Self::successions_open`].
    pub fn successions_unknown(&mut self) {
        self.successions.known = false;
    }

    /// The store's open successions (a coordinator starting, `open_successions()`): each is
    /// run again — its order re-sent to every survivor as it attaches (they answer from
    /// memory), its deadline counted from `now_ms`.
    pub fn successions_open(&mut self, list: Vec<Succession>, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.successions.known = true;
        for s in list {
            if self.successions.open.contains_key(&s.id) {
                continue;
            }
            log::info!(
                "coordinator: succession {} (host {} → heir {}) is open: its orders go out again",
                s.id.0,
                s.predecessor,
                s.heir
            );
            self.open_run(s, now_ms);
        }
    }

    /// The world generation the attached hosts run (what a declaration is checked against).
    pub fn world_generation(&self) -> u64 {
        self.generation_seen
    }

    /// The lapsed hosts to declare dead now, in ONE batch, each with its heir — or `None`
    /// (the module docs). A running cut with a lapsed participant is decided abort here
    /// first (the declaration follows once that decision is recorded). The runtime executes
    /// the batch and answers [`Self::declared`] (or [`Self::declare_failed`]).
    pub fn declare_wanted(&mut self, now_ms: u64) -> Option<Declare> {
        let r = &self.successions;
        if !r.enabled || !r.known || r.declaring || r.rollback || !r.open.is_empty() {
            return None;
        }
        let lapsed = self.lapsed_undeclared(now_ms);
        if lapsed.is_empty() {
            return None;
        }
        if let Some(run) = self.cuts_running_participants() {
            if let Some((h, _)) = lapsed.iter().find(|(h, _)| run.contains(h)) {
                self.abort_cut_for_lapse(*h);
            }
            return None;
        }
        let gone: BTreeSet<u32> = lapsed.iter().map(|(h, _)| *h).collect();
        let live: BTreeSet<u32> = self.attached_live().into_iter().filter(|h| !gone.contains(h)).collect();
        if live.is_empty() {
            log::debug!("coordinator: hosts {gone:?} lapsed and no live host is attached: nothing to declare");
            return None;
        }
        let heirs: Vec<u32> =
            lapsed.iter().map(|(h, _)| crate::heir_of(*h, &live).expect("a live host other than the predecessor")).collect();
        log::warn!("coordinator: hosts {lapsed:?} lost their lease: declaring them dead, heirs {heirs:?}");
        self.successions.declaring = true;
        Some(Declare { dead: lapsed, heirs, kind: SuccessionKind::Crash })
    }

    /// The store's answer to the batch [`Self::declare_wanted`] handed out (the module docs).
    pub fn declared(&mut self, outcome: Declared, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.successions.declaring = false;
        match outcome {
            Declared::Done(list) => {
                for s in list {
                    log::warn!(
                        "coordinator: succession {} declared: host {} (boot {:#x}) → heir {}, {} super-region(s), base cut {}, \
                         survivors {:?}",
                        s.id.0,
                        s.predecessor,
                        s.predecessor_boot,
                        s.heir,
                        s.moved.len(),
                        s.base_cut.0,
                        s.survivors
                    );
                    let predecessor = s.predecessor;
                    self.open_run(s, now_ms);
                    self.host_succeeded(predecessor);
                }
            }
            Declared::SuccessionOpen => {
                log::error!("coordinator: another succession is open in the store: the world rolls back");
                self.successions.rollback = true;
            }
            Declared::NotLapsed(hosts) => {
                log::info!("coordinator: hosts {hosts:?} beat again (or left) before the declaration: forgotten");
            }
            Declared::Gone(hosts) => {
                log::info!("coordinator: hosts {hosts:?} were replaced or already succeeded: the declaration is forgotten");
            }
        }
    }

    /// The batch [`Self::declare_wanted`] handed out could not be executed (a store error, a
    /// lock timeout): forgotten; the runtime asks again after its retry wait.
    pub fn declare_failed(&mut self) {
        self.successions.declaring = false;
    }

    /// Whether the world must roll back to its last committed cut (the module docs): the
    /// runtime calls `rollback_world` and then [`Self::rolled_back`].
    pub fn rollback_wanted(&self) -> bool {
        self.successions.rollback
    }

    /// The world was rolled back to `generation`: every open succession is abandoned
    /// ([`CoordMsg::SuccessionDone`] unsettled) and every host told
    /// ([`CoordMsg::Generation`]: it exits).
    pub fn rolled_back(&mut self, generation: u64) {
        log::error!("coordinator: the world rolled back to its last committed cut: generation {generation}");
        let open = std::mem::take(&mut self.successions.open);
        for id in open.into_keys() {
            self.push(Target::Every, CoordMsg::SuccessionDone { id, settled: false });
        }
        self.successions.rollback = false;
        self.successions.declaring = false;
        self.push(Target::Every, CoordMsg::Generation(generation));
    }

    /// Test support: every open succession with the survivors that applied it and whether
    /// the heir adopted it.
    #[doc(hidden)]
    pub fn succession_progress(&self) -> Vec<(SuccessionId, Vec<u32>, bool)> {
        self.successions.open.iter().map(|(id, r)| (*id, r.applied.iter().copied().collect(), r.adopted)).collect()
    }

    /// Survivor `host` applied succession `id`.
    pub(super) fn succession_applied(&mut self, host: u32, id: SuccessionId) {
        match self.successions.open.get_mut(&id) {
            Some(run) if run.s.survivors.contains(&host) => {
                run.applied.insert(host);
            }
            Some(_) => log::warn!("coordinator: host {host} applied succession {}, which does not name it a survivor", id.0),
            None => log::debug!("coordinator: host {host} applied succession {}, which is not open", id.0),
        }
    }

    /// The heir `host` adopted succession `id`.
    pub(super) fn succession_adopted(&mut self, host: u32, id: SuccessionId) {
        match self.successions.open.get_mut(&id) {
            Some(run) if run.s.heir == host => run.adopted = true,
            Some(run) => log::warn!("coordinator: host {host} adopted succession {}, whose heir is {}", id.0, run.s.heir),
            None => log::debug!("coordinator: host {host} adopted succession {}, which is not open", id.0),
        }
    }

    /// An `Attach` of `host` listing the successions it answered (`(id, adopted)`): counted
    /// as answered again; every open one naming it a survivor that it did not list is sent
    /// (again).
    pub(super) fn succession_attach(&mut self, host: u32, answered: Vec<(SuccessionId, bool)>) {
        for (id, adopted) in answered {
            self.succession_applied(host, id);
            if adopted {
                self.succession_adopted(host, id);
            }
        }
        let orders: Vec<Succession> = self
            .successions
            .open
            .values()
            .filter(|r| r.s.survivors.contains(&host) && !r.applied.contains(&host))
            .map(|r| r.s.clone())
            .collect();
        for s in orders {
            self.push(Target::Host(host), order(&s));
        }
    }

    /// The upkeep: a run past its deadline without every answer, or a lapse while one is
    /// open, asks for a rollback.
    pub(super) fn maintain_successions(&mut self, now_ms: u64) {
        let r = &self.successions;
        if !r.enabled || r.rollback || r.open.is_empty() {
            return;
        }
        let late: Vec<(SuccessionId, Vec<u32>, bool)> = r
            .open
            .values()
            .filter(|run| !run.answered() && now_ms >= run.deadline_ms)
            .map(|run| {
                (run.s.id, run.s.survivors.iter().copied().filter(|h| !run.applied.contains(h)).collect(), run.adopted)
            })
            .collect();
        if let Some((id, silent, adopted)) = late.first() {
            log::error!(
                "coordinator: succession {} not answered within {SUCCESSION_DEADLINE_MS} ms (survivors {silent:?} silent, \
                 adopted: {adopted}): the world rolls back",
                id.0
            );
            self.successions.rollback = true;
            return;
        }
        let lapsed = self.lapsed_undeclared(now_ms);
        if !lapsed.is_empty() {
            log::error!("coordinator: hosts {lapsed:?} lost their lease while a succession is open: the world rolls back");
            self.successions.rollback = true;
        }
    }

    /// Whether no world cut may begin: the open successions are not known yet, one is being
    /// declared, a rollback is wanted, or an open one is not answered by everyone. The
    /// latter's ids are returned for the blocked report.
    pub(super) fn successions_block_cuts(&self) -> Option<Vec<SuccessionId>> {
        let r = &self.successions;
        if !r.enabled {
            return None;
        }
        let waiting: Vec<SuccessionId> = r.open.values().filter(|run| !run.answered()).map(|run| run.s.id).collect();
        (!r.known || r.declaring || r.rollback || !waiting.is_empty()).then_some(waiting)
    }

    /// The open successions a cut beginning now settles if it commits (every one answered:
    /// no cut begins otherwise).
    pub(super) fn successions_to_settle(&self) -> Vec<SuccessionId> {
        self.successions.open.keys().copied().collect()
    }

    /// A cut naming `settles` committed: those successions are settled, their survivors told.
    pub(super) fn successions_settled(&mut self, settles: &[SuccessionId]) {
        for id in settles {
            let Some(run) = self.successions.open.remove(id) else { continue };
            log::info!("coordinator: succession {} (host {} → heir {}) settled", id.0, run.s.predecessor, run.s.heir);
            for &h in &run.s.survivors {
                self.push(Target::Host(h), CoordMsg::SuccessionDone { id: *id, settled: true });
            }
        }
    }

    /// The lapsed rows, `(ordinal, boot)`, but those already declared.
    fn lapsed_undeclared(&self, now_ms: u64) -> Vec<(u32, u64)> {
        let declared = &self.successions.declared;
        self.leases.lapsed(now_ms).into_iter().filter(|d| !declared.contains(d)).collect()
    }

    /// Run `s` from `now_ms`: its orders go to the survivors attached now.
    fn open_run(&mut self, s: Succession, now_ms: u64) {
        self.successions.declared.insert((s.predecessor, s.predecessor_boot));
        self.note_succeeded(s.predecessor, s.predecessor_boot);
        let attached: Vec<u32> = s.survivors.iter().copied().filter(|h| self.is_attached(*h)).collect();
        for h in attached {
            self.push(Target::Host(h), order(&s));
        }
        let run = Run { s, applied: BTreeSet::new(), adopted: false, deadline_ms: now_ms + SUCCESSION_DEADLINE_MS };
        self.successions.open.insert(run.s.id, run);
    }

    /// Host `host` was succeeded: what an `Attach` of it with nothing running and nobody
    /// online does — its history-live regions park (the heir's regions start them again),
    /// its online players (and those in transit to it) go offline, and what the
    /// coordinator knew of it (`region_host`, `online_on`) and still meant to send it (its
    /// directives and claim answers) is dropped.
    fn host_succeeded(&mut self, host: u32) {
        self.drop_unsent_directives(host);
        self.drop_unsent(host, "claim answers", |m| matches!(m, CoordMsg::ClaimIs { .. } | CoordMsg::HopIs { .. }));
        let parked: Vec<crate::RegionCoords> =
            self.history.live_regions().into_iter().filter(|rc| self.region_host.get(rc) == Some(&host)).collect();
        for rc in &parked {
            self.history.region_parked(*rc, Vec::new());
        }
        self.region_host.retain(|_, h| *h != host);
        let gone: Vec<crate::PlayerId> = self.online_on.iter().filter(|(_, h)| **h == host).map(|(p, _)| *p).collect();
        for p in &gone {
            self.online_on.remove(p);
            self.online_reported.remove(p);
            self.displaced_told.retain(|(q, _, _)| q != p);
            self.set_online(*p, false);
        }
        log::warn!("coordinator: host {host} succeeded: {} region(s) parked, {} player(s) offline", parked.len(), gone.len());
        self.dirty_since_save = true;
        self.push_directives();
    }
}

/// The order of succession `s` ([`CoordMsg::Succession`]).
fn order(s: &Succession) -> CoordMsg {
    CoordMsg::Succession {
        id: s.id,
        predecessor: s.predecessor,
        predecessor_boot: s.predecessor_boot,
        heir: s.heir,
        kind: s.kind,
        base_cut: s.base_cut,
        moved: s.moved.clone(),
        survivors: s.survivors.clone(),
    }
}

impl super::Coordinator {
    /// A successful read of the `hosts` rows, issued at `issued_ms` and received at
    /// `received_ms` ([`LeaseWatch::observe`]).
    pub fn observe_leases(&mut self, rows: &[HostSeen], issued_ms: u64, received_ms: u64) {
        self.leases.observe(rows, issued_ms, received_ms);
    }

    /// A failed read of the `hosts` rows ([`LeaseWatch::observe_failed`]).
    pub fn leases_unread(&mut self) {
        self.leases.observe_failed();
    }

    /// The lease watch (Task 8 declares successions from [`LeaseWatch::lapsed`]).
    pub fn leases(&self) -> &LeaseWatch {
        &self.leases
    }
}
