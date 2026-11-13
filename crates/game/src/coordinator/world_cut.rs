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

use std::collections::{BTreeMap, BTreeSet};

use super::{CoordMsg, Coordinator, CutFailure, Target};
use crate::{CutId, Tick};

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

/// A world cut to decide in the store ([`Coordinator::take_decision`]): record `commit`
/// for `cut` (`hosts` staged their parts, `leaving` leave in it, the world at
/// `generation`), then tell the core the STORED decision ([`Coordinator::decided`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutDecide {
    pub cut: CutId,
    pub tick: Tick,
    pub commit: bool,
    pub hosts: Vec<u32>,
    pub leaving: Vec<u32>,
    pub generation: u64,
}

/// The world cut running.
#[derive(Debug)]
struct CutRun {
    cut: CutId,
    tick: Tick,
    hosts: Vec<u32>,
    leaving: Vec<u32>,
    generation: u64,
    /// Participants whose part is staged.
    prepared: BTreeSet<u32>,
    /// `clock_ms` past which it is aborted.
    deadline_ms: u64,
    /// Decided here (`Some(commit)`), waiting for the runtime.
    verdict: Option<bool>,
    /// The decision was handed to the runtime ([`Coordinator::take_decision`]).
    taken: bool,
}

impl CutRun {
    /// Every host taking part (`hosts`: the leaving ones are among them).
    fn participants(&self) -> impl Iterator<Item = u32> + '_ {
        self.hosts.iter().copied()
    }

    fn has(&self, host: u32) -> bool {
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
    expected: BTreeSet<u32>,
    /// `expected` is as last read ([`Coordinator::expected_hosts_unknown`] clears it: a
    /// failed read, no cut until the next good one).
    expected_known: bool,
    /// The hosts attached (an `Attach` handled, no detach since).
    attached: BTreeSet<u32>,
    /// The world tick as the runtime last set it.
    world_tick: Tick,
    lead: Tick,
    /// `clock_ms` the period runs from (the last cut's end, or when cuts were turned on).
    last_ms: u64,
    run: Option<CutRun>,
    /// Outcomes of decided cuts, newest [`WORLD_CUT_OUTCOMES_KEPT`].
    outcomes: BTreeMap<CutId, bool>,
    /// `(host, cut)` whose outcome the runtime is to look up in the store.
    lookups: Vec<(u32, CutId)>,
    /// Hosts asking to leave, with the `clock_ms` of their first request.
    leave_requests: BTreeMap<u32, u64>,
    /// Hosts that left in a committed cut, with the boot they left as (`None`: unknown):
    /// not expected until another boot of theirs attaches or registers.
    left: BTreeMap<u32, Option<u64>>,
    /// Each host's boot as its latest `Attach` named it.
    boots: BTreeMap<u32, u64>,
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
        }
    }
}

impl Coordinator {
    /// World cuts every `every_ms` (0: off). Only with several hosts: one host saves on
    /// its own (the one-phase cut) and never gets a `BeginCut`.
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
    pub fn set_expected_hosts(&mut self, hosts: BTreeSet<u32>) {
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
    pub fn host_detached(&mut self, host: u32) {
        self.cuts.attached.remove(&host);
        self.cuts.leave_requests.remove(&host);
        self.abort_for(host, "detached");
    }

    /// The boot each host's `hosts` row names: a host that left as another boot has
    /// registered again, and is expected again.
    pub fn set_host_boots(&mut self, boots: BTreeMap<u32, u64>) {
        self.cuts.left.retain(|h, left_as| match (boots.get(h), *left_as) {
            (Some(b), Some(l)) if *b != l => {
                log::info!("coordinator: host {h} registered again (boot {b}): expected again");
                false
            }
            _ => true,
        });
    }

    /// The hosts that left in a committed cut and have not come back.
    pub fn left_hosts(&self) -> Vec<u32> {
        self.cuts.left.keys().copied().collect()
    }

    /// Test support: the running world cut, the hosts whose part is staged, and its
    /// verdict if taken.
    #[doc(hidden)]
    pub fn world_cut_progress(&self) -> Option<(CutId, Vec<u32>, Option<bool>)> {
        self.cuts.run.as_ref().map(|r| (r.cut, r.prepared.iter().copied().collect(), r.verdict))
    }

    /// Host `host` asks to leave ([`crate::HostMsg::LeaveRequest`]): it is named `leaving` in
    /// the next cut, which begins [`LEAVE_BATCH_MS`] after the first request not served.
    pub(super) fn leave_request(&mut self, host: u32) {
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
    }

    /// `(host, cut)` pairs whose outcome is not known here: look each up in the store
    /// (an undecided one aborted) and answer with [`Self::cut_outcome`].
    pub fn take_cut_lookups(&mut self) -> Vec<(u32, CutId)> {
        std::mem::take(&mut self.cuts.lookups)
    }

    /// The stored outcome of `cut`, for `host` (a lookup's answer).
    pub fn cut_outcome(&mut self, host: u32, cut: CutId, committed: bool) {
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

    /// An `Attach` of `host`: it is attached; a cut not decided yet is aborted — one it
    /// takes part in (its link dropped: messages of the cut may be lost), and one it does
    /// not (a host arriving makes the cut's participants stale: it may exchange entities
    /// with them before the cut's tick, which no part would save); the cut it says is
    /// pending is answered.
    pub(super) fn cut_attach(&mut self, host: u32, pending: Option<CutId>, boot: u64) {
        self.cuts.attached.insert(host);
        self.cuts.boots.insert(host, boot);
        if self.cuts.left.get(&host).is_some_and(|left_as| *left_as != Some(boot)) {
            log::info!("coordinator: host {host} is back (boot {boot}): expected again");
            self.cuts.left.remove(&host);
        }
        let why = if self.cuts.run.as_ref().is_some_and(|r| r.has(host)) { "re-attached" } else { "attached, not in the cut" };
        self.abort_running(host, why);
        if let Some(cut) = pending {
            if self.cuts.run.as_ref().is_none_or(|r| r.cut != cut) {
                self.answer_cut(host, cut);
            }
        }
    }

    pub(super) fn cut_prepared(&mut self, host: u32, cut: CutId) {
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

    pub(super) fn cut_failed(&mut self, host: u32, cut: CutId, why: CutFailure) {
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
        if !leave_due && now_ms < self.cuts.last_ms.saturating_add(every) {
            return;
        }
        // Every expected host attached, and no attached host outside the set (one that
        // registered since the last read would run unsaved state beside the cut). A host
        // that left is neither (its process may still be attached on its way out).
        let left = &self.cuts.left;
        let expected: BTreeSet<u32> = self.cuts.expected.iter().copied().filter(|h| !left.contains_key(h)).collect();
        let attached: BTreeSet<u32> = self.cuts.attached.iter().copied().filter(|h| !left.contains_key(h)).collect();
        if !self.cuts.expected_known || expected.is_empty() || expected != attached {
            return;
        }
        self.cuts.next += 1;
        let cut = CutId((self.cuts.epoch << 32) | self.cuts.next);
        let tick = self.cuts.world_tick + self.cuts.lead;
        let hosts: Vec<u32> = expected.iter().copied().collect();
        // The hosts leaving in this cut (among `hosts`); a request of a host not expected is
        // dropped (it is not in the cut; it exits without one).
        let leaving: Vec<u32> = if leave_due {
            let asked: Vec<u32> = std::mem::take(&mut self.cuts.leave_requests).into_keys().collect();
            for h in asked.iter().filter(|h| !expected.contains(h)) {
                log::warn!("coordinator: host {h} asks to leave, but it is not expected; ignored");
            }
            asked.into_iter().filter(|h| expected.contains(h)).collect()
        } else {
            Vec::new()
        };
        let generation = self.generation_seen;
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
            prepared: BTreeSet::new(),
            deadline_ms: now_ms + WORLD_CUT_DEADLINE_MS,
            verdict: None,
            taken: false,
        });
    }

    /// A running cut `host` takes part in, not decided yet, is aborted (`why`, for the log).
    fn abort_for(&mut self, host: u32, why: &str) {
        if self.cuts.run.as_ref().is_some_and(|r| r.has(host)) {
            self.abort_running(host, why);
        }
    }

    /// The running cut, not decided yet, is aborted because of `host` (`why`, for the log).
    fn abort_running(&mut self, host: u32, why: &str) {
        if let Some(run) = self.cuts.run.as_mut().filter(|r| r.verdict.is_none()) {
            log::warn!("coordinator: world cut {}: host {host} {why}; aborted", run.cut.0);
            run.verdict = Some(false);
        }
    }

    /// `host` asks about `cut`, which is not the cut running: its outcome if remembered,
    /// else the runtime looks it up.
    fn answer_cut(&mut self, host: u32, cut: CutId) {
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
