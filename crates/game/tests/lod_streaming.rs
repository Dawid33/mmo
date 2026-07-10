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
            grid: vec![Voxel::new(BlockId(42)); CHUNK_VOXEL_COUNT],
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
    // (RequestFarSnapshot), the reply seeds far_cache, and the pending LOD
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
    // Pump the spawner + region outputs (answers RequestFarSnapshot, seeds
    // far_cache, re-enters the pending LOD build) then the lod results.
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
