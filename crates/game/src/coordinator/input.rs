//! The coordinator's inputs, logged (Elastic 2b, decision 1). Every input a replica applies
//! through [`Coordinator::apply`] is kept, with the store time it was applied at, until the
//! transition holding its effect commits ([`Coordinator::transition_written`]) — or until it
//! proves to have changed nothing durable ([`Coordinator::take_out`] releases what it decided).
//! When the replica's write meets a `Conflict` (the row moved under it: another writer), the
//! runtime rebases the core onto the newer row ([`Coordinator::rebase`]): the projection is
//! replaced and the log applied again, in order, onto it — each input re-decided against what
//! the row holds now (plan decision 3: each is pure on the projection and its input, or
//! reconciles against it), each at its own store time, never earlier than a time the newer row
//! recorded ([`Coordinator::clock_floor`]).
//!
//! Never logged: what a host tells the login service ([`crate::HostMsg::Login`], forwarded at
//! once into the process's login relay: a replay would tell it twice). The answers of the store
//! DECISIONS are inputs (Elastic 2b Task 3, [`Input::Decision`]). Not logged yet (Elastic 2b
//! Task 6 makes them inputs): the answers of the store's READS and lookups (the `hosts` rows,
//! the open successions, a cut's stored decision, owners, the generation) — they change the
//! projection outside the log, so a core holding such a change since its last commit cannot be
//! rebased ([`Coordinator::unlogged_pending`]); the runtime then stops as Elastic 2a did, and the
//! next start loads the row. A decision handed out (its mark, `decisions.rs`) is no such change:
//! a rebase drops it with the transition it was in, which did not commit.

use crate::HostId;
use crate::history::persist::HistoryLoadError;

use super::state::{decode_core, raw_core};
use super::{Coordinator, DecisionAnswer, HostMsg, Projection, ReplicaId};

/// One input of the coordinator core, as a replica applies it ([`Coordinator::apply`]) and
/// replays it at a rebase.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A message of host `host` (the host its link is for). Never [`HostMsg::Login`] in the
    /// log: [`Coordinator::apply`] forwards it to the login relay unlogged.
    Host { host: HostId, msg: HostMsg },
    /// Host `host`'s link to this replica went down: it detaches only an attachment stamped
    /// with this replica (one to another replica stands).
    LinkDown { host: HostId },
    /// The upkeep: the calendar, the history's directives and notices, the cuts' and the
    /// successions' timers ([`Coordinator::apply`]).
    Maintain,
    /// The store's answer to one decision of a committed transition (`decisions.rs`): applied
    /// only if its stamp is the mark's — at a replay too, so a rebase onto a row holding the
    /// mark applies it there, and onto one without it changes nothing.
    Decision(DecisionAnswer),
    /// Replica `me` registered (Elastic 2b Task 4): every replica's first input. It adds `me`
    /// to the live set; `alone` (the store found no other replica beaten within
    /// [`super::REPLICA_LEASE_MS`] and marked them dead: a total replica crash) drops every
    /// replica registered before it. Idempotent.
    ReplicaJoined { me: ReplicaId, alone: bool },
    /// Replica `r` lapsed (this replica's own [`super::ReplicaWatch`] saw its row stand still
    /// for a lease): dropped from the live set, its row ended in the same transaction
    /// ([`super::StoreDecision::EndReplica`]). Idempotent: a replay onto a row that dropped it
    /// does nothing.
    ReplicaLapsed(ReplicaId),
}

impl Coordinator {
    /// Apply `input` at store time `now_ms` (never earlier than [`Self::clock_floor`]), and log
    /// it for a rebase. What it decides waits for the transition holding its effect
    /// ([`Self::take_transition`]), or leaves with [`Self::take_out`] when nothing durable
    /// changed.
    pub fn apply(&mut self, input: Input, now_ms: u64) {
        if let Input::Host { host, msg: HostMsg::Login(out) } = input {
            // For the login service, forwarded as it is — never logged, never replayed.
            self.forward_login(host, out);
            return;
        }
        self.clean_checked = false;
        if !self.unstored {
            // No store, no row to move under it (the inline link): nothing to replay.
            self.log.push((now_ms, input.clone()));
        }
        self.apply_logged(input, now_ms);
    }

    /// One logged input, applied (first, or again at a rebase) at `now_ms` lifted to the
    /// projection's clock floor.
    fn apply_logged(&mut self, input: Input, now_ms: u64) {
        let now = now_ms.max(self.clock_floor());
        match input {
            Input::Host { host, msg } => self.on_host(host, msg, now),
            Input::LinkDown { host } => {
                if self.is_attached_here(host) {
                    self.detach_host(host);
                } else {
                    log::debug!("coordinator: host {host}'s link down: not attached to this replica, nothing to detach");
                }
            }
            Input::Maintain => self.on_maintain(now),
            Input::Decision(answer) => self.on_decision(answer, now),
            Input::ReplicaJoined { me, alone } => self.on_replica_joined(me, alone),
            Input::ReplicaLapsed(r) => self.on_replica_lapsed(r),
        }
    }

    /// The newest store time the projection recorded: the calendar's anchor and last sign of
    /// life, the cut period's start and the running cut's, a leave request's first ask, an open
    /// succession's start. An input is never applied earlier: a replica whose clock lags the
    /// writer of the row it rebased onto by a few milliseconds never moves a recorded time
    /// backwards (plan decision 3, "Clock").
    pub fn clock_floor(&self) -> u64 {
        self.calendar_seen_ms.max(self.calendar_anchor.0).max(self.cuts_clock_floor()).max(self.successions_clock_floor())
    }

    /// Replace the projection by a newer row's — `core`, and `history` (`None`: the cached
    /// history IS the row's, kept as it is) — at row `version`, keep the process's own fields
    /// (the links' and the watches' observations, the login relay, the log), clear what was
    /// decided and not handed out (`out`) and the write in flight, and apply every logged input
    /// again, in order, each at its logged store time. Called on a write's `Conflict`; the
    /// transition that met it is dropped (its messages are decided again by the replay). Only
    /// for a core with nothing changed outside the log since its last commit
    /// ([`Self::unlogged_pending`]): what such a change held would be lost. A row that does not
    /// decode is an error and changes nothing.
    pub fn rebase(&mut self, core: &[u8], history: Option<&[u8]>, version: u64) -> Result<(), HistoryLoadError> {
        debug_assert!(!self.unlogged_pending(), "a rebase would lose what changed outside the log");
        let mut p: Projection = decode_core(core)?;
        let history = match history {
            Some(bytes) => Some(crate::history::persist::decode(bytes, self.history.grid().clone())?),
            None => None,
        };
        p.cuts.carry_process_fields(&mut self.cuts);
        p.successions.carry_process_fields(&mut self.successions);
        self.replace_projection(p, history);
        self.committed_raw = raw_core(&self.projection());
        self.history.clear_content_dirty();
        self.version = version;
        let log = std::mem::take(&mut self.log);
        log::info!("coordinator: rebased onto version {version}; {} logged input(s) applied again", log.len());
        for (now, input) in &log {
            self.apply_logged(input.clone(), *now);
        }
        self.log = log;
        // A mark of this replica still set now was committed in the row by an earlier
        // transition of it whose answer was never applied (nothing is in flight after a
        // rebase: the transition that met the conflict did not commit): looked up, never
        // issued again. A live replica's marks are its own to answer; those of a replica not in
        // the live set (Elastic 2b Task 4: ended, or gone in a lone registration) are looked up.
        let (me, live) = (self.replica, self.replicas.clone());
        self.orphan_marks(
            |stamp| stamp.replica == me || !live.contains(&stamp.replica),
            "its answer was never applied before a rebase, or its replica is not live",
        );
        Ok(())
    }

    /// A refresh (Elastic 2b: a version check found the row moved): [`Self::rebase`] onto the row
    /// at `version` only if it is NEWER than this cache's — a row read before this cache's own
    /// later commit is older, and rebasing onto it would regress the projection (Task 4 fix round
    /// 1). Whether it rebased.
    pub fn refresh(&mut self, core: &[u8], history: Option<&[u8]>, version: u64) -> Result<bool, HistoryLoadError> {
        if version <= self.version {
            log::debug!("coordinator: a refresh read version {version}, this cache holds {}: nothing to do", self.version);
            return Ok(false);
        }
        self.rebase(core, history, version).map(|()| true)
    }

    /// Whether the projection was changed outside the log since the last commit (a store's
    /// answer, a hand-out of a store call, a read of the hosts, the history changed directly):
    /// a rebase would lose it — the runtime stops instead, and the next start loads the row.
    pub fn unlogged_pending(&self) -> bool {
        self.unlogged != self.unlogged_committed
    }

    /// The projection changed outside the log (see [`Self::unlogged_pending`]).
    pub(super) fn note_unlogged(&mut self) {
        self.unlogged += 1;
        self.clean_checked = false;
    }

    /// `f`, the body of a public entry point that is no input (`what`, for the message): in
    /// debug builds, it may change the projection only if it says so (`note_unlogged`) or hands
    /// a store decision out (its mark) — while store answers are still applied outside the log
    /// (Elastic 2b Task 6 makes them inputs), a change that says nothing would be lost by a
    /// rebase unnoticed.
    pub(super) fn unlogged_guard<R>(&mut self, what: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        #[cfg(debug_assertions)]
        let before = (raw_core(&self.projection()), self.unlogged, self.decisions.len());
        let r = f(self);
        #[cfg(debug_assertions)]
        if self.unlogged == before.1 && self.decisions.len() == before.2 {
            debug_assert!(
                raw_core(&self.projection()) == before.0,
                "coordinator: {what} changed the projection outside the log without noting it"
            );
        }
        #[cfg(not(debug_assertions))]
        let _ = what;
        r
    }

    /// Inputs logged since the last commit (test support).
    #[doc(hidden)]
    pub fn logged(&self) -> usize {
        self.log.len()
    }
}
