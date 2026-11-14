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
//!
//! **Drain** (Task 11, decision 7): a host that leaves parks everything in its leave cut, its
//! `hosts` row turns clean and its claims STAY. When a live host needs one of its regions it
//! says so ([`HostMsg::LandWanted`]); the runtime looks the owner up
//! ([`Coordinator::take_land_lookups`] → [`Coordinator::land_wanted`]) and, when no host
//! lapsed, [`Coordinator::declare_wanted`] hands out ONE `Leave` succession giving ALL that
//! host's land to `heir_of` over the live attached hosts, with the same runs as a crash's —
//! at once for an ordinal at or beyond the configured host count
//! ([`Coordinator::set_host_count`]), else [`HEIR_WAIT_MS`] after the first request (a
//! rolling restart gets its land back). Only between cuts, and only for the boot the latest
//! hosts read shows clean and live; a request is forgotten once that ordinal is back (its row
//! dirty, or attached and live), and the store refuses a declaration that a registration beat
//! (`Gone`). Nothing is decided by the leaver. A restarted coordinator forgets the requests:
//! the hosts ask again every [`crate::LAND_WANTED_EVERY_MS`].

use std::collections::{BTreeMap, BTreeSet};

use super::{CoordMsg, Target};
use crate::{
    CrashPolicy, Declared, Succession, SuccessionId, SuccessionKind, HEIR_WAIT_MS, HOST_LEASE_MS, HOST_LIVE, SUCCESSION_DEADLINE_MS,
};

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
    /// The lease a row lapses after, ms: [`HOST_LEASE_MS`] unless a real-time test shortened
    /// it ([`Coordinator::set_lease_dials`]).
    lease_ms: Option<u64>,
    /// Successful reads observed so far: a declaration the store refused waits for a newer
    /// one ([`Coordinator::declared`]).
    reads: u64,
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
        self.reads += 1;
    }

    /// Host `ordinal`'s row as the latest successful read showed it.
    pub fn row(&self, ordinal: u32) -> Option<&HostSeen> {
        self.rows.get(&ordinal).map(|w| &w.row)
    }

    /// Successful reads observed so far.
    pub fn reads(&self) -> u64 {
        self.reads
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
                w.row.seen_ms == 0
                    || w.last_issued_ms.min(now_ms).saturating_sub(w.first_received_ms) >= self.lease_ms.unwrap_or(HOST_LEASE_MS)
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
    /// [`LeaseWatch::reads`] when the declaration in flight was handed out.
    declared_at_read: u64,
    /// The deadline of a run, ms: [`SUCCESSION_DEADLINE_MS`] unless a real-time test changed
    /// it ([`Coordinator::set_lease_dials`]).
    deadline_ms: Option<u64>,
    /// The store refused the last declaration (`NotLapsed` / `Gone`) as of this read: no
    /// declaration until a newer read (the same observation would hand out the same batch
    /// on every runtime pass).
    refused_at_read: Option<u64>,
    /// Ids an `Attach` listed before the open successions were known: answered then.
    listed_early: Vec<(u32, SuccessionId, bool)>,
    /// Super-regions hosts asked for ([`crate::HostMsg::LandWanted`]) that the runtime is to
    /// look up in the store ([`Coordinator::take_land_lookups`]).
    land_lookups: BTreeSet<crate::SuperRegion>,
    /// Hosts that left cleanly whose land is wanted ([`Coordinator::land_wanted`]), each with
    /// the `clock_ms` of its FIRST request (asking again does not restart the wait).
    land_wanted: BTreeMap<u32, u64>,
    /// The configured host count (`VOXEL_HOSTS`, [`Coordinator::set_host_count`]): the land
    /// of an ordinal at or beyond it is given away at once. Unknown: every ordinal waits.
    host_count: Option<u32>,
    /// The heir wait, ms: [`HEIR_WAIT_MS`] unless a real-time test changed it
    /// ([`Coordinator::set_heir_wait_ms`]).
    heir_wait_ms: Option<u64>,
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
            declared_at_read: 0,
            deadline_ms: None,
            refused_at_read: None,
            listed_early: Vec::new(),
            land_lookups: BTreeSet::new(),
            land_wanted: BTreeMap::new(),
            host_count: None,
            heir_wait_ms: None,
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

    /// Test support (real-time tests): the lease a host lapses after and a run's deadline, ms,
    /// instead of [`HOST_LEASE_MS`] and [`SUCCESSION_DEADLINE_MS`] — set together with the
    /// store's (`world_store`'s `set_host_lease_ms`) and the hosts' heartbeat period.
    #[doc(hidden)]
    pub fn set_lease_dials(&mut self, lease_ms: u64, deadline_ms: u64) {
        self.leases.lease_ms = Some(lease_ms);
        self.successions.deadline_ms = Some(deadline_ms);
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
        let early = std::mem::take(&mut self.successions.listed_early);
        for (host, id, adopted) in early {
            if self.successions.open.contains_key(&id) {
                self.succession_applied(host, id);
                if adopted {
                    self.succession_adopted(host, id);
                }
            } else {
                self.answer_closed(host, id);
            }
        }
    }

    /// Host `host` listed succession `id`, which is not open: it is over. An abandoned one
    /// came with a world rollback, and a host of the older generation is refused (the
    /// runtime) or told the generation (`rolled_back_to`) before its `Attach` is applied — so
    /// an id a host of the current generation holds and that is not open was settled.
    fn answer_closed(&mut self, host: u32, id: SuccessionId) {
        log::info!("coordinator: host {host} holds succession {}, which is over: told it settled", id.0);
        self.push(Target::Host(host), CoordMsg::SuccessionDone { id, settled: true });
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
        if r.refused_at_read.is_some_and(|at| self.leases.reads() <= at) {
            return None;
        }
        let lapsed = self.lapsed_undeclared(now_ms);
        if lapsed.is_empty() {
            return self.leave_wanted(now_ms);
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
        self.successions.declared_at_read = self.leases.reads();
        Some(Declare { dead: lapsed, heirs, kind: SuccessionKind::Crash })
    }

    /// The store's answer to the batch [`Self::declare_wanted`] handed out (the module docs).
    pub fn declared(&mut self, outcome: Declared, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.successions.declaring = false;
        match outcome {
            Declared::Done(list) => {
                for s in list {
                    self.successions.land_wanted.remove(&s.predecessor);
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
                self.successions.refused_at_read = Some(self.successions.declared_at_read);
            }
            Declared::Gone(hosts) => {
                log::info!("coordinator: hosts {hosts:?} were replaced or already succeeded: the declaration is forgotten");
                self.successions.refused_at_read = Some(self.successions.declared_at_read);
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
        // A host attaching with an older generation from now on is told this one.
        self.generation_seen = self.generation_seen.max(generation);
        self.rolled_back_to = Some(generation);
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
            if !self.successions.open.contains_key(&id) {
                if self.successions.known {
                    self.answer_closed(host, id);
                } else {
                    self.successions.listed_early.push((host, id, adopted));
                }
                continue;
            }
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
                "coordinator: succession {} not answered within {} ms (survivors {silent:?} silent, \
                 adopted: {adopted}): the world rolls back",
                id.0,
                self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS)
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
    /// no cut begins otherwise). Pairing: the heir answered `SuccessionAdopted`, so its part
    /// of that cut carries `WorldCut::adopts` (Task 9) — the store does not check it.
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
        let deadline = self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS);
        let run = Run { s, applied: BTreeSet::new(), adopted: false, deadline_ms: now_ms + deadline };
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
    /// The configured host count (`VOXEL_HOSTS`): the land of a host that left cleanly with
    /// an ordinal at or beyond it is given away at the first request (it will not come back).
    pub fn set_host_count(&mut self, hosts: u32) {
        self.successions.host_count = Some(hosts);
    }

    /// Test support (real-time tests): the heir wait, ms, instead of [`HEIR_WAIT_MS`].
    #[doc(hidden)]
    pub fn set_heir_wait_ms(&mut self, ms: u64) {
        self.successions.heir_wait_ms = Some(ms);
    }

    /// Host `host` needs a region of `sr`, whose owner left cleanly
    /// ([`crate::HostMsg::LandWanted`]): looked up by the runtime
    /// ([`Self::take_land_lookups`]). Ignored unless successions run (`Static` never gives
    /// land away: its leaver gets it back when it returns, R3).
    pub(super) fn land_asked(&mut self, host: u32, sr: crate::SuperRegion) {
        if !self.successions.enabled {
            log::debug!("coordinator: host {host} wants super-region {sr:?}, but successions are off; ignored");
            return;
        }
        self.successions.land_lookups.insert(sr);
    }

    /// The super-regions hosts asked for since the last call: the runtime reads their owners
    /// in the store and answers [`Self::land_wanted`] for each owned one.
    pub fn take_land_lookups(&mut self) -> Vec<crate::SuperRegion> {
        std::mem::take(&mut self.successions.land_lookups).into_iter().collect()
    }

    /// Host `owner` holds a super-region a host asked for (the runtime's lookup): if it left
    /// cleanly, ALL its land goes to one heir — at once if `owner` is at or beyond the
    /// configured host count, else [`HEIR_WAIT_MS`] after the first such request — unless
    /// that ordinal comes back first ([`Self::declare_wanted`] decides; the module docs).
    pub fn land_wanted(&mut self, owner: u32, now_ms: u64) {
        if !self.successions.enabled {
            return;
        }
        self.clock_ms = self.clock_ms.max(now_ms);
        if self.attached_live().contains(&owner) {
            // Running: nothing of it to give away (a stale directory row of the asker).
            return;
        }
        let wait = self.heir_wait_for(owner);
        self.successions.land_wanted.entry(owner).or_insert_with(|| {
            log::info!(
                "coordinator: host {owner} left and its land is wanted: it goes to an heir in {wait} ms unless host {owner} \
                 comes back"
            );
            now_ms
        });
    }

    /// How long the land of departed host `owner` waits for it.
    fn heir_wait_for(&self, owner: u32) -> u64 {
        if self.successions.host_count.is_some_and(|n| owner >= n) {
            0
        } else {
            self.successions.heir_wait_ms.unwrap_or(HEIR_WAIT_MS)
        }
    }

    /// The `Leave` succession to declare now for drained land, if any (called by
    /// [`Self::declare_wanted`] when no host lapsed): a wanted owner whose `hosts` row the
    /// latest read shows clean and live, not declared yet, whose wait is over, with a live
    /// attached heir, between cuts. A request is forgotten once its ordinal is back — its
    /// row dirty (another boot registered) or the ordinal attached and live; a row not read
    /// (yet) is waited for.
    fn leave_wanted(&mut self, now_ms: u64) -> Option<Declare> {
        if self.successions.land_wanted.is_empty() {
            return None;
        }
        let attached: BTreeSet<u32> = self.attached_live().into_iter().collect();
        let leases = &self.leases;
        self.successions.land_wanted.retain(|owner, _| {
            let back = attached.contains(owner) || leases.row(*owner).is_some_and(|r| !r.clean || r.state != HOST_LIVE);
            if back {
                log::info!("coordinator: host {owner} is back (or no longer left): its land is no longer given away");
            }
            !back
        });
        let mut due = None;
        for (&owner, &first) in &self.successions.land_wanted {
            let Some(row) = self.leases.row(owner) else { continue };
            if self.successions.declared.contains(&(owner, row.boot)) {
                continue;
            }
            if now_ms < first.saturating_add(self.heir_wait_for(owner)) {
                continue;
            }
            due = Some((owner, row.boot));
            break;
        }
        let (owner, boot) = due?;
        if self.cuts_running_participants().is_some() {
            // Only between cuts: the store's base cut is the one every survivor counts from.
            return None;
        }
        let live: BTreeSet<u32> = attached.into_iter().filter(|h| *h != owner).collect();
        let Some(heir) = crate::heir_of(owner, &live) else {
            log::debug!("coordinator: host {owner}'s land is wanted and no live host is attached to inherit it");
            return None;
        };
        log::warn!("coordinator: host {owner} (boot {boot:#x}) left and its land is wanted: giving it all to heir {heir}");
        self.successions.declaring = true;
        self.successions.declared_at_read = self.leases.reads();
        Some(Declare { dead: vec![(owner, boot)], heirs: vec![heir], kind: SuccessionKind::Leave })
    }

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
