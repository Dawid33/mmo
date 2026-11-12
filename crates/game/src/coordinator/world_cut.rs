//! The coordinator's half of the world cut (several hosts, R3 Task 13): it picks ONE world
//! tick for a cut, asks every expected host for its part ([`CoordMsg::BeginCut`]), and
//! once every part is staged ([`HostMsg::CutPrepared`]) hands the runtime a decision
//! ([`Coordinator::take_decision`]) to record in the store; the outcome goes to every
//! participant ([`CoordMsg::CutDone`]). Any [`HostMsg::CutFailed`], a participant
//! detaching or re-attaching, or [`WORLD_CUT_DEADLINE_MS`] passing decides an abort. One
//! cut at a time: a new one starts only once the last is decided, a period
//! ([`Coordinator::set_world_cuts`]) after it, and only while every expected host is
//! attached. With world cuts off (one host, role `all`) nothing here sends anything.

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
    /// Every host taking part (the leaving ones too).
    fn participants(&self) -> impl Iterator<Item = u32> + '_ {
        self.hosts.iter().chain(&self.leaving).copied()
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

    /// Host `host`'s link went down: a cut it takes part in and not decided yet is aborted.
    pub fn host_detached(&mut self, host: u32) {
        self.cuts.attached.remove(&host);
        self.abort_for(host, "detached");
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

    /// An `Attach` of `host`: it is attached; a cut it takes part in and not decided yet
    /// is aborted (its link dropped: messages of the cut may be lost); the cut it says is
    /// pending is answered.
    pub(super) fn cut_attach(&mut self, host: u32, pending: Option<CutId>) {
        self.cuts.attached.insert(host);
        self.abort_for(host, "re-attached");
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
        if now_ms < self.cuts.last_ms.saturating_add(every) {
            return;
        }
        // Every expected host attached, and no attached host outside the set (one that
        // registered since the last read would run unsaved state beside the cut).
        if !self.cuts.expected_known
            || self.cuts.expected.is_empty()
            || self.cuts.expected != self.cuts.attached
        {
            return;
        }
        self.cuts.next += 1;
        let cut = CutId((self.cuts.epoch << 32) | self.cuts.next);
        let tick = self.cuts.world_tick + self.cuts.lead;
        let hosts: Vec<u32> = self.cuts.expected.iter().copied().collect();
        // Task 14: the hosts leaving in this cut.
        let leaving = Vec::new();
        let generation = self.generation_seen;
        log::info!("coordinator: world cut {} at tick {tick} (lead {}) of hosts {hosts:?}", cut.0, self.cuts.lead);
        for &h in &hosts {
            let msg = CoordMsg::BeginCut { cut, tick, hosts: hosts.clone(), leaving: Vec::new(), generation };
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
        if let Some(run) = self.cuts.run.as_mut().filter(|r| r.verdict.is_none() && r.has(host)) {
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
