//! Headless probe: does a never-run region's streamed LOD payload cover the
//! whole region? Generates the region with the production seed, builds its
//! LOD payload through the same `build_lod_job` the server's workers call,
//! decodes it, and prints a top-down map of solid voxels per x/z cell group.
//!
//!   cargo run --release -p worldgen --example lod_coverage -- [rx rz [factor]]
//!
//! A region is 32x32 chunks, so factor 2 should fill a 16x16 map, factor 4 an
//! 8x8 one. Rows are z (top = low z), columns are x. `.` = the payload has no
//! cell for that x/z at all, `0` = cells exist but hold only air, `1`-`9` =
//! solid share of the fullest cell in tenths, `#` = completely solid.

use std::collections::BTreeMap;

use game::lod::LodKey;
use game::{BlockId, LodBuildJob, RegionCoords, build_lod_job};

fn main() {
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let region = RegionCoords::new(*args.first().unwrap_or(&0), *args.get(1).unwrap_or(&-1));
    let factor = *args.get(2).unwrap_or(&2) as u32;
    let materials = worldgen::material::Materials {
        stone: BlockId(1),
        dirt: BlockId(2),
        grass: BlockId(3),
        sand: BlockId(4),
        water: BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let seed = worldgen::WORLD_SEED;

    let columns = worldgen::generate_region(region, seed, materials);
    let materialized: usize = columns
        .iter()
        .map(|(_, c)| c.materialized_sub_chunks().len())
        .sum();
    println!(
        "region {region:?}: {} columns, {materialized} materialized sub-chunks",
        columns.len()
    );

    let key = LodKey::for_region(factor, region);
    let coarse = |k: &LodKey| worldgen::coarse_tile(k, seed, materials);
    let generate = |rc: RegionCoords| worldgen::generate_region(rc, seed, materials);
    let water = |k: &LodKey| worldgen::coarse_water(k, seed, materials);
    let (_, payload, _) = build_lod_job(
        LodBuildJob::Pristine { key, region },
        &coarse,
        &water,
        &generate,
        &game::NoStoredTerrain,
    );

    // x/z group -> (cells, max solid voxels in one cell)
    let mut map: BTreeMap<(i32, i32), (u32, usize)> = BTreeMap::new();
    let (mut ys, mut undecodable) = (BTreeMap::<i32, u32>::new(), 0);
    for (group, packed) in &payload {
        let Some(grid) = packed.decode() else {
            undecodable += 1;
            continue;
        };
        let solid = grid.iter().filter(|v| v.block != BlockId::AIR).count();
        let entry = map.entry((group[0], group[2])).or_default();
        entry.0 += 1;
        entry.1 = entry.1.max(solid);
        *ys.entry(group[1]).or_default() += 1;
    }
    println!(
        "factor {factor}: {} cells, {undecodable} undecodable, y groups {ys:?}",
        payload.len()
    );

    let span = (32 / factor as i32).max(1);
    let (min_x, max_x) = (
        map.keys().map(|k| k.0).min().unwrap_or(0).min(0),
        map.keys().map(|k| k.0).max().unwrap_or(0).max(span - 1),
    );
    let (min_z, max_z) = (
        map.keys().map(|k| k.1).min().unwrap_or(0).min(0),
        map.keys().map(|k| k.1).max().unwrap_or(0).max(span - 1),
    );
    println!(
        "x {min_x}..={max_x} (columns), z {min_z}..={max_z} (rows); expected 0..={}",
        span - 1
    );
    for z in min_z..=max_z {
        let row: String = (min_x..=max_x)
            .map(|x| match map.get(&(x, z)) {
                None => '.',
                Some((_, 0)) => '0',
                Some((_, solid)) if *solid == game::BRICK_VOXEL_COUNT => '#',
                Some((_, solid)) => {
                    char::from(b'1' + (solid * 9 / game::BRICK_VOXEL_COUNT).min(8) as u8)
                }
            })
            .collect();
        println!("  {row}");
    }
}
