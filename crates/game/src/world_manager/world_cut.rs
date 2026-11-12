//! The host's half of a world cut (several hosts, R3 Task 13; plan decision 6 and the
//! controller's ruling `task-13-ruling.md`). The coordinator picks one world tick T
//! ([`crate::CoordMsg::BeginCut`]); every participant snapshots its regions at T exactly as
//! a one-phase cut does (`save_cut.rs`, [`WorldManager::begin_cut_at`]) and, once every
//! region has answered, queues a [`Reliable::CutMark`] to every other participant. A
//! host's part is complete when it also holds every other participant's mark.
//!
//! **Who accounts for a state message on the wire at T: the message says.** A state
//! message (transfer, with or without a player; player move; intent or reply) M from host
//! A to a participant B while A's part of cut K is not decided:
//! 1. `view` (its source region had answered: A's snapshot holds what it carries) →
//!    sent wrapped, [`Reliable::InCut`] `{ K, M }`; A writes nothing for it. Its stamp /
//!    due is ≥ T by construction.
//! 2. not `view`, A's mark not queued yet → sent bare: B's (its snapshot if applied by T,
//!    else B's row / record). A's snapshot never held it (it left before its source's
//!    state was captured).
//! 3. not `view`, A's mark already queued (a manager-made message: a refusal of an intent
//!    that came after the mark, a region started after the marks) → A keeps it in its own
//!    part — a row (transfer, intent, reply) or a player record (hand-off, move) — raises
//!    its stamp / due to at least T (no receiver region applies it before its snapshot) and
//!    sends it wrapped.
//! After A's part is complete (until its `CutDone`) everything is case 1. A message made
//! while a wrapped one is applied carries the same wrapper (its lineage is accounted).
//!
//! The receiver applies a wrapped M normally but notes what it creates as accounted for by
//! the sender in K (`accounted_in` on the entry, `accounted_players`): never written by
//! this host's part of K — also when the wrapper arrives before this host's `BeginCut`. A
//! bare state message after a participant's mark of the pending cut is a protocol
//! violation: applied, and the part fails.
//!
//! For each state message exactly one of (bare, before the mark: B's), (wrapped case 1:
//! A's snapshot), (wrapped case 3: A's row / record) holds, so restoring every host from K
//! yields each entity, item and player once; a case-3 row with a foreign target is queued
//! to its owner again on restore ([`WorldManager::restore_in_flight`]). A link that is down
//! between two participants stops the marks: the cut aborts.
//!
//! **Aborts.** A participant's link going down (or a newer generation) while the part is
//! not complete, a region missing the tick or lost, a deadline, or another cut still
//! pending fail the part ([`crate::HostMsg::CutFailed`]); the coordinator then aborts the
//! cut for every participant. A complete part stays until its
//! [`crate::CoordMsg::CutDone`] ([`WorldManager::take_world_cut_done`]).

use super::*;
use crate::{CutFailure, Reliable};

/// Where this host's part of a world cut stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PartState {
    /// Regions answering, marks going out and coming in.
    Pending,
    /// Complete ([`WorldManager::take_cut`] gives it), not staged yet.
    Complete,
    /// Staged in the store ([`WorldManager::world_cut_prepared`]).
    Prepared,
    /// Failed here (`CutFailed` sent): kept until its `CutDone`, so what this host sends a
    /// participant still goes wrapped (never counted in the dead cut) and no other cut
    /// begins meanwhile.
    Failed,
}

/// The world cut this host takes part in.
#[derive(Debug)]
pub(super) struct WorldCutRun {
    pub cut: CutId,
    pub tick: crate::Tick,
    /// The other participants (the leaving ones too).
    pub peers: BTreeSet<u32>,
    /// The participants whose mark arrived.
    pub marks_in: BTreeSet<u32>,
    /// This host's marks are queued.
    pub mark_sent: bool,
    pub state: PartState,
    /// Inside [`WorldManager::begin_world_cut`]: a failure there is answered by its caller
    /// (one `CutFailed`).
    pub beginning: bool,
}

/// The cut tick must be at least this many ticks past the world tick when the host begins
/// it: closer, the cut is missed (its regions may already be past it).
pub const WORLD_CUT_MIN_LEAD_TICKS: crate::Tick = SAVE_CUT_LEAD_TICKS / 2;

impl<S: RegionSpawner> WorldManager<S> {
    /// Take part in world cut `cut` at world tick `tick` with `hosts` (`leaving`: the
    /// hosts leaving in it — Task 14: treated as participants for now). `Missed` when
    /// `tick` is closer than [`WORLD_CUT_MIN_LEAD_TICKS`] or past; `Aborted` while another
    /// cut is pending here (it is left untouched), when a participant is not reachable or a
    /// region thread is dead.
    pub fn begin_world_cut(
        &mut self,
        cut: CutId,
        tick: crate::Tick,
        hosts: &[u32],
        leaving: &[u32],
    ) -> Result<(), CutFailure> {
        if self.world_cut.is_some() || self.pending_cut.is_some() || self.ready_cut.is_some() {
            log::warn!("world cut {}: another cut is still pending here; refused", cut.0);
            return Err(CutFailure::Aborted);
        }
        self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
        let me = self.ownership.0.ordinal();
        let now = self.spawner.world_tick();
        if tick < now + WORLD_CUT_MIN_LEAD_TICKS {
            log::warn!("world cut {} at tick {tick}: the world tick is {now} already; missed", cut.0);
            return Err(CutFailure::Missed);
        }
        // A wrapper of this cut (or a later one) already came: a peer's region is past the
        // tick, this host is at the edge of missing it — it does not take part ("wait,
        // never guess"); the coordinator doubles the lead.
        if self.wrapper_seen.is_some_and(|w| w >= cut) {
            log::warn!("world cut {}: a peer's wrapped message of it came before its BeginCut; missed", cut.0);
            return Err(CutFailure::Missed);
        }
        // Task 14: a leaving host takes part like any participant for now.
        let all: BTreeSet<u32> = hosts.iter().chain(leaving).copied().collect();
        if !all.contains(&me) {
            log::error!("world cut {}: host {me} is not among its hosts {hosts:?}; refused", cut.0);
            return Err(CutFailure::Aborted);
        }
        let peers: BTreeSet<u32> = all.into_iter().filter(|h| *h != me).collect();
        // Every participant's link must be up: one that is down would never bring its mark
        // (wait, never guess) — the cut is aborted at once.
        if let Some(p) = peers.iter().find(|p| !self.peer_reachable(**p)) {
            log::warn!("world cut {}: host {p} is not reachable; aborted", cut.0);
            return Err(CutFailure::Aborted);
        }
        // Marks of this cut that came before its `BeginCut`.
        let marks_in: BTreeSet<u32> = self.early_marks.iter().filter(|(p, c)| **c == cut && peers.contains(p)).map(|(p, _)| *p).collect();
        self.early_marks.retain(|_, c| *c > cut);
        self.world_cut =
            Some(WorldCutRun { cut, tick, peers, marks_in, mark_sent: false, state: PartState::Pending, beginning: true });
        if self.begin_cut_at(cut, tick).is_none() {
            // A dead region thread: the part failed; the caller sends the one `CutFailed`.
            self.world_cut = None;
            return Err(CutFailure::Aborted);
        }
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == cut) {
            run.beginning = false;
        }
        log::debug!("[server] world cut {} at tick {tick} begun", cut.0);
        self.try_complete_cut();
        Ok(())
    }

    /// The world cut this host takes part in (until its `CutDone`, or a local failure).
    pub fn world_cut(&self) -> Option<CutId> {
        self.world_cut.as_ref().map(|r| r.cut)
    }

    /// The server staged this host's part of world cut `cut`: the coordinator is told
    /// ([`crate::HostMsg::CutPrepared`]); the part waits for its `CutDone`.
    pub fn world_cut_prepared(&mut self, cut: CutId) {
        match self.world_cut.as_mut() {
            Some(run) if run.cut == cut && run.state == PartState::Complete => {
                run.state = PartState::Prepared;
                self.world_send(HostMsg::CutPrepared { cut });
            }
            _ => log::debug!("world cut {}: prepared, but no longer this host's cut (decided meanwhile)", cut.0),
        }
    }

    /// The server could not stage this host's part of world cut `cut` (`why`): its
    /// uncommitted entries stay for the next cut ([`Self::cut_failed`]) and the
    /// coordinator is told.
    pub fn world_cut_failed(&mut self, cut: CutId, why: CutFailure) {
        self.cut_failed(cut);
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == cut) {
            run.state = PartState::Failed;
        }
        self.world_send(HostMsg::CutFailed { cut, why });
    }

    /// World cuts decided since the last call (`CutDone`), `(cut, committed)`: the server
    /// settles its staged part with the store, then calls [`Self::cut_committed`] or
    /// [`Self::cut_failed`] (the rig does the same with its in-memory store).
    pub fn take_world_cut_done(&mut self) -> Vec<(CutId, bool)> {
        std::mem::take(&mut self.world_cut_done)
    }

    /// The coordinator's `BeginCut`: begun, or refused with `CutFailed`.
    pub(super) fn world_begin_cut(&mut self, cut: CutId, tick: crate::Tick, hosts: &[u32], leaving: &[u32], generation: u64) {
        if !self.world_attached() {
            // Sent before the coordinator applied our latest `Attach`, which aborted it.
            log::debug!("world cut {}: begun before our latest attach; dropped", cut.0);
            return;
        }
        if generation != self.host_generation {
            log::warn!("world cut {}: of world generation {generation}, this host runs {}; refused", cut.0, self.host_generation);
            if generation > self.host_generation {
                self.higher_generation = Some(self.higher_generation.map_or(generation, |g| g.max(generation)));
            }
            self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
            return self.world_send(HostMsg::CutFailed { cut, why: CutFailure::Aborted });
        }
        if let Err(why) = self.begin_world_cut(cut, tick, hosts, leaving) {
            self.world_send(HostMsg::CutFailed { cut, why });
        }
    }

    /// The coordinator's `CutDone`: the cut is over here (a part still pending is dropped:
    /// aborted elsewhere), and the server settles it ([`Self::take_world_cut_done`]).
    pub(super) fn world_cut_decided(&mut self, cut: CutId, committed: bool) {
        if let Some(run) = self.world_cut.take_if(|r| r.cut == cut) {
            match run.state {
                PartState::Pending => {
                    if self.pending_cut.as_ref().is_some_and(|c| c.id == cut) {
                        self.pending_cut = None;
                    }
                }
                PartState::Complete => {
                    if self.ready_cut.as_ref().is_some_and(|(c, _)| c.id == cut) {
                        self.ready_cut = None;
                    }
                }
                PartState::Prepared | PartState::Failed => {}
            }
            if committed && run.state != PartState::Prepared {
                log::error!("world cut {}: committed, but this host's part was never prepared", cut.0);
            }
        }
        self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
        self.early_marks.retain(|_, c| *c > cut);
        self.world_cut_done.push((cut, committed));
    }

    /// The pending cut `id` was dropped here (a region missed or was lost, a deadline):
    /// if it is the world cut's part, the part fails (`why`).
    pub(super) fn world_cut_dropped(&mut self, id: CutId, why: CutFailure) {
        if self.world_cut.as_ref().is_some_and(|r| r.cut == id && r.state == PartState::Pending) {
            self.world_cut_fail(why);
        }
    }

    /// The part of the world cut, not complete yet, fails: the cut is dropped here, the
    /// coordinator is told, and the part stays `Failed` until its `CutDone`.
    pub(super) fn world_cut_fail(&mut self, why: CutFailure) {
        let Some(run) = self.world_cut.as_mut().filter(|r| r.state == PartState::Pending) else { return };
        run.state = PartState::Failed;
        let (cut, beginning) = (run.cut, run.beginning);
        log::warn!("world cut {}: this host's part failed ({why:?})", cut.0);
        if self.pending_cut.as_ref().is_some_and(|c| c.id == cut) {
            self.pending_cut = None;
        }
        if !beginning {
            self.world_send(HostMsg::CutFailed { cut, why });
        }
    }

    /// Every region answered pending cut `id`: for a world cut, this host's marks are
    /// queued now; whether every participant's mark is in (a one-phase cut: always).
    pub(super) fn world_cut_marks_in(&mut self, id: CutId) -> bool {
        let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == id) else { return true };
        if !run.mark_sent {
            run.mark_sent = true;
            let peers: Vec<u32> = run.peers.iter().copied().collect();
            log::debug!("[server] world cut {}: every region answered; marks to {peers:?}", id.0);
            for p in peers {
                self.queue_now(p, Reliable::CutMark { cut: id });
            }
        }
        let run = self.world_cut.as_ref().expect("checked");
        run.peers.is_subset(&run.marks_in)
    }

    /// The other participants of world cut `id` (empty for a one-phase cut).
    pub(super) fn world_cut_peers(&self, id: CutId) -> BTreeSet<u32> {
        self.world_cut.as_ref().filter(|r| r.cut == id).map(|r| r.peers.clone()).unwrap_or_default()
    }

    /// The pending cut `id` is complete (`ready_cut`).
    pub(super) fn world_cut_complete(&mut self, id: CutId) {
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == id) {
            run.state = PartState::Complete;
        }
    }

    /// How `msg` to `ordinal` goes (the module docs' cases): `(msg, Some(cut))` wrapped in
    /// [`Reliable::InCut`], `(msg, None)` bare. Case 3 writes the message into this part
    /// and raises its stamp / due to the cut tick; case 2 notes a player as the
    /// receiver's. To a host outside the cut, or a non-state message: bare (the lineage of
    /// a wrapped message being applied aside).
    pub(super) fn world_cut_wrap(&mut self, ordinal: u32, mut msg: Reliable, view: bool) -> (Reliable, Option<CutId>) {
        if !msg.is_state() || matches!(msg, Reliable::InCut { .. }) {
            return (msg, None);
        }
        let Some(run) = self.world_cut.as_ref().filter(|r| r.peers.contains(&ordinal)) else {
            // Made while a wrapped message is applied: accounted where that one is.
            return (msg, self.inbound_accounted);
        };
        let (cut, tick) = (run.cut, run.tick);
        match run.state {
            PartState::Complete | PartState::Prepared | PartState::Failed => (msg, Some(cut)),
            PartState::Pending if view => (msg, Some(cut)),
            PartState::Pending if !run.mark_sent => {
                if let Some(player) = reliable_player(&msg) {
                    if let Some(pending) = self.pending_cut.as_mut() {
                        pending.not_ours.insert(player, None);
                    }
                }
                (msg, None)
            }
            PartState::Pending => {
                raise_to(&mut msg, tick);
                self.keep_in_part(&msg);
                (msg, Some(cut))
            }
        }
    }

    /// Case 3: `msg`, sent after this host's mark and in no snapshot of it, is this part's —
    /// an in-flight row, or a player's record.
    fn keep_in_part(&mut self, msg: &Reliable) {
        let row = match msg {
            Reliable::Transfer { target, stamp, bundle, player: None } => {
                Some(InFlightRow::Transfer { target: *target, bundle: (**bundle).clone(), stamp: *stamp })
            }
            Reliable::Intent(m) => Some(InFlightRow::Intent { target: peers::peer_intent_target(m), msg: *m }),
            _ => None,
        };
        let record = match msg {
            Reliable::Transfer { target, bundle, player: Some(h), .. } => {
                peers::handoff_record(*target, bundle, h).map(|r| (h.player, r))
            }
            Reliable::PlayerMove { handoff, record, .. } => Some((handoff.player, (**record).clone())),
            _ => None,
        };
        self.cut_order += 1;
        let order = self.cut_order;
        let Some(pending) = self.pending_cut.as_mut() else { return };
        if let Some(row) = row {
            pending.rows.push(row);
        }
        if let Some((player, record)) = record {
            pending.not_ours.remove(&player);
            pending.crossing_players.push((order, player, record));
        }
    }

    /// Whether what a wrapper of `cut` creates is out of this host's pending cut (it is
    /// that cut).
    pub(super) fn accounted_now(&self, cut: CutId) -> bool {
        self.pending_cut.as_ref().is_some_and(|c| c.id == cut)
    }

    /// A wrapper of cut `cut` came: if that cut is not this host's world cut, this host is
    /// too late for it (and every earlier one) — [`Self::begin_world_cut`] answers `Missed`.
    pub(super) fn note_wrapper(&mut self, cut: CutId) {
        if self.world_cut.as_ref().is_none_or(|r| r.cut != cut) {
            self.wrapper_seen = Some(self.wrapper_seen.map_or(cut, |w| w.max(cut)));
        }
    }

    /// A wrapped player-bearing `msg` of cut `cut`, pending here: the player is its
    /// sender's in that cut; what this part already held of it at the cut tick (a record it
    /// passed through with before it left) stays.
    pub(super) fn note_player_accounted(&mut self, msg: &Reliable, cut: CutId) {
        let Some(player) = reliable_player(msg) else { return };
        if let Some(pending) = self.pending_cut.as_mut().filter(|c| c.id == cut) {
            let had = pending.transit_players.get(&player).or(pending.parked_at_t.get(&player)).cloned();
            pending.not_ours.entry(player).or_insert(had);
        }
    }

    /// A BARE player-bearing `msg` arrived: the player is this host's in the pending cut
    /// (it may have left bare before — case 2 noted it as the receiver's).
    pub(super) fn note_player_returned(&mut self, msg: &Reliable) {
        let Some(player) = reliable_player(msg) else { return };
        if let Some(pending) = self.pending_cut.as_mut() {
            pending.not_ours.remove(&player);
        }
    }

    /// Whether `from`'s mark of the pending world cut came in (and the part is not complete).
    pub(super) fn after_mark(&self, from: u32) -> bool {
        self.world_cut.as_ref().is_some_and(|r| r.state == PartState::Pending && r.marks_in.contains(&from))
    }

    /// Host `from`'s mark of world cut `cut`.
    pub(super) fn peer_cut_mark(&mut self, from: u32, cut: CutId) {
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == cut) {
            if run.peers.contains(&from) {
                run.marks_in.insert(from);
                self.try_complete_cut();
            } else {
                log::warn!("world cut {}: a mark from host {from}, which is not in it; ignored", cut.0);
            }
            return;
        }
        if self.world_cut_seen.is_some_and(|s| cut <= s) {
            log::debug!("world cut {}: host {from}'s mark of a cut over here; ignored", cut.0);
            return;
        }
        // Its `BeginCut` has not come yet: counted when it does.
        self.early_marks.insert(from, cut);
    }

    /// The Attach's pending world cut (its outcome is asked for).
    pub(super) fn attach_pending_cut(&self) -> Option<CutId> {
        self.world_cut.as_ref().map(|r| r.cut)
    }

    /// Test support: apply `msg` as if host `from` had sent it on its peer session (the
    /// inbound path a session's `Reliable` event takes).
    #[doc(hidden)]
    pub fn apply_peer_reliable(&mut self, from: u32, msg: Reliable) {
        self.peer_reliable(from, msg);
        self.flush_world();
    }

    /// Test support: send `msg` to host `ordinal` as the manager sends a state message
    /// (`view`: departed after its source answered the pending cut) — through the world
    /// cut's wrapper rule.
    #[doc(hidden)]
    pub fn send_reliable_for_test(&mut self, ordinal: u32, msg: Reliable, view: bool) {
        self.send_reliable(ordinal, msg, view);
    }

    /// Test support: every crossing in flight with the world cut its sender accounts for it
    /// in (`None`: this host's).
    #[doc(hidden)]
    pub fn in_flight_accounted(&self) -> Vec<(crate::EntityId, Option<CutId>)> {
        self.in_flight.iter().map(|(id, f)| (*id, f.accounted_in)).collect()
    }

    /// Test support: the pending world cut part's state, as text (`None`: none).
    pub fn world_cut_state(&self) -> Option<(CutId, &'static str)> {
        self.world_cut.as_ref().map(|r| {
            (r.cut, match r.state {
                PartState::Pending if r.mark_sent => "marked",
                PartState::Pending => "pending",
                PartState::Complete => "complete",
                PartState::Prepared => "prepared",
                PartState::Failed => "failed",
            })
        })
    }
}

/// The player a reliable message hands over, if any.
fn reliable_player(msg: &Reliable) -> Option<PlayerId> {
    match msg.inner() {
        Reliable::Transfer { player: Some(h), .. } => Some(h.player),
        Reliable::PlayerMove { handoff, .. } => Some(handoff.player),
        _ => None,
    }
}

/// A transfer's stamp, an intent's or reply's due, raised to at least `tick`.
fn raise_to(msg: &mut Reliable, tick: crate::Tick) {
    match msg {
        Reliable::Transfer { stamp, .. } => *stamp = (*stamp).max(tick),
        Reliable::Intent(crate::IntentMsg::Intent(s)) => s.due = s.due.max(tick),
        Reliable::Intent(crate::IntentMsg::Reply(s)) => s.due = s.due.max(tick),
        _ => {}
    }
}
