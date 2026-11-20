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
//! to its owner again on restore ([`WorldManager::restore_in_flight`]).
//!
//! **4. Counted across a down link** (R4 Task 12, plan decision 9). A mark is only a
//! position in a numbered stream: "everything before it reached the receiver". When the
//! link between A and a participant B is down at A's `BeginCut`, or goes down while A's part
//! is pending, or B's count comes first, the pair is in COUNT MODE for cut K: A freezes its
//! session with B ([`crate::PeerSession::freeze`] — nothing read, nothing sent, no new
//! connection taken up, until K's `CutDone`), and reports what it received of B's numbering
//! ([`crate::PeerSession::received_through`], with the B boot it counted) to the coordinator
//! ([`crate::HostMsg::CutPeerCount`]), which relays it to B. From then on what A sends B is
//! held back (the leave pairs' path, `hold_for_leave`: never bare — case 1, or case 3 at
//! once). A's count c_A is the stream position "everything up to here reached A, nothing
//! after it ever will before K is decided" — exactly what B's mark would have told A, at
//! a position A chose instead of B: **a count is a mark of the receiver's making**, and
//! because the receiver is frozen from the moment it counts, the position cannot move.
//! So for M in B's numbering toward A:
//! - numbered ≤ c_A: A received it before freezing — the bare rule makes it A's (A's
//!   snapshot if applied by T, else A's row / record, `in_source_view = false`), unless it
//!   travelled wrapped in K (B's, as ever);
//! - numbered > c_A: A never gets it before `CutDone` — B writes it in its part as for an
//!   absent peer (`cut_counted`: a row or player record, its stamp / due raised to T), unless
//!   it travelled wrapped in K (B's snapshot or case-3 row holds it already).
//! A relayed count of ANOTHER process of B's (its `boot` not B's own) counted a numbering
//! that no longer exists: A received nothing of B's current one, so B counts 0 — everything
//! owed is B's. A part completes with, per participant, its `CutMark` or — counted with —
//! its relayed count. The relayed count is also the peer's commitment to write what is
//! above this host's count, so a pair in count mode always needs it, even when the peer's
//! mark came before the freeze: that mark settles only the peer's direction (a mark already
//! received wins over the count for what it covers — the count lies past it, the two agree).
//! A part complete by marks when a peer's count arrives can never write what that peer
//! lacks of it: it fails the cut (`CutFailed`, wait — never guess). Nothing waits for a dead
//! coordinator: no count travels, and its detach aborts the cut as before.
//!
//! **Holds that meet on one session.** The freeze composes with the other holds; it never
//! touches them, and they never thaw it:
//!
//! | session is also…                                   | what wins                                  |
//! |----------------------------------------------------|--------------------------------------------|
//! | in a leave pair of the same cut (the peer leaves, or this host does) | frozen until `CutDone`; what is sent meanwhile is held by the LEAVE pair; committed: held, entries ≤ the count dropped (`drop_through`, not through the mark: the leaver may lack it), the rest owed to the leaver's next boot; then thawed (still held) |
//! | held for a peer that left (`left_peers`, its new boot a participant) | frozen until `CutDone`, then thawed and still held — its release stays the leave's (`peer_hello_boot` / `returning_owns`) |
//! | declared dead (a succession)                        | the coordinator aborts a running cut with a lapsed participant before declaring, and `CutDone` precedes the order on its link: count mode ends first; an order meeting a count pair anyway thaws it and reports what it held |
//! | a new boot's hello while frozen                    | not read until thawed: the numbering a count refers to never changes |
//! | an heir's `inheriting` mask (`succession.rs`)       | cannot meet a counted cut that commits: no cut begins between an order and the heir's adoption (the coordinator's `successions_block_cuts`), and the mask is lifted at adoption. The one overlap: an order applied while a count pair of the cut before it still lives (that cut is decided — aborted, or committed and learned from the store's poll — its `CutDone` still on its way): its part is taken or dropped already, so the pair only waits to thaw (a pair with the predecessor is thawed and reported by the order itself). Either way the mask gates `owner_of`, never a session: what waits on the heir for its land (`unowned_waiting`) is in no session and no count touches it — it is the heir's own row in any cut (`cut_peer_rows`) |
//!
//! **Aborts.** A participant running a newer generation while the part is not complete, a
//! region missing the tick or lost, a deadline, or another cut still pending fail the part
//! ([`crate::HostMsg::CutFailed`]); the coordinator then aborts the cut for every
//! participant. A link that goes down is counted across instead (case 4). A complete part
//! stays until its [`crate::CoordMsg::CutDone`] ([`WorldManager::take_world_cut_done`]).
//!
//! **The peer journal** (R4, `peer/journal.rs`) is this ownership read backwards: every
//! transfer and player move queued to (`queue_now`) or received from (`peer_reliable`) a
//! peer, with this host's marks and the peer's marks in between, is kept until a committed
//! cut explains it — at `CutDone` (`world_cut_decided`) the entries that crossed bare (or
//! wrapped in another cut) before the mark of their direction are forgotten; for a pair
//! counted with, by the counts instead ([`crate::PeerJournal::counted`]: a sent entry above
//! the peer's count stays, received ones inside this host's count go). Every participant,
//! counted with or not, is recorded at `BeginCut` (`PeerJournal::cut_begun`).
//!
//! **Hosts outside the cut** (Elastic 1 Task 4). A host that registered after the cut began
//! (the store recorded the begin before its id was issued: `world_store` `record_cut_begin`)
//! is not in it and does not abort it: the decision may leave it out. It owned nothing when
//! it registered (another host was alive), so what it may hold at T can only have come from a
//! participant — and no participant lets that happen:
//! - at its `BeginCut` a participant refuses its part if it exchanged a state message
//!   (transfer, player move, intent, reply; `unsaved_traffic`) with a host outside the cut
//!   since the last committed cut both took part in (left and succeeded hosts aside: their own
//!   rules hold) — what that host holds may be this cut's;
//! - from its `BeginCut` until the cut's `CutDone` a participant FREEZES its session with
//!   every host outside the cut (one whose session is made meanwhile, `set_peer`, too):
//!   nothing is sent or read. What it queues for such a host stays in its session and is this
//!   part's row or record exactly as for an absent peer (`cut_peer_rows`,
//!   `cut_peer_players`); what that host sends waits in ITS session (its own state, in no part
//!   of this cut: it is in the next one).
//! So nothing of the cut's state crosses to a host outside it before the decision: a restore
//! from the cut has every entity, item and player once, and the newcomer's land — regions it
//! claimed — is untouched by the cut (a part writes only its own regions). The journal does
//! not truncate toward a host outside the cut (`cut_begun` names the participants only). The
//! next cut lists the newcomer (its dirty row is read by the coordinator).
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
//! held back goes out as usual. Under `claim` (R4 drain) a host that needs land of a host
//! that left — a message held for it, a spawn placement — asks the coordinator for it
//! (`land.rs` `ask_for_land`, [`crate::HostMsg::LandNeeded`]); its `Leave` succession hands
//! the held messages to the heir instead (`succession.rs`). Under `Static` nothing changes.

use super::*;
use crate::HostId;
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
    pub peers: BTreeSet<HostId>,
    /// The participants whose mark arrived.
    pub marks_in: BTreeSet<HostId>,
    /// This host's marks are queued.
    pub mark_sent: bool,
    pub state: PartState,
    /// Inside [`WorldManager::begin_world_cut`]: a failure there is answered by its caller
    /// (one `CutFailed`).
    pub beginning: bool,
    /// Hosts outside the cut whose session this host froze for it (the module docs, "Hosts
    /// outside the cut"): thawed at its `CutDone`.
    pub outsiders: BTreeSet<HostId>,
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
    /// The peer leaves in `cut` (else this host does): the boot that leaves (its latest
    /// hello at the `BeginCut`) and, under `claim`, the land this host's directory gave it
    /// then (`None`: not read yet — unknown, never filled later). Taken when the pair is
    /// made: the cut cannot commit without this host's part and no `Leave` succession can
    /// precede that commit, so the directory still shows the leaver's land here — a
    /// `CutDone` learned late may come after a declaration moved it.
    pub leaver: Option<(Option<u64>, Option<BTreeSet<crate::SuperRegion>>)>,
}

/// A participant the pending world cut runs with in count mode (the module docs' fourth
/// case): their link was down at the `BeginCut`, went down while this part was pending, or
/// the peer's own count came first. This host's session with it is frozen until the cut's
/// `CutDone`, its count of the peer's numbering went to the coordinator
/// ([`crate::HostMsg::CutPeerCount`]).
#[derive(Debug)]
pub(super) struct CountPair {
    pub cut: CutId,
    /// The peer's count of this host's numbering, relayed (`Some(0)` for a count of another
    /// boot of this host): everything numbered above it is this part's.
    pub theirs: Option<u64>,
    /// What this host sent the peer from count mode on, held back until the cut is decided
    /// (each wrapped: this part's, case 1 or 3) — unless a leave pair holds it.
    pub held: Vec<Reliable>,
}

impl<S: RegionSpawner> WorldManager<S> {
    /// Take part in world cut `cut` at world tick `tick` with `hosts` (`leaving`: the
    /// hosts leaving in it, among them — a leaving host parks everything for it; every pair
    /// a leave joins holds back what it sends: the module docs). `Missed` when `tick` is
    /// closer than [`WORLD_CUT_MIN_LEAD_TICKS`] or past; `Aborted` while another cut is
    /// pending here (it is left untouched), when a region thread is dead, or this host is
    /// named leaving without having asked. A participant that is not reachable is counted
    /// with (the module docs' fourth case).
    pub fn begin_world_cut(
        &mut self,
        cut: CutId,
        tick: crate::Tick,
        hosts: &[HostId],
        leaving: &[HostId],
        moves: &[crate::LandMove],
    ) -> Result<(), CutFailure> {
        let me = self.ordinal();
        if hosts.iter().chain(leaving).any(|h| *h == me) {
            // Named in it: a succession based on this cut waits for its decision here, whether
            // this part begins or not (`try_apply_succession`).
            self.cuts_joined_max = Some(self.cuts_joined_max.map_or(cut, |j| j.max(cut)));
        }
        if leaving.contains(&me) && self.leave_requested {
            // This attempt's cut, whether this host manages to take part or not: its
            // outcome (an abort, if it fails here) is what the exit waits for.
            self.leaving_cut = Some(cut);
            self.leave_outcome = None;
            self.leave_decided = false;
        }
        if self.world_cut.is_some() || self.pending_cut.is_some() || self.ready_cut.is_some() {
            log::warn!(
                "world cut {}: another cut is still pending here; refused",
                cut.0
            );
            return Err(CutFailure::Aborted);
        }
        self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
        let now = self.spawner.world_tick();
        if tick < now + WORLD_CUT_MIN_LEAD_TICKS {
            log::warn!(
                "world cut {} at tick {tick}: the world tick is {now} already; missed",
                cut.0
            );
            return Err(CutFailure::Missed);
        }
        // A wrapper of this cut (or a later one) already came: a peer's region is past the
        // tick, this host is at the edge of missing it — it does not take part ("wait,
        // never guess"); the coordinator doubles the lead.
        if self.wrapper_seen.is_some_and(|w| w >= cut) {
            log::warn!(
                "world cut {}: a peer's wrapped message of it came before its BeginCut; missed",
                cut.0
            );
            return Err(CutFailure::Missed);
        }
        let all: BTreeSet<HostId> = hosts.iter().chain(leaving).copied().collect();
        if !all.contains(&me) {
            log::error!(
                "world cut {}: host {me} is not among its hosts {hosts:?}; refused",
                cut.0
            );
            return Err(CutFailure::Aborted);
        }
        let i_leave = leaving.contains(&me);
        if i_leave && !self.leave_requested {
            log::error!(
                "world cut {}: names host {me} leaving, which it never asked; refused",
                cut.0
            );
            return Err(CutFailure::Aborted);
        }
        let peers: BTreeSet<HostId> = all.into_iter().filter(|h| *h != me).collect();
        // Elastic 5: a move this host gives that it cannot give refuses the part; the taker's and
        // the peers' notes of the moves are made.
        let participants: BTreeSet<HostId> = peers.iter().copied().chain([me]).collect();
        self.check_cut_moves(cut, &participants, moves)?;
        // Hosts outside the cut (Elastic 1 Task 4: registered after it began). One this host
        // exchanged state with that no committed cut of both has saved may hold some of this
        // cut's state, which no part would write: this part is refused ("wait, never guess";
        // the next cut lists it). Hosts that left or were succeeded have their own rules.
        let outside = |h: &HostId| {
            !peers.contains(h)
                && *h != me
                && !self.left_peers.contains_key(h)
                && !self.succeeded_peers.contains_key(h)
        };
        let unsaved: Vec<HostId> = self
            .unsaved_traffic
            .iter()
            .copied()
            .filter(|h| outside(h))
            .collect();
        if !unsaved.is_empty() {
            // Elastic 2a: one of them was seen DEAD by a directory read before this `BeginCut`.
            // No cut runs at a declaration and none begins until every recipient applied its
            // order, which reached this host before this `BeginCut` on the same link: had this
            // host been a recipient it would hold the dead host as succeeded. It is not — it won
            // land after the declaration and traded state with the dead host (a stale
            // directory) that nobody will report to the heir. No cut may commit without that
            // state and none can with it: this host stops without saving, and its lapse rolls
            // the world back (`take_unaccounted_dead`).
            if let Some(dead) = unsaved.iter().copied().find(|h| self.dead_seen.contains(h)) {
                log::error!(
                    "world cut {}: this host exchanged state with host {dead}, whose row is dead, and holds no succession \
                     of it (it is no recipient): that state is accounted for nowhere — stopping without a save; the world \
                     rolls back to its last committed cut",
                    cut.0
                );
                self.unaccounted_dead.get_or_insert(dead);
            } else {
                log::warn!(
                    "world cut {} of hosts {hosts:?}: this host exchanged state with hosts {unsaved:?}, which are not in it \
                     and no cut has saved since: refused (the next cut takes them in)",
                    cut.0
                );
            }
            // A host outside the cut may be dead: the next directory read tells.
            self.directory_wanted = true;
            self.cut_refused_for = Some((cut, unsaved));
            return Err(CutFailure::Aborted);
        }
        let outsiders: BTreeSet<HostId> =
            self.peers.keys().copied().filter(|h| outside(h)).collect();
        // Idle land this part releases is masked from here until the cut is decided here
        // (`land.rs`): a region load there would wait for the staged part's decision.
        let now_ms = self.clock_ms;
        if !i_leave {
            self.begin_release(cut, now_ms);
        }
        // A participant this host cannot reach now would never bring its mark: the pair
        // counts instead (the fourth case), once the part is begun.
        let unreachable: Vec<HostId> = peers
            .iter()
            .copied()
            .filter(|p| !self.peer_reachable(*p))
            .collect();
        // Count pairs end with their cut's `CutDone`, and no cut begins before that: one still
        // here is a lost `CutDone` — it thaws and what it held goes out (as for an abort).
        let stale: BTreeSet<CutId> = self
            .count_pairs
            .values()
            .map(|c| c.cut)
            .filter(|c| *c != cut)
            .collect();
        for c in stale {
            log::error!(
                "[server] count pairs of world cut {} were never ended; ended now",
                c.0
            );
            self.end_counts(c);
        }
        // Marks of this cut that came before its `BeginCut`.
        let marks_in: BTreeSet<HostId> = self
            .early_marks
            .iter()
            .filter(|(p, c)| **c == cut && peers.contains(p))
            .map(|(p, _)| *p)
            .collect();
        self.early_marks.retain(|_, c| *c > cut);
        self.world_cut = Some(WorldCutRun {
            cut,
            tick,
            peers,
            marks_in,
            mark_sent: false,
            state: PartState::Pending,
            beginning: true,
            outsiders: BTreeSet::new(),
        });
        // Nothing goes to or comes from a host outside the cut until it is decided: what this
        // host queues for one stays in its session (this part's rows, as for an absent peer),
        // what it sent stays its own.
        for o in outsiders {
            self.freeze_outsider(o);
        }
        if self.begin_cut_at(cut, tick).is_none() {
            // A dead region thread: the part failed; the caller sends the one `CutFailed`.
            // Nothing of it was staged: its mask lifts now (its `CutDone` may never come).
            self.release_aborted(cut);
            self.move_aborted(cut);
            if let Some(run) = self.world_cut.take() {
                for o in &run.outsiders {
                    if let Some(peer) = self.peers.get_mut(o) {
                        peer.session.thaw();
                    }
                }
            }
            return Err(CutFailure::Aborted);
        }
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == cut) {
            run.beginning = false;
        }
        // The journal truncates toward these when the cut commits, whoever reports it —
        // every participant, those it will count with included (by counts, then).
        let participants = self.world_cut_peers(cut);
        self.journal.cut_begun(cut, &participants);
        for p in unreachable {
            self.world_cut_count_mode(p, "not reachable at the BeginCut");
        }
        // A leave joins this host and a peer when either leaves: everything this host sends
        // that peer from now until the cut is decided waits here, in this host's part.
        let peers: Vec<HostId> = self.world_cut_peers(cut).into_iter().collect();
        for p in peers.into_iter().filter(|p| i_leave || leaving.contains(p)) {
            let leaver = leaving
                .contains(&p)
                .then(|| (self.peer_boots.get(&p).copied(), self.land_of(p)));
            if leaver.is_some() {
                if let Some(peer) = self.peers.get_mut(&p) {
                    peer.session.hold_on_new_boot(true);
                }
            }
            // A pair of an earlier cut is ended when that cut is decided, and no cut begins
            // before that: one still here would be a lost `CutDone` — what it held goes
            // out (as for an abort) rather than being overwritten.
            if let Some(stale) = self.leave_pairs.insert(
                p,
                LeavePair {
                    cut,
                    held: Vec::new(),
                    leaver,
                },
            ) {
                log::error!(
                    "[server] a leave pair with host {p} of cut {} was never ended; what it held goes out",
                    stale.cut.0
                );
                for msg in stale.held {
                    self.queue_now(p, msg);
                }
            }
        }
        // Elastic 5: land this host gives in the cut is masked, its players parked keeping their
        // sessions, its regions stopped (their `Stopped` answers are the cut's).
        self.begin_moves_out(cut, moves);
        if i_leave {
            // Everything here parks for it: the regions' `Stopped` answers are the cut's.
            log::info!(
                "[server] world cut {} at tick {tick}: this host leaves in it; parking everything",
                cut.0
            );
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

    /// This host asked to leave ([`Self::request_leave`]): it is exiting.
    pub fn leave_asked(&self) -> bool {
        self.leave_requested
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
    pub(super) fn leave_blocks(&self, ordinal: HostId) -> bool {
        self.leave_pairs.contains_key(&ordinal) || self.left_peers.contains_key(&ordinal)
    }

    /// `msg` to `ordinal`, joined by a leave of the pending world cut or counted with in it:
    /// held back (`None`) until the cut is decided — by the leave pair when there is one
    /// (the hold-composition table: it decides what the held messages become) — anything
    /// else is given back to be queued.
    pub(super) fn hold_for_leave(&mut self, ordinal: HostId, msg: Reliable) -> Option<Reliable> {
        if let Some(pair) = self.leave_pairs.get_mut(&ordinal) {
            pair.held.push(msg);
            return None;
        }
        match self.count_pairs.get_mut(&ordinal) {
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
        let pairs: Vec<HostId> = self
            .leave_pairs
            .iter()
            .filter(|(_, p)| p.cut == cut)
            .map(|(o, _)| *o)
            .collect();
        let me_leaving = self.leaving_cut == Some(cut);
        for ordinal in pairs {
            let pair = self.leave_pairs.remove(&ordinal).expect("listed");
            // Counted across a down link: the other side received exactly up to its count
            // (it may lack this host's mark; what is above the count is this part's rows).
            let counted = self
                .count_pairs
                .get(&ordinal)
                .filter(|c| c.cut == cut)
                .and_then(|c| c.theirs);
            if let Some(peer) = self.peers.get_mut(&ordinal) {
                peer.session.hold_on_new_boot(false);
                if committed {
                    peer.session.hold();
                    match counted {
                        Some(count) => peer.session.drop_through(count),
                        None => peer.session.drop_through_mark(cut),
                    }
                    // A next boot's hello may have come already (held at once): it counts
                    // what is left from 1.
                    peer.session.renumber_unsent();
                } else {
                    peer.session.release();
                }
            }
            let mut back = None;
            if committed && !me_leaving {
                // The boot that left and its land, as noted when the leave began (a hello of
                // its NEXT boot may have come before this `CutDone`).
                let (boot, land) = pair
                    .leaver
                    .clone()
                    .unwrap_or((self.peer_boots.get(&ordinal).copied(), None));
                log::info!(
                    "[server] host {ordinal} left in world cut {}: {} message(s) held for it wait for its land's next owner",
                    cut.0,
                    pair.held.len()
                );
                self.left_peers.insert(ordinal, boot);
                if let Some(directory) = self.ownership.0.directory_mut() {
                    // Not live here at once (its row is clean: the next read leaves it out too).
                    directory.note_dead(ordinal);
                    // Rows still giving it land: read again (land its leave cut released —
                    // its `Released` may never have come — shows free; land it left with
                    // stays its until its `Leave` succession moves it).
                    if !directory.owned_by(ordinal).is_empty() {
                        self.directory_wanted = true;
                    }
                    self.left_land.insert(ordinal, land);
                }
                back = self
                    .peer_boots
                    .get(&ordinal)
                    .copied()
                    .filter(|b| Some(*b) != boot);
            }
            for msg in pair.held {
                self.queue_now(ordinal, msg);
            }
            // Its next boot said hello already: as at that hello, now that it is noted (and
            // as at its `Owns`, if that showed it holds land it left with).
            let owns_seen = self.pair_owns_seen.remove(&ordinal);
            if let Some(b) = back {
                self.peer_hello_boot(ordinal, b);
                self.returning_owns(ordinal, owns_seen);
            }
        }
        if committed {
            self.forget_landless_leavers();
        }
    }

    /// Hosts that left cleanly holding NO land — as the leave noted it (`left_land`) and as the
    /// directory gives it now — are forgotten ([`Self::forget_host`]): no `Leave` succession
    /// can owe an heir anything of them, and a restarted process is a new host (Elastic 1), so
    /// nothing will ever be sent to that id again. One still holding land is kept: its
    /// `left_land` and held session are its `Leave` succession's, which forgets it when it
    /// settles. First, what is held for a departed host that owns nothing NOW goes on by its
    /// land's owner ([`Self::reroute_held_for_landless_leavers`]): forgetting drops it.
    pub(super) fn forget_landless_leavers(&mut self) {
        self.reroute_held_for_landless_leavers();
        let landless: Vec<HostId> = self
            .left_peers
            .keys()
            .copied()
            .filter(|o| !self.leave_pairs.contains_key(o) && !self.returning_peers.contains_key(o))
            .filter(|o| {
                self.left_land
                    .get(o)
                    .is_some_and(|l| l.as_ref().is_some_and(|l| l.is_empty()))
            })
            .filter(|o| self.land_of(*o).is_some_and(|l| l.is_empty()))
            .filter(|o| !self.successions.values().any(|r| r.order.predecessor == *o))
            .collect();
        for o in landless {
            match self.forget_host(o) {
                Ok(()) => log::info!("[server] host {o} left holding no land: forgotten"),
                Err(e) => {
                    log::warn!("[server] host {o} left holding no land, but is not forgotten: {e}")
                }
            }
        }
    }

    /// Under `claim`: the land host `ordinal` holds as this host's directory gives it now
    /// (`None`: the directory was not read whole yet).
    pub(super) fn land_of(&self, ordinal: HostId) -> Option<BTreeSet<crate::SuperRegion>> {
        let directory = self.ownership.0.directory()?;
        directory
            .is_read()
            .then(|| directory.owned_by(ordinal).into_iter().collect())
    }

    /// Hosts the world store says left in a committed leave cut (their `hosts` row is
    /// clean), each with the boot that left: the server reads them at boot, before any
    /// placement, and this host treats them as it treats a leave it saw (`end_leaves`) —
    /// nothing is handed to them, and a spawn order does not wait for their sync (they hold
    /// nobody). A host that crashed (a dirty row) is not one of them: it stays expected.
    /// One that comes back says so with another boot's hello (`peer_hello_boot`), and is
    /// expected again from then on — a clean row read at boot is never the row of a boot
    /// that runs: every boot clears its row before it dials or listens. Called before the
    /// first [`Self::maintain`] (no hello read yet), and later through
    /// [`Self::note_clean_rows`] for a leave this host missed.
    pub fn note_left_peers(&mut self, left: impl IntoIterator<Item = (HostId, u64)>) {
        let me = self.ordinal();
        for (ordinal, boot) in left {
            if ordinal == me {
                continue;
            }
            log::info!(
                "[server] host {ordinal} left the world cleanly (boot {boot}): not awaited until it returns"
            );
            self.left_peers.insert(ordinal, Some(boot));
            // Boot path (and a leave missed while outside its cut: the cut froze the session,
            // the leaver received nothing of this host): no message of this host reached the
            // departed boot after its part was taken, so its held session only gates NEW
            // traffic, and that goes by
            // this host's directory: once it is read, what it gives the departed host stands
            // — land it still holds (not given away yet: an order will come, this host being
            // a survivor), or none (it never held any, or a `Leave` succession of it was
            // declared before this boot registered: then this host is no survivor, no order
            // comes, and nothing it holds is owed to the heir — its directory sent that land's
            // traffic to the heir from the start).
            if self.ownership.0.directory().is_some() {
                match self.land_of(ordinal) {
                    Some(land) => {
                        self.left_land.insert(ordinal, Some(land));
                    }
                    None => {
                        self.left_land.insert(ordinal, None);
                        self.left_land_unread.insert(ordinal);
                    }
                }
            }
        }
    }

    /// The clean `hosts` rows of a periodic read (`(id, boot)`: hosts that left in a committed
    /// leave cut; the server's directory keeper hands them over after each directory read).
    /// One this host has not heard of is noted as at boot ([`Self::note_left_peers`]) — a host
    /// that registered while that leave cut ran is outside the cut and gets no `CutDone`, and a
    /// boot whose own read of the clean rows failed missed it; without this, its sync would be
    /// awaited for ever and its land never asked for. Skipped: this host, a host whose leave
    /// cut this host takes part in (`end_leaves` decides it), and one already known to have
    /// left, been succeeded or forgotten. A session with it is held (nothing more goes to the
    /// leaver's process: what this host owes it is its `Leave` succession's — the leave cut
    /// froze that session, so the leaver received nothing of it), it leaves the claim
    /// preference, and a landless one is forgotten.
    pub fn note_clean_rows(&mut self, clean: impl IntoIterator<Item = (HostId, u64)>) {
        let me = self.ordinal();
        let new: Vec<(HostId, u64)> = clean
            .into_iter()
            .filter(|(o, _)| {
                *o != me && !self.left_peers.contains_key(o) && !self.leave_pairs.contains_key(o)
            })
            .filter(|(o, _)| !self.peer_succeeded(*o) && !self.forgotten_hosts.contains(o))
            .collect();
        if new.is_empty() {
            return;
        }
        for (o, _) in &new {
            if let Some(peer) = self.peers.get_mut(o) {
                peer.session.hold();
            }
            if let Some(directory) = self.ownership.0.directory_mut() {
                directory.note_dead(*o);
            }
        }
        self.note_left_peers(new);
        self.forget_landless_leavers();
    }

    /// The directory was read whole: a departed host's land the boot path could not note yet
    /// is noted now, and a returning boot of a host that left holding none is released
    /// (`peer_hello_boot`). A leave seen in a cut is never noted here (`LeavePair::leaver`).
    pub(super) fn left_land_read(&mut self) {
        for o in std::mem::take(&mut self.left_land_unread) {
            if self.left_land.contains_key(&o) {
                let land = self.land_of(o);
                self.left_land.insert(o, land);
            }
        }
        let back: Vec<HostId> = self.returning_peers.keys().copied().collect();
        for o in back {
            if self
                .left_land
                .get(&o)
                .is_some_and(|l| l.as_ref().is_some_and(|l| l.is_empty()))
            {
                log::info!(
                    "[server] host {o} left holding no land: what waited for its new boot goes out"
                );
                self.release_left(o);
            }
        }
        self.forget_landless_leavers();
    }

    /// Host `from`'s hello of boot `boot`: a host that left comes back as another boot —
    /// its held session goes out again. Under `claim` (R4 drain):
    /// - it left holding NO land (`left_land`, as this host's directory gave it then): at
    ///   once — no `Leave` succession can owe an heir anything of it. Its leave was noted
    ///   before any directory read: once one is (`left_land_read`) — "wait, never guess";
    /// - it left holding land: once that boot shows it re-claimed some of it
    ///   ([`Self::returning_owns`]). A boot registered after a `Leave` succession of the one
    ///   that left owns nothing, and what waited goes to the heir (`succession.rs`).
    pub(super) fn peer_hello_boot(&mut self, from: HostId, boot: u64) {
        self.peer_boots.insert(from, boot);
        if let Some(pair) = self.leave_pairs.get(&from) {
            if pair
                .leaver
                .as_ref()
                .is_some_and(|(left_as, _)| *left_as != Some(boot))
            {
                // The next boot of a host leaving in a cut not decided here yet: nothing goes
                // to it before the decision (`end_leaves` decides as at this hello).
                log::info!(
                    "[server] host {from} is back (boot {boot}) before its leave cut was decided here: held"
                );
                if let Some(peer) = self.peers.get_mut(&from) {
                    peer.session.hold();
                }
            }
            return;
        }
        let back = self
            .left_peers
            .get(&from)
            .is_some_and(|left_as| *left_as != Some(boot));
        if !back {
            return;
        }
        if self.ownership.0.directory().is_some() {
            self.returning_peers.insert(from, boot);
            match self.left_land.get(&from) {
                Some(Some(land)) if land.is_empty() => {
                    log::info!(
                        "[server] host {from} is back (boot {boot}) and left holding no land: what waited for it goes out"
                    );
                    self.release_left(from);
                }
                _ => log::info!(
                    "[server] host {from} is back (boot {boot}): what waited for it goes out once it holds its land again"
                ),
            }
            return;
        }
        self.release_left(from);
    }

    /// Host `from`, back from a leave, holds `reclaimed` super-regions of the land it left
    /// with (its [`Reliable::Owns`], under `claim`): its held session goes out.
    pub(super) fn returning_owns(&mut self, from: HostId, reclaimed: bool) {
        if let Some((left_as, _)) = self.leave_pairs.get(&from).and_then(|p| p.leaver.as_ref()) {
            // Its leave is not decided here yet: `end_leaves` releases it then — but only for
            // an `Owns` of its NEXT boot (`peer_boots` names that boot once its hello came;
            // nothing of it arrives before). One the leaving boot queued before its
            // `BeginCut` says nothing of the next boot.
            if reclaimed
                && self
                    .peer_boots
                    .get(&from)
                    .is_some_and(|b| Some(*b) != *left_as)
            {
                self.pair_owns_seen.insert(from);
            }
            return;
        }
        if reclaimed && self.returning_peers.remove(&from).is_some() {
            self.release_left(from);
        }
    }

    /// Host `from`, back as another boot, gets what waited for it.
    fn release_left(&mut self, from: HostId) {
        self.left_peers.remove(&from);
        self.returning_peers.remove(&from);
        self.left_land.remove(&from);
        self.left_land_unread.remove(&from);
        if let Some(peer) = self.peers.get_mut(&from) {
            log::info!("[server] host {from} is back: what waited for it goes out");
            peer.session.release();
        }
    }

    /// Test support: whether this host's session with `ordinal` is held for a leave.
    #[doc(hidden)]
    pub fn peer_left(&self, ordinal: HostId) -> bool {
        self.left_peers.contains_key(&ordinal)
    }

    /// Test support: the reliable messages this host owes `ordinal` (its session's and the
    /// ones a leave holds back), in order.
    #[doc(hidden)]
    pub fn peer_owed(&self, ordinal: HostId) -> Vec<Reliable> {
        let mut out: Vec<Reliable> = self
            .peers
            .get(&ordinal)
            .map(|p| p.session.unreceived().cloned().collect())
            .unwrap_or_default();
        out.extend(
            self.peer_waiting
                .get(&ordinal)
                .into_iter()
                .flatten()
                .cloned(),
        );
        out.extend(
            self.leave_pairs
                .get(&ordinal)
                .into_iter()
                .flat_map(|p| p.held.iter().cloned()),
        );
        out.extend(
            self.count_pairs
                .get(&ordinal)
                .into_iter()
                .flat_map(|p| p.held.iter().cloned()),
        );
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
            _ => log::debug!(
                "world cut {}: prepared, but no longer this host's cut (decided meanwhile)",
                cut.0
            ),
        }
    }

    /// The server could not stage this host's part of world cut `cut` (`why`): its
    /// uncommitted entries stay for the next cut ([`Self::cut_failed`]) and the
    /// coordinator is told.
    pub fn world_cut_failed(&mut self, cut: CutId, why: CutFailure) {
        // Not decided yet (the part may even be staged after a backend error and then
        // committed): the journal keeps the cut's marks until its decision.
        self.cut_settled_failed(cut);
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
    pub(super) fn world_begin_cut(
        &mut self,
        cut: CutId,
        tick: crate::Tick,
        hosts: &[HostId],
        leaving: &[HostId],
        generation: u64,
        moves: &[crate::LandMove],
    ) {
        if !self.world_attached() {
            // Sent before the coordinator applied our latest `Attach`, which aborted it.
            log::debug!(
                "world cut {}: begun before our latest attach; dropped",
                cut.0
            );
            return;
        }
        if generation != self.host_generation {
            log::warn!(
                "world cut {}: of world generation {generation}, this host runs {}; refused",
                cut.0,
                self.host_generation
            );
            if generation > self.host_generation {
                self.higher_generation = Some(
                    self.higher_generation
                        .map_or(generation, |g| g.max(generation)),
                );
            }
            self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
            return self.world_send(HostMsg::CutFailed {
                cut,
                why: CutFailure::Aborted,
            });
        }
        if let Err(why) = self.begin_world_cut(cut, tick, hosts, leaving, moves) {
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
        if committed {
            // What this host exchanged with the participants is saved by this cut.
            self.unsaved_traffic.retain(|h| !participants.contains(h));
            // Elastic 5: the taker and the peers read the directory for the moved land.
            self.move_cut_committed(cut);
        } else {
            // Elastic 5: the taker's and the peers' notes of the cut's moves go (the giver's mask
            // lifts with the cut's settle, `cut_failed`).
            self.expecting
                .values_mut()
                .filter(|m| m.cut_is(cut))
                .for_each(|m| m.uncut());
            self.peer_moving.retain(|_, (c, _)| *c != cut);
        }
        if let Some(run) = self.world_cut.take_if(|r| r.cut == cut) {
            for o in &run.outsiders {
                if let Some(peer) = self.peers.get_mut(o) {
                    log::info!(
                        "[server] world cut {} decided: host {o}, outside it, is reachable again",
                        cut.0
                    );
                    peer.session.thaw();
                }
            }
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
                log::error!(
                    "world cut {}: committed, but this host's part was never prepared",
                    cut.0
                );
            }
        }
        self.world_cut_seen = Some(self.world_cut_seen.map_or(cut, |s| s.max(cut)));
        self.early_marks.retain(|_, c| *c > cut);
        self.end_leaves(cut, committed);
        self.end_counts(cut);
        if self.leaving_cut == Some(cut) {
            self.leave_decided = true;
        }
        self.world_cut_done.push((cut, committed));
        // A succession waiting for this base cut answers now (R4).
        self.retry_successions();
    }

    /// The pending cut `id` was dropped here (a region missed or was lost, a deadline):
    /// if it is the world cut's part, the part fails (`why`).
    pub(super) fn world_cut_dropped(&mut self, id: CutId, why: CutFailure) {
        if self
            .world_cut
            .as_ref()
            .is_some_and(|r| r.cut == id && r.state == PartState::Pending)
        {
            self.world_cut_fail(why);
        }
    }

    /// The part of the world cut, not complete yet, fails: the cut is dropped here, the
    /// coordinator is told, and the part stays `Failed` until its `CutDone`.
    pub(super) fn world_cut_fail(&mut self, why: CutFailure) {
        self.world_cut_fail_saying(why, None);
    }

    /// [`Self::world_cut_fail`], its one warning saying `reason` (`None`: the failure itself)
    /// — a caller that knows why the part fails logs one line, not two.
    pub(super) fn world_cut_fail_saying(&mut self, why: CutFailure, reason: Option<&str>) {
        let Some(run) = self
            .world_cut
            .as_mut()
            .filter(|r| r.state == PartState::Pending)
        else {
            return;
        };
        run.state = PartState::Failed;
        let (cut, beginning) = (run.cut, run.beginning);
        match reason {
            Some(reason) => log::warn!("world cut {}: this host's part failed: {reason}", cut.0),
            None => log::warn!("world cut {}: this host's part failed ({why:?})", cut.0),
        }
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
        let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == id) else {
            return true;
        };
        if !run.mark_sent && world_tick < run.tick {
            // Never before T (the module docs); the upkeep tries again.
            return false;
        }
        if !run.mark_sent {
            run.mark_sent = true;
            let peers: Vec<HostId> = run.peers.iter().copied().collect();
            log::debug!(
                "[server] world cut {}: every region answered; marks to {peers:?}",
                id.0
            );
            let now = self.spawner.world_tick();
            self.marks_queued.push((id, now));
            if self.marks_queued.len() > MARKS_QUEUED_KEPT {
                self.marks_queued.remove(0);
            }
            // A pair counted with gets no mark: its relayed count stands for it.
            let marked: Vec<HostId> = peers
                .into_iter()
                .filter(|p| !self.count_pairs.contains_key(p))
                .collect();
            for p in marked {
                self.queue_now(p, Reliable::CutMark { cut: id });
            }
        }
        // Per participant its mark, or — counted with — its relayed count (a mark that came
        // before the freeze settles only what the peer sent; this host's own stream needs the
        // count it committed to by reporting its own).
        let run = self.world_cut.as_ref().expect("checked");
        run.peers.iter().all(|p| match self.count_pairs.get(p) {
            Some(pair) => pair.theirs.is_some(),
            None => run.marks_in.contains(p),
        })
    }

    /// The pending world cut runs with participant `p` in count mode from now on (the module
    /// docs' fourth case; `why`, for the log): the session is frozen until the cut's
    /// `CutDone`, and this host's count of `p`'s numbering goes to the coordinator, relayed
    /// to `p`. Only while this part is pending; once per pair.
    pub(super) fn world_cut_count_mode(&mut self, p: HostId, why: &str) {
        let Some(run) = self
            .world_cut
            .as_ref()
            .filter(|r| r.state == PartState::Pending && r.peers.contains(&p))
        else {
            return;
        };
        let cut = run.cut;
        if self.count_pairs.contains_key(&p) {
            return;
        }
        let (count, boot) = match self.peers.get_mut(&p) {
            Some(peer) => {
                peer.session.freeze();
                peer.session.received_through()
            }
            None => (0, None),
        };
        log::warn!(
            "[server] world cut {}: host {p} {why}: counted across the link (received {count} of boot {:#x})",
            cut.0,
            boot.unwrap_or(0)
        );
        self.count_pairs.insert(
            p,
            CountPair {
                cut,
                theirs: None,
                held: Vec::new(),
            },
        );
        self.world_send(HostMsg::CutPeerCount {
            cut,
            peer: p,
            received_through: count,
            boot: boot.unwrap_or(0),
        });
    }

    /// World cut `cut` is decided: each pair it counted with ends — the session thaws (a
    /// hold of a leave stays: the hold-composition table) and what was held back for it is
    /// queued.
    fn end_counts(&mut self, cut: CutId) {
        let pairs: Vec<HostId> = self
            .count_pairs
            .iter()
            .filter(|(_, c)| c.cut == cut)
            .map(|(o, _)| *o)
            .collect();
        for ordinal in pairs {
            let pair = self.count_pairs.remove(&ordinal).expect("listed");
            if let Some(peer) = self.peers.get_mut(&ordinal) {
                peer.session.thaw();
            }
            for msg in pair.held {
                self.queue_now(ordinal, msg);
            }
        }
    }

    /// Whether `id` is the world cut pending here (from its `BeginCut` until it is decided).
    pub(super) fn world_cut_is(&self, id: CutId) -> bool {
        self.world_cut.as_ref().is_some_and(|r| r.cut == id)
    }

    /// The other participants of world cut `id` (empty for a one-phase cut).
    pub(super) fn world_cut_peers(&self, id: CutId) -> BTreeSet<HostId> {
        self.world_cut
            .as_ref()
            .filter(|r| r.cut == id)
            .map(|r| r.peers.clone())
            .unwrap_or_default()
    }

    /// Host `o`, outside the pending world cut, is frozen until the cut's `CutDone`.
    fn freeze_outsider(&mut self, o: HostId) {
        let Some(run) = self.world_cut.as_mut() else {
            return;
        };
        if run.outsiders.insert(o) {
            log::info!(
                "[server] world cut {}: host {o} is not in it: nothing goes to or comes from it until it is decided",
                run.cut.0
            );
        }
        if let Some(peer) = self.peers.get_mut(&o) {
            peer.session.freeze();
        }
    }

    /// A session with host `o` is made while a world cut is pending here (`set_peer`): `true`
    /// (it is to be frozen) when `o` is outside that cut — noted, and thawed at its `CutDone`.
    pub(super) fn world_cut_outsider_arrived(&mut self, o: HostId) -> bool {
        let Some(run) = self.world_cut.as_mut() else {
            return false;
        };
        if run.peers.contains(&o)
            || self.left_peers.contains_key(&o)
            || self.succeeded_peers.contains_key(&o)
        {
            return false;
        }
        if run.outsiders.insert(o) {
            log::info!(
                "[server] world cut {}: host {o} joined during it: nothing goes to or comes from it until it is decided",
                run.cut.0
            );
        }
        true
    }

    /// Test support: the latest world cut this host refused its part of because it exchanged
    /// state with hosts outside it that no cut has saved (the module docs, "Hosts outside the
    /// cut"), with those hosts.
    #[doc(hidden)]
    pub fn world_cut_refused_for(&self) -> Option<(CutId, Vec<HostId>)> {
        self.cut_refused_for.clone()
    }

    /// Test support: the hosts outside the pending world cut this host froze for it.
    #[doc(hidden)]
    pub fn world_cut_outsiders(&self) -> Vec<HostId> {
        self.world_cut
            .as_ref()
            .map(|r| r.outsiders.iter().copied().collect())
            .unwrap_or_default()
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
    pub(super) fn world_cut_wrap(
        &mut self,
        ordinal: HostId,
        mut msg: Reliable,
        view: bool,
    ) -> (Reliable, Option<CutId>) {
        if !msg.is_state() || matches!(msg, Reliable::InCut { .. }) {
            return (msg, None);
        }
        let Some(run) = self
            .world_cut
            .as_ref()
            .filter(|r| r.peers.contains(&ordinal))
        else {
            // Made while a wrapped message is applied: accounted where that one is.
            return (msg, self.inbound_accounted);
        };
        let (cut, tick) = (run.cut, run.tick);
        // To (or from) a host leaving in this cut, or one this part counts with, nothing goes
        // bare: it is held back until the cut is decided (`hold_for_leave`), so it is this
        // part's (case 1 or 3).
        let leave =
            self.leave_pairs.contains_key(&ordinal) || self.count_pairs.contains_key(&ordinal);
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
            Reliable::Transfer { bundle, .. }
                if matches!(bundle.kind, crate::EntityKind::Person { .. }) =>
            {
                None
            }
            Reliable::Transfer {
                target,
                stamp,
                bundle,
                player: None,
            } => Some(InFlightRow::Transfer {
                target: *target,
                bundle: (**bundle).clone(),
                stamp: *stamp,
            }),
            Reliable::Intent(m) => Some(InFlightRow::Intent {
                target: peers::peer_intent_target(m),
                msg: *m,
            }),
            _ => None,
        };
        let record = match msg {
            Reliable::Transfer {
                target,
                bundle,
                player: Some(h),
                ..
            } => peers::handoff_record(*target, bundle, h).map(|r| (h.player, r)),
            Reliable::PlayerMove {
                handoff, record, ..
            } => Some((handoff.player, (**record).clone())),
            _ => None,
        };
        self.cut_order += 1;
        let order = self.cut_order;
        let Some(pending) = self.pending_cut.as_mut() else {
            return;
        };
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
        let Some(player) = reliable_player(msg) else {
            return;
        };
        if let Some(pending) = self.pending_cut.as_mut().filter(|c| c.id == cut) {
            let had = pending
                .transit_players
                .get(&player)
                .or(pending.parked_at_t.get(&player))
                .cloned();
            pending.not_ours.entry(player).or_insert(had);
            // Its sender accounts for it now, whatever this host sent before.
            pending.sent_bare.remove(&player);
        }
    }

    /// A BARE player-bearing `msg` arrived: a player this host sent bare (case 2 noted it
    /// as the receiver's) is this host's again in the pending cut. A note made because a
    /// wrapped message brought it stays.
    pub(super) fn note_player_returned(&mut self, msg: &Reliable) {
        let Some(player) = reliable_player(msg) else {
            return;
        };
        if let Some(pending) = self.pending_cut.as_mut() {
            if pending.sent_bare.remove(&player) {
                pending.not_ours.remove(&player);
            }
        }
    }

    /// Whether `from`'s mark of this host's world cut came in while the part has failed
    /// here (a bare message after it is expected then: the cut is dead).
    pub(super) fn after_mark_failed(&self, from: HostId) -> bool {
        self.world_cut
            .as_ref()
            .is_some_and(|r| r.state == PartState::Failed && r.marks_in.contains(&from))
    }

    /// Whether `from`'s mark of the pending world cut came in (and the part is not complete).
    pub(super) fn after_mark(&self, from: HostId) -> bool {
        self.world_cut
            .as_ref()
            .is_some_and(|r| r.state == PartState::Pending && r.marks_in.contains(&from))
    }

    /// Host `from`'s mark of world cut `cut`.
    pub(super) fn peer_cut_mark(&mut self, from: HostId, cut: CutId) {
        self.journal.mark_in(from, cut);
        if let Some(run) = self.world_cut.as_mut().filter(|r| r.cut == cut) {
            if run.peers.contains(&from) {
                run.marks_in.insert(from);
                self.try_complete_cut();
            } else {
                log::warn!(
                    "world cut {}: a mark from host {from}, which is not in it; ignored",
                    cut.0
                );
            }
            return;
        }
        if self.world_cut_seen.is_some_and(|s| cut <= s) {
            log::debug!(
                "world cut {}: host {from}'s mark of a cut over here; ignored",
                cut.0
            );
            return;
        }
        // Its `BeginCut` has not come yet: counted when it does.
        self.early_marks.insert(from, cut);
    }

    /// Participant `from`'s count of this host's numbering toward it in world cut `cut`,
    /// relayed by the coordinator ([`crate::CoordMsg::CutPeerCount`]).
    pub(super) fn world_cut_peer_count(
        &mut self,
        cut: CutId,
        from: HostId,
        received_through: u64,
        boot: u64,
    ) {
        let Some(run) = self
            .world_cut
            .as_ref()
            .filter(|r| r.cut == cut && r.peers.contains(&from))
        else {
            log::debug!(
                "world cut {}: host {from}'s count of a cut not pending here; ignored",
                cut.0
            );
            return;
        };
        match run.state {
            PartState::Pending => {
                // The peer froze: nothing more of this host's reaches it, and its mark will not
                // come — this host counts too (its own count goes out now if not yet).
                self.world_cut_count_mode(from, "counts across the link");
                // A count of another process of this host: the peer received nothing of this
                // one's numbering — everything owed it is this part's.
                let own = self.peers.get(&from).map(|p| p.session.own_boot());
                let theirs = if own == Some(boot) {
                    received_through
                } else {
                    0
                };
                if theirs != received_through {
                    log::warn!(
                        "world cut {}: host {from} counted {received_through} of boot {boot:#x}, not this host's: 0 counted",
                        cut.0
                    );
                }
                if let Some(pair) = self.count_pairs.get_mut(&from).filter(|c| c.cut == cut) {
                    pair.theirs = Some(theirs);
                }
                self.try_complete_cut();
            }
            PartState::Complete | PartState::Prepared if !self.count_pairs.contains_key(&from) => {
                // This part is taken by the marks: it can never account for what the peer
                // lacks of this host's numbering (it needs this host's count, which this part
                // never committed to). Wait, never guess: the cut cannot commit — it fails now
                // rather than at the deadline.
                log::warn!(
                    "world cut {}: host {from} counts across the link, but this part is complete by marks; aborted",
                    cut.0
                );
                self.world_send(HostMsg::CutFailed {
                    cut,
                    why: CutFailure::Aborted,
                });
            }
            _ => {}
        }
    }

    /// The pairs the pending world cut `id` counts with and their relayed counts, for the
    /// part's rows (`try_complete_cut`).
    pub(super) fn world_cut_counts(&self, id: CutId) -> Vec<(HostId, u64)> {
        self.count_pairs
            .iter()
            .filter(|(_, c)| c.cut == id)
            .filter_map(|(o, c)| c.theirs.map(|t| (*o, t)))
            .collect()
    }

    /// The Attach's pending world cut (its outcome is asked for).
    pub(super) fn attach_pending_cut(&self) -> Option<CutId> {
        self.world_cut.as_ref().map(|r| r.cut)
    }

    /// Test support: apply `msg` as if host `from` had sent it on its peer session (the
    /// inbound path a session's `Reliable` event takes).
    #[doc(hidden)]
    pub fn apply_peer_reliable(&mut self, from: HostId, msg: Reliable) {
        self.peer_reliable(from, msg);
        self.flush_world();
    }

    /// Test support: send `msg` to host `ordinal` as the manager sends a state message
    /// (`view`: departed after its source answered the pending cut) — through the world
    /// cut's wrapper rule.
    #[doc(hidden)]
    pub fn send_reliable_for_test(&mut self, ordinal: HostId, msg: Reliable, view: bool) {
        self.send_reliable(ordinal, msg, view);
    }

    /// Test support: `msg` waits for a session with host `ordinal` (`peer_waiting`), as if
    /// that host had none yet.
    #[doc(hidden)]
    pub fn wait_for_session_for_test(&mut self, ordinal: HostId, msg: Reliable) {
        *self.peer_queued.entry(ordinal).or_default() += 1;
        self.peer_waiting.entry(ordinal).or_default().push(msg);
    }

    /// Test support: `msg` is held back for host `ordinal` as a leave of cut `cut` holds it
    /// (`leave_pairs`), never queued.
    #[doc(hidden)]
    pub fn hold_for_leave_for_test(&mut self, ordinal: HostId, cut: CutId, msg: Reliable) {
        self.leave_pairs
            .entry(ordinal)
            .or_insert_with(|| LeavePair {
                cut,
                held: Vec::new(),
                leaver: None,
            })
            .held
            .push(msg);
    }

    /// Test support: every crossing in flight with the world cut its sender accounts for it
    /// in (`None`: this host's).
    #[doc(hidden)]
    pub fn in_flight_accounted(&self) -> Vec<(crate::EntityId, Option<CutId>)> {
        self.in_flight
            .iter()
            .map(|(id, f)| (*id, f.accounted_in))
            .collect()
    }

    /// Test support: the latest world cuts this host queued its marks of (at most
    /// [`MARKS_QUEUED_KEPT`]), with the world tick it did so at.
    #[doc(hidden)]
    pub fn marks_queued(&self) -> &[(CutId, crate::Tick)] {
        &self.marks_queued
    }

    /// Test support: what this host's session with `ordinal` has received from it, and the
    /// peer boot that count is of ([`crate::PeerSession::received_through`]).
    #[doc(hidden)]
    pub fn peer_received_through(&self, ordinal: HostId) -> Option<(u64, Option<u64>)> {
        self.peers
            .get(&ordinal)
            .map(|p| p.session.received_through())
    }

    /// Test support: whether this host's session with `ordinal` is frozen for a world cut in
    /// count mode (`world_cut.rs`).
    #[doc(hidden)]
    pub fn peer_frozen(&self, ordinal: HostId) -> bool {
        self.peers
            .get(&ordinal)
            .is_some_and(|p| p.session.is_frozen())
    }

    /// Test support: the pending world cut part's state, as text (`None`: none).
    pub fn world_cut_state(&self) -> Option<(CutId, &'static str)> {
        self.world_cut.as_ref().map(|r| {
            (
                r.cut,
                match r.state {
                    PartState::Pending if r.mark_sent => "marked",
                    PartState::Pending => "pending",
                    PartState::Complete => "complete",
                    PartState::Prepared => "prepared",
                    PartState::Failed => "failed",
                },
            )
        })
    }
}

/// The player a reliable message hands over, if any.
fn reliable_player(msg: &Reliable) -> Option<PlayerId> {
    match msg.inner() {
        Reliable::Transfer {
            player: Some(h), ..
        } => Some(h.player),
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
