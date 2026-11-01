//! Region cost and walkability of generated terrain (whole blocks + one-cuboid
//! half-block chisels since 2026-09-22; the voxel-chisel era numbers are in
//! docs/superpowers/specs/2026-09-22-block-and-half-block-worldgen.md).
//! Reports snapshot size, collider cells per grid, region build time and how
//! many neighbour steps need a jump.
//!
//!   cargo run --release -p worldgen --example quantize_probe -- [rx rz]

use std::collections::BTreeMap;
use std::time::Instant;

use game::{
    BlockId, Chunk, ChunkCoords, Region, RegionCoords, BLOCK_VOXELS, CHUNK_BLOCKS,
};

macro_rules! size {
    ($value:expr) => {
        bincode::serialized_size($value).map_or(0.0, |n| n as f64 / (1024.0 * 1024.0))
    };
}

const WATER: BlockId = BlockId(5);

/// Ground height (voxels) of every block column, sampled at the block's
/// center voxel column; water is not ground.
fn ground_heights(chunks: &[(ChunkCoords, Chunk)]) -> BTreeMap<(i32, i32), i32> {
    let mut out = BTreeMap::new();
    let mid = BLOCK_VOXELS / 2;
    for (cc, chunk) in chunks {
        for (bi, block) in chunk.blocks.iter() {
            let (bx, by, bz) = bi.xyz();
            if block.id == BlockId::AIR || block.id == WATER {
                continue;
            }
            let fill = match chunk.chisel.get(&bi) {
                Some(c) => (0..BLOCK_VOXELS).filter(|&vy| c.is_solid(mid, vy, mid)).count(),
                None => BLOCK_VOXELS,
            };
            if fill == 0 {
                continue;
            }
            let key = (
                (cc.x * CHUNK_BLOCKS + bx) as i32,
                (cc.z * CHUNK_BLOCKS + bz) as i32,
            );
            let top = (cc.y * CHUNK_BLOCKS as i32 + by as i32) * BLOCK_VOXELS as i32 + fill as i32;
            let e = out.entry(key).or_insert(top);
            *e = (*e).max(top);
        }
    }
    out
}

fn main() {
    let args: Vec<i32> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let rc = RegionCoords::new(*args.first().unwrap_or(&0), *args.get(1).unwrap_or(&0));
    let mats = worldgen::material::Materials {
        stone: BlockId(2),
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: WATER,
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let t = Instant::now();
    let base: Vec<(ChunkCoords, Chunk)> = worldgen::generate_region(rc, worldgen::WORLD_SEED, mats)
        .into_iter()
        .flat_map(|(cc, col)| {
            col.materialized_sub_chunks()
                .into_iter()
                .map(move |(sub_y, ch)| (ChunkCoords::new(cc.x, sub_y, cc.z), ch))
        })
        .collect();
    println!("region {rc:?}: generate_region {:.0} ms, {} chunks", t.elapsed().as_secs_f64() * 1e3, base.len());

    {
        let chunks = base;
        let chiseled: usize = chunks.iter().map(|(_, c)| c.chisel.len()).sum();
        let chisel_mib: f64 = chunks.iter().map(|(_, c)| size!(&c.chisel)).sum();
        let (mut block_cells, mut half_cells, mut voxel_cells) = (0usize, 0usize, 0usize);
        for (_, c) in &chunks {
            let cells = game::collider_cells(c);
            block_cells += cells.blocks.len();
            half_cells += cells.halves.len();
            voxel_cells += cells.voxels.len();
        }

        // Rises between 4-neighbour block columns, in voxels.
        let heights = ground_heights(&chunks);
        let step_vox = (game::movement::STEP_HEIGHT * BLOCK_VOXELS as f32) as i32;
        let (mut flat, mut sub_half, mut half, mut stepable, mut jump, mut cliff, mut pairs) = (0, 0, 0, 0, 0, 0, 0);
        let mut rise_levels = std::collections::BTreeSet::new();
        for (&(x, z), &h) in &heights {
            for n in [(x + 1, z), (x, z + 1)] {
                let Some(&hn) = heights.get(&n) else { continue };
                let d = (h - hn).abs();
                pairs += 1;
                rise_levels.insert(d % BLOCK_VOXELS as i32);
                match d {
                    0 => flat += 1,
                    d if d < 8 => sub_half += 1,
                    8 => half += 1,
                    d if d <= step_vox => stepable += 1,
                    d if d <= 2 * BLOCK_VOXELS as i32 => jump += 1,
                    _ => cliff += 1,
                }
            }
        }
        let pct = |n: usize| 100.0 * n as f64 / pairs.max(1) as f64;

        let t = Instant::now();
        let region = Region::from_chunks(rc, chunks);
        let build_ms = t.elapsed().as_secs_f64() * 1e3;
        let data = region.data().clone();
        let packet = game::ServerPacket::Region(rc, data);
        let wire = game::encode_packet(&packet).expect("encodes");

        println!("  chiseled blocks {chiseled:>6}   chisel data {chisel_mib:>6.2} MiB");
        println!("  collider cells: {block_cells} block-scale, {half_cells} half-block, {voxel_cells} voxel-scale");
        println!("  snapshot {:>6.2} MiB (colliders {:.2} MiB), wire {:.2} MiB, Region::from_chunks {build_ms:.0} ms",
            size!(&**region.data()), size!(&region.data().data.physics.colliders), wire.len() as f64 / (1024.0 * 1024.0));
        println!("  neighbour rises ({pairs} pairs): flat {:.1}%  <0.5m {:.1}%  =0.5m {:.1}%  0.5-{}m {:.1}%  jump(<=2m) {:.1}%  cliff {:.1}%",
            pct(flat), pct(sub_half), pct(half), game::movement::STEP_HEIGHT, pct(stepable), pct(jump), pct(cliff));
        println!("  distinct sub-block rise remainders (voxels): {:?}", rise_levels.len());
    }
}
