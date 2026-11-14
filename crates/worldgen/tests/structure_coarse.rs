//! The far-terrain twin of the structure pass: a coarse LOD tile shows a placement's levelled ground, no
//! trees over it, and each prefab column's silhouette up to its top — and with no structures in reach the
//! tile (and its water layer) is exactly what it was. Procedural terrain through the explicit `*_with`
//! entries: nothing is installed.

use std::collections::BTreeMap;

use game::lod::{LodCell, LodKey};
use game::{BlockId, RegionCoords, Voxel};
use worldgen::structures::*;

use crate::structure_support::world::Regions;
use crate::structure_support::*;

/// A coarse tile's cells by key-local cell coordinates, with the tile's world block origin.
struct Tile {
    grids: BTreeMap<[i32; 3], Vec<Voxel>>,
    /// World block origin (x, z) and blocks per cell edge.
    ox: i32,
    oz: i32,
    cb: i32,
    cells: i32,
}

impl Tile {
    fn new(key: &LodKey, cells: Vec<LodCell>) -> Self {
        let o = key.origin_region();
        let cb = (key.factor / game::BLOCK_VOXELS as u32) as i32;
        let cells_per_edge = key.region_span() * RB / cb;
        let grids = cells.into_iter().map(|c| (c.group, c.grid)).collect();
        Self { grids, ox: o.x * RB, oz: o.z * RB, cb, cells: cells_per_edge }
    }

    /// The non-air cells of cell column (cx, cz), by cell y.
    fn column(&self, cx: i32, cz: i32) -> BTreeMap<i32, BlockId> {
        let mut out = BTreeMap::new();
        for (g, grid) in &self.grids {
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
    }

    /// The cell column holding world block column (x, z).
    fn cell_of(&self, x: i32, z: i32) -> (i32, i32) {
        ((x - self.ox).div_euclid(self.cb), (z - self.oz).div_euclid(self.cb))
    }

    /// The centre block column of cell column (cx, cz): the point the tile samples it at.
    fn centre(&self, cx: i32, cz: i32) -> (i32, i32) {
        (self.ox + cx * self.cb + self.cb / 2, self.oz + cz * self.cb + self.cb / 2)
    }

    fn contains(&self, x: i32, z: i32) -> bool {
        let (cx, cz) = self.cell_of(x, z);
        (0..self.cells).contains(&cx) && (0..self.cells).contains(&cz)
    }
}

fn leaves() -> Vec<BlockId> {
    mats().trees.species.iter().map(|s| s.leaves).collect()
}

/// The topmost non-leaf cell y of cell column (cx, cz): its terrain (or prefab) top.
fn ground_top(t: &Tile, cx: i32, cz: i32) -> i32 {
    let leaves = leaves();
    *t.column(cx, cz).iter().rev().find(|(_, b)| !leaves.contains(b)).expect("terrain under every column").0
}

fn tile(key: &LodKey, src: &dyn StructureSource) -> Tile {
    Tile::new(key, worldgen::coarse_tile_with(key, SEED, mats(), src))
}

fn brick() -> BlockId {
    registry().id_of("stone_bricks").unwrap()
}

/// A 9 × 40 × 9 stone-brick tower on a pad (skirt 4) on dry ground of the forest region, inside it by
/// `margin` blocks past the skirt; the bare region it was chosen on.
fn tower(margin: i32) -> (StructurePlacement, Regions) {
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (w, skirt) = (9, 4);
    let m = skirt + margin;
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = world::dry_site(&bare, [r0x + m, r0z + m, r0x + RB - m, r0z + RB - m], w, w, skirt).expect("a dry site");
    let level = world::ground(&bare, ax + w / 2, az + w / 2);
    let tower = prefabs::solid("tower", [w as u32, 40, w as u32], [0, 0, 0], prefabs::BRICK);
    (placed(&tower, [ax, level + 1, az], Some(Pad { level, skirt: skirt as u32 })), bare)
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_source_is_identical() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let keys = [16, 64].into_iter().flat_map(|f| [forest(), RegionCoords::new(-3, -2)].map(|rc| LodKey::for_region(f, rc)));
    for key in keys {
        let (ox, oz) = (key.origin_region().x * RB, key.origin_region().z * RB);
        let far = StructureList::new(
            vec![placed(&prefabs::hut(7, 5, 7), [ox - 400, 60, oz - 400], Some(Pad { level: 60, skirt: 4 }))],
            1,
        );
        let base = worldgen::coarse_tile(&key, SEED, mats());
        for src in [&NoStructures as &dyn StructureSource, &far] {
            let got = worldgen::coarse_tile_with(&key, SEED, mats(), src);
            assert_eq!(got.len(), base.len(), "{key:?}");
            for (a, b) in got.iter().zip(&base) {
                assert_eq!(a.group, b.group, "{key:?}");
                assert!(a.grid == b.grid, "{key:?} group {:?}", a.group);
            }
            let water = worldgen::coarse_water_with(&key, SEED, mats(), src);
            assert_eq!(water, worldgen::coarse_water(&key, SEED, mats()), "water {key:?}");
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_tower_shows_at_factor_16() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let (sp, bare) = tower(1);
    let key = LodKey::for_region(16, forest());
    let t = tile(&key, &StructureList::new(vec![sp.clone()], 1));
    assert_eq!(t.cb, 1);
    let level = sp.pad.unwrap().level;
    let skirt = sp.pad.unwrap().skirt as i32;
    let (min, max) = sp.bounds();
    let top = max[1] - 1;
    let (mut inside, mut ring) = (0, 0);
    for x in min[0] - skirt - 1..max[0] + skirt + 1 {
        for z in min[2] - skirt - 1..max[2] + skirt + 1 {
            assert!(t.contains(x, z));
            let (cx, cz) = t.cell_of(x, z);
            let col = t.column(cx, cz);
            let d = box_distance(&sp, x, z);
            if d == 0 {
                inside += 1;
                assert!(col.get(&level).is_some_and(|&b| b != brick()), "terrain at the pad level ({x}, {z})");
                for y in level + 1..=top {
                    assert_eq!(col.get(&y), Some(&brick()), "tower cell ({x}, {y}, {z})");
                }
                assert_eq!(col.range(top + 1..).next(), None, "above the tower at ({x}, {z})");
                continue;
            }
            assert!(!col.values().any(|&b| b == brick()), "a tower cell outside the box at ({x}, {z})");
            if d == 1 {
                ring += 1;
            }
            if d <= skirt {
                let natural = world::ground(&bare, x, z);
                let PadEffect::Level { surface, .. } = pad_at(&[&sp], x, z, natural) else { panic!("in the skirt") };
                assert_eq!(ground_top(&t, cx, cz), surface, "pad surface at ({x}, {z})");
            }
        }
    }
    assert_eq!(inside, 81);
    assert!(ring > 0);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_tower_shows_at_factor_64() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let (sp, _) = tower(4);
    let key = LodKey::for_region(64, forest());
    let t = tile(&key, &StructureList::new(vec![sp.clone()], 1));
    assert_eq!(t.cb, 4);
    let level = sp.pad.unwrap().level;
    let (min, max) = sp.bounds();
    let (lo_cell, top_cell) = (level.div_euclid(4) + 1, (max[1] - 1).div_euclid(4));
    let ((cx0, cz0), (cx1, cz1)) = (t.cell_of(min[0] - 8, min[2] - 8), t.cell_of(max[0] + 8, max[2] + 8));
    let (mut inside, mut outside) = (0, 0);
    for cx in cx0..=cx1 {
        for cz in cz0..=cz1 {
            let (x, z) = t.centre(cx, cz);
            assert!(t.contains(x, z));
            let col = t.column(cx, cz);
            if box_distance(&sp, x, z) == 0 {
                inside += 1;
                assert!(col.get(&(lo_cell - 1)).is_some_and(|&b| b != brick()), "terrain cell at ({cx}, {cz})");
                for cy in lo_cell..=top_cell {
                    assert_eq!(col.get(&cy), Some(&brick()), "tower cell ({cx}, {cy}, {cz})");
                }
                assert_eq!(col.range(top_cell + 1..).next(), None, "above the tower at ({cx}, {cz})");
            } else {
                outside += 1;
                assert!(!col.values().any(|&b| b == brick()), "a tower cell in column ({cx}, {cz})");
            }
        }
    }
    assert!(inside > 0 && outside > 0, "inside {inside}, outside {outside}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_coarse_tile_tracks_full_generation() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (w, h, d, skirt) = (7, 5, 7, 4);
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = world::dry_site(&bare, [r0x + skirt, r0z + skirt, r0x + RB - skirt, r0z + RB - skirt], w, d, skirt)
        .expect("a dry site");
    let level = world::ground(&bare, ax + w / 2, az + d / 2);
    let sp = placed(&prefabs::hut(w as u32, h as u32, d as u32), [ax, level + 1, az], Some(Pad { level, skirt: skirt as u32 }));
    let list = StructureList::new(vec![sp.clone()], 1);
    let full = world::generate(&[rc], &list);
    let key = LodKey::for_region(16, rc);
    let t = tile(&key, &list);
    let (min, max) = sp.bounds();
    let (mut hits, mut total) = (0, 0);
    for x in min[0] - 24..max[0] + 24 {
        for z in min[2] - 24..max[2] + 24 {
            if !t.contains(x, z) {
                continue;
            }
            let (cx, cz) = t.cell_of(x, z);
            let coarse = ground_top(&t, cx, cz);
            let exact = world::top_untreed(&full, x, z);
            if box_distance(&sp, x, z) == 0 {
                assert_eq!(coarse, exact, "top in the box at ({x}, {z})");
                assert_eq!(coarse, max[1] - 1, "the roof at ({x}, {z})");
            }
            total += 1;
            hits += usize::from((coarse - exact).abs() <= 1);
        }
    }
    assert!(total > 100, "{total} columns compared");
    assert!(hits * 100 >= total * 95, "coarse top within ±1 of full generation for >=95% ({hits}/{total})");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trees_are_suppressed_the_same_way() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let key = LodKey::for_region(16, rc);
    let bare = tile(&key, &NoStructures);
    let leaves = leaves();
    let leafy = |t: &Tile, x: i32, z: i32| {
        let (cx, cz) = t.cell_of(x, z);
        t.column(cx, cz).values().filter(|b| leaves.contains(b)).count()
    };
    // A 7 × 7 box under the bare tile's canopy, inside the region by the keep-out.
    let (w, skirt) = (7, 4u32);
    let m = TREE_REACH + 1;
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = (r0x + m..r0x + RB - m - w)
        .flat_map(|x| (r0z + m..r0z + RB - m - w).map(move |z| (x, z)))
        .find(|&(x, z)| leafy(&bare, x + w / 2, z + w / 2) > 0)
        .expect("canopy in the forest");
    let (cx, cz) = bare.cell_of(ax + w / 2, az + w / 2);
    let level = ground_top(&bare, cx, cz);
    let sp = placed(&prefabs::hut(w as u32, 5, w as u32), [ax, level + 1, az], Some(Pad { level, skirt }));
    let t = tile(&key, &StructureList::new(vec![sp.clone()], 1));
    // A tree stands beyond the keep-out (`TREE_REACH`) and its coarse crown reaches at most its crown
    // radius from its trunk: no canopy over the box or within the leaf reach of it.
    let (min, max) = sp.bounds();
    let (mut near_bare, mut near_with) = (0, 0);
    for x in min[0] - TREE_REACH..max[0] + TREE_REACH {
        for z in min[2] - TREE_REACH..max[2] + TREE_REACH {
            let dist = box_distance(&sp, x, z);
            let n = leafy(&t, x, z);
            if dist <= worldgen::trees::LEAF_REACH_PAST_CROWN_BLOCKS {
                assert_eq!(n, 0, "canopy at ({x}, {z}), {dist} from the box");
            }
            near_with += n;
            near_bare += leafy(&bare, x, z);
        }
    }
    assert!(near_with < near_bare, "the structure removed no canopy ({near_with} vs {near_bare})");
}
