//! `ground_y`, the dev structure spec and `dev_placements`. Nothing is installed.

use game::prefab::*;
use game::RegionCoords;
use worldgen::structures::*;

use crate::structure_support::*;

fn entry(prefab: &str, offset: Option<[i32; 2]>, turn: u8) -> DevEntry {
    DevEntry { prefab: prefab.into(), offset, turn }
}

#[test]
fn parse() {
    let ok = |spec: &str| parse_dev_spec(spec).unwrap_or_else(|e| panic!("{spec:?}: {e}"));
    assert_eq!(ok("a"), vec![entry("a", None, 0)]);
    assert_eq!(ok("a:3,-4"), vec![entry("a", Some([3, -4]), 0)]);
    assert_eq!(ok("a:-5,3"), vec![entry("a", Some([-5, 3]), 0)]);
    assert_eq!(ok("3-statuen"), vec![entry("3-statuen", None, 0)]);
    assert_eq!(ok("12"), vec![entry("12", None, 0)]);
    assert_eq!(ok("a:1,2;b"), vec![entry("a", Some([1, 2]), 0), entry("b", None, 0)]);
    assert_eq!(ok("a:1,2:3;b:4,5"), vec![entry("a", Some([1, 2]), 3), entry("b", Some([4, 5]), 0)]);
    assert_eq!(ok(" a : 1 , 2 : 3 ; b "), vec![entry("a", Some([1, 2]), 3), entry("b", None, 0)]);
    assert_eq!(
        ok("dev-hut;statuen-06:40,0:1"),
        vec![entry("dev-hut", None, 0), entry("statuen-06", Some([40, 0]), 1)]
    );
    for bad in ["", "a;", ";a", "a;;b", "a:5", "a:1,2,3", "a:1,99999999999", "a:1,2:4", "a:1,2:1:1", "a b", ":1,2", "a:x,1"] {
        let e = parse_dev_spec(bad).expect_err(bad);
        assert!(!e.is_empty(), "{bad}");
    }
    let e = parse_dev_spec("good;a:1,2:4").unwrap_err();
    assert!(e.contains("a:1,2:4") && e.contains('2'), "{e}");
}

fn set_of(sizes: &[(&str, u32)]) -> PrefabSet {
    let reg = registry();
    let texts: Vec<(String, String)> = sizes
        .iter()
        .map(|&(n, w)| (format!("{n}.prefab.ron"), prefab_to_ron(&prefabs::file(n, [w, 3, 5], [2, 0, 1]))))
        .collect();
    PrefabSet::from_sources(texts.iter().map(|(n, t)| (n.as_str(), t.as_bytes())), &|n| reg.id_of(n)).unwrap()
}

#[test]
fn a_row_beside_the_origin() {
    let set = set_of(&[("narrow", 7), ("wide", 12)]);
    let origin = [100, -50];
    let flat = |_: i32, _: i32| 70;
    let out = dev_placements(&[entry("narrow", None, 0), entry("wide", None, 0)], origin, &set, &flat).unwrap();
    assert_eq!(out.len(), 2);
    let (a, b) = (out[0].bounds(), out[1].bounds());
    assert_eq!(a.0[0], origin[0] + DEV_GAP_BLOCKS);
    assert_eq!(a.1[0] - a.0[0], 7);
    assert_eq!(b.0[0], a.1[0] + DEV_GAP_BLOCKS);
    for p in &out {
        assert_eq!(p.at.anchor[1], 71);
        assert_eq!(p.at.anchor[2], origin[1]);
        assert_eq!(p.pad, Some(Pad { level: 70, skirt: DEV_PAD_SKIRT }));
        assert!(!p.at.mirror);
    }
    // An explicit offset is honoured.
    let out = dev_placements(&[entry("wide", Some([40, 9]), 1)], origin, &set, &flat).unwrap();
    assert_eq!(out[0].at.anchor, [140, 71, -41]);
    assert_eq!(out[0].at.turn, 1);
    // Ground is asked for at the anchor column.
    let out = dev_placements(&[entry("narrow", Some([5, 5]), 0)], origin, &set, &|x, z| x * 1000 + z).unwrap();
    assert_eq!(out[0].pad.unwrap().level, 105 * 1000 - 45);
    assert_eq!(out[0].at.anchor[1], out[0].pad.unwrap().level + 1);
    // An unknown name is an error naming it.
    let e = dev_placements(&[entry("nope", None, 0)], origin, &set, &flat).unwrap_err();
    assert!(e.contains("nope"), "{e}");
}

#[test]
fn the_row_follows_turned_boxes_and_ignores_offsets() {
    let set = set_of(&[("narrow", 7), ("wide", 12)]);
    let origin = [-300, -20];
    let flat = |_: i32, _: i32| 70;
    let entries = [
        entry("narrow", None, 0),
        entry("wide", None, 1), // turned: its box is 5 wide in x
        entry("narrow", Some([50, 8]), 2), // beside the row, not in it
        entry("wide", None, 3),
        entry("narrow", None, 0),
    ];
    let out = dev_placements(&entries, origin, &set, &flat).unwrap();
    assert_eq!(out.len(), 5);
    let b: Vec<_> = out.iter().map(|p| p.bounds()).collect();
    assert_eq!(b[0].0[0], origin[0] + DEV_GAP_BLOCKS);
    assert_eq!(b[1].0[0], b[0].1[0] + DEV_GAP_BLOCKS);
    assert_eq!(b[1].1[0] - b[1].0[0], 5, "a turn swaps the box's x and z extents");
    assert_eq!(out[2].at.anchor, [origin[0] + 50, 71, origin[1] + 8]);
    assert_eq!(b[3].0[0], b[1].1[0] + DEV_GAP_BLOCKS, "an offset entry leaves the row cursor alone");
    assert_eq!(b[4].0[0], b[3].1[0] + DEV_GAP_BLOCKS);
    for i in [0, 1, 3, 4] {
        assert_eq!(out[i].at.anchor[2], origin[1]);
    }
}

/// Compares `ground_y` with the generated top terrain block on every dry, tree-free column of
/// `cols` (world x/z); returns how many were checked.
fn check_ground(r: &world::Regions, cols: &[(i32, i32)]) -> usize {
    let mats = mats();
    let mut checked = 0;
    for &(x, z) in cols {
        if !world::dry(r, x, z) {
            continue;
        }
        let top = world::top_untreed(r, x, z);
        let treed = (top + 1..=top + 40).any(|y| world::stored(r, [x, y, z]).is_some_and(|(b, _)| b.tree_id() != 0));
        if treed {
            continue;
        }
        assert_eq!(worldgen::ground_y(x, z, SEED, mats), top, "column ({x}, {z})");
        checked += 1;
    }
    checked
}

#[test]
fn ground_y_is_the_generated_surface() {
    install_real_block_props();
    let mut checked = 0;
    // A steep spot: the largest surface step between neighbouring columns in the forest region.
    let rc = forest();
    let r = world::generate(&[rc], &NoStructures);
    let (x0, z0) = (rc.x * RB, rc.z * RB);
    let mut best = (0, x0, z0);
    for x in x0..x0 + RB - 17 {
        for z in z0..z0 + RB - 17 {
            let step = (world::ground(&r, x, z) - world::ground(&r, x + 1, z)).abs();
            if step > best.0 {
                best = (step, x, z);
            }
        }
    }
    let block: Vec<(i32, i32)> = (0..16).flat_map(|i| (0..16).map(move |j| (best.1 + i, best.2 + j))).collect();
    checked += check_ground(&r, &block);
    // A region with negative x and z, columns spread over the whole region.
    let neg = RegionCoords::from_world(-2000.0, -2000.0);
    assert!(neg.x < 0 && neg.z < 0);
    let rn = world::generate(&[neg], &NoStructures);
    let spread: Vec<(i32, i32)> =
        (0..RB).step_by(3).flat_map(|i| (0..RB).step_by(5).map(move |j| (neg.x * RB + i, neg.z * RB + j))).collect();
    checked += check_ground(&rn, &spread);
    assert!(checked >= 100, "only {checked} columns checked");
}
