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

mod link;
mod msg;
mod state;

pub use link::*;
pub use msg::*;
pub use state::*;

use std::collections::{BTreeMap, VecDeque};

use crate::history::persist::HistoryLoadError;
use crate::history::{
    AdventureRefusal, CalendarTick, Directive, EmbarkRefusal, History, SiteId, SocietyGrid, TICKS_PER_DAY,
};
use crate::{EmbarkRequest, PlayerId, RegionCoords};

/// Most answers of state-changing requests remembered for replay; past it the oldest is
/// forgotten (a host replays a request within `WORLD_REQUEST_TIMEOUT_SECS`, so 4096
/// answers is far more than one downtime can produce).
pub const ANSWERED_MEMO_MAX: usize = 4096;
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
    /// `memo`'s ids, oldest first (eviction order, and the saved order).
    memo_order: VecDeque<RequestId>,
    /// Waiting for a save: the first gated message and everything after it, in order.
    gated: VecDeque<CoordMsg>,
    /// Free to leave, in order.
    out: Vec<CoordMsg>,
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
            gated: VecDeque::new(),
            out: Vec::new(),
            save_in_flight: None,
            dirty_since_save: false,
            force_save: false,
            last_save_ms: now_ms,
            clock_ms: now_ms,
            calendar_anchor,
        };
        c.push_view();
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

    /// The calendar at `now_ms`: the anchor's tick plus one per `TICK_RATE` ms since.
    pub fn calendar_now(&self, now_ms: u64) -> CalendarTick {
        let (ms, tick) = self.calendar_anchor;
        CalendarTick(tick.0 + now_ms.saturating_sub(ms) / crate::TICK_RATE)
    }

    /// Apply one host message.
    pub fn handle(&mut self, msg: HostMsg, now_ms: u64) {
        self.clock_ms = self.clock_ms.max(now_ms);
        self.dirty_since_save = true;
        match msg {
            HostMsg::Attach { host, epoch, running, online } => self.attach(host, epoch, running, online),
            HostMsg::RegionStarted { rc, incarnation } => {
                self.incarnations.insert(rc, incarnation);
                let events = self.history.region_started(rc);
                self.push(CoordMsg::SpawnPeople { rc, incarnation, events });
            }
            HostMsg::RegionStopping { rc } => self.history.region_stopping(rc),
            HostMsg::RegionParked { rc, people } => self.history.region_parked(rc, people),
            HostMsg::PersonCrossed { person, to, delivered } => {
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
            HostMsg::Request { id, player, req } => self.request(id, player, req),
            HostMsg::Settle { id, req } => self.settle(id, req),
            HostMsg::PlayerOnline(p) => self.player_online(p),
            HostMsg::PlayerOffline { player } => self.set_online(player, false),
            HostMsg::ReleaseClaim { player } => self.release_claim(player),
        }
        self.push_directives();
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
            self.push(CoordMsg::Notice { region, line });
        }
    }

    /// The messages free to leave now, in order.
    pub fn take_out(&mut self) -> Vec<CoordMsg> {
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
        while self.gated.front().is_some_and(|m| !m.is_gated()) {
            self.out.push(self.gated.pop_front().expect("checked"));
        }
    }

    /// Queue an outgoing message: behind any waiting one, or gated itself.
    fn push(&mut self, msg: CoordMsg) {
        if !self.gated.is_empty() || msg.is_gated() {
            self.gated.push_back(msg);
        } else {
            self.out.push(msg);
        }
    }

    fn push_directives(&mut self) {
        for d in self.history.take_directives() {
            self.push_directive(d);
        }
    }

    /// Queue a directive stamped with its region's latest started incarnation.
    fn push_directive(&mut self, directive: Directive) {
        let incarnation = self.incarnations.get(&directive.region).copied().unwrap_or(0);
        self.push(CoordMsg::Directive { directive, incarnation });
    }

    /// Forget every directive not yet handed out (waiting or free to leave): an `Attach`
    /// re-decides every region from its bodies, so they are stale. Those already handed
    /// to the link reach the host ahead of [`CoordMsg::Attached`], and it drops them.
    fn drop_unsent_directives(&mut self) {
        let is_directive = |m: &CoordMsg| matches!(m, CoordMsg::Directive { .. });
        let before = self.gated.len() + self.out.len();
        if let Some(n) = self.save_in_flight.as_mut() {
            let covered_dropped = self.gated.iter().take(*n).filter(|m| is_directive(m)).count();
            *n -= covered_dropped;
        }
        self.gated.retain(|m| !is_directive(m));
        self.out.retain(|m| !is_directive(m));
        if self.save_in_flight.is_none() {
            // What waited only behind a dropped directive may leave.
            self.release(0);
        }
        let dropped = before - self.gated.len() - self.out.len();
        if dropped > 0 {
            log::info!("coordinator: {dropped} unsent directives superseded by an attach");
        }
    }

    fn push_view(&mut self) {
        let calendar = self.calendar_now(self.clock_ms);
        self.push(CoordMsg::View {
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
        self.push(CoordMsg::Calendar(now));
        let key = sites_key(&self.history);
        if key != self.sites_key {
            self.sites_key = key;
            self.push(CoordMsg::Sites(self.history.site_markers()));
        }
    }

    /// A host (re)attached: it is told so first ([`CoordMsg::Attached`], ahead of every
    /// directive decided from here on), its running regions are re-embodied as they stand
    /// (regions the history had live but the host does not run park empty), its online
    /// players are the online ones, and it gets the view.
    fn attach(
        &mut self,
        host: u32,
        epoch: u64,
        running: Vec<RunningRegion>,
        online: Vec<OnlinePlayer>,
    ) {
        log::info!(
            "coordinator: host {host} attached (epoch {epoch}) with {} running regions, {} online",
            running.len(),
            online.len()
        );
        self.drop_unsent_directives();
        self.push(CoordMsg::Attached { epoch });
        let listed: std::collections::BTreeSet<RegionCoords> = running.iter().map(|r| r.rc).collect();
        for rc in self.history.live_regions() {
            if !listed.contains(&rc) {
                log::warn!("coordinator: {rc:?} was live but host {host} does not run it; parking it empty");
                self.history.region_parked(rc, Vec::new());
            }
        }
        for r in &running {
            self.incarnations.insert(r.rc, r.incarnation);
        }
        let regions: Vec<(RegionCoords, Vec<(crate::history::PersonId, [f32; 3])>)> =
            running.into_iter().map(|r| (r.rc, r.people)).collect();
        for d in self.history.reattach_regions(&regions) {
            self.push_directive(d);
        }
        // TODO(R3): one host per world. With several hosts an `Attach` must mark offline
        // only that host's players, not every claimant.
        for (_, on) in self.claims.values_mut() {
            *on = false;
        }
        for p in online {
            self.player_online(p);
        }
        self.push_view();
    }

    /// `player` is online, as its host reports it. With a claim-changing request of the
    /// player unsettled on the host (`claim_pending`) the coordinator's claim stands (that
    /// request's answer, or the host's `ReleaseClaim` after it, settles it); without one
    /// the host's claim is the truth: the coordinator's own may be a save behind (a crash
    /// lost a grant) or ahead (a crash lost a release). A claim the host reports and the
    /// coordinator lacks is taken unless another *online* player holds its fortress (an
    /// offline claimant loses it, as to a return); then the host is told to drop it
    /// ([`CoordMsg::FortressRefused`]) — one fortress never has two players.
    fn player_online(&mut self, report: OnlinePlayer) {
        let OnlinePlayer { player, claim, claim_pending } = report;
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
    }

    /// Take the claim the host reports for `player` (who holds none here).
    fn adopt_claim(&mut self, player: PlayerId, claim: Claim) {
        match claim {
            Claim::Fortress(site) => match self.claims.get(&site).copied() {
                Some((other, true)) if other != player => {
                    log::info!("coordinator: {player:?}'s fortress {site:?} is {other:?}'s now; the host drops it");
                    self.push(CoordMsg::FortressRefused { player, site });
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
    fn settle(&mut self, id: RequestId, req: WorldRequest) {
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
        self.push(CoordMsg::Reply { id, reply });
    }

    fn request(&mut self, id: RequestId, player: PlayerId, req: WorldRequest) {
        if let Some(reply) = self.memo.get(&id) {
            log::debug!("coordinator: {id:?} repeated; answering from the memo");
            let reply = reply.clone();
            return self.push(CoordMsg::Reply { id, reply });
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
        self.push(CoordMsg::Reply { id, reply });
        if granted {
            // The host's view of the sites follows at once (a founded site, its claim);
            // clients still get them at the day's turn.
            self.push(CoordMsg::SiteMarkers(self.history.site_markers()));
        }
    }

    fn remember(&mut self, id: RequestId, reply: WorldReply) {
        if self.memo.insert(id, reply).is_none() {
            self.memo_order.push_back(id);
        }
        while self.memo_order.len() > ANSWERED_MEMO_MAX {
            let old = self.memo_order.pop_front().expect("over the cap");
            self.memo.remove(&old);
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
