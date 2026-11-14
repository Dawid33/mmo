//! `ground_y`, the dev structure spec and `dev_placements`. Nothing is installed.

use game::prefab::*;
use worldgen::structures::*;

use crate::structure_support::*;

fn entry(prefab: &str, offset: Option<[i32; 2]>, turn: u8) -> DevEntry {
    DevEntry { prefab: prefab.into(), offset, turn }
}

#[test]
fn parse() {
    assert_eq!(parse_dev_spec("a").unwrap(), vec![entry("a", None, 0)]);
    assert_eq!(parse_dev_spec("a:3,-4").unwrap(), vec![entry("a", Some([3, -4]), 0)]);
    assert_eq!(
        parse_dev_spec("dev-hut,statuen-06:40,0:1").unwrap(),
        vec![entry("dev-hut", None, 0), entry("statuen-06", Some([40, 0]), 1)]
    );
    for bad in ["", "a,,b", "a,", ",a", "a:x,1", "a:1", "a:1,2,3", "a:1,2:4", "a:1,2:x", ":1,2", "a:1,2:1:1"] {
        let e = parse_dev_spec(bad).expect_err(bad);
        assert!(!e.is_empty(), "{bad}");
    }
    assert!(parse_dev_spec("a:1,2:4").unwrap_err().contains("a:1,2:4"));
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
fn ground_y_is_the_generated_surface() {
    install_real_block_props();
    let (rc, mats) = (forest(), mats());
    let r = world::generate(&[rc], &NoStructures);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |m: i32| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) % m as u64) as i32
    };
    let mut checked = 0;
    for _ in 0..20 {
        let (x, z) = (rc.x * RB + next(RB), rc.z * RB + next(RB));
        let gy = worldgen::ground_y(x, z, SEED, mats);
        // Skip columns where something stands above the ground: a tree, water, or ice.
        let (col, lx, lz) = world::column(&r, x, z);
        let wet = col.ground().unwrap().recipe_water(lx, lz).is_some();
        let top = world::top_untreed(&r, x, z);
        let treed = (top + 1..=top + 40).any(|y| world::stored(&r, [x, y, z]).is_some_and(|(b, _)| b.tree_id() != 0));
        if wet || treed {
            continue;
        }
        assert_eq!(gy, top, "column ({x}, {z})");
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} columns checked");
}
