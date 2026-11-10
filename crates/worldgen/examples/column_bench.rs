//! Micro-benchmark attributing `generate_stud_region` cost to `TerrainPipeline::column`.
//! Run: cargo run -p worldgen --release --example column_bench
use std::hint::black_box;
use std::time::Instant;

use worldgen::pipeline::TerrainPipeline;

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

    // 4. A whole region's recipes and trees, for the share math.
    let t = Instant::now();
    let seed = worldgen::stud_gen::generate_stud_region(game::RegionCoords::new(3, 5), 1);
    let gen_t = t.elapsed();
    black_box(&seed);

    // The generator samples column() only on the corner lattice of its patch
    // (`PROFILE_LATTICE_BLOCKS` apart, the region plus the tree margin) and
    // interpolates every stud column's profile from it.
    let lattice = worldgen::pipeline::PROFILE_LATTICE_BLOCKS as f64;
    let per_edge = (game::REGION_BLOCKS as f64 / lattice).ceil() + 1.0;
    let lattice_calls = per_edge * per_edge;
    let modeled_lattice = lattice_calls * ns_per_column / 1e6;
    let gen_ms = gen_t.as_secs_f64() * 1e3;
    println!("generate_stud_region: {gen_t:?}");
    println!(
        "modeled lattice column() cost (region only, margin excluded): {:.2}ms of {:.0}ms ({:.1}%)  [{:.0} column() calls]",
        modeled_lattice,
        gen_ms,
        100.0 * modeled_lattice / gen_ms,
        lattice_calls,
    );
}
