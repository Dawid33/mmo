//! Several region hosts of one world in one test: a [`Rig`] per host (its own
//! [`game::WorldManager`] on an [`game::InlineSpawner`], owning the regions
//! `game::Static` gives it), every pair of hosts joined by a seeded, delaying
//! [`InlinePeerLink`] under a [`PeerSession`] at each end. Each host's world tick is its
//! spawner's round count, so [`MultiRig::round`] ticks every host every round and checks
//! they agree. Every host-seam test builds on it. [`MultiRig::with_coordinator`] gives
//! every host its own seeded, delaying inline link to ONE world coordinator.
use super::cut_store::CutStore;
use super::rig::{Rig, CLIENT};
use game::{
    ClientId, ClientPacket, CutId, EntityId, HostEndpoint, InFlightRow, Intent, IntentMsg, InlinePeerLink, InlineSchedule,
    InlineSpawner, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId, PlayerRecord, RegionCoords,
    RegionSpawner, Reliable, ServerEvent, StoredRegion, WorldCut, REGION_SIZE, TICK_RATE,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The world generation every host of the rig runs.
pub const RIG_GENERATION: u64 = 1;
/// Most rounds [`MultiRig::connect`] waits for every session to be up.
pub const CONNECT_ROUNDS: usize = 40;

/// splitmix64 of `seed` and `i`: the rig's per-host and per-link seeds.
pub fn mix(seed: u64, i: u64) -> u64 {
    let mut z = seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A host's link to the rig's shared coordinator, recording every message the host sent
/// while the link was up.
pub struct RigWorldLink {
    inner: game::InlineWorldLink,
    sent: Arc<Mutex<Vec<game::HostMsg>>>,
    /// Test hook ([`MultiRig::hold_world`]): while set, nothing is read (the
    /// coordinator's messages wait on the link, in order); sending goes on.
    hold: Arc<std::sync::atomic::AtomicBool>,
    /// `HopIs` messages the host read ([`MultiRig::hop_is_received`]).
    hop_is: Arc<std::sync::atomic::AtomicUsize>,
}

impl game::WorldLink for RigWorldLink {
    fn send(&mut self, msg: game::HostMsg) {
        if self.inner.is_up() {
            self.sent.lock().unwrap().push(msg.clone());
        }
        self.inner.send(msg)
    }
    fn recv(&mut self) -> Option<game::CoordMsg> {
        if self.hold.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let msg = self.inner.recv();
        if matches!(msg, Some(game::CoordMsg::HopIs { .. })) {
            self.hop_is.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        msg
    }
    fn is_up(&self) -> bool {
        self.inner.is_up()
    }
    fn pump(&mut self, now_ms: u64) {
        self.inner.pump(now_ms)
    }
    fn as_inline_mut(&mut self) -> Option<&mut game::InlineWorldLink> {
        Some(&mut self.inner)
    }
    fn as_inline(&self) -> Option<&game::InlineWorldLink> {
        Some(&self.inner)
    }
}

/// A test hook on one direction of a link ([`MultiRig::strip_next_wrapper`]): once armed,
/// the first [`Reliable::InCut`] sent after a [`Reliable::CutMark`] goes out bare — a
/// sender breaking the world cut's rule.
#[derive(Default, Debug)]
pub struct Strip {
    pub armed: bool,
    pub after_mark: bool,
    pub stripped: usize,
    /// While set, the acknowledgements this end sends are lost ([`MultiRig::mute_acks`]).
    pub mute_acks: bool,
}

/// One end of a link the rig keeps a handle on (to take it down, read its transit), and
/// its sending side's [`Strip`] hook.
pub struct SharedEnd(pub Arc<Mutex<InlinePeerLink>>, pub Arc<Mutex<Strip>>);

impl SharedEnd {
    fn end(&self) -> std::sync::MutexGuard<'_, InlinePeerLink> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl PeerLink for SharedEnd {
    fn send(&mut self, msg: PeerMsg) {
        let msg = {
            let mut strip = self.1.lock().unwrap_or_else(|e| e.into_inner());
            if strip.mute_acks && matches!(msg, PeerMsg::Ack { .. }) {
                return;
            }
            match msg {
                PeerMsg::Reliable { body: Reliable::CutMark { cut }, seq } if strip.armed => {
                    strip.after_mark = true;
                    PeerMsg::Reliable { body: Reliable::CutMark { cut }, seq }
                }
                PeerMsg::Reliable { body: Reliable::InCut { body, .. }, seq } if strip.armed && strip.after_mark => {
                    strip.armed = false;
                    strip.stripped += 1;
                    PeerMsg::Reliable { body: *body, seq }
                }
                other => other,
            }
        };
        self.end().send(msg)
    }
    fn recv(&mut self) -> Option<PeerMsg> {
        self.end().recv()
    }
    fn is_up(&self) -> bool {
        self.end().is_up()
    }
    fn connection(&self) -> u64 {
        self.end().connection()
    }
    fn pump(&mut self, now_ms: u64) {
        self.end().pump(now_ms)
    }
}

/// The hello host `ordinal` of the rig starts its sessions with.
pub fn rig_hello(ordinal: u32) -> PeerHello {
    rig_hello_as(ordinal, rig_boot(ordinal))
}

/// Host `ordinal`'s first boot in the rig.
pub fn rig_boot(ordinal: u32) -> u64 {
    1000 + u64::from(ordinal)
}

/// [`rig_hello`] of boot `boot` (a host started again, [`MultiRig::start`]).
pub fn rig_hello_as(ordinal: u32, boot: u64) -> PeerHello {
    PeerHello {
        ordinal,
        boot,
        generation: RIG_GENERATION,
        endpoint: rig_endpoint(ordinal),
        received_through: 0,
        received_from_boot: None,
    }
}

/// The world-link boot (request ids) of a host whose peer boot is `peer_boot`.
pub fn world_boot(peer_boot: u64) -> u64 {
    peer_boot + 3000
}

/// Rounds [`MultiRig::run_succession`] waits for a running world cut to be decided.
pub const SUCCESSION_DECLARE_ROUNDS: usize = 400;

/// The adjacent pair of regions nearest the origin whose owners differ (`hosts` hosts,
/// `s` regions per super-region side): `(a, b)`, `b` one step +x or +z of `a`.
pub fn host_seam(hosts: u32, s: u32) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| game::Static { ordinal: 0, hosts }.owner_of(rc, s);
    for a in nearest_first(64) {
        for b in [RegionCoords::new(a.x + 1, a.z), RegionCoords::new(a.x, a.z + 1)] {
            if owner(a) != owner(b) {
                return (a, b);
            }
        }
    }
    panic!("no host seam within 64 regions of the origin ({hosts} hosts, s = {s})");
}

/// The 2x2 block of regions nearest the origin whose four owners all differ (`s`
/// regions per super-region side, 4 hosts), as `[a, a+x, a+x+z, a+z]` — a loop round
/// its corner.
pub fn four_host_corner(s: u32) -> [RegionCoords; 4] {
    let owner = |rc: RegionCoords| game::Static { ordinal: 0, hosts: 4 }.owner_of(rc, s);
    for a in nearest_first(64) {
        let block = [
            a,
            RegionCoords::new(a.x + 1, a.z),
            RegionCoords::new(a.x + 1, a.z + 1),
            RegionCoords::new(a.x, a.z + 1),
        ];
        let owners: BTreeSet<u32> = block.iter().map(|rc| owner(*rc)).collect();
        if owners.len() == 4 {
            return block;
        }
    }
    panic!("no four-host corner within 64 regions of the origin (s = {s})");
}

/// Regions within `r` of the origin, nearest first (then by x, z).
fn nearest_first(r: i32) -> Vec<RegionCoords> {
    let mut all: Vec<RegionCoords> =
        (-r..=r).flat_map(|x| (-r..=r).map(move |z| RegionCoords::new(x, z))).collect();
    all.sort_by_key(|rc| (rc.x.abs() + rc.z.abs(), rc.x, rc.z));
    all
}

/// Where an entity is, for [`MultiRig::assert_conserved_world`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// Owned by a running region of a host.
    Region { host: u32, rc: RegionCoords },
    /// In a host's `in_flight` or `pending_arrivals`.
    Manager { host: u32 },
    /// In a host's session to `peer`, never sent.
    Unsent { host: u32, peer: u32 },
    /// In transit on the link to host `to`.
    Transit { to: u32 },
    /// In the state of a region parked on a host (only with [`MultiRig::parked_hold`]).
    Parked { host: u32, rc: RegionCoords },
    /// Waiting on a host for its region's super-region to have an owner it knows
    /// (`unowned_waiting`, [`MultiRig::with_directory`]).
    Unowned { host: u32, sr: game::SuperRegion },
}

pub struct MultiRig {
    pub hosts: Vec<Rig>,
    /// Regions per super-region side.
    pub s: u32,
    /// Each host's keeper region: the region its keeper client's body stands in (owned
    /// by the host, far from the origin, so it never is one a test plays with).
    pub homes: Vec<RegionCoords>,
    /// Each pair of hosts' link `(a, b)` with `a < b`: (end on `a`, end on `b`).
    pub links: BTreeMap<(u32, u32), (Arc<Mutex<InlinePeerLink>>, Arc<Mutex<InlinePeerLink>>)>,
    /// Every non-person entity seen, and where it was last (`None` = only in a session's
    /// sent and unacknowledged part): it must never vanish.
    pub tracked: BTreeMap<EntityId, Option<Place>>,
    /// The voxel bodies seen: they may end by settling in the region that owns them.
    pub settling: BTreeSet<EntityId>,
    /// Every person body ever seen in a region: never tracked (a history may absorb one,
    /// also while it is only in a sent, unacknowledged transfer).
    pub person_ids: BTreeSet<EntityId>,
    /// Items the world holds, once a test put some in ([`Self::assert_items`]): checked
    /// every round.
    pub items: Option<u64>,
    /// The players a test connected ([`Self::connect_home`]), by session id: each is
    /// conserved across hosts every round (see [`Self::assert_conserved_world`]).
    pub players: BTreeMap<ClientId, PlayerId>,
    /// The bodies those players were seen in: one may end by parking.
    pub player_bodies: BTreeMap<EntityId, PlayerId>,
    /// The seed and link delay the rig was made with ([`Self::with_coordinator`]).
    pub seed: u64,
    pub link_delay: u32,
    /// The world coordinator every host's world link reaches ([`Self::with_coordinator`]).
    pub coordinator: Option<Arc<Mutex<game::coordinator::SharedCore>>>,
    /// What each host sent its coordinator (while its link was up), by host.
    pub world_sent: Vec<Arc<Mutex<Vec<game::HostMsg>>>>,
    /// Each host's [`RigWorldLink`] hold flag.
    pub world_hold: Vec<Arc<std::sync::atomic::AtomicBool>>,
    /// Each host's count of `HopIs` read ([`RigWorldLink`]).
    pub world_hop_is: Vec<Arc<std::sync::atomic::AtomicUsize>>,
    /// Made without a spawner schedule (every region ticks in lockstep).
    pub lockstep: bool,
    /// The rig's in-memory two-phase store (world cuts, [`Self::drive_world_cuts`]):
    /// staged parts by `(cut, host)`, as Postgres holds prepared transactions.
    pub parts: BTreeMap<(CutId, u32), WorldCut>,
    /// The decision of every cut decided (first writer wins).
    pub decided: BTreeMap<CutId, bool>,
    /// What each host's committed parts hold (regions, its in-flight rows).
    pub committed: BTreeMap<u32, CutStore>,
    /// The players table, as committed parts wrote it.
    pub committed_players: BTreeMap<PlayerId, PlayerRecord>,
    /// The parts of the last committed cut, by host.
    pub last_cut: Option<(CutId, BTreeMap<u32, WorldCut>)>,
    /// Every part a host staged, in order: `(cut, host)`.
    pub prepared: Vec<(CutId, u32)>,
    /// Parts of cuts decided aborted, as they were staged.
    pub aborted_parts: Vec<(CutId, u32, WorldCut)>,
    /// Each direction's [`Strip`] hook, by `(from, to)`.
    pub strips: BTreeMap<(u32, u32), Arc<Mutex<Strip>>>,
    /// The entities in every host's parked regions' states count as places too
    /// ([`Place::Parked`], decoded every round — costly, so off by default): a test that
    /// parks whole hosts (every host leaving) turns it on.
    pub parked_hold: bool,
    /// The directory every host answers from ([`Self::with_directory`]); `None` = `Static`.
    pub directory: Option<Vec<(game::SuperRegion, Option<u32>, u64)>>,
    /// The rig's `super_regions` table while hosts claim on demand ([`Self::with_claims`]):
    /// owner (`None` = a free row) and epoch per super-region.
    pub claim_store: Option<BTreeMap<game::SuperRegion, (Option<u32>, u64)>>,
    /// Claims asked of the store and not answered yet: `(rounds left, host, super-regions)`
    /// (at most one per host, as a server keeps one `claim_free` in flight).
    pub claims_pending: Vec<(u32, u32, Vec<game::SuperRegion>)>,
    /// Rounds played (the seed of each claim's answer delay).
    pub rounds: u64,
    /// Each host's current boot ([`Self::start`] makes a new one).
    pub boots: Vec<u64>,
    /// Hosts killed ([`Self::kill`]) and not started again: their slot holds an empty
    /// placeholder that only keeps the world tick.
    pub dead: BTreeSet<u32>,
    /// The schedule the hosts' spawners run (a started host's too).
    pub schedule: Option<fn(u64) -> InlineSchedule>,
    /// The rig store's `successions` table ([`Self::run_succession`]), with each one's
    /// settling cut (`None`: open).
    pub successions: Vec<(game::Succession, Option<CutId>)>,
    /// What each committed part adopted (`WorldCut::adopts`, Task 9): `(cut, heir, adopted)`.
    pub adopts: Vec<(CutId, u32, Vec<u32>)>,
    /// Entities a killed host held, or that were on their way to or from it
    /// ([`Self::kill`]): no longer tracked — the heir's resolution (Task 9) brings them back.
    pub forgiven: BTreeSet<EntityId>,
    /// Whether the rig answers an heir's adoption reads ([`Self::with_adoption`]).
    pub adopting: bool,
    /// Adoption reads asked and not answered yet: `(rounds left, heir, succession, predecessor)`.
    pub adoptions_pending: Vec<(u32, u32, game::SuccessionId, u32)>,
    /// Every adoption read answered: `(succession, heir)`, in order.
    pub adopted: Vec<(game::SuccessionId, u32)>,
    /// Test hook: the first start of every region the store answer of an adoption read
    /// hands the heir fails its load (`InlineSpawner::fail_next_start`), as a store read
    /// error on the region's thread would.
    pub fail_adoption_loads: bool,
    /// Successions a rollback abandoned ([`Self::roll_back`]).
    pub abandoned: BTreeSet<game::SuccessionId>,
    /// The rig store's world generation (raised by [`Self::roll_back`]).
    pub generation: u64,
}

/// Host `h`'s endpoint in the rig (what its sessions' first packet names).
pub fn rig_endpoint(h: u32) -> HostEndpoint {
    HostEndpoint { ordinal: h, quic: None, webtransport: None, cert_hash: None }
}

impl MultiRig {
    /// `hosts` hosts of one voxel world (a one-block floor everywhere), `s` regions per
    /// super-region side; each host's spawner runs `schedule` (seeded from `seed` and
    /// its ordinal) or lockstep, every link delays by 0..=`link_delay_steps` pumps.
    /// Each host gets a keeper client standing in its home region; every session is up
    /// before this returns.
    pub fn new(
        hosts: u32,
        s: u32,
        schedule: Option<fn(u64) -> InlineSchedule>,
        seed: u64,
        link_delay_steps: u32,
    ) -> MultiRig {
        Self::new_from(hosts, s, schedule, seed, link_delay_steps, None)
    }

    /// [`Self::new`], every host's regions starting from `stored` where it holds them (a
    /// world restarted from its store).
    pub fn new_from(
        hosts: u32,
        s: u32,
        schedule: Option<fn(u64) -> InlineSchedule>,
        seed: u64,
        link_delay_steps: u32,
        stored: Option<BTreeMap<RegionCoords, StoredRegion>>,
    ) -> MultiRig {
        let mut rigs = Vec::new();
        let mut homes = Vec::new();
        for h in 0..hosts {
            let ownership = game::Static { ordinal: h, hosts };
            let mut spawner = match &stored {
                Some(regions) => InlineSpawner::with_portable(regions.clone()),
                None => InlineSpawner::default(),
            };
            if let Some(f) = schedule {
                spawner.set_schedule(Some(f(mix(seed, u64::from(h)))));
            }
            let mut rig = Rig::with_ownership(spawner, super::seam_translate::uniform_floor, Box::new(ownership), s);
            rig.manager.set_endpoints(rig_endpoint(h), game::seeded_nonces(mix(seed, 2000 + u64::from(h))));
            rigs.push(rig);
            let home = nearest_first(64)
                .into_iter()
                .map(|rc| RegionCoords::new(rc.x + 32, rc.z + 32))
                .find(|rc| ownership.owns(*rc, s))
                .expect("a host owns a region near (32, 32)");
            homes.push(home);
        }
        let mut links = BTreeMap::new();
        let mut strips = BTreeMap::new();
        for a in 0..hosts {
            for b in a + 1..hosts {
                let (ea, eb) = InlinePeerLink::pair(mix(seed, 1000 + u64::from(a * hosts + b)), link_delay_steps);
                let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
                let (sa, sb) = (Arc::new(Mutex::new(Strip::default())), Arc::new(Mutex::new(Strip::default())));
                rigs[a as usize].manager.set_peer(b, PeerSession::new(rig_hello(a), b), Box::new(SharedEnd(ea.clone(), sa.clone())));
                rigs[b as usize].manager.set_peer(a, PeerSession::new(rig_hello(b), a), Box::new(SharedEnd(eb.clone(), sb.clone())));
                links.insert((a, b), (ea, eb));
                strips.insert((a, b), sa);
                strips.insert((b, a), sb);
            }
        }
        let mut rig =
            MultiRig {
                hosts: rigs,
                s,
                homes,
                links,
                tracked: BTreeMap::new(),
                settling: BTreeSet::new(),
                person_ids: BTreeSet::new(),
                items: None,
                players: BTreeMap::new(),
                player_bodies: BTreeMap::new(),
                seed,
                link_delay: link_delay_steps,
                coordinator: None,
                world_sent: Vec::new(),
                world_hold: Vec::new(),
                world_hop_is: Vec::new(),
                lockstep: schedule.is_none(),
                parts: BTreeMap::new(),
                decided: BTreeMap::new(),
                committed: BTreeMap::new(),
                committed_players: BTreeMap::new(),
                last_cut: None,
                prepared: Vec::new(),
                aborted_parts: Vec::new(),
                strips,
                parked_hold: false,
                directory: None,
                claim_store: None,
                claims_pending: Vec::new(),
                rounds: 0,
                boots: (0..hosts).map(rig_boot).collect(),
                dead: BTreeSet::new(),
                schedule,
                successions: Vec::new(),
                adopts: Vec::new(),
                forgiven: BTreeSet::new(),
                adopting: false,
                adoptions_pending: Vec::new(),
                adopted: Vec::new(),
                fail_adoption_loads: false,
                abandoned: BTreeSet::new(),
                generation: RIG_GENERATION,
            };
        for h in 0..hosts {
            rig.keep(h);
        }
        rig.connect();
        rig
    }

    /// One world coordinator over `history` for every host: each host's world link is its
    /// own [`game::InlineWorldLink::shared`] link to it, delaying both ways by
    /// 0..=`link_delay` pumps with its own seed (so different hosts' messages interleave
    /// differently per seed). Rounds until every host is attached.
    pub fn with_coordinator(&mut self, history: game::history::History) {
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let core = game::coordinator::SharedCore::new(game::Coordinator::new(history, now));
        // Every link before any is installed (an install pumps it): none misses the view.
        let links: Vec<game::InlineWorldLink> = (0..self.hosts.len() as u32)
            .map(|h| game::InlineWorldLink::shared(core.clone(), h).with_delay(mix(self.seed, 3000 + u64::from(h)), self.link_delay))
            .collect();
        for (h, inner) in links.into_iter().enumerate() {
            let sent = Arc::new(Mutex::new(Vec::new()));
            self.world_sent.push(sent.clone());
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.world_hold.push(hold.clone());
            let hop_is = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            self.world_hop_is.push(hop_is.clone());
            let rig = &mut self.hosts[h];
            let now = rig.now_ms;
            rig.manager.set_world_link(Box::new(RigWorldLink { inner, sent, hold, hop_is }), world_boot(self.boots[h]), now);
            rig.settle();
        }
        self.coordinator = Some(core);
        for _ in 0..CONNECT_ROUNDS {
            if self.hosts.iter().all(|r| r.manager.world_attached()) {
                return;
            }
            self.round();
        }
        panic!("the hosts never all attached to the coordinator");
    }

    /// Arm the [`Strip`] hook of the link `from` → `to`.
    pub fn strip_next_wrapper(&mut self, from: u32, to: u32) {
        let mut strip = self.strips[&(from, to)].lock().unwrap();
        *strip = Strip { armed: true, after_mark: false, stripped: strip.stripped, mute_acks: strip.mute_acks };
    }

    /// From now on the acknowledgements host `from` sends host `to` are lost: what `to`
    /// sent stays unacknowledged in its session.
    pub fn mute_acks(&mut self, from: u32, to: u32) {
        self.strips[&(from, to)].lock().unwrap().mute_acks = true;
    }

    /// Wrappers the `from` → `to` hook stripped so far.
    pub fn stripped(&self, from: u32, to: u32) -> usize {
        self.strips[&(from, to)].lock().unwrap().stripped
    }

    /// World cuts every `every_ms` (the coordinator's period; its clock is the hosts').
    /// Rounds then go through [`Self::cut_round`].
    pub fn with_world_cuts(&mut self, every_ms: u64) {
        let hosts: BTreeSet<u32> = (0..self.hosts.len() as u32).collect();
        let mut core = self.core();
        core.set_cut_epoch(1);
        core.set_expected_hosts(hosts);
        core.set_world_cuts(every_ms);
    }

    /// A round ([`Self::round`]) with every host's upkeep ([`game::WorldManager::maintain`],
    /// which drives the coordinator's clock) and the world cuts driven
    /// ([`Self::drive_world_cuts`]).
    pub fn cut_round(&mut self) {
        let tick = self.hosts[0].manager.spawner_mut().world_tick();
        if self.coordinator.is_some() {
            self.core().set_world_tick(tick);
        }
        self.round();
        for rig in &mut self.hosts {
            let now = rig.now_ms;
            rig.manager.maintain(now);
            rig.settle();
        }
        self.drive_world_cuts();
        for rig in &mut self.hosts {
            rig.settle();
        }
    }

    /// What a server and the coordinator runtime do with a world cut, against the rig's
    /// in-memory two-phase store: each host's complete part is staged
    /// ([`Self::prepare_part`]) and the host told; the coordinator's decisions are
    /// recorded ([`Self::decide`]) and handed back; each host's `CutDone` is settled
    /// (`cut_committed` / `cut_failed`); a lookup is answered from the decisions (an
    /// undecided cut aborted).
    pub fn drive_world_cuts(&mut self) {
        let Some(core) = self.coordinator.clone() else { return };
        for h in 0..self.hosts.len() as u32 {
            let m = &mut self.hosts[h as usize].manager;
            // Only a world cut's part (a test's one-phase cut stays for the test to take).
            if m.world_cut().is_none() {
                continue;
            }
            if let Some(cut) = m.take_cut() {
                let id = cut.id;
                match self.prepare_part(h, cut) {
                    Ok(()) => self.hosts[h as usize].manager.world_cut_prepared(id),
                    Err(why) => self.hosts[h as usize].manager.world_cut_failed(id, game::CutFailure::Store(why)),
                }
            }
        }
        for rig in &mut self.hosts {
            for (cut, committed) in rig.manager.take_world_cut_done() {
                if committed {
                    rig.manager.cut_committed(cut, Vec::new());
                } else {
                    rig.manager.cut_failed(cut);
                }
            }
        }
        loop {
            let decision = core.lock().unwrap_or_else(|e| e.into_inner()).take_decision();
            let Some(d) = decision else { break };
            let stored = self.decide_settling(d.cut, d.commit, &d.hosts, &d.settles);
            core.lock().unwrap_or_else(|e| e.into_inner()).decided(d.cut, stored);
        }
        let lookups = core.lock().unwrap_or_else(|e| e.into_inner()).take_cut_lookups();
        for (host, cut) in lookups {
            let stored = self.decide(cut, false, &[]);
            core.lock().unwrap_or_else(|e| e.into_inner()).cut_outcome(host, cut, stored);
        }
    }

    /// Host `h` stages its part `cut` (as `prepare_cut`): refused with the history in it;
    /// a part of a cut decided already finishes itself (committed or dropped). No two
    /// staged parts of one cut may write one player record or one entity (a test failure).
    pub fn prepare_part(&mut self, h: u32, cut: WorldCut) -> Result<(), String> {
        assert!(cut.history.is_none(), "a host's part never carries the history");
        assert_eq!(cut.host, h, "a part is its host's");
        if let Some(&committed) = self.decided.get(&cut.id) {
            if committed {
                self.apply_part(h, &cut);
                return Ok(());
            }
            return Err(format!("cut {} was decided aborted before this part prepared", cut.id.0));
        }
        // Criterion C: every host takes part in the rig's cuts (but one that left), so a part writes no row for
        // what is in transit to another participant — except what it sent wrapped after its
        // own mark (the ruling's case 3), whose stamp / due it raised to the cut tick.
        for row in &cut.in_flight {
            let (target, at) = match row {
                InFlightRow::Transfer { target, stamp, .. } => (*target, *stamp),
                InFlightRow::Intent { target, msg: IntentMsg::Intent(s) } => (*target, s.due),
                InFlightRow::Intent { target, msg: IntentMsg::Reply(s) } => (*target, s.due),
            };
            // A region nobody owns (or whose owner is unknown) waits on its sender: its row.
            let Some(owner) = self.hosts[h as usize].manager.owner_host(target) else { continue };
            // A host that left (Task 14) takes no part: what waits for it is the sender's row.
            let absent = self.hosts[h as usize].manager.peer_left(owner);
            assert!(
                owner == h || at >= cut.tick || absent,
                "cut {}: host {h}'s part has a row for {target:?} (host {owner}'s) due at {at}, before the cut tick {}",
                cut.id.0,
                cut.tick
            );
        }
        let mine_players: BTreeSet<PlayerId> = cut.players.iter().map(|(p, _)| *p).collect();
        let mine_ids = part_entities(&cut);
        for ((c, other), part) in &self.parts {
            if *c != cut.id {
                continue;
            }
            let theirs: BTreeSet<PlayerId> = part.players.iter().map(|(p, _)| *p).collect();
            let both: Vec<&PlayerId> = mine_players.intersection(&theirs).collect();
            assert!(both.is_empty(), "cut {}: hosts {h} and {other} both write player records {both:?}", cut.id.0);
            let ids = part_entities(part);
            let both: Vec<&EntityId> = mine_ids.intersection(&ids).collect();
            assert!(both.is_empty(), "cut {}: hosts {h} and {other} both write entities {both:?}", cut.id.0);
        }
        self.prepared.push((cut.id, h));
        self.parts.insert((cut.id, h), cut);
        Ok(())
    }

    /// The decision on `cut` (as `decide_cut` / `abort_cut`): the stored one if any (first
    /// writer wins); else a commit only if asked and exactly `hosts` staged parts, an
    /// abort otherwise. Every staged part is then committed or dropped as it says.
    pub fn decide(&mut self, cut: CutId, commit: bool, hosts: &[u32]) -> bool {
        self.decide_settling(cut, commit, hosts, &[])
    }

    /// [`Self::decide`] of a cut that settles `settles` if it commits (as `decide_cut`): a
    /// commit is recorded as an abort unless `settles` is exactly the open successions; the
    /// commit settles them.
    pub fn decide_settling(&mut self, cut: CutId, commit: bool, hosts: &[u32], settles: &[game::SuccessionId]) -> bool {
        if let Some(&stored) = self.decided.get(&cut) {
            return stored;
        }
        let staged: BTreeSet<u32> = self.parts.keys().filter(|(c, _)| *c == cut).map(|(_, h)| *h).collect();
        let listed: BTreeSet<u32> = hosts.iter().copied().collect();
        let open = self.open_successions();
        let named: BTreeSet<game::SuccessionId> = settles.iter().copied().collect();
        let stored = commit && staged == listed && open == named;
        if stored && self.adopting {
            // The pairing the store does not check (Task 9): the heir's part of a cut that
            // settles a succession adopts its predecessor.
            for (s, _) in self.successions.iter().filter(|(s, _)| named.contains(&s.id)) {
                let adopts = self.parts.get(&(cut, s.heir)).map(|p| p.adopts.clone()).unwrap_or_default();
                assert!(
                    adopts.contains(&s.predecessor),
                    "cut {} settles succession {} but heir {}'s part adopts {adopts:?} (seed {})",
                    cut.0,
                    s.id.0,
                    s.heir,
                    self.seed
                );
            }
        }
        if stored {
            for (s, settled) in &mut self.successions {
                if named.contains(&s.id) {
                    *settled = Some(cut);
                }
            }
        }
        self.decided.insert(cut, stored);
        let mut parts = BTreeMap::new();
        for h in staged {
            let part = self.parts.remove(&(cut, h)).expect("staged");
            if stored {
                self.apply_part(h, &part);
            } else {
                self.aborted_parts.push((cut, h, part.clone()));
            }
            parts.insert(h, part);
        }
        if stored {
            self.last_cut = Some((cut, parts));
            self.assert_stored_once(cut);
        }
        stored
    }

    /// The store's `entities` rows are keyed per region (world-store migration 0009), so
    /// nothing in the database refuses one entity stored in two regions by two hosts'
    /// parts: after a committed cut, every entity is in at most one stored region of the
    /// whole world (every host's committed parts together).
    fn assert_stored_once(&self, cut: CutId) {
        let mut at: BTreeMap<EntityId, Vec<(u32, RegionCoords)>> = BTreeMap::new();
        for (h, store) in &self.committed {
            for (rc, region) in &store.regions {
                for b in &region.entities {
                    at.entry(b.id).or_default().push((*h, *rc));
                }
            }
        }
        let twice: Vec<_> = at.into_iter().filter(|(_, places)| places.len() > 1).collect();
        assert!(twice.is_empty(), "cut {}: entities stored in two regions (host, region): {twice:?}", cut.0);
    }

    fn apply_part(&mut self, h: u32, part: &WorldCut) {
        if !part.adopts.is_empty() {
            self.adopts.push((part.id, h, part.adopts.clone()));
        }
        // An heir's part takes its predecessors' stored regions over (rows are keyed per
        // region in the store: they are the heir's land now) and replaces their in-flight
        // rows (what it kept of them is in its own).
        for pred in &part.adopts {
            if let Some(theirs) = self.committed.remove(pred) {
                let mine = self.committed.entry(h).or_default();
                for (rc, region) in theirs.regions {
                    mine.regions.entry(rc).or_insert(region);
                }
            }
        }
        self.committed.entry(h).or_default().apply(part);
        self.committed_players.extend(part.players.iter().cloned());
    }

    /// The last committed cut, or a panic.
    pub fn last_committed(&self) -> (CutId, &BTreeMap<u32, WorldCut>) {
        let (cut, parts) = self.last_cut.as_ref().expect("a committed world cut");
        (*cut, parts)
    }

    /// Where the committed store holds `id`: `(host, region, "state" | "row")`.
    pub fn stored_places(&self, id: EntityId) -> Vec<(u32, RegionCoords, &'static str)> {
        let mut out = Vec::new();
        for (h, store) in &self.committed {
            out.extend(store.places(id).into_iter().map(|(rc, what)| (*h, rc, what)));
        }
        out
    }

    /// Every host's peer journal explains how the running world differs from the last
    /// committed world cut K, and holds nothing K already explains.
    ///
    /// Needed entries: for every pair of hosts (A, B), every tracked entity the store holds
    /// on B that is live on A now (a running region of A, or A's `in_flight` /
    /// `pending_arrivals`) has a `Received` entry from B on A AND a `Sent` entry toward A on
    /// B — unless it went through a third host C (C has a `Received` entry from B and a
    /// `Sent` entry toward A for it); the same for every connected player, by the part that
    /// wrote its record and the host of its body. Returns how many moves were checked
    /// without that exemption.
    ///
    /// No explained entry: every host that took part in K and counts from it has truncated
    /// toward every other participant by both marks of K, and every entry left from before
    /// a mark of its direction travelled wrapped in K. A journal never counts from a cut
    /// later than K.
    pub fn assert_journals_explain_the_cut(&mut self) -> usize {
        use game::{JournalEvent, Subject};
        let Some((cut, _)) = &self.last_cut else { return 0 };
        let cut = *cut;
        let n = self.hosts.len() as u32;
        let (_, parts) = self.last_committed();
        let participants: BTreeSet<u32> = parts.keys().copied().collect();
        // (host, peer, sent?) → subjects.
        let mut journalled: BTreeMap<(u32, u32, bool), BTreeSet<Subject>> = BTreeMap::new();
        for h in 0..n {
            let journal = self.hosts[h as usize].manager.peer_journal();
            let base = journal.base();
            assert!(base <= cut, "host {h}'s journal counts from cut {} past the last committed {}", base.0, cut.0);
            for p in (0..n).filter(|p| *p != h) {
                for (e, _) in journal.entries(p) {
                    let sent = matches!(e.event, JournalEvent::Sent(_));
                    journalled.entry((h, p, sent)).or_default().insert(e.subject);
                }
            }
            // A cut with no other participant (the last host standing after a succession)
            // truncates toward nobody.
            if base != cut || !participants.contains(&h) || participants.len() == 1 {
                continue;
            }
            let Some((marked, marks)) = journal.last_marks() else {
                panic!("host {h} counts from cut {} without having truncated by its marks (seed {})", cut.0, self.seed)
            };
            assert_eq!(*marked, cut, "host {h}'s last truncation was by cut {}, its base is {}", marked.0, cut.0);
            for p in participants.iter().copied().filter(|p| *p != h) {
                let Some(&(Some(out), Some(inn))) = marks.get(&p) else {
                    panic!("host {h} truncated by cut {} without both marks with host {p}: {:?} (seed {})", cut.0, marks.get(&p), self.seed)
                };
                for (e, seq, accounted) in journal.rows(p) {
                    let mark = if matches!(e.event, JournalEvent::Sent(_)) { out } else { inn };
                    assert!(
                        seq >= mark || accounted == Some(cut),
                        "host {h} keeps {:?} with host {p} from before the mark of cut {}, not wrapped in it ({accounted:?}): \
                         the cut explains it (seed {}, link delay {})",
                        e.subject,
                        cut.0,
                        self.seed,
                        self.link_delay
                    );
                }
            }
        }
        let mut moved: Vec<(Subject, u32, u32)> = Vec::new();
        let ids: Vec<EntityId> = self.tracked.keys().copied().collect();
        for id in ids {
            let stored: BTreeSet<u32> = self.stored_places(id).into_iter().map(|(h, _, _)| h).collect();
            let mut live: BTreeSet<u32> = self.owners_of(id).into_iter().map(|(h, _)| h).collect();
            for (h, rig) in self.hosts.iter().enumerate() {
                let m = &rig.manager;
                if m.in_flight_ids().iter().chain(m.pending_arrival_ids().iter()).any(|(i, _)| *i == id) {
                    live.insert(h as u32);
                }
            }
            for &b in &stored {
                moved.extend(live.iter().filter(|a| **a != b).map(|a| (Subject::Entity(id), b, *a)));
            }
        }
        let (_, parts) = self.last_committed();
        let players: Vec<(ClientId, PlayerId, Vec<u32>)> = self
            .players
            .iter()
            .map(|(c, p)| (*c, *p, parts.iter().filter(|(_, part)| part.players.iter().any(|(q, _)| q == p)).map(|(h, _)| *h).collect()))
            .collect();
        for (client, player, writers) in players {
            if let Some((a, _, _)) = self.player_body(client) {
                moved.extend(writers.into_iter().filter(|b| *b != a).map(|b| (Subject::Player(player), b, a)));
            }
        }
        let has = |key: (u32, u32, bool), s: &Subject| journalled.get(&key).is_some_and(|set| set.contains(s));
        let mut checked = 0;
        for (subject, b, a) in moved {
            let through_third = (0..n).any(|c| c != a && c != b && has((c, b, false), &subject) && has((c, a, true), &subject));
            if through_third {
                continue;
            }
            checked += 1;
            assert!(
                has((a, b, false), &subject),
                "{subject:?}: stored on host {b} by cut {}, now on host {a}, which has no Received entry from {b} \
                 (seed {}, link delay {})",
                cut.0,
                self.seed,
                self.link_delay
            );
            assert!(
                has((b, a, true), &subject),
                "{subject:?}: stored on host {b} by cut {}, now on host {a}; host {b} has no Sent entry toward {a} \
                 (seed {}, link delay {})",
                cut.0,
                self.seed,
                self.link_delay
            );
        }
        checked
    }

    /// The parts of the last committed cut that write `player`'s record.
    pub fn last_cut_writers(&self, player: PlayerId) -> Vec<u32> {
        let (_, parts) = self.last_committed();
        parts.iter().filter(|(_, p)| p.players.iter().any(|(q, _)| *q == player)).map(|(h, _)| *h).collect()
    }

    /// Every host answers ownership from a `PgClaim` directory holding exactly `rows`
    /// (`(super-region, owner, epoch)`; owner `None` = a free row) from now on: a
    /// super-region with no row is `Unknown` (the directory is never read whole). The rows
    /// must give every host its keeper's home as `Static` did. Kept by
    /// [`Self::rebuild_from_last_cut`].
    pub fn with_directory(&mut self, rows: Vec<(game::SuperRegion, Option<u32>, u64)>) {
        let hosts = self.hosts.len() as u32;
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            let mut claim = game::PgClaim::new(h as u32, hosts);
            let directory = game::Ownership::directory_mut(&mut claim).expect("a directory");
            for (sr, owner, epoch) in &rows {
                directory.set(*sr, *owner, *epoch);
            }
            rig.manager.set_ownership(Box::new(claim), self.s);
            assert!(rig.manager.owns(self.homes[h]), "the rows give host {h} its keeper's home");
        }
        self.directory = Some(rows);
    }

    /// From now on every host claims on demand against the rig's `super_regions` table, made
    /// from the rows [`Self::with_directory`] gave (call it first): each host reads it whole
    /// (every host live), and every round ([`Self::drive_claims`]) a host's
    /// `take_claims_wanted` is answered as `claim_free` answers it — after 0..=`link_delay`
    /// rounds (seeded) — with `claimed`, and a directory read when it won less than it asked
    /// or asked for one (`take_directory_wanted`).
    pub fn with_claims(&mut self) {
        let rows = self.directory.clone().expect("with_directory first");
        self.claim_store = Some(rows.into_iter().map(|(sr, owner, epoch)| (sr, (owner, epoch))).collect());
        for h in 0..self.hosts.len() as u32 {
            self.read_directory(h);
        }
    }

    /// Host `h` reads the rig's `super_regions` table whole, every host live.
    fn read_directory(&mut self, h: u32) {
        let store = self.claim_store.as_ref().expect("with_claims");
        let rows: Vec<(game::SuperRegion, i64, u64)> =
            store.iter().map(|(sr, (owner, epoch))| (*sr, owner.map_or(game::NO_OWNER, i64::from), *epoch)).collect();
        let live: BTreeSet<u32> = (0..self.hosts.len() as u32).filter(|h| !self.dead.contains(h)).collect();
        self.hosts[h as usize].manager.directory_read(rows, live);
    }

    /// One round of every host's claim loop (as the server's): directory reads asked for,
    /// claims asked for (one in flight per host), claims due answered.
    pub fn drive_claims(&mut self) {
        if self.claim_store.is_none() {
            return;
        }
        self.rounds += 1;
        for h in 0..self.hosts.len() as u32 {
            if self.hosts[h as usize].manager.take_directory_wanted() {
                self.read_directory(h);
            }
            if self.claims_pending.iter().any(|(_, p, _)| *p == h) {
                continue;
            }
            let wanted = self.hosts[h as usize].manager.take_claims_wanted();
            if !wanted.is_empty() {
                let delay = (mix(self.seed, self.rounds * 64 + u64::from(h)) % u64::from(self.link_delay + 1)) as u32;
                self.claims_pending.push((delay, h, wanted));
            }
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.claims_pending).into_iter().partition(|(d, _, _)| *d == 0);
        self.claims_pending = later.into_iter().map(|(d, h, srs)| (d - 1, h, srs)).collect();
        for (_, h, srs) in due {
            let store = self.claim_store.as_mut().expect("with_claims");
            let mut won = Vec::new();
            for sr in &srs {
                match store.get(sr).copied() {
                    None => {
                        store.insert(*sr, (Some(h), 1));
                        won.push((*sr, 1));
                    }
                    Some((None, epoch)) => {
                        store.insert(*sr, (Some(h), epoch + 1));
                        won.push((*sr, epoch + 1));
                    }
                    Some((Some(_), _)) => {}
                }
            }
            let short = won.len() < srs.len();
            if !won.is_empty() {
                self.hosts[h as usize].manager.claimed(won);
            }
            if short {
                self.read_directory(h);
            }
        }
        for rig in &mut self.hosts {
            rig.settle();
        }
    }

    /// The owner of `sr` in the rig's `super_regions` table ([`Self::with_claims`]).
    pub fn stored_owner(&self, sr: game::SuperRegion) -> Option<u32> {
        self.claim_store.as_ref().and_then(|s| s.get(&sr)).and_then(|(owner, _)| *owner)
    }

    /// Host `h` alone answers from a `PgClaim` directory read whole from `rows`
    /// (`(super-region, owner, epoch)`), every host live; the others keep theirs.
    pub fn set_directory(&mut self, h: u32, rows: Vec<(game::SuperRegion, Option<u32>, u64)>) {
        let hosts = self.hosts.len() as u32;
        let m = &mut self.hosts[h as usize].manager;
        m.set_ownership(Box::new(game::PgClaim::new(h, hosts)), self.s);
        let rows = rows.into_iter().map(|(sr, owner, epoch)| (sr, owner.map_or(game::NO_OWNER, i64::from), epoch)).collect();
        m.directory_read(rows, (0..hosts).collect());
        assert!(m.owns(self.homes[h as usize]), "the rows give host {h} its keeper's home");
    }

    /// The world restarted from the store: every host restored from its committed parts
    /// (its regions from the stored ones, its in-flight rows put back on their way),
    /// lockstep, no link delay, each keeper back home.
    pub fn rebuild_from_last_cut(&self) -> MultiRig {
        let regions: BTreeMap<RegionCoords, StoredRegion> =
            self.committed.values().flat_map(|s| s.regions.iter().map(|(rc, r)| (*rc, r.clone()))).collect();
        let hosts = self.hosts.len() as u32;
        let mut rig = MultiRig::new_from(hosts, self.s, None, 0, 0, Some(regions));
        if let Some(rows) = &self.directory {
            rig.with_directory(rows.clone());
        }
        for (h, store) in &self.committed {
            rig.hosts[*h as usize].manager.restore_in_flight(store.in_flight.clone());
        }
        rig
    }

    /// Host `h` dies: its manager and spawner are dropped (an empty placeholder keeps its
    /// slot and the world tick), its peer links go down for everyone (what is in transit is
    /// lost), the coordinator sees its link drop, and its parts staged in the rig store stay.
    /// What it held, and what was on its way to or from it, is no longer tracked
    /// ([`Self::forgiven`]): its regions come back from the store with the heir (Task 9).
    pub fn kill(&mut self, h: u32) {
        assert!(!self.dead.contains(&h), "host {h} is dead already");
        let n = self.hosts.len() as u32;
        let mut gone: BTreeSet<EntityId> = BTreeSet::new();
        for (id, last) in &self.tracked {
            let on_h = match last {
                Some(Place::Region { host, .. } | Place::Manager { host } | Place::Parked { host, .. } | Place::Unowned { host, .. }) => {
                    *host == h
                }
                Some(Place::Unsent { host, peer }) => *host == h || *peer == h,
                Some(Place::Transit { to }) => *to == h,
                None => false,
            };
            if on_h {
                gone.insert(*id);
            }
        }
        for (r, rig) in self.hosts.iter().enumerate() {
            for (peer, id, _) in rig.manager.peer_unreceived_ids() {
                if r as u32 == h || peer == h {
                    gone.insert(id);
                }
            }
        }
        for (&(a, b), (ea, eb)) in &self.links {
            if a != h && b != h {
                continue;
            }
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body, .. } = &msg {
                        if let Reliable::Transfer { bundle, .. } = body.inner() {
                            gone.insert(bundle.id);
                        }
                    }
                }
            }
        }
        for id in &gone {
            self.tracked.remove(id);
        }
        self.forgiven.extend(gone);
        for other in (0..n).filter(|o| *o != h) {
            self.link_down(h, other);
        }
        if self.coordinator.is_some() {
            self.core().host_detached(h);
        }
        let tick = self.hosts[h as usize].manager.spawner_mut().world_tick();
        let now = self.hosts[h as usize].now_ms;
        let mut spawner = InlineSpawner::default();
        spawner.set_world_tick(tick);
        let mut placeholder = Rig::with_spawner(spawner, super::seam_translate::uniform_floor);
        placeholder.now_ms = now;
        self.hosts[h as usize] = placeholder;
        self.dead.insert(h);
    }

    /// A dead host `h` starts again as a new boot ([`Self::start_as`]): its boot.
    pub fn start(&mut self, h: u32) -> u64 {
        let boot = self.boots[h as usize] + 1_000_000;
        self.start_as(h, boot);
        boot
    }

    /// A dead host `h` starts again as process `boot` (its dead boot again: a zombie of it):
    /// a fresh manager on the rig store's committed state — its regions as stored, its
    /// journal from the last committed cut, a `PgClaim` directory read from the rig's
    /// `super_regions` table — with a new link to every live host (whose sessions with `h`
    /// are kept, only the link swapped) and to the coordinator.
    pub fn start_as(&mut self, h: u32, boot: u64) {
        assert!(self.dead.contains(&h), "host {h} runs");
        let n = self.hosts.len() as u32;
        let regions: BTreeMap<RegionCoords, StoredRegion> =
            self.committed.values().flat_map(|s| s.regions.iter().map(|(rc, r)| (*rc, r.clone()))).collect();
        let tick = self.hosts[h as usize].manager.spawner_mut().world_tick();
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let mut spawner = InlineSpawner::with_portable(regions);
        spawner.set_world_tick(tick);
        if let Some(f) = self.schedule {
            spawner.set_schedule(Some(f(mix(self.seed, u64::from(h) ^ boot))));
        }
        let rows: Vec<(game::SuperRegion, Option<u32>, u64)> = match (&self.claim_store, &self.directory) {
            (Some(store), _) => store.iter().map(|(sr, (o, e))| (*sr, *o, *e)).collect(),
            (None, Some(rows)) => rows.clone(),
            (None, None) => panic!("a host starts again only under `claim` (with_directory)"),
        };
        let mut rig = Rig::with_ownership(spawner, super::seam_translate::uniform_floor, Box::new(game::PgClaim::new(h, n)), self.s);
        rig.now_ms = now;
        rig.manager.set_endpoints(rig_endpoint(h), game::seeded_nonces(mix(self.seed, 2000 + u64::from(h) + boot)));
        let base = self.last_cut.as_ref().map_or(CutId(0), |(c, _)| *c);
        rig.manager.start_peer_journal(base);
        let live: BTreeSet<u32> = (0..n).filter(|o| !self.dead.contains(o) || *o == h).collect();
        let rows = rows.into_iter().map(|(sr, o, e)| (sr, o.map_or(game::NO_OWNER, i64::from), e)).collect();
        rig.manager.directory_read(rows, live);
        self.hosts[h as usize] = rig;
        self.dead.remove(&h);
        self.boots[h as usize] = boot;
        for other in (0..n).filter(|o| *o != h && !self.dead.contains(o)) {
            let (a, b) = (h.min(other), h.max(other));
            let (ea, eb) = InlinePeerLink::pair(mix(self.seed, boot ^ u64::from(a * n + b)), self.link_delay);
            let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
            let (sa, sb) = (self.strips[&(a, b)].clone(), self.strips[&(b, a)].clone());
            let (ba, bb) = (self.boots[a as usize], self.boots[b as usize]);
            self.hosts[a as usize].manager.set_peer(b, PeerSession::new(rig_hello_as(a, ba), b), Box::new(SharedEnd(ea.clone(), sa)));
            self.hosts[b as usize].manager.set_peer(a, PeerSession::new(rig_hello_as(b, bb), a), Box::new(SharedEnd(eb.clone(), sb)));
            self.links.insert((a, b), (ea, eb));
        }
        if let Some(core) = self.coordinator.clone() {
            let inner = game::InlineWorldLink::shared(core, h).with_delay(mix(self.seed, 3000 + u64::from(h) ^ boot), self.link_delay);
            let sent = Arc::new(Mutex::new(Vec::new()));
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let hop_is = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            self.world_sent[h as usize] = sent.clone();
            self.world_hold[h as usize] = hold.clone();
            self.world_hop_is[h as usize] = hop_is.clone();
            let rig = &mut self.hosts[h as usize];
            rig.manager.set_world_link(Box::new(RigWorldLink { inner, sent, hold, hop_is }), world_boot(boot), now);
            rig.settle();
        }
    }

    /// The rig coordinator declares host `dead` (killed, [`Self::kill`]) dead as the
    /// coordinator runtime would: its lease watch reads every live host beating and `dead`'s
    /// row vouched (`seen_ms = 0`), [`game::Coordinator::declare_wanted`] is asked (cut
    /// rounds while a running cut is first decided aborted), the batch is declared in the
    /// rig store ([`Self::declare_in_store`]) and the store's answer handed back — the orders
    /// go out to the survivors. The successions declared.
    pub fn run_succession(&mut self, dead: u32) -> Vec<game::Succession> {
        self.run_successions(&[dead])
    }

    /// [`Self::run_succession`] of several killed hosts at once: every one of them vouched
    /// dead in the same lease reads, declared in one batch.
    pub fn run_successions(&mut self, deads: &[u32]) -> Vec<game::Succession> {
        let n = self.hosts.len() as u32;
        self.core().set_crash_policy(game::CrashPolicy::Succession);
        let declared_dead: BTreeSet<(u32, u64)> =
            self.successions.iter().map(|(s, _)| (s.predecessor, s.predecessor_boot)).collect();
        let mut d = None;
        for r in 0..SUCCESSION_DECLARE_ROUNDS {
            let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
            let rows: Vec<game::coordinator::HostSeen> = (0..n)
                .filter(|h| !self.dead.contains(h) || deads.contains(h))
                .map(|h| {
                    let boot = self.boots[h as usize];
                    let state = if declared_dead.contains(&(h, boot)) { game::HOST_DEAD } else { game::HOST_LIVE };
                    // Every live host beats every round; the dead ones are vouched.
                    let seen_ms = if deads.contains(&h) { 0 } else { now + 1 };
                    game::coordinator::HostSeen { ordinal: h, boot, clean: false, state, seen_ms }
                })
                .collect();
            let mut core = self.core();
            core.observe_leases(&rows, now, now);
            d = core.declare_wanted(now);
            drop(core);
            if d.is_some() {
                break;
            }
            assert!(r + 1 < SUCCESSION_DECLARE_ROUNDS, "hosts {deads:?} were never declared (seed {})", self.seed);
            self.cut_round();
        }
        let d = d.expect("declared");
        let outcome = self.declare_in_store(&d);
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let live: BTreeSet<u32> = (0..n).filter(|h| !self.dead.contains(h) && !d.dead.iter().any(|(p, _)| p == h)).collect();
        let mut core = self.core();
        core.declared(outcome.clone(), now);
        // The predecessor's row is dead: not expected by world cuts any more.
        core.set_expected_hosts(live);
        drop(core);
        match outcome {
            game::Declared::Done(list) => list,
            other => panic!("the rig store refused the declaration: {other:?}"),
        }
    }

    /// The rig store's `declare_succession`: one succession per predecessor of `d`, each
    /// moving every super-region of the predecessor to its heir at `epoch + 1`, from the
    /// last committed cut, its survivors every live host but the predecessors.
    /// `SuccessionOpen` while one is open.
    pub fn declare_in_store(&mut self, d: &game::coordinator::Declare) -> game::Declared {
        if !self.open_successions().is_empty() {
            return game::Declared::SuccessionOpen;
        }
        let n = self.hosts.len() as u32;
        let base_cut = self.last_cut.as_ref().map_or(CutId(0), |(c, _)| *c);
        let predecessors: BTreeSet<u32> = d.dead.iter().map(|(p, _)| *p).collect();
        let survivors: Vec<u32> = (0..n).filter(|h| !self.dead.contains(h) && !predecessors.contains(h)).collect();
        let store = self.claim_store.as_mut().expect("with_claims: the rig's super_regions table");
        let mut out = Vec::new();
        for (&(predecessor, predecessor_boot), &heir) in d.dead.iter().zip(&d.heirs) {
            let mut moved = Vec::new();
            for (sr, (owner, epoch)) in store.iter_mut() {
                if *owner == Some(predecessor) {
                    *owner = Some(heir);
                    *epoch += 1;
                    moved.push((*sr, *epoch));
                }
            }
            let id = game::SuccessionId(self.successions.len() as u64 + out.len() as u64 + 1);
            out.push(game::Succession {
                id,
                predecessor,
                predecessor_boot,
                heir,
                kind: d.kind,
                base_cut,
                moved,
                survivors: survivors.clone(),
            });
        }
        self.successions.extend(out.iter().cloned().map(|s| (s, None)));
        game::Declared::Done(out)
    }

    /// The rig store's open successions (neither settled nor abandoned).
    pub fn open_successions(&self) -> BTreeSet<game::SuccessionId> {
        self.successions
            .iter()
            .filter(|(s, settled)| settled.is_none() && !self.abandoned.contains(&s.id))
            .map(|(s, _)| s.id)
            .collect()
    }

    /// From now on the rig answers every heir's adoption read as the server does (Task 9):
    /// [`game::WorldManager::take_adoption_wanted`] is answered after 0..=`link_delay` rounds
    /// (seeded) with the predecessor's in-flight rows of the last committed cut
    /// (`load_in_flight_of`) and the stored regions of the drop set (`regions_of_entities`),
    /// the predecessor's stored regions handed to the heir's spawner first (what its
    /// region threads would read from the store).
    pub fn with_adoption(&mut self) {
        self.adopting = true;
    }

    /// One round of every heir's adoption reads ([`Self::with_adoption`]).
    pub fn drive_adoptions(&mut self) {
        if !self.adopting {
            return;
        }
        for h in 0..self.hosts.len() as u32 {
            if self.dead.contains(&h) {
                continue;
            }
            while let Some((id, pred)) = self.hosts[h as usize].manager.take_adoption_wanted() {
                let delay = (mix(self.seed, self.rounds * 64 + 7000 + u64::from(h)) % u64::from(self.link_delay + 1)) as u32;
                self.adoptions_pending.push((delay, h, id, pred));
            }
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.adoptions_pending).into_iter().partition(|(d, ..)| *d == 0);
        self.adoptions_pending = later.into_iter().map(|(d, h, id, pred)| (d - 1, h, id, pred)).collect();
        for (_, h, id, pred) in due {
            if self.dead.contains(&h) {
                continue;
            }
            let theirs = self.committed.get(&pred);
            let rows = theirs.map(|s| s.in_flight.clone()).unwrap_or_default();
            let regions: BTreeMap<RegionCoords, StoredRegion> = theirs.map(|s| s.regions.clone()).unwrap_or_default();
            let drop: BTreeSet<EntityId> = self.hosts[h as usize].manager.adoption_drop_set(id).into_iter().collect();
            let mut located = Vec::new();
            for id in &drop {
                for store in self.committed.values() {
                    for (rc, region) in &store.regions {
                        if region.entities.iter().any(|b| b.id == *id) {
                            located.push((*id, *rc));
                        }
                    }
                }
            }
            let m = &mut self.hosts[h as usize].manager;
            if self.fail_adoption_loads {
                for rc in regions.keys() {
                    m.spawner_mut().fail_next_start(*rc);
                }
            }
            m.spawner_mut().add_portable(regions);
            m.adopted_rows(id, rows, located);
            self.adopted.push((id, h));
            self.hosts[h as usize].settle();
        }
    }

    /// Host `dead` (killed after a committed leave cut it left in) is declared dead as a
    /// LEAVE succession ([`game::SuccessionKind::Leave`]): the rig store moves its land to
    /// its heir and the coordinator sends the orders. The succession declared.
    pub fn declare_leave(&mut self, dead: u32) -> game::Succession {
        let n = self.hosts.len() as u32;
        self.core().set_crash_policy(game::CrashPolicy::Succession);
        let live: BTreeSet<u32> = (0..n).filter(|h| !self.dead.contains(h) && *h != dead).collect();
        let heir = game::heir_of(dead, &live).expect("a live host");
        let d = game::coordinator::Declare {
            dead: vec![(dead, self.boots[dead as usize])],
            heirs: vec![heir],
            kind: game::SuccessionKind::Leave,
        };
        let outcome = self.declare_in_store(&d);
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let mut core = self.core();
        core.declared(outcome.clone(), now);
        core.set_expected_hosts(live);
        drop(core);
        match outcome {
            game::Declared::Done(mut list) => list.remove(0),
            other => panic!("the rig store refused the declaration: {other:?}"),
        }
    }

    /// Host `dead` (killed while a succession is open) lapses: the coordinator wants the
    /// world rolled back (cut rounds while it notices), the rig store abandons every open
    /// succession and raises the world generation, and the coordinator tells every host
    /// ([`game::CoordMsg::Generation`]). The new generation.
    pub fn roll_back(&mut self, dead: u32) -> u64 {
        let n = self.hosts.len() as u32;
        for r in 0..SUCCESSION_DECLARE_ROUNDS {
            let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
            let rows: Vec<game::coordinator::HostSeen> = (0..n)
                .filter(|h| !self.dead.contains(h) || *h == dead)
                .map(|h| {
                    let seen_ms = if h == dead { 0 } else { now + 1 };
                    game::coordinator::HostSeen { ordinal: h, boot: self.boots[h as usize], clean: false, state: game::HOST_LIVE, seen_ms }
                })
                .collect();
            let mut core = self.core();
            core.observe_leases(&rows, now, now);
            core.maintain(now);
            if core.rollback_wanted() {
                break;
            }
            drop(core);
            assert!(r + 1 < SUCCESSION_DECLARE_ROUNDS, "the coordinator never wanted a rollback (seed {})", self.seed);
            self.cut_round();
        }
        let open = self.open_successions();
        self.abandoned.extend(open);
        self.generation += 1;
        let generation = self.generation;
        self.core().rolled_back(generation);
        generation
    }

    /// The rig's coordinator, locked ([`Self::with_coordinator`]).
    pub fn core(&self) -> std::sync::MutexGuard<'_, game::coordinator::SharedCore> {
        self.coordinator.as_ref().expect("a rig coordinator").lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold back (`true`) or release what the coordinator sends host `h`.
    pub fn hold_world(&mut self, h: u32, hold: bool) {
        self.world_hold[h as usize].store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    /// What host `h` sent its coordinator so far.
    /// `HopIs` messages every host read from the coordinator.
    pub fn hop_is_received(&self) -> usize {
        self.world_hop_is.iter().map(|n| n.load(std::sync::atomic::Ordering::SeqCst)).sum()
    }

    pub fn sent_to_coordinator(&self, h: u32) -> Vec<game::HostMsg> {
        self.world_sent[h as usize].lock().unwrap().clone()
    }

    /// Messages the hosts sent the coordinator that it has not handled yet (its links'
    /// delay).
    pub fn coordinator_pending(&self) -> Vec<game::HostMsg> {
        self.hosts
            .iter()
            .filter_map(|r| r.manager.world_link().and_then(|l| l.as_inline()))
            .flat_map(|l| l.pending().cloned().collect::<Vec<_>>())
            .collect()
    }

    /// Host `h`'s keeper: a client whose body stands in the middle of its home region.
    fn keep(&mut self, h: u32) {
        let home = self.homes[h as usize];
        let y = super::standing_y(super::FLOOR_TOP);
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: home,
            fortress: None,
            adventure: None,
        };
        let rig = &mut self.hosts[h as usize];
        rig.event(ServerEvent::ClientConnected { client: CLIENT, player: PlayerId(u64::from(h)), record: Some(record) });
    }

    /// `client` (session id) of `player` connects to host `home` as its home session,
    /// restored from `record`; tracked from now on.
    pub fn connect_home(&mut self, home: u32, client: ClientId, player: PlayerId, record: PlayerRecord) {
        self.players.insert(client, player);
        let rig = &mut self.hosts[home as usize];
        rig.event(ServerEvent::ClientConnected { client, player, record: Some(record) });
        // Its body, if the restore already gave it one, is tracked from now on.
        self.assert_conserved_world();
    }

    /// [`Self::connect_home`], and an observer session of the same client on every other
    /// host (what a client holds once its home host redirected it there).
    pub fn connect_everywhere(&mut self, home: u32, client: ClientId, player: PlayerId, record: PlayerRecord) {
        self.connect_home(home, client, player, record);
        for h in 0..self.hosts.len() as u32 {
            if h != home {
                self.hosts[h as usize].event(ServerEvent::ObserverConnected { client, player });
            }
        }
    }

    /// `client` of `player` connects to host `h` with a login ticket of (`token`, `epoch`)
    /// ([`ServerEvent::LoginConnected`]), restored from `record`; tracked from now on.
    pub fn connect_login(
        &mut self,
        h: u32,
        client: ClientId,
        player: PlayerId,
        record: Option<PlayerRecord>,
        token: &str,
        epoch: u64,
    ) {
        self.players.insert(client, player);
        let ev = ServerEvent::LoginConnected { client, player, record, player_token: token.into(), session_epoch: epoch };
        self.hosts[h as usize].event(ev);
        self.assert_conserved_world();
    }

    /// Every host's server half of the login relay, as `server::login_link` plays it: a
    /// spawn order a host took is recorded — the player's sessions there superseded first
    /// (`Supersede`) — and answered ok; a kick another host relayed is applied and
    /// answered. The orders taken, with the host that took each.
    pub fn serve_logins(&mut self) -> Vec<(u32, game::SpawnRequest)> {
        let mut taken = Vec::new();
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for order in rig.manager.take_spawn_here() {
                rig.event(ServerEvent::Supersede { player: order.player, reason: game::LOGGED_IN_ELSEWHERE.into() });
                taken.push((h as u32, order.clone()));
                rig.manager.spawn_answered(order, true);
            }
            for kick in rig.manager.take_kicks_in() {
                rig.manager.kick_applied(kick, true);
            }
        }
        taken
    }

    /// `client`'s connection to host `h` ends.
    pub fn disconnect(&mut self, h: u32, client: ClientId) {
        self.hosts[h as usize].event(ServerEvent::ClientDisconnected(client));
    }

    /// Every host records the packets its manager sends ([`Rig::packets_seen`]).
    pub fn record_packets(&mut self) {
        for rig in &mut self.hosts {
            rig.packets_seen = Some(Vec::new());
        }
    }

    /// Every host's clock jumps `ms` ahead and its upkeep runs ([`game::WorldManager::maintain`]).
    pub fn maintain_all(&mut self, ms: u64) {
        for rig in &mut self.hosts {
            rig.now_ms += ms;
            let now = rig.now_ms;
            rig.manager.maintain(now);
            rig.settle();
        }
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    pub fn player_owners(&mut self, client: ClientId) -> Vec<(u32, RegionCoords)> {
        let mut out = Vec::new();
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    if region.data().player_entites.contains_key(&client) {
                        out.push((h as u32, rc));
                    }
                });
            }
        }
        out
    }

    /// The `EntityId` of `client`'s body, if a running region holds it.
    pub fn player_body(&mut self, client: ClientId) -> Option<(u32, RegionCoords, EntityId)> {
        let mut out = None;
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    let data = region.data();
                    if let Some(&e) = data.player_entites.get(&client) {
                        if let Some(id) = *data.ecs.id.try_get(e) {
                            out = Some((h as u32, rc, id));
                        }
                    }
                });
            }
        }
        out
    }

    /// The hosts holding `player`'s parked record.
    pub fn parked_on(&self, player: PlayerId) -> Vec<u32> {
        (0..self.hosts.len() as u32).filter(|h| self.hosts[*h as usize].manager.parked_record(player).is_some()).collect()
    }

    /// Rounds until every host reaches every other host's home region.
    pub fn connect(&mut self) {
        for _ in 0..CONNECT_ROUNDS {
            if self.all_reachable() {
                return;
            }
            self.round();
        }
        panic!("the peer sessions never all came up");
    }

    fn all_reachable(&self) -> bool {
        self.hosts.iter().all(|r| self.homes.iter().all(|home| r.manager.reaches(*home)))
    }

    /// The host that owns `rc`.
    pub fn owner(&self, rc: RegionCoords) -> u32 {
        let alive = (0..self.hosts.len()).find(|h| !self.dead.contains(&(*h as u32))).expect("a live host");
        self.hosts[alive].manager.owner_host(rc).expect("an owned region")
    }

    /// The rig of the host that owns `rc`.
    pub fn host_of(&mut self, rc: RegionCoords) -> &mut Rig {
        let h = self.owner(rc);
        &mut self.hosts[h as usize]
    }

    /// Keep `rc` running on its owner (its keeper subscribes to it).
    pub fn run(&mut self, rc: RegionCoords) {
        self.host_of(rc).event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
        assert!(self.host_of(rc).manager.running_regions().contains(&rc), "{rc:?} runs on its owner");
    }

    /// Let `rc` go: its owner's keeper unsubscribes, and after the grace it parks.
    pub fn park(&mut self, rc: RegionCoords) {
        let rig = self.host_of(rc);
        rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(rc), CLIENT));
        rig.now_ms += game::UNLOAD_GRACE_MS;
        let now = rig.now_ms;
        rig.manager.maintain(now);
        rig.settle();
        for _ in 0..4 {
            if self.host_of(rc).manager.parked_regions().contains(&rc) {
                break;
            }
            self.round();
        }
        assert!(self.host_of(rc).manager.parked_regions().contains(&rc), "{rc:?} parked");
        // Every host's clock moves on together.
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        for r in &mut self.hosts {
            r.now_ms = now;
        }
    }

    /// Take the link between hosts `a` and `b` down (what is in transit is lost).
    pub fn link_down(&mut self, a: u32, b: u32) {
        self.link(a, b).lock().unwrap().set_up(false);
    }

    /// Bring it back, as a new connection.
    pub fn link_up(&mut self, a: u32, b: u32) {
        self.link(a, b).lock().unwrap().set_up(true);
    }

    /// Give hosts `a` and `b` a fresh link pair (its `connection()` the same as a fresh
    /// pair's: 1): `set_peer` on both with a new session each — the managers keep their
    /// sessions and swap only the link. What was in transit on the old link is lost.
    pub fn replace_link(&mut self, a: u32, b: u32, seed: u64, link_delay_steps: u32) {
        let (a, b) = (a.min(b), a.max(b));
        let (ea, eb) = InlinePeerLink::pair(seed, link_delay_steps);
        let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
        let sa = self.strips[&(a, b)].clone();
        let sb = self.strips[&(b, a)].clone();
        self.hosts[a as usize].manager.set_peer(b, PeerSession::new(rig_hello(a), b), Box::new(SharedEnd(ea.clone(), sa)));
        self.hosts[b as usize].manager.set_peer(a, PeerSession::new(rig_hello(b), a), Box::new(SharedEnd(eb.clone(), sb)));
        self.links.insert((a, b), (ea, eb));
    }

    fn link(&self, a: u32, b: u32) -> &Arc<Mutex<InlinePeerLink>> {
        &self.links[&(a.min(b), a.max(b))].0
    }

    /// One round: every host ticks (their world ticks stay equal), its region outputs
    /// are handled, every peer session is pumped, the arrivals that brought are
    /// handled, then [`Self::assert_conserved_world`].
    pub fn round(&mut self) {
        for rig in &mut self.hosts {
            rig.now_ms += TICK_RATE;
            rig.manager.spawner_mut().tick_all();
        }
        let ticks: Vec<u64> = self.hosts.iter_mut().map(|r| r.manager.spawner_mut().world_tick()).collect();
        assert!(ticks.windows(2).all(|w| w[0] == w[1]), "the hosts' world ticks drifted apart: {ticks:?}");
        for rig in &mut self.hosts {
            rig.settle();
        }
        for rig in &mut self.hosts {
            let now = rig.now_ms;
            rig.manager.pump_peers(now);
        }
        for rig in &mut self.hosts {
            rig.settle();
        }
        self.drive_claims();
        self.drive_adoptions();
        self.assert_conserved_world();
    }

    /// Spawn a size-1 slime in `rc` (on its owner) at `pos` (`rc`'s frame); its id.
    pub fn spawn_slime(&mut self, rc: RegionCoords, pos: [f32; 3]) -> EntityId {
        let mut id = None;
        self.host_of(rc).manager.spawner_mut().with_region(rc, |region| {
            let tick = region.current_tick();
            let e = super::spawn_slime(region, pos, 1, 0.0, tick);
            id = *region.data().ecs.id.try_get(e);
        });
        id.expect("the slime has an EntityId")
    }

    /// Move entity `id`, owned by running region `rc`, to `pos` (`rc`'s frame): a test
    /// teleport. `false` if `rc` does not own it.
    pub fn teleport(&mut self, rc: RegionCoords, id: EntityId, pos: [f32; 3]) -> bool {
        let mut done = false;
        self.host_of(rc).manager.spawner_mut().with_region(rc, |region| {
            let Some(&key) = region.data().id_index.get(&id) else { return };
            region.with_data(|d| {
                let at = game::rapier3d::math::Vector::new(pos[0], pos[1], pos[2]);
                d.set_body_pose_safe(key, game::Pose::from_translation(at));
            });
            done = true;
        });
        done
    }

    /// Where `id` is owned now: `(host, region)` of every running region holding it.
    pub fn owners_of(&mut self, id: EntityId) -> Vec<(u32, RegionCoords)> {
        let mut out = Vec::new();
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    if region.data().id_index.contains_key(&id) {
                        out.push((h as u32, rc));
                    }
                });
            }
        }
        out
    }

    /// Reliable messages not acknowledged yet, over every host.
    pub fn peer_unreceived(&self) -> usize {
        self.hosts.iter().map(|r| r.manager.peer_unreceived()).sum()
    }

    /// Conservation across hosts. Every tracked `EntityId` (people aside, whom a history
    /// may absorb, arrivals a full mailbox despawned, and voxel bodies that settled where
    /// they are owned) is in exactly one of: a running region of one host, one host's
    /// `in_flight` / `pending_arrivals`, one session's never-sent part, or a link's
    /// transit queue. A transfer a session SENT and the peer has not acknowledged is the
    /// acknowledgement lag, not a copy: it may stand beside the entity's one place (the
    /// peer holds it, or it is in transit, or has sent it on unsent), or alone (lost
    /// with a connection, to be sent again) — but two places are never allowed, and an
    /// entity in neither a place nor a sent transfer has vanished.
    pub fn assert_conserved_world(&mut self) {
        let mut places: BTreeMap<EntityId, Vec<Place>> = BTreeMap::new();
        let mut shadows: BTreeSet<EntityId> = BTreeSet::new();
        let mut people = BTreeSet::new();
        let mut settling = BTreeSet::new();
        let tracked_players = &self.players;
        let player_bodies = &mut self.player_bodies;
        let parked_hold = self.parked_hold;
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            let host = h as u32;
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    let data = region.data();
                    for (client, &key) in data.player_entites.iter() {
                        if let (Some(player), Some(id)) = (tracked_players.get(client), *data.ecs.id.try_get(key)) {
                            player_bodies.insert(id, *player);
                        }
                    }
                    for (id, &key) in data.id_index.iter() {
                        places.entry(*id).or_default().push(Place::Region { host, rc });
                        if matches!(data.ecs.kind.try_get(key), Some(game::EntityKind::Person { .. })) {
                            people.insert(*id);
                        }
                        if data.ecs.voxel_body.try_get(key).is_some() {
                            settling.insert(*id);
                        }
                    }
                });
            }
            for (id, _) in rig.manager.in_flight_ids().into_iter().chain(rig.manager.pending_arrival_ids()) {
                places.entry(id).or_default().push(Place::Manager { host });
            }
            if parked_hold {
                for rc in rig.manager.parked_regions() {
                    for id in rig.manager.parked_entity_ids(rc).expect("a parked region's state decodes") {
                        places.entry(id).or_default().push(Place::Parked { host, rc });
                    }
                }
            }
            for (sr, id) in rig.manager.unowned_waiting_ids() {
                places.entry(id).or_default().push(Place::Unowned { host, sr });
            }
            for (peer, id, sent) in rig.manager.peer_unreceived_ids() {
                if sent {
                    shadows.insert(id);
                } else {
                    places.entry(id).or_default().push(Place::Unsent { host, peer });
                }
            }
        }
        for (&(a, b), (ea, eb)) in &self.links {
            for (to, end) in [(a, ea), (b, eb)] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body, .. } = &msg {
                        let Reliable::Transfer { bundle, .. } = body.inner() else { continue };
                        places.entry(bundle.id).or_default().push(Place::Transit { to });
                    }
                }
            }
        }
        self.settling.extend(settling);
        self.person_ids.extend(people.iter().copied());
        let people = &self.person_ids;
        for (id, at) in &places {
            assert_eq!(at.len(), 1, "{id:?} is in several places at once: {at:?}");
        }
        for rig in &self.hosts {
            for id in rig.manager.recently_despawned_arrivals() {
                self.tracked.remove(&id);
            }
        }
        let settled: Vec<EntityId> = self
            .tracked
            .iter()
            .filter(|(id, last)| {
                !places.contains_key(id)
                    && !shadows.contains(id)
                    && matches!(last, Some(Place::Region { .. }))
                    && self.settling.contains(id)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in settled {
            self.tracked.remove(&id);
            self.settling.remove(&id);
        }
        // A tracked player's body ends by parking (its record is on a host now) or by a
        // move's park (its record is on its way to a new body: a creation in flight, a
        // `PlayerMove` between hosts, or that new body already).
        let in_move: BTreeSet<PlayerId> =
            self.players_in_move().keys().filter_map(|c| self.players.get(c)).copied().collect();
        let bodies_now: BTreeSet<PlayerId> =
            places.keys().filter_map(|id| self.player_bodies.get(id)).copied().collect();
        let parked: Vec<EntityId> = self
            .tracked
            .keys()
            .filter(|id| !places.contains_key(id) && !shadows.contains(id))
            .filter(|id| {
                self.player_bodies.get(id).is_some_and(|p| {
                    !self.parked_on(*p).is_empty() || in_move.contains(p) || bodies_now.contains(p)
                })
            })
            .copied()
            .collect();
        for id in parked {
            self.tracked.remove(&id);
        }
        // A transfer a survivor owed a dead host went into its succession report (a `Sent`
        // journal entry): the heir resolves it (Task 9) — no longer tracked.
        let reported = self.reported_entities();
        let into_reports: Vec<EntityId> =
            self.tracked.keys().filter(|id| !places.contains_key(id) && !shadows.contains(id) && reported.contains(id)).copied().collect();
        for id in into_reports {
            self.tracked.remove(&id);
            self.forgiven.insert(id);
        }
        for (id, last) in &self.tracked {
            assert!(
                places.contains_key(id) || shadows.contains(id),
                "{id:?} vanished (last seen {last:?}, None = sent and unacknowledged)"
            );
        }
        self.tracked.retain(|id, _| !people.contains(id));
        let forgiven = &self.forgiven;
        for id in shadows {
            if !people.contains(&id) && !places.contains_key(&id) && !forgiven.contains(&id) {
                self.tracked.insert(id, None);
            }
        }
        for (id, at) in places {
            if !people.contains(&id) && !forgiven.contains(&id) {
                self.tracked.insert(id, Some(at[0].clone()));
            }
        }
        if let Some(expected) = self.items {
            self.assert_items(expected);
        }
        self.assert_players();
    }

    /// Every entity a succession report names (R4): held by an heir, queued to one, or in
    /// transit on a link.
    fn reported_entities(&self) -> BTreeSet<EntityId> {
        let n = self.hosts.len() as u32;
        let mut reports: Vec<Vec<game::JournalEntry>> = Vec::new();
        for rig in &self.hosts {
            for held in rig.manager.all_succession_reports().values() {
                reports.extend(held.values().map(|r| r.entries.clone()));
            }
            for o in 0..n {
                for msg in rig.manager.peer_owed(o) {
                    if let Reliable::SuccessionReport { entries, .. } = msg {
                        reports.push(entries);
                    }
                }
            }
        }
        for (ea, eb) in self.links.values() {
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body: Reliable::SuccessionReport { entries, .. }, .. } = msg {
                        reports.push(entries);
                    }
                }
            }
        }
        reports
            .into_iter()
            .flatten()
            .filter_map(|e| match e.subject {
                game::Subject::Entity(id) => Some(id),
                game::Subject::Player(_) => None,
            })
            .collect()
    }

    /// Each session id whose player's record is in a [`Reliable::PlayerMove`] never sent
    /// or in transit on a link, with how many such places hold it; a move a session SENT
    /// and the peer has not acknowledged (the acknowledgement lag: the peer may hold it) is
    /// listed with no place of its own.
    fn queued_moves(&self) -> BTreeMap<ClientId, usize> {
        let mut out: BTreeMap<ClientId, usize> = BTreeMap::new();
        for rig in &self.hosts {
            for (_, c, _, sent) in rig.manager.peer_unreceived_moves() {
                *out.entry(c).or_default() += usize::from(!sent);
            }
        }
        for (ea, eb) in self.links.values() {
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body, .. } = &msg {
                        let Reliable::PlayerMove { handoff, .. } = body.inner() else { continue };
                        *out.entry(handoff.client).or_default() += 1;
                    }
                }
            }
        }
        out
    }

    /// Each session id whose player's record is on its way to a new body —
    /// [`Self::queued_moves`], or a body's creation in flight on a host
    /// (`creating_players`) — with how many places hold it. (By session id: a login placed
    /// from a stored record may stand beside a queued copy under the old id, which its
    /// arrival drops.)
    fn players_in_move(&self) -> BTreeMap<ClientId, usize> {
        let mut out = self.queued_moves();
        for rig in &self.hosts {
            for (c, _) in rig.manager.creating_players() {
                *out.entry(c).or_default() += 1;
            }
        }
        out
    }

    /// Player conservation across hosts, for every player a test connected: its body is
    /// in at most one region of one host, its parked record on at most one host, its
    /// record on its way to a new body ([`Self::players_in_move`]) in at most one place,
    /// never two of those (a body in flight or in a peer queue is an entity above,
    /// conserved as such; a creation in flight may stand beside the body it made until
    /// its host hears it applied), at most one host has a home for it, has it online
    /// (`player_of`) or holds its claim.
    ///
    /// The same per PLAYER, over every session id a test mapped to it (a login under a new
    /// id beside an older one): at most one body in the world, never a body beside a
    /// parked record, never two queued moves. A body (the login's) beside a queued move
    /// of the older id is allowed: that move is dropped on arrival (the per-id rule above
    /// keeps a body and a queued move of one id apart).
    fn assert_players(&mut self) {
        let players: Vec<(ClientId, PlayerId)> = self.players.iter().map(|(c, p)| (*c, *p)).collect();
        let in_move = self.players_in_move();
        let queued_moves = self.queued_moves();
        let mut per_player: BTreeMap<PlayerId, (Vec<(ClientId, u32, RegionCoords)>, usize)> = BTreeMap::new();
        for &(client, player) in &players {
            let entry = per_player.entry(player).or_default();
            entry.0.extend(self.player_owners(client).into_iter().map(|(h, rc)| (client, h, rc)));
            entry.1 += queued_moves.get(&client).copied().unwrap_or(0);
        }
        for (player, (bodies, queued)) in &per_player {
            let parked = self.parked_on(*player);
            assert!(bodies.len() <= 1, "player {} has several bodies (session, host, region): {bodies:?}", player.0);
            assert!(
                bodies.is_empty() || parked.is_empty(),
                "player {} has a body ({bodies:?}) and a parked record (hosts {parked:?})",
                player.0
            );
            assert!(*queued <= 1, "player {} is in {queued} queued moves", player.0);
        }
        for (client, player) in players {
            let bodies = self.player_owners(client);
            let parked = self.parked_on(player);
            let moving = in_move.get(&client).copied().unwrap_or(0);
            let queued = queued_moves.get(&client).copied().unwrap_or(0);
            assert!(bodies.len() <= 1, "player {}'s body is in several regions: {bodies:?}", player.0);
            assert!(parked.len() <= 1, "player {} is parked on several hosts: {parked:?}", player.0);
            assert!(moving <= 1, "player {}'s record is on its way in {moving} places", player.0);
            assert!(
                bodies.is_empty() || parked.is_empty(),
                "player {} has a body ({bodies:?}) and a parked record (hosts {parked:?})",
                player.0
            );
            assert!(
                queued == 0 || (bodies.is_empty() && parked.is_empty()),
                "player {} is in a queued move and has a body ({bodies:?}) or a parked record (hosts {parked:?})",
                player.0
            );
            let homes: Vec<(u32, RegionCoords)> = (0..self.hosts.len() as u32)
                .filter_map(|h| self.hosts[h as usize].manager.home_of(client).map(|rc| (h, rc)))
                .collect();
            assert!(homes.len() <= 1, "client {client} has a home on several hosts: {homes:?}");
            let online: Vec<u32> = (0..self.hosts.len() as u32)
                .filter(|h| self.hosts[*h as usize].manager.player_of(client) == Some(player))
                .collect();
            assert!(online.len() <= 1, "player {} is online on several hosts: {online:?}", player.0);
            let claims: Vec<(u32, game::Claim)> = (0..self.hosts.len() as u32)
                .filter_map(|h| self.hosts[h as usize].manager.client_claim(client).map(|c| (h, c)))
                .collect();
            assert!(claims.len() <= 1, "player {}'s claim is held on several hosts: {claims:?}", player.0);
        }
    }

    /// Items in the world as `(held, shadow)`: `held` = own piles, intents and replies
    /// queued in running regions or in their outboxes, intents and replies in flight to
    /// a region or waiting in a mailbox, in a session's never-sent part, or in transit on
    /// a link; `shadow` = in intents and replies a session SENT and the peer has not
    /// acknowledged (the peer may hold them already, or they are in transit, or were
    /// lost with a connection and will be sent again).
    pub fn items(&mut self) -> (u64, u64) {
        let (mut held, mut shadow) = (0, 0);
        for rig in &mut self.hosts {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    let data = region.data();
                    held += data.item_piles().values().flat_map(|p| &p.stacks).map(|s| u64::from(s.count)).sum::<u64>();
                    held += data.queued_intents().into_iter().map(|s| msg_items(&IntentMsg::Intent(s))).sum::<u64>();
                    held += data.queued_replies().into_iter().map(|s| msg_items(&IntentMsg::Reply(s))).sum::<u64>();
                    held += region.outgoing_intents().iter().map(msg_items).sum::<u64>();
                });
            }
            held += rig.manager.intents_held().iter().map(msg_items).sum::<u64>();
            // Owed intents a survivor re-routed, in the reports the heir holds (R4).
            for reports in rig.manager.all_succession_reports().values() {
                for report in reports.values() {
                    held += report.owed.iter().flat_map(game::intents_in).map(|m| msg_items(&m)).sum::<u64>();
                }
            }
            for (_, msg, sent) in rig.manager.peer_unreceived_intents() {
                if sent {
                    shadow += msg_items(&msg);
                } else {
                    held += msg_items(&msg);
                }
            }
        }
        for (ea, eb) in self.links.values() {
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body, .. } = &msg {
                        held += game::intents_in(body).iter().map(msg_items).sum::<u64>();
                    }
                }
            }
        }
        (held, shadow)
    }

    /// Item conservation across hosts: `expected` items are held, or some of them only
    /// in sent and unacknowledged messages (never more than `expected` held: a copy),
    /// and nothing was destroyed or given up.
    pub fn assert_items(&mut self, expected: u64) {
        let (held, shadow) = self.items();
        assert!(held <= expected, "{held} items held, {expected} put in: a copy");
        assert!(held + shadow >= expected, "{held} items held + {shadow} unacknowledged, {expected} put in: lost");
        if shadow == 0 {
            assert_eq!(held, expected, "nothing unacknowledged: every item is held");
        }
        for (h, rig) in self.hosts.iter().enumerate() {
            assert_eq!(rig.manager.items_destroyed(), 0, "host {h} destroyed items");
            assert_eq!(rig.manager.intent_replies_lost(), 0, "host {h} gave a reply up");
        }
    }
}

/// Every entity a part writes: in a region snapshot or an in-flight crossing row.
fn part_entities(cut: &WorldCut) -> BTreeSet<EntityId> {
    let mut ids: BTreeSet<EntityId> = cut.regions.iter().flat_map(|(_, p)| p.entities.iter().map(|b| b.id)).collect();
    for row in &cut.in_flight {
        if let InFlightRow::Transfer { bundle, .. } = row {
            ids.insert(bundle.id);
        }
    }
    ids
}

/// The items an intent or a reply carries (a deposit's or a put's stack; a reply's
/// [`game::IntentReply::carried_items`]).
pub fn msg_items(m: &IntentMsg) -> u64 {
    match m {
        IntentMsg::Intent(s) => match s.intent {
            Intent::DepositItems { stack, .. } | Intent::PutInContainer { stack, .. } => u64::from(stack.count),
            _ => 0,
        },
        IntentMsg::Reply(s) => s.reply.carried_items().map_or(0, |(_, stack)| u64::from(stack.count)),
    }
}
