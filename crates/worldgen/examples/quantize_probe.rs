//! Region cost and walkability of generated terrain (whole blocks + slopes on
//! one-block rises since 2026-11-06, docs/superpowers/specs/2026-11-06-terrain-slopes-design.md;
//! the half-block era is docs/superpowers/specs/2026-09-22-block-and-half-block-worldgen.md).
//! Reports snapshot size, collider cells per grid, region build time, slopes
//! by shape and how many neighbour rises are walkable without a jump.
//!
//!   just quantize-probe [rx rz]

use std::collections::BTreeMap;
use std::time::Instant;

use game::{BlockId, CHUNK_BLOCKS, Chunk, ChunkCoords, Region, RegionCoords, SlopeShape};

macro_rules! size {
    ($value:expr) => {
        bincode::serialized_size($value).map_or(0.0, |n| n as f64 / (1024.0 * 1024.0))
    };
}

const WATER: BlockId = BlockId(5);

/// The ground's top block y of every block column, and its slope if the top
/// block is one; water is not ground.
fn ground_tops(chunks: &[(ChunkCoords, Chunk)]) -> BTreeMap<(i32, i32), (i32, Option<SlopeShape>)> {
    let mut out: BTreeMap<(i32, i32), (i32, Option<SlopeShape>)> = BTreeMap::new();
    for (cc, chunk) in chunks {
        for (bi, block) in chunk.blocks.iter() {
            let (bx, by, bz) = bi.xyz();
            if block.id == BlockId::AIR || block.id == WATER {
                continue;
            }
            let key = (
                (cc.x * CHUNK_BLOCKS + bx) as i32,
                (cc.z * CHUNK_BLOCKS + bz) as i32,
            );
            let y = cc.y * CHUNK_BLOCKS as i32 + by as i32;
            let slope = chunk.chisel.get(&bi).and_then(game::slope::classify);
            let e = out.entry(key).or_insert((y, slope));
            if y > e.0 {
                *e = (y, slope);
            }
        }
    }
    out
}

fn main() {
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
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
    println!(
        "region {rc:?}: generate_region {:.0} ms, {} chunks",
        t.elapsed().as_secs_f64() * 1e3,
        base.len()
    );

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

        // Slopes by shape.
        let tops = ground_tops(&chunks);
        let (mut straight, mut outer, mut inner) = (0, 0, 0);
        for (_, slope) in tops.values() {
            match slope {
                Some(SlopeShape::Straight { .. }) => straight += 1,
                Some(SlopeShape::Outer { .. }) => outer += 1,
                Some(SlopeShape::Inner { .. }) => inner += 1,
                None => {}
            }
        }
        // Rises between 4-neighbour block columns, in blocks: a one-block
        // rise is walkable when the higher column's slope takes a walk-up
        // from the lower one's side (`game::walk_up`), else it is a jump.
        let (mut flat, mut walkable, mut jump, mut cliff, mut pairs) = (0, 0, 0, 0, 0);
        for (&(x, z), &(h, slope)) in &tops {
            for (dx, dz) in [(1, 0), (0, 1)] {
                let Some(&(hn, slope_n)) = tops.get(&(x + dx, z + dz)) else {
                    continue;
                };
                pairs += 1;
                match (h - hn).abs() {
                    0 => flat += 1,
                    1 => {
                        // The high column's slope, and the low side from it.
                        let (high, from) = if h > hn {
                            (slope, [dx, dz])
                        } else {
                            (slope_n, [-dx, -dz])
                        };
                        if high.is_some_and(|s| game::walk_up(s, from)) {
                            walkable += 1;
                        } else {
                            jump += 1;
                        }
                    }
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
        println!(
            "  collider cells: {block_cells} block-scale, {half_cells} half-block, {voxel_cells} voxel-scale"
        );
        println!(
            "  snapshot {:>6.2} MiB (colliders {:.2} MiB), wire {:.2} MiB, Region::from_chunks {build_ms:.0} ms",
            size!(&**region.data()),
            size!(&region.data().data.physics.colliders),
            wire.len() as f64 / (1024.0 * 1024.0)
        );
        println!(
            "  slopes {}: straight {straight}, outer {outer}, inner {inner}",
            straight + outer + inner
        );
        println!(
            "  neighbour rises ({pairs} pairs): flat {:.1}%  1 block walkable (slope) {:.1}%  1 block jump {:.1}%  cliff (2+) {:.1}%",
            pct(flat),
            pct(walkable),
            pct(jump),
            pct(cliff)
        );
        let rises = walkable + jump;
        println!(
            "  one-block rises walkable without a jump: {walkable}/{rises} ({:.1}%)",
            100.0 * walkable as f64 / rises.max(1) as f64
        );
    }
}
