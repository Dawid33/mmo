//! The world coordinator: the services that do not partition by region — the history and
//! its calendar, site claims (fortress / adventure exclusivity per player), embark / return /
//! adventure validation, embark options, legends, person info, the daily calendar / site
//! broadcasts and notices. A host talks to it only through [`HostMsg`] / [`CoordMsg`] over a
//! [`WorldLink`]. Threadless and IO-free: time arrives as `now_ms`, saving is the caller's
//! ([`Coordinator::take_save`] / [`Coordinator::saved`]).
//!
//! **Durable before visible:** a directive, or a reply granting a claim, leaves
//! ([`Coordinator::take_out`]) only after a save holding the state that produced it is
//! committed. The outgoing stream keeps its order: once a gated message waits, everything
//! after it waits too, so a host sees the coordinator's messages in the order they were
//! decided (a later `SpawnPeople` never overtakes an earlier directive for that region).
//!
//! **Many hosts:** every outgoing message carries its [`Target`]: a region's messages go to
//! the host that announced the region (`RegionStarted`, its `Attach`, a delivered
//! `PersonCrossed`), a reply or claim answer to the host that asked, the world's view to
//! every host. Which host runs which region (`region_host`) and where each player is
//! online (`online_on`) are rebuilt by the hosts' `Attach`es, never saved.

mod link;
mod msg;
mod state;
mod world_cut;

pub use link::*;
pub use msg::*;
pub use state::*;
pub use world_cut::*;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::history::persist::HistoryLoadError;
use crate::history::{
    AdventureRefusal, CalendarTick, Directive, EmbarkRefusal, History, SiteId, SocietyGrid, TICKS_PER_DAY,
};
use crate::{EmbarkRequest, PlayerId, RegionCoords};

/// Answers of state-changing requests remembered for replay past which the coordinator
/// logs an error (at most once per [`MEMO_WARN_SECS`]): answers are forgotten only when
/// their host acknowledges them ([`HostMsg::Settled`], an `Attach`'s watermark or new
/// boot), never by count — a forgotten answer would re-execute a resent request.
pub const ANSWERED_MEMO_MAX: usize = 4096;
/// Seconds between two errors about a memo over [`ANSWERED_MEMO_MAX`].
pub const MEMO_WARN_SECS: u64 = 60;
/// Most times one host renumbers one player's placement on a [`CoordMsg::HopIs`] (per
/// placement: a new login or arrival starts over): two hosts that both hold the player (a duplicate body)
/// would otherwise outbid each other for ever. Past it the host logs an error once and
/// stops renumbering that player.
pub const HOP_RENUMBER_MAX_PER_PLAYER: u32 = 8;

/// A host's count of [`CoordMsg::HopIs`] renumberings per player
/// ([`HOP_RENUMBER_MAX_PER_PLAYER`]).
#[derive(Debug, Default)]
pub struct RenumberBudget {
    used: BTreeMap<PlayerId, u32>,
    errors: u32,
}

impl RenumberBudget {
    /// Whether `player` may be renumbered once more (counted if so). The first refusal
    /// logs an error.
    pub fn take(&mut self, player: PlayerId) -> bool {
        let n = self.used.entry(player).or_default();
        if *n < HOP_RENUMBER_MAX_PER_PLAYER {
            *n += 1;
            return true;
        }
        if *n == HOP_RENUMBER_MAX_PER_PLAYER {
            *n += 1;
            self.errors += 1;
            log::error!(
                "{player:?}: renumbered {HOP_RENUMBER_MAX_PER_PLAYER} times; another host holds it too — no more renumbering"
            );
        }
        false
    }

    /// `player` is placed on the host anew (a login, an arrival): its budget starts over.
    /// Renumbering itself is no new placement, so two holders still stop outbidding.
    pub fn reset(&mut self, player: PlayerId) {
        self.used.remove(&player);
    }

    /// Errors logged (players whose budget ran out).
    pub fn errors_logged(&self) -> u32 {
        self.errors
    }
}
/// Seconds between periodic saves of a coordinator whose state changed.
pub const COORDINATOR_SAVE_SECS: u64 = 60;

/// The sites as last broadcast, bucketed: a broadcast goes out when a site is founded or
/// abandoned, or a population crosses a multiple of 10.
fn sites_key(history: &History) -> Vec<(SiteId, bool, u32, bool)> {
    history.site_markers().into_iter().map(|m| (m.site, m.abandoned, m.population / 10, m.returnable)).collect()
}

/// The world coordinator's core.
pub struct Coordinator {
    history: History,
    /// Fortress claims: site → (its player, whether that player is online). A return to a
    /// site claimed by an online player is refused `InUse`; an offline claimant loses it.
    claims: BTreeMap<SiteId, (PlayerId, bool)>,
    /// What each player plays (one fortress or one adventure at a time).
    player_claims: BTreeMap<PlayerId, Claim>,
    /// Each region's latest incarnation seen start (`RegionStarted`, `Attach`): the stamp
    /// on its directives, and the bar a bounce must meet to be deferred.
    incarnations: BTreeMap<RegionCoords, u64>,
    /// Answers of state-changing requests, replayed for a repeated id.
    memo: BTreeMap<RequestId, WorldReply>,
    /// `memo`'s ids, oldest first (the saved order).
    memo_order: VecDeque<RequestId>,
    /// Per host, the boots its `Attach`es named: a new one forgets the host's answers of
    /// every other boot. Never saved (an unknown boot after a restart forgets nothing the
    /// host could still ask about: its process is the one attaching).
    boots_seen: BTreeMap<u32, BTreeSet<u64>>,
    /// `clock_ms` of the last error about an oversized memo.
    memo_warned_ms: Option<u64>,
    /// Waiting for a save: the first gated message and everything after it, in order.
    gated: VecDeque<(Target, CoordMsg)>,
    /// Free to leave, in order.
    out: Vec<(Target, CoordMsg)>,
    /// Which host runs each region, as announced (`RegionStarted`, `Attach`, a delivered
    /// `PersonCrossed`); a park forgets it. Rebuilt by every `Attach`, never saved.
    region_host: BTreeMap<RegionCoords, u32>,
    /// The host each online player is online on (`PlayerOnline`, an `Attach`'s online
    /// list, the host that was granted its claim). Rebuilt by every `Attach`, never saved.
    /// A player in transit to a host (`PlayerHandedTo`) stays online on that host until it
    /// reports. Never saved.
    online_on: BTreeMap<PlayerId, u32>,
    /// The highest hop ([`HostMsg`]'s hop rule) of each player's presence messages
    /// applied: a message with a lower one is stale. Unknown = 0 (a restart loses it and
    /// the next reports set it again); cleared by an `Attach` of a newer world
    /// generation (every host starts its hops over). Never saved.
    hop_of: BTreeMap<PlayerId, u64>,
    /// The highest world generation an `Attach` named.
    generation_seen: u64,
    /// For a player online on a host by that host's own report (not in transit to it):
    /// the host and the hop it reported. A message of another host that moves the player
    /// away from it tells it ([`CoordMsg::HopIs`]): if it still holds the player it
    /// re-asserts above the new hop. Never saved.
    online_reported: BTreeMap<PlayerId, (u32, u64)>,
    /// Displaced-host notifications sent, `(player, host, reported hop)`: at most one each.
    /// A player's go when it goes offline on its host; all go at a newer generation.
    displaced_told: BTreeSet<(PlayerId, u32, u64)>,
    /// `Some(n)`: a save was taken ([`Self::take_save`]) holding the state behind the
    /// first `n` messages of `gated`; [`Self::saved`] releases them.
    save_in_flight: Option<usize>,
    /// The state changed since the last save was taken.
    dirty_since_save: bool,
    /// A save was asked for ([`Self::request_save`]).
    force_save: bool,
    /// `now_ms` of the last save taken (or of the start).
    last_save_ms: u64,
    /// The latest `now_ms` seen.
    clock_ms: u64,
    /// `(now_ms, calendar tick)` the calendar was anchored at.
    calendar_anchor: (u64, CalendarTick),
    /// The calendar day last broadcast.
    calendar_day: u64,
    /// The sites as last broadcast ([`sites_key`]).
    sites_key: Vec<(SiteId, bool, u32, bool)>,
    /// The world cuts of several hosts (`world_cut.rs`).
    cuts: WorldCuts,
}

impl Coordinator {
    /// A coordinator over `history`, its calendar running from `history.now()` at
    /// `now_ms`. Its first outgoing message is a [`CoordMsg::View`].
    pub fn new(history: History, now_ms: u64) -> Self {
        let calendar_anchor = (now_ms, history.now());
        let mut c = Coordinator {
            calendar_day: history.now().0 / TICKS_PER_DAY,
            sites_key: sites_key(&history),
            history,
            claims: BTreeMap::new(),
            player_claims: BTreeMap::new(),
            incarnations: BTreeMap::new(),
            memo: BTreeMap::new(),
            memo_order: VecDeque::new(),
            boots_seen: BTreeMap::new(),
            memo_warned_ms: None,
            gated: VecDeque::new(),
            out: Vec::new(),
            region_host: BTreeMap::new(),
            online_on: BTreeMap::new(),
            hop_of: BTreeMap::new(),
            generation_seen: 0,
            online_reported: BTreeMap::new(),
            displaced_told: BTreeSet::new(),
            save_in_flight: None,
            dirty_since_save: false,
            force_save: false,
            last_save_ms: now_ms,
            clock_ms: now_ms,
            calendar_anchor,
            cuts: WorldCuts::default(),
        };
        c.push_view(Target::Every);
        c
    }

    /// A coordinator restarted from its saved state: the history loaded (everyone embodied
    /// back in the abstract history until a host's [`HostMsg::Attach`]), every claimant
    /// offline until its host says otherwise, the memo as saved.
    pub fn from_state(state: CoordinatorState, grid: SocietyGrid, now_ms: u64) -> Result<Self, HistoryLoadError> {
        let history = crate::history::persist::decode(&state.history, grid)?;
        let mut c = Coordinator::new(history, now_ms);
        for (player, claim) in state.claims {
            if let Claim::Fortress(site) = claim {
                c.claims.insert(site, (player, false));
            }
            c.player_claims.insert(player, claim);
        }
        for (id, reply) in state.memo {
            c.remember(id, reply);
        }
        Ok(c)
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// The history, to change directly (tests, tools). What it decides goes out at the
    /// next [`Self::handle`] / [`Self::maintain`]. Never call its region lifecycle methods
    /// through it: the hosts' messages own them.
    pub fn history_mut(&mut self) -> &mut History {
        self.dirty_since_save = true;
        &mut self.history
    }

    /// What `player` plays, as the coordinator has it.
    pub fn claim_of(&self, player: PlayerId) -> Option<Claim> {
        self.player_claims.get(&player).copied()
    }

    /// The host `player` is online on, as the coordinator has it.
    pub fn online_on(&self, player: PlayerId) -> Option<u32> {
        self.online_on.get(&player).copied()
    }

    /// The highest hop of `player`'s presence messages applied (0: none known).
    pub fn hop_of(&self, player: PlayerId) -> u64 {
        self.hop_of.get(&player).copied().unwrap_or(0)
    }

    /// The host that runs `rc`, as announced to the coordinator.
    pub fn region_host(&self, rc: RegionCoords) -> Option<u32> {
        self.region_host.get(&rc).copied()
    }

    /// The calendar at `now_ms`: the anchor's tick plus one per `TICK_RATE` ms since.
    pub fn calendar_now(&self, now_ms: u64) -> CalendarTick {
        let (ms, tick) = self.calendar_anchor;
        CalendarTick(tick.0 + now_ms.saturating_sub(ms) / crate::TICK_RATE)
    }

    /// Apply one message of host `host` (the ordinal its link is for).
    pub fn handle(&mut self, host: u32, msg: HostMsg, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.dirty_since_save = true;
        match msg {
            HostMsg::Attach { host: named, epoch, running, online, generation, boot, settled_below, handed, pending_cut } => {
                if named != host {
                    log::warn!("coordinator: host {host}'s Attach names host {named}; taken as host {host}'s");
                }
                if generation > self.generation_seen {
                    // A newer world generation: every host started its hops over.
                    if !self.hop_of.is_empty() {
                        log::info!("coordinator: world generation {generation}: player hops start over");
                    }
                    self.generation_seen = generation;
                    self.hop_of.clear();
                    self.online_reported.clear();
                    self.displaced_told.clear();
                }
                self.forget_settled(host, boot, settled_below, true);
                self.attach(host, epoch, running, online, handed);
                self.cut_attach(host, pending_cut);
            }
            HostMsg::Settled { boot, below } => self.forget_settled(host, boot, below, false),
            HostMsg::RegionStarted { rc, incarnation } => {
                if let Some(other) = self.region_host.insert(rc, host).filter(|h| *h != host) {
                    log::warn!("coordinator: {rc:?} started on host {host}; host {other} had announced it");
                }
                self.incarnations.insert(rc, incarnation);
                let events = self.history.region_started(rc);
                self.push(Target::Host(host), CoordMsg::SpawnPeople { rc, incarnation, events });
            }
            HostMsg::RegionStopping { rc } => {
                if !self.runs_elsewhere(rc, host) {
                    self.history.region_stopping(rc);
                }
            }
            HostMsg::RegionParked { rc, people } => {
                if !self.runs_elsewhere(rc, host) {
                    self.region_host.remove(&rc);
                    self.history.region_parked(rc, people);
                }
            }
            HostMsg::PersonCrossed { person, to, delivered } => {
                if delivered {
                    // The host put the body in `to`, which it runs.
                    self.region_host.entry(to).or_insert(host);
                }
                let decided = self.history.person_crossed(person, to);
                if decided != delivered {
                    log::error!(
                        "coordinator: {person:?} crossed into {to:?}: the host {} the body, the history wanted it {}",
                        if delivered { "delivered" } else { "dropped" },
                        if decided { "delivered" } else { "absorbed" },
                    );
                }
                if delivered && !decided {
                    // The host's view of running wins: the body stands in `to`.
                    if let Some(d) = self.history.adopt_body(to, person) {
                        self.push_directive(d);
                    }
                }
            }
            HostMsg::Positions(people) => self.history.note_positions(people),
            HostMsg::DirectiveBounced { directive, incarnation } => {
                let latest = self.incarnations.get(&directive.region).copied().unwrap_or(0);
                if incarnation < latest {
                    log::debug!(
                        "coordinator: bounce for {:?} incarnation {incarnation} (latest {latest}) dropped: {:?}",
                        directive.region,
                        directive.event
                    );
                } else {
                    self.history.defer_directive(directive);
                }
            }
            HostMsg::Request { id, player, req } => self.request(host, id, player, req),
            HostMsg::Settle { id, req } => self.settle(host, id, req),
            HostMsg::PlayerOnline(p) => self.player_online(host, p),
            HostMsg::PlayerOffline { player, hop } => self.player_offline(host, player, hop),
            HostMsg::PlayerHandedTo { player, to, hop } => self.player_handed(host, player, to, hop),
            HostMsg::ReleaseClaim { player } => self.release_claim(player),
            HostMsg::CutPrepared { cut } => self.cut_prepared(host, cut),
            HostMsg::CutFailed { cut, why } => self.cut_failed(host, cut, why),
        }
        self.push_directives();
    }

    /// A lifecycle message of `host` about `rc`, which another host announced since: stale,
    /// ignored (the other host's own messages run the region in the history).
    fn runs_elsewhere(&self, rc: RegionCoords, host: u32) -> bool {
        match self.region_host.get(&rc) {
            Some(&other) if other != host => {
                log::warn!("coordinator: host {host}'s message about {rc:?} ignored: host {other} runs it");
                true
            }
            _ => false,
        }
    }

    /// Move the calendar to `now_ms`: the history advances, its directives go to the
    /// running regions, a day's turn broadcasts the calendar (and the sites if they
    /// changed), and the notices go to their regions. `WorldManager::maintain`'s world
    /// services, moved here.
    pub fn maintain(&mut self, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        let to = self.calendar_now(now_ms);
        if to > self.history.now() {
            self.dirty_since_save = true;
        }
        self.history.advance(to);
        self.push_directives();
        self.broadcast_daily(to);
        for (region, line) in self.history.take_notices() {
            let to = self.region_host.get(&region).map_or(Target::Every, |h| Target::Host(*h));
            self.push(to, CoordMsg::Notice { region, line });
        }
        self.maintain_cuts(now_ms);
    }

    /// The messages free to leave now, in order, each with the host(s) it is for.
    pub fn take_out(&mut self) -> Vec<(Target, CoordMsg)> {
        std::mem::take(&mut self.out)
    }


    /// Ask for a save at the next [`Self::take_save`] (an orderly exit).
    pub fn request_save(&mut self) {
        self.force_save = true;
    }

    /// Whether [`Self::take_save`] would take one now: gated messages wait, the periodic
    /// [`COORDINATOR_SAVE_SECS`] passed with the state changed, or one was asked for — and
    /// no save is in flight.
    pub fn save_due(&self) -> bool {
        self.save_in_flight.is_none()
            && (!self.gated.is_empty()
                || self.force_save
                || (self.dirty_since_save
                    && self.clock_ms.saturating_sub(self.last_save_ms) >= COORDINATOR_SAVE_SECS * 1000))
    }

    /// The state to save, when one is due ([`Self::save_due`]). Commit it, then call
    /// [`Self::saved`] (or [`Self::save_failed`]). One save at a time: `None` while one is
    /// in flight.
    pub fn take_save(&mut self) -> Option<CoordinatorState> {
        if !self.save_due() {
            return None;
        }
        self.begin_save();
        Some(self.state())
    }

    /// The save taken by [`Self::take_save`] is committed: the messages it made durable
    /// leave, in order.
    pub fn saved(&mut self) {
        let Some(n) = self.save_in_flight.take() else {
            log::error!("coordinator: saved() with no save in flight");
            return;
        };
        self.release(n);
    }

    /// The save taken by [`Self::take_save`] failed: what it held stays waiting, and the
    /// next [`Self::take_save`] tries again. While saves keep failing nothing behind the
    /// first waiting message leaves either — ungated ones included (the order is kept).
    pub fn save_failed(&mut self) {
        if self.save_in_flight.take().is_some() {
            self.dirty_since_save = true;
            self.force_save = true;
        }
    }

    /// For a link with no store behind it (the inline link): the state as it stands
    /// counts as committed, without encoding a save — the same as [`Self::take_save`] +
    /// [`Self::saved`] for a store that cannot fail.
    pub fn commit_unstored(&mut self) {
        debug_assert!(self.save_in_flight.is_none(), "commit_unstored with a stored save in flight");
        if self.save_due() {
            self.begin_save();
            self.saved();
        }
    }

    /// The state as it stands, for a save.
    pub fn state(&self) -> CoordinatorState {
        CoordinatorState {
            history: crate::history::persist::encode(&self.history),
            claims: self.player_claims.iter().map(|(p, c)| (*p, *c)).collect(),
            memo: self.memo_order.iter().map(|id| (*id, self.memo[id].clone())).collect(),
        }
    }

    fn begin_save(&mut self) {
        self.save_in_flight = Some(self.gated.len());
        self.dirty_since_save = false;
        self.force_save = false;
        self.last_save_ms = self.clock_ms;
    }

    /// Move the first `n` gated messages to `out`, then any ungated ones up to the next
    /// gated message (they waited only to keep the order).
    fn release(&mut self, n: usize) {
        for _ in 0..n.min(self.gated.len()) {
            self.out.push(self.gated.pop_front().expect("counted"));
        }
        while self.gated.front().is_some_and(|(_, m)| !m.is_gated()) {
            self.out.push(self.gated.pop_front().expect("checked"));
        }
    }

    /// Queue an outgoing message for `to`: behind any waiting one, or gated itself.
    fn push(&mut self, to: Target, msg: CoordMsg) {
        if !self.gated.is_empty() || msg.is_gated() {
            self.gated.push_back((to, msg));
        } else {
            self.out.push((to, msg));
        }
    }

    fn push_directives(&mut self) {
        for d in self.history.take_directives() {
            self.push_directive(d);
        }
    }

    /// Queue a directive for the host that runs its region, stamped with the region's
    /// latest started incarnation. A region no host has announced cannot take it: it is
    /// treated as bounced (held for the region's next start), never broadcast.
    fn push_directive(&mut self, directive: Directive) {
        let Some(&host) = self.region_host.get(&directive.region) else {
            log::debug!("coordinator: no host runs {:?}; directive deferred: {:?}", directive.region, directive.event);
            self.history.defer_directive(directive);
            return;
        };
        let incarnation = self.incarnations.get(&directive.region).copied().unwrap_or(0);
        self.push(Target::Host(host), CoordMsg::Directive { directive, incarnation });
    }

    /// Forget every directive for `host` not yet handed out (waiting or free to leave):
    /// its `Attach` re-decides every region it runs from its bodies, so they are stale.
    /// Those already handed to the link reach the host ahead of [`CoordMsg::Attached`],
    /// and it drops them. Other hosts' directives stand.
    fn drop_unsent_directives(&mut self, host: u32) {
        self.drop_unsent(host, "directives", |m| matches!(m, CoordMsg::Directive { .. }));
    }

    /// Forget every message for `host` matching `drops` not yet handed out (waiting or
    /// free to leave), superseded by its attach (`what` names them in the log).
    fn drop_unsent(&mut self, host: u32, what: &str, drops: impl Fn(&CoordMsg) -> bool) {
        let dropped_here = |(to, m): &(Target, CoordMsg)| *to == Target::Host(host) && drops(m);
        let before = self.gated.len() + self.out.len();
        if let Some(n) = self.save_in_flight.as_mut() {
            let covered_dropped = self.gated.iter().take(*n).filter(|m| dropped_here(m)).count();
            *n -= covered_dropped;
        }
        self.gated.retain(|m| !dropped_here(m));
        self.out.retain(|m| !dropped_here(m));
        if self.save_in_flight.is_none() {
            // What waited only behind a dropped message may leave.
            self.release(0);
        }
        let dropped = before - self.gated.len() - self.out.len();
        if dropped > 0 {
            log::info!("coordinator: {dropped} unsent {what} for host {host} superseded by its attach");
        }
    }

    fn push_view(&mut self, to: Target) {
        let calendar = self.calendar_now(self.clock_ms);
        self.push(to, CoordMsg::View {
            civs: self.history.civ_markers(),
            sites: self.history.site_markers(),
            calendar,
        });
    }

    /// At a calendar day's turn: the calendar, and the sites if they changed.
    fn broadcast_daily(&mut self, now: CalendarTick) {
        let day = now.0 / TICKS_PER_DAY;
        if self.calendar_day == day {
            return;
        }
        self.calendar_day = day;
        self.push(Target::Every, CoordMsg::Calendar(now));
        let key = sites_key(&self.history);
        if key != self.sites_key {
            self.sites_key = key;
            self.push(Target::Every, CoordMsg::Sites(self.history.site_markers()));
        }
    }

    /// A host (re)attached: it is told so first ([`CoordMsg::Attached`], ahead of every
    /// directive for it decided from here on), its running regions are re-embodied as they
    /// stand (regions the history had live on this host — or on no known host — that it
    /// does not run park empty; another host's stand), its online players are the ones
    /// online on it, and it gets the view.
    fn attach(
        &mut self,
        host: u32,
        epoch: u64,
        running: Vec<RunningRegion>,
        online: Vec<OnlinePlayer>,
        handed: Vec<(PlayerId, u32, u64)>,
    ) {
        log::info!(
            "coordinator: host {host} attached (epoch {epoch}) with {} running regions, {} online, {} handed",
            running.len(),
            online.len(),
            handed.len()
        );
        self.drop_unsent_directives(host);
        // The attach reports everyone afresh: an answer to an earlier report (perhaps the
        // old process's, whose numbers a restarted host reuses) is void.
        self.drop_unsent(host, "claim answers", |m| matches!(m, CoordMsg::ClaimIs { .. } | CoordMsg::HopIs { .. }));
        self.push(Target::Host(host), CoordMsg::Attached { epoch });
        let listed: BTreeSet<RegionCoords> = running.iter().map(|r| r.rc).collect();
        for rc in self.history.live_regions() {
            let runs_here = self.region_host.get(&rc).is_none_or(|h| *h == host);
            if runs_here && !listed.contains(&rc) {
                log::warn!("coordinator: {rc:?} was live but host {host} does not run it; parking it empty");
                self.history.region_parked(rc, Vec::new());
            }
        }
        self.region_host.retain(|_, h| *h != host);
        for r in &running {
            if let Some(other) = self.region_host.insert(r.rc, host) {
                log::warn!("coordinator: host {host} attached running {:?}, which host {other} had announced", r.rc);
            }
            self.incarnations.insert(r.rc, r.incarnation);
        }
        let regions: Vec<(RegionCoords, Vec<(crate::history::PersonId, [f32; 3])>)> =
            running.into_iter().map(|r| (r.rc, r.people)).collect();
        for d in self.history.reattach_regions(&regions) {
            self.push_directive(d);
        }
        // The host's players: its lists say who is online on it and who it handed away
        // (each applied by its hop, as the message it stands for); any other player the
        // coordinator still has online on it is not there any more.
        let listed: BTreeSet<PlayerId> = online.iter().map(|p| p.player).collect();
        for p in online {
            self.player_online(host, p);
        }
        for (player, to, hop) in handed {
            self.player_handed(host, player, to, hop);
        }
        let gone: Vec<PlayerId> = self
            .online_on
            .iter()
            .filter(|(p, h)| **h == host && !listed.contains(p))
            .map(|(p, _)| *p)
            .collect();
        for p in gone {
            self.online_on.remove(&p);
            self.online_reported.remove(&p);
            self.displaced_told.retain(|(q, _, _)| *q != p);
            self.set_online(p, false);
        }
        self.push_view(Target::Host(host));
    }

    /// `player` is online on `host`, as that host reports it. With a claim-changing
    /// request of the player unsettled on the host (`claim_pending`) the coordinator's
    /// claim stands (that request's answer, or the host's `ReleaseClaim` after it, settles
    /// it); without one the host's claim is the truth: the coordinator's own may be a save
    /// behind (a crash lost a grant) or ahead (a crash lost a release). A claim the host
    /// reports and the coordinator lacks is taken unless another *online* player holds its
    /// fortress (an offline claimant loses it, as to a return) — one fortress never has two
    /// players. Whenever the claim the coordinator ends with differs from the report, the
    /// host is told it ([`CoordMsg::ClaimIs`]): the coordinator's answer is final.
    ///
    /// A report whose hop is lower than the player's latest is stale and ignored; the
    /// host is told the latest ([`CoordMsg::HopIs`]) in case the report is of its current
    /// placement (a login that could not know the hops elsewhere).
    fn player_online(&mut self, host: u32, report: OnlinePlayer) {
        let OnlinePlayer { player, claim, claim_pending, report, hop } = report;
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        // Two hosts are never online at one hop: at the known hop only the host the
        // player is online on (a re-report, or the grant's host) counts.
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} online on host {host} at hop {hop} ignored (hop {known}, online on {on:?})");
            self.push(Target::Host(host), CoordMsg::HopIs { player, reported: hop, hop: known });
            return;
        }
        self.displace(player, host, hop);
        self.hop_of.insert(player, hop);
        self.online_on.insert(player, host);
        self.online_reported.insert(player, (host, hop));
        let mine = self.player_claims.get(&player).copied();
        match (mine, claim) {
            (Some(mine), Some(theirs)) if mine == theirs => {}
            (Some(mine), theirs) if claim_pending => {
                log::debug!("coordinator: {player:?} comes online with {theirs:?} and a request unsettled; it holds {mine:?}");
            }
            (Some(mine), theirs) => {
                log::warn!("coordinator: {player:?} comes online with {theirs:?}; its {mine:?} here is dropped");
                self.release_claim(player);
                if let Some(theirs) = theirs {
                    self.adopt_claim(player, theirs);
                }
            }
            (None, Some(theirs)) => self.adopt_claim(player, theirs),
            (None, None) => {}
        }
        self.set_online(player, true);
        let now = self.player_claims.get(&player).copied();
        if now != claim {
            log::info!("coordinator: host {host} reported {player:?} with {claim:?}; the claim is {now:?}");
            self.push(Target::Host(host), CoordMsg::ClaimIs { player, claim: now, report });
        }
    }

    /// `player`'s placement `hop` on `host` ended. Stale below the latest hop; at it, only
    /// the host the player is online on ends it (a transit to another host stands); above
    /// it, it is the end of a placement whose start has not arrived yet (it travels on
    /// another link, or was lost): offline at that hop, and every message of that hop or
    /// lower is stale from here on.
    fn player_offline(&mut self, host: u32, player: PlayerId, hop: u64) {
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} offline on host {host} at hop {hop} ignored (hop {known}, online on {on:?})");
            return;
        }
        if on == Some(host) {
            self.displaced_told.retain(|(q, _, _)| *q != player);
        } else {
            self.displace(player, host, hop);
        }
        self.hop_of.insert(player, hop);
        self.online_on.remove(&player);
        self.online_reported.remove(&player);
        self.set_online(player, false);
    }

    /// A message of `by` at `hop` (above the known one) moves `player` away from the host
    /// it was online on by that host's own report: that host is told
    /// ([`CoordMsg::HopIs`]), once per reported hop. In a real hand-over it no longer holds
    /// the player and ignores it; if it does hold it (its placement lost the race against
    /// an older placement's messages in flight, or a duplicate) it renumbers above `hop`
    /// and reports again — the newest reporter wins, as the newest session does.
    fn displace(&mut self, player: PlayerId, by: u32, hop: u64) {
        let Some(&(host, reported)) = self.online_reported.get(&player) else { return };
        if host == by || self.online_on.get(&player) != Some(&host) {
            return;
        }
        if self.displaced_told.insert((player, host, reported)) {
            log::debug!("coordinator: {player:?} moved off host {host} (hop {reported}) by host {by} at hop {hop}: told");
            self.push(Target::Host(host), CoordMsg::HopIs { player, reported, hop });
        }
    }

    /// `host` handed `player`, placed there at `hop`, to host `to`: in transit — online on
    /// `to` (its claim in use) until `to` reports it (at `hop + 1`). Stale below the latest
    /// hop, and at it from a host the player is not online on; above it, the placement on
    /// `host` whose start has not arrived yet, then the hand-over.
    fn player_handed(&mut self, host: u32, player: PlayerId, to: u32, hop: u64) {
        let known = self.hop_of(player);
        let on = self.online_on.get(&player).copied();
        if hop < known || (hop == known && on != Some(host)) {
            log::debug!("coordinator: {player:?} handed by host {host} to {to} at hop {hop} ignored (hop {known}, online on {on:?})");
            return;
        }
        if on != Some(host) {
            self.displace(player, host, hop);
        }
        self.hop_of.insert(player, hop);
        self.online_on.insert(player, to);
        self.online_reported.remove(&player);
        self.set_online(player, true);
    }

    /// Take the claim the host reports for `player` (who holds none here).
    fn adopt_claim(&mut self, player: PlayerId, claim: Claim) {
        match claim {
            Claim::Fortress(site) => match self.claims.get(&site).copied() {
                Some((other, true)) if other != player => {
                    log::info!("coordinator: {player:?}'s fortress {site:?} is {other:?}'s now; the host drops it");
                }
                claimed => {
                    if let Some((other, _)) = claimed.filter(|(o, _)| *o != player) {
                        log::info!("coordinator: {player:?} comes back to {site:?}; offline {other:?} loses it");
                        self.player_claims.remove(&other);
                    }
                    self.claims.insert(site, (player, true));
                    self.player_claims.insert(player, Claim::Fortress(site));
                }
            },
            Claim::Adventure(_) => {
                self.player_claims.insert(player, claim);
            }
        }
    }

    fn set_online(&mut self, player: PlayerId, online: bool) {
        if let Some(Claim::Fortress(site)) = self.player_claims.get(&player) {
            if let Some((p, on)) = self.claims.get_mut(site) {
                if *p == player {
                    *on = online;
                }
            }
        }
    }

    /// `player` gives up what it plays. Saved at once ([`Self::request_save`]): a release
    /// lost to a crash would leave a claim the host no longer knows of.
    fn release_claim(&mut self, player: PlayerId) {
        let Some(claim) = self.player_claims.remove(&player) else { return };
        if let Claim::Fortress(site) = claim {
            if self.claims.get(&site).is_some_and(|(p, _)| *p == player) {
                self.claims.remove(&site);
            }
        }
        self.request_save();
    }

    /// Settle a timed-out state-changing request ([`HostMsg::Settle`]): its memoised
    /// answer if it was applied, else a `WorldUnavailable` refusal, memoised without
    /// executing it (a later copy of the request gets that refusal too).
    fn settle(&mut self, host: u32, id: RequestId, req: WorldRequest) {
        let reply = match self.memo.get(&id) {
            Some(reply) => reply.clone(),
            None => {
                let refusal = match req {
                    WorldRequest::Embark(_) | WorldRequest::ReturnTo(_) => {
                        WorldReply::EmbarkRefused(EmbarkRefusal::WorldUnavailable)
                    }
                    WorldRequest::StartAdventure(_) => WorldReply::AdventureRefused(AdventureRefusal::WorldUnavailable),
                    other => {
                        log::warn!("coordinator: settle of {id:?} for a query {other:?}: nothing to settle");
                        return;
                    }
                };
                log::info!("coordinator: {id:?} settled unapplied: refused");
                self.remember(id, refusal.clone());
                refusal
            }
        };
        self.push(Target::Host(host), CoordMsg::Reply { id, reply });
    }

    fn request(&mut self, host: u32, id: RequestId, player: PlayerId, req: WorldRequest) {
        if let Some(reply) = self.memo.get(&id) {
            log::debug!("coordinator: {id:?} repeated; answering from the memo");
            let reply = reply.clone();
            return self.push(Target::Host(host), CoordMsg::Reply { id, reply });
        }
        let memoise = req.changes_state();
        let reply = match req {
            WorldRequest::EmbarkOptions => WorldReply::EmbarkOptions(self.history.embark_options()),
            WorldRequest::Embark(r) => self.embark(player, &r),
            WorldRequest::ReturnTo(site) => self.return_to(player, site),
            WorldRequest::StartAdventure(site) => self.start_adventure(player, site),
            WorldRequest::Legends(q) => WorldReply::Legends(self.history.legends(&q)),
            WorldRequest::PersonInfo(p) => WorldReply::PersonInfo(self.history.person_info(p)),
        };
        if memoise {
            self.remember(id, reply.clone());
        }
        let granted = reply.grants_claim();
        if granted {
            // The player plays it on the host that asked. If another host had it online by
            // its own report, that host is told (`displace`): a grant to an older placement
            // still in flight must not silently take the player off the one holding it.
            let known = self.hop_of(player);
            self.displace(player, host, known);
            self.online_on.insert(player, host);
            if self.online_reported.get(&player).is_some_and(|(h, _)| *h != host) {
                self.online_reported.remove(&player);
            }
        }
        self.push(Target::Host(host), CoordMsg::Reply { id, reply });
        if granted {
            // Every host's view of the sites follows at once (a founded site, its claim:
            // the player may play it on another host); clients still get them at the
            // day's turn.
            self.push(Target::Every, CoordMsg::SiteMarkers(self.history.site_markers()));
        }
    }

    /// Memoise `reply` for `id` until its host acknowledges it ([`Self::forget_settled`]);
    /// never evicted. Past [`ANSWERED_MEMO_MAX`] an error names the host holding most.
    fn remember(&mut self, id: RequestId, reply: WorldReply) {
        if self.memo.insert(id, reply).is_none() {
            self.memo_order.push_back(id);
        }
        let quiet = self.memo_warned_ms.is_some_and(|ms| self.clock_ms.saturating_sub(ms) < MEMO_WARN_SECS * 1000);
        if self.memo.len() > ANSWERED_MEMO_MAX && !quiet {
            self.memo_warned_ms = Some(self.clock_ms);
            let mut per_host: BTreeMap<u32, usize> = BTreeMap::new();
            for id in self.memo.keys() {
                *per_host.entry(id.host).or_default() += 1;
            }
            let (host, n) = per_host.into_iter().max_by_key(|(_, n)| *n).expect("not empty");
            log::error!(
                "coordinator: {} unacknowledged answers remembered (over {ANSWERED_MEMO_MAX}); host {host} holds {n}",
                self.memo.len()
            );
        }
    }

    /// Host `host` acknowledged its answers of `boot` below `below` ([`HostMsg::Settled`],
    /// an `Attach`'s watermark): forgotten. At an `Attach` (`attach`) naming a boot not
    /// seen from the host before, the host's answers of every other boot go too (that
    /// process is gone).
    fn forget_settled(&mut self, host: u32, boot: u64, below: u64, attach: bool) {
        let new_boot = attach && self.boots_seen.entry(host).or_default().insert(boot);
        let forget = |id: &RequestId| id.host == host && ((id.boot == boot && id.seq < below) || (new_boot && id.boot != boot));
        let before = self.memo.len();
        self.memo.retain(|id, _| !forget(id));
        if self.memo.len() != before {
            self.memo_order.retain(|id| !forget(id));
            log::debug!("coordinator: {} answers of host {host} acknowledged", before - self.memo.len());
        }
    }

    /// The player's embark (`WorldManager::embark`'s checks): one claim per player; the
    /// history validates and founds the site at once.
    fn embark(&mut self, player: PlayerId, req: &EmbarkRequest) -> WorldReply {
        if self.player_claims.contains_key(&player) {
            return WorldReply::EmbarkRefused(EmbarkRefusal::AlreadyHasFortress);
        }
        match self.history.player_embark(req) {
            Ok(site) => {
                self.claims.insert(site, (player, true));
                self.player_claims.insert(player, Claim::Fortress(site));
                WorldReply::Embarked { site, region: req.region }
            }
            Err(e) => WorldReply::EmbarkRefused(e),
        }
    }

    /// Return to a retired fortress (`WorldManager::return_to`'s checks): only to a
    /// player-founded site no other *online* player plays; an offline claimant loses it.
    fn return_to(&mut self, player: PlayerId, site: SiteId) -> WorldReply {
        if self.player_claims.contains_key(&player) {
            return WorldReply::EmbarkRefused(EmbarkRefusal::AlreadyHasFortress);
        }
        if !self.history.returnable(site) {
            return WorldReply::EmbarkRefused(EmbarkRefusal::NotPlayerFounded);
        }
        match self.claims.get(&site).copied() {
            Some((other, true)) if other != player => return WorldReply::EmbarkRefused(EmbarkRefusal::InUse),
            Some((other, false)) if other != player => {
                log::info!("coordinator: {player:?} returns to {site:?}; offline {other:?} loses it");
                self.player_claims.remove(&other);
            }
            _ => {}
        }
        let region = self.history.site(site).expect("returnable").region;
        self.claims.insert(site, (player, true));
        self.player_claims.insert(player, Claim::Fortress(site));
        WorldReply::Returned { site, region }
    }

    /// Start an adventure at a populated site (`WorldManager::start_adventure`'s checks).
    fn start_adventure(&mut self, player: PlayerId, site: SiteId) -> WorldReply {
        let refuse = WorldReply::AdventureRefused;
        let Some(s) = self.history.site(site) else {
            return refuse(AdventureRefusal::UnknownSite);
        };
        if s.abandoned.is_some() {
            return refuse(AdventureRefusal::SiteAbandoned);
        }
        if !s.members.iter().any(|&p| self.history.person(p).is_some_and(|p| p.died.is_none())) {
            return refuse(AdventureRefusal::Unpopulated);
        }
        if self.player_claims.contains_key(&player) {
            return refuse(AdventureRefusal::AlreadyPlaying);
        }
        let region = s.region;
        self.player_claims.insert(player, Claim::Adventure(site));
        WorldReply::AdventureStarted { site, region }
    }
}
