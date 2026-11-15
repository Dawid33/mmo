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
//! - The orders go to the survivors AND the heir (the heir is a survivor whenever the
//!   coordinator declared: it was live). A succession `worldctl retire-host` opened while no
//!   host ran has no survivors: its heir booted after it, has nothing to report or resolve and
//!   adopts from the store alone; its answers are all a cut waits for, and its
//!   `SuccessionDone` goes to it too.
//! - **Answers are per boot.** An answer counts only while the boot that gave it is the
//!   host's current one (its latest `Attach`): no cut names a run until the heir's CURRENT boot
//!   adopted it and every survivor's current boot applied it. An offline run's heir that boots
//!   again (it adopted, then died before the settle) has its answers forgotten at its new
//!   boot's `Attach` and gets the order again — it adopts again from the store. A declared
//!   run's recipient that attaches as another boot than the one the run counts on (its boot
//!   when the run opened) lost its journal: no order goes to that boot and the world rolls
//!   back. (`register_host` normally keeps such a boot out until then: a dirty reboot beside a
//!   live host vouches for its old row and waits — that row lapses, a second death, a
//!   rollback; with nobody alive it raises the generation, which fences the old heir's parts.)
//! - A RUNNING coordinator reads the open successions again — on these three triggers only,
//!   never periodically —
//!   ([`Coordinator::take_successions_reread`]) when the store changed them behind its back:
//!   a `hosts` read shows a dead row it never declared (an offline `retire-host`), or a host
//!   attaches at a newer generation, or (the runtime, [`Coordinator::successions_reread`]) a
//!   decision is refused at a stale generation (`worldctl rollback`). Until the answer no
//!   cut begins and nothing is declared; then a new one runs as at a restart (its predecessor
//!   taken out of the view), and a run no longer open is dropped as abandoned (every host
//!   told `SuccessionDone { settled: false }`).
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
use crate::HostId;
use crate::{
    CrashPolicy, Declared, Succession, SuccessionId, SuccessionKind, HEIR_WAIT_MS, HOST_LEASE_MS, HOST_LIVE, SUCCESSION_DEADLINE_MS,
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

    /// Host `ordinal`'s row as the latest successful read showed it.
    pub fn row(&self, ordinal: HostId) -> Option<&HostSeen> {
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

/// A declaration of successions for the runtime to execute in the store
/// ([`Coordinator::declare_wanted`]): `dead[i]` (ordinal, boot) to heir `heirs[i]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declare {
    pub dead: Vec<(HostId, u64)>,
    pub heirs: Vec<HostId>,
    pub kind: SuccessionKind,
}

/// One open succession and its answers.
#[derive(Clone, Debug)]
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
    declared: BTreeSet<(HostId, u64)>,
    /// [`LeaseWatch::reads`] when the declaration in flight was handed out.
    declared_at_read: u64,
    /// The deadline of a run, ms: [`SUCCESSION_DEADLINE_MS`] unless a real-time test changed
    /// it ([`Coordinator::set_lease_dials`]).
    deadline_ms: Option<u64>,
    /// The store refused the last declaration (`NotLapsed` / `Gone`) as of this read: no
    /// declaration until a newer read (the same observation would hand out the same batch
    /// on every runtime pass).
    refused_at_read: Option<u64>,
    /// Ids an `Attach` listed before the open successions were known, with the listing boot:
    /// answered then — only if that boot is still the host's current one.
    listed_early: Vec<(HostId, u64, SuccessionId, bool)>,
    /// The open successions are to be read again ([`Coordinator::take_successions_reread`]).
    reread: bool,
    /// The read asked for is a RE-read (not a start's): runs it does not list are dropped.
    rereading: bool,
    /// Dead rows nobody here declared that already asked for a re-read (asked once each).
    reread_for: BTreeSet<(HostId, u64)>,
    /// Super-regions hosts asked for ([`crate::HostMsg::LandWanted`]) that the runtime is to
    /// look up in the store ([`Coordinator::take_land_lookups`]).
    land_lookups: BTreeSet<crate::SuperRegion>,
    /// Hosts that left cleanly whose land is wanted ([`Coordinator::land_wanted`]), each with
    /// the `clock_ms` of its FIRST request (asking again does not restart the wait).
    land_wanted: BTreeMap<HostId, u64>,
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
            reread: false,
            rereading: false,
            reread_for: BTreeSet::new(),
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

    /// The store's open successions (a coordinator starting, `open_successions()`, or a
    /// re-read): each is run again — its order re-sent to every recipient as it attaches (they
    /// answer from memory), its deadline counted from `now_ms`. After a re-read a run it does
    /// not list is dropped (the store abandoned it: every host told), and a new one's
    /// predecessor is taken out of the view as a declaration's is.
    pub fn successions_open(&mut self, list: Vec<Succession>, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.successions.known = true;
        let rereading = std::mem::take(&mut self.successions.rereading);
        // Dropping an unlisted run below relies on the store thread answering in the order it
        // was asked: a declaration handed out before this read was asked is answered before
        // it, so a run that declaration opened is in `open` AND in the list. (The runtime's
        // succession pump asks for no declaration while a read is out.)
        if rereading {
            let listed: BTreeSet<SuccessionId> = list.iter().map(|s| s.id).collect();
            // A run a commit decided here settles is not listed either once the store holds the
            // commit: its decision's answer settles it ([`Self::successions_settled`]).
            let settling: BTreeSet<SuccessionId> = self.settles_pending().into_iter().collect();
            let gone: Vec<SuccessionId> =
                self.successions.open.keys().copied().filter(|id| !listed.contains(id) && !settling.contains(id)).collect();
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
        }
        for s in list {
            if self.successions.open.contains_key(&s.id) {
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
            if rereading {
                self.host_succeeded(predecessor);
            }
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
        let gone: BTreeSet<HostId> = lapsed.iter().map(|(h, _)| *h).collect();
        let live: BTreeSet<HostId> = self.attached_live().into_iter().filter(|h| !gone.contains(h)).collect();
        if live.is_empty() {
            log::debug!("coordinator: hosts {gone:?} lapsed and no live host is attached: nothing to declare");
            return None;
        }
        let heirs: Vec<HostId> =
            lapsed.iter().map(|(h, _)| crate::heir_of(*h, &live).expect("a live host other than the predecessor")).collect();
        log::warn!("coordinator: hosts {lapsed:?} lost their lease: declaring them dead, heirs {heirs:?}");
        self.successions.declaring = true;
        self.successions.declared_at_read = self.leases.reads();
        Some(Declare { dead: lapsed, heirs, kind: SuccessionKind::Crash })
    }

    /// The store's answer to the batch [`Self::declare_wanted`] handed out (the module docs).
    pub fn declared(&mut self, outcome: Declared, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        if !self.successions.declaring {
            // No declaration is in flight any more: a rollback cleared it ([`Self::rolled_back`])
            // while the runtime still held the store's answer — its transaction abandoned every
            // open run, this one included. Running it would end in a second rollback at its
            // deadline (the hosts restart for the first one and never answer).
            log::warn!("coordinator: a declaration's answer came with none in flight (rolled back meanwhile): ignored");
            return;
        }
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

    /// Test support: a declaration made by hand (not handed out by [`Self::declare_wanted`]) is
    /// in flight — its answer ([`Self::declared`]) is taken.
    #[doc(hidden)]
    pub fn declaring_by_hand(&mut self) {
        self.successions.declaring = true;
        self.successions.declared_at_read = self.leases.reads();
    }

    /// The batch [`Self::declare_wanted`] handed out could not be executed (a store error, a
    /// lock timeout): forgotten; the runtime asks again after its retry wait.
    pub fn declare_failed(&mut self) {
        self.successions.declaring = false;
    }

    /// The store's open successions may have changed behind this coordinator's back (the
    /// module docs): read them again — no cut begins and nothing is declared until
    /// [`Self::successions_open`] answers ([`Self::take_successions_reread`] asks the runtime).
    /// Nothing under the `Rollback` policy.
    pub fn successions_reread(&mut self) {
        if !self.successions.enabled {
            return;
        }
        if !self.successions.reread {
            log::info!("coordinator: the open successions are read again");
        }
        // A start's first read not answered yet holds no run to drop: it stays a start's.
        self.successions.rereading |= self.successions.known;
        self.successions.known = false;
        self.successions.reread = true;
    }

    /// Whether the runtime is to read the open successions again (once per ask) and answer
    /// [`Self::successions_open`].
    pub fn take_successions_reread(&mut self) -> bool {
        std::mem::take(&mut self.successions.reread)
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
        self.push(Target::Every, CoordMsg::Generation(generation));
    }

    /// How many successions are open (declared or read back, not yet settled or abandoned).
    pub fn successions_open_count(&self) -> usize {
        self.successions.open.len()
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
            self.successions.rollback = true;
        }
        for (id, adopted) in answered {
            if !self.successions.open.contains_key(&id) {
                if self.successions.known {
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
        if !r.enabled || r.rollback || r.open.is_empty() {
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
            self.successions.rollback = true;
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
        let lapsed: Vec<(HostId, u64)> = self.lapsed_undeclared(now_ms).into_iter().filter(|d| counts(d)).collect();
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
        let current = |h: HostId| self.attached_boot(h);
        let waiting: Vec<SuccessionId> = r.open.values().filter(|run| !run.answered(current)).map(|run| run.s.id).collect();
        (!r.known || r.declaring || r.rollback || !waiting.is_empty()).then_some(waiting)
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
            log::info!("coordinator: succession {} (host {} → heir {}) settled", id.0, run.s.predecessor, run.s.heir);
            for h in recipients(&run.s) {
                self.push(Target::Host(h), CoordMsg::SuccessionDone { id: *id, settled: true });
            }
        }
    }

    /// The lapsed rows, `(ordinal, boot)`, but those already declared.
    fn lapsed_undeclared(&self, now_ms: u64) -> Vec<(HostId, u64)> {
        let declared = &self.successions.declared;
        self.leases.lapsed(now_ms).into_iter().filter(|d| !declared.contains(d)).collect()
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
        let run = Run { s: s.clone(), applied: BTreeMap::new(), adopted: None, deadline_ms, beat: BTreeSet::new(), boots };
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
    fn host_succeeded(&mut self, host: HostId) {
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
    pub(super) fn land_asked(&mut self, host: HostId, sr: crate::SuperRegion) {
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
    pub fn land_wanted(&mut self, owner: HostId, now_ms: u64) {
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
        let wait = self.heir_wait_for(owner);
        self.successions.land_wanted.entry(owner).or_insert_with(|| {
            log::info!(
                "coordinator: host {owner} left and its land is wanted: it goes to an heir in {wait} ms unless host {owner} \
                 comes back"
            );
            now_ms
        });
    }

    /// Test support: the departed hosts whose land is wanted now (requests not forgotten).
    #[doc(hidden)]
    pub fn lands_wanted(&self) -> Vec<HostId> {
        self.successions.land_wanted.keys().copied().collect()
    }

    /// How long the land of departed host `owner` waits for it.
    fn heir_wait_for(&self, owner: HostId) -> u64 {
        if self.successions.host_count.is_some_and(|n| owner.0 >= u64::from(n)) {
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
        let attached: BTreeSet<HostId> = self.attached_live().into_iter().collect();
        let (leases, declared) = (&self.leases, &self.successions.declared);
        self.successions.land_wanted.retain(|owner, _| {
            let row = leases.row(*owner);
            // Declared already (an owners lookup answered just after the declaration): done.
            if row.is_some_and(|r| declared.contains(&(*owner, r.boot))) {
                return false;
            }
            let back = attached.contains(owner) || row.is_some_and(|r| !r.clean || r.state != HOST_LIVE);
            if back {
                log::info!("coordinator: host {owner} is back (or no longer left): its land is no longer given away");
            }
            !back
        });
        let mut due = None;
        for (&owner, &first) in &self.successions.land_wanted {
            let Some(row) = self.leases.row(owner) else { continue };
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
        let live: BTreeSet<HostId> = attached.into_iter().filter(|h| *h != owner).collect();
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
    /// `received_ms` ([`LeaseWatch::observe`]). A dirty dead row this coordinator never
    /// declared (an offline `worldctl retire-host`) asks, once, for the open successions to be
    /// read again ([`Self::successions_reread`]).
    pub fn observe_leases(&mut self, rows: &[HostSeen], issued_ms: u64, received_ms: u64) {
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
        // Before the open successions are known, their pending read lists it anyway.
        // A declaration in flight explains a dead row its answer will open.
        if !self.successions.enabled || !self.successions.known || self.successions.declaring {
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
            self.successions.reread_for.extend(unknown);
            self.successions_reread();
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
}
