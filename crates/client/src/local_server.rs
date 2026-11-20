//! Embedded single-player "server" for the wasm build: the same
//! WorldManager core the real server runs, with regions pumped inline
//! (no threads in the browser), behind the same channel interface
//! netcode::ServerConnection provides on native. A future
//! WebTransport/WebSocket transport replaces this without touching
//! GameInstanceManager.
use crossbeam::channel::{Receiver, Sender, unbounded};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameError, InlineSpawner,
    RegionCoords, RegionGenerator, RegionOutput, ServerEvent, ServerPacket, SubChunk, TICK_RATE,
    WorldManager,
};

/// The only client in an offline world.
pub const LOCAL_CLIENT_ID: ClientId = 0;
/// Its player: there is no network edge here to resolve a `Hello`, so the
/// in-process session is announced as this player directly.
pub const LOCAL_PLAYER_ID: game::PlayerId = game::PlayerId(LOCAL_CLIENT_ID as u64);

pub struct LocalServer {
    manager: WorldManager<InlineSpawner>,
    recv: Receiver<ClientPacket>,
    send: Sender<ServerPacket>,
    out_recv: Receiver<(Option<ClientId>, ServerPacket)>,
    region_out_recv: Receiver<(RegionCoords, RegionOutput)>,
    /// Monotonic sim-time for grace-period lifecycle; advances TICK_RATE ms
    /// per authoritative tick (no wall clock on wasm).
    now_ms: u64,
}

impl LocalServer {
    pub fn new(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
    ) -> Result<Self, GameError> {
        let (materials, manifest) = terrain_materials();
        let gen_cache = worldgen::cache::WorldgenCache::from_env(worldgen::WORLD_SEED, &manifest);
        let generator: RegionGenerator =
            Box::new(move |rc| gen_cache.generate_cached(rc, worldgen::WORLD_SEED, materials));
        Self::with_generator_and_materials(recv, send, generator, materials)
    }

    /// Construct the offline server with an injected region generator.
    /// Intended for integration harnesses whose subject is networking,
    /// reconciliation, or window management rather than production worldgen.
    #[doc(hidden)]
    pub fn new_with_generator(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
        generator: RegionGenerator,
    ) -> Result<Self, GameError> {
        let (materials, _) = terrain_materials();
        Self::with_generator_and_materials(recv, send, generator, materials)
    }

    fn with_generator_and_materials(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
        generator: RegionGenerator,
        materials: worldgen::material::Materials,
    ) -> Result<Self, GameError> {
        let mut server = Self::unconnected_with_materials(recv, send, generator, materials)?;
        server.connect_local_client();
        Ok(server)
    }

    /// [`Self::new_with_generator`] with no session yet: one host of several
    /// (`harness::SimHarness::with_hosts`) is configured first (its ownership,
    /// endpoint, peers), then the local client connects to its home host
    /// ([`Self::connect_local_client`]) or observes another
    /// ([`Self::observer_connected`]).
    #[doc(hidden)]
    pub fn new_unconnected(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
        generator: RegionGenerator,
    ) -> Result<Self, GameError> {
        let (materials, _) = terrain_materials();
        Self::unconnected_with_materials(recv, send, generator, materials)
    }

    fn unconnected_with_materials(
        recv: Receiver<ClientPacket>,
        send: Sender<ServerPacket>,
        generator: RegionGenerator,
        materials: worldgen::material::Materials,
    ) -> Result<Self, GameError> {
        let (out_send, out_recv) = unbounded();
        let (region_out_send, region_out_recv) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
            coarse: Box::new(move |key| {
                worldgen::coarse_tile(key, worldgen::WORLD_SEED, materials)
            }),
            water: Box::new(move |key| {
                worldgen::coarse_water(key, worldgen::WORLD_SEED, materials)
            }),
            out: lod_result_send,
        });
        let manager = WorldManager::new(
            InlineSpawner::default(),
            generator,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        Ok(Self {
            manager,
            recv,
            send,
            out_recv,
            region_out_recv,
            now_ms: 0,
        })
    }

    /// Server-authoritative player creation, as on ClientConnected. The
    /// handshake is bypassed: the session starts as the local player.
    #[doc(hidden)]
    pub fn connect_local_client(&mut self) {
        self.manager.handle_server_event(
            ServerEvent::ClientConnected {
                client: LOCAL_CLIENT_ID,
                player: LOCAL_PLAYER_ID,
                record: None,
            },
            self.now_ms,
        );
        self.drain();
    }

    /// The local client's observer session here (another host is its home): what a
    /// server's edge announces once an observer ticket was admitted. Harness hook.
    #[doc(hidden)]
    pub fn observer_connected(&mut self, client: ClientId, player: game::PlayerId) {
        self.manager.handle_server_event(
            ServerEvent::ObserverConnected { client, player },
            self.now_ms,
        );
        self.drain();
    }

    /// The local client's session here ended (its connection closed). Harness hook.
    #[doc(hidden)]
    pub fn client_disconnected(&mut self) {
        self.manager.handle_server_event(
            ServerEvent::ClientDisconnected(LOCAL_CLIENT_ID),
            self.now_ms,
        );
        self.drain();
    }

    /// Exchange what is queued with the peer hosts now (a step's departures over a host
    /// seam reach the host they go to before its regions' next tick). Harness hook.
    #[doc(hidden)]
    pub fn pump_peers(&mut self) {
        self.manager.pump_peers(self.now_ms);
        self.drain();
    }

    /// The world manager itself (multi-host harness wiring: ownership, peers,
    /// observer orders). Harness hook.
    #[doc(hidden)]
    pub fn manager_mut(&mut self) -> &mut WorldManager<InlineSpawner> {
        &mut self.manager
    }

    /// Jitter the inline regions' scheduling from a seed (see
    /// `game::InlineSchedule`); `None` restores lockstep. Harness hook.
    #[doc(hidden)]
    pub fn set_schedule(&mut self, schedule: Option<game::InlineSchedule>) {
        self.manager.spawner_mut().set_schedule(schedule);
    }

    /// Drain pending client packets without blocking.
    pub fn pump(&mut self) -> Result<(), GameError> {
        while let Ok(packet) = self.recv.try_recv() {
            // Quit is a no-op offline (no process to stop); the manager
            // returning false is deliberately ignored here.
            let _ = self.manager.handle_server_event(
                ServerEvent::ClientPacket(packet, LOCAL_CLIENT_ID),
                self.now_ms,
            );
        }
        self.drain();
        Ok(())
    }

    /// Advance every running region one tick and run lifecycle upkeep.
    pub fn tick(&mut self) {
        self.now_ms += TICK_RATE;
        self.manager.spawner_mut().tick_all();
        self.manager.maintain(self.now_ms);
        self.drain();
    }

    /// Pump inline regions and route their outputs, twice: outputs can
    /// trigger follow-up work (resubscribe-after-park, respawn snapshots)
    /// that needs one more pump to answer within the same frame. Then
    /// forward everything to the single client (all packets are ours).
    fn drain(&mut self) {
        for _ in 0..2 {
            self.manager.spawner_mut().pump();
            self.manager.pump_lod_results();
            while let Ok((rc, output)) = self.region_out_recv.try_recv() {
                self.manager.handle_region_output(rc, output, self.now_ms);
            }
        }
        while let Ok((_target, packet)) = self.out_recv.try_recv() {
            let _ = self.send.send(packet);
        }
        // No edge here holds observer orders to revoke: the list would only grow.
        let _ = self.manager.take_observer_revocations();
    }

    /// Current sim tick of a running region, or 0 if it isn't running
    /// (not yet spawned / already cycled out). Test/harness hook.
    pub fn region_tick(&mut self, rc: RegionCoords) -> game::Tick {
        let mut tick = 0;
        self.manager
            .spawner_mut()
            .with_region(rc, |r| tick = r.current_tick());
        tick
    }

    /// Run `f` against a running server region (state surgery before a
    /// client loads it, assertions). Test/harness hook.
    #[doc(hidden)]
    pub fn with_region(&mut self, rc: RegionCoords, f: impl FnOnce(&mut game::Region)) {
        self.manager.spawner_mut().with_region(rc, f);
    }

    /// Coords of every region currently running on the server. Test/harness hook.
    pub fn running_regions(&mut self) -> Vec<RegionCoords> {
        self.manager.spawner_mut().running()
    }

    /// Bit-exact state hash of a running region's authoritative data, via
    /// the same `game::state_hash` the rollback log uses. Test/harness hook.
    pub fn region_hash(&mut self, rc: RegionCoords) -> Option<u32> {
        let mut hash = None;
        self.manager
            .spawner_mut()
            .with_region(rc, |r| hash = Some(game::state_hash(r.data())));
        hash
    }

    /// Test/harness hook: record every region's state hash at the START of
    /// each slot — right after its Tick event, before the held inputs and
    /// transfers of the slot (what the desync check hashes).
    #[doc(hidden)]
    pub fn record_slot_start_hashes(&mut self) {
        self.manager.spawner_mut().record_slot_start_hashes();
    }

    /// `(region, tick, hash)` recorded since the last call.
    #[doc(hidden)]
    pub fn take_slot_start_hashes(&mut self) -> Vec<(RegionCoords, game::Tick, u32)> {
        self.manager.spawner_mut().take_slot_start_hashes()
    }

    /// Authoritative teleport of the single local player, wherever its home
    /// region currently runs. Undo-safe (`Region::with_data` runs the pose
    /// mutation in its own forgotten transaction). Test/harness hook.
    pub fn teleport_local_player(&mut self, pos: [f32; 3]) {
        let pose = game::Pose::translation(pos[0], pos[1], pos[2]);
        for rc in self.manager.spawner_mut().running() {
            let mut found = false;
            self.manager.spawner_mut().with_region(rc, |r| {
                if let Some(key) = r.data().player_entites.get(&LOCAL_CLIENT_ID).copied() {
                    r.with_data(|d| d.set_body_pose_safe(key, pose));
                    found = true;
                }
            });
            if found {
                break;
            }
        }
    }
}

fn terrain_materials() -> (worldgen::material::Materials, String) {
    let registry = crate::blocks::load_registry();
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    (
        worldgen::material::Materials::from_names(resolve),
        crate::blocks::manifest_src(),
    )
}

/// A one-column flat region used by integration harnesses. It retains real
/// terrain/collider/LOD plumbing while avoiding 1,024-column production
/// worldgen for tests whose assertions do not depend on terrain shape.
#[doc(hidden)]
pub fn lightweight_test_generator() -> RegionGenerator {
    Box::new(|_| {
        let chunk = Chunk::flat_floor(8);
        let column = Column::new(
            0,
            Block::new(BlockId::AIR),
            Block::new(BlockId::AIR),
            vec![SubChunk::Dense(Box::new(chunk))],
        );
        vec![(ColumnCoords::new(4, 4), column)]
    })
}
