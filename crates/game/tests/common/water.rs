//! Recipe water for test fixtures (water plan Task 5): water is the ground
//! recipe's `water_top` over AIR cells (`ColumnGround::recipe_water`), not
//! liquid blocks. [`floor_with_water`] builds it into a region's genesis
//! columns (what the seam harness's ground hook uses); [`set_recipe_water`]
//! writes it into an already built region (the hand-built fixtures).
use std::ops::Range;

use game::{
    Block, BlockId, Chunk, Column, ColumnCoords, ColumnGround, RegionCoords, Rollback, SubChunk, BLOCK_VOXELS,
    CHUNK_BLOCKS, COLUMN_FOOTPRINT, REGION_BLOCKS, REGION_CHUNKS,
};

/// The stone and bedrock of the fixtures' ground recipes.
pub const RECIPE_STONE: BlockId = BlockId(1);
pub const RECIPE_BEDROCK: BlockId = BlockId(6);

/// Recipe water over the WORLD block columns `xs × zs`: water fills block
/// levels `surface + 1 ..= water_top`.
#[derive(Debug, Clone)]
pub struct WaterBox {
    pub xs: Range<i64>,
    pub zs: Range<i64>,
    pub surface: i32,
    pub water_top: i32,
}

/// `uniform_floor` (a one-block floor at block y 0 under every chunk
/// column) with a ground recipe on every column — surface 0, band base 0 —
/// carrying the recipe water of `boxes` that falls in region `rc`.
pub fn floor_with_water(rc: RegionCoords, boxes: &[WaterBox]) -> Vec<(ColumnCoords, Column)> {
    let air = Block::new(BlockId::AIR);
    let n = REGION_BLOCKS as i64;
    let cb = CHUNK_BLOCKS;
    let mut out = Vec::new();
    for cx in 0..REGION_CHUNKS {
        for cz in 0..REGION_CHUNKS {
            let mut g = ColumnGround::plain(vec![0; COLUMN_FOOTPRINT].into_boxed_slice(), 0, RECIPE_STONE, RECIPE_BEDROCK);
            for lx in 0..cb {
                for lz in 0..cb {
                    let wx = rc.x as i64 * n + (cx * cb + lx) as i64;
                    let wz = rc.z as i64 * n + (cz * cb + lz) as i64;
                    if let Some(b) = boxes.iter().find(|b| b.xs.contains(&wx) && b.zs.contains(&wz)) {
                        g.surface[lx * cb + lz] = b.surface as i16;
                        g.water_top[lx * cb + lz] = b.water_top as i16;
                    }
                }
            }
            let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(BLOCK_VOXELS as u32)));
            out.push((ColumnCoords::new(cx, cz), Column::new(0, air, air, vec![sub]).with_ground(g)));
        }
    }
    out
}

/// Recipe water over the region-local block columns `xs × zs` of an
/// already built region: every cell of `surface + 1 ..= water_top` there is
/// set to air (undo-safely, as a dig would), and the columns' recipes get
/// `surface` and `water_top`. A footprint without a recipe gets a plain one
/// (band base 0) whose other columns' surface is `dry_surface`. The bodies
/// are relabelled over the whole region and seeded raw, as at genesis.
pub fn set_recipe_water(d: &mut Rollback, xs: Range<i32>, zs: Range<i32>, surface: i32, water_top: i32, dry_surface: i32) {
    let air = Block::new(BlockId::AIR);
    for x in xs.clone() {
        for z in zs.clone() {
            for y in surface + 1..=water_top {
                if d.block_at([x, y, z]).is_some_and(|b| b != air) {
                    assert!(d.set_block_safe([x, y, z], air), "clear {x},{y},{z}");
                }
            }
        }
    }
    let cb = CHUNK_BLOCKS as i32;
    let mut ground: std::collections::BTreeMap<ColumnCoords, ColumnGround> =
        d.ground_recipes().iter().map(|(c, g)| (*c, g.clone())).collect();
    for x in xs {
        for z in zs.clone() {
            let cc = ColumnCoords::new(x.div_euclid(cb) as usize, z.div_euclid(cb) as usize);
            let g = ground.entry(cc).or_insert_with(|| {
                ColumnGround::plain(
                    vec![dry_surface as i16; COLUMN_FOOTPRINT].into_boxed_slice(),
                    0,
                    RECIPE_STONE,
                    RECIPE_BEDROCK,
                )
            });
            let i = x.rem_euclid(cb) as usize * CHUNK_BLOCKS + z.rem_euclid(cb) as usize;
            g.surface[i] = surface as i16;
            g.water_top[i] = water_top as i16;
        }
    }
    let mut ground: Vec<(ColumnCoords, ColumnGround)> = ground.into_iter().collect();
    let bodies = {
        let d: &Rollback = d;
        game::label_water_bodies(&mut ground, |c, (lx, lz), y| {
            let p = [c.x as i32 * cb + lx as i32, y, c.z as i32 * cb + lz as i32];
            d.block_at(p).is_some_and(|b| b.id != BlockId::AIR && !game::block_props().is_liquid(b.id))
        })
    };
    d.seed_ground_genesis(ground);
    d.seed_water_genesis(bodies);
}
