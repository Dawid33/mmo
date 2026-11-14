//! A succession (R4): the survivor's half (plan Task 8), then the heir's (Task 9). Host
//! `predecessor` (process `predecessor_boot`) was declared dead and all its super-regions
//! are `heir`'s now ([`crate::CoordMsg::Succession`]). **The survivor's half:** every live
//! host named a survivor — the heir among them — answers it, in order:
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
//! A run is forgotten at [`crate::CoordMsg::SuccessionDone`]; until then an `Attach` lists
//! it as answered (a restarted coordinator counts it again), and the heir's as adopted once
//! it is.
//!
//! **The heir's half.** The heir holds what it inherits masked from step 4 on
//! ([`WorldManager::inheriting`]: `owner_of` answers `Unknown` for it, so nothing of it
//! starts, arrivals and placements wait). Once a report of EVERY survivor is in (its own
//! included; "wait, never guess": a missing one blocks — the coordinator's deadline turns
//! that into a rollback — and a report against another base cut is never resolved) it
//! makes the drop set and asks the server for the store
//! ([`WorldManager::take_adoption_wanted`]); the server's answer
//! ([`WorldManager::adopted_rows`]) runs, in this order:
//!
//! 1. **Resolve.** Every entry grouped by subject; the highest stamp decides (two events of
//!    one subject never share a stamp; if they do, a `Received` one wins — delivering too
//!    could make two). `Received` latest: a survivor's copy lives on — nothing delivered.
//!    `Sent` latest: the predecessor held it — its journalled message is delivered. The
//!    **drop set** is every entity any entry names: none of them is taken from the
//!    predecessor's store (it lives elsewhere, or comes from the journal). A body that hands
//!    a player over is never delivered as an entity: its player's subject decides.
//! 2. **Adopt rows.** The predecessor's in-flight rows of the base cut (the server read
//!    them under the fence of what this host holds now): those of a dropped entity are
//!    discarded, the rest put back on their way ([`WorldManager::restore_in_flight`]).
//! 3. **Deliver.** A plain entity's bundle arrives through `accept_arrival` like any crossing
//!    from another host (its region applies it at its next slot, never on receipt: the
//!    slot-order rule); a target that moved on to another host is sent there. A person's
//!    bundle is NOT delivered (the history's lists respawn people as their region starts —
//!    delivering too would make two): this host, the receiving host now, reports the
//!    crossing absorbed (`PersonCrossed { delivered: false }`), before any region of the
//!    land starts, so a person whose crossing died with the predecessor is abstract, not
//!    lost in a region that no longer holds their body. A player handed or moved to the
//!    predecessor is parked here from the hand-off's record (no session will come: the
//!    client was on the dead host), at its hop, never `departed`. Then the `owed` intents
//!    and replies, in each sender's order, as if that sender's session had brought them.
//! 4. **Load.** Every inherited region the store holds a dropped entity in (`located`), and
//!    every one an owed intent or reply goes to, starts now — the drop set left out of its
//!    store state ([`RegionSpawner::forget_entities`]) — pinned (`succession_pinned`) so it
//!    cannot park before the settle cut writes it. (The store's stale-row guard refuses a
//!    part missing one: the safety net, not the mechanism.)
//! 5. **Answer** [`crate::HostMsg::SuccessionAdopted`]. From here until
//!    `SuccessionDone { settled: true }` every cut part of this host adopts the predecessor
//!    ([`crate::WorldCut::adopts`]: the store replaces its in-flight rows with this part's) —
//!    the coordinator pairs that cut's `settles` with it. `SuccessionDone` unpins the
//!    regions and lifts the drop set (the settle cut rewrote every stored region holding
//!    one of them).
//!
//! **Where the mask is lifted.** `adopt_inherited` (lift the mask, flush what waited for
//! the land, retry held spawn orders) runs AFTER the drop set is installed in the spawner
//! and the rows are filtered, and after the people are reported absorbed, but BEFORE the
//! rows, bundles and owed messages go on: the flush may start an inherited region, which
//! must already leave the drop set out, and a person must be reported before their region
//! runs (the history would embody them in it); everything delivered afterwards finds the
//! land owned (`owns`), so nothing of it waits for an owner or bounces.
//!
//! **Who accounts for what the heir delivers — the three sender cases** (`world_cut.rs`,
//! re-derived for a succession; no world cut runs from the declaration until the heir's
//! `SuccessionAdopted`, and every cut pending with the predecessor was aborted before it):
//! (i) a message the predecessor sent and a survivor received is that survivor's — in its
//!     snapshot of the settle cut if applied by its tick, else its row (`Received` latest:
//!     the heir drops it from the predecessor's store state and rows);
//! (ii) a journalled bundle, player or owed message the heir delivers is the heir's own —
//!     it went bare, no cut wraps it (`inbound_accounted` is none): in its snapshot of the
//!     settle cut if applied by its tick, else its row (an arrival waiting, a parked record,
//!     a mailbox entry);
//! (iii) a message a survivor sends the heir for a moved region after its `Applied` is an
//!     ordinary state message between participants and follows `world_cut_wrap`
//!     unchanged (it waits on the heir while the land is masked: the heir's row).

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

/// Where an heir's adoption of a succession stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdoptionState {
    /// Every report is in: the store read is wanted ([`WorldManager::take_adoption_wanted`]).
    Wanted,
    /// The server reads the store.
    Asked,
    /// Adopted (`SuccessionAdopted` sent): this host's cut parts adopt the predecessor
    /// until the succession is over.
    Adopted,
}

/// An heir's adoption of one succession.
#[derive(Debug, Clone)]
pub(super) struct Adoption {
    pub predecessor: u32,
    /// Every entity a survivor's journal names: never taken from the predecessor's store.
    pub drop: BTreeSet<crate::EntityId>,
    pub state: AdoptionState,
    /// The regions started for it (pinned until the succession is over).
    pub pinned: BTreeSet<RegionCoords>,
}

/// What the survivors' journals resolve to (the module docs' step 1).
#[derive(Debug, Default)]
struct Resolved {
    /// Entries read.
    entries: usize,
    /// `Received` entries (a leave's reports should have none).
    received: usize,
    /// Subjects with two events at one stamp.
    ties: usize,
    /// Entity bundles to deliver: `(target, stamp, bundle)`, oldest stamp first.
    bundles: Vec<(RegionCoords, crate::Tick, crate::EntityBundle)>,
    /// People's bundles (never delivered): `(target, bundle)`.
    people: Vec<(RegionCoords, crate::EntityBundle)>,
    /// Players to park: the hand-off and the record, oldest first.
    players: Vec<(crate::PlayerHandoff, PlayerRecord)>,
}

/// Step 1 of the heir's half: every entry of every report grouped by subject; the entry
/// with the highest stamp decides (two events of one subject never share a stamp — a
/// transfer is applied at stamp + 1; if two ever do, a `Received` one wins: a survivor
/// holds the subject, and delivering too would make two). `Received` latest: nothing to
/// deliver (the survivor's copy lives on). `Sent` latest: the message is delivered — an
/// entity's bundle, a person's never (absorbed), a player's as a record to park; a body
/// that hands a player over is never delivered as an entity (its player's subject decides).
fn resolve(reports: &BTreeMap<u32, SuccessionReportIn>) -> Resolved {
    let mut out = Resolved::default();
    // subject → (stamp, sent, reporter, index)
    let mut latest: BTreeMap<Subject, (crate::Tick, bool, u32, usize)> = BTreeMap::new();
    for (from, report) in reports {
        for (i, e) in report.entries.iter().enumerate() {
            out.entries += 1;
            let sent = matches!(e.event, JournalEvent::Sent(_));
            out.received += usize::from(!sent);
            let replace = match latest.get(&e.subject) {
                None => true,
                Some(&(stamp, _, _, _)) if e.stamp > stamp => true,
                Some(&(stamp, was_sent, _, _)) if e.stamp == stamp => {
                    out.ties += 1;
                    was_sent && !sent
                }
                Some(_) => false,
            };
            if replace {
                latest.insert(e.subject, (e.stamp, sent, *from, i));
            }
        }
    }
    let mut players = Vec::new();
    for (subject, (stamp, sent, from, i)) in latest {
        if !sent {
            continue;
        }
        let JournalEvent::Sent(msg) = &reports[&from].entries[i].event else { continue };
        match (subject, msg.as_ref()) {
            // A body handing a player over: the player's subject decides.
            (Subject::Entity(_), Reliable::Transfer { player: Some(_), .. }) => {}
            (Subject::Entity(_), Reliable::Transfer { target, stamp, bundle, player: None }) => {
                if matches!(bundle.kind, EntityKind::Person { .. }) {
                    out.people.push((*target, (**bundle).clone()));
                } else {
                    out.bundles.push((*target, *stamp, (**bundle).clone()));
                }
            }
            (Subject::Player(_), Reliable::Transfer { target, bundle, player: Some(h), .. }) => {
                match peers::handoff_record(*target, bundle, h) {
                    Some(record) => players.push((stamp, h.clone(), record)),
                    None => log::error!("[server] a journalled hand-off of player {} carries no player", h.player.0),
                }
            }
            (Subject::Player(_), Reliable::PlayerMove { handoff, record, .. }) => {
                players.push((stamp, handoff.clone(), (**record).clone()));
            }
            (subject, _) => log::error!("[server] a journal entry about {subject:?} that is not its message; skipped"),
        }
    }
    out.bundles.sort_by_key(|(_, stamp, b)| (*stamp, b.id));
    players.sort_by_key(|(stamp, h, _)| (*stamp, h.player));
    out.players = players.into_iter().map(|(_, h, r)| (h, r)).collect();
    out
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
        // The heir: its own report is in now; the others may be too.
        self.try_resolve_succession(id);
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
        self.try_resolve_succession(id);
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

    /// The coordinator's [`crate::CoordMsg::SuccessionDone`]: the run and its reports end; an
    /// heir's adoption of it ends too — its regions unpinned, its drop set lifted from the
    /// spawner (the settle cut rewrote every stored region that held one of them).
    pub(super) fn succession_done(&mut self, id: SuccessionId, settled: bool) {
        let run = self.successions.remove(&id);
        let reports = self.succession_reports.remove(&id);
        if run.is_some() || reports.is_some() {
            log::info!("[server] succession {} {}", id.0, if settled { "settled" } else { "abandoned" });
        }
        if let Some(adoption) = self.adoptions.remove(&id) {
            self.succession_pinned = self.adoptions.values().flat_map(|a| a.pinned.iter().copied()).collect();
            let unpinned: Vec<RegionCoords> =
                adoption.pinned.iter().copied().filter(|rc| !self.succession_pinned.contains(rc)).collect();
            let now_ms = self.clock_ms;
            for rc in unpinned {
                // Its keep-alive is the usual one from now on.
                self.refresh_keepalive(rc, now_ms);
            }
            let drop = self.adoption_drop_union();
            self.spawner.forget_entities(&drop);
        }
        // Nothing of it is held any more (Task 9 adopts before the settle; an abandoned one
        // ends with this process).
        self.inheriting.retain(|_, s| *s != id);
        self.successions_done.insert(id);
    }

    /// The successions this host answered (`(id, adopted)`), for its `Attach`: `adopted`
    /// once this host, their heir, adopted them.
    pub(super) fn attach_successions(&self) -> Vec<(SuccessionId, bool)> {
        self.successions
            .iter()
            .filter(|(_, r)| r.applied)
            .map(|(id, _)| (*id, self.adoptions.get(id).is_some_and(|a| a.state == AdoptionState::Adopted)))
            .collect()
    }

    // --- The heir's half (Task 9) ---------------------------------------------------------

    /// Succession `id` (this host its heir, applied here) is resolved once a report of every
    /// survivor is in — never on part of them ("wait, never guess": a missing one blocks,
    /// and the coordinator's deadline rolls the world back): its drop set is made and the
    /// adoption read is wanted ([`Self::take_adoption_wanted`]).
    fn try_resolve_succession(&mut self, id: SuccessionId) {
        let me = self.ordinal();
        let Some(run) = self.successions.get(&id) else { return };
        if run.order.heir != me || !run.applied || self.adoptions.contains_key(&id) {
            return;
        }
        let Some(reports) = self.succession_reports.get(&id) else { return };
        let survivors: BTreeSet<u32> = run.order.survivors.iter().copied().collect();
        if !survivors.iter().all(|h| reports.contains_key(h)) {
            return;
        }
        let other_base: Vec<u32> =
            reports.iter().filter(|(_, r)| r.base_cut != run.order.base_cut).map(|(h, _)| *h).collect();
        if !other_base.is_empty() {
            // Never mixed with another cut's traffic: it waits (for ever: the deadline).
            log::error!(
                "[server] succession {}: the reports of hosts {other_base:?} count from another cut than {}: not resolved",
                id.0,
                run.order.base_cut.0
            );
            return;
        }
        let drop: BTreeSet<crate::EntityId> = reports
            .iter()
            .filter(|(h, _)| survivors.contains(h))
            .flat_map(|(_, r)| r.entries.iter())
            .filter_map(|e| match e.subject {
                Subject::Entity(eid) => Some(eid),
                Subject::Player(_) => None,
            })
            .collect();
        log::info!(
            "[server] succession {}: every survivor's report is in; {} entit(ies) to drop from host {}'s store, adoption read wanted",
            id.0,
            drop.len(),
            run.order.predecessor
        );
        let predecessor = run.order.predecessor;
        self.adoptions.insert(id, Adoption { predecessor, drop, state: AdoptionState::Wanted, pinned: BTreeSet::new() });
    }

    /// The adoption the server reads the store for, if any: `(succession, predecessor)`.
    /// It answers [`Self::adopted_rows`] with the predecessor's in-flight rows
    /// (`load_in_flight_of`) and the stored regions of [`Self::adoption_drop_set`]
    /// (`regions_of_entities`) — its fence already replaced by the claims it holds now — or
    /// [`Self::adoption_read_failed`].
    pub fn take_adoption_wanted(&mut self) -> Option<(SuccessionId, u32)> {
        let (id, a) = self.adoptions.iter_mut().find(|(_, a)| a.state == AdoptionState::Wanted)?;
        a.state = AdoptionState::Asked;
        Some((*id, a.predecessor))
    }

    /// The drop set of succession `id`'s adoption: every entity a survivor's journal names
    /// (each lives elsewhere now, or is delivered from the journal) — none of them is taken
    /// from the predecessor's store.
    pub fn adoption_drop_set(&self, id: SuccessionId) -> Vec<crate::EntityId> {
        self.adoptions.get(&id).map(|a| a.drop.iter().copied().collect()).unwrap_or_default()
    }

    /// The server could not read what [`Self::take_adoption_wanted`] asked for: asked again.
    pub fn adoption_read_failed(&mut self, id: SuccessionId) {
        if let Some(a) = self.adoptions.get_mut(&id).filter(|a| a.state == AdoptionState::Asked) {
            log::warn!("[server] succession {}: the adoption read failed; asked again", id.0);
            a.state = AdoptionState::Wanted;
        }
    }

    /// The store's answer to [`Self::take_adoption_wanted`] for succession `id`: the
    /// predecessor's in-flight `rows` of the base cut and where the store holds each entity
    /// of the drop set (`located`). Steps 1–5 of the module docs, in the order they give.
    pub fn adopted_rows(&mut self, id: SuccessionId, mut rows: Vec<InFlightRow>, located: Vec<(crate::EntityId, RegionCoords)>) {
        let Some(adoption) = self.adoptions.get(&id).filter(|a| a.state == AdoptionState::Asked) else {
            log::warn!("[server] succession {}: adoption rows that were not asked for (or are answered); ignored", id.0);
            return;
        };
        let (pred, drop) = (adoption.predecessor, adoption.drop.clone());
        let Some(order) = self.successions.get(&id).map(|r| r.order.clone()) else {
            log::error!("[server] succession {}: adoption rows for a succession over here; ignored", id.0);
            return;
        };
        let mut reports = self.succession_reports.remove(&id).unwrap_or_default();
        reports.retain(|h, _| order.survivors.contains(h));
        // 1. Resolve: per subject the latest event decides.
        let resolved = resolve(&reports);
        if order.kind == crate::SuccessionKind::Leave && resolved.received > 0 {
            log::error!(
                "[server] succession {} (a leave): {} entries say something came out of host {pred} after its cut; \
                 resolved like any other",
                id.0,
                resolved.received
            );
        }
        if resolved.ties > 0 {
            log::error!("[server] succession {}: {} subject(s) with two events at one stamp (a Received kept)", id.0, resolved.ties);
        }
        // 2. Adopt rows: what a survivor holds or is delivered from a journal is not taken.
        let all_rows = rows.len();
        rows.retain(|r| match r {
            InFlightRow::Transfer { bundle, .. } => !drop.contains(&bundle.id),
            InFlightRow::Intent { .. } => true,
        });
        // 4a. No region of the predecessor's starts from the store with a dropped entity:
        // installed before the mask is lifted (its flush may start one).
        let all_drop = self.adoption_drop_union();
        self.spawner.forget_entities(&all_drop);
        // 3a. A person's bundle is never delivered: the receiving host reports the crossing,
        // and this host is that host now — absorbed (no region of the land runs yet), so the
        // history holds the person abstract and respawns them when it may.
        let now_ms = self.clock_ms;
        let mut people = 0usize;
        for (target, bundle) in &resolved.people {
            if let (EntityKind::Person { person }, true) = (bundle.kind, self.world.is_some()) {
                self.note_moved(person, *target, Step::Absorbed { at: bundle_at(bundle) });
                self.world_send(HostMsg::PersonCrossed { person, to: *target, delivered: false });
                people += 1;
            }
        }
        // The land is this host's from here: what waited for it goes on.
        self.adopt_inherited(id);
        self.restore_in_flight(rows.clone());
        // 3. Deliver, as any crossing (through `accept_arrival`: the region applies it at its
        // next slot, never on receipt).
        for (target, stamp, bundle) in resolved.bundles.iter().cloned() {
            if self.owns(target) {
                self.accept_arrival(None, target, stamp, bundle, now_ms);
            } else {
                let msg = Reliable::Transfer { target, stamp, bundle: Box::new(bundle), player: None };
                match self.foreign_host(target) {
                    Some(owner) => self.send_reliable(owner, msg, false),
                    None => self.wait_for_owner(target, msg, false, None),
                }
            }
        }
        let mut parked = 0usize;
        for (handoff, record) in resolved.players.iter().cloned() {
            if self.park_from_journal(id, handoff, record) {
                parked += 1;
            }
        }
        // 4. Load every inherited region the store holds a dropped entity in — and every one
        // an owed intent or reply goes to (it never ran here: a reply for it would have
        // nowhere to wait) — pinned until the settle cut writes it.
        let owed_targets = reports.values().flat_map(|r| r.owed.iter()).flat_map(crate::intents_in).map(|m| peers::peer_intent_target(&m));
        let to_load: BTreeSet<RegionCoords> = located.iter().map(|(_, rc)| *rc).chain(owed_targets).collect();
        let mut pinned = BTreeSet::new();
        for rc in to_load {
            if self.owns(rc) {
                self.ensure_running(rc);
                if self.regions.contains_key(&rc) {
                    pinned.insert(rc);
                }
            }
        }
        // Owed intents and replies, in each sender's order, as if that sender's session had
        // brought them (not accounted in any cut but this host's next).
        let mut owed = 0usize;
        for (from, report) in std::mem::take(&mut reports) {
            for msg in report.owed {
                owed += 1;
                self.peer_reliable_in(from, msg, false);
            }
        }
        self.succession_pinned.extend(pinned.iter().copied());
        self.succession_seam_events += resolved.entries as u64;
        log::info!(
            "[server] succession {}: adopted host {pred}'s land — {} journal entries resolved, {} bundle(s) delivered, \
             {parked} player(s) parked, {people} person(s) absorbed, {owed} owed intent(s)/repl(ies) applied, \
             {} of {all_rows} in-flight row(s) kept, {} region(s) loaded and pinned",
            id.0,
            resolved.entries,
            resolved.bundles.len(),
            rows.len(),
            pinned.len()
        );
        if let Some(a) = self.adoptions.get_mut(&id) {
            a.state = AdoptionState::Adopted;
            a.pinned = pinned;
        }
        // 5. Answered: the coordinator's next cut settles it (its part here adopts `pred`).
        self.world_send(HostMsg::SuccessionAdopted { id });
    }

    /// A player a survivor handed (or moved) to the predecessor, its journal's latest:
    /// parked here from what was sent — no session will come, the client was on the dead
    /// host — at the hand-off's hop (never `departed`). `false` if this host holds the
    /// player already (a login placed it meanwhile: that copy is the newer truth).
    fn park_from_journal(&mut self, id: SuccessionId, h: crate::PlayerHandoff, record: PlayerRecord) -> bool {
        if self.holds_player(h.player) {
            log::warn!("[server] succession {}: player {} is here already; the journal's copy is dropped", id.0, h.player.0);
            return false;
        }
        self.placed_by_handoff(h.player, h.hop);
        let hop = self.hop_here(h.player);
        self.world_send(HostMsg::PlayerOffline { player: h.player, hop });
        self.parked_players.insert(h.player, record);
        true
    }

    /// Whether `rc` is land this host inherits and has not adopted yet (the mask).
    pub(super) fn inherited_not_adopted(&self, rc: RegionCoords) -> bool {
        self.inheriting.contains_key(&crate::super_region_of(rc, self.ownership.1))
    }

    /// Every drop set of an adoption not over yet.
    fn adoption_drop_union(&self) -> BTreeSet<crate::EntityId> {
        self.adoptions.values().flat_map(|a| a.drop.iter().copied()).collect()
    }

    /// The predecessors this host adopted and whose successions are not settled: its cut
    /// parts carry them ([`crate::WorldCut::adopts`]).
    pub(super) fn adopting_predecessors(&self) -> Vec<u32> {
        let set: BTreeSet<u32> =
            self.adoptions.values().filter(|a| a.state == AdoptionState::Adopted).map(|a| a.predecessor).collect();
        set.into_iter().collect()
    }

    /// Journal entries this host resolved as an heir, since it started (the `[server]` line).
    pub fn succession_seam_events(&self) -> u64 {
        self.succession_seam_events
    }

    /// The regions this host started for an adoption, pinned until its succession is over.
    pub fn succession_pinned(&self) -> Vec<RegionCoords> {
        self.succession_pinned.iter().copied().collect()
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
