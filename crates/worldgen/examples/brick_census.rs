//! Brick census of one generated region: how many 32³ render bricks the
//! client spawns as entities today (per-chunk burial, `bricks::derive_chunk_bricks`),
//! how many of those are buried once the NEIGHBOURING chunks are considered
//! too, and how many can produce any face at all. Block-granular (a chiselled
//! block counts as see-through, so "has faces" is an upper bound).
//!
//!   just brick-census [--world <file>] [--region X,Z]
//!
//! `IMPLIED_AIR=1` treats the ground below a column's materialised band as
//! unknown (never burying) instead of the solid rock it is.

use std::collections::HashMap;
use std::path::PathBuf;

use game::{
    BRICK_BLOCKS, BRICKS_PER_CHUNK_EDGE, Block, BlockId, BlockIndex, BlockPos, CHUNK_BLOCKS,
    Column, REGION_BLOCKS, ReadVol, RegionCoords,
};
use worldgen::material::Materials;

thread_local!(static IMPLIED_SOLID: std::cell::Cell<bool> = const { std::cell::Cell::new(true) });

fn main() {
    IMPLIED_SOLID.with(|c| c.set(std::env::var("IMPLIED_AIR").is_err()));
    let mut world: Option<PathBuf> = None;
    let mut region = (0i32, 0i32);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--world" => world = Some(PathBuf::from(value)),
            "--region" => {
                let (x, z) = value.split_once(',').expect("--region X,Z");
                region = (x.trim().parse().unwrap(), z.trim().parse().unwrap());
            }
            other => panic!("unknown flag {other}"),
        }
    }
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid block manifest");
    game::install_block_props(reg.props());
    let mats =
        Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")));
    let coords = RegionCoords {
        x: region.0,
        z: region.1,
    };
    let t = std::time::Instant::now();
    let columns = match &world {
        Some(path) => {
            let w = worldgen::forged::ForgedWorld::load(path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            worldgen::generate_region_forged(&w, coords, mats)
        }
        None => worldgen::generate_region(coords, worldgen::WORLD_SEED, mats),
    };
    println!(
        "region {region:?} ({REGION_BLOCKS} m) generated in {:.2?}",
        t.elapsed()
    );

    let props = game::block_props();
    let opaque = |b: Block| b.id != BlockId::AIR && props.is_solid(b.id) && !props.is_cutout(b.id);
    let cols: HashMap<(usize, usize), &Column> =
        columns.iter().map(|(c, col)| ((c.x, c.z), col)).collect();
    let cb = CHUNK_BLOCKS as i32;
    // Global (region-local) block lookup; None outside the region.
    let block_at = |x: i32, y: i32, z: i32| -> Option<(Block, bool)> {
        if x < 0 || z < 0 || x >= REGION_BLOCKS as i32 || z >= REGION_BLOCKS as i32 {
            return None;
        }
        let col = cols.get(&((x / cb) as usize, (z / cb) as usize))?;
        let (lx, lz) = (x % cb, z % cb);
        // Below the materialised band (implied ground): unknown unless asked.
        if !IMPLIED_SOLID.with(|c| c.get()) && y.div_euclid(cb) < col.base_sub_y() {
            return None;
        }
        let block = col.get(BlockPos::new(lx, y, lz));
        let sub_y = y.div_euclid(cb);
        let chiselled = match col.sub_chunk_at(sub_y) {
            Some(game::SubChunk::Dense(c)) => c.chisel.contains_key(&BlockIndex::from_xyz(
                lx as usize,
                y.rem_euclid(cb) as usize,
                lz as usize,
            )),
            _ => false,
        };
        Some((block, chiselled))
    };
    let is_opaque = |x, y, z| block_at(x, y, z).is_some_and(|(b, ch)| !ch && opaque(b));

    let (mut chunks, mut uniform_solid_chunks) = (0usize, 0usize);
    let (mut spawned, mut buried_global, mut with_faces, mut air) =
        (0usize, 0usize, 0usize, 0usize);
    let n = BRICKS_PER_CHUNK_EDGE;
    let bb = BRICK_BLOCKS as i32;
    for (cc, col) in &columns {
        for (sub_y, chunk) in col.materialized_sub_chunks() {
            chunks += 1;
            if chunk.blocks.uniform().is_some_and(opaque) && chunk.chisel.is_empty() {
                uniform_solid_chunks += 1;
            }
            let origin = [cc.x as i32 * cb, sub_y * cb, cc.z as i32 * cb];
            // Per-brick: all air? all opaque?
            let brick_class = |b: [usize; 3]| -> (bool, bool) {
                let view = chunk.brick(b);
                let (mut any_solid, mut all_opaque) = (false, true);
                for z in 0..BRICK_BLOCKS {
                    for y in 0..BRICK_BLOCKS {
                        for x in 0..BRICK_BLOCKS {
                            let blk = view.block_at(x, y, z);
                            let ch = view.chisel(x, y, z).is_some();
                            any_solid |= blk.id != BlockId::AIR || ch;
                            all_opaque &= !ch && opaque(blk);
                        }
                    }
                }
                (!any_solid, all_opaque)
            };
            let classes: Vec<(bool, bool)> = (0..n * n * n)
                .map(|i| brick_class([i % n, (i / n) % n, i / (n * n)]))
                .collect();
            let idx = |b: [usize; 3]| b[0] + n * (b[1] + n * b[2]);
            for i in 0..n * n * n {
                let b = [i % n, (i / n) % n, i / (n * n)];
                let (is_air, is_opq) = classes[i];
                if is_air {
                    air += 1;
                    continue;
                }
                // Today's rule: buried only by in-chunk neighbours; face bricks always spawn.
                let in_chunk_buried = is_opq
                    && b.iter().all(|&v| v > 0 && v < n - 1)
                    && [
                        [1i32, 0, 0],
                        [-1, 0, 0],
                        [0, 1, 0],
                        [0, -1, 0],
                        [0, 0, 1],
                        [0, 0, -1],
                    ]
                    .iter()
                    .all(|o| {
                        let nb = [
                            (b[0] as i32 + o[0]) as usize,
                            (b[1] as i32 + o[1]) as usize,
                            (b[2] as i32 + o[2]) as usize,
                        ];
                        classes[idx(nb)].1
                    });
                if in_chunk_buried {
                    continue;
                }
                spawned += 1;
                let base = [
                    origin[0] + b[0] as i32 * bb,
                    origin[1] + b[1] as i32 * bb,
                    origin[2] + b[2] as i32 * bb,
                ];
                // Any block of the brick that is not opaque-surrounded => faces possible.
                let mut faces = false;
                'scan: for z in 0..bb {
                    for y in 0..bb {
                        for x in 0..bb {
                            let (px, py, pz) = (base[0] + x, base[1] + y, base[2] + z);
                            let Some((blk, ch)) = block_at(px, py, pz) else {
                                continue;
                            };
                            if blk.id == BlockId::AIR && !ch {
                                continue;
                            }
                            for o in [
                                [1, 0, 0],
                                [-1, 0, 0],
                                [0, 1, 0],
                                [0, -1, 0],
                                [0, 0, 1],
                                [0, 0, -1],
                            ] {
                                let (qx, qy, qz) = (px + o[0], py + o[1], pz + o[2]);
                                // Outside the region: unknown, counts as a possible face.
                                if block_at(qx, qy, qz).is_none() || !is_opaque(qx, qy, qz) {
                                    faces = true;
                                    break 'scan;
                                }
                            }
                        }
                    }
                }
                if faces {
                    with_faces += 1;
                } else if is_opq {
                    buried_global += 1;
                }
            }
        }
    }
    let total = chunks * n * n * n;
    println!("sim chunks materialised: {chunks} ({uniform_solid_chunks} uniform opaque)");
    println!("bricks in those chunks:  {total} ({air} all-air)");
    println!("spawned as entities today: {spawned}");
    println!(
        "  ...can produce a face:    {with_faces} ({:.1}%)",
        100.0 * with_faces as f64 / spawned.max(1) as f64
    );
    println!("  ...buried across chunks:  {buried_global}");
    println!(
        "  ...other (no faces, not all-opaque): {}",
        spawned - with_faces - buried_global
    );
}
