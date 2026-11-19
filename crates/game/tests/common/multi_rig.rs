//! Several region hosts of one world in one test: a [`Rig`] per host (its own
//! [`game::WorldManager`] on an [`game::InlineSpawner`], owning the regions
//! `game::Static` gives it), every pair of hosts joined by a seeded, delaying
//! [`InlinePeerLink`] under a [`PeerSession`] at each end. Each host's world tick is its
//! spawner's round count, so [`MultiRig::round`] ticks every host every round and checks
//! they agree. Every host-seam test builds on it. [`MultiRig::with_coordinator`] gives
//! every host its own seeded, delaying inline link to ONE world coordinator.
use super::cut_store::CutStore;
use super::CoreInputs;
use super::rig::{Rig, CLIENT};
use game::{
    ClientId, ClientPacket, CutId, HostId, EntityId, HostEndpoint, InFlightRow, Intent, IntentMsg, InlinePeerLink, InlineSchedule,
    InlineSpawner, PeerHello, PeerLink, PeerMsg, PeerSession, PlayerId, PlayerRecord, RegionCoords,
    RegionSpawner, Reliable, ServerEvent, StoredRegion, WorldCut, REGION_SIZE, TICK_RATE,
};
use std::collections::{BTreeMap, BTreeSet};

/// The host ids `h(0)..h(n)`.
pub fn host_ids(n: u32) -> impl Iterator<Item = HostId> + Clone {
    (0..u64::from(n)).map(HostId)
}
use std::sync::{Arc, Mutex};

/// The world generation every host of the rig runs.
pub const RIG_GENERATION: u64 = 1;
/// Most rounds [`MultiRig::connect`] waits for every session to be up.
pub const CONNECT_ROUNDS: usize = 40;
/// Times one drive hands the core's cut decision out again after its transition met a foreign
/// version (the rig's `arm_foreign_version` arms one at a time).
const DECISION_ROUNDS: usize = 4;

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
    /// Test hook ([`MultiRig::hold_orders`]): while set, a succession order and everything
    /// after it wait on the link (what came before it is read).
    hold_orders: Arc<std::sync::atomic::AtomicBool>,
    /// What the order hold kept back, in order.
    stash: std::collections::VecDeque<game::CoordMsg>,
    /// Test hook ([`MultiRig::drop_assigned`]): this many of the next [`game::CoordMsg::Assigned`]
    /// read are lost (the message never came).
    drop_assigned: Arc<std::sync::atomic::AtomicUsize>,
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
        // Everything ready is kept in order here; an order (and what follows) waits while
        // the order hold is set.
        while let Some(m) = self.inner.recv() {
            self.stash.push_back(m);
        }
        let holding = self.hold_orders.load(std::sync::atomic::Ordering::SeqCst);
        let msg = match self.stash.front() {
            None => None,
            Some(game::CoordMsg::Succession { .. }) if holding => None,
            Some(_) => self.stash.pop_front(),
        };
        if matches!(msg, Some(game::CoordMsg::HopIs { .. })) {
            self.hop_is.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if matches!(msg, Some(game::CoordMsg::Assigned { .. })) {
            let left = self.drop_assigned.load(std::sync::atomic::Ordering::SeqCst);
            if left > 0 {
                self.drop_assigned.store(left - 1, std::sync::atomic::Ordering::SeqCst);
                return self.recv();
            }
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
    /// As a QUIC connection closed by one end: both ends see it go and a new one come (the
    /// dialer makes it), everything in transit lost — a hello on each side follows.
    fn close(&mut self) {
        let mut end = self.end();
        if end.is_up() {
            end.set_up(false);
            end.set_up(true);
        }
    }
}

/// The hello host `ordinal` of the rig starts its sessions with.
pub fn rig_hello(ordinal: HostId) -> PeerHello {
    rig_hello_as(ordinal, rig_boot(ordinal))
}

/// Host `ordinal`'s first boot in the rig.
pub fn rig_boot(ordinal: HostId) -> u64 {
    1000 + ordinal.0
}

/// [`rig_hello`] of boot `boot` (a host started again, [`MultiRig::start`]).
pub fn rig_hello_as(ordinal: HostId, boot: u64) -> PeerHello {
    PeerHello {
        id: ordinal,
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

/// The adjacent pair of regions nearest the origin whose owners differ (`hosts` hosts,
/// `s` regions per super-region side): `(a, b)`, `b` one step +x or +z of `a`.
pub fn host_seam(hosts: u32, s: u32) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| super::fixed(HostId::SOLO, hosts).owner_of(rc, s);
    for a in nearest_first(64) {
        for b in [RegionCoords::new(a.x + 1, a.z), RegionCoords::new(a.x, a.z + 1)] {
            if owner(a) != owner(b) {
                return (a, b);
            }
        }
    }
    panic!("no host seam within 64 regions of the origin ({hosts} hosts, s = {s})");
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
    Region { host: HostId, rc: RegionCoords },
    /// In a host's `in_flight` or `pending_arrivals`.
    Manager { host: HostId },
    /// In a host's session to `peer`, never sent.
    Unsent { host: HostId, peer: HostId },
    /// In transit on the link to host `to`.
    Transit { to: HostId },
    /// In the state of a region parked on a host (only with [`MultiRig::parked_hold`]).
    Parked { host: HostId, rc: RegionCoords },
    /// Only in the store: a stored region of released land that runs and parks nowhere (only
    /// with [`MultiRig::parked_hold`]).
    Stored { rc: RegionCoords },
    /// Waiting on a host for its region's super-region to have an owner it knows
    /// (`unowned_waiting`, [`MultiRig::with_directory`]).
    Unowned { host: HostId, sr: game::SuperRegion },
}

pub struct MultiRig {
    pub hosts: Vec<Rig>,
    /// Regions per super-region side.
    pub s: u32,
    /// Each host's keeper region: the region its keeper client's body stands in (owned
    /// by the host, far from the origin, so it never is one a test plays with).
    pub homes: Vec<RegionCoords>,
    /// Each pair of hosts' link `(a, b)` with `a < b`: (end on `a`, end on `b`).
    pub links: BTreeMap<(HostId, HostId), (Arc<Mutex<InlinePeerLink>>, Arc<Mutex<InlinePeerLink>>)>,
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
    pub coordinator: Option<Arc<Mutex<game::coordinator::RigCluster>>>,
    /// What each host sent its coordinator (while its link was up), by host.
    pub world_sent: Vec<Arc<Mutex<Vec<game::HostMsg>>>>,
    /// Each host's [`RigWorldLink`] hold flag.
    pub world_hold: Vec<Arc<std::sync::atomic::AtomicBool>>,
    /// Each host's count of `HopIs` read ([`RigWorldLink`]).
    pub world_hop_is: Vec<Arc<std::sync::atomic::AtomicUsize>>,
    /// Each host's [`RigWorldLink`] order-hold flag ([`Self::hold_orders`]).
    pub world_hold_orders: Vec<Arc<std::sync::atomic::AtomicBool>>,
    /// Each host's [`RigWorldLink`] count of `Assigned` messages still to lose
    /// ([`Self::drop_assigned`]), by host slot (kept across a host's restarts).
    pub world_drop_assigned: Vec<Arc<std::sync::atomic::AtomicUsize>>,
    /// Made without a spawner schedule (every region ticks in lockstep).
    pub lockstep: bool,
    /// The rig's in-memory two-phase store (world cuts, [`Self::drive_world_cuts`]):
    /// staged parts by `(cut, host)`, as Postgres holds prepared transactions.
    pub parts: BTreeMap<(CutId, HostId), WorldCut>,
    /// The decision of every cut decided (first writer wins).
    pub decided: BTreeMap<CutId, bool>,
    /// What each host's committed parts hold (regions, its in-flight rows).
    pub committed: BTreeMap<HostId, CutStore>,
    /// The players table, as committed parts wrote it.
    pub committed_players: BTreeMap<PlayerId, PlayerRecord>,
    /// The parts of the last committed cut, by host.
    pub last_cut: Option<(CutId, BTreeMap<HostId, WorldCut>)>,
    /// Every part a host staged, in order: `(cut, host)`.
    pub prepared: Vec<(CutId, HostId)>,
    /// Parts of cuts decided aborted, as they were staged.
    pub aborted_parts: Vec<(CutId, HostId, WorldCut)>,
    /// Each direction's [`Strip`] hook, by `(from, to)`.
    pub strips: BTreeMap<(HostId, HostId), Arc<Mutex<Strip>>>,
    /// The entities in every host's parked regions' states count as places too
    /// ([`Place::Parked`], decoded every round — costly, so off by default): a test that
    /// parks whole hosts (every host leaving) turns it on.
    pub parked_hold: bool,
    /// The directory every host answers from ([`Self::with_directory`]); `None` = `Static`.
    pub directory: Option<Vec<(game::SuperRegion, Option<HostId>, u64)>>,
    /// The rig's `super_regions` table while land is assigned on demand
    /// ([`Self::with_assignment`]): owner (`None` = a free row) and epoch per super-region.
    pub claim_store: Option<BTreeMap<game::SuperRegion, (Option<HostId>, u64)>>,
    /// Whether the rig coordinator assigns free land ([`Self::with_assignment`]): its crash
    /// policy is `Succession` and every round ([`Self::drive_assignment`]) its land lookups are
    /// read from the rig's `super_regions` table and its assignments executed there.
    pub assigning: bool,
    /// Each host's load as its `hosts` row reports it to the coordinator's reads (none: zero).
    pub loads: BTreeMap<HostId, game::HostLoad>,
    /// What each host's FENCE holds of the land assigned to it: the rows the rig took from its
    /// `take_assigned` (or healed from a directory read) before telling it `claimed`, as the
    /// server's directory keeper does. Cleared when the host dies.
    pub assigned_fences: BTreeMap<HostId, BTreeMap<game::SuperRegion, u64>>,
    /// While set, the store's answers to the assignments executed are lost (the deciding
    /// replica dies after the commit): the coordinator sends no `Assigned` for them.
    pub assign_answers_lost: bool,
    /// Hosts marked draining (`worldctl drain`): out of every directory read's live set, told
    /// to every reader ([`game::WorldManager::set_draining`]) and to the coordinator's reads,
    /// and assigned nothing (the store's guard).
    pub draining: BTreeSet<HostId>,
    /// Directory reads a host asked for (`take_directory_wanted`), per host.
    pub reads_asked: BTreeMap<HostId, u64>,
    /// Rounds played (the seed of each claim's answer delay).
    pub rounds: u64,
    /// Each host's current boot ([`Self::start`] makes a new one).
    pub boots: Vec<u64>,
    /// The hosts the rig was made with (`h(0)..h(founders)`, each with a keeper); hosts
    /// [`Self::start`] adds later have none.
    pub founders: u32,
    /// Hosts killed ([`Self::kill`]) and not started again: their slot holds an empty
    /// placeholder that only keeps the world tick.
    pub dead: BTreeSet<HostId>,
    /// The schedule the hosts' spawners run (a started host's too).
    pub schedule: Option<fn(u64) -> InlineSchedule>,
    /// The rig store's `successions` table ([`Self::run_succession`]), with each one's
    /// settling cut (`None`: open).
    pub successions: Vec<(game::Succession, Option<CutId>)>,
    /// What each committed part adopted (`WorldCut::adopts`, Task 9): `(cut, heir, adopted)`.
    pub adopts: Vec<(CutId, HostId, Vec<HostId>)>,
    /// Entities a killed host held, or that were on their way to or from it
    /// ([`Self::kill`]): no longer tracked — the heir's resolution (Task 9) brings them back.
    pub forgiven: BTreeSet<EntityId>,
    /// Pairs [`Self::assert_journals_explain_the_cut`] checked as truncated by counts (R4
    /// Task 12: a cut committed with their link down), so far.
    pub counted_pairs_checked: usize,
    /// Whether the rig answers an heir's adoption reads ([`Self::with_adoption`]).
    pub adopting: bool,
    /// Adoption reads asked and not answered yet: `(rounds left, heir, succession, predecessor)`.
    pub adoptions_pending: Vec<(u32, HostId, game::SuccessionId, HostId)>,
    /// Every adoption read answered: `(succession, heir)`, in order.
    pub adopted: Vec<(game::SuccessionId, HostId)>,
    /// Test hook: the first start of every region the store answer of an adoption read
    /// hands the heir fails its load (`InlineSpawner::fail_next_start`), as a store read
    /// error on the region's thread would.
    pub fail_adoption_loads: bool,
    /// Successions a rollback abandoned ([`Self::roll_back`]).
    pub abandoned: BTreeSet<game::SuccessionId>,
    /// The rig store's world generation (raised by [`Self::roll_back`]).
    pub generation: u64,
    /// Hosts that left in a committed cut, with the boot that left (their `hosts` row is
    /// clean). A leaver never comes back: a restarted process is a new host ([`Self::start`]).
    pub left: BTreeMap<HostId, u64>,
    /// Whether the rig coordinator gives a departed host's land away when a host asks for
    /// it ([`Self::with_land_on_demand`], [`Self::drive_land`]).
    pub land_on_demand: bool,
    /// The next cut decision [`Self::drive_world_cuts`] commits has its answer dropped (the
    /// coordinator dies after the commit: call [`Self::restart_coordinator_from_store`] next);
    /// the cut and its stored decision land in [`Self::cut_answer_dropped`].
    pub drop_next_cut_answer: bool,
    pub cut_answer_dropped: Option<(CutId, bool)>,
    /// The successions [`Self::drive_land`] declared, in order.
    pub land_declared: Vec<game::Succession>,
    /// The rig store's `cut_begins` (Elastic 1 Task 4): per world cut, the next host id when
    /// its begin was recorded ([`Self::drive_world_cuts`], as the coordinator runtime records
    /// it). A host at or above it registered after the cut began and may be absent from its
    /// decision ([`Self::decide_settling`]).
    pub begins: BTreeMap<CutId, u64>,
    /// The rig store's `desired_hosts` (Elastic 4): name → `(slot, draining, created_ms)`, as the
    /// coordinator's fleet decisions left it ([`Self::add_host_in_store`]).
    pub desired: BTreeMap<String, (u32, bool, u64)>,
    /// The rig store's `world.next_desired_host`.
    pub next_desired_host: u64,
    /// The `hosts.name` of each host [`Self::start`] started under a name (`VOXEL_HOST_NAME`;
    /// Elastic 4: the desired row it runs for); the others' is empty.
    pub names: BTreeMap<HostId, String>,
    /// Rows the rig store ended (Elastic 2a, [`Self::end_landless_in_store`]): `(host, boot)`,
    /// dead with no succession.
    pub ended: BTreeSet<(HostId, u64)>,
    /// Coordinator processes started so far ([`Self::with_coordinator`], then one per
    /// [`Self::restart_coordinator_from_store`] or added replica): each one's process boot and claim epoch.
    pub coordinator_starts: u64,
    /// How many times the rig store ran each cut's decision (Elastic 2b Task 5: once, whatever
    /// replicas the world has).
    pub decide_executions: BTreeMap<CutId, u32>,
    /// Rows the rig store pruned (`register_host`'s `prune_hosts`, Elastic 2b Task 10b): absent
    /// from every later directory read, below its `next_host_id` (the rig's next id,
    /// `hosts.len()`), so each reader forgets them ([`game::WorldManager::forget_absent_hosts`]).
    pub pruned: BTreeSet<HostId>,
    /// Every host's idle-land dial ([`Self::with_land_idle_ms`]; `None`: the manager's default),
    /// a started host's too.
    pub land_idle_ms: Option<u64>,
    /// Super-regions a committed part released (Elastic 3 Task 6): their stored regions that run
    /// and park nowhere are places of what they hold ([`Place::Stored`], with `parked_hold`).
    pub released: BTreeSet<game::SuperRegion>,
    /// The releases of each staged part, by `(cut, host)`: the host's fence shrinks by them
    /// before it hears the commit ([`Self::drive_world_cuts`], the server's order).
    pub part_releases: BTreeMap<(CutId, HostId), Vec<game::SuperRegion>>,
    /// Hosts whose world-cut parts the rig store refuses to stage ([`Self::drive_world_cuts`]):
    /// each such cut is decided aborted for a missing part.
    pub fail_parts: BTreeSet<HostId>,
}

/// Host `h`'s endpoint in the rig (what its sessions' first packet names).
pub fn rig_endpoint(h: HostId) -> HostEndpoint {
    HostEndpoint { id: h, quic: None, webtransport: None, cert_hash: None }
}

/// `world_store::DRAIN_MAX_SECS` (`game` does not depend on `world-store`): the longest drain
/// notice the store's `drain_host_in` accepts. Keep the two equal.
pub const RIG_DRAIN_MAX_SECS: u64 = 7 * 24 * 3600;

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
        Self::new_from(hosts, s, schedule, seed, link_delay_steps, None, hosts)
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
        keepers: u32,
    ) -> MultiRig {
        let mut rigs = Vec::new();
        let mut homes = Vec::new();
        for h in host_ids(hosts) {
            let ownership = super::fixed(h, hosts);
            let mut spawner = match &stored {
                Some(regions) => InlineSpawner::with_portable(regions.clone()),
                None => InlineSpawner::default(),
            };
            if let Some(f) = schedule {
                spawner.set_schedule(Some(f(mix(seed, h.0))));
            }
            let mut rig = Rig::with_ownership(spawner, super::seam_translate::uniform_floor, Box::new(ownership.clone()), s);
            rig.manager.set_endpoints(rig_endpoint(h), game::seeded_nonces(mix(seed, 2000 + h.0)));
            rigs.push(rig);
            let home = nearest_first(64)
                .into_iter()
                .map(|rc| RegionCoords::new(rc.x + 32, rc.z + 32))
                .find(|rc| ownership.owns(*rc, s))
                .expect("a host owns a region near (32, 32)");
            // A host started later (no keeper) keeps no home of its own: a slot only.
            homes.push(if h.0 < u64::from(keepers) { home } else { homes[0] });
        }
        let mut links = BTreeMap::new();
        let mut strips = BTreeMap::new();
        for a in host_ids(hosts) {
            for b in host_ids(hosts).filter(|b| *b > a) {
                let (ea, eb) = InlinePeerLink::pair(mix(seed, 1000 + a.0 * u64::from(hosts) + b.0), link_delay_steps);
                let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
                let (sa, sb) = (Arc::new(Mutex::new(Strip::default())), Arc::new(Mutex::new(Strip::default())));
                rigs[a.0 as usize].manager.set_peer(b, PeerSession::new(rig_hello(a), b), Box::new(SharedEnd(ea.clone(), sa.clone())));
                rigs[b.0 as usize].manager.set_peer(a, PeerSession::new(rig_hello(b), a), Box::new(SharedEnd(eb.clone(), sb.clone())));
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
                world_hold_orders: Vec::new(),
                world_drop_assigned: Vec::new(),
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
                assigning: false,
                loads: BTreeMap::new(),
                assigned_fences: BTreeMap::new(),
                assign_answers_lost: false,
                draining: BTreeSet::new(),
                reads_asked: BTreeMap::new(),
                rounds: 0,
                boots: host_ids(hosts).map(rig_boot).collect(),
                founders: keepers,
                dead: BTreeSet::new(),
                schedule,
                successions: Vec::new(),
                adopts: Vec::new(),
                forgiven: BTreeSet::new(),
                counted_pairs_checked: 0,
                adopting: false,
                adoptions_pending: Vec::new(),
                adopted: Vec::new(),
                fail_adoption_loads: false,
                abandoned: BTreeSet::new(),
                generation: RIG_GENERATION,
                left: BTreeMap::new(),
                land_on_demand: false,
                drop_next_cut_answer: false,
                cut_answer_dropped: None,
                land_declared: Vec::new(),
                begins: BTreeMap::new(),
                desired: BTreeMap::new(),
                next_desired_host: 1,
                names: BTreeMap::new(),
                ended: BTreeSet::new(),
                coordinator_starts: 0,
                decide_executions: BTreeMap::new(),
                pruned: BTreeSet::new(),
                land_idle_ms: None,
                released: BTreeSet::new(),
                part_releases: BTreeMap::new(),
                fail_parts: BTreeSet::new(),
            };
        for h in host_ids(keepers) {
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
        self.coordinator_starts = 1;
        let process = game::coordinator::ReplicaId(self.coordinator_starts);
        // A store stands behind it (Elastic 2a): every output leaves with its transition, and
        // the last transition is what a restarted coordinator loads.
        let core = game::coordinator::RigCluster::with_store(game::Coordinator::new_for(history, now, process));
        while self.world_drop_assigned.len() < self.hosts.len() {
            self.world_drop_assigned.push(Arc::default());
        }
        if self.assigning {
            core.lock().unwrap_or_else(|e| e.into_inner()).set_crash_policy(game::CrashPolicy::Succession);
        }
        // Every link before any is installed (an install pumps it): none misses the view.
        let links: Vec<game::InlineWorldLink> = host_ids(self.hosts.len() as u32)
            .map(|h| game::InlineWorldLink::shared(core.clone(), h).with_delay(mix(self.seed, 3000 + h.0), self.link_delay))
            .collect();
        for (h, inner) in links.into_iter().enumerate() {
            let sent = Arc::new(Mutex::new(Vec::new()));
            self.world_sent.push(sent.clone());
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.world_hold.push(hold.clone());
            let hop_is = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            self.world_hop_is.push(hop_is.clone());
            let hold_orders = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.world_hold_orders.push(hold_orders.clone());
            let rig = &mut self.hosts[h];
            let now = rig.now_ms;
            rig.manager.set_world_link(Box::new(RigWorldLink { inner, sent, hold, hop_is, hold_orders: hold_orders.clone(), stash: Default::default(), drop_assigned: self.world_drop_assigned[h].clone() }), world_boot(self.boots[h]), now);
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

    /// World cuts every `every_ms` (the coordinator's period; its clock is the hosts').
    /// Rounds then go through [`Self::cut_round`].
    pub fn with_world_cuts(&mut self, every_ms: u64) {
        let hosts: BTreeSet<HostId> = host_ids(self.hosts.len() as u32).collect();
        let mut core = self.core();
        // The cut ids' high half, once: the row's bootstrap epoch (Elastic 2b Task 3).
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
        // What the upkeep read of the coordinator's assignments is fenced and taken in the same
        // pass, before any peer message is read (the server's directory keeper runs right after
        // its world link is pumped).
        if self.claim_store.is_some() {
            let live: Vec<HostId> = host_ids(self.hosts.len() as u32).filter(|h| !self.dead.contains(h)).collect();
            for h in live {
                let rows = self.hosts[h.0 as usize].manager.take_assigned();
                self.fence_then_claim(h, rows);
            }
        }
        self.drive_world_cuts();
        self.answer_successions_read();
        self.drive_land();
        for rig in &mut self.hosts {
            rig.settle();
        }
    }

    /// The ticker's read of the open successions, when it owes or was asked one (Elastic 2b Task
    /// 6: a loaded core, a new ticker, a rebase that dropped a read, a dead row nobody declared),
    /// answered from the rig store as the runtime does every pass.
    pub fn answer_successions_read(&mut self) {
        if self.coordinator.is_none() {
            return;
        }
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let open: Vec<game::Succession> = self
            .successions
            .iter()
            .filter(|(s, settled)| settled.is_none() && !self.abandoned.contains(&s.id))
            .map(|(s, _)| s.clone())
            .collect();
        let mut core = self.core();
        if core.take_successions_reread() {
            core.successions_open(open, now);
        }
    }

    /// One pass of the coordinator runtime's succession pump for drained land
    /// ([`Self::with_land_on_demand`]): a read of the `hosts` rows ([`Self::hosts_seen`]), the
    /// land hosts asked for looked up ([`Self::drive_assignment`]'s lookups, Elastic 3), and a
    /// `Leave` declaration the coordinator wants executed in the rig store
    /// ([`Self::declare_in_store`]) and answered. The hosts a cut needs are the live ones whose
    /// row is not clean.
    pub fn drive_land(&mut self) {
        if !self.land_on_demand {
            return;
        }
        let n = self.hosts.len() as u32;
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let rows = self.hosts_seen(now);
        let expected: BTreeSet<HostId> =
            host_ids(n).filter(|h| !self.dead.contains(h) && self.left.get(h) != Some(&self.boots[(*h).0 as usize])).collect();
        {
            let mut core = self.core();
            core.observe_leases(&rows, now, now);
            core.set_expected_hosts(expected);
        }
        self.answer_land_lookups();
        if self.core().declare_wanted(now).is_none() {
            return;
        }
        // Handed out: it leaves with the transition, executed and answered with it.
        self.core().commit();
        for a in self.execute_committed(true) {
            if let game::coordinator::DecisionAnswer::Declared { outcome: Ok(game::Declared::Done(list)), .. } = a {
                self.land_declared.extend(list);
            }
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
        // What the transitions since the last drive committed (a cut's begin, recorded by the
        // transition that starts it), executed and answered.
        self.execute_committed(true);
        for h in host_ids(self.hosts.len() as u32) {
            let m = &mut self.hosts[h.0 as usize].manager;
            // Only a world cut's part (a test's one-phase cut stays for the test to take).
            if m.world_cut().is_none() {
                continue;
            }
            if let Some(cut) = m.take_cut() {
                let id = cut.id;
                if self.fail_parts.contains(&h) {
                    self.hosts[h.0 as usize].manager.world_cut_failed(id, game::CutFailure::Store("test: refused".into()));
                    continue;
                }
                match self.prepare_part(h, cut) {
                    Ok(()) => self.hosts[h.0 as usize].manager.world_cut_prepared(id),
                    Err(why) => self.hosts[h.0 as usize].manager.world_cut_failed(id, game::CutFailure::Store(why)),
                }
            }
        }
        for h in host_ids(self.hosts.len() as u32) {
            for (cut, committed) in self.hosts[h.0 as usize].manager.take_world_cut_done() {
                // The store's decision is what settles the part (a part prepared after the
                // decision is the decision's too).
                let stored = self.decided.get(&cut).copied().unwrap_or(committed);
                let releases = self.part_releases.remove(&(cut, h)).unwrap_or_default();
                if committed && stored {
                    // The fence shrinks BEFORE the manager hears the commit (the server's order).
                    if let Some(fence) = self.assigned_fences.get_mut(&h) {
                        for sr in &releases {
                            fence.remove(sr);
                        }
                    }
                    self.hosts[h.0 as usize].manager.cut_committed(cut, Vec::new());
                } else {
                    self.hosts[h.0 as usize].manager.cut_failed(cut);
                }
            }
        }
        // The core's decision, handed out, leaves with its transition and is executed with it
        // (a transition that met a foreign version committed nothing: handed out again).
        for _ in 0..DECISION_ROUNDS {
            let decision = core.lock().unwrap_or_else(|e| e.into_inner()).take_decision();
            if decision.is_none() {
                break;
            }
            core.lock().unwrap_or_else(|e| e.into_inner()).commit();
            let answer = !std::mem::take(&mut self.drop_next_cut_answer);
            for a in self.execute_committed(answer) {
                if let (false, game::coordinator::DecisionAnswer::Cut { cut, committed: Ok(stored), .. }) = (answer, a) {
                    self.cut_answer_dropped = Some((cut, stored));
                }
            }
            if !answer {
                // The coordinator is dead: nothing more of it runs until its restart.
                return;
            }
        }
        // A decision whose answer was lost (a restarted coordinator's): looked up.
        let lost = core.lock().unwrap_or_else(|e| e.into_inner()).take_decision_lookup();
        if let Some((cut, _)) = lost {
            let stored = self.decide(cut, false, &[]);
            core.lock().unwrap_or_else(|e| e.into_inner()).decision_looked_up(cut, stored);
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
    pub fn prepare_part(&mut self, h: HostId, cut: WorldCut) -> Result<(), String> {
        assert!(cut.history.is_none(), "a host's part never carries the history");
        assert_eq!(cut.host, h, "a part is its host's");
        self.part_releases.insert((cut.id, h), cut.releases.clone());
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
            let Some(owner) = self.hosts[h.0 as usize].manager.owner_host(target) else { continue };
            // A host that left (Task 14) takes no part: what waits for it is the sender's row;
            // so is a host outside the cut (Elastic 1 Task 4: it joined while the cut ran, its
            // session frozen until the decision).
            let m = &self.hosts[h.0 as usize].manager;
            let absent = m.peer_left(owner) || m.world_cut_outsiders().contains(&owner);
            assert!(
                owner == h || at >= cut.tick || absent,
                "cut {}: host {h}'s part has a row for {target:?} (host {owner}'s) due at {at}, before the cut tick {}",
                cut.id.0,
                cut.tick
            );
        }
        // A part releases only land the store says its host holds, at the epoch its fence holds.
        for sr in &cut.releases {
            let row = self.claim_store.as_ref().and_then(|s| s.get(sr)).copied();
            assert!(
                matches!(row, Some((Some(o), _)) if o == h),
                "cut {}: host {h} releases {sr:?}, whose row is {row:?} (seed {})",
                cut.id.0,
                self.seed
            );
            if let (Some(fenced), Some((_, epoch))) = (self.assigned_fence(h).get(sr).copied(), row) {
                assert_eq!(fenced, epoch, "cut {}: host {h} releases {sr:?} at its fence's epoch", cut.id.0);
            }
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

    /// The rig store's side of the transitions committed since the last call (Elastic 2b Task 3,
    /// `world_store` `coordinator_tx.rs`): each store decision they carried executed in order, as
    /// the store runs it inside the transition's transaction, and its answer — with `answer`,
    /// applied to the core as an input (the runtime's next pass); without, dropped (the replica
    /// dies after the commit, before it applied them: its successor looks the marks up). The
    /// answers.
    pub fn execute_committed(&mut self, answer: bool) -> Vec<game::coordinator::DecisionAnswer> {
        use game::coordinator::{DecisionAnswer, StoreDecision};
        let committed = self.core().take_committed();
        let mut answers = Vec::with_capacity(committed.len());
        let mut issuers = Vec::with_capacity(committed.len());
        for (stamp, d) in committed {
            issuers.push(stamp.replica);
            answers.push(match d {
                StoreDecision::DecideCut(c) => {
                    *self.decide_executions.entry(c.cut).or_default() += 1;
                    let stored = self.decide_settling(c.cut, c.commit, &c.hosts, &c.settles);
                    if stored {
                        // Their `hosts` rows are clean now (`decide_in`'s `leaving`).
                        for &h in &c.leaving {
                            self.left.insert(h, self.boots[h.0 as usize]);
                        }
                    }
                    DecisionAnswer::Cut { stamp, cut: c.cut, committed: Ok(stored) }
                }
                StoreDecision::BeginCut { cut } => {
                    // `begin_in`: the next id to issue.
                    let next = self.hosts.len() as u64;
                    let bound = *self.begins.entry(cut).or_insert(next);
                    DecisionAnswer::Begin { cut, bound: Ok(HostId(bound)) }
                }
                StoreDecision::Declare(d) => {
                    let outcome = self.declare_in_store(&d);
                    DecisionAnswer::Declared { stamp, outcome: Ok(outcome) }
                }
                StoreDecision::EndLandless(end) => {
                    let outcome = self.end_landless_in_store(end);
                    DecisionAnswer::Ended { stamp, end, outcome: Ok(outcome) }
                }
                StoreDecision::Rollback { .. } => {
                    // Every open succession abandoned, the generation raised.
                    let open = self.open_successions();
                    self.abandoned.extend(open);
                    self.generation += 1;
                    DecisionAnswer::RolledBack { stamp, outcome: Ok(self.generation) }
                }
                StoreDecision::EndReplica(replica) => DecisionAnswer::ReplicaEnded { replica },
                StoreDecision::AssignLand(assign) => {
                    let outcome = self.assign_in_store(assign);
                    DecisionAnswer::Assigned { stamp, assign, outcome: Ok(outcome) }
                }
                StoreDecision::AddHost(add) => {
                    let outcome = self.add_host_in_store(add);
                    DecisionAnswer::HostAdded { stamp, add, outcome: Ok(outcome) }
                }
                StoreDecision::DrainHost(drain) => {
                    let outcome = self.drain_host_in_store(&drain);
                    DecisionAnswer::HostDrained { stamp, drain, outcome }
                }
                StoreDecision::RemoveHost(remove) => {
                    let outcome = self.remove_host_in_store(&remove);
                    DecisionAnswer::HostRemoved { stamp, remove, outcome: Ok(outcome) }
                }
            });
        }
        if answer {
            // Each to the replica that issued it (Elastic 2b Task 6: whoever was the ticker); one
            // whose process died never gets it.
            let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
            let mut core = self.core();
            for (replica, a) in issuers.into_iter().zip(&answers) {
                core.answer(replica, a.clone(), now);
            }
        }
        answers
    }

    /// The decision on `cut` (as `decide_cut` / `abort_cut`): the stored one if any (first
    /// writer wins); else a commit only if asked and exactly `hosts` staged parts, an
    /// abort otherwise. Every staged part is then committed or dropped as it says.
    pub fn decide(&mut self, cut: CutId, commit: bool, hosts: &[HostId]) -> bool {
        self.decide_settling(cut, commit, hosts, &[])
    }

    /// [`Self::decide`] of a cut that settles `settles` if it commits (as `decide_cut`): a
    /// commit is recorded as an abort unless `settles` is exactly the open successions; the
    /// commit settles them.
    pub fn decide_settling(&mut self, cut: CutId, commit: bool, hosts: &[HostId], settles: &[game::SuccessionId]) -> bool {
        if let Some(&stored) = self.decided.get(&cut) {
            return stored;
        }
        let staged: BTreeSet<HostId> = self.parts.keys().filter(|(c, _)| *c == cut).map(|(_, h)| *h).collect();
        let listed: BTreeSet<HostId> = hosts.iter().copied().collect();
        let open = self.open_successions();
        let named: BTreeSet<game::SuccessionId> = settles.iter().copied().collect();
        // As `decide_cut`: every live host whose row is dirty is listed — but one registered
        // after the cut's begin was recorded (`begins`).
        let bound = self.begins.get(&cut).copied();
        let unlisted: Vec<HostId> = host_ids(self.hosts.len() as u32)
            .filter(|h| !self.dead.contains(h) && self.left.get(h) != Some(&self.boots[h.0 as usize]))
            .filter(|h| !listed.contains(h) && bound.is_none_or(|b| h.0 < b))
            .collect();
        let stored = commit && unlisted.is_empty() && staged == listed && open == named;
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
        let mut at: BTreeMap<EntityId, Vec<(HostId, RegionCoords)>> = BTreeMap::new();
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

    fn apply_part(&mut self, h: HostId, part: &WorldCut) {
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
        // A region row is the region's, not a host's: one another host stored (land it released
        // since) is this part's to write over.
        for (rc, _) in &part.regions {
            let held: Vec<HostId> = self.committed.iter().filter(|(o, s)| **o != h && s.regions.contains_key(rc)).map(|(o, _)| *o).collect();
            for o in held {
                let region = self.committed.get_mut(&o).and_then(|s| s.regions.remove(rc)).expect("held");
                self.committed.entry(h).or_default().regions.entry(*rc).or_insert(region);
            }
        }
        self.committed.entry(h).or_default().apply(part);
        self.committed_players.extend(part.players.iter().cloned());
        // Its releases: free rows at `epoch + 1`, with its commit (`release_in`).
        if let Some(store) = self.claim_store.as_mut() {
            for sr in &part.releases {
                if let Some(row) = store.get_mut(sr) {
                    *row = (None, row.1 + 1);
                }
                self.released.insert(*sr);
            }
        }
    }

    /// Every host answers ownership from a `PgClaim` directory holding exactly `rows`
    /// (`(super-region, owner, epoch)`; owner `None` = a free row) from now on: a
    /// super-region with no row is `Unknown` (the directory is never read whole). The rows
    /// must give every host its keeper's home as `Static` did. Kept by
    /// [`Self::rebuild_from_last_cut`].
    pub fn with_directory(&mut self, rows: Vec<(game::SuperRegion, Option<HostId>, u64)>) {
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            let mut claim = game::PgClaim::new(HostId(h as u64));
            let directory = game::Ownership::directory_mut(&mut claim).expect("a directory");
            for (sr, owner, epoch) in &rows {
                directory.set(*sr, *owner, *epoch);
            }
            rig.manager.set_ownership(Box::new(claim), self.s);
            if h < self.founders as usize {
                assert!(rig.manager.owns(self.homes[h]), "the rows give host {h} its keeper's home");
            }
        }
        self.directory = Some(rows);
    }

    /// From now on free land is ASSIGNED on demand against the rig's `super_regions` table, made
    /// from the rows [`Self::with_directory`] gave (call it first): each host reads it whole
    /// (every host live); the rig coordinator — now, or once [`Self::with_coordinator`] made it —
    /// runs `Succession` and every round ([`Self::drive_assignment`]) reads the land its hosts
    /// asked for from the table, executes its assignments there and answers them; a host's
    /// `take_assigned` is fenced and answered with `claimed`, as the server's directory keeper
    /// does. A test that needs free land assigned gives the rig a coordinator.
    pub fn with_assignment(&mut self) {
        let rows = self.directory.clone().expect("with_directory first");
        self.claim_store = Some(rows.into_iter().map(|(sr, owner, epoch)| (sr, (owner, epoch))).collect());
        self.assigning = true;
        if self.coordinator.is_some() {
            self.core().set_crash_policy(game::CrashPolicy::Succession);
        }
        for h in host_ids(self.hosts.len() as u32) {
            self.read_directory(h);
        }
    }

    /// The rows of land assigned to host `h` that its fence holds ([`Self::assigned_fences`]).
    pub fn assigned_fence(&self, h: HostId) -> BTreeMap<game::SuperRegion, u64> {
        self.assigned_fences.get(&h).cloned().unwrap_or_default()
    }

    /// Host `h` takes `rows` as its own: its fence first ([`Self::assigned_fences`]), then its
    /// manager's `claimed` — the server's order.
    pub fn fence_then_claim(&mut self, h: HostId, rows: Vec<(game::SuperRegion, u64)>) {
        if rows.is_empty() {
            return;
        }
        self.assigned_fences.entry(h).or_default().extend(rows.iter().copied());
        // What its region threads would read from the store for that land (another host may
        // have saved it since: land released and assigned again).
        let srs: BTreeSet<game::SuperRegion> = rows.iter().map(|(sr, _)| *sr).collect();
        let s = self.s;
        let stored: BTreeMap<RegionCoords, StoredRegion> = self
            .committed
            .values()
            .flat_map(|c| c.regions.iter())
            .filter(|(rc, _)| srs.contains(&game::super_region_of(**rc, s)))
            .map(|(rc, r)| (*rc, r.clone()))
            .collect();
        let m = &mut self.hosts[h.0 as usize].manager;
        m.spawner_mut().add_portable(stored);
        m.claimed(rows);
    }

    /// Host `h` reads the rig's `super_regions` table whole, with the `hosts` rows as the
    /// server's directory keeper reads them: live = every running host whose row is not clean
    /// and not marked draining (`claims::live_hosts`), and the clean rows (hosts that left in
    /// a committed cut) handed to the manager after the read (`note_clean_rows`).
    pub fn read_directory(&mut self, h: HostId) {
        let store = self.claim_store.as_ref().expect("with_assignment");
        let rows: Vec<(game::SuperRegion, i64, u64)> =
            store.iter().map(|(sr, (owner, epoch))| (*sr, owner.map_or(game::NO_OWNER, |o| o.0 as i64), *epoch)).collect();
        let clean = |o: &HostId| self.left.get(o) == Some(&self.boots[o.0 as usize]);
        let live: BTreeSet<HostId> = host_ids(self.hosts.len() as u32)
            .filter(|o| !self.dead.contains(o) && !self.draining.contains(o) && (!clean(o) || *o == h))
            .collect();
        let left: Vec<(HostId, u64)> = self.left.iter().map(|(o, b)| (*o, *b)).collect();
        // Dead rows no open succession names (an ended landless host, a settled predecessor):
        // forgotten before the clean rows are noted, as the server's directory keeper does.
        let open = self.open_successions();
        // A pruned row is absent: neither dead nor clean, and below the read's next id.
        let shown = |o: &HostId| !self.pruned.contains(o);
        let gone: Vec<HostId> = host_ids(self.hosts.len() as u32)
            .filter(shown)
            .filter(|o| self.row_dead(*o))
            .filter(|o| !self.successions.iter().any(|(s, _)| open.contains(&s.id) && s.predecessor == *o))
            .collect();
        let dead: Vec<HostId> = host_ids(self.hosts.len() as u32).filter(shown).filter(|o| self.row_dead(*o)).collect();
        let present: BTreeSet<HostId> = host_ids(self.hosts.len() as u32).filter(shown).collect();
        let left: Vec<(HostId, u64)> = left.into_iter().filter(|(o, _)| shown(o)).collect();
        let next_host_id = self.hosts.len() as u64;
        // A lost `Assigned`, healed as the server's directory keeper heals it: a row this host
        // owns that its fence lacks, newer than its directory knows, is fenced and taken FIRST.
        let fence = self.assigned_fence(h);
        let lost: Vec<(game::SuperRegion, u64)> = rows
            .iter()
            .filter(|(sr, owner, epoch)| {
                *owner == h.0 as i64 && !fence.contains_key(sr) && *epoch > self.hosts[h.0 as usize].manager.known_epoch(*sr).unwrap_or(0)
            })
            .map(|(sr, _, epoch)| (*sr, *epoch))
            .collect();
        self.fence_then_claim(h, lost);
        let m = &mut self.hosts[h.0 as usize].manager;
        m.set_draining(self.draining.clone());
        m.directory_read(rows, live);
        m.note_dead_rows(dead);
        m.forget_dead_hosts(gone);
        m.forget_absent_hosts(&present, next_host_id);
        m.note_clean_rows(left);
    }

    /// One round of what serves land on demand ([`Self::with_assignment`]): every host's
    /// directory read it asked for; the coordinator's land lookups (when it has any) answered
    /// from the rig's `super_regions` table after a `hosts` read — every live host beating, its
    /// load and drain mark — and what that decided committed, executed in the table and
    /// answered (lost while [`Self::assign_answers_lost`]); then every host's assigned rows
    /// fenced and taken (`take_assigned` → `claimed`).
    pub fn drive_assignment(&mut self) {
        if self.claim_store.is_none() {
            return;
        }
        self.rounds += 1;
        for h in host_ids(self.hosts.len() as u32) {
            if self.dead.contains(&h) {
                continue;
            }
            if self.hosts[h.0 as usize].manager.take_directory_wanted() {
                *self.reads_asked.entry(h).or_default() += 1;
                self.read_directory(h);
            }
        }
        if self.assigning && self.coordinator.is_some() {
            self.answer_land_lookups();
        }
        for h in host_ids(self.hosts.len() as u32) {
            if self.dead.contains(&h) {
                continue;
            }
            let rows = self.hosts[h.0 as usize].manager.take_assigned();
            self.fence_then_claim(h, rows);
        }
        for rig in &mut self.hosts {
            rig.settle();
        }
    }

    /// The `hosts` rows as the coordinator's ticker reads them: every host running (or that
    /// left, as the boot that left) beating at `now`, its row dead once a crash succession
    /// declared that boot, clean once it left, with its drain mark and load.
    fn hosts_seen(&self, now: u64) -> Vec<game::coordinator::HostSeen> {
        let declared_dead: BTreeSet<(HostId, u64)> =
            self.successions.iter().map(|(s, _)| (s.predecessor, s.predecessor_boot)).collect();
        host_ids(self.hosts.len() as u32)
            .filter(|h| !self.dead.contains(h) || self.left.contains_key(h))
            .map(|h| {
                let boot = self.boots[h.0 as usize];
                let clean = self.left.get(&h) == Some(&boot);
                let state = if declared_dead.contains(&(h, boot)) && !clean { game::HOST_DEAD } else { game::HOST_LIVE };
                game::coordinator::HostSeen {
                    ordinal: h,
                    name: self.names.get(&h).cloned().unwrap_or_default(),
                    boot,
                    clean,
                    state,
                    seen_ms: now + 1,
                    draining: self.draining.contains(&h),
                    load: self.loads.get(&h).copied().unwrap_or_default(),
                    landed: self.landed(h),
                }
            })
            .collect()
    }

    /// The ticker's `hosts` read as the runtime feeds it (`Input::HostsRead`, Elastic 4): the
    /// rows of [`Self::hosts_seen`] at the rig's latest host time, with the rig store's
    /// `desired_hosts` rows by slot, issued and received at that time. Refused on a replica that
    /// is not the ticker (as the core refuses it).
    pub fn read_hosts(&mut self) {
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let rows = self.hosts_seen(now);
        let mut desired: Vec<game::coordinator::DesiredHost> = self
            .desired
            .iter()
            .map(|(name, &(slot, draining, created_ms))| game::coordinator::DesiredHost { name: name.clone(), slot, draining, created_ms })
            .collect();
        desired.sort_by_key(|d| d.slot);
        let next_host_id = self.hosts.len() as u64;
        let tick = self.hosts.iter_mut().map(|r| r.manager.spawner_mut().world_tick()).max().unwrap_or(0);
        // The mailboxes' oldest rows (none under `Direct`, the rig's delivery).
        let mailbox_oldest = self.core().store().map(|s| s.mailbox_oldest()).unwrap_or_default();
        let read = game::coordinator::Input::HostsRead {
            rows,
            desired,
            mailbox_oldest,
            next_host_id,
            issued_mono: now,
            received_mono: now,
            world_tick: tick,
        };
        self.core().apply(read, now);
    }

    /// The ticker's land lookups ([`game::Coordinator::take_land_lookups`]), when it has any:
    /// a `hosts` read first ([`Self::hosts_seen`]: who may be assigned), then the owners read of
    /// the asked rows from the rig's `super_regions` table (`Input::LandOwners`), and what it
    /// decided committed, executed and answered. A rig without a table reads nothing.
    fn answer_land_lookups(&mut self) {
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let rows = self.hosts_seen(now);
        let asked = {
            let mut core = self.core();
            if !core.is_ticker() {
                return;
            }
            let asked = core.take_land_lookups();
            if asked.is_empty() {
                return;
            }
            core.observe_leases(&rows, now, now);
            asked
        };
        let Some(store) = &self.claim_store else { return };
        let owned: Vec<(game::SuperRegion, i64, u64)> = asked
            .iter()
            .filter_map(|(sr, _)| store.get(sr).map(|&(owner, epoch)| (*sr, owner.map_or(game::NO_OWNER, |o| o.0 as i64), epoch)))
            .collect();
        self.core().apply(game::coordinator::Input::LandOwners { asked, rows: owned }, now);
        self.core().commit();
        let answer = !self.assign_answers_lost;
        for a in self.execute_committed(answer) {
            if let game::coordinator::DecisionAnswer::Declared { outcome: Ok(game::Declared::Done(list)), .. } = a {
                self.land_declared.extend(list);
            }
        }
        // The answers' messages leave with the next transition (write-before-send).
        self.core().commit();
    }

    /// Host `h` dies: its manager and spawner are dropped (an empty placeholder keeps its
    /// slot and the world tick), its peer links go down for everyone (what is in transit is
    /// lost), the coordinator sees its link drop, and its parts staged in the rig store stay.
    /// What it held, and what was on its way to or from it, is no longer tracked
    /// ([`Self::forgiven`]): its regions come back from the store with the heir (Task 9).
    pub fn kill(&mut self, h: HostId) {
        assert!(!self.dead.contains(&h), "host {h} is dead already");
        // Its fence dies with its process.
        self.assigned_fences.remove(&h);
        let n = self.hosts.len() as u32;
        let mut gone: BTreeSet<EntityId> = BTreeSet::new();
        for (id, last) in &self.tracked {
            let on_h = match last {
                Some(Place::Region { host, .. } | Place::Manager { host } | Place::Parked { host, .. } | Place::Unowned { host, .. }) => {
                    *host == h
                }
                Some(Place::Unsent { host, peer }) => *host == h || *peer == h,
                Some(Place::Transit { to }) => *to == h,
                Some(Place::Stored { .. }) | None => false,
            };
            if on_h {
                gone.insert(*id);
            }
        }
        for (r, rig) in self.hosts.iter().enumerate() {
            for (peer, id, _) in rig.manager.peer_unreceived_ids() {
                if HostId(r as u64) == h || peer == h {
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
        // (A host started after another died has no link to it.)
        let linked: Vec<HostId> = host_ids(n).filter(|o| *o != h && self.links.contains_key(&(h.min(*o), h.max(*o)))).collect();
        for other in linked {
            self.link_down(h, other);
        }
        if self.coordinator.is_some() {
            self.core().host_detached(h);
        }
        let tick = self.hosts[h.0 as usize].manager.spawner_mut().world_tick();
        let now = self.hosts[h.0 as usize].now_ms;
        let mut spawner = InlineSpawner::default();
        spawner.set_world_tick(tick);
        let mut placeholder = Rig::with_spawner(spawner, super::seam_translate::uniform_floor);
        placeholder.now_ms = now;
        self.hosts[h.0 as usize] = placeholder;
        self.dead.insert(h);
    }

    /// A host process named `name` starts (Elastic 1: a restarted process is a NEW host): the
    /// rig issues it the next id — its own counter standing in for the store's `next_host_id`,
    /// never an id a host had before — with exactly one boot. It owns nothing (a `PgClaim`
    /// directory read from the rig's `super_regions` table: what it holds is what the table
    /// says, i.e. nothing until a succession or a claim gives it land), no keeper; a link to
    /// every live host and to the coordinator. Its id.
    pub fn start(&mut self, name: &str) -> HostId {
        let h = HostId(self.hosts.len() as u64);
        let boot = 7_000_000 + h.0;
        let tick = self.hosts.iter_mut().map(|r| r.manager.spawner_mut().world_tick()).max().unwrap_or(0);
        let mut spawner = InlineSpawner::default();
        spawner.set_world_tick(tick);
        self.hosts.push(Rig::with_spawner(spawner, super::seam_translate::uniform_floor));
        // No keeper: its "home" is never kept (a slot so every table stays indexed by id).
        self.homes.push(self.homes[0]);
        self.boots.push(boot);
        while self.world_drop_assigned.len() < self.hosts.len() {
            self.world_drop_assigned.push(Arc::default());
        }
        if self.coordinator.is_some() {
            self.world_sent.push(Arc::default());
            self.world_hold.push(Arc::default());
            self.world_hop_is.push(Arc::default());
            self.world_hold_orders.push(Arc::default());
        }
        self.dead.insert(h);
        // The name labels a process for people (`VOXEL_HOST_NAME`), and the desired row it runs
        // for (Elastic 4); it is never a key.
        if !name.is_empty() {
            self.names.insert(h, name.to_string());
        }
        self.start_process(h, boot);
        // Every live host learns the new one is live at its next directory read (claim on demand).
        if self.claim_store.is_some() {
            let live: Vec<HostId> = host_ids(self.hosts.len() as u32).filter(|o| !self.dead.contains(o) && *o != h).collect();
            for o in live {
                self.read_directory(o);
            }
        }
        h
    }

    /// Host slot `h` (dead, see [`Self::start`]) runs as process `boot`: a fresh manager on the
    /// rig store's committed state — its regions as stored, its journal from the last
    /// committed cut, a `PgClaim` directory read from the rig's `super_regions` table — with a
    /// new link to every live host and to the coordinator.
    fn start_process(&mut self, h: HostId, boot: u64) {
        assert!(self.dead.contains(&h), "host {h} runs");
        let n = self.hosts.len() as u32;
        let regions: BTreeMap<RegionCoords, StoredRegion> =
            self.committed.values().flat_map(|s| s.regions.iter().map(|(rc, r)| (*rc, r.clone()))).collect();
        let tick = self.hosts[h.0 as usize].manager.spawner_mut().world_tick();
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        let mut spawner = InlineSpawner::with_portable(regions);
        spawner.set_world_tick(tick);
        if let Some(f) = self.schedule {
            spawner.set_schedule(Some(f(mix(self.seed, h.0 ^ boot))));
        }
        let rows: Vec<(game::SuperRegion, Option<HostId>, u64)> = match (&self.claim_store, &self.directory) {
            (Some(store), _) => store.iter().map(|(sr, (o, e))| (*sr, *o, *e)).collect(),
            (None, Some(rows)) => rows.clone(),
            (None, None) => panic!("a host starts again only under `claim` (with_directory)"),
        };
        let mut rig = Rig::with_ownership(spawner, super::seam_translate::uniform_floor, Box::new(game::PgClaim::new(h)), self.s);
        rig.now_ms = now;
        rig.manager.set_endpoints(rig_endpoint(h), game::seeded_nonces(mix(self.seed, 2000 + h.0 + boot)));
        let base = self.last_cut.as_ref().map_or(CutId(0), |(c, _)| *c);
        rig.manager.start_peer_journal(base);
        let live: BTreeSet<HostId> = host_ids(n).filter(|o| !self.dead.contains(o) || *o == h).collect();
        let rows = rows.into_iter().map(|(sr, o, e)| (sr, o.map_or(game::NO_OWNER, |o| o.0 as i64), e)).collect();
        rig.manager.directory_read(rows, live);
        if let Some(ms) = self.land_idle_ms {
            rig.manager.set_land_idle_ms(ms);
        }
        self.hosts[h.0 as usize] = rig;
        self.dead.remove(&h);
        self.boots[h.0 as usize] = boot;
        // Registered as another boot: its row is dirty again.
        if self.left.get(&h).is_some_and(|left_as| *left_as != boot) {
            self.left.remove(&h);
        }
        for other in host_ids(n).filter(|o| *o != h && !self.dead.contains(o)) {
            let (a, b) = (h.min(other), h.max(other));
            let (ea, eb) = InlinePeerLink::pair(mix(self.seed, boot ^ a.0 * u64::from(n) + b.0), self.link_delay);
            let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
            let sa = self.strips.entry((a, b)).or_default().clone();
            let sb = self.strips.entry((b, a)).or_default().clone();
            let (ba, bb) = (self.boots[a.0 as usize], self.boots[b.0 as usize]);
            self.hosts[a.0 as usize].manager.set_peer(b, PeerSession::new(rig_hello_as(a, ba), b), Box::new(SharedEnd(ea.clone(), sa)));
            self.hosts[b.0 as usize].manager.set_peer(a, PeerSession::new(rig_hello_as(b, bb), a), Box::new(SharedEnd(eb.clone(), sb)));
            self.links.insert((a, b), (ea, eb));
        }
        if let Some(core) = self.coordinator.clone() {
            let inner = game::InlineWorldLink::shared(core, h).with_delay(mix(self.seed, 3000 + h.0 ^ boot), self.link_delay);
            let sent = Arc::new(Mutex::new(Vec::new()));
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let hop_is = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let hold_orders = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.world_hold_orders[h.0 as usize] = hold_orders.clone();
            self.world_sent[h.0 as usize] = sent.clone();
            self.world_hold[h.0 as usize] = hold.clone();
            self.world_hop_is[h.0 as usize] = hop_is.clone();
            let rig = &mut self.hosts[h.0 as usize];
            rig.manager.set_world_link(Box::new(RigWorldLink { inner, sent, hold, hop_is, hold_orders: hold_orders.clone(), stash: Default::default(), drop_assigned: self.world_drop_assigned[h.0 as usize].clone() }), world_boot(boot), now);
            rig.settle();
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
        if d.kind == game::SuccessionKind::Leave {
            // A leave needs the clean row of that very boot (`declare_succession`): a boot
            // registered since is another host (`Gone`), a dirty one did not leave.
            for &(pred, boot) in &d.dead {
                if self.boots[pred.0 as usize] != boot || self.successions.iter().any(|(s, _)| (s.predecessor, s.predecessor_boot) == (pred, boot)) {
                    return game::Declared::Gone(vec![pred]);
                }
                if self.left.get(&pred) != Some(&boot) {
                    return game::Declared::NotLapsed(vec![pred]);
                }
            }
        }
        let n = self.hosts.len() as u32;
        let base_cut = self.last_cut.as_ref().map_or(CutId(0), |(c, _)| *c);
        let predecessors: BTreeSet<HostId> = d.dead.iter().map(|(p, _)| *p).collect();
        // `declare_in`'s survivors (Elastic 2a), exactly: the dirty live `hosts` rows — a killed
        // host whose row is not dead yet (not declared, not ended) is one — holding land (a
        // `super_regions` or `in_flight` row) or named heir, minus the predecessors.
        let survivors: Vec<HostId> = host_ids(n)
            .filter(|h| !self.row_dead(*h) && self.left.get(h) != Some(&self.boots[h.0 as usize]) && !predecessors.contains(h))
            .filter(|h| self.landed(*h) || d.heirs.contains(h))
            .collect();
        let store = self.claim_store.as_mut().expect("with_assignment: the rig's super_regions table");
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

    /// Whether host `h` holds land in the rig store (`hosts().owns_land`): a row of the rig's
    /// `super_regions` table it owns, or an in-flight row its committed parts left. Without a
    /// `super_regions` table (`Static`) every host is landed.
    pub fn landed(&self, h: HostId) -> bool {
        let Some(store) = &self.claim_store else { return true };
        store.values().any(|(owner, _)| *owner == Some(h)) || self.committed.get(&h).is_some_and(|s| !s.in_flight.is_empty())
    }

    /// Whether host `h`'s rig `hosts` row (of its current boot) is dead: a crash succession
    /// declared it, or the store ended it ([`Self::end_landless_in_store`]).
    pub fn row_dead(&self, h: HostId) -> bool {
        let boot = self.boots[h.0 as usize];
        self.ended.contains(&(h, boot))
            || self.successions.iter().any(|(s, _)| s.kind == game::SuccessionKind::Crash && (s.predecessor, s.predecessor_boot) == (h, boot))
    }

    /// `assign_land_in` against the rig's `super_regions` table ([`Self::with_assignment`]): `Done`
    /// for an absent (epoch 1) or free (epoch + 1) row when the host's row is that live,
    /// dirty boot and not draining; otherwise the matching refusal, nothing written.
    pub fn assign_in_store(&mut self, a: game::coordinator::AssignLand) -> game::coordinator::Assigned {
        use game::coordinator::Assigned;
        let h = a.host;
        if self.boots.get(h.0 as usize) != Some(&a.boot) || self.row_dead(h) || self.left.get(&h) == Some(&a.boot) {
            return Assigned::HostNotLive;
        }
        if self.draining.contains(&h) {
            return Assigned::HostDraining;
        }
        let store = self.claim_store.as_mut().expect("with_assignment: the rig's super_regions table");
        let epoch = match store.get(&a.sr).copied() {
            None => 1,
            Some((None, epoch)) => epoch + 1,
            Some((Some(_), _)) => return Assigned::Owned,
        };
        store.insert(a.sr, (Some(h), epoch));
        Assigned::Done { epoch }
    }

    /// `fleet::add_host_in` against the rig's `desired_hosts` (Elastic 4): `NotNeeded` unless
    /// exactly `expect_wanted` rows are wanted, `Full` at `max` rows, else `h<next>` at the lowest
    /// free slot.
    pub fn add_host_in_store(&mut self, a: game::coordinator::AddHost) -> game::coordinator::HostAdded {
        use game::coordinator::HostAdded;
        let wanted = self.desired.values().filter(|(_, draining, _)| !draining).count();
        if wanted != a.expect_wanted as usize {
            return HostAdded::NotNeeded;
        }
        if self.desired.len() >= a.max as usize {
            return HostAdded::Full;
        }
        let used: BTreeSet<u32> = self.desired.values().map(|(slot, _, _)| *slot).collect();
        let slot = (0..).find(|s| !used.contains(s)).expect("a free slot");
        let name = format!("h{}", self.next_desired_host);
        self.next_desired_host += 1;
        let now = self.hosts.iter().map(|r| r.now_ms).max().unwrap_or(0);
        self.desired.insert(name.clone(), (slot, false, now));
        HostAdded::Done { name, slot }
    }

    /// `fleet::drain_host_in` against the rig's `desired_hosts` (Elastic 4): a notice longer
    /// than [`RIG_DRAIN_MAX_SECS`] is refused before anything is written (the store's
    /// `Unsupported`); `Unknown` for no row; else the row draining and, with `host` that live
    /// dirty boot's row, it marked draining (`Marked`), or not (`RowOnly`).
    pub fn drain_host_in_store(&mut self, d: &game::coordinator::DrainHost) -> Result<game::coordinator::HostDrained, String> {
        use game::coordinator::HostDrained;
        if d.in_ms > RIG_DRAIN_MAX_SECS * 1000 {
            return Err(format!(
                "a drain notice of {} ms is longer than the longest, {RIG_DRAIN_MAX_SECS} s (DRAIN_MAX_SECS)",
                d.in_ms
            ));
        }
        let Some(row) = self.desired.get_mut(&d.name) else { return Ok(HostDrained::Unknown) };
        row.1 = true;
        let Some((h, boot)) = d.host else { return Ok(HostDrained::RowOnly) };
        if self.boots.get(h.0 as usize) != Some(&boot) || self.row_dead(h) || self.left.get(&h) == Some(&boot) {
            return Ok(HostDrained::RowOnly);
        }
        self.draining.insert(h);
        Ok(HostDrained::Marked)
    }

    /// `fleet::remove_host_in` against the rig's `desired_hosts` (Elastic 4).
    pub fn remove_host_in_store(&mut self, r: &game::coordinator::RemoveHost) -> game::coordinator::HostRemoved {
        use game::coordinator::HostRemoved;
        if self.desired.remove(&r.name).is_some() { HostRemoved::Done } else { HostRemoved::Gone }
    }

    /// The rig store's `end_landless_host` (Elastic 2a): `Gone` unless `(host, boot)` is the
    /// current boot's live dirty row, `OwnsLand` while it holds land, `Recipient` while an open
    /// succession names it heir or survivor, else its row is dead — no succession, the
    /// generation untouched. The lapse is the caller's to have seen (the rig vouches a killed
    /// host in its reads, [`Self::drive_leases`]).
    pub fn end_landless_in_store(&mut self, end: game::coordinator::EndLandless) -> game::LandlessEnd {
        let (h, boot) = (end.host, end.boot);
        if self.boots[h.0 as usize] != boot || self.row_dead(h) || self.left.get(&h) == Some(&boot) {
            return game::LandlessEnd::Gone;
        }
        if self.landed(h) {
            return game::LandlessEnd::OwnsLand;
        }
        let open = self.open_successions();
        if self.successions.iter().any(|(s, _)| open.contains(&s.id) && (s.heir == h || s.survivors.contains(&h))) {
            return game::LandlessEnd::Recipient;
        }
        self.ended.insert((h, boot));
        game::LandlessEnd::Ended
    }

    /// The rig store's open successions (neither settled nor abandoned).
    pub fn open_successions(&self) -> BTreeSet<game::SuccessionId> {
        self.successions
            .iter()
            .filter(|(s, settled)| settled.is_none() && !self.abandoned.contains(&s.id))
            .map(|(s, _)| s.id)
            .collect()
    }

    /// One round of every heir's adoption reads ([`Self::with_adoption`]).
    pub fn drive_adoptions(&mut self) {
        if !self.adopting {
            return;
        }
        for h in host_ids(self.hosts.len() as u32) {
            if self.dead.contains(&h) {
                continue;
            }
            while let Some((id, pred)) = self.hosts[h.0 as usize].manager.take_adoption_wanted() {
                let delay = (mix(self.seed, self.rounds * 64 + 7000 + h.0) % u64::from(self.link_delay + 1)) as u32;
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
            let drop: BTreeSet<EntityId> = self.hosts[h.0 as usize].manager.adoption_drop_set(id).into_iter().collect();
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
            let m = &mut self.hosts[h.0 as usize].manager;
            if self.fail_adoption_loads {
                for rc in regions.keys() {
                    m.spawner_mut().fail_next_start(*rc);
                }
            }
            m.spawner_mut().add_portable(regions);
            m.adopted_rows(id, rows, located);
            self.adopted.push((id, h));
            self.hosts[h.0 as usize].settle();
        }
    }

    /// The rig's coordinator, locked ([`Self::with_coordinator`]).
    pub fn core(&self) -> std::sync::MutexGuard<'_, game::coordinator::RigCluster> {
        self.coordinator.as_ref().expect("a rig coordinator").lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Host `h`'s keeper: a client whose body stands in the middle of its home region.
    pub fn keep(&mut self, h: HostId) {
        let home = self.homes[h.0 as usize];
        let y = super::standing_y(super::FLOOR_TOP);
        let record = PlayerRecord {
            client: game::Client::default(),
            pose: game::Pose::from_translation(game::rapier3d::math::Vector::new(REGION_SIZE / 2.0, y, REGION_SIZE / 2.0)),
            region: home,
            adventure: None,
        };
        let rig = &mut self.hosts[h.0 as usize];
        rig.event(ServerEvent::ClientConnected { client: CLIENT, player: PlayerId(h.0), record: Some(record) });
    }

    /// `client` (session id) of `player` connects to host `home` as its home session,
    /// restored from `record`; tracked from now on.
    pub fn connect_home(&mut self, home: HostId, client: ClientId, player: PlayerId, record: PlayerRecord) {
        self.players.insert(client, player);
        let rig = &mut self.hosts[home.0 as usize];
        rig.event(ServerEvent::ClientConnected { client, player, record: Some(record) });
        // Its body, if the restore already gave it one, is tracked from now on.
        self.assert_conserved_world();
    }

    /// Every host records the packets its manager sends ([`Rig::packets_seen`]).
    pub fn record_packets(&mut self) {
        for rig in &mut self.hosts {
            rig.packets_seen = Some(Vec::new());
        }
    }

    /// Where `client`'s body is: `(host, region)` of every running region holding it.
    pub fn player_owners(&mut self, client: ClientId) -> Vec<(HostId, RegionCoords)> {
        let mut out = Vec::new();
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    if region.data().player_entites.contains_key(&client) {
                        out.push((HostId(h as u64), rc));
                    }
                });
            }
        }
        out
    }

    /// The `EntityId` of `client`'s body, if a running region holds it.
    pub fn player_body(&mut self, client: ClientId) -> Option<(HostId, RegionCoords, EntityId)> {
        let mut out = None;
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            for rc in rig.manager.running_regions() {
                rig.manager.spawner_mut().with_region(rc, |region| {
                    let data = region.data();
                    if let Some(&e) = data.player_entites.get(&client) {
                        if let Some(id) = *data.ecs.id.try_get(e) {
                            out = Some((HostId(h as u64), rc, id));
                        }
                    }
                });
            }
        }
        out
    }

    /// The hosts holding `player`'s parked record.
    pub fn parked_on(&self, player: PlayerId) -> Vec<HostId> {
        host_ids(self.hosts.len() as u32).filter(|h| self.hosts[(*h).0 as usize].manager.parked_record(player).is_some()).collect()
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
    pub fn owner(&self, rc: RegionCoords) -> HostId {
        let alive = (0..self.hosts.len()).find(|h| !self.dead.contains(&HostId(*h as u64))).expect("a live host");
        self.hosts[alive].manager.owner_host(rc).expect("an owned region")
    }

    /// The rig of the host that owns `rc`.
    pub fn host_of(&mut self, rc: RegionCoords) -> &mut Rig {
        let h = self.owner(rc);
        &mut self.hosts[h.0 as usize]
    }

    /// Keep `rc` running on its owner (its keeper subscribes to it).
    pub fn run(&mut self, rc: RegionCoords) {
        self.host_of(rc).event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
        assert!(self.host_of(rc).manager.running_regions().contains(&rc), "{rc:?} runs on its owner");
    }

    /// Take the link between hosts `a` and `b` down (what is in transit is lost).
    pub fn link_down(&mut self, a: HostId, b: HostId) {
        self.link(a, b).lock().unwrap().set_up(false);
    }

    fn link(&self, a: HostId, b: HostId) -> &Arc<Mutex<InlinePeerLink>> {
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
        self.drive_assignment();
        self.drive_adoptions();
        self.assert_conserved_world();
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
            let host = HostId(h as u64);
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
        }
        if parked_hold && !self.released.is_empty() {
            let mut live: BTreeSet<RegionCoords> = BTreeSet::new();
            for rig in &self.hosts {
                live.extend(rig.manager.running_regions());
                live.extend(rig.manager.parked_regions());
            }
            for store in self.committed.values() {
                for (rc, region) in &store.regions {
                    if self.released.contains(&game::super_region_of(*rc, self.s)) && !live.contains(rc) {
                        for b in &region.entities {
                            places.entry(b.id).or_default().push(Place::Stored { rc: *rc });
                        }
                    }
                }
            }
        }
        for (h, rig) in self.hosts.iter_mut().enumerate() {
            let host = HostId(h as u64);
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
            for o in host_ids(n) {
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
        let mut per_player: BTreeMap<PlayerId, (Vec<(ClientId, HostId, RegionCoords)>, usize)> = BTreeMap::new();
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
            let homes: Vec<(HostId, RegionCoords)> = host_ids(self.hosts.len() as u32)
                .filter_map(|h| self.hosts[h.0 as usize].manager.home_of(client).map(|rc| (h, rc)))
                .collect();
            assert!(homes.len() <= 1, "client {client} has a home on several hosts: {homes:?}");
            let online: Vec<HostId> = host_ids(self.hosts.len() as u32)
                .filter(|h| self.hosts[(*h).0 as usize].manager.player_of(client) == Some(player))
                .collect();
            assert!(online.len() <= 1, "player {} is online on several hosts: {online:?}", player.0);
            let claims: Vec<(HostId, game::Claim)> = host_ids(self.hosts.len() as u32)
                .filter_map(|h| self.hosts[h.0 as usize].manager.client_claim(client).map(|c| (h, c)))
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
