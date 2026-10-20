//! Headless stage timing of one server LOD build for a never-run region — the
//! work a `LodBuildJob::Pristine` does on a LOD worker thread:
//! generate -> materialize chunks -> downsample -> RLE-encode.
//!
//!   cargo run --release -p worldgen --example lod_build_profile -- [rx rz]
//!
//! Single-threaded on purpose: it is the per-key cost that, divided by
//! `LOD_BUILD_THREADS`, paces how fast the horizon fills after a join.

use std::time::Instant;

use game::lod::{downsample_chunks, PackedLodGrid};
use game::{BlockId, RegionCoords, TerrainSnapshot};

fn main() {
    let args: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let region = RegionCoords::new(*args.first().unwrap_or(&3), *args.get(1).unwrap_or(&-2));
    let materials = worldgen::material::Materials {
        stone: BlockId(1),
        dirt: BlockId(2),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
    };
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let columns = worldgen::generate_region(region, worldgen::WORLD_SEED, materials);
    let generate = ms(t);

    let t = Instant::now();
    let chunks = TerrainSnapshot::from_columns(columns).chunks;
    let materialize = ms(t);
    println!("region {region:?}: {} chunks", chunks.len());
    println!("  generate_region            {generate:8.1} ms");
    println!("  from_columns (materialize) {materialize:8.1} ms");

    for factor in [2u32, 4, 16] {
        let input = chunks.clone();
        let t = Instant::now();
        let cells = downsample_chunks(input, factor);
        let downsample = ms(t);
        let t = Instant::now();
        let bytes: usize = cells
            .iter()
            .map(|cell| PackedLodGrid::encode(&cell.grid).0.len() * 4)
            .sum();
        let encode = ms(t);
        println!(
            "  factor {factor:>2}: downsample {downsample:8.1} ms, encode {encode:6.1} ms, {} cells, ~{} KiB packed   => build total {:8.1} ms",
            cells.len(),
            bytes / 1024,
            generate + materialize + downsample + encode
        );
    }
}
