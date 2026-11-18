//! Successions (R4): which hosts lost their lease, and the successions the coordinator
//! declares and runs for them. The coordinator declares a host dead only from what it read
//! itself — the same `seen_ms` over [`HOST_LEASE_MS`] of its own clock, every read in
//! between a successful one — so a database outage never looks like every host dying. Pure:
//! the runtime reads the `hosts` rows and passes its clock in; the declarations, ends and
//! rollbacks the core hands out run in the store inside the transaction of the transition
//! carrying them (Elastic 2b Task 3, `decisions.rs`), and their answers come back as inputs.
//!
//! **The runs** ([`SuccessionRuns`], `CrashPolicy::Succession` only — `Static` never
//! declares, its crash policy is the world rollback):
//! - [`Coordinator::declare_wanted`] hands the runtime ONE batch of every lapsed host, each
//!   with its heir ([`crate::heir_of`] over the attached hosts that did not lapse — those
//!   neither draining nor leaving, while there is one: `heir_candidates`), only
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
//! - A restarted coordinator loads its runs with its projection (Elastic 2a), reads the open
//!   successions again ([`Coordinator::successions_open`]: a run the store no longer lists is
//!   dropped; one it lists that the projection lacks — the dead process never wrote its
//!   declaration's answer — has its predecessor taken out of the view) and re-sends their
//!   orders as the recipients attach again; hosts answer again from memory
//!   (`Attach.successions`). A dead dirty row this core never declared (an end whose answer
//!   was lost so) is taken out of the view at the `hosts` read that shows it
//!   ([`Coordinator::observe_leases`]).
//! - The orders go to the survivors AND the heir (the heir is a survivor whenever the
//!   coordinator declared: it was live). A succession `worldctl retire-host` opened while no
//!   host ran (before Elastic 2a; that command now refuses with no host running, so no new one
//!   is opened — the handling below stays for such a row) has no survivors: its heir booted after it, has nothing to report or resolve and
//!   adopts from the store alone; its answers are all a cut waits for, and its
//!   `SuccessionDone` goes to it too.
//! - **Answers are per boot.** An answer counts only while the boot that gave it is the
//!   host's current one (its latest `Attach`): no cut names a run until the heir's CURRENT boot
//!   adopted it and every survivor's current boot applied it. An offline run's heir that boots
//!   again (it adopted, then died before the settle) has its answers forgotten at its new
//!   boot's `Attach` and gets the order again — it adopts again from the store. A declared
//!   run's recipient that attaches as another boot than the one the run counts on (its boot
//!   when the run opened) lost its journal: no order goes to that boot and the world rolls
//!   back. (Elastic 1: a restarted process registers under a NEW id, so no recipient attaches
//!   as another boot of itself any more — the guard stays. A recipient's restart leaves its old
//!   id silent instead: that id lapses while the run is open — a second failure before the
//!   settle — and the world rolls back; with nobody alive the restart's registration raises
//!   the generation, which fences the old parts.)
//! - A RUNNING coordinator reads the open successions again — on these three triggers only,
//!   never periodically —
//!   ([`Coordinator::take_successions_reread`]) when the store changed them behind its back:
//!   a `hosts` read shows a dead row it never declared (an offline `retire-host`), or a host
//!   attaches at a newer generation, or (the runtime, [`Coordinator::successions_reread`]) a
//!   decision is refused at a stale generation (`worldctl rollback`). Until the answer no
//!   cut begins and nothing is declared; then a new one runs as at a restart (its predecessor
//!   taken out of the view), and a run no longer open is dropped as abandoned (every host
//!   told `SuccessionDone { settled: false }`) — only a run that opened at or before the row
//!   version the read was issued at (Elastic 2b Task 6, audit H5: a run opened later may be
//!   missing from a read that ran before its row committed).
//! - **The ticker** (Elastic 2b Task 6): of several coordinator replicas only the ticker (the
//!   lowest live id) reads the `hosts` rows and the open successions, watches the leases and
//!   hands out declarations, ends and rollbacks. Its lease watch and its knowledge of the open
//!   successions are its process's: a new ticker counts every lease from zero (it never
//!   hastens a declaration) and reads the open successions first; a store call the previous
//!   ticker handed out is looked up (its marks, `decisions.rs`), never issued again. A replica
//!   that does not know the open successions buffers an `Attach`'s listing in the projection
//!   for the ticker to answer.
//!
//! **The crash-loop rule** (Elastic 2a, decision 9: recipients own land). The store's survivors
//! are the dirty live hosts holding land (a `super_regions` or `in_flight` row) plus the heirs;
//! heirs are picked among landed hosts that take logins first ([`HostSeen::landed`]). A lapsed
//! host holding NO land that no open run names is ENDED ([`Coordinator::take_end_landless`]:
//! its row dead, no succession, the generation untouched) — with no run open instead of a
//! succession, and while one is open instead of a second failure: a pod that crash-loops
//! beside a healthy host (each restart a new landless id) never rolls the world back. A lapsed
//! LANDED host while a run is open, or a lapsed recipient, is still a second failure (the
//! residual: a host that won land after the declaration and dies before the settle rolls the
//! world back — a rollback, never a loss). Such a host that traded state with the dead host
//! (its directory stale: no order reached it) stops itself without a save at the next
//! `BeginCut` (`world_manager/world_cut.rs`, `take_unaccounted_dead`), which is that rollback.
//!
//! **Drain** (Task 11, decision 7): a host that leaves parks everything in its leave cut, its
//! `hosts` row turns clean and its claims STAY. When a live host needs one of its regions it
//! says so ([`HostMsg::LandWanted`]); the runtime looks the owner up
//! ([`Coordinator::take_land_lookups`] → [`Coordinator::land_wanted`]) and, when no host
//! lapsed, [`Coordinator::declare_wanted`] hands out ONE `Leave` succession giving ALL that
//! host's land to `heir_of` over the live attached hosts, with the same runs as a crash's —
//! at once (Elastic 1: a host that left never returns as itself — its restarted process is a
//! new host with a new id — so nothing waits for it). Only between cuts, and only for the boot
//! the latest hosts read shows clean and live; a request is dropped if that row stops being
//! clean and live, and the store refuses a declaration whose rows changed under it (`Gone` /
//! `NotLapsed`: asked again after a fresh read). Nothing is decided by the leaver. The requests
//! are in the projection (Elastic 2a); the hosts ask again every [`crate::LAND_WANTED_EVERY_MS`]
//! anyway.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{CallStamp, CoordMsg, DECISION_RETRY_MS, StoreDecision, Target};
use crate::HostId;
use crate::{
    CrashPolicy, Declared, Succession, SuccessionId, SuccessionKind, HOST_LEASE_MS, HOST_LIVE, SUCCESSION_DEADLINE_MS,
};

/// One `hosts` row as the lease watch needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSeen {
    pub ordinal: HostId,
    pub boot: u64,
    pub clean: bool,
    /// `HOST_LIVE` or `HOST_DEAD`.
    pub state: i64,
    /// The database clock at its last beat; 0 = vouched dead by a boot of its ordinal.
    pub seen_ms: u64,
    /// The operator marked it draining (`hosts.drain_at_ms` set, `worldctl drain`): it is never
    /// the gate host ([`super::Coordinator::gate_host`]).
    pub draining: bool,
    /// It holds land at the read: a `super_regions` row or an `in_flight` row of its id
    /// (Elastic 2a). A lapse of a landless host that no open run names is ENDED
    /// ([`Coordinator::take_end_landless`]), never succeeded; heirs are picked among landed
    /// hosts first ([`Coordinator::declare_wanted`]).
    pub landed: bool,
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
/// **Its clock is the process's monotonic one** (Elastic 2a final review, S1), never the
/// store's that the rest of the core runs on: the watch is an observation of this process (not
/// in the projection — a restart counts every lease from zero), and a forward step of the
/// store's clock (`StoreClock` takes any) between two reads would otherwise stretch a span
/// over a whole lease and declare a healthy host dead. The store's own lapse check
/// (`PG_NOW_MS - seen_ms` at the declaration) stays the second gate.
///
/// A read is stamped twice on that clock: when it was issued and when its answer was
/// received. The unchanged span counts from the first read's RECEIVE to the
/// latest read's ISSUE — a lower bound on how long the row really stood still. Stamping by
/// processing time alone would let a coordinator stall (an answer waiting in a channel)
/// stretch two ordinary reads of one beat over a whole lease.
#[derive(Clone, Debug, Default)]
pub struct LeaseWatch {
    rows: BTreeMap<HostId, Watched>,
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

    /// The watch of a replica that just became the ticker (Elastic 2b Task 6): nothing observed
    /// — every lease counts from zero, so a new ticker never hastens a declaration — under the
    /// same lease dial. The read count goes on (the refusals noted against it stay comparable).
    pub(super) fn restarted(&self) -> LeaseWatch {
        LeaseWatch { rows: BTreeMap::new(), lease_ms: self.lease_ms, reads: self.reads }
    }

    /// Host `ordinal`'s row as the latest successful read showed it.
    pub fn row(&self, ordinal: HostId) -> Option<&HostSeen> {
        self.rows.get(&ordinal).map(|w| &w.row)
    }

    /// Successful reads observed so far.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// A read that failed: every observation is forgotten, so a lapse needs a whole lease
    /// of successful reads again (an outage is no evidence of anyone's death). The drain
    /// marks are the projection's, and stay.
    pub fn observe_failed(&mut self) {
        self.rows.clear();
    }

    /// [`Self::lapsed`] by every read observed (the core's question: it never compares its
    /// store-clock `now` with the watch's monotonic stamps).
    pub fn lapsed_by_reads(&self) -> Vec<(HostId, u64)> {
        self.lapsed(u64::MAX)
    }

    /// The `(ordinal, boot)` of every dirty live row that lapsed by the reads issued up to
    /// `now_ms` (the watch's clock): vouched (`seen_ms == 0`), or showing one `seen_ms` from a
    /// read received at `first` to a later read issued at least [`HOST_LEASE_MS`] after
    /// `first`. Time without a read counts for nothing (`now_ms` never extends the span). By
    /// ordinal.
    pub fn lapsed(&self, now_ms: u64) -> Vec<(HostId, u64)> {
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

/// A lapsed host that holds no land, for the runtime to end in the store
/// (`end_landless_host`, [`Coordinator::take_end_landless`]): its row marked dead, no
/// succession, no generation change. The answer goes to [`Coordinator::landless_ended`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndLandless {
    pub host: HostId,
    pub boot: u64,
}

/// A declaration of successions for the store ([`Coordinator::declare_wanted`]): `dead[i]`
/// (ordinal, boot) to heir `heirs[i]`, decided at world `generation` (the one the hosts attach
/// at: the store refuses it `StaleGeneration` at another).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declare {
    pub dead: Vec<(HostId, u64)>,
    pub heirs: Vec<HostId>,
    pub kind: SuccessionKind,
    pub generation: u64,
}

/// One open succession and its answers.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Run {
    s: Succession,
    /// Recipients that answered `SuccessionApplied` (or listed it in an `Attach`), each with
    /// the boot that answered: an answer counts only while that boot is the host's current one.
    applied: BTreeMap<HostId, u64>,
    /// The boot of the heir that answered `SuccessionAdopted`.
    adopted: Option<u64>,
    /// A declared run's (survivors): each recipient's boot when the run opened (or at its first
    /// `Attach` after): the boot whose journal the run counts on. Another boot of it means that
    /// journal is gone — the run can never be answered: a rollback.
    boots: BTreeMap<HostId, u64>,
    /// The row version of the cache that opened it here (Elastic 2b Task 6, audit H5): never
    /// earlier than the commit of its `successions` row — a read issued at an older version may
    /// not list it, and abandons it only if issued at this version or later.
    opened_at: u64,
    /// `clock_ms` past which a run not answered by everyone rolls the world back. An offline
    /// run's (no survivors) counts from when its order first went to the heir: `u64::MAX`
    /// until then — the heir may boot long after the operator retired the host.
    deadline_ms: u64,
    /// An offline run's (no survivors): the boots the lease watch saw beat since it opened. A
    /// later lapse of one of them is a death during the succession (a rollback); a boot that
    /// was dead already when the world was retired is not — the retirement raised the
    /// generation, so it is refused at its next write, and its next boot resumes from the cut.
    beat: BTreeSet<(HostId, u64)>,
}

impl Run {
    /// Every survivor applied it and the heir adopted it, each by its CURRENT boot (`current`:
    /// a host's boot as its latest `Attach` named it): a cut may settle it. An answer of a boot
    /// that died since counts for nothing — the new boot holds nothing of what it adopted.
    fn answered(&self, current: impl Fn(HostId) -> Option<u64>) -> bool {
        let by_current = |h: HostId, boot: Option<u64>| boot.is_some() && boot == current(h);
        by_current(self.s.heir, self.adopted) && self.s.survivors.iter().all(|h| by_current(*h, self.applied.get(h).copied()))
    }
}

/// Whether host `host` gets succession `s`'s order (and its `SuccessionDone`): a survivor,
/// or the heir — which is no survivor only in a succession `worldctl retire-host` opened while
/// no host ran.
fn recipient(s: &Succession, host: HostId) -> bool {
    s.heir == host || s.survivors.contains(&host)
}

/// `s`'s recipients ([`recipient`]), by ordinal.
fn recipients(s: &Succession) -> BTreeSet<HostId> {
    s.survivors.iter().copied().chain(std::iter::once(s.heir)).collect()
}

/// The coordinator's successions, in its projection (Elastic 2a) but for the configuration,
/// the counts of lease reads and the ticker's reads of the open successions (`#[serde(skip)]`:
/// observations of this process — a restart counts again and reads again; a rebase keeps them,
/// [`SuccessionRuns::carry_process_fields`]). The store's open `successions` rows stay the
/// truth: the ticker reads them again (a starting, rebased or new ticker owes one read) and
/// drops a run they no longer list. A store call handed out by another replica (a declaration,
/// an end) counts as not in flight.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SuccessionRuns {
    /// `CrashPolicy::Succession`: the runs are on (configuration).
    #[serde(skip)]
    enabled: bool,
    /// This replica read the store's open successions and nothing asked for another read since
    /// (Elastic 2b Task 6: process-local — a loaded core, a rebased ticker or a new ticker owes
    /// a read; a replica that is not the ticker never reads). Until then nothing is declared, no
    /// cut begins and an `Attach`'s listing waits — "wait, never guess".
    #[serde(skip)]
    known: bool,
    /// A read of the open successions is handed out ([`Coordinator::take_successions_reread`])
    /// and not answered.
    #[serde(skip)]
    reading: bool,
    /// Runs this replica settled or abandoned while a read was out: a read that still lists one
    /// (it ran before the close) does not open it again.
    #[serde(skip)]
    closed_while_reading: BTreeSet<SuccessionId>,
    /// A declaration was handed out and is not answered yet: its mark (`decisions.rs`).
    declaring: Option<CallStamp>,
    open: BTreeMap<SuccessionId, Run>,
    /// A world rollback is wanted ([`Coordinator::rollback_wanted`]), decided at this world
    /// generation: the store rolls back only from it ([`Coordinator::rollback_from`]), so a
    /// rollback that committed before a crash is never done twice.
    rollback: Option<u64>,
    /// The rollback handed out ([`Coordinator::take_rollback`]) and not answered yet: its mark.
    rolling: Option<CallStamp>,
    /// A rollback's answer was lost: the world generation is to be read
    /// ([`Coordinator::take_generation_lookup`]); no rollback is handed out until it is.
    generation_lookup: bool,
    /// `clock_ms` before which no rollback is handed out again (after a failed one).
    #[serde(skip)]
    rollback_retry_ms: Option<u64>,
    /// The predecessors declared, `(ordinal, boot)`: never lapsed or declared again.
    declared: BTreeSet<(HostId, u64)>,
    /// [`LeaseWatch::reads`] when the declaration in flight was handed out.
    #[serde(skip)]
    declared_at_read: u64,
    /// The deadline of a run, ms: [`SUCCESSION_DEADLINE_MS`] unless a real-time test changed
    /// it ([`Coordinator::set_lease_dials`]; configuration).
    #[serde(skip)]
    deadline_ms: Option<u64>,
    /// The store refused the last declaration (`NotLapsed` / `Gone`) as of this read: no
    /// declaration until a newer read (the same observation would hand out the same batch
    /// on every runtime pass).
    #[serde(skip)]
    refused_at_read: Option<u64>,
    /// Ids an `Attach` listed before the open successions were known, with the listing boot:
    /// answered then — only if that boot is still the host's current one. Durable: a listing
    /// another replica took (it never reads) is answered by the ticker.
    listed_early: Vec<(HostId, u64, SuccessionId, bool)>,
    /// The open successions are to be read again ([`Coordinator::take_successions_reread`]):
    /// asked by an input on any replica, durable so the ticker sees it.
    reread: bool,
    /// Dead rows nobody here declared that already asked for a re-read (asked once each).
    reread_for: BTreeSet<(HostId, u64)>,
    /// Super-regions hosts asked for ([`crate::HostMsg::LandWanted`]) that the runtime is to
    /// look up in the store ([`Coordinator::take_land_lookups`]).
    land_lookups: BTreeSet<crate::SuperRegion>,
    /// Hosts that left cleanly whose land is wanted ([`Coordinator::land_wanted`]): given
    /// away at the next [`Coordinator::declare_wanted`] between cuts.
    land_wanted: BTreeSet<HostId>,
    /// The landless host handed out to end ([`Coordinator::take_end_landless`]), not answered
    /// yet, with its mark.
    ending: Option<(HostId, u64, CallStamp)>,
    /// [`LeaseWatch::reads`] when `ending` was handed out.
    #[serde(skip)]
    ending_at_read: u64,
    /// The store refused the last end (`NotLapsed` / `OwnsLand` / `Gone`) as of this read: no
    /// end until a newer read decides again.
    #[serde(skip)]
    end_refused_at_read: Option<u64>,
}

impl Default for SuccessionRuns {
    fn default() -> Self {
        SuccessionRuns {
            enabled: false,
            known: true,
            reading: false,
            closed_while_reading: BTreeSet::new(),
            declaring: None,
            open: BTreeMap::new(),
            rollback: None,
            rolling: None,
            generation_lookup: false,
            rollback_retry_ms: None,
            declared: BTreeSet::new(),
            declared_at_read: 0,
            deadline_ms: None,
            refused_at_read: None,
            listed_early: Vec::new(),
            reread: false,
            reread_for: BTreeSet::new(),
            land_lookups: BTreeSet::new(),
            land_wanted: BTreeSet::new(),
            ending: None,
            ending_at_read: 0,
            end_refused_at_read: None,
        }
    }
}

impl SuccessionRuns {
    /// A rebase ([`super::Coordinator::rebase`]): these runs (a newer row's, decoded) take the
    /// process's own fields of `from` (the configuration and the lease-read counts). Every field
    /// is named: a new one does not compile until classified.
    pub(super) fn carry_process_fields(&mut self, from: &SuccessionRuns) {
        let SuccessionRuns {
            enabled,
            known,
            reading,
            closed_while_reading,
            declaring: _,
            open: _,
            rollback: _,
            rolling: _,
            generation_lookup: _,
            rollback_retry_ms,
            declared: _,
            declared_at_read,
            deadline_ms,
            refused_at_read,
            listed_early: _,
            reread: _,
            reread_for: _,
            land_lookups: _,
            land_wanted: _,
            ending: _,
            ending_at_read,
            end_refused_at_read,
        } = from;
        self.enabled = *enabled;
        self.known = *known;
        self.reading = *reading;
        self.closed_while_reading = closed_while_reading.clone();
        self.rollback_retry_ms = *rollback_retry_ms;
        self.declared_at_read = *declared_at_read;
        self.deadline_ms = *deadline_ms;
        self.refused_at_read = *refused_at_read;
        self.ending_at_read = *ending_at_read;
        self.end_refused_at_read = *end_refused_at_read;
    }
}

impl super::Coordinator {
    /// The newest store time the open runs recorded ([`super::Coordinator::clock_floor`]):
    /// each run's start (its deadline less its span; an offline run waiting for its heir has
    /// none).
    pub(super) fn successions_clock_floor(&self) -> u64 {
        let span = self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS);
        self.successions
            .open
            .values()
            .filter(|r| r.deadline_ms != u64::MAX)
            .map(|r| r.deadline_ms.saturating_sub(span))
            .max()
            .unwrap_or(0)
    }
    /// What a host crash costs the world this coordinator runs: `Succession` turns the
    /// succession runs on (`claim`, any number of hosts); `Rollback` (the default) never
    /// declares one.
    pub fn set_crash_policy(&mut self, policy: CrashPolicy) {
        self.successions.enabled = policy == CrashPolicy::Succession;
    }

    /// The crash policy this core was configured with ([`Self::set_crash_policy`]).
    pub fn crash_policy(&self) -> CrashPolicy {
        if self.successions.enabled { CrashPolicy::Succession } else { CrashPolicy::Rollback }
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
    /// declared and no cut begins until the ticker read them ([`Input::SuccessionsOpen`]). A run
    /// the read does not list (a `worldctl rollback` while no coordinator ran) is dropped, every
    /// host told.
    ///
    /// [`Input::SuccessionsOpen`]: super::Input::SuccessionsOpen
    pub fn successions_unknown(&mut self) {
        self.successions_owed();
    }

    /// This replica owes a read of the open successions (a start, a rebase of the ticker, a new
    /// ticker): until it is answered they are not known here.
    pub(super) fn successions_owed(&mut self) {
        self.successions.known = false;
        self.successions.reading = false;
    }

    /// Whether this replica knows the store's open successions: read and nothing asked for a
    /// re-read since (always, with the runs off).
    pub(super) fn successions_known(&self) -> bool {
        let r = &self.successions;
        !r.enabled || (r.known && !r.reread)
    }

    /// A loaded core's run deadlines at store time `now_ms` ([`super::rebase_deadline`]).
    pub(super) fn rebase_succession_deadlines(&mut self, now_ms: u64) {
        let span = self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS);
        for run in self.successions.open.values_mut() {
            run.deadline_ms = super::rebase_deadline(run.deadline_ms, span, now_ms);
        }
    }

    /// A declaration is in flight (its mark set; one whose answer was lost is looked up and
    /// cleared, never counted as in flight for ever).
    fn declaring(&self) -> bool {
        self.successions.declaring.is_some()
    }

    /// The end of a landless host in flight.
    fn ending(&self) -> Option<(HostId, u64)> {
        self.successions.ending.map(|(h, b, _)| (h, b))
    }

    /// The succession marks whose stamp `matches` take `to`.
    pub(super) fn successions_stamp_marks(&mut self, matches: &dyn Fn(&CallStamp) -> bool, to: CallStamp) {
        let r = &mut self.successions;
        for stamp in [r.declaring.as_mut(), r.ending.as_mut().map(|(_, _, s)| s), r.rolling.as_mut()].into_iter().flatten() {
            if matches(stamp) {
                *stamp = to;
            }
        }
    }

    /// The succession marks whose stamp `matches` lost their answers (`why`, for the log):
    /// looked up, never issued again ([`super::Coordinator::orphan_marks`]) — a declaration or an
    /// end makes the open successions be read again (a run the store opened is then run, its
    /// predecessor out of the view; a row the store marked dead is taken out of the view at the
    /// next `hosts` read showing it, C1), and is not handed out again on the read it was handed
    /// out at; a rollback makes the world generation be read. Whether any matched.
    pub(super) fn successions_orphan_marks(&mut self, matches: &dyn Fn(&CallStamp) -> bool, why: &str) -> bool {
        let reads = self.leases.reads();
        let mut any = false;
        if self.successions.declaring.as_ref().is_some_and(|s| matches(s)) {
            log::warn!("coordinator: the answer to a declaration is lost ({why}): the open successions are read again");
            self.successions.declaring = None;
            self.successions.refused_at_read = Some(reads);
            self.reread_successions();
            any = true;
        }
        if let Some((host, boot, _)) = self.successions.ending.filter(|(_, _, s)| matches(s)) {
            log::warn!(
                "coordinator: the answer to ending host {host} (boot {boot:#x}) is lost ({why}): the open successions are \
                 read again, its row at the next read"
            );
            self.successions.ending = None;
            self.successions.end_refused_at_read = Some(reads);
            self.reread_successions();
            any = true;
        }
        if self.successions.rolling.as_ref().is_some_and(|s| matches(s)) {
            log::warn!("coordinator: the answer to a world rollback is lost ({why}): the world generation is read");
            self.successions.rolling = None;
            self.successions.generation_lookup = true;
            any = true;
        }
        any
    }

    /// Test support: [`super::Input::SuccessionsOpen`] of `list`, read as of this cache's version.
    #[doc(hidden)]
    pub fn successions_open(&mut self, list: Vec<Succession>, now_ms: u64) {
        let issued_at_version = self.version;
        self.apply(super::Input::SuccessionsOpen { list, issued_at_version }, now_ms);
    }

    /// The store's open successions, as the ticker's read issued at this cache's row version
    /// `issued_at` lists them ([`super::Input::SuccessionsOpen`]; a coordinator starting, or a
    /// re-read): each is run again — its order re-sent to every recipient as it attaches (they
    /// answer from memory), its deadline counted from `now_ms`. A run it does not list is
    /// dropped (the store abandoned it: every host told) — only if it opened at or before
    /// `issued_at` (Elastic 2b Task 6, audit H5: a run opened later, its row perhaps committed
    /// after the read ran, is no abandoned run), and never one a commit decided here settles. A
    /// run this core did not hold — at a re-read, or at a start (its declaration's answer died
    /// with the process that asked) — has its predecessor taken out of the view as a
    /// declaration's is; never one this replica settled or abandoned while the read was out.
    pub(super) fn on_successions_open(&mut self, list: Vec<Succession>, issued_at: u64, now_ms: u64) {
        self.reads_applied += 1;
        self.clock_ms = self.clock_ms.max(now_ms);
        self.successions.known = true;
        self.successions.reading = false;
        let closed = std::mem::take(&mut self.successions.closed_while_reading);
        let listed: BTreeSet<SuccessionId> = list.iter().map(|s| s.id).collect();
        // A run a commit decided here settles is not listed either once the store holds the
        // commit: its decision's answer settles it ([`Self::successions_settled`]).
        let settling: BTreeSet<SuccessionId> = self.settles_pending().into_iter().collect();
        let gone: Vec<SuccessionId> = self
            .successions
            .open
            .iter()
            .filter(|(id, run)| !listed.contains(id) && !settling.contains(id) && run.opened_at <= issued_at)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            let run = self.successions.open.remove(&id).expect("listed above");
            log::warn!(
                "coordinator: succession {} (host {} → heir {}) is no longer open in the store (abandoned by a world \
                 rollback): dropped",
                id.0,
                run.s.predecessor,
                run.s.heir
            );
            self.push(Target::Every, CoordMsg::SuccessionDone { id, settled: false });
        }
        for s in list {
            if self.successions.open.contains_key(&s.id) {
                continue;
            }
            if closed.contains(&s.id) {
                log::debug!("coordinator: succession {} was closed here while the read ran: not opened again", s.id.0);
                continue;
            }
            log::info!(
                "coordinator: succession {} (host {} → heir {}, survivors {:?}) is open: its orders go out again",
                s.id.0,
                s.predecessor,
                s.heir,
                s.survivors
            );
            let predecessor = s.predecessor;
            self.open_run(s, now_ms);
            // A run new to this core — at a re-read, and at a start too: the store committed
            // it and the process that declared it died before writing its answer (Elastic 2a
            // final review, C1; the projection still has the predecessor running regions and
            // holding players) — takes the predecessor out of the view as a declaration's
            // answer does. Idempotent: only what is still the predecessor's.
            self.host_succeeded(predecessor);
        }
        self.drain_listed_early();
    }

    /// The successions `Attach`es listed while the open ones were not known (any replica's: a
    /// non-ticker never knows them), answered once they are here.
    pub(super) fn drain_listed_early(&mut self) {
        if !self.successions_known() || self.successions.listed_early.is_empty() {
            return;
        }
        let early = std::mem::take(&mut self.successions.listed_early);
        for (host, boot, id, adopted) in early {
            if self.attached_boot(host) != Some(boot) {
                // Listed by a boot that is gone: its answers are not its successor's.
                continue;
            }
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
    fn answer_closed(&mut self, host: HostId, id: SuccessionId) {
        log::info!("coordinator: host {host} holds succession {}, which is over: told it settled", id.0);
        self.push(Target::Host(host), CoordMsg::SuccessionDone { id, settled: true });
    }

    /// The world generation the attached hosts run (what a declaration is checked against).
    pub fn world_generation(&self) -> u64 {
        self.generation_seen
    }

    /// The lapsed hosts to declare dead now, in ONE batch, each with its heir — or `None`
    /// (the module docs). A running cut with a lapsed participant is decided abort here
    /// first (the declaration follows once that decision is recorded). The batch leaves with
    /// the next transition and runs in its transaction; its answer is an input
    /// ([`super::DecisionAnswer::Declared`]).
    ///
    /// The ticker's hand-out (Elastic 2b Task 6): on any other replica, nothing.
    pub fn declare_wanted(&mut self, now_ms: u64) -> Option<Declare> {
        if !self.ticking_now() {
            return None;
        }
        // The lapses are the lease watch's, on its own clock ([`LeaseWatch`]); `now_ms` only
        // moves the core's.
        self.clock_ms = self.clock_ms.max(now_ms);
        let (declaring, ending, known) = (self.declaring(), self.ending().is_some(), self.successions_known());
        let r = &self.successions;
        if !r.enabled || !known || declaring || r.rollback.is_some() || ending || !r.open.is_empty() {
            return None;
        }
        if r.refused_at_read.is_some_and(|at| self.leases.reads() <= at) {
            return None;
        }
        let all_lapsed = self.lapsed_undeclared();
        // A landless lapse is ended, never succeeded (Elastic 2a: with no run open nothing
        // names it — [`Self::take_end_landless`]).
        let lapsed: Vec<(HostId, u64)> = all_lapsed.iter().copied().filter(|(h, _)| self.landed(*h)).collect();
        if lapsed.is_empty() {
            return self.leave_wanted();
        }
        if let Some(run) = self.cuts_running_participants() {
            if let Some((h, _)) = lapsed.iter().find(|(h, _)| run.contains(h)) {
                self.abort_cut_for_lapse(*h);
            }
            return None;
        }
        // No lapsed host inherits, landed or not.
        let gone: BTreeSet<HostId> = all_lapsed.iter().map(|(h, _)| *h).collect();
        let live: BTreeSet<HostId> = self.attached_live().into_iter().filter(|h| !gone.contains(h)).collect();
        if live.is_empty() {
            log::debug!("coordinator: hosts {gone:?} lapsed and no live host is attached: nothing to declare");
            return None;
        }
        let live = self.heir_candidates(live);
        let heirs: Vec<HostId> =
            lapsed.iter().map(|(h, _)| crate::heir_of(*h, &live).expect("a live host other than the predecessor")).collect();
        log::warn!("coordinator: hosts {lapsed:?} lost their lease: declaring them dead, heirs {heirs:?}");
        let generation = self.generation_seen;
        Some(self.hand_out_declaration(Declare { dead: lapsed, heirs, kind: SuccessionKind::Crash, generation }))
    }

    /// Declaration `d` handed out: its mark set (pending until its transition is taken), the
    /// decision into the next transition (`decisions.rs`).
    fn hand_out_declaration(&mut self, d: Declare) -> Declare {
        self.successions.declaring = Some(CallStamp::pending(self.replica));
        self.successions.declared_at_read = self.leases.reads();
        self.push_decision(StoreDecision::Declare(d.clone()));
        d
    }

    /// Test support: the store's answer to the batch [`Self::declare_wanted`] handed out (the
    /// module docs) — an input carrying the declaration's mark (with none in flight it is
    /// ignored).
    #[doc(hidden)]
    pub fn declared(&mut self, outcome: Declared, now_ms: u64) {
        let stamp = self.successions.declaring.unwrap_or(CallStamp::pending(self.replica));
        self.apply(super::Input::Decision(super::DecisionAnswer::Declared { stamp, outcome: Ok(outcome) }), now_ms);
    }

    /// The store's answer to the declaration handed out as `stamp` (the module docs): its
    /// outcome, or its refusal (nothing written: a lock timeout behind a staged part, a store
    /// error) — forgotten, declared again after the runtime's retry wait. An answer with another
    /// stamp changes nothing.
    /// The marks of the declaration, the end and the rollback in flight ([`Self::answer_awaited`]).
    pub(super) fn successions_declaring(&self) -> Option<CallStamp> {
        self.successions.declaring
    }

    pub(super) fn successions_ending(&self) -> Option<(HostId, u64, CallStamp)> {
        self.successions.ending
    }

    pub(super) fn successions_rolling(&self) -> Option<CallStamp> {
        self.successions.rolling
    }

    pub(super) fn declare_answered(&mut self, stamp: CallStamp, outcome: Result<Declared, String>, now_ms: u64) {
        if self.successions.declaring != Some(stamp) {
            // Not this mark's: another call's, or none in flight any more — a rollback cleared
            // it ([`Self::rolled_back`]): its transaction abandoned every open run, this one
            // included. Running it would end in a second rollback at its deadline (the hosts
            // restart for the first one and never answer).
            log::warn!("coordinator: a declaration's answer ({stamp:?}) that no mark here awaits: ignored");
            return;
        }
        self.successions.declaring = None;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                log::debug!("coordinator: the declaration could not be executed ({e}); declared again later");
                return;
            }
        };
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
                    if self.successions.open.contains_key(&s.id) {
                        // A re-read listed it first (its dead row came before this answer):
                        // opened and its predecessor taken out of the view then.
                        continue;
                    }
                    let predecessor = s.predecessor;
                    self.open_run(s, now_ms);
                    self.host_succeeded(predecessor);
                }
            }
            Declared::SuccessionOpen => {
                log::error!("coordinator: another succession is open in the store: the world rolls back");
                self.want_rollback();
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

    /// Test support: a declaration made by hand (not handed out by [`Self::declare_wanted`]) is
    /// in flight — its answer ([`Self::declared`]) is taken.
    #[doc(hidden)]
    pub fn declaring_by_hand(&mut self) {
        self.clean_checked = false;
        self.successions.declaring = Some(CallStamp::pending(self.replica));
        self.successions.declared_at_read = self.leases.reads();
    }

    /// Test support: the batch [`Self::declare_wanted`] handed out could not be executed (a
    /// store error, a lock timeout) — the refusal as an input carrying its mark: forgotten,
    /// asked again after the runtime's retry wait.
    #[doc(hidden)]
    pub fn declare_failed(&mut self) {
        let stamp = self.successions.declaring.unwrap_or(CallStamp::pending(self.replica));
        let now = self.clock_ms;
        let failed = super::DecisionAnswer::Declared { stamp, outcome: Err("test: the declaration failed".into()) };
        self.apply(super::Input::Decision(failed), now);
    }

    /// The store's open successions may have changed behind this coordinator's back (the
    /// module docs): read them again — no cut begins and nothing is declared until
    /// the read answers ([`Self::take_successions_reread`] asks the ticker's runtime). Nothing
    /// under the `Rollback` policy. The runtime's call (a store's answer: outside the log); the
    /// core's own inputs ask [`Self::reread_successions`].
    pub fn successions_reread(&mut self) {
        self.clean_checked = false;
        self.reread_successions();
    }

    /// [`Self::successions_reread`], as an input's (or an answer's) own step: asked in the
    /// projection, so the ticker sees an ask any replica's input made.
    pub(super) fn reread_successions(&mut self) {
        if !self.successions.enabled {
            return;
        }
        if !self.successions.reread {
            log::info!("coordinator: the open successions are read again");
        }
        self.successions.reread = true;
    }

    /// Whether the ticker's runtime is to read the open successions (again) now and answer
    /// [`super::Input::SuccessionsOpen`], issued at this cache's version: asked (once per ask),
    /// or owed (a start, a rebase of the ticker, a new ticker) with no read out. A read already
    /// out is dropped by the runtime (it may predate the ask). Never on a replica that is not the
    /// ticker (Elastic 2b Task 6).
    pub fn take_successions_reread(&mut self) -> bool {
        if !self.ticking_now() || !self.successions.enabled {
            return false;
        }
        let r = &mut self.successions;
        let asked = std::mem::take(&mut r.reread) || (!r.known && !r.reading);
        if asked {
            r.known = false;
            r.reading = true;
            r.closed_while_reading.clear();
            self.clean_checked = false;
        }
        asked
    }

    /// Whether the world must roll back to its last committed cut (the module docs): the
    /// runtime hands the rollback out ([`Self::take_rollback`]); its answer rolls the core back
    /// ([`Self::rolled_back`]).
    pub fn rollback_wanted(&self) -> bool {
        self.successions.rollback.is_some()
    }

    /// The wanted rollback, handed out ONCE (none in flight, none being looked up, not before
    /// [`DECISION_RETRY_MS`] after a failed one): it leaves with the next transition and runs in
    /// its transaction (`decisions.rs`), conditional on the generation it was decided at — that
    /// generation, for the log and the tests. The ticker's hand-out (Elastic 2b Task 6).
    pub fn take_rollback(&mut self) -> Option<u64> {
        if !self.ticking_now() {
            return None;
        }
        let r = &self.successions;
        let from = r.rollback?;
        if r.rolling.is_some() || r.generation_lookup || r.rollback_retry_ms.is_some_and(|at| self.clock_ms < at) {
            return None;
        }
        self.successions.rolling = Some(CallStamp::pending(self.replica));
        self.push_decision(StoreDecision::Rollback { from });
        Some(from)
    }

    /// The store's answer to the rollback handed out as `stamp`: the world's generation after
    /// it ([`Self::rolled_back`]), or its refusal (nothing written) — handed out again after
    /// [`DECISION_RETRY_MS`]. An answer with another stamp changes nothing.
    pub(super) fn rollback_answered(&mut self, stamp: CallStamp, outcome: Result<u64, String>, now_ms: u64) {
        if self.successions.rolling != Some(stamp) {
            log::warn!("coordinator: a rollback's answer ({stamp:?}) that no mark here awaits: ignored");
            return;
        }
        self.successions.rolling = None;
        match outcome {
            Ok(generation) => self.rolled_back_to_generation(generation),
            Err(e) => {
                log::warn!("coordinator: the world rollback could not be recorded ({e}); trying again");
                self.successions.rollback_retry_ms = Some(now_ms + DECISION_RETRY_MS);
            }
        }
    }

    /// A rollback whose answer was lost is to be looked up: the generation it was decided at,
    /// once — read the world generation and answer [`Self::generation_looked_up`]. The ticker's
    /// hand-out (Elastic 2b Task 6).
    pub fn take_generation_lookup(&mut self) -> Option<u64> {
        if !self.ticking_now() || !std::mem::take(&mut self.successions.generation_lookup) {
            return None;
        }
        self.clean_checked = false;
        Some(self.successions.rollback.unwrap_or(self.generation_seen))
    }

    /// The generation read [`Self::take_generation_lookup`] asked for failed: asked for again.
    pub fn generation_lookup_failed(&mut self) {
        if self.successions.rolling.is_none() && self.successions.rollback.is_some() {
            self.clean_checked = false;
            self.successions.generation_lookup = true;
        }
    }

    /// The world generation as the store holds it (a lookup's answer,
    /// [`Self::take_generation_lookup`]): past the one the rollback was decided at, it rolled
    /// back ([`Self::rolled_back`]); otherwise it did not, and the rollback is handed out again.
    /// Outside the log (Elastic 2b Task 6): a rebase before the next commit drops it — the
    /// wanted rollback is then handed out again, conditional on its generation (one that
    /// committed is answered as done, never done twice).
    pub fn generation_looked_up(&mut self, generation: u64) {
        let from = self.successions.rollback.unwrap_or(self.generation_seen);
        if generation > from {
            log::warn!("coordinator: the lost rollback committed: the world is at generation {generation}");
            self.clean_checked = false;
            self.rolled_back_to_generation(generation);
        } else {
            log::info!("coordinator: the lost rollback did not commit (generation {generation}): it is handed out again");
        }
    }

    /// The world generation the wanted rollback was decided at: the store rolls back only from
    /// it, and answers a world already past it as rolled back (a rollback that committed before
    /// this coordinator could write its transition is never done twice).
    pub fn rollback_from(&self) -> Option<u64> {
        self.successions.rollback
    }

    /// A world rollback is wanted from the generation the hosts run now.
    fn want_rollback(&mut self) {
        self.successions.rollback.get_or_insert(self.generation_seen);
    }

    /// The world was rolled back to `generation`: every open succession is abandoned
    /// ([`CoordMsg::SuccessionDone`] unsettled) and every host told
    /// ([`CoordMsg::Generation`]: it exits).
    ///
    /// A world hosts already attach at (`generation` not above the one seen: a rollback that
    /// committed while no coordinator ran, its hosts restarted and attached before this
    /// answer) is not rolled back again here: the flags clear and its runs go (every host is
    /// told each is abandoned, `SuccessionDone` unsettled — a host of the new generation holds
    /// none and ignores it), but no `Generation` is sent and nobody is detached.
    ///
    /// Test support: the answer to the rollback handed out (one is handed out first if none
    /// is), an input carrying its mark; with no rollback wanted, applied directly.
    #[doc(hidden)]
    pub fn rolled_back(&mut self, generation: u64) {
        let stamp = match self.successions.rolling {
            Some(stamp) => Some(stamp),
            None => {
                // Not paced: the test answers at once.
                self.successions.rollback_retry_ms = None;
                self.successions.generation_lookup = false;
                self.take_rollback().and(self.successions.rolling)
            }
        };
        match stamp {
            Some(stamp) => {
                let now = self.clock_ms;
                self.apply(super::Input::Decision(super::DecisionAnswer::RolledBack { stamp, outcome: Ok(generation) }), now);
            }
            None => {
                self.clean_checked = false;
                self.rolled_back_to_generation(generation);
            }
        }
    }

    /// Run `id` was settled or abandoned here: a successions read out now (it may have run
    /// before) does not open it again.
    fn closed_here(&mut self, id: SuccessionId) {
        if self.successions.reading {
            self.successions.closed_while_reading.insert(id);
        }
    }

    /// The world was rolled back to `generation` ([`Self::rolled_back`]).
    fn rolled_back_to_generation(&mut self, generation: u64) {
        let open = std::mem::take(&mut self.successions.open);
        for id in open.into_keys() {
            self.closed_here(id);
            self.push(Target::Every, CoordMsg::SuccessionDone { id, settled: false });
        }
        self.successions.rollback = None;
        self.successions.rolling = None;
        self.successions.generation_lookup = false;
        self.successions.rollback_retry_ms = None;
        // A declaration in flight is abandoned with the runs: its answer is ignored.
        self.successions.declaring = None;
        if generation <= self.generation_seen && self.attached_generation >= generation {
            log::warn!("coordinator: the world is at generation {generation} already and its hosts attach there: nothing more to roll back");
            self.rolled_back_to = self.rolled_back_to.max(Some(generation));
            return;
        }
        log::error!("coordinator: the world rolled back to its last committed cut: generation {generation}");
        // Every host restarts from the cut, its hops with it (as a newer-generation `Attach`
        // says, `handle`: this coordinator raised the generation itself, so no `Attach` is
        // "newer" and nothing else would clear them).
        if generation > self.generation_seen {
            self.hop_of.clear();
            self.online_reported.clear();
            self.displaced_told.clear();
        }
        // A host attaching with an older generation from now on is told this one.
        self.generation_seen = self.generation_seen.max(generation);
        self.rolled_back_to = Some(generation);
        // Every host is told the generation and exits (a restarted process attaches as a new
        // host at the new generation; an old one's `Attach` is answered with the generation):
        // none counts as attached from here — none may be an heir or the gate, and no cut
        // begins — however long its `Generation` takes to leave (Elastic 2a: after its
        // transition is written) or its link to drop.
        self.detach_all();
        self.push(Target::Every, CoordMsg::Generation(generation));
    }

    /// How many successions are open (declared or read back, not yet settled or abandoned).
    pub fn successions_open_count(&self) -> usize {
        self.successions.open.len()
    }

    /// Test support: the open successions as a store holding exactly this core's runs would list
    /// them (what a reloaded copy of this core reads).
    #[doc(hidden)]
    pub fn successions_listed(&self) -> Vec<Succession> {
        self.successions.open.values().map(|r| r.s.clone()).collect()
    }

    /// Test support: every open succession with the survivors that applied it and whether
    /// the heir adopted it.
    #[doc(hidden)]
    pub fn succession_progress(&self) -> Vec<(SuccessionId, Vec<HostId>, bool)> {
        self.successions.open.iter().map(|(id, r)| (*id, r.applied.keys().copied().collect(), r.adopted.is_some())).collect()
    }

    /// Survivor (or heir) `host` applied succession `id` — as the boot its latest `Attach` named.
    pub(super) fn succession_applied(&mut self, host: HostId, id: SuccessionId) {
        let boot = self.attached_boot(host).unwrap_or_default();
        match self.successions.open.get_mut(&id) {
            Some(run) if recipient(&run.s, host) => {
                run.applied.insert(host, boot);
            }
            Some(_) => log::warn!("coordinator: host {host} applied succession {}, which does not name it a survivor", id.0),
            None => log::debug!("coordinator: host {host} applied succession {}, which is not open", id.0),
        }
    }

    /// The heir `host` adopted succession `id` — as the boot its latest `Attach` named.
    pub(super) fn succession_adopted(&mut self, host: HostId, id: SuccessionId) {
        let boot = self.attached_boot(host).unwrap_or_default();
        match self.successions.open.get_mut(&id) {
            Some(run) if run.s.heir == host => run.adopted = Some(boot),
            Some(run) => log::warn!("coordinator: host {host} adopted succession {}, whose heir is {}", id.0, run.s.heir),
            None => log::debug!("coordinator: host {host} adopted succession {}, which is not open", id.0),
        }
    }

    /// An `Attach` of `host` listing the successions it answered (`(id, adopted)`): counted
    /// as answered again; every open one naming it a recipient (a survivor, or the heir) that
    /// it did not list is sent (again). Answers are per boot: an offline run's answers of
    /// another boot of `host` are forgotten (its new boot adopts again); a declared run's
    /// recipient attaching as another boot than the one it counts on lost its journal — no
    /// order goes to that boot and the world rolls back.
    pub(super) fn succession_attach(&mut self, host: HostId, answered: Vec<(SuccessionId, bool)>) {
        let boot = self.attached_boot(host);
        // Listings buffered from another boot of this host are that boot's: dropped.
        self.successions.listed_early.retain(|(h, b, _, _)| *h != host || Some(*b) == boot);
        let mut lost = BTreeSet::new();
        for run in self.successions.open.values_mut().filter(|r| recipient(&r.s, host)) {
            let listed = answered.iter().any(|(id, _)| *id == run.s.id);
            if run.s.survivors.is_empty() {
                if !listed && run.applied.get(&host).is_some_and(|b| Some(*b) != boot) {
                    log::warn!(
                        "coordinator: succession {}: host {host} attaches as another boot than the one that answered: \
                         its answers are forgotten, the order goes to the new boot",
                        run.s.id.0
                    );
                    run.applied.remove(&host);
                    if run.s.heir == host {
                        run.adopted = None;
                        // The deadline starts again with the new boot's order (`order_sent`,
                        // below): it counted from the old boot's, long past by now.
                        run.deadline_ms = u64::MAX;
                    }
                }
            } else if let Some(b) = boot {
                match run.boots.get(&host) {
                    Some(known) if *known != b => {
                        log::error!(
                            "coordinator: succession {}: survivor {host} attaches as boot {b:#x}, not {known:#x} — its journal \
                             is gone: the world rolls back",
                            run.s.id.0
                        );
                        lost.insert(run.s.id);
                    }
                    Some(_) => {}
                    None => {
                        run.boots.insert(host, b);
                    }
                }
            }
        }
        if !lost.is_empty() {
            self.want_rollback();
        }
        for (id, adopted) in answered {
            if !self.successions.open.contains_key(&id) {
                if self.successions_known() {
                    self.answer_closed(host, id);
                } else {
                    self.successions.listed_early.push((host, boot.unwrap_or_default(), id, adopted));
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
            .filter(|r| recipient(&r.s, host) && r.applied.get(&host).copied() != boot && !lost.contains(&r.s.id))
            .map(|r| r.s.clone())
            .collect();
        for s in orders {
            self.order_sent(&s, host);
            self.push(Target::Host(host), order(&s));
        }
    }

    /// The upkeep: a run past its deadline without every answer, or a lapse while one is
    /// open, asks for a rollback. While only offline runs are open, a lapse counts only for a
    /// boot seen beating since they opened ([`Run::beat`]).
    pub(super) fn maintain_successions(&mut self, now_ms: u64) {
        let r = &self.successions;
        if !r.enabled || r.rollback.is_some() || r.open.is_empty() {
            return;
        }
        let current = |h: HostId| self.attached_boot(h);
        let late: Vec<(SuccessionId, Vec<HostId>, bool)> = r
            .open
            .values()
            .filter(|run| !run.answered(current) && now_ms >= run.deadline_ms)
            .map(|run| {
                let silent = run.s.survivors.iter().copied().filter(|h| run.applied.get(h).copied() != current(*h)).collect();
                (run.s.id, silent, run.adopted.is_some() && run.adopted == current(run.s.heir))
            })
            .collect();
        if let Some((id, silent, adopted)) = late.first() {
            log::error!(
                "coordinator: succession {} not answered within {} ms (survivors {silent:?} silent, \
                 adopted: {adopted}): the world rolls back",
                id.0,
                self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS)
            );
            self.want_rollback();
            return;
        }
        let open = &self.successions.open;
        // A vouched row (`seen_ms == 0`: `worldctl retire-host` beside live hosts, or a boot
        // of that ordinal) is a statement made NOW, whatever the run: it counts for offline
        // runs too — else that host's dirty row blocks every cut (so the run never settles)
        // while the open run blocks its declaration, with no exit but the operator's rollback.
        let vouched = |d: &(HostId, u64)| self.leases.row(d.0).is_some_and(|r| r.boot == d.1 && r.seen_ms == 0);
        let counts =
            |d: &(HostId, u64)| vouched(d) || open.values().any(|run| !run.s.survivors.is_empty() || run.beat.contains(d));
        // Elastic 2a: a landless host no open run names is ended ([`Self::take_end_landless`]),
        // not a second failure; a landed one, or a recipient, still is (the residual: a host
        // that won land after the declaration and dies before the settle rolls back).
        let ends = |d: &(HostId, u64)| !self.landed(d.0) && !self.is_recipient(d.0);
        let lapsed: Vec<(HostId, u64)> =
            self.lapsed_undeclared().into_iter().filter(|d| counts(d) && !ends(d)).collect();
        if !lapsed.is_empty() {
            log::error!("coordinator: hosts {lapsed:?} lost their lease while a succession is open: the world rolls back");
            self.want_rollback();
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
        let current = |h: HostId| self.attached_boot(h);
        let waiting: Vec<SuccessionId> = r.open.values().filter(|run| !run.answered(current)).map(|run| run.s.id).collect();
        (!self.successions_known() || self.declaring() || r.rollback.is_some() || !waiting.is_empty()).then_some(waiting)
    }

    /// Whether an open succession is answered by everyone and nothing else holds cuts back: a
    /// cut is due at once to settle it (`maintain_cuts`, [`super::SETTLE_CUT_RETRY_MS`]).
    pub(super) fn settle_due(&self) -> bool {
        let r = &self.successions;
        r.enabled && !r.open.is_empty() && self.successions_block_cuts().is_none()
    }

    /// Those of `waiting` (open, unanswered) that were opened while no host ran and whose
    /// heir has not been sent the order yet — it never attached: no deadline runs for them.
    pub(super) fn successions_awaiting_heir(&self, waiting: &[SuccessionId]) -> Vec<(SuccessionId, HostId)> {
        waiting
            .iter()
            .filter_map(|id| self.successions.open.get(id))
            .filter(|run| run.s.survivors.is_empty() && run.deadline_ms == u64::MAX)
            .map(|run| (run.s.id, run.s.heir))
            .collect()
    }

    /// The open successions a cut beginning now settles if it commits (every one answered:
    /// no cut begins otherwise). Pairing: the heir answered `SuccessionAdopted`, so its part
    /// of that cut carries `WorldCut::adopts` (Task 9) — the store does not check it.
    pub(super) fn successions_to_settle(&self) -> Vec<SuccessionId> {
        self.successions.open.keys().copied().collect()
    }

    /// A cut naming `settles` committed: those successions are settled, their recipients told.
    pub(super) fn successions_settled(&mut self, settles: &[SuccessionId]) {
        for id in settles {
            let Some(run) = self.successions.open.remove(id) else { continue };
            self.closed_here(*id);
            log::info!("coordinator: succession {} (host {} → heir {}) settled", id.0, run.s.predecessor, run.s.heir);
            for h in recipients(&run.s) {
                self.push(Target::Host(h), CoordMsg::SuccessionDone { id: *id, settled: true });
            }
        }
    }

    /// The lapsed rows by every read observed ([`LeaseWatch::lapsed_by_reads`]: the watch's
    /// own clock), `(ordinal, boot)`, but those already declared.
    fn lapsed_undeclared(&self) -> Vec<(HostId, u64)> {
        let declared = &self.successions.declared;
        self.leases.lapsed_by_reads().into_iter().filter(|d| !declared.contains(d)).collect()
    }

    /// Run `s` from `now_ms`: its orders go to the recipients attached now.
    fn open_run(&mut self, s: Succession, now_ms: u64) {
        if self.successions.open.contains_key(&s.id) {
            // Never re-opened: its answers are reset only by a boot change.
            return;
        }
        self.successions.declared.insert((s.predecessor, s.predecessor_boot));
        self.note_succeeded(s.predecessor, s.predecessor_boot);
        let attached: Vec<HostId> = recipients(&s).into_iter().filter(|h| self.is_attached(*h)).collect();
        let deadline = self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS);
        // An offline run (no survivors) waits for its heir to boot: its deadline starts with
        // the heir's order.
        let deadline_ms = if s.survivors.is_empty() { u64::MAX } else { now_ms + deadline };
        // A declared run counts on each recipient's boot of now (one not attached: its first
        // `Attach`, [`Self::succession_attach`]).
        let boots: BTreeMap<HostId, u64> = if s.survivors.is_empty() {
            BTreeMap::new()
        } else {
            // Only an ATTACHED host's boot: a detached one's last boot may be stale (its next
            // boot registered but has not attached); it is recorded at its first `Attach`.
            recipients(&s).into_iter().filter(|h| self.is_attached(*h)).filter_map(|h| self.attached_boot(h).map(|b| (h, b))).collect()
        };
        let run =
            Run { s: s.clone(), applied: BTreeMap::new(), adopted: None, deadline_ms, beat: BTreeSet::new(), boots, opened_at: self.version };
        self.successions.open.insert(run.s.id, run);
        for h in attached {
            self.order_sent(&s, h);
            self.push(Target::Host(h), order(&s));
        }
    }

    /// Succession `s`'s order goes to `host` now: an offline run's deadline starts at its
    /// heir's first order.
    fn order_sent(&mut self, s: &Succession, host: HostId) {
        let deadline = self.successions.deadline_ms.unwrap_or(SUCCESSION_DEADLINE_MS);
        let now = self.clock_ms;
        if let Some(run) = self.successions.open.get_mut(&s.id).filter(|r| r.deadline_ms == u64::MAX && r.s.heir == host) {
            run.deadline_ms = now + deadline;
        }
    }

    /// Host `host` was succeeded: what an `Attach` of it with nothing running and nobody
    /// online does — its history-live regions park (the heir's regions start them again),
    /// its online players (and those in transit to it) go offline, and what the
    /// coordinator knew of it (`region_host`, `online_on`) and still meant to send it (its
    /// directives and claim answers) is dropped.
    pub(super) fn host_succeeded(&mut self, host: HostId) {
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
        }
        log::warn!("coordinator: host {host} succeeded: {} region(s) parked, {} player(s) offline", parked.len(), gone.len());
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
    /// Host `host` needs a region of `sr`, whose owner left cleanly
    /// ([`crate::HostMsg::LandWanted`]): looked up by the runtime
    /// ([`Self::take_land_lookups`]). Ignored unless successions run (`Static` never gives
    /// land away: its leaver gets it back when it returns, R3).
    pub(super) fn land_asked(&mut self, host: HostId, sr: crate::SuperRegion) {
        if !self.successions.enabled {
            log::debug!("coordinator: host {host} wants super-region {sr:?}, but successions are off; ignored");
            return;
        }
        self.successions.land_lookups.insert(sr);
    }

    /// The super-regions hosts asked for since the last call (on any replica: queued in the
    /// projection): the ticker's runtime reads their owners in the store and answers
    /// [`super::Input::LandOwners`]. Nothing on a replica that is not the ticker (Elastic 2b Task
    /// 6; a rebase before the next commit hands them out again — the hosts ask again anyway).
    pub fn take_land_lookups(&mut self) -> Vec<crate::SuperRegion> {
        if !self.ticking_now() {
            return Vec::new();
        }
        let lookups: Vec<crate::SuperRegion> = std::mem::take(&mut self.successions.land_lookups).into_iter().collect();
        if !lookups.is_empty() {
            self.clean_checked = false;
        }
        lookups
    }

    /// Test support: [`super::Input::LandOwners`] naming `owner`.
    #[doc(hidden)]
    pub fn land_wanted(&mut self, owner: HostId, now_ms: u64) {
        self.apply(super::Input::LandOwners { owners: vec![owner] }, now_ms);
    }

    /// Host `owner` holds a super-region a host asked for (the ticker's lookup,
    /// [`super::Input::LandOwners`]): if it left cleanly, ALL its land goes to one heir at the
    /// next [`Self::declare_wanted`] between cuts — at once: a host that left never returns as
    /// itself (the module docs).
    pub(super) fn on_land_owner(&mut self, owner: HostId, now_ms: u64) {
        if !self.successions.enabled {
            return;
        }
        self.clock_ms = self.clock_ms.max(now_ms);
        if self.attached_live().contains(&owner) {
            // Running: nothing of it to give away (a stale directory row of the asker).
            return;
        }
        if self.leases.row(owner).is_some_and(|r| self.successions.declared.contains(&(owner, r.boot))) {
            // Its land was given away already (a lookup answered after the declaration).
            return;
        }
        if self.successions.land_wanted.insert(owner) {
            log::info!("coordinator: host {owner} left and its land is wanted: it goes to an heir");
        }
    }

    /// Test support: the departed hosts whose land is wanted now (requests not forgotten).
    #[doc(hidden)]
    pub fn lands_wanted(&self) -> Vec<HostId> {
        self.successions.land_wanted.iter().copied().collect()
    }

    /// The hosts an heir is picked among, of `live` (attached, live, no predecessor of the
    /// declaration), tried in order (Elastic 2a): those holding land that take logins — neither
    /// draining nor leaving ([`Self::takes_logins`]) — then those that take logins, then all of
    /// `live` (no land is left without an heir). A landed heir is a survivor anyway; a host
    /// about to go adopts no land it would hand on minutes later.
    fn heir_candidates(&self, live: BTreeSet<HostId>) -> BTreeSet<HostId> {
        let staying: BTreeSet<HostId> = live.iter().copied().filter(|h| self.takes_logins(*h)).collect();
        // Ranked on evidence: a row the latest read did not show (registered since) is not landed.
        let landed: BTreeSet<HostId> =
            staying.iter().copied().filter(|h| self.leases.row(*h).is_some_and(|r| r.landed)).collect();
        if !landed.is_empty() {
            landed
        } else if !staying.is_empty() {
            staying
        } else {
            live
        }
    }

    /// The `Leave` succession to declare now for drained land, if any (called by
    /// [`Self::declare_wanted`] when no host lapsed): a wanted owner whose `hosts` row the
    /// latest read shows clean and live, not declared yet, with a live attached heir, between
    /// cuts. A request is dropped once that row is no longer clean and live, or its id is
    /// attached and live (neither happens to a host that left: its id never registers again —
    /// kept as a guard); a row not read (yet) is waited for.
    fn leave_wanted(&mut self) -> Option<Declare> {
        if self.successions.land_wanted.is_empty() {
            return None;
        }
        let attached: BTreeSet<HostId> = self.attached_live().into_iter().collect();
        let (leases, declared) = (&self.leases, &self.successions.declared);
        self.successions.land_wanted.retain(|owner| {
            let row = leases.row(*owner);
            // Declared already (an owners lookup answered just after the declaration): done.
            if row.is_some_and(|r| declared.contains(&(*owner, r.boot))) {
                return false;
            }
            let back = attached.contains(owner) || row.is_some_and(|r| !r.clean || r.state != HOST_LIVE);
            if back {
                log::info!("coordinator: host {owner} is attached or its row is no longer a clean live one: its land is not given away");
            }
            !back
        });
        let (owner, boot) =
            self.successions.land_wanted.iter().find_map(|&owner| self.leases.row(owner).map(|row| (owner, row.boot)))?;
        if self.cuts_running_participants().is_some() {
            // Only between cuts: the store's base cut is the one every survivor counts from.
            return None;
        }
        let live = self.heir_candidates(attached.into_iter().filter(|h| *h != owner).collect());
        let Some(heir) = crate::heir_of(owner, &live) else {
            log::debug!("coordinator: host {owner}'s land is wanted and no live host is attached to inherit it");
            return None;
        };
        log::warn!("coordinator: host {owner} (boot {boot:#x}) left and its land is wanted: giving it all to heir {heir}");
        let generation = self.generation_seen;
        Some(self.hand_out_declaration(Declare { dead: vec![(owner, boot)], heirs: vec![heir], kind: SuccessionKind::Leave, generation }))
    }

    /// Test support: the lease part of a [`super::Input::HostsRead`] (`rows`, issued at
    /// `issued_ms` and received at `received_ms` on the monotonic clock), as a read whose next
    /// id is unknown (every absent id was pruned): the lease watch, the drain marks, the ids
    /// forgotten, the dead rows nobody here declared. Refused on a replica that is not the
    /// ticker.
    pub fn observe_leases(&mut self, rows: &[HostSeen], issued_ms: u64, received_ms: u64) {
        if self.ticking_now() {
            self.clean_checked = false;
            self.observe_leases_inner(rows, u64::MAX, issued_ms, received_ms);
        }
    }

    /// The ticker's read of every `hosts` row ([`super::Input::HostsRead`]): the boots a
    /// departed host came back as, the hosts a cut needs (the dirty live rows: a row marked dead
    /// runs nothing — `decide_in` reads the same predicate), the world tick, and the leases
    /// ([`Self::observe_leases`]).
    pub(super) fn on_hosts_read(&mut self, rows: &[HostSeen], next_host_id: u64, issued_mono: u64, received_mono: u64, world_tick: crate::Tick) {
        let boots: BTreeMap<HostId, u64> = rows.iter().map(|r| (r.ordinal, r.boot)).collect();
        self.set_host_boots_inner(&boots);
        let expected: BTreeSet<HostId> = rows.iter().filter(|r| !r.clean && r.state == HOST_LIVE).map(|r| r.ordinal).collect();
        self.set_expected_hosts(expected);
        self.set_world_tick(world_tick);
        self.observe_leases_inner(rows, next_host_id, issued_mono, received_mono);
    }

    /// A successful read of the `hosts` rows, issued at `issued_ms` and received at
    /// `received_ms` — the process's MONOTONIC clock, never the store's ([`LeaseWatch`]) — when
    /// the store's next host id was `next_host_id`. The drain marks it shows are the gate's. A
    /// dirty dead row this coordinator never declared (an end or a declaration whose answer a
    /// dead process never wrote, a total crash's registration) is taken out of the view at once
    /// ([`Self::host_ended`]) and asks, once, for the open successions to be read again. What it
    /// changes in the projection is outside the log (Elastic 2b Task 6: a rebase drops it, the
    /// next read derives it again — the dead rows' asks are in the projection, so the rebased
    /// core asks again).
    fn observe_leases_inner(&mut self, rows: &[HostSeen], next_host_id: u64, issued_ms: u64, received_ms: u64) {
        // Beats since the last read (a boot's `seen_ms` moved): what an offline run counts.
        let beating: Vec<(HostId, u64)> = rows
            .iter()
            .filter(|r| r.seen_ms != 0 && self.leases.row(r.ordinal).is_some_and(|w| w.boot == r.boot && w.seen_ms != r.seen_ms))
            .map(|r| (r.ordinal, r.boot))
            .collect();
        for run in self.successions.open.values_mut().filter(|run| run.s.survivors.is_empty()) {
            run.beat.extend(beating.iter().copied());
        }
        self.leases.observe(rows, issued_ms, received_ms);
        let draining: BTreeSet<HostId> = rows.iter().filter(|r| r.draining).map(|r| r.ordinal).collect();
        if draining != self.draining {
            self.draining = draining;
        }
        // An id absent from the read was pruned (a clean or dead row owning nothing, named by no
        // open succession): nothing kept of it any more (B-M5) — an id is never issued again.
        // Not an id the read could not show (registered after it began, audit H7).
        let present: BTreeSet<HostId> = rows.iter().map(|r| r.ordinal).collect();
        self.forget_absent_hosts(&present, next_host_id);
        let keep = |h: &HostId| present.contains(h) || h.0 >= next_host_id;
        self.successions.declared.retain(|(h, _)| keep(h));
        self.successions.reread_for.retain(|(h, _)| keep(h));
        // Before the open successions are known, their pending read lists it anyway.
        // A declaration in flight explains a dead row its answer will open.
        if !self.successions.enabled || !self.successions_known() || self.declaring() {
            return;
        }
        let unknown: Vec<(HostId, u64)> = rows
            .iter()
            .filter(|r| !r.clean && r.state != HOST_LIVE)
            .map(|r| (r.ordinal, r.boot))
            .filter(|d| !self.successions.declared.contains(d) && !self.successions.reread_for.contains(d))
            .collect();
        if !unknown.is_empty() {
            log::warn!("coordinator: hosts {unknown:?} are marked dead and this coordinator declared none of them");
            // Dead in the store: whatever ended it (an end or a succession whose answer a dead
            // process never wrote, a total crash's registration), it runs nothing any more —
            // taken out of the view now (Elastic 2a final review, C1). A run the re-read finds
            // for it takes it out again: idempotent.
            for &(host, boot) in &unknown {
                self.host_ended(host, boot);
            }
            self.successions.reread_for.extend(unknown);
            self.reread_successions();
        }
    }

    /// A failed read of the `hosts` rows ([`LeaseWatch::observe_failed`]).
    pub fn leases_unread(&mut self) {
        self.leases.observe_failed();
    }

    /// The lease watch (Task 8 declares successions from [`LeaseWatch::lapsed`]).
    pub fn leases(&self) -> &LeaseWatch {
        &self.leases
    }

    /// Whether the latest successful hosts read showed host `host` holding land (a row not read:
    /// `true` — nothing is ended on no evidence).
    fn landed(&self, host: HostId) -> bool {
        self.leases.row(host).is_none_or(|r| r.landed)
    }

    /// Test support: which of the per-host memories still hold host `host` (by name: `left`,
    /// `succeeded`, `boots`, `declared`, `reread_for`) — empty once a read no longer shows it.
    #[doc(hidden)]
    pub fn host_mentions(&self, host: HostId) -> Vec<&'static str> {
        let mut out = self.cut_mentions(host);
        if self.successions.declared.iter().any(|(h, _)| *h == host) {
            out.push("declared");
        }
        if self.successions.reread_for.iter().any(|(h, _)| *h == host) {
            out.push("reread_for");
        }
        out
    }

    /// Whether an open run names host `host` heir or survivor.
    fn is_recipient(&self, host: HostId) -> bool {
        self.successions.open.values().any(|run| recipient(&run.s, host))
    }

    /// The crash-loop rule (Elastic 2a): a lapsed host that holds no land and that no open run
    /// names (heir or survivor) is handed to the runtime to END — its row marked dead in the
    /// store (`end_landless_host`), no succession, no generation change — also while a run is
    /// open, and with none open instead of a succession. One at a time, only between cuts (a
    /// running cut it takes part in is decided abort first), never while a declaration or a
    /// rollback is under way, and not again on the read the store refused it at. The runtime
    /// answers [`Self::landless_ended`] (or [`Self::end_failed`]). The ticker's hand-out (Elastic
    /// 2b Task 6).
    pub fn take_end_landless(&mut self, now_ms: u64) -> Option<EndLandless> {
        if !self.ticking_now() {
            return None;
        }
        // As [`Self::declare_wanted`]: the lapses are the lease watch's own.
        self.clock_ms = self.clock_ms.max(now_ms);
        let (declaring, ending, known) = (self.declaring(), self.ending().is_some(), self.successions_known());
        let r = &self.successions;
        if !r.enabled || !known || declaring || r.rollback.is_some() || ending {
            return None;
        }
        if r.end_refused_at_read.is_some_and(|at| self.leases.reads() <= at) {
            return None;
        }
        let (host, boot) = self
            .lapsed_undeclared()
            .into_iter()
            .find(|(h, _)| !self.landed(*h) && !self.is_recipient(*h))?;
        if self.cuts_running_participants().is_some_and(|run| run.contains(&host)) {
            self.abort_cut_for_lapse(host);
            return None;
        }
        if self.cuts_running_participants().is_some() {
            // Between cuts only: the running cut is decided before the row turns dead.
            return None;
        }
        log::warn!("coordinator: host {host} (boot {boot:#x}) lost its lease holding no land: ending it (no succession)");
        self.successions.ending = Some((host, boot, CallStamp::pending(self.replica)));
        self.successions.ending_at_read = self.leases.reads();
        let end = EndLandless { host, boot };
        self.push_decision(StoreDecision::EndLandless(end));
        Some(end)
    }

    /// Test support: the store's answer to the end [`Self::take_end_landless`] handed out — an
    /// input carrying its mark (with no such end in flight it is ignored).
    #[doc(hidden)]
    pub fn landless_ended(&mut self, end: EndLandless, outcome: crate::LandlessEnd, now_ms: u64) {
        let stamp = self.successions.ending.map_or(CallStamp::pending(self.replica), |(_, _, s)| s);
        self.apply(super::Input::Decision(super::DecisionAnswer::Ended { stamp, end, outcome: Ok(outcome) }), now_ms);
    }

    /// The store's answer to the end handed out as `stamp`: `Ended` takes the host out of the
    /// view ([`Self::host_ended`]); `OwnsLand` (it won land after the read), `NotLapsed` and
    /// `Gone` wait for a newer read, which decides again; `Recipient` (an open run names it) is
    /// a second failure before the settle — a rollback while a run is open here, a re-read of
    /// the open successions otherwise (the store knows one this coordinator does not). A
    /// refusal (nothing written: a lock timeout, a store error) is forgotten: the runtime asks
    /// again after its retry wait. An answer with another stamp changes nothing.
    pub(super) fn end_answered(&mut self, stamp: CallStamp, end: EndLandless, outcome: Result<crate::LandlessEnd, String>, _now_ms: u64) {
        if self.successions.ending != Some((end.host, end.boot, stamp)) {
            log::warn!("coordinator: an answer to ending host {} ({stamp:?}) that no mark here awaits: ignored", end.host);
            return;
        }
        self.successions.ending = None;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                log::debug!("coordinator: host {} could not be ended ({e}); ended again later", end.host);
                return;
            }
        };
        match outcome {
            crate::LandlessEnd::Ended => self.host_ended(end.host, end.boot),
            crate::LandlessEnd::Recipient => {
                if self.successions.open.is_empty() {
                    log::warn!("coordinator: host {} is named by a succession this coordinator does not know: re-read", end.host);
                    self.reread_successions();
                } else {
                    log::error!(
                        "coordinator: host {} lost its lease and an open succession names it: the world rolls back",
                        end.host
                    );
                    self.want_rollback();
                }
                self.successions.end_refused_at_read = Some(self.successions.ending_at_read);
            }
            other => {
                log::info!("coordinator: host {} was not ended ({other:?}): decided again at a newer read", end.host);
                self.successions.end_refused_at_read = Some(self.successions.ending_at_read);
            }
        }
    }

    /// Test support: the end [`Self::take_end_landless`] handed out could not be executed (a
    /// store error, a lock timeout) — the refusal as an input carrying its mark.
    #[doc(hidden)]
    pub fn end_failed(&mut self) {
        let Some((host, boot, stamp)) = self.successions.ending else { return };
        let now = self.clock_ms;
        let end = EndLandless { host, boot };
        let failed = super::DecisionAnswer::Ended { stamp, end, outcome: Err("test: the end failed".into()) };
        self.apply(super::Input::Decision(failed), now);
    }

    /// Host `host`'s boot `boot` was ended (its landless row is dead): never lapsed or ended
    /// again, not expected by cuts nor counted attached (a zombie's messages are dropped, as a
    /// succeeded boot's), and taken out of the view as a predecessor is.
    fn host_ended(&mut self, host: HostId, boot: u64) {
        self.successions.declared.insert((host, boot));
        self.note_succeeded(host, boot);
        self.host_succeeded(host);
    }
}
