//! Server-authored session moves for Play: `CreateClientAt` lands a player
//! over a chosen block column, `ClientLeft` takes the player out again —
//! both ordinary undoable sim events.
use game::{Chunk, ChunkCoords, GameEvent, GameEventKind, Region, RegionId, Rollback, BLOCK_VOXELS};

/// The block column `CreateClientAt` targets in these tests.
const COLUMN: [u32; 2] = [5, 7];

// A floor whose top face is at Y = 128 under the spawn column (region-local
// voxel (128,128), as in spawn_surface.rs) and under `COLUMN`.
fn region_with_floor() -> Region {
    let cu = game::CHUNK_UNITS;
    let (cy, depth) = ((128 - 1) / cu, 128 - (128 - 1) / cu * cu);
    let floor = |x: usize, z: usize| (ChunkCoords::new(x / cu, cy as i32, z / cu), Chunk::flat_floor(depth as u32));
    let mut chunks = vec![floor(128, 128)];
    let col = floor(COLUMN[0] as usize * BLOCK_VOXELS, COLUMN[1] as usize * BLOCK_VOXELS);
    if col.0 != chunks[0].0 {
        chunks.push(col);
    }
    Region::from_chunks(RegionId::new(0, 0), chunks)
}

fn player_translation(region: &Region, client: game::ClientId) -> (f32, f32, f32) {
    let data = region.data();
    let e = *data.player_entites.get(&client).unwrap();
    let handle = *data.ecs.rigidbody.get(e);
    let t = data.physics.bodies.get(handle).unwrap().translation();
    (t.x, t.y, t.z)
}

#[test]
fn create_client_at_lands_over_the_column() {
    // The kit compared below reads the process-global block props.
    crate::common::install_real_block_props();
    let mut region = region_with_floor();
    region.handle_event(GameEventKind::CreateClientAt(0, COLUMN, None)).unwrap();
    let (x, y, z) = player_translation(&region, 0);
    let bv = BLOCK_VOXELS as f32;
    let (cx, cz) = ((COLUMN[0] as f32 + 0.5) * bv, (COLUMN[1] as f32 + 0.5) * bv);
    assert!((x - cx).abs() <= 0.5 * bv, "x={x} not over column centre {cx}");
    assert!((z - cz).abs() <= 0.5 * bv, "z={z} not over column centre {cz}");
    let surface = region.data().surface_voxel_y(cx as i32, cz as i32).expect("floor under the column");
    assert!(y > surface as f32, "body y={y} must be above surface {surface}");
    let client = region.data().clients.get(&0).expect("client 0 created");
    assert_eq!(client.inventory, game::Inventory::starting_kit());
}

#[test]
fn client_left_removes_body_and_client() {
    let mut region = region_with_floor();
    let before = region.data().ecs.entities.len();
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    assert!(region.data().ecs.entities.len() > before);
    region.handle_event(GameEventKind::ClientLeft(0)).unwrap();
    let data = region.data();
    assert!(data.player_entites.get(&0).is_none(), "player entity mapping left behind");
    assert!(data.clients.get(&0).is_none(), "client entry left behind");
    assert_eq!(data.ecs.entities.len(), before, "entity count back to the pre-create count");
}

#[test]
fn client_left_undoes_bit_exact() {
    game::set_hash_mode(game::HashMode::Full);
    // Undo replays render events through a live sender (`from_chunks`
    // wires `None`, which the undo closures unwrap).
    let (send, _recv) = crossbeam::channel::unbounded();
    let built = region_with_floor();
    let mut region = Region::new(built.data().clone(), Some(send), RegionId::new(0, 0), None);
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::ClientLeft(0)).unwrap();
    assert_ne!(before, game::state_hash(region.data()));
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()), "ClientLeft must invert bit-exact");
}

#[test]
fn client_left_of_a_missing_player_is_a_no_op() {
    let mut region = region_with_floor();
    let entities = region.data().ecs.entities.len();
    region.handle_event(GameEventKind::ClientLeft(3)).unwrap();
    assert!(region.data().player_entites.is_empty());
    assert_eq!(region.data().ecs.entities.len(), entities);
}

#[test]
fn client_left_reconciles_on_a_client_replica() {
    let id = RegionId::new(0, 0);
    let mut server = region_with_floor();
    server.handle_event(GameEventKind::CreateClient(0)).unwrap();
    server.commit_server_event();
    let snapshot: Rollback = server.data().clone();
    let left = server.handle_event(GameEventKind::ClientLeft(0)).unwrap();

    // Client replica built from the snapshot; it has a pending local
    // prediction at the same event id, so reconcile has to replace it.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut client = Region::new(snapshot, Some(send), id, Some(0));
    client.handle_event(GameEventKind::Tick).unwrap();
    client
        .reconcile(GameEvent::new(left.kind.clone(), left.id, id))
        .expect("authoritative ClientLeft reconciles without a GameError");
    assert!(client.data().player_entites.get(&0).is_none(), "body for 0 must be gone");
    assert!(client.data().clients.get(&0).is_none(), "client 0 must be gone");
    // The replica keeps its Tick prediction, replayed after the server's
    // event: the server running that Tick next must land on the same state.
    server.handle_event(GameEventKind::Tick).unwrap();
    assert_eq!(
        game::state_hash(client.data()),
        game::state_hash(server.data()),
        "the replica must end in the server's state"
    );
}

// ---- WorldManager section ----

mod manager {
    use crossbeam::channel::{unbounded, Receiver};
    use game::{
        Block, BlockId, Chunk, ClientId, ClientPacket, Column, ColumnCoords, InlineLodBuilder,
        InlineSpawner, LodBuilder, PlayRefusal, RegionCoords, RegionOutput, ServerEvent,
        ServerPacket, SubChunk, TileSummary, WorldManager, WorldMap, SPAWN_REGION, UNLOAD_GRACE_MS,
    };

    struct Harness {
        manager: WorldManager<InlineSpawner>,
        region_out: Receiver<(RegionCoords, RegionOutput)>,
        packets: Receiver<(Option<ClientId>, ServerPacket)>,
    }

    fn harness() -> Harness {
        let (out_send, packets) = unbounded();
        let (region_out_send, region_out) = unbounded();
        let generator = Box::new(|_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
            let below = Block::new(BlockId::AIR);
            let above = Block::new(BlockId::AIR);
            let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
            vec![(ColumnCoords::new(0, 0), Column::new(0, below, above, vec![sub]))]
        });
        let (lod_result_send, lod_result_recv) = unbounded();
        let lod_builder: Box<dyn LodBuilder> = Box::new(InlineLodBuilder {
            coarse: Box::new(|_key: &game::lod::LodKey| Vec::new()),
            water: Box::new(game::lod::LodWater::dry),
            out: lod_result_send,
        });
        Harness {
            manager: WorldManager::new(
                InlineSpawner::default(),
                generator,
                out_send,
                region_out_send,
                lod_builder,
                lod_result_recv,
            ),
            region_out,
            packets,
        }
    }

    impl Harness {
        fn settle(&mut self, now_ms: u64) {
            for _ in 0..2 {
                self.manager.spawner_mut().pump();
                while let Ok((rc, out)) = self.region_out.try_recv() {
                    self.manager.handle_region_output(rc, out, now_ms);
                }
                self.manager.pump_lod_results();
            }
        }
        fn event(&mut self, ev: ServerEvent, now_ms: u64) {
            self.manager.handle_server_event(ev, now_ms);
            self.settle(now_ms);
        }
        fn play(&mut self, rc: RegionCoords, now_ms: u64) {
            self.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(rc), 0), now_ms);
        }
        fn drain(&mut self) -> Vec<(Option<ClientId>, ServerPacket)> {
            self.packets.try_iter().collect()
        }
    }

    fn tile(landing: Option<[u32; 2]>) -> TileSummary {
        TileSummary {
            ground_min_m: 0.0,
            ground_mean_m: 0.0,
            ground_max_m: 0.0,
            relief_m: 0.0,
            slope: [0.0, 0.0],
            sea_fraction: 0.0,
            lake_fraction: 0.0,
            lake_level_m: None,
            rivers: 0,
            river_max_discharge_m2: 0.0,
            river_max_width_m: 0.0,
            surface_rock: None,
            landing,
            biome: game::Biome::Grassland,
            region: 0,
            temperature_c: 10.0,
            precip_mm: 700.0,
            savagery: 50,
            alignment: 0,
            volcano: false,
        }
    }

    /// 3x3 at origin (-1,-1); every tile lands at [2,2] except (1,1).
    fn map() -> WorldMap {
        let tiles = (0..9)
            .map(|i| tile(if i == 8 { None } else { Some([2, 2]) }))
            .collect();
        WorldMap { origin: RegionCoords::new(-1, -1), width: 3, height: 3, tiles, rock_names: vec![], region_types: vec![] }
    }

    fn with_map() -> Harness {
        let mut h = harness();
        h.manager.set_world_map(map());
        h.event(ServerEvent::connected(0), 0);
        h.drain();
        h
    }

    fn refusal(packets: &[(Option<ClientId>, ServerPacket)]) -> Option<(RegionCoords, PlayRefusal)> {
        packets.iter().find_map(|(t, p)| match p {
            ServerPacket::PlayRefused(rc, r) if *t == Some(0) => Some((*rc, *r)),
            _ => None,
        })
    }

    fn players_in(h: &mut Harness, rc: RegionCoords) -> usize {
        let mut n = 0;
        h.manager.spawner_mut().with_region(rc, |r| n = r.data().player_entites.len());
        n
    }

    #[test]
    fn connect_pushes_world_map() {
        let mut h = harness();
        h.manager.set_world_map(map());
        h.event(ServerEvent::connected(0), 0);
        assert!(h.drain().iter().any(|(t, p)| *t == Some(0) && matches!(p, ServerPacket::WorldMap(_))));
        let mut h = harness();
        h.event(ServerEvent::connected(0), 0);
        assert!(!h.drain().iter().any(|(_, p)| matches!(p, ServerPacket::WorldMap(_))));
    }

    #[test]
    fn play_moves_home_and_creates_player_in_target() {
        let mut h = with_map();
        let target = RegionCoords::new(1, 0);
        h.play(target, 10);
        let packets = h.drain();
        assert!(packets.iter().any(|(t, p)| {
            *t == Some(0) && matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == target)
        }));
        assert!(h.manager.running_regions().contains(&target));
        let mut found = None;
        h.manager.spawner_mut().with_region(target, |r| {
            let data = r.data();
            let e = *data.player_entites.get(&0).expect("player in target");
            let handle = *data.ecs.rigidbody.get(e);
            found = Some(data.physics.bodies.get(handle).unwrap().translation());
        });
        let t = found.expect("target region live");
        let bv = game::BLOCK_VOXELS as f32;
        assert!((t.x - 2.5 * bv).abs() <= 0.5 * bv && (t.z - 2.5 * bv).abs() <= 0.5 * bv, "{t:?}");
        assert_eq!(players_in(&mut h, SPAWN_REGION), 0);
    }

    #[test]
    fn old_region_parks_after_grace() {
        let mut h = with_map();
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(SPAWN_REGION), 0), 0);
        h.play(RegionCoords::new(1, 0), 10);
        h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(SPAWN_REGION), 0), 10);
        h.manager.maintain(10 + UNLOAD_GRACE_MS + 1);
        h.settle(10 + UNLOAD_GRACE_MS + 1);
        assert!(!h.manager.running_regions().contains(&SPAWN_REGION));
    }

    #[test]
    fn play_refusals() {
        fn home_of(h: &mut Harness) -> Option<RegionCoords> {
            h.event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 0), 1);
            h.drain().iter().find_map(|(_, p)| match p {
                ServerPacket::PlayerRegion(rc, 0) => *rc,
                _ => None,
            })
        }
        // No map.
        let mut h = harness();
        h.event(ServerEvent::connected(0), 0);
        h.drain();
        h.play(RegionCoords::new(1, 0), 1);
        assert_eq!(refusal(&h.drain()), Some((RegionCoords::new(1, 0), PlayRefusal::NoWorldMap)));
        assert_eq!(h.manager.running_regions(), vec![SPAWN_REGION]);
        assert_eq!(home_of(&mut h), Some(SPAWN_REGION));

        let mut h = with_map();
        for (rc, want) in [
            (RegionCoords::new(5, 5), PlayRefusal::OffMap),
            (SPAWN_REGION, PlayRefusal::AlreadyHere),
            (RegionCoords::new(1, 1), PlayRefusal::NoDryLand),
        ] {
            h.play(rc, 1);
            assert_eq!(refusal(&h.drain()), Some((rc, want)));
            assert_eq!(h.manager.running_regions(), vec![SPAWN_REGION]);
            assert_eq!(home_of(&mut h), Some(SPAWN_REGION));
        }
        // Landing column outside the region.
        let mut m = map();
        let edge = game::REGION_BLOCKS as u32;
        m.tiles[5].landing = Some([edge, 0]); // (1,0)
        h.manager.set_world_map(m);
        h.play(RegionCoords::new(1, 0), 1);
        assert_eq!(refusal(&h.drain()), Some((RegionCoords::new(1, 0), PlayRefusal::NoDryLand)));
        assert_eq!(h.manager.running_regions(), vec![SPAWN_REGION]);
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestPlayerRegion, 0), 1);
        assert!(h.drain().iter().any(|(_, p)| matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc == SPAWN_REGION)));
    }

    fn extent_of(h: &mut Harness, rc: RegionCoords) -> Option<game::MapExtent> {
        let mut got = None;
        h.manager.spawner_mut().with_region(rc, |r| got = r.map_extent());
        got
    }

    #[test]
    fn regions_are_bounded_by_the_world_map() {
        let want = Some(map().extent());
        assert!(map().contains(RegionCoords::new(1, 1)) && !map().contains(RegionCoords::new(2, 0)));
        // Already running when the map is installed.
        let mut h = harness();
        h.event(ServerEvent::connected(0), 0);
        h.drain();
        assert_eq!(extent_of(&mut h, SPAWN_REGION), None, "no map: unbounded");
        h.manager.set_world_map(map());
        h.settle(0);
        assert_eq!(extent_of(&mut h, SPAWN_REGION), want);
        // Woken after it.
        let target = RegionCoords::new(1, 0);
        h.play(target, 1);
        assert_eq!(extent_of(&mut h, target), want);
    }

    #[test]
    fn departure_off_the_map_is_returned() {
        let mut h = with_map();
        let target = RegionCoords::new(1, 0); // its east neighbour (2, 0) is off the map
        h.play(target, 1);
        h.drain();
        // A region that was never bounded walks its player out east.
        h.manager.spawner_mut().with_region(target, |r| {
            r.set_map_extent(None);
            r.with_data(|d| {
                let e = *d.player_entites.get(&0).unwrap();
                let handle = *d.ecs.rigidbody.get(e);
                let mut pose = *d.physics.bodies.get(handle).unwrap().position();
                pose.translation.x = game::REGION_SIZE + 3.0 * game::FLIP_HYSTERESIS;
                d.set_body_pose_safe(e, pose);
            });
        });
        h.manager.spawner_mut().tick_all();
        h.settle(2);
        assert!(!h.manager.running_regions().contains(&RegionCoords::new(2, 0)), "no region off the map wakes");
        assert_eq!(players_in(&mut h, target), 1, "the player is handed back");
        assert!(!h.drain().iter().any(|(_, p)| matches!(p, ServerPacket::PlayerRegion(Some(rc), 0) if *rc != target)));
    }

    #[test]
    fn double_play_is_refused() {
        let mut h = with_map();
        let target = RegionCoords::new(1, 0);
        h.play(target, 1);
        h.play(target, 1);
        assert_eq!(refusal(&h.drain()), Some((target, PlayRefusal::AlreadyHere)));
        assert_eq!(players_in(&mut h, target), 1);
    }

    #[test]
    fn play_from_a_parked_home_removes_the_old_body() {
        let mut h = harness();
        h.manager.set_world_map(map());
        h.event(ServerEvent::connected(0), 0);
        h.event(ServerEvent::ClientDisconnected(0), 10);
        let late = 10 + UNLOAD_GRACE_MS + 1;
        h.manager.maintain(late);
        h.settle(late);
        assert!(!h.manager.running_regions().contains(&SPAWN_REGION), "home parked");
        h.event(ServerEvent::connected(0), late);
        let target = RegionCoords::new(1, 0);
        h.play(target, late);
        h.settle(late);
        h.drain();
        assert!(h.manager.running_regions().contains(&SPAWN_REGION), "home woken for the leave");
        assert_eq!(players_in(&mut h, SPAWN_REGION), 0);
        assert_eq!(players_in(&mut h, target), 1);
    }

    #[test]
    fn play_while_home_is_mid_shutdown() {
        let mut h = with_map();
        // Disconnect-free: drop the home's keep-alive by releasing nothing;
        // drive the stop directly. Disconnect, expire, Shutdown sent but not
        // yet processed (no settle), then reconnect and Play.
        h.event(ServerEvent::ClientDisconnected(0), 10);
        let late = 10 + UNLOAD_GRACE_MS + 1;
        h.manager.maintain(late); // Shutdown queued; Stopped not yet handled
        h.manager.handle_server_event(ServerEvent::connected(0), late);
        let target = RegionCoords::new(1, 0);
        h.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(target), 0), late);
        h.settle(late);
        // The move parks the restored body out of the re-woken home first,
        // then creates it in the target: one more round trip.
        h.settle(late);
        assert_eq!(players_in(&mut h, SPAWN_REGION), 0);
        assert_eq!(players_in(&mut h, target), 1);
        // The re-woken old region has no keep-alive reason: it re-parks.
        let later = late + UNLOAD_GRACE_MS + 1;
        h.manager.maintain(later);
        h.settle(later);
        assert!(!h.manager.running_regions().contains(&SPAWN_REGION));
    }

    #[test]
    fn play_into_a_region_that_is_mid_shutdown() {
        let mut h = with_map();
        let target = RegionCoords::new(1, 0);
        // Run the target, subscribe then release so it times out.
        h.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(target), 0), 0);
        h.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(target), 0), 10);
        let late = 10 + UNLOAD_GRACE_MS + 1;
        h.manager.maintain(late); // Shutdown sent, Stopped pending
        h.manager.handle_server_event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(target), 0), late);
        h.settle(late);
        assert!(h.manager.running_regions().contains(&target));
        assert_eq!(players_in(&mut h, target), 1);
        assert_eq!(players_in(&mut h, SPAWN_REGION), 0);
    }
}
