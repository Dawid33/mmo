//! Headless benchmark of the CLIENT GAME THREAD's cost per arriving region
//! snapshot — the work `instance.rs` does in `handle_server(Region)` /
//! `new_region` / `drop_region`. That thread also runs prediction, so every
//! millisecond here freezes the local player; a region crossing delivers a
//! whole row of the near window at once. Ignored (prints, doesn't assert):
//! `just bench-client-region`.
use std::time::{Duration, Instant};

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
    }
}

#[test]
#[ignore = "benchmark; run explicitly via `just bench-client-region`"]
fn client_region_load_cost() {
    game::set_hash_mode(game::HashMode::Off); // the client binary's default
    let regions = [(0, 0), (1, 0), (0, 1), (-1, 0), (0, -1)];
    let mut totals = [Duration::ZERO; 7];
    for (x, z) in regions {
        let rc = game::RegionCoords::new(x, z);
        // Server side (not timed): genesis + one tick, then the snapshot the
        // region thread sends, through the real wire framing.
        let mut server = game::Region::from_columns(rc, worldgen::generate_region(rc, worldgen::WORLD_SEED, mats()));
        let _ = server.handle_event(game::GameEventKind::Tick);
        let wire = game::encode_packet(&game::ServerPacket::Region(rc, server.data().clone())).unwrap();

        // Netcode thread (off the game thread, shown for scale).
        let t = Instant::now();
        let packet: game::ServerPacket = game::decode_packet(&wire).unwrap();
        let decode = t.elapsed();
        let game::ServerPacket::Region(_, raw) = packet else { unreachable!() };

        // Game thread: exactly new_region's work.
        let t = Instant::now();
        let for_region = raw.clone();
        let rollback_clone = t.elapsed();
        let t = Instant::now();
        let for_bridge = (*raw.data).clone();
        let data_clone = t.elapsed();
        let (send, _recv) = crossbeam::channel::unbounded();
        let t = Instant::now();
        let mut region = game::Region::new(for_region, Some(send), rc, None);
        let region_new = t.elapsed();
        let t = Instant::now();
        let _ = region.handle_event(game::GameEventKind::Tick);
        let first_tick = t.elapsed();
        let t = Instant::now();
        let _ = region.handle_event(game::GameEventKind::Tick);
        let second_tick = t.elapsed();
        // drop_region: the stale row is torn down synchronously too.
        let t = Instant::now();
        drop(region);
        drop(raw);
        let drop_t = t.elapsed();
        drop(for_bridge);

        let row = [decode, rollback_clone, data_clone, region_new, first_tick, second_tick, drop_t];
        for (acc, v) in totals.iter_mut().zip(row) {
            *acc += v;
        }
        eprintln!(
            "region {x},{z}: wire={:.1} MiB decode={decode:?} | game thread: rollback_clone={rollback_clone:?} \
             data_clone={data_clone:?} Region::new={region_new:?} first_tick={first_tick:?} \
             second_tick={second_tick:?} drop={drop_t:?}",
            wire.len() as f64 / (1 << 20) as f64
        );
    }
    let n = regions.len() as u32;
    eprintln!(
        "--- mean of {n}: decode={:?} rollback_clone={:?} data_clone={:?} Region::new={:?} first_tick={:?} second_tick={:?} drop={:?}",
        totals[0] / n, totals[1] / n, totals[2] / n, totals[3] / n, totals[4] / n, totals[5] / n, totals[6] / n
    );
}
