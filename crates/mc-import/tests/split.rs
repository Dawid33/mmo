use crate::common::vol;
use mc_import::split::{slug, split, GroundMode, SplitOptions};
use mc_import::{BlockEntity, BlockVolume, Table, Tag};
use std::collections::BTreeMap;

const AIR: &str = "minecraft:air";
const GRASS: &str = "minecraft:grass_block";
const STONE: &str = "minecraft:stone_bricks";

fn opts() -> SplitOptions {
    SplitOptions { gap: 3, min_blocks: 30, ground: GroundMode::Auto }
}

/// A 4 x 6 x 4 pillar at (x, 2, 4) on a plate of 2 grass layers.
fn pillars(size: [u32; 3], plate: u32, xs: &'static [u32]) -> BlockVolume {
    vol(size, move |x, y, z| {
        if y < plate {
            GRASS.into()
        } else if xs.iter().any(|&p| (p..p + 4).contains(&x)) && (4..8).contains(&z) && y < plate + 6 {
            STONE.into()
        } else {
            AIR.into()
        }
    })
}

fn sign(pos: [i32; 3], text: &str) -> BlockEntity {
    let msgs = Tag::List(vec![Tag::String(format!("{{\"text\":\"{text}\"}}"))]);
    let front = Tag::Compound(BTreeMap::from([("messages".to_string(), msgs)]));
    BlockEntity {
        pos,
        id: "minecraft:sign".into(),
        nbt: Tag::Compound(BTreeMap::from([("front_text".to_string(), front)])),
    }
}

#[test]
fn three_statues_on_a_plate() {
    let v = pillars([40, 12, 14], 2, &[3, 15, 30]);
    let s = split(&v, Table::builtin(), &opts());
    assert_eq!(s.ground_layers, 2);
    assert_eq!(s.dropped_small, 0);
    let boxes: Vec<_> = s.builds.iter().map(|b| (b.min, b.max, b.blocks)).collect();
    assert_eq!(
        boxes,
        vec![
            ([3, 2, 4], [7, 8, 8], 96),
            ([15, 2, 4], [19, 8, 8], 96),
            ([30, 2, 4], [34, 8, 8], 96),
        ]
    );
}

#[test]
fn a_gap_joins_or_separates() {
    // Pillars at x = 3..7 and 9..13: two empty columns between them.
    let v = pillars([20, 12, 14], 2, &[3, 9]);
    let joined = split(&v, Table::builtin(), &SplitOptions { gap: 3, ..opts() });
    assert_eq!(joined.builds.len(), 1);
    assert_eq!(joined.builds[0].min, [3, 2, 4]);
    assert_eq!(joined.builds[0].max, [13, 8, 8]);
    let apart = split(&v, Table::builtin(), &SplitOptions { gap: 1, ..opts() });
    assert_eq!(apart.builds.len(), 2);
}

#[test]
fn small_debris_is_dropped() {
    let v = vol([20, 12, 14], |x, y, z| {
        if y < 2 {
            GRASS.into()
        } else if x == 3 && z == 4 && y < 4 {
            STONE.into()
        } else if (10..14).contains(&x) && (4..8).contains(&z) && y < 8 {
            STONE.into()
        } else {
            AIR.into()
        }
    });
    let s = split(&v, Table::builtin(), &opts());
    assert_eq!(s.dropped_small, 1);
    assert_eq!(s.builds.len(), 1);
    assert_eq!(s.builds[0].min, [10, 2, 4]);
}

#[test]
fn signs_label_builds() {
    let mut v = pillars([40, 12, 14], 2, &[3, 15, 30]);
    // On the plate, 2 cells east of the middle pillar (x 15..19).
    v.block_entities = vec![sign([20, 2, 5], "Zeus"), sign([20, 2, 6], "")];
    let s = split(&v, Table::builtin(), &opts());
    let labels: Vec<_> = s.builds.iter().map(|b| b.label.clone()).collect();
    assert_eq!(labels, vec![None, Some("Zeus".to_string()), None]);
}

#[test]
fn a_sign_goes_to_the_nearest_build_and_far_signs_are_ignored() {
    let mut v = pillars([40, 12, 14], 2, &[3, 15, 30]);
    v.block_entities = vec![
        sign([8, 2, 5], "West"),    // 2 from build 0, 7 from build 1
        sign([11, 2, 5], "Middle"), // 4 from build 1, 8 from build 0
        sign([12, 2, 5], "Closer"), // 3 from build 1: wins build 1
        sign([25, 2, 5], "Far"),    // 5 from build 2 (7 from build 1): build 2
        sign([38, 2, 12], "Tie"),   // also 5 from build 2: loses the tie on z
    ];
    let s = split(&v, Table::builtin(), &opts());
    let labels: Vec<_> = s.builds.iter().map(|b| b.label.clone().unwrap()).collect();
    assert_eq!(labels, vec!["West", "Closer", "Far"]);
}

#[test]
fn no_ground() {
    let v = vol([20, 12, 14], |x, y, z| {
        if (3..7).contains(&x) && (4..8).contains(&z) && y < 6 {
            STONE.into()
        } else {
            AIR.into()
        }
    });
    let s = split(&v, Table::builtin(), &opts());
    assert_eq!(s.ground_layers, 0);
    assert_eq!(s.builds.len(), 1);
    assert_eq!(s.builds[0].min, [3, 0, 4]);
    assert_eq!(s.builds[0].max, [7, 6, 8]);
    let forced = split(&v, Table::builtin(), &SplitOptions { ground: GroundMode::Layers(1), ..opts() });
    assert_eq!(forced.ground_layers, 1);
    assert_eq!(forced.builds[0].min, [3, 1, 4]);
    assert_eq!(forced.builds[0].blocks, 80);
    let none = split(&v, Table::builtin(), &SplitOptions { ground: GroundMode::None, ..opts() });
    assert_eq!(none.ground_layers, 0);
    let clamped = split(&v, Table::builtin(), &SplitOptions { ground: GroundMode::Layers(99), ..opts() });
    assert_eq!(clamped.ground_layers, 12);
    assert!(clamped.builds.is_empty());
}

#[test]
fn a_sparse_layer_is_not_ground() {
    // 59 of 100 cells solid in layer 0: not ground; 60 of 100: ground.
    let mk = |n: u32| vol([10, 3, 10], move |x, y, z| if y == 0 && z * 10 + x < n { GRASS.into() } else { AIR.into() });
    assert_eq!(split(&mk(59), Table::builtin(), &opts()).ground_layers, 0);
    assert_eq!(split(&mk(60), Table::builtin(), &opts()).ground_layers, 1);
}

#[test]
fn a_sign_inside_a_build_does_not_label_it() {
    let mut v = pillars([40, 12, 14], 2, &[3, 15, 30]);
    // On the plate beside pillar 1 (x 15..19): "Zeus"; inside pillar 0's box (x 3..7, z 4..8): "Decor".
    v.block_entities = vec![sign([20, 2, 5], "Zeus"), sign([4, 4, 5], "Decor")];
    let s = split(&v, Table::builtin(), &opts());
    let labels: Vec<_> = s.builds.iter().map(|b| b.label.clone()).collect();
    assert_eq!(labels, vec![None, Some("Zeus".to_string()), None]);
}

#[test]
fn short_labels_are_ignored_and_text_is_trimmed() {
    let mut v = pillars([40, 12, 14], 2, &[3, 15, 30]);
    v.block_entities = vec![sign([20, 2, 5], "d"), sign([10, 2, 5], "  Ab! "), sign([25, 2, 5], "  Hera  ")];
    let s = split(&v, Table::builtin(), &opts());
    let labels: Vec<_> = s.builds.iter().map(|b| b.label.clone()).collect();
    // "d" (build 1) and "Ab!" (2 alphanumerics, build 0 or 1) are too short; "Hera" labels build 2.
    assert_eq!(labels, vec![None, None, Some("Hera".to_string())]);
}

#[test]
fn slug_rules() {
    assert_eq!(slug(&"ab ".repeat(100)).len(), 59);
    assert!(slug(&"a".repeat(200)).len() <= 60);
    assert_eq!(slug(&format!("{}-b", "a".repeat(59))), "a".repeat(59));
    assert!(!slug(&"word ".repeat(40)).ends_with('-'));
    assert_eq!(slug("Zeus Statue #2!"), "zeus-statue-2");
    assert_eq!(slug("Häuser"), "h-user");
    assert_eq!(slug("!!!"), "");
    assert_eq!(slug("  a -- b  "), "a-b");
}
