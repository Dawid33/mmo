//! Headless timing of server region genesis (`Region::seed_stud_world`:
//! pristine chunks from the ground recipes, tree bricks, collider genesis),
//! the way the server runs it: `HASH_MODE` from the environment (the binaries
//! default to Off) and on a spawned thread.
//!
//!   cargo run --release -p worldgen --example genesis_profile -- [rx rz [repeats]]

use std::time::Instant;

use game::{Region, RegionCoords, Rollback, StudRegionSeed};

fn genesis(rc: RegionCoords, seed: StudRegionSeed) -> f64 {
    let t = Instant::now();
    let mut region = Region::new(Rollback::new(None), None, rc, None);
    region.seed_stud_world(seed);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    drop(region);
    ms
}

fn main() {
    game::set_hash_mode_from_env();
    let args: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let rc = RegionCoords::new(*args.first().unwrap_or(&0), *args.get(1).unwrap_or(&0));
    let repeats = *args.get(2).unwrap_or(&2);
    for round in 0..repeats {
        let t = Instant::now();
        let seed = worldgen::stud_gen::generate_stud_region(rc, worldgen::WORLD_SEED);
        let generate_ms = t.elapsed().as_secs_f64() * 1000.0;
        let chunks: usize = seed.ground.iter().map(|(_, g)| g.surface_layers().count()).sum();
        let tree_bricks: usize = seed.features.iter().map(|(_, b)| b.len()).sum();
        let main_ms = genesis(rc, seed.clone());
        let thread_ms = std::thread::Builder::new()
            .name(format!("region {},{}", rc.x, rc.z))
            .spawn(move || genesis(rc, seed))
            .unwrap()
            .join()
            .unwrap();
        println!(
            "round {round}: generate {generate_ms:8.1} ms, {chunks} surface chunks, {tree_bricks} tree bricks; \
             genesis: main thread {main_ms:8.1} ms   spawned thread {thread_ms:8.1} ms"
        );
    }
}
