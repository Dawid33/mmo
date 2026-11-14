//! The structure pass of generation: a placement's ground is levelled, trees keep away, the stored band
//! covers it, and its blocks land after terrain and trees — the same however the region is split into
//! windows, and with no structures exactly the terrain there was before. Procedural terrain through the
//! explicit `*_with` entries: nothing is installed.

use std::collections::{BTreeMap, BTreeSet};

use game::prefab::*;
use game::{Block, BlockId, RegionCoords, REGION_CHUNKS};
use worldgen::structures::*;

use crate::structure_support::world::Regions;
use crate::structure_support::*;

/// Every cell of `expected` reads back as its write (a carve as plain air).
fn assert_cells(r: &Regions, sp: &StructurePlacement) {
    let want = prefabs::expected(&sp.prefab, &sp.at);
    assert!(!want.is_empty());
    for (p, w) in want {
        let w = w.unwrap_or((air(), None));
        assert_eq!(world::read(r, p), w, "prefab cell {p:?}");
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_source_is_byte_identical() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let half = REGION_CHUNKS / 2;
    for rc in [forest(), RegionCoords::new(0, 0), RegionCoords::new(-3, -2)] {
        let a = worldgen::generate_region_with(rc, SEED, mats(), &NoStructures);
        assert!(bytes(&a) == bytes(&worldgen::generate_region(rc, SEED, mats())), "region {rc:?}");
        let far = StructureList::new(
            vec![placed(&prefabs::hut(7, 5, 7), [rc.x * RB - 400, 60, rc.z * RB - 400], Some(Pad { level: 60, skirt: 4 }))],
            1,
        );
        assert!(bytes(&a) == bytes(&worldgen::generate_region_with(rc, SEED, mats(), &far)), "far list {rc:?}");
        let w = worldgen::generate_columns_with(rc, 0..half, 0..REGION_CHUNKS, SEED, mats(), &NoStructures);
        assert!(bytes(&w) == bytes(&worldgen::generate_columns(rc, 0..half, 0..REGION_CHUNKS, SEED, mats())), "window {rc:?}");
    }
}

/// crc32 of the bincode of generation with NO structure source: these pin the "no structures
/// installed" output (full columns and coarse LOD tiles) against the terrain before the structure pass
/// existed, which `no_source_is_byte_identical` cannot (both its sides run the new code). Recorded at
/// the plan's base commit f0df695e (before any structure code in `lib.rs`) and equal at the structure
/// pass, at the default dials (region 64, chunk 16, no world file). Re-record only together with a deliberate
/// terrain change, and say why in that commit:
///
/// ```sh
/// STRUCTURE_GOLDEN_PRINT=1 just test-one worldgen "--test it -- --include-ignored --nocapture structure_pass::no_structure_output_is_pinned"
/// ```
const PINNED_REGIONS: [u32; 3] = [0x3B11_2A9C, 0x28C8_0944, 0xAA0F_DEE1];
/// `generate_columns` over the forest region's west half.
const PINNED_HALF_WINDOW: u32 = 0x6691_8C16;
/// `coarse_tile` (cells: group, grid) and `coarse_water` of the forest region's key at factor 16, then 64.
const PINNED_COARSE: [(u32, u32); 2] = [(0x804E_A0A4, 0xAB24_6ECB), (0x1B16_E5E7, 0x3661_EBE0)];

/// The dials the pinned constants hold for (`scene_hash_golden`'s guard, chunk 16 only).
fn golden_dials() -> bool {
    game::CHUNK_BLOCKS == 16
        && game::REGION_BLOCKS == 64
        && option_env!("VOXEL_ISLAND_CELLS").is_none_or(|v| v.is_empty() || v == "0" || v == "16")
        && std::env::var_os(worldgen::forged::WORLD_FILE_ENV).is_none_or(|v| v.is_empty())
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_structure_output_is_pinned() {
    if !golden_dials() {
        eprintln!("structure_pass::no_structure_output_is_pinned: non-default world dials, skipped");
        return;
    }
    install_real_block_props();
    let crc = |v: &[u8]| crc32fast::hash(v);
    let regions = [forest(), RegionCoords::new(0, 0), RegionCoords::new(-3, -2)]
        .map(|rc| crc(&bytes(&worldgen::generate_region(rc, SEED, mats()))));
    let half = crc(&bytes(&worldgen::generate_columns(forest(), 0..REGION_CHUNKS / 2, 0..REGION_CHUNKS, SEED, mats())));
    let coarse = [16, 64].map(|f| {
        let key = game::lod::LodKey::for_region(f, forest());
        let cells: Vec<_> = worldgen::coarse_tile(&key, SEED, mats()).into_iter().map(|c| (c.group, c.grid)).collect();
        (crc(&bytes(&cells)), crc(&bytes(&worldgen::coarse_water(&key, SEED, mats()))))
    });
    if std::env::var_os("STRUCTURE_GOLDEN_PRINT").is_some() {
        let hex = |v: u32| format!("0x{:04X}_{:04X}", v >> 16, v & 0xFFFF);
        println!("PINNED_REGIONS = [{}]", regions.map(hex).join(", "));
        println!("PINNED_HALF_WINDOW = {}", hex(half));
        println!("PINNED_COARSE = [{}]", coarse.map(|(t, w)| format!("({}, {})", hex(t), hex(w))).join(", "));
    }
    assert_eq!(regions, PINNED_REGIONS, "generate_region without structures moved");
    assert_eq!(half, PINNED_HALF_WINDOW, "generate_columns without structures moved");
    assert_eq!(coarse, PINNED_COARSE, "coarse_tile / coarse_water without structures moved");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_hut_stands_on_its_pad() {
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
    let r = world::generate(&[rc], &list);
    assert_cells(&r, &sp);
    let (min, max) = sp.bounds();
    let cells = prefabs::expected(&sp.prefab, &sp.at);
    for x in min[0] - skirt..max[0] + skirt {
        for z in min[2] - skirt..max[2] + skirt {
            let natural = world::ground(&bare, x, z);
            let surface = match pad_at(&[&sp], x, z, natural) {
                PadEffect::Level { surface, .. } => surface,
                PadEffect::None => panic!("({x}, {z}) is in the skirt"),
            };
            assert_eq!(world::ground(&r, x, z), surface, "pad surface at ({x}, {z})");
            assert_ne!(world::read(&r, [x, surface, z]).0.id, BlockId::AIR, "top terrain block at ({x}, {z})");
            if box_distance(&sp, x, z) > 0 {
                continue;
            }
            assert_eq!(surface, level);
            for y in surface + 1..max[1] + 8 {
                if !cells.contains_key(&[x, y, z]) {
                    assert_eq!(world::read(&r, [x, y, z]), (air(), None), "above the ground at ({x}, {y}, {z})");
                }
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn window_independence_with_a_structure() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let half = REGION_CHUNKS / 2;
    let (mx, mz) = (rc.x * RB + half as i32 * CB, rc.z * RB + half as i32 * CB);
    let bare = world::generate(&[rc], &NoStructures);
    let level = world::ground(&bare, mx, mz);
    // A hut straddling the middle on both axes, its box and skirt crossing the window cuts.
    let sp = placed(&prefabs::hut(9, 6, 9), [mx - 4, level + 1, mz - 4], Some(Pad { level, skirt: 6 }));
    world::assert_dry(&bare, &sp);
    let list = StructureList::new(vec![sp.clone()], 1);
    let whole = worldgen::generate_region_with(rc, SEED, mats(), &list);
    let by_col: BTreeMap<_, _> = whole.iter().map(|(cc, col)| ((cc.x, cc.z), bytes(col))).collect();
    let (min, _) = sp.bounds();
    let corner = (((min[0] - rc.x * RB) / CB) as usize, ((min[2] - rc.z * RB) / CB) as usize);
    let all = 0..REGION_CHUNKS;
    let windows = [
        (0..half, all.clone()),
        (half..REGION_CHUNKS, all.clone()),
        (all.clone(), 0..half),
        (corner.0..corner.0 + 1, corner.1..corner.1 + 1),
    ];
    for (xs, zs) in windows {
        let part = worldgen::generate_columns_with(rc, xs.clone(), zs.clone(), SEED, mats(), &list);
        assert_eq!(part.len(), xs.len() * zs.len());
        for (cc, col) in &part {
            assert!(by_col[&(cc.x, cc.z)] == bytes(col), "column {cc:?} of window {xs:?} x {zs:?}");
        }
    }
    // The structure is really there.
    let r: Regions = [((rc.x, rc.z), whole)].into_iter().collect();
    assert_cells(&r, &sp);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_structure_crosses_a_region_corner_whole() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let c = forest();
    let rcs: Vec<RegionCoords> =
        [(-1, -1), (0, -1), (-1, 0), (0, 0)].map(|(dx, dz)| RegionCoords::new(c.x + dx, c.z + dz)).to_vec();
    let (cx, cz) = (c.x * RB, c.z * RB);
    let bare = world::generate(&rcs, &NoStructures);
    let level = world::ground(&bare, cx, cz);
    let sp = placed(&prefabs::hut(20, 6, 20), [cx - 10, level + 1, cz - 10], Some(Pad { level, skirt: 4 }));
    world::assert_dry(&bare, &sp);
    let r = world::generate(&rcs, &StructureList::new(vec![sp.clone()], 1));
    let (min, max) = sp.bounds();
    let mut regions = BTreeSet::new();
    for x in min[0]..max[0] {
        for z in min[2]..max[2] {
            regions.insert((x.div_euclid(RB), z.div_euclid(RB)));
        }
    }
    assert_eq!(regions.len(), 4, "the box spans four regions");
    assert_cells(&r, &sp);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn trees_stay_out() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    // The forest region and its east neighbour; the hut near their shared edge's west side, so trees far
    // from it exist on both sides.
    let rcs = [rc, RegionCoords::new(rc.x + 1, rc.z)];
    let bare = world::generate(&rcs, &NoStructures);
    let (ax, az) = (rc.x * RB + RB / 2, rc.z * RB + RB / 2 - 3);
    let level = world::ground(&bare, ax + 3, az + 3);
    let sp = placed(&prefabs::hut(7, 5, 7), [ax, level + 1, az], Some(Pad { level, skirt: 4 }));
    let r = world::generate(&rcs, &StructureList::new(vec![sp.clone()], 1));
    let with = world::tree_blocks(&r);
    let without = world::tree_blocks(&bare);
    // Nothing of a tree in the box, at or above the pad.
    for (p, b) in &with {
        assert!(box_distance(&sp, p[0], p[2]) > 0 || p[1] < level, "tree block {b:?} at {p:?} in the box");
    }
    // No tree stands within the keep-out: a tree's lowest block (its trunk base) is beyond it.
    let mut base: BTreeMap<u32, [i32; 3]> = BTreeMap::new();
    for (p, b) in &with {
        let e = base.entry(b.tree_id()).or_insert(*p);
        if p[1] < e[1] {
            *e = *p;
        }
    }
    for (id, p) in &base {
        assert!(box_distance(&sp, p[0], p[2]) > TREE_REACH, "tree {id} stands at {p:?}");
    }
    // Far from the structure the trees are untouched.
    let bound = TREE_REACH + 2 * worldgen::trees::TREE_LATTICE_BLOCKS + MAX_PAD_SKIRT as i32;
    let far = |m: &BTreeMap<[i32; 3], Block>| -> BTreeMap<[i32; 3], Block> {
        m.iter().filter(|(p, _)| box_distance(&sp, p[0], p[2]) > bound).map(|(p, b)| (*p, *b)).collect()
    };
    let (a, b) = (far(&with), far(&without));
    assert!(!b.is_empty(), "no trees far from the structure to compare");
    assert!(a == b, "trees far from the structure changed");
    assert!(without.len() > with.len(), "the structure removed no tree");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cellar_has_real_walls() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    // The box and its carve start on a chunk column edge in x and z, so the walls there lie in the
    // neighbouring chunk columns, which the box itself does not reach.
    let (ex, ez) = (rc.x * RB + 2 * CB, rc.z * RB + 2 * CB);
    // The lowest natural ground of the chunk columns around that corner: a pad at or under it leaves
    // their bands' own bottoms at `level - MAX_SOIL_DEPTH` or above.
    let low = (ex - CB..ex + CB).flat_map(|x| (ez - CB..ez + CB).map(move |z| (x, z)));
    let low = low.map(|(x, z)| world::ground(&bare, x, z)).min().unwrap();
    // Two pad heights against the chunk grid: the floor's row (level - 5) just under a chunk edge the
    // neighbours' natural bands stop at, and the floor on a chunk edge with the row under it in the
    // chunk below.
    for phase in [3, 5] {
        let level = low - (low - phase).rem_euclid(CB);
        // 6 × 9 × 6, anchor cell (0, 6, 0): a cellar carved in cells y 0..=6, x/z 0..=3; plank walls
        // above on the far sides.
        let mut f = prefabs::file("cellar", [6, 9, 6], [0, 6, 0]);
        f.carve = vec![FileCarve { min: [0, 0, 0], size: [4, 7, 4] }];
        f.boxes = vec![
            FileBox { min: [0, 7, 5], size: [6, 2, 1], mat: prefabs::PLANK },
            FileBox { min: [5, 7, 0], size: [1, 2, 5], mat: prefabs::PLANK },
        ];
        let sp = placed(&prefabs::resolve_file(f), [ex, level + 1, ez], Some(Pad { level, skirt: 4 }));
        world::assert_dry(&bare, &sp);
        let r = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
        assert_cells(&r, &sp);
        let cells = prefabs::expected(&sp.prefab, &sp.at);
        let carved: BTreeSet<[i32; 3]> = cells.iter().filter(|(_, w)| w.is_none()).map(|(p, _)| *p).collect();
        let (min, max) = sp.bounds();
        assert_eq!(min[1], level - 5, "the cellar floor is 6 below the anchor");
        let mut checked = 0;
        for p in &carved {
            for d in [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]] {
                let n = [p[0] + d[0], p[1] + d[1], p[2] + d[2]];
                if carved.contains(&n) || cells.contains_key(&n) || n[1] > level {
                    continue;
                }
                let got =
                    world::stored(&r, n).unwrap_or_else(|| panic!("wall cell {n:?} beside {p:?} is implied ground"));
                assert_ne!(got.0.id, BlockId::AIR, "wall cell {n:?} beside {p:?}");
                checked += 1;
            }
        }
        assert!(checked > 0);
        // Every chunk column within one block of the box stores down to below the cellar floor.
        for x in min[0] - 1..=max[0] {
            for z in min[2] - 1..=max[2] {
                let (col, _, _) = world::column(&r, x, z);
                assert!(col.base_sub_y() * CB < min[1], "column at ({x}, {z}): band base {}", col.base_sub_y() * CB);
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn later_placements_override() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    // Both boxes (9 × 8 together) and their skirts on dry ground.
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    let (ax, az) = world::dry_site(&bare, [r0x + 3, r0z + 3, r0x + RB - 3, r0z + RB - 3], 9, 8, 3).expect("a dry site");
    let level = world::ground(&bare, ax, az);
    let pad = Some(Pad { level, skirt: 3 });
    let a = placed(&prefabs::solid("a", [6, 3, 6], [0, 0, 0], prefabs::BRICK), [ax, level + 1, az], pad);
    let b = placed(&prefabs::solid("b", [6, 3, 6], [0, 0, 0], prefabs::PLANK), [ax + 3, level + 2, az + 2], pad);
    let r = world::generate(&[rc], &StructureList::new(vec![a.clone(), b.clone()], 1));
    let (ea, eb) = (prefabs::expected(&a.prefab, &a.at), prefabs::expected(&b.prefab, &b.at));
    let mut overlap = 0;
    for (p, w) in &ea {
        let want = eb.get(p).unwrap_or(w).clone().unwrap();
        overlap += usize::from(eb.contains_key(p));
        assert_eq!(world::read(&r, *p), want, "cell {p:?}");
    }
    assert!(overlap > 0);
    assert_cells(&r, &b);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn nothing_below_bedrock() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (ax, az) = (rc.x * RB + RB / 2, rc.z * RB + RB / 2);
    let level = world::ground(&bare, ax, az);
    // 5 × 85 × 5 of planks, anchored at its top cell: 84 blocks reach below the anchor.
    let sp = placed(&prefabs::solid("deep", [5, 85, 5], [0, 84, 0], prefabs::PLANK), [ax, level, az], Some(Pad { level, skirt: 2 }));
    let r = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
    let plank = registry().id_of("oak_planks").unwrap();
    let bedrock = mats().bedrock;
    let (min, max) = sp.bounds();
    assert!(min[1] < level - game::UNDERGROUND_DEPTH - 10);
    for x in min[0]..max[0] {
        for z in min[2]..max[2] {
            assert_eq!(world::ground(&r, x, z), level);
            for y in min[1]..max[1] {
                let got = world::read(&r, [x, y, z]);
                if y <= level - game::UNDERGROUND_DEPTH {
                    assert_eq!(got.0.id, bedrock, "({x}, {y}, {z}) at or below bedrock");
                } else {
                    // A plain block: a slope's chisel on the ground's top block went with it.
                    assert_eq!(got, (Block::new(plank), None), "({x}, {y}, {z}) above bedrock");
                }
            }
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn no_slope_under_a_box_on_a_hillside() {
    if !default_dials() {
        return;
    }
    install_real_block_props();
    let rc = forest();
    let bare = world::generate(&[rc], &NoStructures);
    let (w, d, skirt) = (7, 7, 4);
    let (r0x, r0z) = (rc.x * RB, rc.z * RB);
    // The steepest dry site: where the first skirt ring lies furthest under the level (the ground at the
    // box's middle) — a skirt column at least `skirt + 1` under it is lower than the box's edge.
    let mut best: Option<(i32, i32, (i32, i32))> = None;
    for ax in r0x + skirt..r0x + RB - skirt - w {
        for az in r0z + skirt..r0z + RB - skirt - d {
            let reach = (ax - skirt..ax + w + skirt).flat_map(|x| (az - skirt..az + d + skirt).map(move |z| (x, z)));
            if !reach.into_iter().all(|(x, z)| world::dry(&bare, x, z)) {
                continue;
            }
            let level = world::ground(&bare, ax + w / 2, az + d / 2);
            let ring = (ax - 1..=ax + w).flat_map(|x| (az - 1..=az + d).map(move |z| (x, z)));
            let drop = ring.map(|(x, z)| level - world::ground(&bare, x, z)).max().unwrap();
            let heights = (ax..ax + w).flat_map(|x| (az..az + d).map(move |z| (x, z))).map(|(x, z)| world::ground(&bare, x, z));
            let (lo, hi) = heights.fold((i32::MAX, i32::MIN), |(lo, hi), g| (lo.min(g), hi.max(g)));
            if best.is_none_or(|b| drop > b.0) {
                best = Some((drop, hi - lo, (ax, az)));
            }
        }
    }
    let (drop, fall, (ax, az)) = best.expect("a dry site");
    assert!(fall >= 3, "the steepest dry site falls only {fall} blocks across the box");
    assert!(drop > skirt, "no skirt column lies lower than the box's edge (drop {drop})");
    let level = world::ground(&bare, ax + w / 2, az + d / 2);
    let sp = placed(&prefabs::hut(w as u32, 5, d as u32), [ax, level + 1, az], Some(Pad { level, skirt: skirt as u32 }));
    let r = world::generate(&[rc], &StructureList::new(vec![sp.clone()], 1));
    assert_cells(&r, &sp);
    for x in ax..ax + w {
        for z in az..az + d {
            let (block, chisel) = world::read(&r, [x, level, z]);
            assert_ne!(block.id, BlockId::AIR, "the pad's top block at ({x}, {z})");
            assert_eq!(chisel, None, "a chisel on the pad's top block at ({x}, {level}, {z})");
        }
    }
}
