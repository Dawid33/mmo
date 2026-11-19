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
//! **The ticker's inputs** (Elastic 2b Task 6, plan departs 2): ONE replica — the ticker, the
//! lowest id of the durable live set ([`Coordinator::is_ticker`]) — runs the timed and lease
//! work: the upkeep ([`Input::Maintain`], logged: a rebase drops it from the log when the rebased
//! core's ticker is another replica), and the answers of the store READS it alone issues —
//! [`Input::HostsRead`], [`Input::SuccessionsOpen`], [`Input::LandOwners`]. Those three are
//! observations, never logged and never replayed: a non-ticker refuses them, and a rebase drops
//! their effect — re-derived at the ticker's next read (a ticker whose rebase dropped a
//! successions read owes one, as a loaded core and a new ticker do; the `hosts` read comes every
//! few seconds; the hosts ask for land again).
//!
//! A host's answer to the login service ([`crate::HostMsg::Login`]) is a logged host message like
//! any other (coordinator HTTP API): its only effect is a `login_outbox` entry, durable with the
//! transition holding it and sent (or read by the login service) only after it committed; a
//! rebase drops the entry and the replay pushes it again. The answers of the
//! store DECISIONS are logged inputs ([`Input::Decision`], Elastic 2b Task 3), and so is a cut's
//! stored decision read back ([`Input::CutLookedUp`]: a recorded decision never changes, so a
//! replay is what applying it there first would have done).
//!
//! **A stalled host** ([`Input::HostStalled`], coordinator HTTP API): derived by the ticker from
//! its `hosts` read's `mailbox_oldest` (a host's oldest unread mailbox row older than
//! [`super::MAILBOX_STALL_MS`] on the store's clock, and not older than the store time its current
//! `Attach` was applied at — a read issued before a re-`Attach` committed still shows the previous
//! attachment's rows), applied by role `all`'s runtime when its host's channel closes, and by
//! the HTTP runtime when it refuses an `Attach` of a boot it holds attached — logged and
//! replayed, guarded by the attachment it observed, `(boot, epoch)`: it detaches only while the
//! host's newest applied `Attach` is exactly that one (a re-`Attach` keeps the boot and raises the
//! epoch, so a stale or replayed stall after it changes nothing).
//!
//! **Any replica's logged inputs** besides the hosts' and the decisions' answers: a replica's
//! own lifecycle ([`Input::ReplicaJoined`], [`Input::ReplicaLapsed`]) and the ask for the
//! world's first host a login order with no gate anywhere makes ([`Input::StartWanted`],
//! Elastic 4 Task 8: replayed — the store's `expect_wanted: 0` makes a second ask, a replay or a
//! second replica's ask add nothing). The login order that made it is never logged.
//!
//! **A superseded link's messages** (final review G-I1): a replica applies a host's `Attach`
//! (and what follows it on that link), its write is held up, the host's link drops and it
//! attaches to another replica, which commits first — the replay would put the OLD `Attach` (and
//! its link's later messages) after the newer one. So the projection keeps, per host, the
//! `(boot, epoch)` of the newest `Attach` applied ([`HostMsg::Attach`]'s `epoch` grows by one per
//! link up of a host process), every logged host message carries the `(boot, epoch)` of the
//! `Attach` its link opened with on this replica ([`Logged::link`]), and a message of an older
//! link of the same boot is dropped — at its first apply and at a replay, and out of the log: the
//! newer `Attach` re-reported everything, and the host resends what was never answered. So is a
//! message of the newest link itself while that attachment is DETACHED (a stall, an end, an older
//! generation's): a detached pair is never attached again (a re-`Attach` raises the epoch), so
//! the rule reads the same at the first apply and at a replay — a batch posted to a replica whose
//! cache had not seen the detach is dropped where the replay meets it.

use crate::{CutId, HostId, Succession, Tick};
use crate::history::persist::HistoryLoadError;

use super::state::{decode_core, raw_core};
use super::{Coordinator, DecisionAnswer, HostMsg, HostSeen, Projection, ReplicaId};

/// One input of the coordinator core, as a replica applies it ([`Coordinator::apply`]) and
/// replays it at a rebase.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A message of host `host` (the host its link is for). [`HostMsg::Login`] too: its effect
    /// is a login answer that leaves with the transition holding it.
    Host { host: HostId, msg: HostMsg },
    /// Host `host`, attached as boot `boot`, is not reading what the coordinator tells it: its
    /// oldest mailbox row waited past [`super::MAILBOX_STALL_MS`] (the ticker's derivation from a
    /// `hosts` read), its channel link closed (role `all`), or its re-`Attach` was refused. It is
    /// detached (a cut it takes part in aborts, its mailbox rows go) — only while attached as
    /// `boot`; it attaches again once it reaches a replica. Only while its newest applied
    /// `Attach` is `(boot, epoch)` — the attachment the stall was observed on (a re-`Attach`
    /// raises the epoch). Logged and replayed; ANY replica (the runtime applies the last two
    /// itself).
    HostStalled { host: HostId, boot: u64, epoch: u64 },
    /// The upkeep: the calendar, the history's directives and notices, the cuts' and the
    /// successions' timers ([`Coordinator::apply`]). The ticker's only (refused elsewhere, not
    /// logged); a rebase drops it from the log once this replica is not the ticker of the
    /// rebased core.
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
    /// The ticker's read of every `hosts` row (Elastic 2b Task 6): `rows`, the next host id the
    /// store would issue when the read began (`world.next_host_id`, read before the rows), the
    /// read's issue and receipt on the process's MONOTONIC clock (the lease watch's,
    /// [`super::LeaseWatch`]), and the world tick. It feeds the lease watch, the hosts a cut
    /// needs (the dirty live rows), the boots a departed host came back as, the drain marks, and
    /// forgets the ids the read shows pruned; and (Elastic 4) `desired`, every `desired_hosts`
    /// row read with them, the fleet's rules' table (`fleet.rs`). Ticker only; never logged nor
    /// replayed — `desired` neither: a rebase keeps the last read's rows (a process field), the
    /// next read brings them again. `mailbox_oldest` (coordinator HTTP API): per host with an
    /// unread mailbox row, `(host, boot, the store ms its oldest row was inserted at)` — part of
    /// the read, never logged; a row older than [`super::MAILBOX_STALL_MS`] of an attached boot
    /// is applied as an [`Input::HostStalled`] (logged), one of a boot no attachment holds has
    /// that boot's rows dropped (not logged: `stalled_mailboxes`' orphan rule).
    HostsRead {
        rows: Vec<HostSeen>,
        desired: Vec<super::DesiredHost>,
        mailbox_oldest: Vec<(HostId, u64, u64)>,
        next_host_id: u64,
        issued_mono: u64,
        received_mono: u64,
        world_tick: Tick,
    },
    /// The ticker's read of the store's open successions, issued when this cache stood at row
    /// version `issued_at_version` ([`Coordinator::take_successions_reread`]): a run it does not
    /// list is abandoned only if it opened at or before that version (audit H5). Ticker only;
    /// never logged nor replayed.
    SuccessionsOpen { list: Vec<Succession>, issued_at_version: u64 },
    /// The ticker's owners read for the super-regions hosts asked for
    /// ([`Coordinator::take_land_lookups`]): `asked`, each with its askers, as handed out, and
    /// the `super_regions` rows read of them — `(super-region, owner or NO_OWNER, epoch)`; an
    /// asked super-region with no row is absent (free). Answered by
    /// [`Coordinator::on_land_read`] (Elastic 3: an assignment, a named owner, or a departed
    /// owner's land given away). Ticker only; never logged nor replayed (the hosts ask again).
    LandOwners { asked: Vec<(crate::SuperRegion, Vec<HostId>)>, rows: Vec<(crate::SuperRegion, i64, u64)> },
    /// A world cut's decision as the store holds it, read back (a lookup: its answer was lost,
    /// or a host named a cut this core does not remember): for host `host`, or (`None`) the
    /// running cut's own decision. Logged and replayed — a recorded decision never changes.
    CutLookedUp { host: Option<HostId>, cut: CutId, committed: bool },
    /// A login order found no host taking logins ([`super::Gate::Nobody`],
    /// Elastic 4 Task 8) and the fleet's dials are set: the world's first host is asked for,
    /// `AddHost { max, expect_wanted: 0 }` ([`Coordinator::login_gate`], `fleet.rs`), at most
    /// once per [`super::WORLD_START_ASK_EVERY_MS`] on this replica. ANY replica (the order
    /// reached whichever one the login service dialled); logged and replayed — a replay onto a
    /// row whose world has a serving host by then asks nothing, and one whose ask a rebase
    /// dropped asks again (the store answers `NotNeeded` once a wanted row exists). The order
    /// itself is never logged.
    StartWanted,
}

/// The `(boot, epoch)` of a host's `Attach`: what a link to a replica opened with.
pub(super) type LinkStamp = (u64, u64);

/// One input in the log: the store time it was applied at, the input, and — for a host's
/// message — the `Attach` its link opened with on this replica (`None`: none seen; never dropped).
#[derive(Debug, Clone)]
pub(super) struct Logged {
    pub(super) now: u64,
    pub(super) input: Input,
    pub(super) link: Option<LinkStamp>,
}

impl Input {
    /// Whether this input is the ticker's store-read answer: refused on a non-ticker, never
    /// logged, never replayed.
    fn is_ticker_read(&self) -> bool {
        matches!(self, Input::HostsRead { .. } | Input::SuccessionsOpen { .. } | Input::LandOwners { .. })
    }
}

impl Coordinator {
    /// Apply `input` at store time `now_ms` (never earlier than [`Self::clock_floor`]), and log
    /// it for a rebase. What it decides waits for the transition holding its effect
    /// ([`Self::take_transition`]), or leaves with [`Self::take_out`] when nothing durable
    /// changed. The ticker's work ([`Input::Maintain`] and the ticker's reads) is refused on a
    /// replica that is not the ticker; the reads are never logged.
    pub fn apply(&mut self, input: Input, now_ms: u64) {
        let ticker_work = matches!(input, Input::Maintain) || input.is_ticker_read();
        if ticker_work && !self.ticking_now() {
            log::trace!("coordinator: replica {} is not the ticker: {input:?} refused", self.replica);
            return;
        }
        // The link a host's message came on: the `Attach` it opened with here (an `Attach` opens
        // one). Messages of one link arrive in order, its `Attach` first.
        let link = match &input {
            Input::Host { host, msg: HostMsg::Attach { boot, epoch, .. } } => {
                self.link_attach.insert(*host, (*boot, *epoch));
                Some((*boot, *epoch))
            }
            Input::Host { host, .. } => self.link_attach.get(host).copied(),
            Input::HostStalled { host, boot, epoch } => {
                // That link carries nothing more (a new one opens with its `Attach`).
                if self.link_attach.get(host) == Some(&(*boot, *epoch)) {
                    self.link_attach.remove(host);
                }
                None
            }
            _ => None,
        };
        if self.superseded(&input, link) {
            return;
        }
        self.clean_checked = false;
        if !self.unstored && !input.is_ticker_read() {
            // No store, no row to move under it (the inline link): nothing to replay.
            self.log.push(Logged { now: now_ms, input: input.clone(), link });
        }
        self.apply_logged(input, now_ms, link);
    }

    /// Host `host`'s next messages come on the link its `Attach` of `(boot, epoch)` opened, though
    /// that `Attach` was applied elsewhere (a batch posted to another replica names the pair):
    /// they are logged with it ([`Logged::link`]), as if this replica had seen that `Attach`.
    pub fn link_opened(&mut self, host: HostId, boot: u64, epoch: u64) {
        self.link_attach.insert(host, (boot, epoch));
    }

    /// Whether `input` is a message of host `h` that came on a link older than the newest
    /// `Attach` of the same boot the projection holds (`link`: the `Attach` its link opened with;
    /// an `Attach` is its own), or — not an `Attach` — on exactly that newest link while the host
    /// is DETACHED (a stall, an end, an older generation: a detached pair is never attached again,
    /// a re-`Attach` raises the epoch): dropped, with a log line (final review G-I1, the module
    /// notes). Both hold at the first apply and at a replay. A message of another boot is never
    /// dropped here (host ids are never reused: only the solo host's process restarts under one
    /// id, with one replica).
    pub(super) fn superseded(&self, input: &Input, link: Option<LinkStamp>) -> bool {
        let Input::Host { host, msg } = input else { return false };
        let Some((boot, newest)) = self.newest_attach(*host) else { return false };
        let older = match msg {
            HostMsg::Attach { boot: b, epoch, .. } => *b == boot && *epoch < newest,
            _ => link.is_some_and(|(b, e)| b == boot && e < newest),
        };
        if older {
            log::info!(
                "coordinator: host {host}'s message of its link opened by attach {:?} is superseded by its attach {newest} (boot {boot}): dropped",
                link.map(|(_, e)| e)
            );
            return true;
        }
        let detached = !matches!(msg, HostMsg::Attach { .. })
            && link == Some((boot, newest))
            && self.attachment(*host) != Some(boot);
        if detached {
            log::debug!("coordinator: host {host}'s message of its attach {newest} (boot {boot:#x}): that attachment is detached: dropped");
        }
        detached
    }

    /// One input, applied (first, or again at a rebase) at `now_ms` lifted to the projection's
    /// clock floor; `link` as logged ([`Logged::link`]). Whether it was applied — a host's message
    /// of a superseded link is not ([`Self::superseded`]: at a replay, the row it is replayed onto
    /// holds a newer `Attach` of the host).
    fn apply_logged(&mut self, input: Input, now_ms: u64, link: Option<LinkStamp>) -> bool {
        if self.superseded(&input, link) {
            return false;
        }
        let now = now_ms.max(self.clock_floor());
        match input {
            Input::Host { host, msg } => self.on_host(host, msg, now),
            Input::HostStalled { host, boot, epoch } => {
                if self.attachment(host) == Some(boot) && self.newest_attach(host) == Some((boot, epoch)) {
                    log::warn!(
                        "coordinator: host {host} (boot {boot:#x}, attach {epoch}) stalled (its mailbox unread, or its link down): detached"
                    );
                    self.detach_host(host);
                } else {
                    log::debug!(
                        "coordinator: host {host} (boot {boot:#x}, attach {epoch}) stalled, but its attachment is {:?} / {:?}: nothing to detach",
                        self.attachment(host),
                        self.newest_attach(host)
                    );
                }
            }
            Input::Maintain => self.on_maintain(now),
            Input::Decision(answer) => self.on_decision(answer, now),
            Input::ReplicaJoined { me, alone } => self.on_replica_joined(me, alone),
            Input::ReplicaLapsed(r) => self.on_replica_lapsed(r),
            Input::HostsRead { rows, desired, mailbox_oldest, next_host_id, issued_mono, received_mono, world_tick } => {
                self.on_hosts_read(&rows, desired, next_host_id, issued_mono, received_mono, world_tick);
                self.stalled_mailboxes(&mailbox_oldest, now);
            }
            Input::SuccessionsOpen { list, issued_at_version } => self.on_successions_open(list, issued_at_version, now),
            Input::LandOwners { asked, rows } => self.on_land_read(asked, &rows, now),
            Input::CutLookedUp { host, cut, committed } => self.on_cut_looked_up(host, cut, committed),
            Input::StartWanted => self.on_start_wanted(now),
        }
        true
    }

    /// The ticker's read of the hosts' oldest unread mailbox rows (part of [`Input::HostsRead`]):
    /// each attached boot whose oldest row was inserted more than [`super::MAILBOX_STALL_MS`]
    /// before `now_ms` (store ms) — and belongs to the current attachment: not older than the
    /// store time its `Attach` was applied at (less [`super::ATTACH_CLOCK_SLACK_MS`]) — is stalled,
    /// applied as a logged [`Input::HostStalled`] of that attachment's `(boot, epoch)`.
    ///
    /// **Orphans** (final review I1): a head row older than [`super::MAILBOX_STALL_MS`] whose boot
    /// is NOT the one its host is attached as (not attached at all, or as another boot) has no
    /// reader the coordinator knows of — a rollback's `Generation` rows to every attached host
    /// right before `detach_all` (a host dead then never reads its own), a `Generation` told to a
    /// refused `Attach` whose process is gone. Nothing else would ever delete it (host ids are never
    /// reused), and the mailbox gauges and `WorldMailboxStalled` would count it for ever. Its
    /// boot's rows are dropped by the next transition ([`Self::drop_orphan_mailbox`]: by
    /// `(host, boot)`, never a row of another boot — so never one of a current attachment of the
    /// same id). A younger head is left alone (a refused old-generation host may yet read its
    /// `Generation`); a live host of that boot reading its mailbox acknowledges the head long
    /// before the stall time. Not logged (a ticker read's effect): a rebase drops the pending
    /// drop, and the next read decides it again only if the rows are still there and still nobody's
    /// on the rebased core — a re-`Attach` of that boot committed meanwhile makes it its own again;
    /// one in the same transition is applied after the drop and its rows inserted after it.
    fn stalled_mailboxes(&mut self, oldest: &[(HostId, u64, u64)], now_ms: u64) {
        for &(host, boot, at_ms) in oldest {
            let waited = now_ms.saturating_sub(at_ms);
            if waited > super::MAILBOX_STALL_MS && self.attachment(host) != Some(boot) {
                log::info!(
                    "coordinator: host {host}'s mailbox has a row of boot {boot:#x} unread for {waited} ms (over {} ms), \
                     which no attachment holds (attached as {:?}): that boot's rows are dropped",
                    super::MAILBOX_STALL_MS,
                    self.attachment(host)
                );
                self.drop_orphan_mailbox(host, boot);
                continue;
            }
            let Some((b, epoch)) = self.newest_attach(host).filter(|(b, _)| *b == boot) else { continue };
            let current = self.attached_at_ms(host).is_some_and(|since| at_ms + super::ATTACH_CLOCK_SLACK_MS >= since);
            if self.attachment(host) == Some(b) && current && now_ms.saturating_sub(at_ms) > super::MAILBOX_STALL_MS {
                log::warn!(
                    "coordinator: host {host}'s mailbox has a row unread for {} ms (over {} ms): it is stalled",
                    now_ms.saturating_sub(at_ms),
                    super::MAILBOX_STALL_MS
                );
                self.apply(Input::HostStalled { host, boot, epoch }, now_ms);
            }
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
    /// again, in order, each at its logged store time. Called on a write's `Conflict` (and by a
    /// refresh); the transition that met it is dropped (its messages are decided again by the
    /// replay). A row that does not decode is an error and changes nothing.
    ///
    /// What changed outside the log is the row's again: the ticker's reads and hand-outs
    /// (Elastic 2b Task 6) are re-derived — a hand-out the rebased row still wants is handed out
    /// again, a read comes again. A logged upkeep is applied again only while this replica is
    /// the ticker of the core it is replayed onto (at its turn in the log: a replayed lapse may
    /// make it one); otherwise it leaves the log. A ticker whose rebase dropped a successions read
    /// it applied since its last commit owes one again ([`Self::take_successions_reread`]); the
    /// configuration the runtime set (the cut period) holds.
    pub fn rebase(&mut self, core: &[u8], history: Option<&[u8]>, version: u64) -> Result<(), HistoryLoadError> {
        let mut p: Projection = decode_core(core)?;
        let history = match history {
            Some(bytes) => Some(crate::history::persist::decode(bytes, self.history.grid().clone())?),
            None => None,
        };
        p.cuts.carry_process_fields(&mut self.cuts);
        p.successions.carry_process_fields(&self.successions);
        // An ask for the world's first host (`StartWanted`) may be in the transition that met the
        // conflict, or still queued: its pacing goes, so its replay asks again (Elastic 4 Task 8).
        // A refresh (nothing in flight, nothing queued) keeps it.
        if self.writing || !self.decisions.is_empty() {
            self.start_asked_ms = None;
        }
        self.replace_projection(p, history);
        self.committed_raw = raw_core(&self.projection());
        self.history.clear_content_dirty();
        self.version = version;
        self.reapply_cut_config();
        if self.reads_applied != self.reads_committed || self.successions_reading() {
            // A successions read applied since the last commit is lost with the rebase, and one
            // still out may predate what the newer row closed (fix round 1): owed again — before
            // the replay, so that no replayed `Attach` is answered from it — and the one out is
            // stale (its answer dropped). (Everything else the row says of the successions
            // reached it committed.)
            self.successions_owed();
            self.reads_committed = self.reads_applied;
        }
        let log = std::mem::take(&mut self.log);
        log::info!("coordinator: rebased onto version {version}; {} logged input(s) applied again", log.len());
        let mut kept = Vec::with_capacity(log.len());
        for entry in log {
            if matches!(entry.input, Input::Maintain) && !self.ticking_now() {
                log::debug!("coordinator: replica {} is not the ticker of the rebased core: its upkeep is dropped", self.replica);
                continue;
            }
            // A host's message of a link the row holds superseded (G-I1) leaves the log too.
            if self.apply_logged(entry.input.clone(), entry.now, entry.link) {
                kept.push(entry);
            }
        }
        self.log = kept;
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

    /// Whether this replica is the ticker: the lowest id of the live set this core holds
    /// (Elastic 2b Task 6) — the one replica that runs the upkeep, reads the `hosts` rows and the
    /// open successions, looks land and cuts up and hands out the store decisions. A stale cache
    /// may believe it wrongly; the row's compare-and-set then refuses its write, and its rebase
    /// replays without the ticker's work.
    pub fn is_ticker(&self) -> bool {
        self.ticker() == Some(self.replica)
    }

    /// [`Self::is_ticker`], noting a change of role: a replica that becomes the ticker starts its
    /// lease watch from zero (a new ticker never hastens a declaration: it delays one by a lease
    /// at most), knows no `hosts` read yet (no cut begins before one) and owes a successions read.
    pub(super) fn ticking_now(&mut self) -> bool {
        let ticker = self.is_ticker();
        if ticker && !self.ticking {
            log::info!("coordinator: replica {} is the ticker now", self.replica);
            self.leases = self.leases.restarted();
            self.cuts_unread();
            self.successions_owed();
            self.fleet_unread();
        } else if !ticker && self.ticking {
            log::info!("coordinator: replica {} is no longer the ticker", self.replica);
            self.fleet_unread();
        }
        self.ticking = ticker;
        ticker
    }

    /// Inputs logged since the last commit (test support).
    #[doc(hidden)]
    pub fn logged(&self) -> usize {
        self.log.len()
    }
}
