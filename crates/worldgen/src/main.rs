use std::time::Instant;
use worldgen::material::Materials;

fn main() {
    let materials = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    };
    let seed = worldgen::WORLD_SEED;
    let n = 8; // regions per axis
    let start = Instant::now();
    let mut total_cols = 0usize;
    for x in 0..n {
        for z in 0..n {
            let cols = worldgen::generate_region(game::RegionCoords::new(x, z), seed, materials);
            total_cols += cols.len();
        }
    }
    let dt = start.elapsed();
    println!(
        "generated {} regions ({} columns) in {:?} ({:.2} regions/s)",
        n * n,
        total_cols,
        dt,
        (n * n) as f64 / dt.as_secs_f64()
    );
}
