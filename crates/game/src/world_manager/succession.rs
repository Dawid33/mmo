//! The survivor's half of a succession (R4, plan Task 8): host `predecessor` (process
//! `predecessor_boot`) was declared dead and all its super-regions are `heir`'s now
//! ([`crate::CoordMsg::Succession`]). Every live host named a survivor — the heir among them
//! — answers it, in order:
//!
//! 1. **Base.** Its peer journal counts from `base_cut`, or it waits (the `CutDone` of that
//!    cut is on its way: a report against another base would mix two cuts' traffic). Never
//!    reported against another base; a wait is logged once per [`SUCCESSION_WAIT_LOG_MS`],
//!    and a host that never gets there is the coordinator's deadline's to roll back.
//! 2. **Close.** The link to the predecessor is closed and a hello of `predecessor_boot` is
//!    refused from now on: a zombie of it exchanges nothing. A hello of ANOTHER boot of that
//!    ordinal is a new host (fresh numbering both ways, R3's boot rule).
//! 3. **Report** to the heir ([`Reliable::SuccessionReport`]; the heir files its own): the
//!    journal against the predecessor ([`crate::PeerJournal::take`]) and, as `owed`, the
//!    intents and replies this host still owed it — its session's unacknowledged ones,
//!    `peer_waiting`, what a leave held back — in order, unwrapped. A transfer or move owed
//!    is NOT in `owed`: the journal recorded it at queue time (`Sent`); what a leave held back
//!    is journalled now, so it is reported the same way. Everything else owed is dropped: a
//!    `SpawnOrder` is answered `SpawnAck { ok: false }` to its origin; `ObserverOrder`,
//!    `Kick`, `KickAck`, `SessionEnded`, `PlayerParked`, `CutMark`, `Owns` are forgotten (a
//!    kick waiting for the predecessor's acknowledgement no longer does).
//! 4. **Re-route.** The directory gives every `moved` row to the heir (the predecessor is
//!    no longer live); the predecessor's running regions end here (`HaloSourceGone`, marked
//!    stale for the store's re-seed); its observer tickets, endpoint, `left_peers` entry,
//!    `early_marks`, `ClaimWanted` throttle, sync and cut-view marks go; a player handed to
//!    it (`departed`, `handed`) points at the heir when a `Sent` entry of the report carries
//!    it (the heir places it from the journal; on the heir itself it points nowhere) and is
//!    forgotten otherwise (the store's record places it at its next login); a player it handed here and whose session has not come
//!    is parked without telling it.
//! 5. **Answer** [`crate::HostMsg::SuccessionApplied`]. From here a region of a moved
//!    super-region is an ordinary foreign region of the heir — reachable once the heir
//!    announces it — and what this host sends the heir is ordinary traffic between
//!    participants (`world_cut_wrap` unchanged).
//!
//! The heir collects every survivor's report ([`WorldManager::succession_reports`]); what it
//! does with them (resolve, adopt, deliver, load, `SuccessionAdopted`) is its own half
//! (Task 9). A run is forgotten at [`crate::CoordMsg::SuccessionDone`]; until then an
//! `Attach` lists it as answered (a restarted coordinator counts it again).

use super::*;
use crate::{JournalEntry, JournalEvent, Reliable, Subject, Succession, SuccessionId};

/// At most one log line per this many ms about successions waiting for their base cut.
pub const SUCCESSION_WAIT_LOG_MS: u64 = 5_000;

/// A succession this host was ordered to answer.
#[derive(Debug, Clone)]
pub(super) struct SurvivorRun {
    pub order: Succession,
    /// Reported and re-routed (`SuccessionApplied` sent).
    pub applied: bool,
}

/// A survivor's report as the heir holds it ([`Reliable::SuccessionReport`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SuccessionReportIn {
    pub base_cut: CutId,
    pub entries: Vec<JournalEntry>,
    pub owed: Vec<Reliable>,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// The coordinator's order of succession `s` ([`crate::CoordMsg::Succession`]): answered
    /// as the module docs say — now, or once this host's journal counts from its base cut.
    /// A repeated order (a resend after a reconnect) is answered again if it was applied.
    pub(super) fn succession_ordered(&mut self, s: Succession) {
        let me = self.ordinal();
        if let Some(run) = self.successions.get(&s.id) {
            if run.applied {
                log::debug!("[server] succession {}: ordered again; applied already", s.id.0);
                self.world_send(HostMsg::SuccessionApplied { id: s.id });
            }
            return;
        }
        if s.predecessor == me {
            log::error!(
                "[server] succession {}: THIS host ({me}) was declared dead (boot {:#x}); its writes are fenced",
                s.id.0,
                s.predecessor_boot
            );
            return;
        }
        if !s.survivors.contains(&me) {
            log::warn!("[server] succession {}: this host is not among its survivors {:?}; ignored", s.id.0, s.survivors);
            return;
        }
        log::warn!(
            "[server] succession {}: host {} (boot {:#x}) is dead, heir {}, base cut {}",
            s.id.0,
            s.predecessor,
            s.predecessor_boot,
            s.heir,
            s.base_cut.0
        );
        let id = s.id;
        self.successions.insert(id, SurvivorRun { order: s, applied: false });
        self.try_apply_succession(id);
    }

    /// Every succession not applied yet whose base cut this host's journal counts from now
    /// is applied; the others log their wait (at most once per [`SUCCESSION_WAIT_LOG_MS`]).
    pub(super) fn retry_successions(&mut self) {
        let waiting: Vec<SuccessionId> = self.successions.iter().filter(|(_, r)| !r.applied).map(|(id, _)| *id).collect();
        for id in waiting {
            self.try_apply_succession(id);
        }
    }

    fn try_apply_succession(&mut self, id: SuccessionId) {
        let Some(run) = self.successions.get(&id).filter(|r| !r.applied) else { return };
        let base = self.journal.base();
        if base != run.order.base_cut {
            let now = self.clock_ms;
            if self.succession_wait_logged_ms.is_none_or(|at| now.saturating_sub(at) >= SUCCESSION_WAIT_LOG_MS) {
                self.succession_wait_logged_ms = Some(now);
                log::warn!(
                    "[server] succession {}: this host's journal counts from cut {}, the succession from cut {}: waiting \
                     (for that cut's decision; the coordinator rolls the world back at its deadline)",
                    id.0,
                    base.0,
                    run.order.base_cut.0
                );
            }
            return;
        }
        let s = run.order.clone();
        self.apply_succession(&s);
        if let Some(run) = self.successions.get_mut(&id) {
            run.applied = true;
        }
        self.world_send(HostMsg::SuccessionApplied { id });
    }

    /// Steps 2–4 of the module docs for succession `s` (its base cut matches).
    fn apply_succession(&mut self, s: &Succession) {
        let (pred, heir, me) = (s.predecessor, s.heir, self.ordinal());
        // 2. Close: nothing more of the dead boot is read, its hello is refused for good.
        self.dead_boots.insert((pred, s.predecessor_boot));
        self.succeeded_peers.insert(pred, s.predecessor_boot);
        let mut owed_raw: Vec<Reliable> = Vec::new();
        if let Some(peer) = self.peers.get_mut(&pred) {
            peer.session.refuse_boot(s.predecessor_boot);
            peer.link.close();
            owed_raw.extend(peer.session.take_unreceived());
        }
        owed_raw.extend(self.peer_waiting.remove(&pred).unwrap_or_default());
        // What a leave held back was never queued (nor journalled): journalled now, so its
        // transfers and moves are `Sent` entries of the report like every other.
        if let Some(pair) = self.leave_pairs.remove(&pred) {
            let tick = self.spawner.world_tick();
            for msg in pair.held {
                let count = self.peer_queued.entry(pred).or_default();
                let qid = *count;
                *count += 1;
                self.journal.sent(pred, qid, &msg, None, tick);
                owed_raw.push(msg);
            }
        }
        self.peer_in_source_view.retain(|(o, _)| *o != pred);
        // 3. Report.
        let entries = self.journal.take(pred);
        let mut owed = Vec::new();
        let (mut dropped, mut refused_spawns) = (0usize, 0usize);
        for msg in owed_raw {
            match msg.inner().clone() {
                m @ Reliable::Intent(_) => owed.push(m),
                // A journal entry already (`Sent`): the heir resolves it.
                Reliable::Transfer { .. } | Reliable::PlayerMove { .. } => {}
                Reliable::SpawnOrder { origin, player_token, session_epoch, .. } => {
                    refused_spawns += 1;
                    self.spawn_answer(origin, player_token, session_epoch, false, None);
                }
                _ => dropped += 1,
            }
        }
        // A kick no longer waits for the predecessor's acknowledgement.
        let mut acked = Vec::new();
        for (key, waiting) in self.kicks_pending.iter_mut() {
            if waiting.remove(&pred) && waiting.is_empty() {
                acked.push(key.clone());
            }
        }
        for key in acked {
            self.kicks_pending.remove(&key);
            self.login_out.push(LoginOut::KickAck { player_token: key.0, session_epoch: key.1 });
        }
        let carried: BTreeSet<PlayerId> = entries
            .iter()
            .filter(|e| matches!(e.event, JournalEvent::Sent(_)))
            .filter_map(|e| match e.subject {
                Subject::Player(p) => Some(p),
                Subject::Entity(_) => None,
            })
            .collect();
        log::info!(
            "[server] succession {}: reporting {} journal entries and {} owed intents/replies to heir {heir} \
             ({dropped} other messages dropped, {refused_spawns} spawn orders refused)",
            s.id.0,
            entries.len(),
            owed.len()
        );
        let report = SuccessionReportIn { base_cut: s.base_cut, entries, owed };
        if heir == me {
            self.file_succession_report(me, s.id, report);
        } else {
            // A plain reliable message: no cut runs while the succession is unanswered, so it
            // is never wrapped (`world_cut_wrap` leaves a non-state message bare).
            let SuccessionReportIn { base_cut, entries, owed } = report;
            self.queue_to_peer(heir, Reliable::SuccessionReport { id: s.id, base_cut, entries, owed });
        }
        // 4. Re-route. The heir holds what it inherits until it adopts it (Task 9): its own
        // directory row is right (owner = itself, the new epoch), but `owner_of` answers
        // `Unknown` for those super-regions meanwhile — no region of them starts from the
        // store before the survivors' journals are resolved.
        if heir == me {
            for &(sr, _) in &s.moved {
                self.inheriting.insert(sr, s.id);
            }
        }
        if let Some(directory) = self.ownership.0.directory_mut() {
            for &(sr, epoch) in &s.moved {
                directory.set(sr, Some(heir), epoch);
            }
            directory.note_dead(pred);
        } else if !s.moved.is_empty() {
            log::error!("[server] succession {}: this host keeps no directory; {} moved rows not taken", s.id.0, s.moved.len());
        }
        self.peer_lost(pred);
        self.peer_unsynced(pred);
        self.observer_tickets.retain(|(_, owner), _| *owner != pred);
        self.peer_endpoints.remove(&pred);
        self.left_peers.remove(&pred);
        self.early_marks.remove(&pred);
        self.claim_wanted_sent.retain(|_, (to, _)| *to != pred);
        let mut departed_moved = 0usize;
        let mut departed_forgotten = 0usize;
        let pointing: Vec<PlayerId> = self.departed.iter().filter(|(_, h)| **h == pred).map(|(p, _)| *p).collect();
        for p in pointing {
            // The heir itself places a carried player from the journal (Task 9): it points
            // nowhere then (a player is never "handed to" the host that holds it).
            if carried.contains(&p) && heir != me {
                self.departed.insert(p, heir);
                departed_moved += 1;
            } else {
                self.departed.remove(&p);
                departed_forgotten += 1;
            }
        }
        let handed: Vec<PlayerId> = self.handed.iter().filter(|(_, (to, _, _))| *to == pred).map(|(p, _)| *p).collect();
        for p in handed {
            if carried.contains(&p) && heir != me {
                if let Some(entry) = self.handed.get_mut(&p) {
                    entry.0 = heir;
                }
            } else {
                self.handed.remove(&p);
            }
        }
        // Handed here by the predecessor, its session not come: nobody there to tell.
        for (_, from) in self.awaiting_session.values_mut() {
            if *from == pred {
                *from = me;
            }
        }
        if departed_moved + departed_forgotten > 0 {
            log::info!(
                "[server] succession {}: {departed_moved} departed player(s) now at heir {heir}, {departed_forgotten} \
                 forgotten (the store places them)",
                s.id.0
            );
        }
        // What waited for an owner of a moved super-region goes on (to the heir, or here).
        self.flush_unowned();
        // Spawn orders that waited for the predecessor's sync go on.
        self.retry_held_spawns();
    }

    /// A survivor's report of succession `id` (`from`; this host is its heir): kept for the
    /// heir's resolution (Task 9).
    fn file_succession_report(&mut self, from: u32, id: SuccessionId, report: SuccessionReportIn) {
        if self.successions_done.contains(&id) {
            log::warn!("[server] succession {}: host {from}'s report came after it was over; dropped", id.0);
            return;
        }
        if let Some(run) = self.successions.get(&id) {
            if run.order.heir != self.ordinal() {
                log::error!("[server] succession {}: host {from}'s report came here; the heir is {}", id.0, run.order.heir);
            }
        }
        log::info!(
            "[server] succession {}: host {from}'s report ({} entries, {} owed) filed",
            id.0,
            report.entries.len(),
            report.owed.len()
        );
        self.succession_reports.entry(id).or_default().insert(from, report);
    }

    /// Host `from`'s [`Reliable::SuccessionReport`].
    pub(super) fn peer_succession_report(
        &mut self,
        from: u32,
        id: SuccessionId,
        base_cut: CutId,
        entries: Vec<JournalEntry>,
        owed: Vec<Reliable>,
    ) {
        self.file_succession_report(from, id, SuccessionReportIn { base_cut, entries, owed });
    }

    /// The coordinator's [`crate::CoordMsg::SuccessionDone`]: the run and its reports end.
    pub(super) fn succession_done(&mut self, id: SuccessionId, settled: bool) {
        let run = self.successions.remove(&id);
        let reports = self.succession_reports.remove(&id);
        if run.is_some() || reports.is_some() {
            log::info!("[server] succession {} {}", id.0, if settled { "settled" } else { "abandoned" });
        }
        // Nothing of it is held any more (Task 9 adopts before the settle; an abandoned one
        // ends with this process).
        self.inheriting.retain(|_, s| *s != id);
        self.successions_done.insert(id);
    }

    /// The successions this host answered (`(id, adopted)`), for its `Attach`. Adopting is
    /// the heir's (Task 9): never yet.
    pub(super) fn attach_successions(&self) -> Vec<(SuccessionId, bool)> {
        self.successions.iter().filter(|(_, r)| r.applied).map(|(id, _)| (*id, false)).collect()
    }

    /// Whether host `ordinal`'s current boot was succeeded (until a hello of another boot):
    /// it holds nobody and is awaited by nothing.
    pub(super) fn peer_succeeded(&self, ordinal: u32) -> bool {
        self.succeeded_peers.contains_key(&ordinal)
    }

    /// A hello of host `from`'s boot `boot` was accepted: if that ordinal was succeeded as
    /// another boot, it is a new host now.
    pub(super) fn succession_hello(&mut self, from: u32, boot: u64) {
        if self.succeeded_peers.get(&from).is_some_and(|dead| *dead != boot) {
            log::info!("[server] host {from} is back as a new boot ({boot:#x}) after its succession");
            self.succeeded_peers.remove(&from);
        }
    }

    /// The successions this host answers (or waits to): `(id, applied)`.
    pub fn successions(&self) -> Vec<(SuccessionId, bool)> {
        self.successions.iter().map(|(id, r)| (*id, r.applied)).collect()
    }

    /// The heir's: the survivors' reports of succession `id` it holds, by survivor.
    pub fn succession_reports(&self, id: SuccessionId) -> Option<&BTreeMap<u32, SuccessionReportIn>> {
        self.succession_reports.get(&id)
    }

    /// Every report held (the heir's), by succession and survivor.
    pub fn all_succession_reports(&self) -> &BTreeMap<SuccessionId, BTreeMap<u32, SuccessionReportIn>> {
        &self.succession_reports
    }

    /// The heir adopted succession `id` (Task 9 calls this once resolved and loaded): the
    /// super-regions it inherits by it are its own from now on.
    /// It lifts the `inheriting` mask of those super-regions and delivers what waited for them
    /// (`flush_unowned`), then retries held spawn orders.
    pub fn adopt_inherited(&mut self, id: SuccessionId) {
        let before = self.inheriting.len();
        self.inheriting.retain(|_, s| *s != id);
        let lifted = before - self.inheriting.len();
        if lifted > 0 {
            log::info!("[server] succession {}: {lifted} inherited super-region(s) adopted", id.0);
        }
        self.flush_unowned();
        self.retry_held_spawns();
    }

    /// The super-regions this host inherits and has not adopted yet, with their succession.
    pub fn inheriting(&self) -> &BTreeMap<crate::SuperRegion, SuccessionId> {
        &self.inheriting
    }

    /// Test support: whether this host's session with host `ordinal` is reachable now.
    #[doc(hidden)]
    pub fn peer_is_reachable(&self, ordinal: u32) -> bool {
        self.peer_reachable(ordinal)
    }

    /// Test support: apply a coordinator message as if the world link had brought it.
    #[doc(hidden)]
    pub fn apply_coordinator_message(&mut self, msg: crate::CoordMsg) {
        let now = self.world_ms;
        self.apply_world_msg(msg, now);
        self.flush_world();
    }
}
