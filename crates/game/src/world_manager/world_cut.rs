//! The host's half of a world cut (several hosts, R3 Task 13; plan decision 6 and the
//! controller's ruling `task-13-ruling.md`). The coordinator picks one world tick T
//! ([`crate::CoordMsg::BeginCut`]); every participant snapshots its regions at T exactly as
//! a one-phase cut does (`save_cut.rs`, [`WorldManager::begin_cut_at`]) and, once every
//! region has answered AND its world tick has reached T, queues a [`Reliable::CutMark`] to
//! every other participant (checked on every answer and every upkeep, so an idle host
//! marks at T). A host's part is complete when it also holds every other participant's
//! mark.
//!
//! **The mark is never sent before T.** A host with no running region would otherwise
//! mark at once, up to a whole lead early; a region it starts in that window starts before
//! T, is asked for the cut, and sends before it answers — with its source not answered
//! (`view == false`) but after the mark. Marking no earlier than T, everything a region
//! sends before its answer goes before the mark (bare, the receiver's), and everything
//! after the mark left a region that had answered (`view`): no region-sourced message is
//! ever case 3 below.
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
//! 3. not `view`, A's mark already queued — only a message the manager makes (a refusal of
//!    an intent that came after the mark; transfers and moves are kept here defensively:
//!    no path makes one after the mark today) → A keeps it in its own
//!    part — a row (transfer, intent, reply) or a player record (hand-off, move) — raises
//!    its stamp / due to at least T (no receiver region applies it before its snapshot) and
//!    sends it wrapped.
//! After A's part is complete (until its `CutDone`) everything is case 1. A message made
//! while a wrapped one is applied carries the same wrapper (its lineage is accounted).
//!
//! The receiver applies a wrapped M normally but notes what it creates as accounted for by
//! the sender in K (`in_source_view`, `accounted_in` on the entry, `not_ours` for a
//! player): never written by this host's part of K. A wrapper of K that arrives before
//! this host's `BeginCut { K }` makes this host miss K (`wrapper_seen`: a peer's region is
//! already past T). A bare state message after a participant's mark of the pending cut is
//! a protocol violation: applied, and the part fails. A player this host sent bare and
//! that comes back bare is this host's again (`sent_bare`).
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
//!
//! **The peer journal** (R4, `peer/journal.rs`) is this ownership read backwards: every
//! transfer and player move queued to (`queue_now`) or received from (`peer_reliable`) a
//! peer, with this host's marks and the peer's marks in between, is kept until a committed
//! cut explains it — at `CutDone` (`world_cut_decided`) the entries that crossed bare (or
//! wrapped in another cut) before the mark of their direction are forgotten.
//!
//! **Leave cuts** (R3 Task 14). A host about to exit asks for one
//! ([`WorldManager::request_leave`]); on the `BeginCut` naming it in `leaving` it parks every
//! player and region (their `Stopped` answers are the cut's; nothing is woken for it any
//! more: a hand-off or move to it parks the player here). For every pair of participants
//! the leave joins (the leaver and each other one), everything one sends the other from its
//! `BeginCut` until the cut is decided is that sender's (case 1, or case 3 at once — never
//! bare: [`WorldManager::world_cut_wrap`]) and is HELD BACK on the sender
//! ([`WorldManager::hold_for_leave`]); the pair is out of reach meanwhile (no hand-off,
//! intent or subscribe goes to it). Holding back is what keeps a wrapped message out of the
//! leaver: it would apply it and take it away when it exits, while the sender's snapshot
//! holds it and its later cuts no longer do. The marks still go out on the session as
//! usual. Committed: each side holds its session to the other and forgets its mark and
//! everything before it (the other side received them all — its part needed the mark;
//! [`crate::PeerSession::drop_through_mark`]), then queues what it held back behind it: the
//! staying hosts keep it owed to the leaver's NEXT boot (their rows meanwhile: the leaver is
//! absent from their cuts) and release the session on that boot's hello. Aborted: what was
//! held back goes out as usual.

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

/// How many of its latest world cuts' marks a host remembers ([`WorldManager::marks_queued`]).
pub const MARKS_QUEUED_KEPT: usize = 16;

/// A peer joined to this host by a leave of the pending world cut (one of the two leaves):
/// what this host sends it from its `BeginCut` until the cut is decided.
#[derive(Debug)]
pub(super) struct LeavePair {
    pub cut: CutId,
    /// Held back, in order (each already wrapped: this host accounts for it in `cut`).
    pub held: Vec<Reliable>,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// Take part in world cut `cut` at world tick `tick` with `hosts` (`leaving`: the
    /// hosts leaving in it, among them — a leaving host parks everything for it; every pair
    /// a leave joins holds back what it sends: the module docs). `Missed` when `tick` is
    /// closer than [`WORLD_CUT_MIN_LEAD_TICKS`] or past; `Aborted` while another cut is
    /// pending here (it is left untouched), when a participant is not reachable, a region
    /// thread is dead, or this host is named leaving without having asked.
    pub fn begin_world_cut(
        &mut self,
        cut: CutId,
        tick: crate::Tick,
        hosts: &[u32],
        leaving: &[u32],
    ) -> Result<(), CutFailure> {
        let me = self.ordinal();
        if leaving.contains(&me) && self.leave_requested {
            // This attempt's cut, whether this host manages to take part or not: its
            // outcome (an abort, if it fails here) is what the exit waits for.
            self.leaving_cut = Some(cut);
            self.leave_outcome = None;
            self.leave_decided = false;
        }
        if self.world_cut.is_some() || self.pending_cut.is_some() || self.ready_cut.is_some() {
            log::warn!("world cut {}: another cut is still pending here; refused", cut.0);
            return Err(CutFailure::Aborted);
        }
        self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
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
        let all: BTreeSet<u32> = hosts.iter().chain(leaving).copied().collect();
        if !all.contains(&me) {
            log::error!("world cut {}: host {me} is not among its hosts {hosts:?}; refused", cut.0);
            return Err(CutFailure::Aborted);
        }
        let i_leave = leaving.contains(&me);
        if i_leave && !self.leave_requested {
            log::error!("world cut {}: names host {me} leaving, which it never asked; refused", cut.0);
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
        // A leave joins this host and a peer when either leaves: everything this host sends
        // that peer from now until the cut is decided waits here, in this host's part.
        let peers: Vec<u32> = self.world_cut_peers(cut).into_iter().collect();
        for p in peers.into_iter().filter(|p| i_leave || leaving.contains(p)) {
            // A pair of an earlier cut is ended when that cut is decided, and no cut begins
            // before that: one still here would be a lost `CutDone` — what it held goes
            // out (as for an abort) rather than being overwritten.
            if let Some(stale) = self.leave_pairs.insert(p, LeavePair { cut, held: Vec::new() }) {
                log::error!("[server] a leave pair with host {p} of cut {} was never ended; what it held goes out", stale.cut.0);
                for msg in stale.held {
                    self.queue_now(p, msg);
                }
            }
        }
        if i_leave {
            // Everything here parks for it: the regions' `Stopped` answers are the cut's.
            log::info!("[server] world cut {} at tick {tick}: this host leaves in it; parking everything", cut.0);
            self.leave_parked = true;
            let now_ms = self.clock_ms;
            self.park_all_players(now_ms);
            self.shutdown_all();
        }
        log::debug!("[server] world cut {} at tick {tick} begun", cut.0);
        self.try_complete_cut();
        Ok(())
    }

    /// This host is about to exit (several hosts): it asks the coordinator for a cut it
    /// leaves in ([`crate::HostMsg::LeaveRequest`]; sent with the next `Attach` if the link
    /// is down). The server then waits for [`Self::leave_outcome`].
    pub fn request_leave(&mut self) {
        if !self.leave_requested {
            log::info!("[server] asking the coordinator for a leave cut");
        }
        self.leave_requested = true;
        self.world_send(HostMsg::LeaveRequest);
    }

    /// The world cut of this host's current leave attempt (a `BeginCut` named it leaving),
    /// whether its part could be taken or not.
    pub fn leaving_cut(&self) -> Option<CutId> {
        self.leaving_cut
    }

    /// This host parked everything for a leave cut (it stays parked: it is exiting).
    pub fn leave_parked(&self) -> bool {
        self.leave_parked
    }

    /// The leave cut was aborted and the coordinator's `CutDone` of it has come: ask for
    /// another one (`true`). `false` while there is nothing to retry yet.
    pub fn retry_leave(&mut self) -> bool {
        if !self.leave_requested || self.leave_outcome != Some(false) || !self.leave_decided {
            return false;
        }
        log::warn!(
            "[server] the leave cut {} was aborted: asking for another",
            self.leaving_cut.map_or(0, |c| c.0)
        );
        self.leaving_cut = None;
        self.leave_outcome = None;
        self.leave_decided = false;
        self.world_send(HostMsg::LeaveRequest);
        true
    }

    /// Whether the `Attach` must ask for a leave cut again: one is asked for and none has
    /// named this host yet (the coordinator forgets a request when the link drops).
    pub(super) fn leave_pending_request(&self) -> bool {
        self.leave_requested && self.leaving_cut.is_none()
    }

    /// The leave cut's outcome once the server settled it ([`Self::cut_committed`] /
    /// [`Self::cut_failed`]): `true` = this host's part is in the world, it may exit clean.
    pub fn leave_outcome(&self) -> Option<bool> {
        self.leave_outcome
    }

    /// The server settled world cut `id` (`committed` or not): the leave cut's outcome.
    pub(super) fn note_leave_settled(&mut self, id: CutId, committed: bool) {
        if self.leaving_cut == Some(id) && self.leave_outcome.is_none() {
            self.leave_outcome = Some(committed);
        }
    }

    /// Whether `ordinal` is out of reach for a leave: joined by the pending world cut's
    /// leave, or left (until its next boot). Nothing new is handed to it.
    pub(super) fn leave_blocks(&self, ordinal: u32) -> bool {
        self.leave_pairs.contains_key(&ordinal) || self.left_peers.contains_key(&ordinal)
    }

    /// `msg` to `ordinal`, joined by a leave of the pending world cut: held back (`None`)
    /// until the cut is decided; anything else is given back to be queued.
    pub(super) fn hold_for_leave(&mut self, ordinal: u32, msg: Reliable) -> Option<Reliable> {
        match self.leave_pairs.get_mut(&ordinal) {
            Some(pair) => {
                pair.held.push(msg);
                None
            }
            None => Some(msg),
        }
    }

    /// World cut `cut` is decided: each leave it joined this host in ends. Committed: the
    /// session is held (the leaver's process is gone or going), what it holds up to this
    /// host's mark is forgotten (every entry before the mark reached the other side: its
    /// part needed the mark) and what was held back follows, owed to the leaver's next boot
    /// (this host's rows meanwhile). Aborted: what was held back goes out now.
    fn end_leaves(&mut self, cut: CutId, committed: bool) {
        let pairs: Vec<u32> = self.leave_pairs.iter().filter(|(_, p)| p.cut == cut).map(|(o, _)| *o).collect();
        let me_leaving = self.leaving_cut == Some(cut);
        for ordinal in pairs {
            let pair = self.leave_pairs.remove(&ordinal).expect("listed");
            if let Some(peer) = self.peers.get_mut(&ordinal) {
                if committed {
                    peer.session.hold();
                    peer.session.drop_through_mark(cut);
                } else {
                    peer.session.release();
                }
            }
            if committed && !me_leaving {
                let boot = self.peer_boots.get(&ordinal).copied();
                log::info!(
                    "[server] host {ordinal} left in world cut {}: {} message(s) wait for its return",
                    cut.0,
                    pair.held.len()
                );
                self.left_peers.insert(ordinal, boot);
            }
            for msg in pair.held {
                self.queue_now(ordinal, msg);
            }
        }
    }

    /// Hosts the world store says left in a committed leave cut (their `hosts` row is
    /// clean), each with the boot that left: the server reads them at boot, before any
    /// placement, and this host treats them as it treats a leave it saw (`end_leaves`) —
    /// nothing is handed to them, and a spawn order does not wait for their sync (they hold
    /// nobody). A host that crashed (a dirty row) is not one of them: it stays expected.
    /// One that comes back says so with another boot's hello (`peer_hello_boot`), and is
    /// expected again from then on — a clean row read at boot is never the row of a boot
    /// that runs: every boot clears its row before it dials or listens. Called before the
    /// first [`Self::maintain`] (no hello read yet).
    pub fn note_left_peers(&mut self, left: impl IntoIterator<Item = (u32, u64)>) {
        let me = self.ordinal();
        for (ordinal, boot) in left {
            if ordinal == me {
                continue;
            }
            log::info!("[server] host {ordinal} left the world cleanly (boot {boot}): not awaited until it returns");
            self.left_peers.insert(ordinal, Some(boot));
        }
    }

    /// Host `from`'s hello of boot `boot`: a host that left comes back as another boot —
    /// its held session goes out again.
    pub(super) fn peer_hello_boot(&mut self, from: u32, boot: u64) {
        self.peer_boots.insert(from, boot);
        let back = self.left_peers.get(&from).is_some_and(|left_as| *left_as != Some(boot));
        if back {
            self.left_peers.remove(&from);
            if let Some(peer) = self.peers.get_mut(&from) {
                log::info!("[server] host {from} is back (boot {boot}): what waited for it goes out");
                peer.session.release();
            }
        }
    }

    /// Test support: whether this host's session with `ordinal` is held for a leave.
    #[doc(hidden)]
    pub fn peer_left(&self, ordinal: u32) -> bool {
        self.left_peers.contains_key(&ordinal)
    }

    /// Test support: the reliable messages this host owes `ordinal` (its session's and the
    /// ones a leave holds back), in order.
    #[doc(hidden)]
    pub fn peer_owed(&self, ordinal: u32) -> Vec<Reliable> {
        let mut out: Vec<Reliable> =
            self.peers.get(&ordinal).map(|p| p.session.unreceived().cloned().collect()).unwrap_or_default();
        out.extend(self.peer_waiting.get(&ordinal).into_iter().flatten().cloned());
        out.extend(self.leave_pairs.get(&ordinal).into_iter().flat_map(|p| p.held.iter().cloned()));
        out
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
        // The journal forgets what the committed cut explains toward its participants (an
        // abort forgets only its marks).
        let participants = self.world_cut_peers(cut);
        if committed {
            self.journal.committed(cut, &participants);
        } else {
            self.journal.aborted(cut);
        }
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
        self.end_leaves(cut, committed);
        if self.leaving_cut == Some(cut) {
            self.leave_decided = true;
        }
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
        let world_tick = self.spawner.world_tick();
        let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == id) else { return true };
        if !run.mark_sent && world_tick < run.tick {
            // Never before T (the module docs); the upkeep tries again.
            return false;
        }
        if !run.mark_sent {
            run.mark_sent = true;
            let peers: Vec<u32> = run.peers.iter().copied().collect();
            log::debug!("[server] world cut {}: every region answered; marks to {peers:?}", id.0);
            let now = self.spawner.world_tick();
            self.marks_queued.push((id, now));
            if self.marks_queued.len() > MARKS_QUEUED_KEPT {
                self.marks_queued.remove(0);
            }
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
        // To (or from) a host leaving in this cut nothing goes bare: it is held back until
        // the cut is decided (`hold_for_leave`), so it is this part's (case 1 or 3).
        let leave = self.leave_pairs.contains_key(&ordinal);
        match run.state {
            PartState::Complete | PartState::Prepared | PartState::Failed => (msg, Some(cut)),
            PartState::Pending if view => (msg, Some(cut)),
            PartState::Pending if !run.mark_sent && !leave => {
                if let Some(player) = reliable_player(&msg) {
                    if let Some(pending) = self.pending_cut.as_mut() {
                        pending.not_ours.insert(player, None);
                        pending.sent_bare.insert(player);
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

    /// Case 3: `msg`, sent after this host's mark (or, between two hosts a leave joins, at
    /// any time before the decision) and in no snapshot of it, is this part's — an in-flight
    /// row, or a player's record.
    ///
    /// Outside a leave only a manager-made message gets here (marks wait for T, so a
    /// region-sourced one is never case 3), and the transfer and player branches are
    /// defensive. Between leave pairs region-sourced transfers and moves that left before
    /// their source answered come here too (`world_cut_wrap`), held back meanwhile. A
    /// person's bundle is never a row (the history respawns people, as for `cut_crossing`).
    fn keep_in_part(&mut self, msg: &Reliable) {
        let row = match msg {
            Reliable::Transfer { bundle, .. } if matches!(bundle.kind, crate::EntityKind::Person { .. }) => None,
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
            // Its sender accounts for it now, whatever this host sent before.
            pending.sent_bare.remove(&player);
        }
    }

    /// A BARE player-bearing `msg` arrived: a player this host sent bare (case 2 noted it
    /// as the receiver's) is this host's again in the pending cut. A note made because a
    /// wrapped message brought it stays.
    pub(super) fn note_player_returned(&mut self, msg: &Reliable) {
        let Some(player) = reliable_player(msg) else { return };
        if let Some(pending) = self.pending_cut.as_mut() {
            if pending.sent_bare.remove(&player) {
                pending.not_ours.remove(&player);
            }
        }
    }

    /// Whether `from`'s mark of this host's world cut came in while the part has failed
    /// here (a bare message after it is expected then: the cut is dead).
    pub(super) fn after_mark_failed(&self, from: u32) -> bool {
        self.world_cut.as_ref().is_some_and(|r| r.state == PartState::Failed && r.marks_in.contains(&from))
    }

    /// Whether `from`'s mark of the pending world cut came in (and the part is not complete).
    pub(super) fn after_mark(&self, from: u32) -> bool {
        self.world_cut.as_ref().is_some_and(|r| r.state == PartState::Pending && r.marks_in.contains(&from))
    }

    /// Host `from`'s mark of world cut `cut`.
    pub(super) fn peer_cut_mark(&mut self, from: u32, cut: CutId) {
        self.journal.mark_in(from, cut);
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

    /// Test support: the latest world cuts this host queued its marks of (at most
    /// [`MARKS_QUEUED_KEPT`]), with the world tick it did so at.
    #[doc(hidden)]
    pub fn marks_queued(&self) -> &[(CutId, crate::Tick)] {
        &self.marks_queued
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
