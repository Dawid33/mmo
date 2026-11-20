//! Region-build profiling + determinism check for the genesis collider path.
//! Run: cargo run --release -p worldgen --example collider_profile
use game::Region;
use game::StateHash;
use std::time::Instant;
use worldgen::material::Materials;

fn region_hash(r: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    r.data().data.state_hash(&mut h);
    h.finalize()
}

fn main() {
    let materials = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let seed = worldgen::WORLD_SEED;
    let rc = game::RegionCoords::new(0, 0);

    // Worldgen (regenerated per build so the two builds are fully independent).
    let t = Instant::now();
    let cols = worldgen::generate_region(rc, seed, materials);
    let worldgen_dt = t.elapsed();
    let n_cols = cols.len();

    // Build #1 (timed): the real from_columns -> seed_terrain_genesis path.
    let t = Instant::now();
    let region1 = Region::from_columns(rc, cols);
    let build_dt = t.elapsed();
    let h1 = region_hash(&region1);

    // Build #2: independent worldgen + build, to confirm determinism.
    let cols2 = worldgen::generate_region(rc, seed, materials);
    let region2 = Region::from_columns(rc, cols2);
    let h2 = region_hash(&region2);

    println!("worldgen:            {worldgen_dt:?}  ({n_cols} columns)");
    println!("from_columns build:  {build_dt:?}");
    println!("state hash build #1: {h1:#010x}");
    println!("state hash build #2: {h2:#010x}");
    println!(
        "determinism: {}",
        if h1 == h2 {
            "OK (identical)"
        } else {
            "FAIL (hash drift!)"
        }
    );
}
