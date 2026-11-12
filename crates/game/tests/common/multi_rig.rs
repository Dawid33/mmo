//! Several region hosts of one world in one test: a [`Rig`] per host (its own
//! [`game::WorldManager`] on an [`game::InlineSpawner`], owning the regions
//! `Ownership::Static` gives it), every pair of hosts joined by a seeded, delaying
//! [`InlinePeerLink`] under a [`PeerSession`] at each end. Each host's world tick is its
//! spawner's round count, so [`MultiRig::round`] ticks every host every round and checks
//! they agree. Every host-seam test builds on it.
use super::rig::{Rig, CLIENT};
use game::{
    ClientId, ClientPacket, EntityId, HostEndpoint, Intent, IntentMsg, InlinePeerLink, InlineSchedule, InlineSpawner, Ownership, PeerHello,
    PeerLink, PeerMsg, PeerSession, PlayerId, PlayerRecord, RegionCoords, RegionSpawner, Reliable, ServerEvent,
    REGION_SIZE, TICK_RATE,
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

/// One end of a link the rig keeps a handle on (to take it down, read its transit).
pub struct SharedEnd(pub Arc<Mutex<InlinePeerLink>>);

impl SharedEnd {
    fn end(&self) -> std::sync::MutexGuard<'_, InlinePeerLink> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl PeerLink for SharedEnd {
    fn send(&mut self, msg: PeerMsg) {
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
    PeerHello {
        ordinal,
        boot: 1000 + u64::from(ordinal),
        generation: RIG_GENERATION,
        endpoint: rig_endpoint(ordinal),
        received_through: 0,
        received_from_boot: None,
    }
}

/// The adjacent pair of regions nearest the origin whose owners differ (`hosts` hosts,
/// `s` regions per super-region side): `(a, b)`, `b` one step +x or +z of `a`.
pub fn host_seam(hosts: u32, s: u32) -> (RegionCoords, RegionCoords) {
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts }.owner_of(rc, s);
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
    let owner = |rc: RegionCoords| Ownership::Static { ordinal: 0, hosts: 4 }.owner_of(rc, s);
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
    /// The stud bodies seen: they may end by settling in the region that owns them.
    pub settling: BTreeSet<EntityId>,
    /// Items the world holds, once a test put some in ([`Self::assert_items`]): checked
    /// every round.
    pub items: Option<u64>,
    /// The players a test connected ([`Self::connect_home`]), by session id: each is
    /// conserved across hosts every round (see [`Self::assert_conserved_world`]).
    pub players: BTreeMap<ClientId, PlayerId>,
    /// The bodies those players were seen in: one may end by parking.
    pub player_bodies: BTreeMap<EntityId, PlayerId>,
}

/// Host `h`'s endpoint in the rig (what its sessions' first packet names).
pub fn rig_endpoint(h: u32) -> HostEndpoint {
    HostEndpoint { ordinal: h, quic: None, webtransport: None, cert_hash: None }
}

impl MultiRig {
    /// `hosts` hosts of one stud world (flat floor everywhere), `s` regions per
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
        let floor: game::InlineStudGenesis = Arc::new(|_| super::uniform_seed());
        let mut rigs = Vec::new();
        let mut homes = Vec::new();
        for h in 0..hosts {
            let ownership = Ownership::Static { ordinal: h, hosts };
            let spawner = schedule.map_or_else(InlineSpawner::default, |f| InlineSpawner::with_schedule(f(mix(seed, u64::from(h)))));
            let mut rig = Rig::with_ownership(spawner, floor.clone(), ownership, s);
            rig.manager.set_endpoints(rig_endpoint(h), mix(seed, 2000 + u64::from(h)));
            rigs.push(rig);
            let home = nearest_first(64)
                .into_iter()
                .map(|rc| RegionCoords::new(rc.x + 32, rc.z + 32))
                .find(|rc| ownership.owns(*rc, s))
                .expect("a host owns a region near (32, 32)");
            homes.push(home);
        }
        let mut links = BTreeMap::new();
        for a in 0..hosts {
            for b in a + 1..hosts {
                let (ea, eb) = InlinePeerLink::pair(mix(seed, 1000 + u64::from(a * hosts + b)), link_delay_steps);
                let (ea, eb) = (Arc::new(Mutex::new(ea)), Arc::new(Mutex::new(eb)));
                rigs[a as usize].manager.set_peer(b, PeerSession::new(rig_hello(a), b), Box::new(SharedEnd(ea.clone())));
                rigs[b as usize].manager.set_peer(a, PeerSession::new(rig_hello(b), a), Box::new(SharedEnd(eb.clone())));
                links.insert((a, b), (ea, eb));
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
                items: None,
                players: BTreeMap::new(),
                player_bodies: BTreeMap::new(),
            };
        for h in 0..hosts {
            rig.keep(h);
        }
        rig.connect();
        rig
    }

    /// Host `h`'s keeper: a client whose body stands in the middle of its home region.
    fn keep(&mut self, h: u32) {
        let home = self.homes[h as usize];
        let y = super::standing_y(super::FLOOR_PLATES);
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
        self.hosts[0].manager.owner_of(rc)
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
        self.hosts[a as usize].manager.set_peer(b, PeerSession::new(rig_hello(a), b), Box::new(SharedEnd(ea.clone())));
        self.hosts[b as usize].manager.set_peer(a, PeerSession::new(rig_hello(b), a), Box::new(SharedEnd(eb.clone())));
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
    /// may absorb, arrivals a full mailbox despawned, and stud bodies that settled where
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
                        if data.ecs.stud_body.try_get(key).is_some() {
                            settling.insert(*id);
                        }
                    }
                });
            }
            for (id, _) in rig.manager.in_flight_ids().into_iter().chain(rig.manager.pending_arrival_ids()) {
                places.entry(id).or_default().push(Place::Manager { host });
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
                    if let PeerMsg::Reliable { body: Reliable::Transfer { bundle, .. }, .. } = msg {
                        places.entry(bundle.id).or_default().push(Place::Transit { to });
                    }
                }
            }
        }
        self.settling.extend(settling);
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
        let in_move = self.players_in_move();
        let bodies_now: BTreeSet<PlayerId> =
            places.keys().filter_map(|id| self.player_bodies.get(id)).copied().collect();
        let parked: Vec<EntityId> = self
            .tracked
            .keys()
            .filter(|id| !places.contains_key(id) && !shadows.contains(id))
            .filter(|id| {
                self.player_bodies.get(id).is_some_and(|p| {
                    !self.parked_on(*p).is_empty() || in_move.contains_key(p) || bodies_now.contains(p)
                })
            })
            .copied()
            .collect();
        for id in parked {
            self.tracked.remove(&id);
        }
        for (id, last) in &self.tracked {
            assert!(
                places.contains_key(id) || shadows.contains(id),
                "{id:?} vanished (last seen {last:?}, None = sent and unacknowledged)"
            );
        }
        self.tracked.retain(|id, _| !people.contains(id));
        for id in shadows {
            if !people.contains(&id) && !places.contains_key(&id) {
                self.tracked.insert(id, None);
            }
        }
        for (id, at) in places {
            if !people.contains(&id) {
                self.tracked.insert(id, Some(at[0].clone()));
            }
        }
        if let Some(expected) = self.items {
            self.assert_items(expected);
        }
        self.assert_players();
    }

    /// Each player whose record is in a [`Reliable::PlayerMove`] never sent or in transit
    /// on a link, with how many such places hold it; a move a session SENT and the peer
    /// has not acknowledged (the acknowledgement lag: the peer may hold it) is listed with
    /// no place of its own.
    fn queued_moves(&self) -> BTreeMap<PlayerId, usize> {
        let mut out: BTreeMap<PlayerId, usize> = BTreeMap::new();
        for rig in &self.hosts {
            for (_, p, sent) in rig.manager.peer_unreceived_moves() {
                *out.entry(p).or_default() += usize::from(!sent);
            }
        }
        for (ea, eb) in self.links.values() {
            for end in [ea, eb] {
                for msg in end.lock().unwrap().transit() {
                    if let PeerMsg::Reliable { body: Reliable::PlayerMove { handoff, .. }, .. } = msg {
                        *out.entry(handoff.player).or_default() += 1;
                    }
                }
            }
        }
        out
    }

    /// Each player whose record is on its way to a new body — [`Self::queued_moves`], or a
    /// body's creation in flight on a host (`creating_players`) — with how many places
    /// hold it.
    fn players_in_move(&self) -> BTreeMap<PlayerId, usize> {
        let mut out = self.queued_moves();
        for rig in &self.hosts {
            for p in rig.manager.creating_players() {
                *out.entry(p).or_default() += 1;
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
    fn assert_players(&mut self) {
        let players: Vec<(ClientId, PlayerId)> = self.players.iter().map(|(c, p)| (*c, *p)).collect();
        let in_move = self.players_in_move();
        let queued_moves = self.queued_moves();
        for (client, player) in players {
            let bodies = self.player_owners(client);
            let parked = self.parked_on(player);
            let moving = in_move.get(&player).copied().unwrap_or(0);
            let queued = queued_moves.get(&player).copied().unwrap_or(0);
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
                    if let PeerMsg::Reliable { body: Reliable::Intent(m), .. } = msg {
                        held += msg_items(&m);
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
