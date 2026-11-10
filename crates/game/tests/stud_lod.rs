//! Far terrain through the manager: the builder path (`LodBuilder::submit` /
//! `poll`), its cache, and the wire.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crossbeam::channel::unbounded;
use game::lod::LodKey;
use game::*;

/// The tile the test generator makes for `key`: a ramp along x whose base is the key's
/// factor, sand under a level of water on its first row.
fn tile_of(key: &LodKey) -> StudLodTile {
    let side = StudLodTile::side_for(key);
    let n = usize::from(side) + 1;
    let samples: Vec<StudLodSample> = (0..n * n)
        .map(|i| {
            let (x, z) = (i % n, i / n);
            let top = key.factor as i32 + x as i32;
            if z == 0 {
                StudLodSample::of_column(top, MAT_SAND, Some(top + 9))
            } else {
                StudLodSample::of_column(top, MAT_GRASS, None)
            }
        })
        .collect();
    StudLodTile::encode(side, &samples)
}

type Out = crossbeam::channel::Receiver<(Option<ClientId>, ServerPacket)>;

/// A manager whose LOD builder is the inline stud one; `builds` counts its tiles.
fn stud_manager() -> (WorldManager<InlineSpawner>, Out, Arc<AtomicU32>) {
    let (out_send, out_recv) = unbounded();
    let (region_out_send, _region_out_recv) = unbounded();
    let builds = Arc::new(AtomicU32::new(0));
    let counter = builds.clone();
    let builder: Box<dyn LodBuilder> = Box::new(InlineStudLodBuilder::new(Box::new(move |key| {
        counter.fetch_add(1, Ordering::SeqCst);
        tile_of(key)
    })));
    let manager = WorldManager::new(InlineSpawner::default(), out_send, region_out_send, builder);
    (manager, out_recv, builds)
}

fn request(m: &mut WorldManager<InlineSpawner>, client: ClientId, key: LodKey) {
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::RequestLod(key), client), 0);
}

/// Every far-terrain packet in `out`.
fn lod_packets(out: &Out) -> Vec<(Option<ClientId>, ServerPacket)> {
    out.try_iter().filter(|(_, p)| matches!(p, ServerPacket::StudLodData(..))).collect()
}

/// A factor the stud path serves at this region size.
fn factor() -> u32 {
    stud_ring_table(game::lod::LOD_RING_TABLE)[0].1
}

#[test]
fn a_stud_world_answers_request_lod_with_its_tile_and_caches_it() {
    let (mut m, out, builds) = stud_manager();
    m.handle_server_event(ServerEvent::connected(1), 0);
    m.handle_server_event(ServerEvent::connected(2), 0);
    let key = LodKey::for_region(factor(), RegionCoords::new(5, -3));
    request(&mut m, 1, key);
    assert!(lod_packets(&out).is_empty(), "nothing before the builder's result is pumped");
    // A second subscriber while the build is in flight: no second build.
    request(&mut m, 2, key);
    m.pump_lod_results();
    let got = lod_packets(&out);
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(got.len(), 2, "one packet per subscriber");
    let mut targets = Vec::new();
    for (target, packet) in got {
        let ServerPacket::StudLodData(k, tile) = packet else { panic!("far terrain of a stud world is StudLodData") };
        assert_eq!(k, key);
        assert_eq!(tile, tile_of(&key));
        assert_eq!(tile.decode().unwrap().len(), StudLodTile::sample_count(tile.side));
        targets.push(target);
    }
    targets.sort();
    assert_eq!(targets, vec![Some(1), Some(2)]);
    assert_eq!(m.lod_cache_len(), 1);

    // Released and asked again: from the cache.
    m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(key), 1), 0);
    request(&mut m, 1, key);
    let again = lod_packets(&out);
    assert_eq!(again.len(), 1, "a cache hit answers at once");
    assert!(matches!(&again[0], (Some(1), ServerPacket::StudLodData(k, t)) if *k == key && *t == tile_of(&key)));
    assert_eq!(builds.load(Ordering::SeqCst), 1, "no rebuild");
}

/// A client asking for cells finer than a stud (or a factor that is none): no build, no answer, and the manager keeps working.
#[test]
fn a_stud_world_drops_keys_it_has_no_far_terrain_for() {
    let (mut m, out, builds) = stud_manager();
    m.handle_server_event(ServerEvent::connected(1), 0);
    for bad in [2, 4, 3, 1024] {
        request(&mut m, 1, LodKey::for_region(bad.max(2), RegionCoords::new(0, 0)));
        request(&mut m, 1, LodKey { factor: bad, coords: [0, 0] });
    }
    m.pump_lod_results();
    assert!(lod_packets(&out).is_empty());
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert_eq!(m.depths().lod_in_flight, 0);
    let key = LodKey::for_region(factor(), RegionCoords::new(0, 0));
    request(&mut m, 1, key);
    m.pump_lod_results();
    assert_eq!(lod_packets(&out).len(), 1);
}

/// The stud cache obeys the manager's LRU cap and never evicts a subscribed key.
#[test]
fn the_stud_lod_cache_is_capped_and_pins_subscribed_keys() {
    let (mut m, out, builds) = stud_manager();
    m.handle_server_event(ServerEvent::connected(1), 0);
    let keys: Vec<LodKey> = (0..4).map(|x| LodKey::for_region(factor(), RegionCoords::new(10 * x, 0))).collect();
    for key in &keys {
        request(&mut m, 1, *key);
    }
    m.pump_lod_results();
    assert_eq!(lod_packets(&out).len(), 4);
    assert_eq!(m.lod_cache_len(), 4);
    // Everything subscribed: nothing can go.
    m.set_lod_cache_max(1);
    assert_eq!(m.lod_cache_len(), 4);
    for key in &keys[..3] {
        m.handle_server_event(ServerEvent::ClientPacket(ClientPacket::ReleaseLod(*key), 1), 0);
    }
    m.set_lod_cache_max(1);
    assert_eq!(m.lod_cache_len(), 1, "only the subscribed key stays");
    request(&mut m, 1, keys[0]);
    m.pump_lod_results();
    assert_eq!(builds.load(Ordering::SeqCst), 5, "an evicted key is built again");
    assert_eq!(lod_packets(&out).len(), 1);
}

/// The inline builder answers a submitted key once.
#[test]
fn the_inline_stud_builder_answers_each_key_once() {
    let key = LodKey::for_region(factor(), RegionCoords::new(0, 0));
    let mut stud = InlineStudLodBuilder::new(Box::new(tile_of));
    assert!(stud.poll().is_none(), "nothing before a key is submitted");
    stud.submit(key);
    assert_eq!(stud.poll(), Some((key, tile_of(&key))));
    assert_eq!(stud.poll(), None);
}

/// `StudLodData` crosses the framed codec.
#[test]
fn stud_lod_data_round_trips_the_wire() {
    let key = LodKey { factor: 64, coords: [-2, 7] };
    let tile = tile_of(&key);
    let wire = encode_packet(&ServerPacket::StudLodData(key, tile.clone())).unwrap();
    match decode_packet::<ServerPacket>(&wire).unwrap() {
        ServerPacket::StudLodData(k, t) => {
            assert_eq!(k, key);
            assert_eq!(t, tile);
            assert!(t.has_water());
        }
        other => panic!("wrong variant {other:?}"),
    }
    // About a byte per sample before lz4: heights are deltas, the rest is runs.
    let raw = bincode::serialize(&ServerPacket::StudLodData(key, tile.clone())).unwrap();
    assert!(raw.len() < 2 * StudLodTile::sample_count(tile.side), "{} bytes", raw.len());

    // The last packet on the wire, right after `AdventureResult`.
    let index = |p: &ServerPacket| u32::from_le_bytes(bincode::serialize(p).unwrap()[..4].try_into().unwrap());
    let last_before = ServerPacket::AdventureResult(Err(game::history::AdventureRefusal::NoHistory));
    assert_eq!(index(&ServerPacket::StudLodData(key, StudLodTile::default())), index(&last_before) + 1);
}
