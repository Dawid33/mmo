//! Micro-benchmark attributing `generate_region` cost to `TerrainPipeline::column`.
//! Run: cargo run -p worldgen --release --example column_bench
use std::hint::black_box;
use std::time::Instant;

use worldgen::pipeline::TerrainPipeline;

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
    }
}

fn main() {
    let p = TerrainPipeline::new(worldgen::WORLD_SEED);

    // 1. column() throughput over a spread of coordinates (defeats any cache).
    const N: usize = 1_000_000;
    let t = Instant::now();
    let mut acc = 0.0;
    for i in 0..N {
        let x = (i % 1024) as f64 * 0.9173;
        let z = (i / 1024) as f64 * 1.3117;
        let prof = p.column(x, z);
        acc += prof.density_at(64.0);
    }
    let col_t = t.elapsed();
    black_box(acc);
    let ns_per_column = col_t.as_nanos() as f64 / N as f64;
    println!("column():        {:>8.1} ns/call  ({N} calls in {col_t:?})", ns_per_column);

    // 2. density_at throughput (the cheap per-y part), one profile reused.
    let prof = p.column(123.0, 456.0);
    const M: usize = 10_000_000;
    let t = Instant::now();
    let mut acc = 0.0;
    for i in 0..M {
        acc += prof.density_at((i % 385) as f64 - 64.0);
    }
    let dy_t = t.elapsed();
    black_box(acc);
    println!("density_at():    {:>8.1} ns/call", dy_t.as_nanos() as f64 / M as f64);

    // 3. final_density (full 3D graph eval) for comparison.
    const K: usize = 200_000;
    let t = Instant::now();
    let mut acc = 0.0;
    for i in 0..K {
        let x = (i % 1024) as f64 * 0.9173;
        acc += p.final_density(x, 64.0, x * 0.7);
    }
    let fd_t = t.elapsed();
    black_box(acc);
    println!("final_density(): {:>8.1} ns/call", fd_t.as_nanos() as f64 / K as f64);

    // 4. Whole region, for the share math.
    let t = Instant::now();
    let cols = worldgen::generate_region(game::RegionCoords::new(3, 5), 1, mats());
    let gen_t = t.elapsed();
    black_box(&cols);

    // Lattice model (post-ProfileGrid, Task 5): generate_region samples column()
    // only on a 17x17 = 289-point PROFILE_LATTICE_BLOCKS=4 corner lattice per
    // region and bilinearly interpolates the ~1.05M surface-scan + straddle
    // queries (64x64 block columns for the scan, 64x64x256 voxel columns for
    // straddles) from that lattice instead of calling column() directly.
    let lattice_calls = 17.0 * 17.0;
    let modeled_lattice = lattice_calls * ns_per_column / 1e6;
    let gen_ms = gen_t.as_secs_f64() * 1e3;
    println!("generate_region: {gen_t:?}");
    println!(
        "modeled lattice column() cost: {:.2}ms of {:.0}ms ({:.1}%)  [{:.0} column() calls]",
        modeled_lattice,
        gen_ms,
        100.0 * modeled_lattice / gen_ms,
        lattice_calls,
    );

    // Pre-lattice reference model, kept for comparison: what the old
    // straddle-per-voxel-column approach (64x64 surface scan + 64x64x256
    // straddle = ~1.05M column() calls/region) would have cost at today's
    // measured ns/call.
    let scan_calls = 64.0 * 64.0;
    let straddle_calls = 64.0 * 64.0 * 256.0;
    let pre_lattice_modeled = (scan_calls + straddle_calls) * ns_per_column / 1e6;
    println!(
        "  (pre-lattice model would have cost: {:.0}ms  [{:.2}M column() calls])",
        pre_lattice_modeled,
        (scan_calls + straddle_calls) / 1e6,
    );
}
