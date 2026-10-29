use crossbeam::channel::unbounded;
use game::lod::{LodCell, LodKey};
use game::*;

/// Flat-floor generator matching world_manager.rs's test generator; coarse
/// closure returns one marker band so sources are distinguishable.
fn manager_with_lod() -> (
    WorldManager<InlineSpawner>,
    crossbeam::channel::Receiver<(Option<ClientId>, ServerPacket)>,
    crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) {
    let (out_send, out_recv) = unbounded();
    let (region_out_send, region_out_recv) = unbounded();
    let generator = Box::new(|_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        let below = Block::new(BlockId::AIR);
        let above = Block::new(BlockId::AIR);
        let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
        vec![(ColumnCoords::new(0, 0), Column::new(0, below, above, vec![sub]))]
    });
    let (lod_result_send, lod_result_recv) = unbounded();
    let coarse = Box::new(|_key: &LodKey| {
        vec![LodCell {
            group: [0, 0, 0],
            grid: vec![Voxel::new(BlockId(42)); BRICK_VOXEL_COUNT],
        }]
    });
    let builder: Box<dyn LodBuilder> =
        Box::new(InlineLodBuilder { coarse, out: lod_result_send });
    let manager = WorldManager::new(
        InlineSpawner::default(),
        generator,
        out_send,
        region_out_send,
        builder,
        lod_result_recv,
    );
    (manager, out_recv, region_out_recv)
}

/// Route → pump runners → route outputs, twice (outputs can trigger
/// follow-up work needing one more pump). Mirrors world_manager.rs's
/// harness settle().
fn drain_region_outputs(
    m: &mut WorldManager<InlineSpawner>,
    region_out: &crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>,
) {
    for _ in 0..2 {
        m.spawner_mut().pump();
        while let Ok((rc, out)) = region_out.try_recv() {
            m.handle_region_output(rc, out, 0);
        }
    }
}

/// Exactly one LodData for `key` in `out` (draining everything else, same
/// discard convention the other tests use); returns its payload.
fn expect_lod_data(
    out: &crossbeam::channel::Receiver<(Option<ClientId>, ServerPacket)>,
    key: LodKey,
) -> LodPayload {
    let mut hits: Vec<LodPayload> = out
        .try_iter()
        .filter_map(|(_, p)| match p {
            ServerPacket::LodData(k, payload) if k == key => Some(payload),
            _ => None,
        })
        .collect();
    assert_eq!(hits.len(), 1, "exactly one LodData for {key:?}");
    hits.pop().unwrap()
}

/// Every voxel's block id in the x/z rect `x_range` x `z_range` (all y),
/// across every grid in the payload. Grid layout is always x + 32*y +
/// 32*32*z regardless of which vertical band a grid's group indexes.
fn rect_blocks(
    payload: &LodPayload,
    x_range: std::ops::Range<usize>,
    z_range: std::ops::Range<usize>,
) -> Vec<BlockId> {
    let mut blocks = Vec::new();
    for (_, packed) in payload {
        let grid = packed.decode().expect("well-formed RLE grid");
        for z in z_range.clone() {
            for y in 0..32 {
                for x in x_range.clone() {
                    blocks.push(grid[x + 32 * y + 32 * 32 * z].block);
                }
            }
        }
    }
    blocks
}

#[test]
fn request_lod_pristine_region_builds_downsamples_and_caches() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    let key = LodKey::for_region(4, RegionCoords::new(5, 5)); // never run
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1),
        0,
    );
    m.pump_lod_results();
    let data: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(data.len(), 1, "one LodData reply");
    let (target, ServerPacket::LodData(k, payload)) = &data[0] else { unreachable!() };
    assert_eq!(*target, Some(1));
    assert_eq!(*k, key);
    assert!(!payload.is_empty(), "flat floor produces cells");
    for (_, grid) in payload {
        assert!(grid.decode().is_some(), "well-formed RLE grids");
    }

    // Second identical request: served from cache, no rebuild (the inline
    // builder would have pushed a second result).
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1),
        0,
    );
    let cached: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(cached.len(), 1, "cache hit answers without building");
}

#[test]
fn request_lod_running_region_downsamples_authoritative_state() {
    // Subscribe a region normally (it runs), edit nothing; request factor 4
    // for it. Running + unseeded terrain cache → the manager asks the region
    // (RequestTerrain), the reply seeds terrain_cache, and the pending LOD
    // request completes.
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop); // discard the join/create-client traffic

    let rc = RegionCoords::new(2, 2);
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), 1),
        0,
    );
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop); // discard the subscribe's Region snapshot
    assert!(m.running_regions().contains(&rc), "region must already be running");

    let key = LodKey::for_region(4, rc);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    // Pump the spawner + region outputs (answers RequestTerrain, seeds
    // terrain_cache, re-enters the pending LOD build) then the lod results.
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();

    let data: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(data.len(), 1, "exactly one LodData for the key arrives");
    let (target, ServerPacket::LodData(k, payload)) = &data[0] else { unreachable!() };
    assert_eq!(*target, Some(1));
    assert_eq!(*k, key);
    assert!(!payload.is_empty(), "flat floor produces cells");
}

#[test]
fn release_and_disconnect_clear_subscriptions() {
    // RequestLod then ReleaseLod: a later terrain change must push nothing
    // (asserted fully in Task 9's invalidation test; here assert the maps
    // via a fresh RequestLod still answering — i.e. no double-send state).
    // Then RequestLod from client 2 and disconnect client 2: no panic, and
    // client 1's later request still works.
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    let key = LodKey::for_region(4, RegionCoords::new(9, 9)); // never run
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    m.pump_lod_results();
    let first: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(first.len(), 1, "client 1's initial request answers");

    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(key), 1), 0);

    // Client 2 subscribes to the same (now-cached) key, then disconnects
    // before doing anything else with it.
    m.handle_server_event(ServerEvent::ClientConnected(2), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 2), 0);
    m.pump_lod_results();
    let second: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(second.len(), 1, "client 2 served from cache, no rebuild");
    m.handle_server_event(ServerEvent::ClientDisconnected(2), 0);

    // Client 1's later request for the same key still answers cleanly: no
    // leftover in-flight/subscriber bookkeeping from client 1's release or
    // client 2's disconnect blocks it, and no panic occurred above.
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    m.pump_lod_results();
    let third: Vec<_> = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .collect();
    assert_eq!(third.len(), 1, "client 1's later request still answers");
    let (target, ServerPacket::LodData(k, _)) = &third[0] else { unreachable!() };
    assert_eq!(*target, Some(1));
    assert_eq!(*k, key);
}

#[test]
fn invalid_factor_is_dropped() {
    let (mut m, out, _region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    let bogus = LodKey { factor: 3, coords: [0, 0] };
    m.handle_server_event(
        ServerEvent::ClientPacket(ClientPacket::RequestLod(bogus), 1),
        0,
    );
    m.pump_lod_results();
    assert!(
        out.try_iter().all(|(_, p)| !matches!(p, ServerPacket::LodData(..))),
        "bogus factor never builds or panics"
    );
}

#[test]
fn tile_build_is_coarse_only_when_every_region_is_pristine() {
    let (mut m, out, _region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    let key = LodKey { factor: 64, coords: [50, 50] }; // far away, all pristine
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    m.pump_lod_results();
    let payload = expect_lod_data(&out, key);
    // The fake coarse closure emits BlockId(42) everywhere; no overlay ran,
    // so the payload is pure marker.
    let grid = payload[0].1.decode().unwrap();
    assert!(grid.iter().all(|v| v.block == BlockId(42)));
}

/// Block-scale and coarser region keys (16/32) take the coarse path too: the
/// wide outer rings hold thousands of pristine regions, and full generation
/// for each would cost ~40x what sampling the profile does.
#[test]
fn pristine_region_keyed_coarse_factors_are_sampled_not_generated() {
    let (mut m, out, _region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    for factor in [16, 32] {
        let key = LodKey::for_region(factor, RegionCoords::new(50, 50)); // far away, pristine
        assert_eq!(key.region_span(), 1);
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
        m.pump_lod_results();
        let payload = expect_lod_data(&out, key);
        let grid = payload[0].1.decode().unwrap();
        assert!(
            grid.iter().all(|v| v.block == BlockId(42)),
            "factor {factor}: payload is the coarse generator's marker, not generated terrain"
        );
    }
    assert_eq!(m.terrain_cache_len(), 0, "no pristine terrain was generated for them");
}

#[test]
fn tile_build_overlays_regions_with_authoritative_state() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    // Region (0,0) is SPAWN_REGION: ClientConnected already made it client
    // 1's home, so it's running from the start. Disconnect to lift the
    // home keep-alive, park it past the grace period (same dance as
    // world_manager.rs's home_region_survives_zero_subscribers), then
    // reconnect the SAME client id: `homes` already contains it, so
    // ClientConnected takes the reconnect branch and does NOT call
    // ensure_running — the region stays parked, with terrain_cache seeded
    // from the park (the flat-floor generator's state).
    m.handle_server_event(ServerEvent::ClientDisconnected(1), 0);
    m.maintain(UNLOAD_GRACE_MS);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);
    assert!(m.parked_regions().contains(&RegionCoords::new(0, 0)));

    m.handle_server_event(ServerEvent::ClientConnected(1), UNLOAD_GRACE_MS);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);
    assert!(
        !m.running_regions().contains(&RegionCoords::new(0, 0)),
        "reconnecting the same client must not wake the parked region"
    );

    let key = LodKey { factor: 64, coords: [0, 0] }; // covers regions (0..2, 0..2)
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let payload = expect_lod_data(&out, key);

    // Region (0,0)'s cell rect (0..16, 0..16): the overlay clears the
    // coarse marker and writes the authoritative flat floor (BlockId(1)).
    let overlaid = rect_blocks(&payload, 0..16, 0..16);
    assert!(overlaid.iter().all(|b| *b != BlockId(42)), "overlay must wipe the coarse marker");
    assert!(overlaid.iter().any(|b| *b == BlockId(1)), "authoritative floor must appear");

    // Region (1,1)'s cell rect (16..32, 16..32) is pristine: never
    // generated, so untouched coarse marker (or air where the overlay's
    // clear pass reached a shared grid) is all that's there.
    let pristine = rect_blocks(&payload, 16..32, 16..32);
    assert!(
        pristine.iter().all(|b| *b == BlockId(42) || *b == BlockId::AIR),
        "pristine region stays marker/air"
    );
}

#[test]
fn terrain_change_invalidates_and_repushes_subscribed_keys() {
    // Client 1 holds factor-4 of region (0,0) AND the factor-64 tile [0,0].
    // A terrain change on (0,0) must drop both from the cache and push a
    // fresh LodData for both, reflecting the new terrain. A client that
    // released one of the same keys beforehand must get nothing.
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    let rc = RegionCoords::new(0, 0); // SPAWN_REGION: already running (client 1's home)

    let region_key = LodKey::for_region(4, rc);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(region_key), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let before_region = expect_lod_data(&out, region_key);

    let tile_key = LodKey { factor: 64, coords: [0, 0] }; // covers regions (0..2, 0..2)
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(tile_key), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let before_tile = expect_lod_data(&out, tile_key);

    // Client 2 subscribes to the region key then releases it: it must NOT
    // receive the refresh push below (Task 7's carried review obligation).
    m.handle_server_event(ServerEvent::ClientConnected(2), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(region_key), 2), 0);
    m.pump_lod_results();
    out.try_iter().for_each(drop); // discard client 2's cache-hit reply
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(region_key), 2), 0);

    // Terrain change: a different solid material for region (0,0).
    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    let sub = SubChunk::Dense(Box::new(Chunk::flat_floor_with(8, BlockId(99))));
    let changed = vec![(ColumnCoords::new(0, 0), Column::new(0, below, above, vec![sub]))];
    let snap2 = TerrainSnapshot::from_columns(changed);
    m.handle_region_output(rc, RegionOutput::TerrainChanged(snap2), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();

    // Both rebuilds land from the same invalidation pass: snapshot every
    // LodData in one drain rather than calling expect_lod_data twice (which
    // would discard the second key's message while filtering for the first).
    let pushed: Vec<(Option<ClientId>, LodKey, LodPayload)> = out
        .try_iter()
        .filter_map(|(target, p)| match p {
            ServerPacket::LodData(k, payload) => Some((target, k, payload)),
            _ => None,
        })
        .collect();

    let region_hits: Vec<_> = pushed.iter().filter(|(_, k, _)| *k == region_key).collect();
    assert_eq!(region_hits.len(), 1, "exactly one refresh push for the region key");
    assert_ne!(
        region_hits[0].2, before_region,
        "region-key payload reflects the terrain change"
    );

    let tile_hits: Vec<_> = pushed.iter().filter(|(_, k, _)| *k == tile_key).collect();
    assert_eq!(tile_hits.len(), 1, "exactly one refresh push for the tile key");
    assert_ne!(tile_hits[0].2, before_tile, "tile-key payload reflects the terrain change");

    assert!(
        !pushed.iter().any(|(target, k, _)| *target == Some(2) && *k == region_key),
        "client 2 released before the change: it must receive no refresh push"
    );
}

#[test]
fn terrain_change_with_no_subscribers_only_drops_cache() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    let rc = RegionCoords::new(0, 0);
    let key = LodKey::for_region(4, rc);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let _ = expect_lod_data(&out, key);

    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(key), 1), 0);

    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    let sub = SubChunk::Dense(Box::new(Chunk::flat_floor_with(8, BlockId(77))));
    let changed = vec![(ColumnCoords::new(0, 0), Column::new(0, below, above, vec![sub]))];
    let snap2 = TerrainSnapshot::from_columns(changed);
    m.handle_region_output(rc, RegionOutput::TerrainChanged(snap2), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();

    assert!(
        out.try_iter().all(|(_, p)| !matches!(p, ServerPacket::LodData(..))),
        "no subscribers left: cache drops silently, no push"
    );

    // Fresh request rebuilds, reflecting the post-change terrain.
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let after = expect_lod_data(&out, key);
    let blocks = rect_blocks(&after, 0..32, 0..32);
    assert!(blocks.iter().any(|b| *b == BlockId(77)), "rebuild reflects the post-change terrain");
}

#[test]
fn lod_cache_evicts_unsubscribed_lru_but_pins_subscribed() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    m.set_lod_cache_max(3);

    // Fill past the cap with released (unsubscribed) keys.
    for (x, z) in [(100, 100), (101, 101), (102, 102), (103, 103)] {
        let key = LodKey::for_region(4, RegionCoords::new(x, z));
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
        m.pump_lod_results();
        out.try_iter().for_each(drop);
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(key), 1), 0);
    }
    assert_eq!(m.lod_cache_len(), 3, "cache stays at cap under released-only pressure");

    // A still-subscribed key must survive eviction pressure even past the cap.
    let pinned = LodKey::for_region(4, RegionCoords::new(200, 200));
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(pinned), 1), 0);
    m.pump_lod_results();
    out.try_iter().for_each(drop);
    assert_eq!(m.lod_cache_len(), 3, "cache stays at cap with one pinned entry added");

    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(pinned), 1), 0);
    let hit: Vec<_> =
        out.try_iter().filter(|(_, p)| matches!(p, ServerPacket::LodData(..))).collect();
    assert_eq!(hit.len(), 1, "pinned key survived: served from cache, no rebuild");
}

/// Pristine snapshots (generated only to feed a never-run region's LOD build)
/// are LRU-capped, while a region's own reported terrain is never evicted.
#[test]
fn pristine_terrain_cache_is_lru_capped_but_authoritative_terrain_is_kept() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);

    // SPAWN_REGION is running: its LOD build round-trips RequestTerrain, so
    // its cache entry is authoritative.
    let home = LodKey::for_region(4, RegionCoords::new(0, 0));
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(home), 1), 0);
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    assert_eq!(m.terrain_cache_len(), 1);

    m.set_pristine_terrain_max(2);
    for x in 100..105 {
        let key = LodKey::for_region(4, RegionCoords::new(x, x));
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
        m.pump_lod_results();
    }
    assert_eq!(
        m.terrain_cache_len(),
        3,
        "two pristine snapshots at the cap plus the authoritative home entry"
    );
    let built = out
        .try_iter()
        .filter(|(_, p)| matches!(p, ServerPacket::LodData(..)))
        .count();
    assert_eq!(built, 6, "eviction never costs a build its payload");

    // An evicted pristine region still serves a different factor: it just
    // regenerates instead of hitting the cache.
    let again = LodKey::for_region(2, RegionCoords::new(100, 100));
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(again), 1), 0);
    m.pump_lod_results();
    expect_lod_data(&out, again);
    assert_eq!(m.terrain_cache_len(), 3);
}

#[test]
fn tile_build_waits_for_running_unseeded_regions() {
    let (mut m, out, region_out) = manager_with_lod();
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    out.try_iter().for_each(drop);
    // Region (0,0) is SPAWN_REGION: running from client 1's home, but
    // never sampled, so terrain_cache has no entry for it yet.
    assert!(m.running_regions().contains(&RegionCoords::new(0, 0)));

    let key = LodKey { factor: 64, coords: [0, 0] }; // covers regions (0..2, 0..2)
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);
    assert!(
        out.try_iter().all(|(_, p)| !matches!(p, ServerPacket::LodData(..))),
        "no LodData before the terrain reply lands"
    );

    // Pumping the spawner answers the deferred RequestTerrain, seeding
    // terrain_cache and re-entering the tile build that was waiting on it.
    drain_region_outputs(&mut m, &region_out);
    m.pump_lod_results();
    let payload = expect_lod_data(&out, key);

    let overlaid = rect_blocks(&payload, 0..16, 0..16);
    assert!(overlaid.iter().all(|b| *b != BlockId(42)), "overlay must wipe the coarse marker");
    assert!(overlaid.iter().any(|b| *b == BlockId(1)), "authoritative floor must appear");
}

/// Stand-in for the native server's thread pool: it generates pristine
/// terrain itself (`generates_pristine`), and records jobs instead of building.
struct RecordingBuilder {
    jobs: std::sync::Arc<std::sync::Mutex<Vec<LodBuildJob>>>,
}

impl LodBuilder for RecordingBuilder {
    fn submit(&mut self, job: LodBuildJob) {
        self.jobs.lock().unwrap().push(job);
    }

    fn generates_pristine(&self) -> bool {
        true
    }
}

/// Worldgen for a never-run region's LOD must not run on the manager thread
/// (it measured up to 522 ms per call, during which no packet routes), and the
/// full-resolution result must not be parked in `terrain_cache`.
#[test]
fn pristine_lod_source_is_generated_by_the_builder_not_the_manager() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    let (out_send, out) = unbounded();
    let (region_out_send, region_out) = unbounded();
    let generated = Arc::new(AtomicUsize::new(0));
    let counter = generated.clone();
    let generator = Box::new(move |_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        counter.fetch_add(1, Ordering::SeqCst);
        let air = Block::new(BlockId::AIR);
        let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
        vec![(ColumnCoords::new(0, 0), Column::new(0, air, air, vec![sub]))]
    });
    let jobs = Arc::new(Mutex::new(Vec::new()));
    let (_lod_result_send, lod_result_recv) = unbounded::<LodResult>();
    let mut m = WorldManager::new(
        InlineSpawner::default(),
        generator,
        out_send,
        region_out_send,
        Box::new(RecordingBuilder { jobs: jobs.clone() }),
        lod_result_recv,
    );
    m.handle_server_event(ServerEvent::ClientConnected(1), 0);
    drain_region_outputs(&mut m, &region_out);
    let after_join = generated.load(Ordering::SeqCst);
    assert_eq!(after_join, 1, "the home region itself still generates on the manager");
    let cached_before = m.terrain_cache_len();

    let far = RegionCoords::new(100, 100);
    let key = LodKey::for_region(4, far);
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), 1), 0);

    assert_eq!(generated.load(Ordering::SeqCst), after_join, "manager must not run worldgen for LOD");
    assert_eq!(m.terrain_cache_len(), cached_before, "no full-resolution pristine snapshot is retained");
    {
        let jobs = jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(&jobs[0], LodBuildJob::Pristine { key: k, region } if *k == key && *region == far),
            "expected a Pristine job, got {:?}",
            jobs[0]
        );
    }

    // The result path is unchanged: the payload fans out to the subscriber.
    out.try_iter().for_each(drop);
    m.handle_lod_result(key, Vec::new());
    expect_lod_data(&out, key);
}

/// A `Pristine` job is the old two-step (generate, then downsample the
/// materialized chunks) moved into the builder — same payload, bit for bit.
#[test]
fn pristine_job_builds_the_same_payload_as_generate_then_region_job() {
    let generate = |_rc: RegionCoords| -> Vec<(ColumnCoords, Column)> {
        let air = Block::new(BlockId::AIR);
        (0..2usize)
            .map(|x| {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8 + x as u32)));
                (ColumnCoords::new(x, 0), Column::new(0, air, air, vec![sub]))
            })
            .collect()
    };
    let coarse = |_key: &LodKey| -> Vec<LodCell> { Vec::new() };
    let region = RegionCoords::new(3, -2);
    let key = LodKey::for_region(2, region);

    let chunks = TerrainSnapshot::from_columns(generate(region)).chunks;
    let (_, expected) = build_lod_job(LodBuildJob::Region { key, chunks }, &coarse, &generate);
    let (got_key, got) = build_lod_job(LodBuildJob::Pristine { key, region }, &coarse, &generate);

    assert_eq!(got_key, key);
    assert!(!got.is_empty(), "the flat floors must produce LOD cells");
    assert_eq!(bincode::serialize(&got).unwrap(), bincode::serialize(&expected).unwrap());
}

#[test]
fn a_partial_overlay_keeps_pristine_terrain_where_it_has_no_columns() {
    // Cell-mode regions only hold generated cells: the LOD of such a region
    // must fall back to pristine coarse terrain everywhere else, not go empty.
    use game::lod::{LodCell, LodKey};
    use game::{BlockId, Chunk, ChunkCoords, Voxel, BRICK_VOXEL_COUNT};
    let key = LodKey::for_region(16, game::RegionCoords::new(0, 0));
    assert_eq!(key.region_span(), 1, "fixture: a one-region factor-16 key");
    let per_side = (game::lod::cells_per_region(16) / 32).max(1) as i32;
    let coarse = move |_: &LodKey| -> Vec<LodCell> {
        let mut cells = Vec::new();
        for x in 0..per_side {
            for z in 0..per_side {
                cells.push(LodCell { group: [x, 0, z], grid: vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT] });
            }
        }
        cells
    };
    // Authoritative terrain: one chunk in group (0,0,0) only.
    let overlay = vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor_with(8, BlockId(7)))];
    let (_, payload) = game::build_lod_job(
        game::LodBuildJob::Tile { key, overlays: vec![(game::RegionCoords::new(0, 0), overlay)] },
        &coarse,
        &game::no_pristine_generator,
    );
    let groups: std::collections::BTreeSet<[i32; 3]> = payload.iter().map(|(g, _)| *g).collect();
    assert_eq!(groups.len() as i32, per_side * per_side, "every column keeps terrain: {groups:?}");
    let g0 = payload.iter().find(|(g, _)| *g == [0, 0, 0]).unwrap();
    let grid = g0.1.decode().unwrap();
    assert!(grid.iter().any(|v| v.block == BlockId(7)), "the overlaid column is authoritative");
    assert!(!grid.iter().all(|v| v.block == BlockId(1)), "not the coarse fill");
}

#[path = "support/lod_reference.rs"]
mod lod_reference;

/// A seeded region-shaped chunk list: per column a stack of chunks from
/// below y=0 up — uniform stone at the bottom, mixed surface chunks (random
/// blocks, half-block and random chisels), uniform air on top — at
/// region-local x/z, so every factor's group lattice is exercised.
fn seeded_region_chunks(seed: u64) -> Vec<(ChunkCoords, Chunk)> {
    let mut s = seed | 1;
    let mut next = move |m: u64| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % m
    };
    let mut chunks = Vec::new();
    // Both sides of the factor-4 and factor-16 group seams (chunk 16).
    let cols = [0usize, 1, 2, 3, 14, 15, 16, 17];
    for &cx in &cols {
        for &cz in &cols {
            let surface = next(4) as i32 - 2;
            for cy in -3..4 {
                let chunk = if cy < surface {
                    Chunk::uniform(Block::new(BlockId(2)))
                } else if cy > surface {
                    Chunk::default()
                } else {
                    let mut c = Chunk::default();
                    for b in 0..CHUNK_BLOCK_COUNT {
                        let bi = BlockIndex::from_linear(b);
                        let (bx, by, bz) = bi.xyz();
                        c.set_block(bx, by, bz, Block::new(BlockId(next(4) as u16)));
                        match next(6) {
                            0 => {
                                let palette = vec![BlockId(1), BlockId(3), BlockId::AIR];
                                let mut grid = vec![None; BLOCK_VOXEL_COUNT];
                                for _ in 0..next(2000) {
                                    let i = next(BLOCK_VOXEL_COUNT as u64) as usize;
                                    grid[i] = (next(4) != 0).then(|| next(3) as u8);
                                }
                                c.chisel.insert(bi, ChiselData::from_grid(palette, &grid));
                            }
                            1 => {
                                let top = [BLOCK_VOXELS, HALF_BLOCK_VOXELS, BLOCK_VOXELS];
                                c.chisel.insert(bi, ChiselData::cuboid(BlockId(3), [0, 0, 0], top));
                            }
                            _ => {}
                        }
                    }
                    c
                };
                chunks.push((ChunkCoords::new(cx, cy, cz), chunk));
            }
        }
    }
    chunks
}

/// LOD reads chunks as 32³ brick views: the factor-2/4/16 grids from that path
/// equal the pre-brick downsampler's byte for byte (only meaningful while one
/// chunk is one brick — the reference's premise).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn far_lod_from_bricks_matches_reference() {
    // The brick API itself: one chunk is BRICKS_PER_CHUNK_EDGE³ bricks whose
    // dense voxels tile the chunk, and world brick coords scale chunk coords.
    let chunks = seeded_region_chunks(0x9E37_79B9_7F4A_7C15);
    let (coords, sample) = chunks.iter().find(|(_, c)| c.blocks.uniform().is_none()).unwrap();
    assert_eq!(sample.bricks().count(), BRICKS_PER_CHUNK_EDGE.pow(3));
    for (b, brick) in sample.bricks() {
        assert_eq!(brick.voxels().len(), BRICK_VOXEL_COUNT);
        assert!(brick.voxels() == derive_voxels(sample, b), "brick {b:?} voxels match the oracle");
        let w = brick_coords(*coords, b);
        assert_eq!(w[1], coords.y as i64 * BRICKS_PER_CHUNK_EDGE as i64 + b[1] as i64);
    }
    if !lod_reference::reference_applies() {
        return;
    }
    for factor in [2u32, 4, 16] {
        let got = lod::downsample_chunks(chunks.clone(), factor);
        let want = lod_reference::downsample_reference(chunks.clone(), factor);
        assert_eq!(
            bincode::serialize(&lod_reference::packed(&got)).unwrap(),
            bincode::serialize(&lod_reference::packed(&want)).unwrap(),
            "factor {factor}"
        );
    }
}
