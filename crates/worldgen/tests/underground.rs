//! Worldgen's half of the lazy underground: every generated column carries a
//! `ColumnGround` recipe, the band's deep solids are that recipe's blocks, and
//! no generated air or water block faces a missing chunk the sim would read
//! as implied ground (a cliff face would otherwise have no chunk to show).

use std::collections::BTreeMap;

use game::{
    BlockId, BlockIndex, Column, ColumnCoords, RegionCoords, SubChunk, CHUNK_BLOCKS, UNDERGROUND_DEPTH,
};
use worldgen::forged::{ForgedWorld, WorldArtifact, ARTIFACT_VERSION};

const STONE: BlockId = BlockId(2);
const WATER: BlockId = BlockId(5);
const BEDROCK: BlockId = BlockId(6);
/// worldgen's `MAX_SOIL_DEPTH`: solids deeper than this come from the recipe.
const SOIL_DEPTH: i32 = 3;
const CB: i32 = CHUNK_BLOCKS as i32;

fn mats() -> worldgen::material::Materials {
    worldgen::material::Materials {
        stone: STONE,
        dirt: BlockId(1),
        grass: BlockId(3),
        sand: BlockId(4),
        water: WATER,
        bedrock: BEDROCK,
        rocks: [7, 8, 9, 10, 11, 12].map(BlockId),
    }
}

fn default_dials() -> bool {
    std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

/// Every block of the materialised band of `col`: `(block x, y, z within the
/// footprint, sub-chunk)` with the block and whether it is chiseled.
fn band_blocks(col: &Column) -> Vec<((usize, i32, usize), BlockId, bool)> {
    let mut out = Vec::new();
    for (i, sub) in col.sub_chunks().iter().enumerate() {
        let sub_y = col.base_sub_y() + i as i32;
        for bx in 0..CHUNK_BLOCKS {
            for by in 0..CHUNK_BLOCKS {
                for bz in 0..CHUNK_BLOCKS {
                    let y = sub_y * CB + by as i32;
                    let (id, chiseled) = match sub {
                        SubChunk::Air => (BlockId::AIR, false),
                        SubChunk::Dense(c) => (
                            c.block_at(bx, by, bz).id,
                            c.chisel.contains_key(&BlockIndex::from_xyz(bx, by, bz)),
                        ),
                    };
                    out.push(((bx, y, bz), id, chiseled));
                }
            }
        }
    }
    out
}

/// Every air / water / chiseled block of `columns` whose face neighbour lies
/// in a missing implied chunk of a column of the same set (neighbours outside
/// the set are not checked).
fn exposed_implied(columns: &[(ColumnCoords, Column)]) -> Vec<([i32; 3], [i32; 3])> {
    let by_coords: BTreeMap<(i32, i32), &Column> =
        columns.iter().map(|(c, col)| ((c.x as i32, c.z as i32), col)).collect();
    let mut out = Vec::new();
    for (c, col) in columns {
        for ((bx, y, bz), id, chiseled) in band_blocks(col) {
            if !(id == BlockId::AIR || id == WATER || chiseled) {
                continue;
            }
            let pos = [c.x as i32 * CB + bx as i32, y, c.z as i32 * CB + bz as i32];
            for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                let n = [pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]];
                let Some(ncol) = by_coords.get(&(n[0].div_euclid(CB), n[2].div_euclid(CB))) else { continue };
                let g = ncol.ground().expect("every generated column carries ground");
                if g.is_implied(n[1].div_euclid(CB)) {
                    out.push((pos, n));
                }
            }
        }
    }
    out
}

#[test]
fn procedural_columns_carry_plain_ground() {
    if !default_dials() {
        return;
    }
    let cols = worldgen::generate_region(RegionCoords::new(0, 0), worldgen::WORLD_SEED, mats());
    for (c, col) in &cols {
        let g = col.ground().unwrap_or_else(|| panic!("column {c:?} has no ground"));
        assert_eq!(g.strata.len(), 1, "column {c:?}");
        assert_eq!(g.band_base_sub_y, col.base_sub_y(), "column {c:?}");
        for lx in 0..CHUNK_BLOCKS {
            for lz in 0..CHUNK_BLOCKS {
                let s = i32::from(g.surface[lx * CHUNK_BLOCKS + lz]);
                assert_eq!(g.block_at(lx, lz, s - UNDERGROUND_DEPTH).id, BEDROCK, "column {c:?}");
                assert_eq!(g.block_at(lx, lz, s - UNDERGROUND_DEPTH + 1).id, STONE, "column {c:?}");
            }
        }
    }
}

#[test]
fn band_and_ground_agree() {
    if !default_dials() {
        return;
    }
    let cols = worldgen::generate_region(RegionCoords::new(1, -2), worldgen::WORLD_SEED, mats());
    let mut checked = 0;
    for (c, col) in &cols {
        let g = col.ground().unwrap();
        for ((bx, y, bz), id, _) in band_blocks(col) {
            let surface = i32::from(g.surface[bx * CHUNK_BLOCKS + bz]);
            if y >= surface - SOIL_DEPTH {
                continue;
            }
            assert_eq!(id, g.block_at(bx, bz, y).id, "column {c:?} block ({bx}, {y}, {bz})");
            checked += 1;
        }
    }
    assert!(checked > 0, "the band holds deep solids to compare");
    assert!(exposed_implied(&cols).is_empty(), "{:?}", &exposed_implied(&cols)[..1]);
}

/// Step height (m) of the synthetic cliff. The forged ground is latticed at
/// 4 m (`PROFILE_LATTICE_BLOCKS`), so a step spreads over a lattice cell: a
/// 40 m step leaves ~10 blocks between neighbouring block columns. The step
/// must exceed `SOIL_DEPTH + 2 * CB` blocks (the cliff test's bar), so it
/// scales with the chunk height: 40 m at 2-block chunks.
const STEP_M: f32 = (4 * (SOIL_DEPTH + 2 * CB) + 12) as f32;
const N: usize = 64;
const CELL_M: f64 = 4.0;
const STEP_IX: usize = 32;
/// Engine (8.5, 8.5) is artifact SPAWN: the step lands ~22 m east of it,
/// inside region (0, 0).
const SPAWN: [f64; 2] = [STEP_IX as f64 * CELL_M - 22.0, 128.0];

/// A dry plateau north-south, with a cliff of `STEP_M` facing west.
fn cliff_world() -> ForgedWorld {
    let height = (0..N * N)
        .map(|i| if i % N < STEP_IX { 6.0 } else { 6.0 + STEP_M })
        .collect();
    ForgedWorld::new(WorldArtifact {
        version: ARTIFACT_VERSION,
        seed: 7,
        size_m: N as f64 * CELL_M,
        cell_m: CELL_M,
        n: N as u32,
        height,
        lake_level: vec![f32::NAN; N * N],
        uplifted: vec![0.0; N * N],
        bedding: vec![0.0; N * N],
        rain: vec![1.0; N * N],
        coast_dist_m: vec![0.0; N * N],
        column: vec![(700.0, 0)],
        rock_names: vec!["sandstone".into()],
        rivers: vec![],
        lakes: vec![],
        spawn: SPAWN,
    })
}

#[test]
fn cliffs_are_closed_at_genesis() {
    let world = cliff_world();
    let cols = worldgen::generate_region_forged(&world, RegionCoords::new(0, 0), mats());
    // The cliff is there: some neighbouring block columns differ by many blocks.
    let tops: BTreeMap<i32, i32> = cols
        .iter()
        .flat_map(|(c, col)| {
            let g = col.ground().unwrap();
            (0..CHUNK_BLOCKS).map(move |lx| (c.x as i32 * CB + lx as i32, i32::from(g.surface[lx * CHUNK_BLOCKS])))
        })
        .collect();
    let steepest = tops.values().zip(tops.values().skip(1)).map(|(a, b)| (a - b).abs()).max().unwrap();
    assert!(steepest > SOIL_DEPTH + 2 * CB, "the synthetic cliff is steep: {steepest} blocks");
    let exposed = exposed_implied(&cols);
    assert!(exposed.is_empty(), "{} exposed, first {:?}", exposed.len(), exposed.first());
}
