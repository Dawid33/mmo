//! The path `--prefab` takes, end to end, over a forged world: a lazy `DevRegionSource` whose ground is
//! `ground_y_forged`, a row of prefabs crossing a region seam, full generation and the coarse twin. The
//! small synthetic forged world of `common` is built in memory and passed explicitly: nothing is
//! installed.

use std::sync::Arc;

use game::lod::LodKey;
use game::prefab::*;
use game::{BlockId, RegionCoords};
use worldgen::forged::ForgedWorld;
use worldgen::structures::*;

use crate::common::{engine, synthetic, CELL_M};
use crate::structure_support::world::{self, Regions};
use crate::structure_support::{air, install_real_block_props, mats, prefabs, registry, RB};

/// A one-prefab set: a solid stone-brick box `w × 3 × 5` named `box<w>`.
fn box_set(w: u32) -> (PrefabSet, String) {
    let reg = registry();
    let name = format!("box{w}");
    let mut f = prefabs::file(&name, [w, 3, 5], [0, 0, 0]);
    f.boxes = vec![FileBox { min: [0, 0, 0], size: [w, 3, 5], mat: prefabs::BRICK }];
    let text = prefab_to_ron(&f);
    let file = format!("{name}.prefab.ron");
    let set = PrefabSet::from_sources([(file.as_str(), text.as_bytes())].into_iter(), &|n| reg.id_of(n)).unwrap();
    (set, name)
}

/// The topmost non-air cell (y, block) of coarse factor-16 tile cells at world column (x, z) of `rc`.
fn coarse_top(cells: &[game::lod::LodCell], rc: RegionCoords, x: i32, z: i32) -> Option<(i32, BlockId)> {
    let (cx, cz) = (x - rc.x * RB, z - rc.z * RB);
    let mut top: Option<(i32, BlockId)> = None;
    for cell in cells {
        if cell.group[0] != cx.div_euclid(32) || cell.group[2] != cz.div_euclid(32) {
            continue;
        }
        for ly in 0..32usize {
            let v = cell.grid[cx.rem_euclid(32) as usize + 32 * ly + 32 * 32 * cz.rem_euclid(32) as usize];
            let y = cell.group[1] * 32 + ly as i32;
            if !v.is_air() && top.is_none_or(|t| y > t.0) {
                top = Some((y, v.block));
            }
        }
    }
    top
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_dev_row_over_a_forged_world() {
    install_real_block_props();
    let world = Arc::new(ForgedWorld::new(synthetic()));
    // Wide enough that the row, laid out beside a region's centre, crosses into the next region east.
    let w = (RB / 2 + 8) as u32;
    let (set, name) = box_set(w);
    let gw = world.clone();
    let src = DevRegionSource::new(
        vec![DevEntry { prefab: name, offset: None, turn: 0 }],
        &set,
        Box::new(|rc: RegionCoords| [rc.x * RB + RB / 2, rc.z * RB + RB / 2]),
        Box::new(move |x, z| worldgen::ground_y_forged(&gw, x, z, mats())),
    )
    .unwrap();
    // The plain: east of the sea, north of the river, clear of the lake.
    let (x, z) = engine(40.0 * CELL_M, 8.0 * CELL_M);
    let c = RegionCoords::new((x / RB as f64).floor() as i32, (z / RB as f64).floor() as i32);
    let rcs = [c, RegionCoords::new(c.x + 1, c.z)];
    let generate = |s: &dyn StructureSource| -> Regions {
        rcs.iter().map(|&rc| ((rc.x, rc.z), worldgen::generate_region_forged_with(&world, rc, mats(), s))).collect()
    };
    let bare = generate(&NoStructures);
    let r = generate(&src);
    let in_view = |x: i32, z: i32| x >= c.x * RB && x < (c.x + 2) * RB && z >= c.z * RB && z < (c.z + 1) * RB;

    // ground_y_forged is the generated surface on the dry, tree-free columns of a 16 × 16 block.
    let mut checked = 0;
    for x in c.x * RB + 3..c.x * RB + 19 {
        for z in c.z * RB + 3..c.z * RB + 19 {
            if !world::dry(&bare, x, z) {
                continue;
            }
            let top = world::top_untreed(&bare, x, z);
            if (top + 1..=top + 40).any(|y| world::stored(&bare, [x, y, z]).is_some_and(|(b, _)| b.tree_id() != 0)) {
                continue;
            }
            assert_eq!(worldgen::ground_y_forged(&world, x, z, mats()), top, "ground_y_forged at ({x}, {z})");
            checked += 1;
        }
    }
    assert!(checked >= 128, "only {checked} of 256 columns were dry and tree-free");

    // Every placement reaching the two regions: one per region of the row, laid out on its anchor's
    // ground; every cell in view reads back whole, from both regions where the row crosses the seam.
    let all = src.placements_in(c.x * RB, c.z * RB, (c.x + 2) * RB, (c.z + 1) * RB, 0);
    assert!(all.len() >= 2, "{} placements reach the two regions", all.len());
    let seam = (c.x + 1) * RB;
    let (mut crossing, mut cells) = (0, 0);
    for sp in &all {
        let (min, max) = sp.bounds();
        let [ax, ay, az] = sp.at.anchor;
        let level = sp.pad.expect("a dev placement has a pad").level;
        assert_eq!(ay, level + 1);
        if in_view(ax, az) {
            assert_eq!(level, world::ground(&bare, ax, az), "the pad level is the anchor's generated ground");
        }
        if min[0] < seam && max[0] > seam {
            crossing += 1;
        }
        for (p, want) in prefabs::expected(&sp.prefab, &sp.at) {
            if in_view(p[0], p[2]) {
                assert_eq!(world::read(&r, p), want.unwrap_or((air(), None)), "prefab cell {p:?}");
                cells += 1;
            }
        }
        for x in min[0]..max[0] {
            for z in min[2]..max[2] {
                if in_view(x, z) {
                    assert!(world::dry(&r, x, z), "the box stands in water at ({x}, {z})");
                    assert_eq!(world::ground(&r, x, z), level, "pad at ({x}, {z})");
                }
            }
        }
    }
    assert!(crossing > 0, "a row crosses the seam between {:?} and {:?}", rcs[0], rcs[1]);
    assert!(cells > 0);

    // The coarse twin at factor 16 agrees with full generation on every column of the boxes.
    let brick = registry().id_of("stone_bricks").unwrap();
    let mut agreed = 0;
    for rc in rcs {
        let tile = worldgen::coarse_tile_forged_with(&world, &LodKey::for_region(16, rc), mats(), &src);
        for sp in &all {
            let (min, max) = sp.bounds();
            for x in min[0]..max[0] {
                for z in min[2]..max[2] {
                    if x.div_euclid(RB) != rc.x || z.div_euclid(RB) != rc.z {
                        continue;
                    }
                    let full = world::top_untreed(&r, x, z);
                    assert_eq!(full, max[1] - 1, "the box's top in full generation at ({x}, {z})");
                    assert_eq!(coarse_top(&tile, rc, x, z), Some((full, brick)), "coarse top at ({x}, {z}) of {rc:?}");
                    agreed += 1;
                }
            }
        }
    }
    assert!(agreed > 0);
}
