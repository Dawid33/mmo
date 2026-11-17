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
//! once into the process's login relay: a replay would tell it twice). Not logged yet (Elastic
//! 2b Tasks 3 and 6 make them inputs): the store's answers and the runtime's reads — they
//! change the projection outside the log, so a core holding such a change since its last commit
//! cannot be rebased ([`Coordinator::unlogged_pending`]); the runtime then stops as Elastic 2a
//! did, and the next start loads the row.

use crate::HostId;
use crate::history::persist::HistoryLoadError;

use super::state::{decode_core, raw_core};
use super::{Coordinator, HostMsg, Projection};

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
                if self.is_attached(host) {
                    self.detach_host(host);
                } else {
                    log::debug!("coordinator: host {host}'s link down: not attached to this replica, nothing to detach");
                }
            }
            Input::Maintain => self.on_maintain(now),
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
        Ok(())
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

    /// Inputs logged since the last commit (test support).
    #[doc(hidden)]
    pub fn logged(&self) -> usize {
        self.log.len()
    }
}
