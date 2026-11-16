//! The coordinator's half of the world cut (several hosts, R3 Task 13): it picks ONE world
//! tick for a cut, asks every expected host for its part ([`CoordMsg::BeginCut`]), and
//! once every part is staged ([`HostMsg::CutPrepared`]) hands the runtime a decision
//! ([`Coordinator::take_decision`]) to record in the store; the outcome goes to every
//! participant ([`CoordMsg::CutDone`]). Any [`HostMsg::CutFailed`], a participant
//! detaching or re-attaching, or [`WORLD_CUT_DEADLINE_MS`] passing decides an abort. One
//! cut at a time: a new one starts only once the last is decided, a period
//! ([`Coordinator::set_world_cuts`]) after it, and only while every expected host is
//! attached. With world cuts off (one host, role `all`) nothing here sends anything.
//!
//! **Leave cuts (Task 14).** A host about to exit asks to leave ([`HostMsg::LeaveRequest`]):
//! the requests are collected for [`LEAVE_BATCH_MS`] (from the first), then a cut begins at
//! once — no period wait, but never beside another cut — with those hosts in `leaving`
//! (they are among its `hosts` too: each stages a part, having parked everything for it).
//! Once it commits their `hosts` rows are clean and they are no longer expected (`left`)
//! until they come back: an `Attach` of another boot, or a `hosts` row of another boot
//! ([`Coordinator::set_host_boots`]). An aborted leave cut forgets the requests (those hosts
//! exit without a cut: the world rolls back to the last cut when they return).
//!
//! **A host joining during a cut (Elastic 1 Task 4).** A host that attaches while a cut it is
//! not in runs does not abort it; the runtime records each cut's begin in the store
//! ([`Coordinator::world_cut_running`] → `record_cut_begin`), so the decision may leave out a
//! host registered after it. The next cut lists the newcomer (its dirty row is read).

use std::collections::{BTreeMap, BTreeSet};

use super::{CoordMsg, Coordinator, CutFailure, Target};
use crate::HostId;
use crate::{CutId, SuccessionId, Tick};

/// How far ahead of the coordinator's world tick a world cut is placed: every host gets
/// the `BeginCut` and every region reaches the tick on its own clock (two seconds at 50 ms
/// ticks: a host link's latency on top of a region's lead).
pub const WORLD_CUT_LEAD_TICKS: Tick = 40;
/// The longest lead a retry after a missed world cut doubles up to.
pub const WORLD_CUT_LEAD_MAX_TICKS: Tick = 640;
/// A world cut not decided this long (wall ms) after it began is aborted.
pub const WORLD_CUT_DEADLINE_MS: u64 = 30_000;
/// Outcomes of decided world cuts the coordinator remembers, newest kept: what a host
/// re-attaching with a pending cut, or a late `CutPrepared` / `CutFailed`, is answered
/// from. An older one is looked up in the store by the runtime
/// ([`Coordinator::take_cut_lookups`]).
pub const WORLD_CUT_OUTCOMES_KEPT: usize = 64;
/// How long (wall ms, from the first) the coordinator collects hosts' leave requests
/// before it begins their leave cut.
pub const LEAVE_BATCH_MS: u64 = 500;
/// How long an exiting host (several hosts) waits for the `BeginCut` that names it
/// leaving; past it, or with that cut aborted, it exits without a cut.
pub const LEAVE_CUT_TIMEOUT_SECS: u64 = 30;
/// How long (wall ms) a due world cut must stay blocked by a host mismatch (a dirty `hosts`
/// row with no attached host, or an attached host with no dirty row) before the core
/// reports it ([`Coordinator::take_cuts_blocked`]): longer than the runtime's re-read of
/// the rows, so a host starting up (attached before the next read) is not reported.
pub const WORLD_CUT_BLOCKED_GRACE_MS: u64 = 5_000;
/// A succession every recipient answered makes a world cut due this long after the last
/// cut ended (R4), whatever the save period: its settle cut begins at once — the window in
/// which a second failure rolls the world back, and the wait of the predecessor's next boot,
/// are seconds, not a save period — and one that aborted (a pinned region still loading) is
/// tried again after the same wait, not in a tight loop.
pub const SETTLE_CUT_RETRY_MS: u64 = 2_000;

/// No world cut begins: the hosts the cuts need (dirty `hosts` rows) are not the hosts
/// attached. Its `Display` is the warning the coordinator logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutsBlocked {
    /// Hosts with a dirty `hosts` row that are not attached.
    pub missing: Vec<HostId>,
    /// Hosts attached with no dirty `hosts` row (registered since the rows were last read).
    pub unexpected: Vec<HostId>,
    /// Open successions (R4) not answered yet by every survivor and their heir.
    pub successions: Vec<SuccessionId>,
    /// Those of `successions` opened while no host ran (the offline `worldctl retire-host` of
    /// worlds before Elastic 2a, which no longer opens one) whose heir has not attached since,
    /// with that heir: no deadline runs for them until it does.
    pub awaiting_heir: Vec<(SuccessionId, HostId)>,
}

impl std::fmt::Display for CutsBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no world cut can begin, the world is NOT being saved:")?;
        if !self.missing.is_empty() {
            write!(
                f,
                " hosts {:?} have a dirty `hosts` row (they may hold state) but are not attached — under claim \
                 ownership such a row is declared dead once its lease lapses and its land goes to a live host (a row \
                 holding no land is ended: nothing to inherit); with \
                 nobody else alive no host can inherit it, and the next host to register takes the land of every \
                 dirty row (the total crash: the world restarts from its last cut). `worldctl retire-host <url> \
                 <id>` vouches for a death the lease has not seen yet. Until then nothing is saved;",
                self.missing.iter().map(|h| h.0).collect::<Vec<_>>()
            )?;
        }
        if !self.unexpected.is_empty() {
            write!(f, " hosts {:?} are attached but have no dirty `hosts` row (registered since the last read?);", self.unexpected.iter().map(|h| h.0).collect::<Vec<_>>())?;
        }
        let ids: Vec<u64> =
            self.successions.iter().filter(|id| !self.awaiting_heir.iter().any(|(a, _)| a == *id)).map(|id| id.0).collect();
        if !ids.is_empty() {
            write!(
                f,
                " successions {ids:?} wait for their survivors' reports and their heir's adoption (the world rolls back \
                 if they do not come within the succession deadline);"
            )?;
        }
        for (id, heir) in &self.awaiting_heir {
            let heir = heir.0;
            write!(
                f,
                " succession {} was opened while no host ran and waits for its heir, host {heir}, to attach: no deadline \
                 runs until it does — a restarted process is a NEW host, so start any host (with nobody else alive its \
                 registration abandons the succession and takes every dirty row's land), or give the succession up with \
                 `worldctl rollback <url> --yes` (the world restarts from its last cut, the land stays with host {heir});",
                id.0
            )?;
        }
        Ok(())
    }
}

/// A world cut to decide in the store ([`Coordinator::take_decision`]): record `commit`
/// for `cut` (`hosts` staged their parts, `leaving` leave in it, the world at
/// `generation`), then tell the core the STORED decision ([`Coordinator::decided`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutDecide {
    pub cut: CutId,
    pub tick: Tick,
    pub commit: bool,
    pub hosts: Vec<HostId>,
    pub leaving: Vec<HostId>,
    pub generation: u64,
    /// The open successions this cut settles if it commits (R4): every one open when it
    /// began — none begins while one is not answered by all its survivors and its heir, so
    /// the heir's part of this cut adopts the predecessor's in-flight rows (`WorldCut::adopts`,
    /// Task 9). The store records the commit as an abort unless this is exactly the set of
    /// open successions.
    pub settles: Vec<SuccessionId>,
}

/// The world cut running.
#[derive(Debug)]
struct CutRun {
    cut: CutId,
    tick: Tick,
    hosts: Vec<HostId>,
    leaving: Vec<HostId>,
    generation: u64,
    /// [`CutDecide::settles`].
    settles: Vec<SuccessionId>,
    /// Participants whose part is staged.
    prepared: BTreeSet<HostId>,
    /// `clock_ms` past which it is aborted.
    deadline_ms: u64,
    /// Decided here (`Some(commit)`), waiting for the runtime.
    verdict: Option<bool>,
    /// The decision was handed to the runtime ([`Coordinator::take_decision`]).
    taken: bool,
}

impl CutRun {
    /// Every host taking part (`hosts`: the leaving ones are among them).
    fn participants(&self) -> impl Iterator<Item = HostId> + '_ {
        self.hosts.iter().copied()
    }

    fn has(&self, host: HostId) -> bool {
        self.participants().any(|h| h == host)
    }
}

/// The world-cut state of the coordinator (never saved: a restarted coordinator starts
/// over; a part left staged is decided by the store's recovery, and a host asks again
/// with its `Attach`).
#[derive(Debug)]
pub(super) struct WorldCuts {
    /// The period between cuts (`None`: world cuts off).
    every_ms: Option<u64>,
    /// The coordinator's claim epoch: the high half of every cut id it makes.
    epoch: u64,
    /// The low half of the last cut id made.
    next: u64,
    /// The hosts whose part every cut needs (the runtime: those whose `hosts` row is not
    /// clean).
    expected: BTreeSet<HostId>,
    /// `expected` is as last read ([`Coordinator::expected_hosts_unknown`] clears it: a
    /// failed read, no cut until the next good one).
    expected_known: bool,
    /// The hosts attached (an `Attach` handled, no detach since).
    attached: BTreeSet<HostId>,
    /// The world tick as the runtime last set it.
    world_tick: Tick,
    lead: Tick,
    /// `clock_ms` the period runs from (the last cut's end, or when cuts were turned on).
    last_ms: u64,
    run: Option<CutRun>,
    /// Outcomes of decided cuts, newest [`WORLD_CUT_OUTCOMES_KEPT`].
    outcomes: BTreeMap<CutId, bool>,
    /// `(host, cut)` whose outcome the runtime is to look up in the store.
    lookups: Vec<(HostId, CutId)>,
    /// Hosts asking to leave, with the `clock_ms` of their first request.
    leave_requests: BTreeMap<HostId, u64>,
    /// Hosts that left in a committed cut, with the boot they left as (`None`: unknown):
    /// not expected until another boot of theirs attaches or registers.
    left: BTreeMap<HostId, Option<u64>>,
    /// Each host's boot as its latest `Attach` named it.
    boots: BTreeMap<HostId, u64>,
    /// `clock_ms` since which a due cut has been blocked by a host mismatch.
    blocked_since: Option<u64>,
    /// `clock_ms` the mismatch was last reported at.
    blocked_noted: Option<u64>,
    /// The report not taken yet ([`Coordinator::take_cuts_blocked`]).
    blocked: Option<CutsBlocked>,
    /// Hosts succeeded (R4), with the boot that died: not expected (nor counted attached)
    /// until another boot of theirs attaches or registers, as for `left`.
    succeeded: BTreeMap<HostId, u64>,
}

impl Default for WorldCuts {
    fn default() -> Self {
        WorldCuts {
            every_ms: None,
            epoch: 0,
            next: 0,
            expected: BTreeSet::new(),
            expected_known: false,
            attached: BTreeSet::new(),
            world_tick: 0,
            lead: WORLD_CUT_LEAD_TICKS,
            last_ms: 0,
            run: None,
            outcomes: BTreeMap::new(),
            lookups: Vec::new(),
            leave_requests: BTreeMap::new(),
            left: BTreeMap::new(),
            boots: BTreeMap::new(),
            blocked_since: None,
            blocked_noted: None,
            blocked: None,
            succeeded: BTreeMap::new(),
        }
    }
}

impl Coordinator {
    /// World cuts every `every_ms` (0: off). Only under `claim` (any number of hosts): a lone
    /// `static` host saves on its own (the one-phase cut) and never gets a `BeginCut`.
    pub fn set_world_cuts(&mut self, every_ms: u64) {
        self.cuts.every_ms = (every_ms > 0).then_some(every_ms);
        self.cuts.last_ms = self.clock_ms;
    }

    /// The coordinator's claim epoch: the high half of its cut ids, so a restarted
    /// coordinator's ids never repeat an earlier one's.
    pub fn set_cut_epoch(&mut self, epoch: u64) {
        self.cuts.epoch = epoch;
    }

    /// The hosts every world cut needs a part of (those whose `hosts` row is not clean).
    pub fn set_expected_hosts(&mut self, hosts: BTreeSet<HostId>) {
        self.cuts.expected = hosts;
        self.cuts.expected_known = true;
    }

    /// The hosts a cut needs could not be read: no cut begins until they are again
    /// ([`Self::set_expected_hosts`]).
    pub fn expected_hosts_unknown(&mut self) {
        self.cuts.expected_known = false;
    }

    /// The world tick now (the coordinator's clock): a cut is placed a lead past it.
    pub fn set_world_tick(&mut self, tick: Tick) {
        self.cuts.world_tick = tick;
    }

    /// Host `host`'s link went down: a cut it takes part in and not decided yet is aborted;
    /// a leave request of it not begun is forgotten (it sends it again with its `Attach`).
    pub fn host_detached(&mut self, host: HostId) {
        self.cuts.attached.remove(&host);
        self.cuts.leave_requests.remove(&host);
        self.abort_for(host, "detached");
    }

    /// The boot each host's `hosts` row names: a host that left as another boot has
    /// registered again, and is expected again.
    pub fn set_host_boots(&mut self, boots: BTreeMap<HostId, u64>) {
        self.cuts.left.retain(|h, left_as| match (boots.get(h), *left_as) {
            (Some(b), Some(l)) if *b != l => {
                log::info!("coordinator: host {h} registered again (boot {b}): expected again");
                false
            }
            _ => true,
        });
        self.cuts.succeeded.retain(|h, dead| match boots.get(h) {
            Some(b) if b != dead => {
                log::info!("coordinator: succeeded host {h} registered again (boot {b}): expected again");
                false
            }
            _ => true,
        });
    }

    /// The ids of a successful `hosts` read (`present`): an id absent from it was pruned and is
    /// never issued again, so nothing is kept of it (B-M5) — not as left, succeeded, nor its
    /// boot. An ATTACHED id keeps all three: a host registered after the read was issued attaches
    /// before the next read shows it (its answers count per boot), and a zombie still attached
    /// stays marked until its link drops.
    pub(super) fn forget_absent_hosts(&mut self, present: &BTreeSet<HostId>) {
        let attached = &self.cuts.attached;
        let keep = |h: &HostId| present.contains(h) || attached.contains(h);
        self.cuts.left.retain(|h, _| keep(h));
        self.cuts.succeeded.retain(|h, _| keep(h));
        self.cuts.boots.retain(|h, _| keep(h));
    }

    /// Which of `left`, `succeeded` and `boots` hold host `host` ([`Self::host_mentions`]).
    pub(super) fn cut_mentions(&self, host: HostId) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.cuts.left.contains_key(&host) {
            out.push("left");
        }
        if self.cuts.succeeded.contains_key(&host) {
            out.push("succeeded");
        }
        if self.cuts.boots.contains_key(&host) {
            out.push("boots");
        }
        out
    }

    /// Why no world cut begins although one is due, if that lasted
    /// [`WORLD_CUT_BLOCKED_GRACE_MS`]: reported at most once per cut period (the runtime
    /// logs it).
    pub fn take_cuts_blocked(&mut self) -> Option<CutsBlocked> {
        self.cuts.blocked.take()
    }

    /// The hosts that left in a committed cut and have not come back.
    pub fn left_hosts(&self) -> Vec<HostId> {
        self.cuts.left.keys().copied().collect()
    }

    /// Test support: the running world cut, the hosts whose part is staged, and its
    /// verdict if taken.
    #[doc(hidden)]
    pub fn world_cut_progress(&self) -> Option<(CutId, Vec<HostId>, Option<bool>)> {
        self.cuts.run.as_ref().map(|r| (r.cut, r.prepared.iter().copied().collect(), r.verdict))
    }

    /// Host `host` asks to leave ([`crate::HostMsg::LeaveRequest`]): it is named `leaving` in
    /// the next cut, which begins [`LEAVE_BATCH_MS`] after the first request not served.
    pub(super) fn leave_request(&mut self, host: HostId) {
        if self.cuts.every_ms.is_none() {
            log::warn!("coordinator: host {host} asks to leave, but world cuts are off; ignored");
            return;
        }
        if self.cuts.left.contains_key(&host) {
            log::debug!("coordinator: host {host} asks to leave again; it left already");
            return;
        }
        let now = self.clock_ms;
        self.cuts.leave_requests.entry(host).or_insert_with(|| {
            log::info!("coordinator: host {host} asks to leave");
            now
        });
    }

    /// The decision of the running cut, once there is one (all parts staged: commit;
    /// a failure, a detach or the deadline: abort). Record it in the store and call
    /// [`Self::decided`] with the stored decision.
    pub fn take_decision(&mut self) -> Option<CutDecide> {
        let run = self.cuts.run.as_mut().filter(|r| !r.taken)?;
        let commit = run.verdict?;
        run.taken = true;
        Some(CutDecide {
            cut: run.cut,
            tick: run.tick,
            commit,
            hosts: run.hosts.clone(),
            leaving: run.leaving.clone(),
            generation: run.generation,
            settles: run.settles.clone(),
        })
    }

    /// Cut `cut` is decided as stored (`committed`): every participant is told
    /// ([`CoordMsg::CutDone`]), and the next cut may start a period from now.
    pub fn decided(&mut self, cut: CutId, committed: bool) {
        let Some(run) = self.cuts.run.take_if(|r| r.cut == cut) else {
            log::warn!("coordinator: cut {} decided ({committed}), but it is not the cut running", cut.0);
            self.remember_outcome(cut, committed);
            return;
        };
        log::info!(
            "coordinator: world cut {} at tick {} {}",
            cut.0,
            run.tick,
            if committed { "committed" } else { "aborted" }
        );
        for h in run.participants() {
            self.push(Target::Host(h), CoordMsg::CutDone { cut, committed });
        }
        if committed {
            self.cuts.lead = WORLD_CUT_LEAD_TICKS;
        }
        for &h in &run.leaving {
            self.cuts.leave_requests.remove(&h);
            if committed {
                // Its row is clean now: not expected until another boot of it comes.
                let boot = self.cuts.boots.get(&h).copied();
                log::info!("coordinator: host {h} left in world cut {}", cut.0);
                self.cuts.left.insert(h, boot);
            } else {
                log::warn!("coordinator: host {h}'s leave cut {} was aborted: it leaves without one", cut.0);
            }
        }
        self.cuts.last_ms = self.clock_ms;
        self.remember_outcome(cut, committed);
        if committed {
            self.successions_settled(&run.settles);
        }
    }

    /// `(host, cut)` pairs whose outcome is not known here: look each up in the store
    /// (an undecided one aborted) and answer with [`Self::cut_outcome`].
    pub fn take_cut_lookups(&mut self) -> Vec<(HostId, CutId)> {
        std::mem::take(&mut self.cuts.lookups)
    }

    /// The stored outcome of `cut`, for `host` (a lookup's answer).
    pub fn cut_outcome(&mut self, host: HostId, cut: CutId, committed: bool) {
        self.remember_outcome(cut, committed);
        self.push(Target::Host(host), CoordMsg::CutDone { cut, committed });
    }

    /// The world cut running (not decided yet): its id and tick.
    pub fn world_cut_running(&self) -> Option<(CutId, Tick)> {
        self.cuts.run.as_ref().map(|r| (r.cut, r.tick))
    }

    /// The lead the next world cut is placed at.
    pub fn world_cut_lead(&self) -> Tick {
        self.cuts.lead
    }

    /// An `Attach` of `host`: it is attached; a cut not decided yet that it takes part in is
    /// aborted (its link dropped: messages of the cut may be lost); the cut it says is
    /// pending is answered. A host outside the running cut (Elastic 1 Task 4: one that
    /// registered after the cut began) leaves the cut alone: every participant freezes its
    /// session with a host outside the cut from its `BeginCut` until the decision and fails
    /// its part if it exchanged state with one no cut has saved yet
    /// (`world_manager/world_cut.rs`, "Hosts outside the cut"), and the store lets the
    /// decision leave out only a host registered after the cut's begin was recorded
    /// (`world_store` `record_cut_begin`). It is in the next cut.
    pub(super) fn cut_attach(&mut self, host: HostId, pending: Option<CutId>, boot: u64) {
        self.cuts.attached.insert(host);
        self.cuts.boots.insert(host, boot);
        if self.cuts.left.get(&host).is_some_and(|left_as| *left_as != Some(boot)) {
            log::info!("coordinator: host {host} is back (boot {boot}): expected again");
            self.cuts.left.remove(&host);
        }
        if self.cuts.succeeded.get(&host).is_some_and(|dead| *dead != boot) {
            log::info!("coordinator: succeeded host {host} is back as boot {boot}: expected again");
            self.cuts.succeeded.remove(&host);
        }
        if self.cuts.run.as_ref().is_some_and(|r| r.has(host)) {
            self.abort_running(host, "re-attached");
        } else if let Some(run) = self.cuts.run.as_ref().filter(|r| r.verdict.is_none()) {
            log::info!("coordinator: host {host} attached during world cut {}, which it is not in: it is in the next one", run.cut.0);
        }
        if let Some(cut) = pending {
            if self.cuts.run.as_ref().is_none_or(|r| r.cut != cut) {
                self.answer_cut(host, cut);
            }
        }
    }

    pub(super) fn cut_prepared(&mut self, host: HostId, cut: CutId) {
        match self.cuts.run.as_mut() {
            Some(run) if run.cut == cut => {
                if !run.has(host) {
                    log::warn!("coordinator: host {host} prepared cut {}, which it does not take part in", cut.0);
                    return;
                }
                if run.verdict.is_none() {
                    run.prepared.insert(host);
                    if run.participants().all(|h| run.prepared.contains(&h)) {
                        run.verdict = Some(true);
                    }
                }
            }
            _ => self.answer_cut(host, cut),
        }
    }

    pub(super) fn cut_failed(&mut self, host: HostId, cut: CutId, why: CutFailure) {
        match self.cuts.run.as_mut() {
            Some(run) if run.cut == cut => {
                if run.verdict.is_none() && run.has(host) {
                    log::warn!("coordinator: world cut {}: host {host} failed ({why:?}); aborted", cut.0);
                    run.verdict = Some(false);
                    if why == CutFailure::Missed {
                        self.cuts.lead = (self.cuts.lead * 2).min(WORLD_CUT_LEAD_MAX_TICKS);
                    }
                }
            }
            _ => self.answer_cut(host, cut),
        }
    }

    /// The upkeep: a running cut past its deadline is aborted; with none running, a cut
    /// begins once a period passed and every expected host is attached.
    pub(super) fn maintain_cuts(&mut self, now_ms: u64) {
        if let Some(run) = self.cuts.run.as_mut() {
            if run.verdict.is_none() && now_ms >= run.deadline_ms {
                log::warn!(
                    "coordinator: world cut {} still waiting for {:?} past its deadline; aborted",
                    run.cut.0,
                    run.participants().filter(|h| !run.prepared.contains(h)).collect::<Vec<_>>()
                );
                run.verdict = Some(false);
            }
            return;
        }
        let Some(every) = self.cuts.every_ms else { return };
        // A leave batch due (its first request `LEAVE_BATCH_MS` old) begins at once.
        let leave_due = self.cuts.leave_requests.values().min().is_some_and(|first| now_ms >= first + LEAVE_BATCH_MS);
        // R4: an open succession every recipient answered is settled by the next cut — due
        // `SETTLE_CUT_RETRY_MS` after the last cut ended, not a save period.
        let settle_due = self.settle_due() && now_ms >= self.cuts.last_ms.saturating_add(every.min(SETTLE_CUT_RETRY_MS));
        if !leave_due && !settle_due && now_ms < self.cuts.last_ms.saturating_add(every) {
            return;
        }
        // Every expected host attached, and no attached host outside the set (one that
        // registered since the last read would run unsaved state beside the cut). A host
        // that left is neither (its process may still be attached on its way out).
        // A succeeded host is neither either (a zombie of it may still be attached).
        let (left, succeeded) = (&self.cuts.left, &self.cuts.succeeded);
        let out = |h: &HostId| !left.contains_key(h) && !succeeded.contains_key(h);
        let expected: BTreeSet<HostId> = self.cuts.expected.iter().copied().filter(out).collect();
        let attached: BTreeSet<HostId> = self.cuts.attached.iter().copied().filter(out).collect();
        // R4: no cut while a succession is open and not answered by everyone (nor while one
        // is being declared, a rollback is wanted, or the open ones are not known yet).
        let waiting = self.successions_block_cuts();
        let mismatch = self.cuts.expected_known && expected != attached;
        if !self.cuts.expected_known || expected != attached || expected.is_empty() || waiting.is_some() {
            // A due cut blocked by a mismatch, or by successions waiting for answers, is
            // reported (after the grace, once a period); an unknown read is the runtime's own
            // warning, and no host at all is idle.
            let waiting = waiting.unwrap_or_default();
            if mismatch || !waiting.is_empty() {
                let since = *self.cuts.blocked_since.get_or_insert(now_ms);
                let noted = self.cuts.blocked_noted.is_some_and(|at| now_ms < at.saturating_add(every));
                if now_ms >= since.saturating_add(WORLD_CUT_BLOCKED_GRACE_MS) && !noted {
                    self.cuts.blocked_noted = Some(now_ms);
                    let (missing, unexpected) = if mismatch {
                        (expected.difference(&attached).copied().collect(), attached.difference(&expected).copied().collect())
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    let awaiting_heir = self.successions_awaiting_heir(&waiting);
                    self.cuts.blocked = Some(CutsBlocked { missing, unexpected, successions: waiting, awaiting_heir });
                }
            } else {
                self.cuts.blocked_since = None;
            }
            return;
        }
        self.cuts.blocked_since = None;
        self.cuts.blocked_noted = None;
        self.cuts.next += 1;
        let cut = CutId((self.cuts.epoch << 32) | self.cuts.next);
        let tick = self.cuts.world_tick + self.cuts.lead;
        let hosts: Vec<HostId> = expected.iter().copied().collect();
        // The hosts leaving in this cut (among `hosts`); a request of a host not expected is
        // dropped (it is not in the cut; it exits without one).
        let leaving: Vec<HostId> = if leave_due {
            let asked: Vec<HostId> = std::mem::take(&mut self.cuts.leave_requests).into_keys().collect();
            for h in asked.iter().filter(|h| !expected.contains(h)) {
                log::warn!("coordinator: host {h} asks to leave, but it is not expected; ignored");
            }
            asked.into_iter().filter(|h| expected.contains(h)).collect()
        } else {
            Vec::new()
        };
        let generation = self.generation_seen;
        // Only answered successions (every survivor applied, the heir ADOPTED: its part
        // carries `adopts`, Task 9) — none begins otherwise; the store does not check it.
        let settles = self.successions_to_settle();
        log::info!(
            "coordinator: world cut {} at tick {tick} (lead {}) of hosts {hosts:?}, leaving {leaving:?}",
            cut.0,
            self.cuts.lead
        );
        for &h in &hosts {
            let msg = CoordMsg::BeginCut { cut, tick, hosts: hosts.clone(), leaving: leaving.clone(), generation };
            self.push(Target::Host(h), msg);
        }
        self.cuts.run = Some(CutRun {
            cut,
            tick,
            hosts,
            leaving,
            generation,
            settles,
            prepared: BTreeSet::new(),
            deadline_ms: now_ms + WORLD_CUT_DEADLINE_MS,
            verdict: None,
            taken: false,
        });
    }

    /// Host `host`'s count of what it received from participant `peer` in world cut `cut`
    /// ([`crate::HostMsg::CutPeerCount`]): relayed to `peer` while `cut` runs undecided and
    /// both are participants (two different ones); dropped otherwise — a count of a decided
    /// cut (an abort for a lapsed participant included) is never used.
    pub(super) fn cut_peer_count(&mut self, host: HostId, cut: CutId, peer: HostId, received_through: u64, boot: u64) {
        let running = self.cuts.run.as_ref().filter(|r| r.cut == cut && r.verdict.is_none());
        if !running.is_some_and(|r| host != peer && r.has(host) && r.has(peer)) {
            log::debug!("coordinator: host {host}'s count toward host {peer} in cut {} dropped: not between participants of it running", cut.0);
            return;
        }
        log::info!("coordinator: world cut {}: host {host} counts {received_through} from host {peer}; relayed", cut.0);
        self.push(Target::Host(peer), CoordMsg::CutPeerCount { cut, from: host, received_through, boot });
    }

    /// The participants of the running cut (decided or not, until its decision is
    /// recorded), if one runs.
    pub(super) fn cuts_running_participants(&self) -> Option<Vec<HostId>> {
        self.cuts.run.as_ref().map(|r| r.participants().collect())
    }

    /// A participant `host` of the running cut lost its lease: the cut, not decided yet, is
    /// aborted (a succession is declared only between cuts).
    pub(super) fn abort_cut_for_lapse(&mut self, host: HostId) {
        self.abort_for(host, "lost its lease");
    }

    /// Whether `host` asked to leave (its request waits, or the cut it leaves in runs).
    pub(super) fn leave_asked(&self, host: HostId) -> bool {
        self.cuts.leave_requests.contains_key(&host) || self.cuts.run.as_ref().is_some_and(|r| r.leaving.contains(&host))
    }

    /// The hosts attached now that neither left nor were succeeded.
    pub(super) fn attached_live(&self) -> Vec<HostId> {
        self.cuts
            .attached
            .iter()
            .copied()
            .filter(|h| !self.cuts.left.contains_key(h) && !self.cuts.succeeded.contains_key(h))
            .collect()
    }

    /// Whether a message of `host` comes from a succeeded boot of it (`boot`: the boot an
    /// `Attach` names; otherwise the boot of its latest `Attach`): a zombie, ignored.
    pub(super) fn from_succeeded_boot(&self, host: HostId, boot: Option<u64>) -> bool {
        let Some(&dead) = self.cuts.succeeded.get(&host) else { return false };
        boot.or_else(|| self.cuts.boots.get(&host).copied()) == Some(dead)
    }

    pub(super) fn is_attached(&self, host: HostId) -> bool {
        self.cuts.attached.contains(&host)
    }

    /// The boot host `host`'s latest `Attach` named (`None`: it never attached).
    pub(super) fn attached_boot(&self, host: HostId) -> Option<u64> {
        self.cuts.boots.get(&host).copied()
    }

    /// The successions a commit decided here and not yet answered by the runtime would
    /// settle: the store may hold them settled already.
    pub(super) fn settles_pending(&self) -> Vec<SuccessionId> {
        self.cuts.run.as_ref().filter(|r| r.verdict == Some(true)).map(|r| r.settles.clone()).unwrap_or_default()
    }

    /// Whether `host`'s latest known boot was succeeded and no other boot of it came since.
    pub(super) fn host_is_succeeded(&self, host: HostId) -> bool {
        self.cuts.succeeded.contains_key(&host)
    }

    /// Host `host`'s boot `boot` was succeeded (R4): not expected, nor counted attached,
    /// until another boot of it comes.
    pub(super) fn note_succeeded(&mut self, host: HostId, boot: u64) {
        self.cuts.succeeded.insert(host, boot);
        self.cuts.leave_requests.remove(&host);
    }

    /// A running cut `host` takes part in, not decided yet, is aborted (`why`, for the log).
    fn abort_for(&mut self, host: HostId, why: &str) {
        if self.cuts.run.as_ref().is_some_and(|r| r.has(host)) {
            self.abort_running(host, why);
        }
    }

    /// The running cut, not decided yet, is aborted because of `host` (`why`, for the log).
    fn abort_running(&mut self, host: HostId, why: &str) {
        if let Some(run) = self.cuts.run.as_mut().filter(|r| r.verdict.is_none()) {
            log::warn!("coordinator: world cut {}: host {host} {why}; aborted", run.cut.0);
            run.verdict = Some(false);
        }
    }

    /// `host` asks about `cut`, which is not the cut running: its outcome if remembered,
    /// else the runtime looks it up.
    fn answer_cut(&mut self, host: HostId, cut: CutId) {
        match self.cuts.outcomes.get(&cut).copied() {
            Some(committed) => self.push(Target::Host(host), CoordMsg::CutDone { cut, committed }),
            None => {
                if !self.cuts.lookups.contains(&(host, cut)) {
                    self.cuts.lookups.push((host, cut));
                }
            }
        }
    }

    fn remember_outcome(&mut self, cut: CutId, committed: bool) {
        self.cuts.outcomes.insert(cut, committed);
        while self.cuts.outcomes.len() > WORLD_CUT_OUTCOMES_KEPT {
            self.cuts.outcomes.pop_first();
        }
    }
}
