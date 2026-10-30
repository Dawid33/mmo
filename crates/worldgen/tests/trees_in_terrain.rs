//! Generated trees in the terrain: log / branch / leaf blocks carrying their
//! tree id, standing on the ground, the same however the region is split
//! into windows, and shown by coarse LOD as a canopy.

use std::collections::{BTreeMap, BTreeSet};

use game::{Block, Column, ColumnCoords, RegionCoords, SubChunk, CHUNK_BLOCKS, REGION_CHUNKS};
use worldgen::material::{Materials, TreeBlocks};
use worldgen::pipeline::SEA_TOP_BLOCK;

const CB: i32 = CHUNK_BLOCKS as i32;
const RB: i32 = game::REGION_BLOCKS as i32;

fn mats() -> Materials {
    let manifest = include_str!("../../../assets/blocks/blocks.ron");
    let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
    Materials::from_names(|n| reg.id_of(n).unwrap_or_else(|| panic!("missing block {n}")))
}

fn default_dials() -> bool {
    std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

/// Every materialised block of `cols`: world block `[x, y, z]` → (block,
/// chiseled).
fn blocks(rc: RegionCoords, cols: &[(ColumnCoords, Column)]) -> BTreeMap<[i32; 3], (Block, bool)> {
    let mut out = BTreeMap::new();
    for (cc, col) in cols {
        let (x0, z0) = (rc.x * RB + cc.x as i32 * CB, rc.z * RB + cc.z as i32 * CB);
        for (sub_y, sub) in (col.base_sub_y()..).zip(col.sub_chunks()) {
            let SubChunk::Dense(chunk) = sub else { continue };
            for (bi, block) in chunk.blocks.iter() {
                let (bx, by, bz) = bi.xyz();
                let p = [x0 + bx as i32, sub_y * CB + by as i32, z0 + bz as i32];
                out.insert(p, (block, chunk.chisel.contains_key(&bi)));
            }
        }
    }
    out
}

/// The terrain surface of world block column `(x, z)` (its `ColumnGround`).
fn surface(rc: RegionCoords, cols: &[(ColumnCoords, Column)], x: i32, z: i32) -> i32 {
    let (lx, lz) = (x - rc.x * RB, z - rc.z * RB);
    let cc = ColumnCoords::new((lx / CB) as usize, (lz / CB) as usize);
    let col = &cols.iter().find(|(c, _)| *c == cc).unwrap().1;
    i32::from(col.ground().unwrap().surface[(lx % CB) as usize * CHUNK_BLOCKS + (lz % CB) as usize])
}

/// A forested 64 m region near spawn (by world position, units), so it is the
/// same land at any region size.
fn forest() -> RegionCoords {
    RegionCoords::from_world(-1536.0, 2560.0)
}

fn forest_columns(mats: Materials) -> Vec<(ColumnCoords, Column)> {
    worldgen::generate_region(forest(), worldgen::WORLD_SEED, mats)
}

/// Tree blocks by tree id: world block → (block, chiseled).
fn trees_by_id(blocks: &BTreeMap<[i32; 3], (Block, bool)>) -> BTreeMap<u32, BTreeMap<[i32; 3], (Block, bool)>> {
    let mut out: BTreeMap<u32, BTreeMap<_, _>> = BTreeMap::new();
    for (&p, &b) in blocks {
        if b.0.tree_id() != 0 {
            out.entry(b.0.tree_id()).or_default().insert(p, b);
        }
    }
    out
}

#[test]
fn forested_region_has_trees() {
    if !default_dials() {
        return;
    }
    let m = mats();
    let blocks = blocks(forest(), &forest_columns(m));
    let trees = trees_by_id(&blocks);
    assert!(trees.len() >= 5, "only {} trees", trees.len());
    let ids: BTreeSet<_> = blocks.values().filter(|(b, _)| b.tree_id() != 0).map(|(b, _)| b.id).collect();
    let all: BTreeSet<_> = m.trees.species.iter().flat_map(|s| [s.log, s.branch, s.leaves]).collect();
    assert!(ids.is_subset(&all), "tree ids on non-tree blocks: {ids:?}");
    let has = |part: fn(&worldgen::material::SpeciesBlocks) -> game::BlockId| {
        m.trees.species.iter().any(|s| ids.contains(&part(s)))
    };
    assert!(has(|s| s.log) && has(|s| s.leaves), "logs and leaves: {ids:?}");
    // Every tree block is a tree's: no tree material without its id.
    assert!(blocks.values().all(|(b, _)| !all.contains(&b.id) || b.tree_id() != 0));
}

/// Trees only fill air: generating with trees changes no ground or water
/// block of the same region without them, and every tree block is above its
/// column's surface and the sea.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_tree_blocks_below_surface_or_in_water() {
    if !default_dials() {
        return;
    }
    let cols = forest_columns(mats());
    let with = blocks(forest(), &cols);
    let without = blocks(forest(), &forest_columns(Materials { trees: TreeBlocks::NONE, ..mats() }));
    let mut tree_blocks = 0;
    for (p, (b, chiseled)) in &with {
        let bare = without.get(p).copied();
        if b.tree_id() == 0 {
            assert_eq!(Some((*b, *chiseled)), bare.or(Some((Block::default(), false))), "terrain changed at {p:?}");
            continue;
        }
        tree_blocks += 1;
        assert!(bare.is_none_or(|(b, _)| b.id == game::BlockId::AIR), "tree block {p:?} replaced {bare:?}");
        assert!(p[1] > surface(forest(), &cols, p[0], p[2]) && p[1] > SEA_TOP_BLOCK, "tree block {p:?} too low");
    }
    for (p, bare) in &without {
        assert!(with.contains_key(p) || bare.0.id == game::BlockId::AIR, "lost {p:?}");
    }
    assert!(tree_blocks > 0);
    // `ColumnGround.surface` is still the terrain's.
    let bare = forest_columns(Materials { trees: TreeBlocks::NONE, ..mats() });
    for ((_, a), (_, b)) in cols.iter().zip(&bare) {
        assert_eq!(a.ground().unwrap().surface, b.ground().unwrap().surface);
    }
}

/// Review focus 1: a column comes out the same whether the region is made
/// whole or as two half windows (each window places the trees whose crowns
/// reach into it from outside).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn window_independent_generation() {
    if !default_dials() {
        return;
    }
    let (rc, seed, m) = (forest(), worldgen::WORLD_SEED, mats());
    let half = REGION_CHUNKS / 2;
    let whole = worldgen::generate_columns(rc, 0..REGION_CHUNKS, 0..REGION_CHUNKS, seed, m);
    for (xs, zs) in [(0..half, 0..REGION_CHUNKS), (0..REGION_CHUNKS, half..REGION_CHUNKS), (half..half + 1, 1..2)] {
        for (cc, col) in worldgen::generate_columns(rc, xs.clone(), zs.clone(), seed, m) {
            let want = &whole.iter().find(|(c, _)| *c == cc).unwrap().1;
            assert!(
                bincode::serialize(&col).unwrap() == bincode::serialize(want).unwrap(),
                "column {cc:?} differs in window {xs:?} x {zs:?}"
            );
        }
    }
    // Some tree straddles the split, so a window really did place a tree whose
    // base is outside it.
    let split = rc.x * RB + half as i32 * CB;
    let straddles = trees_by_id(&blocks(rc, &whole))
        .values()
        .any(|t| t.keys().any(|p| p[0] < split) && t.keys().any(|p| p[0] >= split));
    assert!(straddles, "no tree crosses the half-window split");
}

/// All blocks of one tree id are one 26-connected set standing on the ground
/// (some block sits right on its column's surface).
#[test]
fn every_tree_block_carries_its_tree_id() {
    if !default_dials() {
        return;
    }
    let cols = forest_columns(mats());
    let trees = trees_by_id(&blocks(forest(), &cols));
    assert!(!trees.is_empty());
    for (id, tree) in &trees {
        let start = *tree.keys().next().unwrap();
        let (mut seen, mut stack) = (BTreeSet::from([start]), vec![start]);
        while let Some(p) = stack.pop() {
            for d in (0..27).map(|i| [i % 3 - 1, (i / 3) % 3 - 1, i / 9 - 1]) {
                let n = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                if tree.contains_key(&n) && seen.insert(n) {
                    stack.push(n);
                }
            }
        }
        assert_eq!(seen.len(), tree.len(), "tree {id:#X} is in pieces");
        assert!(
            tree.keys().any(|p| p[1] == surface(forest(), &cols, p[0], p[2]) + 1),
            "tree {id:#X} does not touch the ground"
        );
    }
}

/// A factor-16 coarse cell (one block) over a trunk is topped with leaves.
#[test]
fn coarse_lod_shows_canopy() {
    if !default_dials() {
        return;
    }
    let m = mats();
    let rc = forest();
    let blocks = blocks(rc, &forest_columns(m));
    let leaves: BTreeSet<_> = m.trees.species.iter().map(|s| s.leaves).collect();
    let logs: BTreeSet<_> = m.trees.species.iter().map(|s| s.log).collect();
    let trunks: BTreeSet<[i32; 2]> =
        blocks.iter().filter(|(_, (b, _))| logs.contains(&b.id)).map(|(p, _)| [p[0], p[2]]).collect();
    assert!(!trunks.is_empty());

    let key = game::lod::LodKey::for_region(16, rc);
    let cells = worldgen::coarse_tile(&key, worldgen::WORLD_SEED, m);
    // Topmost non-air cell per (x, z) cell column, region-local blocks.
    let mut top: BTreeMap<[i32; 2], (i32, game::BlockId)> = BTreeMap::new();
    for c in &cells {
        for (i, v) in c.grid.iter().enumerate().filter(|(_, v)| !v.is_air()) {
            let (x, y, z) = (i % 32, (i / 32) % 32, i / 1024);
            let col = [c.group[0] * 32 + x as i32, c.group[2] * 32 + z as i32];
            let y = c.group[1] * 32 + y as i32;
            let e = top.entry(col).or_insert((y, v.block));
            if y > e.0 {
                *e = (y, v.block);
            }
        }
    }
    let canopy = trunks
        .iter()
        .filter(|t| top.get(&[t[0] - rc.x * RB, t[1] - rc.z * RB]).is_some_and(|(_, b)| leaves.contains(b)))
        .count();
    assert_eq!(canopy, trunks.len(), "trunk columns under a coarse canopy");
}
