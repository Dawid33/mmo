//! Headless timing of server region genesis (`Region::from_columns`: chunk
//! flatten + collider genesis), the way the server runs it: `HASH_MODE` from
//! the environment (the binaries default to Off) and on a spawned thread.
//!
//!   cargo run --release -p worldgen --example genesis_profile -- [rx rz [repeats]]

use std::time::Instant;

use game::{BlockId, Region, RegionCoords};

fn main() {
    game::set_hash_mode_from_env();
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let rc = RegionCoords::new(*args.first().unwrap_or(&0), *args.get(1).unwrap_or(&0));
    let repeats = *args.get(2).unwrap_or(&2);
    let materials = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    for round in 0..repeats {
        let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, materials);
        let chunks: usize = columns
            .iter()
            .map(|(_, c)| c.materialized_sub_chunks().len())
            .sum();
        let t = Instant::now();
        let region = Region::from_columns(rc, columns);
        let main_ms = t.elapsed().as_secs_f64() * 1000.0;
        drop(region);

        let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, materials);
        let thread_ms = std::thread::Builder::new()
            .name(format!("region {},{}", rc.x, rc.z))
            .spawn(move || {
                let t = Instant::now();
                let region = Region::from_columns(rc, columns);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                drop(region);
                ms
            })
            .unwrap()
            .join()
            .unwrap();
        println!(
            "round {round}: {chunks} chunks  main thread {main_ms:8.1} ms   spawned thread {thread_ms:8.1} ms"
        );
    }
}
