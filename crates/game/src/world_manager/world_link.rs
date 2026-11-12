//! The world-link half of [`WorldManager`]: the host's side of the conversation with
//! the world coordinator (attach, lifecycle messages, claims, embark / return /
//! adventure requests). Split out of `mod.rs`; the core calls in through the
//! `pub(super)` methods here.

use super::*;

/// Seconds a player's world request (embark, return, adventure, legends, person info,
/// embark options) waits for the coordinator's answer, queued while the link is down
/// included; past it an embark / return / adventure start is answered `WorldUnavailable`
/// and the request forgotten (a query just goes unanswered).
pub const WORLD_REQUEST_TIMEOUT_SECS: u64 = 120;

/// Most ms the host waits for its running regions to list their people
/// (`RegionInput::ListPeople`) before it sends its `Attach` to a (re)connected world
/// coordinator with what it has.
pub const ATTACH_WAIT_MS: u64 = 2000;

/// A player's world request sent (or queued) to the coordinator and not answered yet.
#[derive(Debug, Clone)]
pub(super) struct PendingRequest {
    client: ClientId,
    player: PlayerId,
    req: WorldRequest,
    /// When the client asked: the timeout runs from here.
    sent_ms: u64,
}

/// People bodies a region listed for an `Attach` (`None`: not answered yet).
type Listed = Option<Vec<(PersonId, [f32; 3])>>;

/// Where a person's body went over a seam while an `Attach` was gathered (the latest
/// move wins), so the lists can be made exact across seams ([`WorldManager::try_finish_attach`]).
#[derive(Debug, Clone, Copy)]
pub(super) enum Moved {
    /// Delivered into `to` (in flight there, or arrived): the body is `to`'s.
    Into { from: RegionCoords, to: RegionCoords, at: [f32; 3] },
    /// Absorbed at the seam out of `from` (its target was not running): no body is left;
    /// the `PersonCrossed { delivered: false }` held behind the `Attach` says so.
    Absorbed { from: RegionCoords, at: [f32; 3] },
}

/// Make the gathered `Attach` lists exact across seams. A region's outputs are FIFO, so
/// a body that left a region before it answered `ListPeople` is not in that region's
/// list (its departure was handled first), and one that lands in its target after the
/// target answered is not in the target's: a person crossing while the attach is
/// gathered can be in no list — the history would take them back and the attach's
/// catch-up spawn a second body beside the one landing — or, having left after its
/// source listed it, in two. Each person in `moved` ends up in exactly one list:
/// - delivered into a listed region: that region's (the body is, or lands, there; the
///   held `PersonCrossed` then agrees);
/// - delivered into a region not listed (it started during the gather: its
///   `RegionStarted` is held too), or absorbed at the seam: the list of the region it
///   first left during the gather, so the held crossings replay the move from there
///   (an absorbed one is taken back by its own `PersonCrossed`, not by the attach —
///   the attach would otherwise re-spawn a person whose crossing then absorbs them).
///
/// A listed region lost during the gather (its thread died) is listed as its loss left
/// it: empty. A person delivered into it still goes into that empty list — their body
/// went down with the region — and its `RegionParked { people: [] }`, held behind the
/// `Attach`, then gives them back to the history like everyone else it had there.
fn exact_across_seams(
    lists: &mut BTreeMap<RegionCoords, (u64, Vec<(PersonId, [f32; 3])>)>,
    moved: &BTreeMap<PersonId, Moved>,
) {
    for (p, m) in moved {
        let (home, at) = match *m {
            Moved::Into { to, at, .. } if lists.contains_key(&to) => (to, at),
            Moved::Into { from, at, .. } | Moved::Absorbed { from, at } => (from, at),
        };
        if !lists.contains_key(&home) {
            continue;
        }
        let mut kept = None;
        for (rc, (_, people)) in lists.iter_mut() {
            if let Some(i) = people.iter().position(|(q, _)| q == p) {
                let (_, pos) = people.remove(i);
                if *rc == home {
                    kept = Some(pos);
                }
            }
        }
        lists.get_mut(&home).expect("checked").1.push((*p, kept.unwrap_or(at)));
    }
}

/// One seam step of a person's body during a gather, seen in region `here` (the region
/// it left, or the one it arrived in).
#[derive(Debug, Clone, Copy)]
pub(super) enum Step {
    Into { to: RegionCoords, at: [f32; 3] },
    Absorbed { at: [f32; 3] },
}

/// Record `step` of `person` seen in `here`: the latest step wins, but `from` stays the
/// region the person was first seen in during this gather — the one whose list their
/// held crossings replay from (a multi-hop through regions not listed must still fall
/// back to a listed one).
fn record_move(moved: &mut BTreeMap<PersonId, Moved>, person: PersonId, here: RegionCoords, step: Step) {
    let from = moved
        .get(&person)
        .map_or(here, |m| match *m {
            Moved::Into { from, .. } | Moved::Absorbed { from, .. } => from,
        });
    let m = match step {
        Step::Into { to, at } => Moved::Into { from, to, at },
        Step::Absorbed { at } => Moved::Absorbed { from, at },
    };
    moved.insert(person, m);
}

/// Where the host stands with the world coordinator.
#[derive(Debug)]
pub(super) enum WorldAttach {
    /// The link is down: lifecycle messages are dropped (the next `Attach` supersedes
    /// them), requests and claim messages wait in the outbox.
    Down,
    /// The link is up and the `Attach` is being gathered: each listed region (with its
    /// incarnation) is asked for its people. Lifecycle messages made meanwhile are
    /// `held` and follow the `Attach`; requests and claim messages go to the outbox.
    /// A running region enters the `Attach` only with its exact list for this epoch
    /// (`RegionOutput::PeopleListed`) or as its park / loss left it — never guessed — and
    /// the `Attach` waits until no region is stopping (a stopping region's bodies are in
    /// no list): past [`ATTACH_WAIT_MS`] the stragglers are logged (`warned`) and the host
    /// keeps waiting. A region started meanwhile is not listed (its `RegionStarted` is
    /// held and follows the `Attach`).
    Gathering {
        epoch: u64,
        since_ms: u64,
        regions: BTreeMap<RegionCoords, (u64, Listed)>,
        held: Vec<HostMsg>,
        warned: bool,
        /// Person bodies that crossed a seam meanwhile ([`Moved`]), by person.
        moved: BTreeMap<PersonId, Moved>,
    },
    /// The `Attach` of `epoch` is sent; until the coordinator echoes it
    /// ([`CoordMsg::Attached`]) every `SpawnPeople` / `Directive` that arrives was decided
    /// before it and is dropped.
    Sent { epoch: u64 },
    /// Attached: everything flows.
    Attached,
}

/// What the host did with coordinator messages it could not apply (tests, the
/// `[server]` counters).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldLinkCounters {
    /// Directives answered with `DirectiveBounced` (their region's incarnation stopped,
    /// or its people had not landed yet).
    pub directives_bounced: u64,
    /// `SpawnPeople` for an incarnation that had stopped.
    pub spawns_dropped: u64,
    /// `SpawnPeople` / `Directive`s that arrived between the link going down and the
    /// coordinator's `Attached`: decided before the attach, superseded by it.
    pub stale_dropped: u64,
}

/// What the world coordinator last told the host of the world
/// ([`CoordMsg::View`], `Calendar`, `Sites`).
#[derive(Debug, Clone)]
pub(super) struct WorldViewCache {
    civs: Vec<crate::CivMarker>,
    sites: BTreeMap<SiteId, crate::SiteMarker>,
    /// `(now_ms, calendar tick)`: the calendar as last told, and when.
    calendar: (u64, CalendarTick),
}

impl<S: RegionSpawner> WorldManager<S> {
    /// Link the world coordinator (its history and world services); `boot` is random per
    /// host process start (request ids), `now_ms` is when. The host attaches at once if
    /// the link is up ([`Self::world_link_up`]), and every region already running is
    /// announced to it (its people spawned). Installed once: a second link would orphan
    /// the bodies the first one placed.
    pub fn set_world_link(&mut self, link: Box<dyn WorldLink + Send>, boot: u64, now_ms: u64) {
        debug_assert!(self.world.is_none(), "a world link is already installed");
        let up = link.is_up();
        self.world_generation = link.generation();
        self.world = Some(link);
        self.world_boot = boot;
        self.world_ms = self.world_ms.max(now_ms);
        self.world_attach = WorldAttach::Down;
        if up {
            self.world_link_up(now_ms);
        }
        let running: Vec<RegionCoords> =
            self.regions.iter().filter(|(_, l)| !l.stopping).map(|(rc, _)| *rc).collect();
        for rc in running {
            self.world_region_started(rc);
        }
        self.flush_world();
    }

    /// The calendar at `now_ms`: as the coordinator last told it, plus one tick per
    /// `TICK_RATE` ms since. `None` before it has told any (or without a world link).
    ///
    /// The tick is taken as of its arrival, so the host's calendar trails the
    /// coordinator's by the link's latency (well under a tick on one machine) until the
    /// next `Calendar`; the two processes share no clock to correct it by, and nothing
    /// in the simulation reads it (clients show it).
    pub fn calendar_now(&self, now_ms: u64) -> Option<CalendarTick> {
        let (ms, tick) = self.world_view.as_ref()?.calendar;
        Some(CalendarTick(tick.0 + now_ms.saturating_sub(ms) / crate::TICK_RATE))
    }

    /// The world link (tests and tools reach an inline coordinator through
    /// [`WorldLink::as_inline`]).
    pub fn world_link(&self) -> Option<&(dyn WorldLink + Send)> {
        self.world.as_deref()
    }

    /// The world link, to drive or reach into (`WorldLink::as_inline_mut` →
    /// `InlineWorldLink::coordinator_mut`). Never call the history's region lifecycle
    /// methods through it: the manager's messages own them.
    pub fn world_link_mut(&mut self) -> Option<&mut (dyn WorldLink + Send + 'static)> {
        self.world.as_deref_mut()
    }

    /// The world generation this host process runs (the store's, at its registration):
    /// sent in every `Attach` (a coordinator refuses a lower one than the store's).
    pub fn set_host_generation(&mut self, generation: u64) {
        self.host_generation = generation;
    }

    /// What the host did with coordinator messages it could not apply.
    pub fn world_link_counters(&self) -> WorldLinkCounters {
        self.world_counters
    }

    /// Player world requests not answered yet (sent or queued).
    pub fn pending_world_requests(&self) -> usize {
        self.pending_requests.len()
    }

    /// Requests and claim messages waiting for the link ([`Self::world_link_up`]).
    pub fn world_outbox_len(&self) -> usize {
        self.outbox.len()
    }

    /// Whether the coordinator has applied the host's latest `Attach`.
    pub fn world_attached(&self) -> bool {
        matches!(self.world_attach, WorldAttach::Attached)
    }

    /// Test hook: [`WORLD_REQUEST_TIMEOUT_SECS`], in ms, for this manager.
    pub fn set_world_request_timeout_ms(&mut self, ms: u64) {
        self.world_request_timeout_ms = ms;
    }

    /// The site markers clients are told: the coordinator's last view / broadcast.
    pub(super) fn world_sites(&self) -> Vec<crate::SiteMarker> {
        self.world_view.as_ref().map(|v| v.sites.values().cloned().collect()).unwrap_or_default()
    }

    /// The civ markers clients are told ([`Self::world_sites`]' rule).
    pub(super) fn world_civs(&self) -> Vec<crate::CivMarker> {
        self.world_view.as_ref().map(|v| v.civs.clone()).unwrap_or_default()
    }

    /// The host's number in request ids and its `Attach`: its ordinal in a static
    /// ownership, else 0.
    fn world_host(&self) -> u32 {
        match self.ownership.0 {
            crate::Ownership::Static { ordinal, .. } => ordinal,
            crate::Ownership::AllInOne => 0,
        }
    }

    /// What `client` plays, as a claim.
    fn claim_of_client(&self, client: ClientId) -> Option<Claim> {
        self.fortress_of
            .get(&client)
            .map(|&s| Claim::Fortress(s))
            .or_else(|| self.adventurer_of.get(&client).map(|&s| Claim::Adventure(s)))
    }

    /// The link came up (at start, or after it was down): the host attaches. Every region
    /// announced and running (not stopping) is asked for its people
    /// (`RegionInput::ListPeople`); once all have answered exactly (however long it
    /// takes; [`ATTACH_WAIT_MS`] only logs the stragglers) the `Attach` goes, then the
    /// requests whose answer never came, then the outbox, then
    /// the lifecycle messages made meanwhile. A stopping region is not listed: the
    /// coordinator parks it empty (its people back in the abstract history; a park never
    /// keeps bodies) and its own park sends nothing.
    pub fn world_link_up(&mut self, now_ms: u64) {
        if self.world.is_none() {
            return;
        }
        self.attach_epoch += 1;
        let epoch = self.attach_epoch;
        let listed: BTreeSet<RegionCoords> = self
            .world_live
            .iter()
            .copied()
            .filter(|rc| self.regions.get(rc).is_some_and(|l| !l.stopping))
            .collect();
        for rc in self.world_live.difference(&listed) {
            log::info!("world link up: {rc:?} is stopping; not attached (its people go back to the history)");
            self.spawn_waiting.remove(rc);
        }
        self.world_live = listed.clone();
        log::info!("world link up: gathering attach {epoch} over {} regions", listed.len());
        let regions =
            listed.iter().map(|rc| (*rc, (self.incarnation.get(rc).copied().unwrap_or(0), None))).collect();
        self.world_attach =
            WorldAttach::Gathering {
                epoch,
                since_ms: now_ms,
                regions,
                held: Vec::new(),
                warned: false,
                moved: BTreeMap::new(),
            };
        for rc in listed {
            self.send_to_region(rc, RegionInput::ListPeople(epoch));
        }
        self.try_finish_attach();
    }

    /// A listed region's exact people for the attach being gathered: its
    /// `PeopleListed` of that epoch, or what its park / loss left (the park's bodies;
    /// none when lost). The `Attach` goes once every listed region has one.
    pub(super) fn note_listed(&mut self, rc: RegionCoords, epoch: Option<u64>, people: &[(PersonId, [f32; 3])]) {
        if let WorldAttach::Gathering { epoch: gathering, regions, .. } = &mut self.world_attach {
            if epoch.is_some_and(|e| e != *gathering) {
                log::debug!("{rc:?} listed its people for attach {epoch:?}, not {gathering}: ignored");
                return;
            }
            if let Some((_, listed)) = regions.get_mut(&rc) {
                *listed = Some(people.to_vec());
            }
        }
        self.try_finish_attach();
    }

    /// Past [`ATTACH_WAIT_MS`]: log the regions the attach still waits for (once); it
    /// keeps waiting — a running region is never attached with a guessed list (its
    /// unlisted people would go back to the history while their bodies run).
    pub(super) fn warn_attach_stragglers(&mut self, now_ms: u64) {
        if let WorldAttach::Gathering { epoch, since_ms, regions, warned, .. } = &mut self.world_attach {
            if !*warned && now_ms.saturating_sub(*since_ms) >= ATTACH_WAIT_MS {
                *warned = true;
                let waiting: Vec<RegionCoords> =
                    regions.iter().filter(|(_, (_, l))| l.is_none()).map(|(rc, _)| *rc).collect();
                let stopping: Vec<RegionCoords> =
                    self.regions.iter().filter(|(_, l)| l.stopping).map(|(rc, _)| *rc).collect();
                log::warn!(
                    "attach {epoch}: still waiting after {ATTACH_WAIT_MS} ms for {waiting:?} to list their people \
                     and {stopping:?} to park"
                );
            }
        }
    }

    /// While an `Attach` is gathered: `person`'s body moved over a seam (`moved` of the
    /// region it was first seen leaving, if it moved before during this gather).
    pub(super) fn note_moved(&mut self, person: PersonId, here: RegionCoords, step: Step) {
        if let WorldAttach::Gathering { moved, .. } = &mut self.world_attach {
            record_move(moved, person, here, step);
        }
    }

    /// Send the `Attach` being gathered once every listed region answered, then the
    /// requests whose answer never came (sent before the link went down, or timed out
    /// toward the client after they left — the coordinator's memo answers a
    /// state-changing one once), the outbox, and the lifecycle messages held meanwhile,
    /// in that order.
    pub(super) fn try_finish_attach(&mut self) {
        let WorldAttach::Gathering { regions, .. } = &self.world_attach else { return };
        if regions.values().any(|(_, listed)| listed.is_none()) {
            return;
        }
        // No region may be stopping: its bodies are in no list (a stopping region is not
        // attached), so a resident whose body stands there would be spawned again at home
        // by the attach's catch-up while that body still waits for the park. A stop always
        // ends — the park, or the loss of a dead thread — and that output retries this.
        if self.regions.values().any(|l| l.stopping) {
            return;
        }
        let WorldAttach::Gathering { epoch, regions, held, mut moved, .. } =
            std::mem::replace(&mut self.world_attach, WorldAttach::Down)
        else {
            unreachable!("checked")
        };
        self.world_attach = WorldAttach::Sent { epoch };
        let mut lists: BTreeMap<RegionCoords, (u64, Vec<(PersonId, [f32; 3])>)> = regions
            .into_iter()
            .map(|(rc, (incarnation, listed))| (rc, (incarnation, listed.expect("every listed region answered"))))
            .collect();
        // A body still in flight is its target's, whenever it left.
        for f in self.in_flight.values() {
            if let EntityKind::Person { person } = f.bundle.kind {
                record_move(&mut moved, person, f.target, Step::Into { to: f.target, at: bundle_at(&f.bundle) });
            }
        }
        exact_across_seams(&mut lists, &moved);
        let running: Vec<RunningRegion> = lists
            .into_iter()
            .map(|(rc, (incarnation, people))| RunningRegion { rc, incarnation, people })
            .collect();
        for r in &running {
            // Its people as they stand are the history's now: no `SpawnPeople` comes.
            if self.spawn_waiting.get(&r.rc) == Some(&r.incarnation) {
                self.spawn_waiting.remove(&r.rc);
            }
        }
        let reports: Vec<(ClientId, PlayerId)> = self.player_of.iter().map(|(c, p)| (*c, *p)).collect();
        let online: Vec<OnlinePlayer> = reports.into_iter().map(|(c, p)| self.online_report(c, p)).collect();

        // The `Attach` names who is online as things stand: a queued `PlayerOnline` /
        // `PlayerOffline` would only repeat an older report after it.
        self.outbox.retain(|m| {
            !matches!(m, HostMsg::PlayerOnline(_) | HostMsg::PlayerOffline { .. } | HostMsg::PlayerHandedTo { .. })
        });
        log::info!("attach {epoch}: {} regions, {} online, {} queued", running.len(), online.len(), self.outbox.len());
        let host = self.world_host();
        let generation = self.host_generation;
        let (boot, settled_below) = (self.world_boot, self.lowest_outstanding());
        self.settled_below = settled_below;
        self.link_send(HostMsg::Attach { host, epoch, running, online, generation, boot, settled_below });
        let queued: BTreeSet<RequestId> = self
            .outbox
            .iter()
            .filter_map(|m| match m {
                HostMsg::Request { id, .. } => Some(*id),
                _ => None,
            })
            .collect();
        let mut resend: BTreeMap<RequestId, HostMsg> = self
            .pending_requests
            .iter()
            .filter(|(id, _)| !queued.contains(id))
            .map(|(id, p)| (*id, HostMsg::Request { id: *id, player: p.player, req: p.req.clone() }))
            .collect();
        // Settle-only: a request its client was told failed is never executed now.
        resend.extend(
            self.expired_requests.iter().map(|(id, (_, req))| (*id, HostMsg::Settle { id: *id, req: req.clone() })),
        );
        let resend = resend.into_values();
        for m in resend.chain(std::mem::take(&mut self.outbox)).chain(held) {
            self.link_send(m);
        }
    }

    /// Straight onto the link.
    fn link_send(&mut self, msg: HostMsg) {
        if let Some(link) = self.world.as_mut() {
            link.send(msg);
        }
    }

    /// Notice the link going down (or coming back up). A new connection generation while
    /// up ([`WorldLink::generation`]) is a reconnect missed between two looks: it counts
    /// as down, then up (a fresh `Attach`).
    fn watch_world_link(&mut self, now_ms: u64) {
        let Some(link) = self.world.as_ref() else { return };
        let (up, generation) = (link.is_up(), link.generation());
        if up && generation != self.world_generation && !matches!(self.world_attach, WorldAttach::Down) {
            log::warn!("world link reconnected (generation {generation}): re-attaching");
            self.world_attach = WorldAttach::Down;
        }
        self.world_generation = generation;
        match (&self.world_attach, up) {
            (WorldAttach::Down, true) => self.world_link_up(now_ms),
            (WorldAttach::Down, false) | (_, true) => {}
            (_, false) => {
                log::warn!("world link down: lifecycle messages dropped, requests queued until it is back");
                self.world_attach = WorldAttach::Down;
            }
        }
    }

    /// Pump the world link at `now_ms` (an inline coordinator runs there) and apply
    /// everything the coordinator sent, in order.
    pub fn pump_world(&mut self, now_ms: u64) {
        let Some(link) = self.world.as_mut() else { return };
        link.pump(now_ms);
        self.watch_world_link(now_ms);
        // Each message is taken before it is applied: applying one may start a region,
        // which pumps the link again (`ensure_running`).
        while let Some(msg) = self.world.as_mut().and_then(|l| l.recv()) {
            self.apply_world_msg(msg, now_ms);
        }
    }

    /// [`Self::pump_world`] between upkeeps: at the latest `maintain`'s time, so the
    /// coordinator's calendar moves only in `maintain` (today's lockstep at no delay).
    pub(super) fn flush_world(&mut self) {
        self.pump_world(self.world_ms);
    }

    /// Send `msg` to the coordinator as the link stands: while it is down a request or
    /// claim message waits in the outbox and a lifecycle message is dropped (the next
    /// `Attach` supersedes it); while the `Attach` is gathered both wait (lifecycle held
    /// behind it).
    pub(super) fn world_send(&mut self, msg: HostMsg) {
        if let HostMsg::ReleaseClaim { player } = &msg {
            self.claim_changed(*player);
        }
        let Some(link) = self.world.as_ref() else { return };
        if !link.is_up() && !matches!(self.world_attach, WorldAttach::Down) {
            log::warn!("world link down: lifecycle messages dropped, requests queued until it is back");
            self.world_attach = WorldAttach::Down;
        }
        let queued = matches!(
            msg,
            HostMsg::Request { .. }
                | HostMsg::Settle { .. }
                | HostMsg::PlayerOnline(_)
                | HostMsg::PlayerOffline { .. }
                | HostMsg::ReleaseClaim { .. }
                | HostMsg::PlayerHandedTo { .. }
        );
        // `Settled` is not queued: the next `Attach` carries the watermark.
        match &mut self.world_attach {
            WorldAttach::Down | WorldAttach::Gathering { .. } if queued => self.outbox.push_back(msg),
            WorldAttach::Down => log::debug!("world link down: {msg:?} dropped"),
            WorldAttach::Gathering { held, .. } => held.push(msg),
            WorldAttach::Sent { .. } | WorldAttach::Attached => self.link_send(msg),
        }
    }

    fn apply_world_msg(&mut self, msg: CoordMsg, now_ms: u64) {
        match msg {
            CoordMsg::SpawnPeople { .. } | CoordMsg::Directive { .. } if !self.world_attached() => {
                // Decided before the coordinator applied our latest `Attach`, which
                // re-decided every listed region from its bodies: stale.
                log::debug!("coordinator message from before the attach dropped: {msg:?}");
                self.world_counters.stale_dropped += 1;
            }
            CoordMsg::Attached { epoch } => match self.world_attach {
                WorldAttach::Sent { epoch: sent } if sent == epoch => {
                    log::info!("world link: attach {epoch} applied");
                    self.world_attach = WorldAttach::Attached;
                }
                _ => log::debug!("world link: stale attach echo {epoch} ignored"),
            },
            CoordMsg::SpawnPeople { rc, incarnation, events } => {
                let current = self.spawn_waiting.get(&rc) == Some(&incarnation)
                    && self.incarnation.get(&rc) == Some(&incarnation)
                    && self.regions.get(&rc).is_some_and(|l| !l.stopping);
                if !current {
                    // That incarnation's `RegionParked` went ahead of this on the link: the
                    // history restored everyone it placed there.
                    log::debug!("people for {rc:?} incarnation {incarnation}, which has stopped, dropped");
                    self.world_counters.spawns_dropped += 1;
                    return;
                }
                self.spawn_waiting.remove(&rc);
                for kind in events {
                    if !self.send_to_region(rc, RegionInput::Event(kind)) {
                        break; // Lost and respawned: its new start asks again.
                    }
                }
            }
            CoordMsg::Directive { directive, incarnation } => {
                let rc = directive.region;
                let applies = self.incarnation.get(&rc) == Some(&incarnation)
                    && self.spawn_waiting.get(&rc) != Some(&incarnation)
                    && self.regions.get(&rc).is_some_and(|l| !l.stopping);
                if !applies {
                    log::debug!("history directive for {rc:?} incarnation {incarnation} bounced: {:?}", directive.event);
                    self.world_counters.directives_bounced += 1;
                    return self.world_send(HostMsg::DirectiveBounced { directive, incarnation });
                }
                let input = match self.directive_stamps.get(&rc) {
                    Some(&stamp) => RegionInput::Transfer(directive.event, stamp),
                    None => RegionInput::Event(directive.event),
                };
                // A region lost here is respawned and caught up afresh (a new incarnation:
                // what else was decided for this one bounces and is dropped as stale).
                self.send_to_region(rc, input);
            }
            CoordMsg::View { civs, sites, calendar } => {
                self.site_regions.extend(sites.iter().map(|m| (m.site, m.region)));
                self.world_view = Some(WorldViewCache {
                    civs: civs.clone(),
                    sites: sites.iter().map(|m| (m.site, m.clone())).collect(),
                    calendar: (now_ms, calendar),
                });
                for c in self.sessions.keys() {
                    let _ = self.out.send((Some(*c), ServerPacket::Calendar(calendar)));
                    let _ = self.out.send((Some(*c), ServerPacket::Sites(sites.clone())));
                    let _ = self.out.send((Some(*c), ServerPacket::Civs(civs.clone())));
                }
            }
            CoordMsg::Calendar(tick) => {
                if let Some(v) = self.world_view.as_mut() {
                    v.calendar = (now_ms, tick);
                }
                for c in self.sessions.keys() {
                    let _ = self.out.send((Some(*c), ServerPacket::Calendar(tick)));
                }
            }
            CoordMsg::Sites(sites) => {
                self.site_regions.extend(sites.iter().map(|m| (m.site, m.region)));
                if let Some(v) = self.world_view.as_mut() {
                    v.sites = sites.iter().map(|m| (m.site, m.clone())).collect();
                }
                for c in self.sessions.keys() {
                    let _ = self.out.send((Some(*c), ServerPacket::Sites(sites.clone())));
                }
            }
            CoordMsg::SiteMarkers(sites) => {
                self.site_regions.extend(sites.iter().map(|m| (m.site, m.region)));
                if let Some(v) = self.world_view.as_mut() {
                    v.sites = sites.into_iter().map(|m| (m.site, m)).collect();
                }
            }
            // Routed at the end of the client event or upkeep that made it, to the homes
            // as they stand then (a player embarking moves home after its region starts).
            CoordMsg::Notice { region, line } => self.world_notices.push((region, line)),
            CoordMsg::Reply { id, reply } => {
                self.world_reply(id, reply, now_ms);
                self.acknowledge_settled();
            }
            CoordMsg::ClaimIs { player, claim, report } => self.claim_is(player, claim, report),
        }
    }

    /// `player`'s claim changed here (a release sent, a granting reply applied, the
    /// player arrived from or left for another host).
    pub(super) fn claim_changed(&mut self, player: PlayerId) {
        self.claim_seq += 1;
        self.claim_changes.insert(player, self.claim_seq);
    }

    /// The report of `client`'s `player` for the coordinator, numbered: a `ClaimIs` is
    /// applied only if it answers the latest one ([`Self::claim_is`]).
    fn online_report(&mut self, client: ClientId, player: PlayerId) -> OnlinePlayer {
        self.claim_seq += 1;
        self.claim_reported.insert(player, self.claim_seq);
        OnlinePlayer {
            player,
            claim: self.claim_of_client(client),
            claim_pending: self.claim_pending(player),
            report: self.claim_seq,
        }
    }

    /// `player` has no session or body here any more: its claim bookkeeping goes (an
    /// answer to an old report of it is then ignored: [`Self::claim_is`]).
    pub(super) fn forget_claim_reports(&mut self, player: PlayerId) {
        if !self.player_of.values().any(|p| *p == player) {
            self.claim_changes.remove(&player);
            self.claim_reported.remove(&player);
        }
    }

    /// `player` gives up the claim `granted` (a grant it cannot use, or one its player was
    /// told failed): the coordinator is told, and every client of the player here that
    /// plays exactly that claim — handed to it by a `ClaimIs` meanwhile — drops it and is
    /// told so.
    fn give_back(&mut self, player: PlayerId, granted: Option<Claim>) {
        self.world_send(HostMsg::ReleaseClaim { player });
        let holders: Vec<ClientId> = self
            .player_of
            .iter()
            .filter(|(c, p)| **p == player && granted.is_some() && self.claim_of_client(**c) == granted)
            .map(|(c, _)| *c)
            .collect();
        for c in holders {
            log::info!("client {c}: gives back {granted:?}");
            self.fortress_of.remove(&c);
            self.adventurer_of.remove(&c);
            self.send_fortress(c);
        }
    }

    /// The coordinator's answer (`ClaimIs`) to report `report` of `player`: final. The
    /// player's client here plays exactly `claim` from now on, and is told so — unless the
    /// answer is not to the latest report, or the claim changed here since (a release, a
    /// grant, a crossing): stale. If no newer report is on its way the player is reported
    /// again at once, so the coordinator answers the claim as it stands now.
    fn claim_is(&mut self, player: PlayerId, claim: Option<Claim>, report: u64) {
        let Some(&latest) = self.claim_reported.get(&player) else {
            log::debug!("the claim of {player:?} is {claim:?}; the player is no longer here");
            return;
        };
        if report != latest {
            log::debug!("the claim of {player:?} is {claim:?} as of report {report}; report {latest} is on its way");
            return;
        }
        if self.claim_changes.get(&player).is_some_and(|&c| c > report) {
            log::debug!("the claim of {player:?} is {claim:?} as of report {report}, older than its last change: re-reported");
            let clients: Vec<ClientId> =
                self.player_of.iter().filter(|(_, p)| **p == player).map(|(c, _)| *c).collect();
            for c in clients {
                self.world_player_online(c);
            }
            return;
        }
        let clients: Vec<ClientId> = self.player_of.iter().filter(|(_, p)| **p == player).map(|(c, _)| *c).collect();
        if clients.is_empty() {
            log::debug!("the claim of {player:?} is {claim:?}; the player is no longer online here");
        }
        for c in clients {
            if self.claim_of_client(c) == claim {
                continue;
            }
            log::info!("client {c}: plays {:?}; the coordinator says {claim:?}", self.claim_of_client(c));
            self.fortress_of.remove(&c);
            self.adventurer_of.remove(&c);
            match claim {
                Some(Claim::Fortress(site)) => {
                    self.fortress_of.insert(c, site);
                }
                Some(Claim::Adventure(site)) => {
                    self.adventurer_of.insert(c, site);
                }
                None => {}
            }
            self.send_fortress(c);
        }
    }

    /// Send each notice the coordinator made to the connected clients whose home is its
    /// region.
    pub(super) fn route_notices(&mut self) {
        for (rc, line) in std::mem::take(&mut self.world_notices) {
            for c in self.sessions.keys().filter(|c| self.homes.get(c) == Some(&rc)) {
                let _ = self.out.send((Some(*c), ServerPacket::Notice(line.clone())));
            }
        }
    }

    /// `rc` started (its current incarnation): announce it, and — through an inline
    /// coordinator — take its people at once, before anything else reaches it.
    pub(super) fn world_region_started(&mut self, rc: RegionCoords) {
        if self.world.is_none() {
            return;
        }
        let incarnation = self.incarnation.get(&rc).copied().unwrap_or(0);
        debug_assert!(!self.world_live.contains(&rc), "{rc:?} started again before it parked");
        self.world_live.insert(rc);
        self.spawn_waiting.insert(rc, incarnation);
        self.world_send(HostMsg::RegionStarted { rc, incarnation });
        self.flush_world();
    }

    /// `rc` parked with `people`'s bodies in it (empty: it was lost — its thread died —
    /// and the history takes back everyone it had there). Once per start.
    pub(super) fn world_region_parked(&mut self, rc: RegionCoords, people: Vec<(crate::history::PersonId, [f32; 3])>) {
        self.spawn_waiting.remove(&rc);
        self.directive_stamps.remove(&rc);
        if self.world_live.remove(&rc) {
            // An attach being gathered lists it as it parked; the park follows the attach.
            self.note_listed(rc, None, &people);
            self.world_send(HostMsg::RegionParked { rc, people });
        }
    }

    /// `rc` stopped without parking its people (its thread died).
    pub(super) fn history_region_lost(&mut self, rc: RegionCoords) {
        self.world_region_parked(rc, Vec::new());
    }

    /// `rc` is stopping (its `Shutdown` sent): the history defers what acts on it.
    pub(super) fn world_region_stopping(&mut self, rc: RegionCoords) {
        self.directive_stamps.remove(&rc);
        if self.world_live.contains(&rc) {
            self.world_send(HostMsg::RegionStopping { rc });
        }
    }

    fn send_fortress(&self, id: ClientId) {
        let held = self.fortress_of.get(&id).and_then(|&site| {
            let region = *self.site_regions.get(&site)?;
            Some(crate::HeldFortress { site, region })
        });
        let _ = self.out.send((Some(id), ServerPacket::Fortress(held)));
    }

    fn embark_result(&self, id: ClientId, r: Result<SiteId, EmbarkRefusal>) {
        let _ = self.out.send((Some(id), ServerPacket::EmbarkResult(r)));
    }

    /// The player's embark: one fortress per player (checked here and by the coordinator,
    /// which validates and founds the site); the move happens when the coordinator's
    /// answer comes ([`Self::world_reply`]).
    pub(super) fn embark(&mut self, id: ClientId, req: EmbarkRequest, now_ms: u64) {
        if self.world.is_none() {
            return self.embark_result(id, Err(EmbarkRefusal::NoHistory));
        }
        if self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id) {
            return self.embark_result(id, Err(EmbarkRefusal::AlreadyHasFortress));
        }
        if self.is_unavailable(req.region) {
            return self.embark_result(id, Err(EmbarkRefusal::RegionUnavailable));
        }
        self.world_request(id, WorldRequest::Embark(req), now_ms);
    }

    /// Retire: the player stops playing its fortress (its claim given back; the site
    /// stays, returnable) and moves back where a fresh connection starts (`SPAWN_REGION`);
    /// the fortress region parks by the usual grace rule (its people go back to the
    /// abstract history). No fortress = no-op.
    pub(super) fn retire(&mut self, id: ClientId, now_ms: u64) {
        // No move possible (no refusal exists for a retire): the fortress stays.
        if !self.fortress_of.contains_key(&id) || !self.can_move(id, SPAWN_REGION) {
            return;
        }
        self.fortress_of.remove(&id);
        let landing = self.landing_in(SPAWN_REGION);
        self.move_client(id, SPAWN_REGION, landing, now_ms);
        self.release_claim_of(id);
        if self.world.is_some() {
            let _ = self.out.send((Some(id), ServerPacket::Sites(self.world_sites())));
        }
        self.send_fortress(id);
    }

    /// Return to a retired fortress: asked of the coordinator, which allows it only to a
    /// **player-founded** site no other *online* player plays (an offline claimant loses
    /// it, and finds it gone when it comes back).
    pub(super) fn return_to(&mut self, id: ClientId, site: SiteId, now_ms: u64) {
        if self.world.is_none() {
            return self.embark_result(id, Err(EmbarkRefusal::NoHistory));
        }
        if self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id) {
            return self.embark_result(id, Err(EmbarkRefusal::AlreadyHasFortress));
        }
        if self.site_regions.get(&site).is_some_and(|&rc| self.is_unavailable(rc)) {
            return self.embark_result(id, Err(EmbarkRefusal::RegionUnavailable));
        }
        self.world_request(id, WorldRequest::ReturnTo(site), now_ms);
    }

    fn adventure_result(&self, id: ClientId, r: Result<SiteId, AdventureRefusal>) {
        let _ = self.out.send((Some(id), ServerPacket::AdventureResult(r)));
    }

    /// Start an adventure at a populated site (the coordinator validates it): one at a
    /// time, no adventure while a fortress or another adventure is held.
    pub(super) fn start_adventure(&mut self, id: ClientId, site: SiteId, now_ms: u64) {
        if self.world.is_none() {
            return self.adventure_result(id, Err(AdventureRefusal::NoHistory));
        }
        if self.fortress_of.contains_key(&id) || self.adventurer_of.contains_key(&id) {
            return self.adventure_result(id, Err(AdventureRefusal::AlreadyPlaying));
        }
        if self.site_regions.get(&site).is_some_and(|&rc| self.is_unavailable(rc)) {
            return self.adventure_result(id, Err(AdventureRefusal::RegionUnavailable));
        }
        self.world_request(id, WorldRequest::StartAdventure(site), now_ms);
    }

    /// End the adventure: back to Fortress mode, then to `SPAWN_REGION` like
    /// [`Self::retire`]. No adventure = no-op.
    pub(super) fn end_adventure(&mut self, id: ClientId, now_ms: u64) {
        // No move possible (no refusal exists for an adventure's end): it goes on.
        if !self.adventurer_of.contains_key(&id) || !self.can_move(id, SPAWN_REGION) {
            return;
        }
        // A move in flight to another host's region has no home here: the order goes with
        // the move (`finish_move`).
        let home = self.homes.get(&id).copied().or_else(|| self.moving.get(&id).map(|m| m.target));
        if let Some(home) = home {
            self.send_player_event(
                id,
                home,
                GameEventKind::Order(id, Order::SetMode { mode: PlayMode::Fortress, at: None }),
            );
        }
        self.adventurer_of.remove(&id);
        let landing = self.landing_in(SPAWN_REGION);
        self.move_client(id, SPAWN_REGION, landing, now_ms);
        self.release_claim_of(id);
        if self.world.is_some() {
            let _ = self.out.send((Some(id), ServerPacket::Sites(self.world_sites())));
        }
    }

    /// `id`'s player gave up what it played: the coordinator frees the claim.
    fn release_claim_of(&mut self, id: ClientId) {
        if let Some(&player) = self.player_of.get(&id) {
            self.world_send(HostMsg::ReleaseClaim { player });
        }
    }

    /// Ask the coordinator `req` for `client`'s player: sent at once, or queued while the
    /// link is down ([`Self::world_send`]); answered by [`Self::world_reply`], or
    /// `WorldUnavailable` after [`WORLD_REQUEST_TIMEOUT_SECS`].
    pub(super) fn world_request(&mut self, client: ClientId, req: WorldRequest, now_ms: u64) {
        let Some(&player) = self.player_of.get(&client) else {
            log::warn!("client {client}: a world request with no player; refused");
            return self.answer_unavailable(client, &req);
        };
        let id = RequestId { host: self.world_host(), boot: self.world_boot, seq: self.next_request_seq };
        self.next_request_seq += 1;
        self.pending_requests.insert(id, PendingRequest { client, player, req: req.clone(), sent_ms: now_ms });
        self.world_send(HostMsg::Request { id, player, req });
    }

    /// The answer a request gets when the world services cannot give one.
    fn answer_unavailable(&self, client: ClientId, req: &WorldRequest) {
        match req {
            WorldRequest::Embark(_) | WorldRequest::ReturnTo(_) => {
                self.embark_result(client, Err(EmbarkRefusal::WorldUnavailable))
            }
            WorldRequest::StartAdventure(_) => self.adventure_result(client, Err(AdventureRefusal::WorldUnavailable)),
            WorldRequest::EmbarkOptions | WorldRequest::Legends(_) | WorldRequest::PersonInfo(_) => {}
        }
    }

    /// Requests waiting longer than the timeout: the player is told the world is
    /// unavailable and the request forgotten (out of the outbox too). A state-changing
    /// one that had left is kept in `expired_requests` until its answer comes.
    pub(super) fn expire_world_requests(&mut self, now_ms: u64) {
        let timeout = self.world_request_timeout_ms;
        let expired: Vec<RequestId> = self
            .pending_requests
            .iter()
            .filter(|(_, p)| now_ms.saturating_sub(p.sent_ms) >= timeout)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let p = self.pending_requests.remove(&id).expect("listed");
            let is_it = |m: &HostMsg| matches!(m, HostMsg::Request { id: i, .. } if *i == id);
            let queued = self.outbox.iter().any(is_it);
            self.outbox.retain(|m| !is_it(m));
            if !queued && p.req.changes_state() {
                self.expired_requests.insert(id, (p.player, p.req.clone()));
            }
            log::warn!("world request {id:?} of client {} unanswered in {timeout} ms: {:?}", p.client, p.req);
            if self.sessions.contains_key(&p.client) {
                self.answer_unavailable(p.client, &p.req);
            }
        }
        self.acknowledge_settled();
    }

    /// The lowest sequence of this boot whose request is still outstanding (pending, or
    /// expired and waiting for its answer); the next one to be made if none is.
    fn lowest_outstanding(&self) -> u64 {
        let first = |id: Option<&RequestId>| id.map_or(self.next_request_seq, |id| id.seq);
        first(self.pending_requests.keys().next()).min(first(self.expired_requests.keys().next()))
    }

    /// Tell the coordinator its answers below the lowest outstanding request are no
    /// longer needed ([`HostMsg::Settled`]) when that watermark rose.
    fn acknowledge_settled(&mut self) {
        let below = self.lowest_outstanding();
        if below > self.settled_below {
            self.settled_below = below;
            self.world_send(HostMsg::Settled { boot: self.world_boot, below });
        }
    }

    /// The coordinator answered request `id`. The first answer wins (a replayed request
    /// may be answered twice); an answer for a client that has gone gives back the claim
    /// it grants (the site stays, returnable).
    fn world_reply(&mut self, id: RequestId, reply: WorldReply, now_ms: u64) {
        let Some(p) = self.pending_requests.remove(&id) else {
            match self.expired_requests.remove(&id) {
                Some((player, _)) => {
                    log::info!("world reply {id:?} came after its request timed out: {reply:?}");
                    let granted = granted_claim(&reply);
                    if granted.is_some() {
                        // Its player was told it failed: it never completes.
                        self.give_back(player, granted);
                    }
                }
                None => log::debug!("world reply {id:?} answered already: dropped"),
            }
            return;
        };
        let client = p.client;
        if !self.sessions.contains_key(&client) || self.player_of.get(&client) != Some(&p.player) {
            log::info!("world reply {id:?} for client {client}, which has gone: {reply:?}");
            if reply.grants_claim() {
                self.give_back(p.player, granted_claim(&reply));
            }
            return;
        }
        match reply {
            WorldReply::EmbarkOptions(options) => {
                let _ = self.out.send((Some(client), ServerPacket::EmbarkOptions(options)));
            }
            WorldReply::Legends(page) => {
                let _ = self.out.send((Some(client), ServerPacket::Legends(page)));
            }
            WorldReply::PersonInfo(info) => {
                if let Some(info) = info {
                    let _ = self.out.send((Some(client), ServerPacket::PersonInfo(info)));
                }
            }
            WorldReply::EmbarkRefused(e) => self.embark_result(client, Err(e)),
            WorldReply::AdventureRefused(e) => self.adventure_result(client, Err(e)),
            WorldReply::Embarked { site, region } | WorldReply::Returned { site, region } => {
                self.site_regions.insert(site, region);
                if self.is_unavailable(region) || !self.can_move(client, region) {
                    self.give_back(p.player, Some(Claim::Fortress(site)));
                    return self.embark_result(client, Err(EmbarkRefusal::RegionUnavailable));
                }
                // The site is founded (or taken up): the player moves in (the region's
                // catch-up, or the founding itself when it already runs, spawns the party).
                let landing = self.landing_in(region);
                self.move_client(client, region, landing, now_ms);
                self.fortress_of.insert(client, site);
                self.claim_changed(p.player);
                self.embark_result(client, Ok(site));
                self.send_fortress(client);
                if let WorldRequest::Embark(req) = &p.req {
                    crate::perf_event!(
                        "embark",
                        "client={client} site={} people={} to={},{}",
                        site.0,
                        req.people.len(),
                        region.x,
                        region.z
                    );
                }
            }
            WorldReply::AdventureStarted { site, region } => {
                self.site_regions.insert(site, region);
                if self.is_unavailable(region) || !self.can_move(client, region) {
                    self.give_back(p.player, Some(Claim::Adventure(site)));
                    return self.adventure_result(client, Err(AdventureRefusal::RegionUnavailable));
                }
                let landing = self.landing_in(region);
                // Already at home in that region: `move_client` only resends `PlayerRegion`
                // and the body stays put, so the order itself carries the landing tile
                // (a high level clamps to the column's top and `nearest_standable` searches
                // down). Elsewhere `CreateClientAt` has placed the body.
                let at = (self.homes.get(&client) == Some(&region))
                    .then(|| [landing[0] as i32, ADVENTURE_LANDING_HINT_Y, landing[1] as i32]);
                self.move_client(client, region, landing, now_ms);
                self.send_player_event(
                    client,
                    region,
                    GameEventKind::Order(client, Order::SetMode { mode: PlayMode::Adventure, at }),
                );
                self.adventurer_of.insert(client, site);
                self.claim_changed(p.player);
                self.adventure_result(client, Ok(site));
                self.send_fortress(client);
                crate::perf_event!("adventure_start", "client={client} site={} to={},{}", site.0, region.x, region.z);
            }
        }
    }

    /// `id`'s player is online with what it plays (its claims are settled: restored, or
    /// none): the coordinator counts its fortress as in use. A player handed here whose
    /// session has not come yet counts as online (its body is here).
    pub(super) fn world_player_online(&mut self, id: ClientId) {
        if self.world.is_none() || !(self.sessions.contains_key(&id) || self.awaiting_session.contains_key(&id)) {
            return;
        }
        let Some(&player) = self.player_of.get(&id) else { return };
        let report = self.online_report(id, player);
        self.world_send(HostMsg::PlayerOnline(report));
    }

    /// Whether a claim-changing request of `player` is unsettled here (pending toward its
    /// client, or timed out and waiting for its answer): until it settles the
    /// coordinator's claim for the player stands over the one reported.
    pub(super) fn claim_pending(&self, player: PlayerId) -> bool {
        self.pending_requests.values().any(|p| p.player == player && p.req.changes_state())
            || self.expired_requests.values().any(|(p, _)| *p == player)
    }

    /// What a session holds, told on connect: its fortress (`Fortress`), and a
    /// restored claim as the answer an embark / adventure start would have had,
    /// so the client leaves the arrival map without embarking again.
    pub(super) fn send_claims(&self, id: ClientId) {
        self.send_fortress(id);
        if let Some(&site) = self.fortress_of.get(&id) {
            self.embark_result(id, Ok(site));
        }
        if let Some(&site) = self.adventurer_of.get(&id) {
            self.adventure_result(id, Ok(site));
        }
    }
}

/// The claim a granting reply hands out.
fn granted_claim(reply: &WorldReply) -> Option<Claim> {
    match *reply {
        WorldReply::Embarked { site, .. } | WorldReply::Returned { site, .. } => Some(Claim::Fortress(site)),
        WorldReply::AdventureStarted { site, .. } => Some(Claim::Adventure(site)),
        _ => None,
    }
}

#[cfg(test)]
mod attach_lists_tests {
    use super::*;

    fn rc(x: i32, z: i32) -> RegionCoords {
        RegionCoords::new(x, z)
    }

    /// P leaves listed A before A lists (A's list lacks P), is delivered into C — started
    /// during the gather, so not listed — and crosses on out of C (absorbed, or into D,
    /// also unlisted). The held crossings replay A → C → …, so P must be in A's list,
    /// and in no other; a fallback to C would leave P in no list (the catch-up then
    /// respawns P at A beside a ghost or a second body).
    #[test]
    fn multi_hop_through_unlisted_regions_lists_the_first_region_left() {
        let (a, b, c, d) = (rc(1, 0), rc(2, 0), rc(3, 0), rc(4, 0));
        let (p, q) = (PersonId(7), PersonId(8));
        for absorbed in [true, false] {
            let mut moved = BTreeMap::new();
            record_move(&mut moved, p, a, Step::Into { to: c, at: [1.0; 3] });
            record_move(&mut moved, p, c, Step::Into { to: c, at: [2.0; 3] });
            let last = if absorbed { Step::Absorbed { at: [3.0; 3] } } else { Step::Into { to: d, at: [3.0; 3] } };
            record_move(&mut moved, p, c, last);
            let mut lists = BTreeMap::from([(a, (1, vec![(q, [0.0; 3])])), (b, (1, Vec::new()))]);
            exact_across_seams(&mut lists, &moved);
            let holding: Vec<RegionCoords> =
                lists.iter().filter(|(_, (_, l))| l.iter().any(|(x, _)| *x == p)).map(|(r, _)| *r).collect();
            assert_eq!(holding, vec![a], "absorbed {absorbed}: P in A's list only: {lists:?}");
            assert!(lists[&a].1.iter().any(|(x, _)| *x == q), "A keeps its own people");
        }
    }
}

