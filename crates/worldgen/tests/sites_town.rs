//! The town layout: a plaza, straight streets and houses on lots facing them. The kit is made in memory.

use game::prefab::{Facing, PrefabFile};
use game::BlockId;
use worldgen::sites::kit::*;
use worldgen::sites::town::*;
use worldgen::structures::StructurePlacement;

use crate::structure_support::prefabs::{house_box, set_of, town_house, town_houses};

const ROAD: BlockId = BlockId(3);
const PLAZA: BlockId = BlockId(4);
const O: [i32; 2] = [1000, -2000];
const STYLE: &str = "testville";

fn flat(_: i32, _: i32) -> i32 {
    70
}
fn dry(_: i32, _: i32) -> bool {
    false
}

fn houses() -> Vec<PrefabFile> {
    town_houses(STYLE, true)
}

fn kit() -> TownKit {
    town_kit(&set_of(&houses()), STYLE).unwrap()
}

fn kit_with_statue() -> TownKit {
    let mut files = houses();
    files.push(house_box("statue", [5, 6, 5], &["statue"], &[]));
    let k = town_kit(&set_of(&files), STYLE).unwrap();
    assert_eq!(k.centrepieces.len(), 1);
    k
}

fn small(seed: u64) -> TownParams {
    TownParams::small(seed, ROAD, PLAZA)
}

fn town(p: &TownParams) -> TownLayout {
    layout_town(&kit(), O, p, &flat, &dry)
}

/// The arm's outward direction and its left-hand direction (looking outward from the plaza).
fn dirs(arm: u8) -> ([i32; 2], [i32; 2]) {
    match arm {
        0 => ([1, 0], [0, -1]),
        1 => ([-1, 0], [0, 1]),
        2 => ([0, 1], [1, 0]),
        _ => ([0, -1], [-1, 0]),
    }
}

/// The half-open range a world box covers along unit direction `u` measured from the origin.
fn range_along(min: [i32; 3], max: [i32; 3], u: [i32; 2]) -> (i32, i32) {
    let (axis, sign) = if u[0] != 0 { (0, u[0]) } else { (2, u[1]) };
    let o = if axis == 0 { O[0] } else { O[1] };
    if sign > 0 { (min[axis] - o, max[axis] - o) } else { (o - max[axis] + 1, o - min[axis] + 1) }
}

fn rect_of(min: [i32; 3], max: [i32; 3]) -> [i32; 4] {
    [min[0], min[2], max[0], max[2]]
}

fn grow(r: [i32; 4], by: i32) -> [i32; 4] {
    [r[0] - by, r[1] - by, r[2] + by, r[3] + by]
}

fn meet(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The arm's full street strip as a world rectangle.
fn strip(p: &TownParams, arm: u8) -> [i32; 4] {
    let (h, s) = (p.plaza / 2, p.street_w / 2);
    match arm {
        0 => [O[0] + h + 1, O[1] - s, O[0] + h + 1 + p.arm_len, O[1] + s + 1],
        1 => [O[0] - h - p.arm_len, O[1] - s, O[0] - h, O[1] + s + 1],
        2 => [O[0] - s, O[1] + h + 1, O[0] + s + 1, O[1] + h + 1 + p.arm_len],
        _ => [O[0] - s, O[1] - h - p.arm_len, O[0] + s + 1, O[1] - h],
    }
}

fn plaza_rect(p: &TownParams) -> [i32; 4] {
    let h = p.plaza / 2;
    [O[0] - h, O[1] - h, O[0] + h + 1, O[1] + h + 1]
}

fn house_placements(l: &TownLayout) -> &[StructurePlacement] {
    &l.placements[l.placements.len() - l.lots.len()..]
}

#[test]
fn deterministic_and_seed_sensitive() {
    let p = small(7);
    let (a, b) = (town(&p), town(&p));
    assert_eq!(a.lots, b.lots);
    assert_eq!(a.paints, b.paints);
    let ats = |l: &TownLayout| l.placements.iter().map(|sp| sp.at).collect::<Vec<_>>();
    assert_eq!(ats(&a), ats(&b));
    assert_eq!(a.skipped, b.skipped);
    assert_ne!(town(&small(8)).lots, a.lots);
}

#[test]
fn every_house_faces_its_street() {
    let k = kit();
    let l = town(&small(3));
    assert!(!l.lots.is_empty());
    for (lot, sp) in l.lots.iter().zip(house_placements(&l)) {
        let kh = k.houses.iter().find(|h| h.prefab.name == lot.name).unwrap();
        assert_eq!(sp.prefab.name, lot.name);
        assert_eq!(Facing::from_quarter_turns(kh.front.quarter_turns() + sp.at.turn), lot.faces, "{lot:?}");
        let want = match (lot.arm, lot.left) {
            (0, true) => Facing::South,
            (0, false) => Facing::North,
            (1, true) => Facing::North,
            (1, false) => Facing::South,
            (2, true) => Facing::West,
            (2, false) => Facing::East,
            (3, true) => Facing::East,
            _ => Facing::West,
        };
        assert_eq!(lot.faces, want, "{lot:?}");
        assert!(!sp.at.mirror);
    }
}

#[test]
fn fronts_stand_at_the_setback() {
    for setback in [1, 3] {
        let p = TownParams { setback, ..small(11) };
        let l = town(&p);
        assert!(!l.lots.is_empty());
        let s = p.street_w / 2;
        for lot in &l.lots {
            let (_, left) = dirs(lot.arm);
            let side = if lot.left { left } else { [-left[0], -left[1]] };
            let (near, _) = range_along(lot.bounds.0, lot.bounds.1, side);
            assert_eq!(near - (s + 1), setback, "{lot:?}");
        }
    }
}

#[test]
fn nothing_overlaps() {
    let p = small(5);
    let l = layout_town(&kit_with_statue(), O, &p, &flat, &dry);
    let rects: Vec<[i32; 4]> = l.placements.iter().map(|sp| {
        let (min, max) = sp.bounds();
        rect_of(min, max)
    }).collect();
    for (i, a) in rects.iter().enumerate() {
        for b in &rects[i + 1..] {
            assert!(!meet(grow(*a, 1), *b), "{a:?} touches {b:?}");
        }
    }
    for lot in &l.lots {
        let r = rect_of(lot.bounds.0, lot.bounds.1);
        assert!(!meet(r, plaza_rect(&p)), "{lot:?}");
        for arm in 0..4 {
            assert!(!meet(r, strip(&p, arm)), "{lot:?} on arm {arm}");
        }
    }
}

#[test]
fn lots_stay_within_the_arm() {
    for seed in [1, 2, 3] {
        let p = small(seed);
        let h = p.plaza / 2;
        for lot in &town(&p).lots {
            let (d, _) = dirs(lot.arm);
            let (a0, a1) = range_along(lot.bounds.0, lot.bounds.1, d);
            assert!(a0 >= h + 1 && a1 - 1 <= h + p.arm_len, "{lot:?}");
        }
    }
}

#[test]
fn houses_sit_on_their_pads() {
    let p = small(9);
    let l = town(&p);
    for sp in house_placements(&l) {
        let pad = sp.pad.unwrap();
        assert_eq!((pad.level, pad.skirt), (70, p.pad_skirt));
        assert_eq!(sp.bounds().0[1], 71);
    }
}

#[test]
fn paints_cover_plaza_and_streets() {
    let p = small(4);
    let l = town(&p);
    assert_eq!(l.paints[0].rect, plaza_rect(&p));
    assert_eq!(l.paints[0].block, PLAZA);
    let (h, s) = (p.plaza / 2, p.street_w / 2);
    let mut i = 1;
    for arm in 0..4u8 {
        let lots: Vec<_> = l.lots.iter().filter(|lot| lot.arm == arm).collect();
        if lots.is_empty() {
            continue;
        }
        let (d, left) = dirs(arm);
        let far = lots.iter().map(|lot| range_along(lot.bounds.0, lot.bounds.1, d).1).max().unwrap();
        let paint = l.paints[i];
        i += 1;
        assert_eq!(paint.block, ROAD);
        let (pmin, pmax) = ([paint.rect[0], 0, paint.rect[1]], [paint.rect[2], 1, paint.rect[3]]);
        assert_eq!(range_along(pmin, pmax, d), (h + 1, far), "arm {arm}");
        assert_eq!(range_along(pmin, pmax, left), (-s, s + 1), "arm {arm}");
    }
    assert_eq!(i, l.paints.len());
    assert!(i > 1);

    let none = town(&TownParams { arm_len: 4, ..p });
    assert!(none.lots.is_empty());
    assert_eq!(none.paints.len(), 1);
}

#[test]
fn wet_and_steep_lots_are_skipped() {
    let p = small(6);
    let base = town(&p);
    let arm_lots = |l: &TownLayout, arm: u8| l.lots.iter().filter(|lot| lot.arm == arm).cloned().collect::<Vec<_>>();

    let line = O[0] + 40;
    let wet = layout_town(&kit(), O, &p, &flat, &move |x, _| x > line);
    assert!(wet.skipped > base.skipped);
    assert!(!arm_lots(&wet, 0).is_empty());
    for lot in arm_lots(&wet, 0) {
        assert!(lot.bounds.1[0] - 1 <= line, "{lot:?}");
    }
    for arm in 1..4 {
        assert_eq!(arm_lots(&wet, arm), arm_lots(&base, arm), "arm {arm}");
    }

    let steep_p = TownParams { max_relief: 3, ..p };
    let steep_base = town(&steep_p);
    let rise = |_: i32, z: i32| 70 + (z - (O[1] + 30)).max(0);
    let steep = layout_town(&kit(), O, &steep_p, &rise, &dry);
    assert!(steep.skipped > steep_base.skipped);
    assert!(arm_lots(&steep, 2).len() < arm_lots(&steep_base, 2).len());
    for arm in [0, 1, 3] {
        assert_eq!(arm_lots(&steep, arm), arm_lots(&steep_base, arm), "arm {arm}");
    }
}

#[test]
fn a_town_has_a_sensible_number_of_houses() {
    for seed in [1, 2, 3, 4, 5] {
        let n = town(&small(seed)).lots.len();
        assert!((12..=60).contains(&n), "seed {seed}: {n} lots");
    }
}

#[test]
fn the_centrepiece_is_on_the_plaza() {
    let p = small(2);
    let l = layout_town(&kit_with_statue(), O, &p, &flat, &dry);
    assert_eq!(l.placements.len(), l.lots.len() + 1);
    let c = &l.placements[0];
    assert_eq!(c.prefab.name, "statue");
    assert_eq!(c.at.turn, 0);
    let (min, max) = c.bounds();
    assert_eq!((min[0] + max[0] - 1, min[2] + max[2] - 1), (2 * O[0], 2 * O[1]));
    assert_eq!(c.pad.map(|pad| (pad.level, pad.skirt)), Some((70, 0)));
    assert_eq!(min[1], 71);
    let cr = rect_of(min, max);
    for lot in &l.lots {
        assert!(!meet(grow(rect_of(lot.bounds.0, lot.bounds.1), 1), cr), "{lot:?}");
    }
}

#[test]
fn roll_is_stable() {
    let r = roll(1, 2, 3, 4);
    assert_eq!(r, 15_350_249_956_527_044_492);
    for other in [roll(2, 2, 3, 4), roll(1, 3, 3, 4), roll(1, 2, 4, 4), roll(1, 2, 3, 5)] {
        assert_ne!(other, r);
    }
}


/// `n` houses of one size, told apart by name.
fn same_size_kit(n: usize) -> TownKit {
    let files: Vec<PrefabFile> =
        (0..n).map(|i| town_house(&format!("s{i:02}"), STYLE, [9, 5, 9], Facing::South, true)).collect();
    town_kit(&set_of(&files), STYLE).unwrap()
}

#[test]
fn a_town_repeats_no_house_while_the_kit_has_more() {
    let kit = same_size_kit(80);
    for seed in [1, 2, 3, 4] {
        let l = layout_town(&kit, O, &small(seed), &flat, &dry);
        assert!(l.lots.len() >= 8 && l.lots.len() <= 80, "seed {seed}: {}", l.lots.len());
        let mut names: Vec<&str> = l.lots.iter().map(|lot| lot.name.as_str()).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "seed {seed}: a house repeats");
    }
}

#[test]
fn a_small_kit_repeats_but_uses_every_house_and_fills_the_lots() {
    let kit = same_size_kit(3);
    for seed in 1..=4 {
        let l = layout_town(&kit, O, &small(seed), &flat, &dry);
        assert!(l.lots.len() > 3, "seed {seed}: {}", l.lots.len());
        for h in &kit.houses {
            assert!(l.lots.iter().any(|lot| lot.name == h.prefab.name), "seed {seed}: {} unused", h.prefab.name);
        }
        // the repeats do not starve the lots: as many as the same arms hold with an endless kit
        let wide = layout_town(&same_size_kit(60), O, &small(seed), &flat, &dry);
        assert_eq!(l.lots.len(), wide.lots.len(), "seed {seed}");
    }
}

#[test]
fn the_pick_is_deterministic_with_the_no_repeat_rule() {
    let kit = same_size_kit(20);
    let (a, b) = (layout_town(&kit, O, &small(9), &flat, &dry), layout_town(&kit, O, &small(9), &flat, &dry));
    assert_eq!(a.lots, b.lots);
    assert_eq!(a.skipped, b.skipped);
}

#[test]
fn ground_is_asked_once_per_column_and_not_per_candidate() {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    let kit = same_size_kit(40);
    let p = small(3);
    let asked = RefCell::new(BTreeSet::new());
    let calls = RefCell::new(0u32);
    let ground = |x: i32, z: i32| {
        *calls.borrow_mut() += 1;
        assert!(asked.borrow_mut().insert((x, z)), "ground asked twice for ({x}, {z})");
        70
    };
    let l = layout_town(&kit, O, &p, &ground, &dry);
    // One lot attempt (laid or skipped) reads at most its 5 corner/centre samples plus the middle of the
    // front face; the memo makes a repeat free and a rejected candidate reads nothing, so 6 per attempt
    // is a guaranteed ceiling, whatever the kit size (40 houses x 5 x lots would be ~10x this).
    let attempts = l.lots.len() as u32 + l.skipped;
    assert!(*calls.borrow() <= 6 * attempts, "{} calls for {attempts} attempts", calls.borrow());
}

/// Five houses too long for a short arm, one narrow one: an arm side is not ended by picking a wide one
/// (the old rule broke on the first misfit pick, which here is 5 in 6).
#[test]
fn a_wide_house_does_not_end_the_arm_while_a_narrow_one_fits() {
    let mut files: Vec<PrefabFile> =
        (0..5).map(|i| town_house(&format!("wide{i}"), STYLE, [28, 5, 9], Facing::South, true)).collect();
    files.push(town_house("narrow", STYLE, [7, 5, 7], Facing::South, true));
    let kit = town_kit(&set_of(&files), STYLE).unwrap();
    for seed in 1..=8 {
        let p = TownParams { arms: 1, arm_len: 26, ..small(seed) };
        let l = layout_town(&kit, O, &p, &flat, &dry);
        for left in [true, false] {
            let n = l.lots.iter().filter(|lot| lot.left == left).count();
            // 7 wide + 2..5 gap: at least 2 narrow houses fit between 10 and 34
            assert!(n >= 2, "seed {seed} left={left}: {n} lots");
        }
        assert!(l.lots.iter().all(|lot| !lot.name.starts_with("wide")));
    }
}

#[test]
fn a_lot_takes_a_small_house_where_big_ones_fail_relief() {
    let mut files: Vec<PrefabFile> =
        (0..3).map(|i| town_house(&format!("big{i}"), STYLE, [21, 5, 9], Facing::South, true)).collect();
    files.extend((0..3).map(|i| town_house(&format!("sm{i}"), STYLE, [5, 5, 5], Facing::South, true)));
    let kit = town_kit(&set_of(&files), STYLE).unwrap();
    let slope = |x: i32, _: i32| 70 + (x - O[0]) / 2;
    // relief of a 21-wide house is ~10 > 6; of a 5-wide one ~2
    let p = TownParams { arms: 1, ..small(4) };
    let l = layout_town(&kit, O, &p, &slope, &dry);
    assert!(!l.lots.is_empty());
    assert!(l.lots.iter().all(|lot| lot.name.starts_with("sm")), "{:?}", l.lots);
    assert_eq!(l.skipped, 0, "a lot is skipped only when no house fits");
    // nothing fits: every attempt is a skip
    let none = layout_town(&kit, O, &TownParams { max_relief: 0, ..p }, &slope, &dry);
    assert!(none.lots.is_empty());
    assert!(none.skipped > 0);
}
