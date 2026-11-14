//! The paint pass: a structure source's `Paint` recolours the top ground block of every column in a
//! rectangle (a road, a plaza) — heights, slopes, water and the ground recipe untouched, no tree standing
//! on it, a building's box beating it — the same in every window and region, and in the coarse LOD tile.
//! Procedural terrain through the explicit `*_with` entries: nothing is installed.

use std::collections::{BTreeMap, BTreeSet};

use game::lod::LodKey;
use game::{Block, BlockId, ChiselData, RegionCoords, REGION_CHUNKS};
use worldgen::structures::*;

use crate::structure_support::world::Regions;
use crate::structure_support::*;

fn road() -> BlockId {
    registry().id_of("cobbled_road").expect("cobbled_road in blocks.ron")
}

fn in_rect([x0, z0, x1, z1]: [i32; 4], x: i32, z: i32) -> bool {
    (x0..x1).contains(&x) && (z0..z1).contains(&z)
}

fn columns_of([x0, z0, x1, z1]: [i32; 4]) -> impl Iterator<Item = (i32, i32)> {
    (x0..x1).flat_map(move |x| (z0..z1).map(move |z| (x, z)))
}

fn painted(rect: [i32; 4], placements: Vec<StructurePlacement>) -> StructureList {
    StructureList::with_paints(placements, vec![Paint { rect, block: road() }], 1)
}

/// Every stored cell of block column (x, z), bottom up: (y, block, chisel).
fn stored_column(r: &Regions, x: i32, z: i32) -> Vec<(i32, Block, Option<ChiselData>)> {
    let (col, _, _) = world::column(r, x, z);
    let lo = col.base_sub_y() * CB;
    let hi = (col.base_sub_y() + col.sub_chunks().len() as i32) * CB;
    (lo..hi).map(|y| {
        let (b, c) = world::stored(r, [x, y, z]).unwrap();
        (y, b, c)
    })
    .collect()
}

/// The base (lowest block) of every tree in `r`, by tree id.
fn tree_bases(r: &Regions) -> BTreeMap<u32, [i32; 3]> {
    let mut base: BTreeMap<u32, [i32; 3]> = BTreeMap::new();
    for (p, b) in world::tree_blocks(r) {
        let e = base.entry(b.tree_id()).or_insert(p);
        if p[1] < e[1] {
            *e = p;
        }
    }
    base
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_paint_is_byte_identical() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    for rc in [forest(), RegionCoords::new(0, 0)] {
        let bare = bytes(&worldgen::generate_region_with(rc, SEED, mats(), &NoStructures));
        let empty = StructureList::with_paints(vec![], vec![], 3);
        assert!(bytes(&worldgen::generate_region_with(rc, SEED, mats(), &empty)) == bare, "empty list {rc:?}");
        let (x, z) = (rc.x * RB - 400, rc.z * RB - 400);
        let far = painted([x, z, x + 5, z + 40], vec![]);
        assert!(bytes(&worldgen::generate_region_with(rc, SEED, mats(), &far)) == bare, "far paint {rc:?}");
        for f in [16, 64] {
            let key = LodKey::for_region(f, rc);
            let cells = |src: &dyn StructureSource| -> Vec<_> {
                worldgen::coarse_tile_with(&key, SEED, mats(), src).into_iter().map(|c| (c.group, c.grid)).collect()
            };
            assert!(cells(&far) == cells(&NoStructures), "far paint, coarse {key:?}");
        }
    }
}

/// A `w × d` rectangle (x by z) inside one of a few regions, inset by `inset`, every column of it dry
/// and free of tree blocks; with that region's bare generation.
fn dry_treeless_rect(w: i32, d: i32, inset: i32) -> ([i32; 4], RegionCoords, Regions) {
    for rc in [RegionCoords::new(0, 0), forest(), RegionCoords::new(1, 0), RegionCoords::new(0, 1), RegionCoords::new(-3, -2)] {
        let bare = world::generate(&[rc], &NoStructures);
        let treed: BTreeSet<(i32, i32)> = world::tree_blocks(&bare).keys().map(|p| (p[0], p[2])).collect();
        let (r0x, r0z) = (rc.x * RB, rc.z * RB);
        for x0 in r0x + inset..=r0x + RB - inset - w {
            for z0 in r0z + inset..=r0z + RB - inset - d {
                let rect = [x0, z0, x0 + w, z0 + d];
                if columns_of(rect).all(|(x, z)| world::dry(&bare, x, z) && !treed.contains(&(x, z))) {
                    return (rect, rc, bare);
                }
            }
        }
    }
    panic!("no dry tree-free {w} x {d} rectangle in the regions scanned");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_road_recolours_the_top_block_only() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let (rect, rc, bare) = dry_treeless_rect(40, 5, 2);
    let r = world::generate(&[rc], &painted(rect, vec![]));
    let road = road();
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    for x in r0x..r0x + RB {
        for z in r0z..r0z + RB {
            let (bc, _, _) = world::column(&bare, x, z);
            let (pc, _, _) = world::column(&r, x, z);
            assert!(bytes(bc.ground().unwrap()) == bytes(pc.ground().unwrap()), "ColumnGround at ({x}, {z})");
            if !in_rect(rect, x, z) {
                assert!(stored_column(&r, x, z) == stored_column(&bare, x, z), "column ({x}, {z}) outside the road changed");
                continue;
            }
            let top = world::ground(&bare, x, z);
            assert_eq!(world::ground(&r, x, z), top, "height at ({x}, {z})");
            let (b, c) = world::read(&r, [x, top, z]);
            assert_eq!(b.id, road, "top block at ({x}, {top}, {z})");
            assert_eq!(c.is_some(), world::read(&bare, [x, top, z]).1.is_some(), "slope at ({x}, {z})");
            assert_eq!(world::read(&r, [x, top - 1, z]), world::read(&bare, [x, top - 1, z]), "under the road at ({x}, {z})");
            assert_eq!(world::read(&r, [x, top + 1, z]), world::read(&bare, [x, top + 1, z]), "over the road at ({x}, {z})");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_road_keeps_slopes() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let road = road();
    for rc in [forest(), RegionCoords::new(0, 0), RegionCoords::new(-3, -2)] {
        let bare = world::generate(&[rc], &NoStructures);
        let (r0x, r0z) = (rc.x * RB, rc.z * RB);
        let sloped = |x: i32, z: i32| world::dry(&bare, x, z) && world::read(&bare, [x, world::ground(&bare, x, z), z]).1.is_some();
        // The 5 × 5 rectangle (inside the region by 2) holding the most dry sloped columns.
        let best = (r0x + 2..r0x + RB - 7)
            .flat_map(|x| (r0z + 2..r0z + RB - 7).map(move |z| [x, z, x + 5, z + 5]))
            .map(|rect| (columns_of(rect).filter(|&(x, z)| sloped(x, z)).count(), rect))
            .max_by_key(|&(n, rect)| (n, std::cmp::Reverse(rect)));
        let Some((n, rect)) = best.filter(|b| b.0 >= 3) else { continue };
        let r = world::generate(&[rc], &painted(rect, vec![]));
        let mut checked = 0;
        for (x, z) in columns_of(rect).filter(|&(x, z)| sloped(x, z)) {
            let top = world::ground(&bare, x, z);
            let shape = game::slope::classify(&world::read(&bare, [x, top, z]).1.unwrap()).expect("a slope");
            assert_eq!(world::ground(&r, x, z), top);
            assert_eq!(world::read(&r, [x, top, z]), (Block::new(road), Some(game::slope_chisel(road, shape))), "({x}, {top}, {z})");
            checked += 1;
        }
        assert_eq!(checked, n);
        return;
    }
    panic!("no sloped dry ground found");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_trees_on_a_road() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let bases = tree_bases(&bare);
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    // The 5-wide road along x (inside the region by 4) over the most trunks.
    let (n, rect) = (r0z + 4..r0z + RB - 9)
        .map(|z| [r0x + 4, z, r0x + RB - 4, z + 5])
        .map(|rect| (bases.values().filter(|p| in_rect(rect, p[0], p[2])).count(), rect))
        .max_by_key(|&(n, rect)| (n, std::cmp::Reverse(rect)))
        .unwrap();
    assert!(n >= 2, "only {n} trunks under the road");
    let r = world::generate(&[rc], &painted(rect, vec![]));
    for (id, p) in tree_bases(&r) {
        assert!(!in_rect(rect, p[0], p[2]), "tree {id} stands on the road at {p:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_building_beats_a_road() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (w, d, skirt) = (7, 7, 4);
    let m = skirt + 3;
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = world::dry_site(&bare, [r0x + m, r0z + m, r0x + RB - m, r0z + RB - m], w, d, skirt).expect("a dry site");
    let level = world::ground(&bare, ax + w / 2, az + d / 2);
    let sp = placed(&prefabs::hut(w as u32, 5, d as u32), [ax, level + 1, az], Some(Pad { level, skirt: skirt as u32 }));
    // A road through the box's middle rows, past the skirt on both sides.
    let rect = [ax - skirt - 3, az + 2, ax + w + skirt + 3, az + 5];
    let pad_only = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
    let r = world::generate(&[rc], &painted(rect, vec![sp.clone()]));
    let road = road();
    let (mut boxed, mut skirted) = (0, 0);
    for (x, z) in columns_of(rect) {
        let dist = box_distance(&sp, x, z);
        let top = world::ground(&pad_only, x, z);
        assert_eq!(world::ground(&r, x, z), top, "height at ({x}, {z})");
        if dist == 0 {
            assert_eq!(top, level);
            let got = world::read(&r, [x, level, z]);
            assert_ne!(got.0.id, road, "a road block in the box at ({x}, {z})");
            assert_eq!(got, world::read(&pad_only, [x, level, z]), "the pad's top block at ({x}, {z})");
            boxed += 1;
        } else if dist <= skirt {
            assert_eq!(world::read(&r, [x, top, z]).0.id, road, "the skirt's top block at ({x}, {top}, {z})");
            skirted += 1;
        }
    }
    assert_eq!(boxed, w * 3);
    assert_eq!(skirted, 2 * skirt * 3);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn window_and_region_independence() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let c = forest();
    let (cx, cz) = (c.x * RB, c.z * RB);
    // A 24 × 24 plaza on the corner of four regions.
    let rect = [cx - 12, cz - 12, cx + 12, cz + 12];
    let list = painted(rect, vec![]);
    let half = REGION_CHUNKS / 2;
    let all = 0..REGION_CHUNKS;
    let mut roads = 0;
    for (dx, dz) in [(-1, -1), (0, -1), (-1, 0), (0, 0)] {
        let rc = RegionCoords::new(c.x + dx, c.z + dz);
        let whole = worldgen::generate_region_with(rc, SEED, mats(), &list);
        let by_col: BTreeMap<_, _> = whole.iter().map(|(cc, col)| ((cc.x, cc.z), bytes(col))).collect();
        let corner = |d: i32| if d < 0 { REGION_CHUNKS - 1 } else { 0 };
        let (kx, kz) = (corner(dx), corner(dz));
        let windows = [
            (0..half, all.clone()),
            (half..REGION_CHUNKS, all.clone()),
            (all.clone(), 0..half),
            (kx..kx + 1, kz..kz + 1),
        ];
        for (xs, zs) in windows {
            let part = worldgen::generate_columns_with(rc, xs.clone(), zs.clone(), SEED, mats(), &list);
            assert_eq!(part.len(), xs.len() * zs.len());
            for (cc, col) in &part {
                assert!(by_col[&(cc.x, cc.z)] == bytes(col), "region {rc:?} column {cc:?} of window {xs:?} x {zs:?}");
            }
        }
        let r: Regions = [((rc.x, rc.z), whole)].into_iter().collect();
        for (x, z) in columns_of(rect).filter(|&(x, z)| x.div_euclid(RB) == rc.x && z.div_euclid(RB) == rc.z) {
            roads += usize::from(world::read(&r, [x, world::ground(&r, x, z), z]).0.id == road());
        }
    }
    assert!(roads > 0, "the plaza shows nowhere");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn water_and_ice_are_left_alone() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    // A 3 × 3 rectangle of open water on the coast north-east of spawn (that region, else a neighbour).
    let coast = RegionCoords::from_world(5632.0, -2560.0);
    let neighbours = [(0, 0), (1, 0), (0, 1), (-1, 0), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)];
    let site = neighbours.into_iter().find_map(|(dx, dz)| {
        let rc = RegionCoords::new(coast.x + dx, coast.z + dz);
        let bare = world::generate(&[rc], &NoStructures);
        let wet = |x: i32, z: i32| {
            let (col, lx, lz) = world::column(&bare, x, z);
            col.ground().unwrap().recipe_water(lx, lz).is_some()
        };
        let (r0x, r0z) = (rc.x * RB, rc.z * RB);
        (r0x + 2..r0x + RB - 5)
            .flat_map(|x| (r0z + 2..r0z + RB - 5).map(move |z| [x, z, x + 3, z + 3]))
            .find(|&rect| columns_of(rect).all(|(x, z)| wet(x, z)))
            .map(|rect| (rc, rect))
    });
    let (rc, rect) = site.expect("open water on the coast");
    let bare = bytes(&worldgen::generate_region_with(rc, SEED, mats(), &NoStructures));
    assert!(bytes(&worldgen::generate_region_with(rc, SEED, mats(), &painted(rect, vec![]))) == bare, "paint over water at {rect:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_coarse_tile_shows_the_road() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    // A road 32 wide (z) by 48 long (x) through the forest; its middle strip lies further than any crown
    // reaches from the ground beside the road.
    let rect = [r0x + 8, r0z + 16, r0x + 56, r0z + 48];
    let strip = TREE_REACH + 4;
    let list = painted(rect, vec![]);
    let road = road();
    let leaves: Vec<BlockId> = mats().trees.species.iter().map(|s| s.leaves).collect();
    for factor in [16, 64] {
        let key = LodKey::for_region(factor, rc);
        let cb = (factor / game::BLOCK_VOXELS as u32) as i32;
        let cells = RB / cb;
        let column = |grids: &BTreeMap<[i32; 3], Vec<game::Voxel>>, cx: i32, cz: i32| {
            let mut out = BTreeMap::new();
            for (g, grid) in grids {
                if g[0] != cx.div_euclid(32) || g[2] != cz.div_euclid(32) {
                    continue;
                }
                let (lx, lz) = (cx.rem_euclid(32) as usize, cz.rem_euclid(32) as usize);
                for ly in 0..32 {
                    let v = grid[lx + 32 * ly + 32 * 32 * lz];
                    if !v.is_air() {
                        out.insert(g[1] * 32 + ly as i32, v.block);
                    }
                }
            }
            out
        };
        let grids = |src: &dyn StructureSource| -> BTreeMap<[i32; 3], Vec<game::Voxel>> {
            worldgen::coarse_tile_with(&key, SEED, mats(), src).into_iter().map(|c| (c.group, c.grid)).collect()
        };
        let (bare, with) = (grids(&NoStructures), grids(&list));
        let water = worldgen::coarse_water_with(&key, SEED, mats(), &NoStructures).decode().unwrap();
        let ground_top = |col: &BTreeMap<i32, BlockId>| *col.iter().rev().find(|(_, b)| !leaves.contains(b)).unwrap().0;
        let (mut shown, mut canopy_bare) = (0, 0);
        for cx in 0..cells {
            for cz in 0..cells {
                let (x, z) = (r0x + cx * cb + cb / 2, r0z + cz * cb + cb / 2);
                if !in_rect(rect, x, z) {
                    continue;
                }
                let (b, p) = (column(&bare, cx, cz), column(&with, cx, cz));
                let top = ground_top(&b);
                assert_eq!(ground_top(&p), top, "factor {factor}: height of cell column ({cx}, {cz})");
                if water[(cz * cells + cx) as usize].0 == game::NO_WATER {
                    assert_eq!(p[&top], road, "factor {factor}: the top cell of ({cx}, {cz})");
                    shown += 1;
                }
                if z - rect[1] >= strip && rect[3] - 1 - z >= strip && x - rect[0] >= strip && rect[2] - 1 - x >= strip {
                    canopy_bare += b.values().filter(|v| leaves.contains(v)).count();
                    assert!(!p.values().any(|v| leaves.contains(v)), "factor {factor}: canopy over the road at ({cx}, {cz})");
                }
            }
        }
        assert!(shown > 0, "factor {factor}: no dry cell column on the road");
        assert!(canopy_bare > 0, "factor {factor}: no canopy over the road's middle without the paint");
    }
}

#[test]
fn first_paint_wins_and_hash_tracks_paints() {
    let (a, b) = (BlockId(167), BlockId(3));
    let paints = [Paint { rect: [0, 0, 10, 10], block: a }, Paint { rect: [5, 5, 200, 15], block: b }];
    assert_eq!(paint_at(&paints, 6, 6), Some(a), "the first paint wins where both hold the column");
    assert_eq!(paint_at(&paints, 12, 12), Some(b));
    assert_eq!(paint_at(&paints, 10, 4), None, "max is exclusive");
    assert_eq!(paint_at(&paints, -1, 0), None);

    // A paint over several buckets is returned once, in list order.
    let list = StructureList::with_paints(vec![], paints.to_vec(), 9);
    assert_eq!(list.paints_in(0, 0, 300, 300, 0), paints.to_vec());
    assert_eq!(list.paints_in(150, 10, 151, 11, 0), vec![paints[1]]);
    assert_eq!(list.paints_in(150, 20, 151, 21, 4), vec![], "outside the margin");
    assert_eq!(list.paints_in(150, 18, 151, 21, 4), vec![paints[1]], "inside the margin");

    let hut = prefabs::hut(7, 5, 7);
    let p = || vec![placed(&hut, [3, 60, 3], Some(Pad { level: 59, skirt: 4 }))];
    assert_eq!(StructureList::new(p(), 9).content_hash(), StructureList::with_paints(p(), vec![], 9).content_hash());
    let one = StructureList::with_paints(p(), vec![paints[0]], 9).content_hash();
    assert_ne!(StructureList::new(p(), 9).content_hash(), one);
    assert_ne!(StructureList::with_paints(p(), vec![paints[1]], 9).content_hash(), one, "the paint itself is hashed");
    let swapped = StructureList::with_paints(p(), vec![paints[1], paints[0]], 9).content_hash();
    assert_ne!(StructureList::with_paints(p(), paints.to_vec(), 9).content_hash(), swapped, "in order");
    assert_eq!(NoStructures.paints_in(0, 0, 10, 10, 0), vec![]);
}
