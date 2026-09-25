//! Embedded single-player "server" for the wasm build: the same
//! WorldManager core the real server runs, with regions pumped inline
//! (no threads in the browser), behind the same channel interface
//! netcode::ServerConnection provides on native. A future
//! WebTransport/WebSocket transport replaces this without touching
//! GameInstanceManager.
use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, GameError, InlineSpawner,
    RegionCoords, RegionGenerator, RegionOutput, ServerEvent, ServerPacket, SubChunk, WorldManager,
    TICK_RATE,
};

/// The only client in an offline world.
pub const LOCAL_CLIENT_ID: ClientId = 0;

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
        let (out_send, out_recv) = unbounded();
        let (region_out_send, region_out_recv) = unbounded();
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn game::LodBuilder> = Box::new(game::InlineLodBuilder {
            coarse: Box::new(move |key| {
                worldgen::coarse_tile(key, worldgen::WORLD_SEED, materials)
            }),
            out: lod_result_send,
        });
        let mut manager = WorldManager::new(
            if game::CELL_CHUNKS == 0 {
                InlineSpawner::default()
            } else {
                InlineSpawner::with_cell_generator(
                    game::CELL_CHUNKS,
                    std::sync::Arc::new(move |rc| -> game::CellGenerator {
                        Box::new(move |cell| {
                            worldgen::generate_cell(
                                rc,
                                cell,
                                game::CELL_CHUNKS,
                                worldgen::WORLD_SEED,
                                materials,
                            )
                        })
                    }),
                )
            },
            generator,
            out_send,
            region_out_send,
            lod_builder,
            lod_result_recv,
        );
        // Server-authoritative player creation, as on ClientConnected.
        manager.handle_server_event(ServerEvent::ClientConnected(LOCAL_CLIENT_ID), 0);
        let mut server = Self {
            manager,
            recv,
            send,
            out_recv,
            region_out_recv,
            now_ms: 0,
        };
        server.drain();
        Ok(server)
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
    }

    /// Current sim tick of a running region, or 0 if it isn't running
    /// (not yet spawned / already cycled out). Test/harness hook.
    pub fn region_tick(&mut self, rc: RegionCoords) -> usize {
        let mut tick = 0;
        self.manager
            .spawner_mut()
            .with_region(rc, |r| tick = r.current_tick());
        tick
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

    /// Authoritative teleport of the single local player, wherever its home
    /// region currently runs. Undo-safe (`Region::with_data` runs the pose
    /// mutation in its own forgotten transaction). Test/harness hook.
    pub fn teleport_local_player(&mut self, pos: [f32; 3]) {
        let pose = game::IsometryReal::from_parts(
            game::na::Translation3::new(pos[0].into(), pos[1].into(), pos[2].into()),
            game::na::Unit::<game::na::Quaternion<game::parry::math::Real>>::identity(),
        );
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
        worldgen::material::Materials {
            stone: resolve("stone"),
            dirt: resolve("dirt"),
            grass: resolve("grass"),
            sand: resolve("sand"),
            water: resolve("water"),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GameInstanceManager;
    use game::{ClientUpdateEvent, ServerPacket};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    /// Full offline handshake: manager + LocalServer wired over channels,
    /// pumped in lockstep like the wasm frame driver will.
    #[test]
    fn offline_handshake_loads_region() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        // LOD off + home region only: every ring key and every near-window
        // region is a synchronous LocalServer build (see SimHarness::new).
        // This test is about the handshake, not the window or LOD.
        manager.set_lod_table(&[]);
        manager.set_near_radius(0);

        let mut server = LocalServer::new_with_generator(
            manager.client_packet_recv(),
            server_send,
            lightweight_test_generator(),
        )
        .unwrap();
        manager.start();

        // A few frames of the wasm drive loop: server pump -> client pump.
        for _ in 0..6 {
            server.pump().unwrap();
            assert!(manager.pump(&server_recv).unwrap());
        }

        // Client received region + player identity. NewRegion carries the
        // render bridge's Receiver; keep every one alive so no region's
        // Sender hits a closed channel when later ticks emit render updates.
        let mut saw_region = false;
        let mut saw_player = false;
        let mut bridge_recv = Vec::new();
        while let Ok(ev) = client_recv.try_recv() {
            match ev {
                ClientUpdateEvent::NewRegion(_, _, recv) => {
                    saw_region = true;
                    bridge_recv.push(recv);
                }
                ClientUpdateEvent::SetPlayer(id) => {
                    saw_player = true;
                    assert_eq!(id, LOCAL_CLIENT_ID);
                }
                _ => {}
            }
        }
        assert!(saw_region, "client never received the region snapshot");
        assert!(saw_player, "client never learned its ClientId");

        // Server ticks advance the authoritative world and reach the client.
        server.tick();
        manager.pump(&server_recv).unwrap();

        // Client ticks advance the local prediction.
        manager.send_tick();
        assert!(manager.pump(&server_recv).unwrap());
        drop(bridge_recv);
    }

    /// The offline client loads the full 3x3 window through the shared
    /// WorldManager core.
    #[test]
    fn offline_window_loads_nine_regions() {
        let (game_send, game_recv) = crossbeam::channel::unbounded();
        let (client_send, client_recv) = crossbeam::channel::unbounded::<ClientUpdateEvent>();
        let (server_send, server_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let dummy_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0));
        let mut manager = GameInstanceManager::new(
            game_send.clone(),
            game_recv,
            client_send,
            dummy_addr,
            game::DebugFlags::default(),
        );
        // LOD off: every ring key is a synchronous LocalServer build (see
        // SimHarness::new). This test is about the 3x3 window, not LOD.
        manager.set_lod_table(&[]);
        let mut server = LocalServer::new_with_generator(
            manager.client_packet_recv(),
            server_send,
            lightweight_test_generator(),
        )
        .unwrap();
        manager.start();

        // Handshake + window subscription needs a few pump rounds:
        // RequestPlayerRegion -> PlayerRegion -> 9x RequestRegionConnection -> 9x Region.
        for _ in 0..6 {
            server.pump().unwrap();
            assert!(manager.pump(&server_recv).unwrap());
        }

        let mut loaded = std::collections::BTreeSet::new();
        let mut receivers = Vec::new(); // keep bridge receivers alive
        while let Ok(ev) = client_recv.try_recv() {
            if let ClientUpdateEvent::NewRegion(rc, _, recv) = ev {
                loaded.insert(rc);
                receivers.push(recv);
            }
        }
        let expected: std::collections::BTreeSet<_> = game::RegionCoords::new(0, 0)
            .window_3x3()
            .into_iter()
            .collect();
        assert_eq!(loaded, expected, "3x3 offline window loaded");
        drop(receivers);
    }

    /// Release + grace parks a region offline; resubscribing restores it
    /// from the parking lot (a fresh Region packet arrives again).
    #[test]
    fn offline_release_parks_and_resubscribe_restores() {
        use game::{RegionCoords, TICK_RATE, UNLOAD_GRACE_MS};

        let (packet_send, packet_recv) = crossbeam::channel::unbounded::<ClientPacket>();
        let (out_send, out_recv) = crossbeam::channel::unbounded::<ServerPacket>();
        let mut server =
            LocalServer::new_with_generator(packet_recv, out_send, lightweight_test_generator())
                .unwrap();

        let corner = RegionCoords::new(-1, -1);
        packet_send
            .send(ClientPacket::RequestRegionConnection(corner))
            .unwrap();
        server.pump().unwrap();
        assert!(
            out_recv
                .try_iter()
                .any(|p| matches!(p, ServerPacket::Region(rc, _) if rc == corner)),
            "snapshot on first subscribe"
        );

        packet_send
            .send(ClientPacket::ReleaseRegionConnection(corner))
            .unwrap();
        server.pump().unwrap();
        // tick() advances the internal clock TICK_RATE ms per call; run past
        // the grace period so maintain() parks the corner.
        for _ in 0..(UNLOAD_GRACE_MS / TICK_RATE + 2) {
            server.tick();
        }
        while out_recv.try_recv().is_ok() {} // discard tick traffic

        packet_send
            .send(ClientPacket::RequestRegionConnection(corner))
            .unwrap();
        server.pump().unwrap();
        assert!(
            out_recv
                .try_iter()
                .any(|p| matches!(p, ServerPacket::Region(rc, _) if rc == corner)),
            "parked region restored on resubscribe"
        );
    }
}
