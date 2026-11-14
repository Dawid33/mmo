//! The town kit: houses by style tag with a front and a frontage. Prefabs are made in memory.

use game::prefab::Facing;
use worldgen::sites::kit::*;

use crate::structure_support::prefabs::{house_box, resolve_file, set_of};

fn boxed(size: [u32; 3], doors: &[[i32; 3]]) -> std::sync::Arc<game::prefab::ResolvedPrefab> {
    resolve_file(house_box("b", size, &["medieval", "house"], doors))
}

#[test]
fn front_follows_the_door() {
    assert_eq!(front_of(&boxed([9, 5, 7], &[[8, 1, 3]])), Facing::East);
    assert_eq!(front_of(&boxed([9, 5, 7], &[[4, 4, 0], [4, 1, 6]])), Facing::South);
    assert_eq!(front_of(&boxed([9, 5, 7], &[[1, 1, 3]])), Facing::West);
    assert_eq!(front_of(&boxed([9, 5, 7], &[])), Facing::South);
    // a marker outside the box clamps to the face
    assert_eq!(front_of(&boxed([9, 5, 7], &[[4, 1, -2]])), Facing::North);
}

#[test]
fn frontage_is_along_the_front() {
    let tags = ["medieval", "house"];
    let east = set_of(&[house_box("e", [9, 5, 7], &tags, &[[8, 1, 3]]), house_box("n", [9, 5, 7], &tags, &[[4, 1, 6]]),
        house_box("o", [9, 5, 7], &tags, &[])]);
    let kit = town_kit(&east, "medieval").unwrap();
    let by = |n: &str| kit.houses.iter().find(|h| h.prefab.name == n).unwrap().clone();
    let e = by("e");
    assert_eq!((e.front, e.frontage, e.depth), (Facing::East, 7, 9));
    let s = by("n");
    assert_eq!((s.front, s.frontage, s.depth), (Facing::South, 9, 7));
}

#[test]
fn the_kit_filters_by_tags_and_size() {
    let m = ["medieval", "house"];
    let mut files: Vec<_> = [6u32, 20, 48, 49, 4]
        .iter()
        .map(|&s| house_box(&format!("h{s:02}"), [s, 6, s], &m, &[]))
        .collect();
    files.push(house_box("hz-long", [6, 6, 60], &m, &[]));
    files.push(house_box("hz-tall", [6, 90, 6], &m, &[]));
    files.push(house_box("desert", [8, 6, 8], &["desert", "house"], &[]));
    files.push(house_box("statue", [10, 10, 10], &["medieval", "statue"], &[]));
    files.push(house_box("big-decor", [30, 3, 30], &["decor"], &[]));
    let kit = town_kit(&set_of(&files), "medieval").unwrap();
    let names: Vec<&str> = kit.houses.iter().map(|h| h.prefab.name.as_str()).collect();
    assert_eq!(names, ["h06", "h20", "h48", "hz-tall"]);
    let cs: Vec<&str> = kit.centrepieces.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(cs, ["statue"]);
}

#[test]
fn too_few_houses_is_an_error() {
    let m = ["medieval", "house"];
    let set = set_of(&[house_box("a", [6, 6, 6], &m, &[]), house_box("b", [7, 6, 7], &m, &[])]);
    let err = town_kit(&set, "medieval").unwrap_err();
    assert!(err.contains("medieval") && err.contains('2'), "{err}");
}

#[test]
fn turn_to_face_turns_the_front_toward() {
    let all = [Facing::North, Facing::East, Facing::South, Facing::West];
    for front in all {
        for toward in all {
            let t = turn_to_face(front, toward);
            assert!(t < 4);
            assert_eq!(Facing::from_quarter_turns(front.quarter_turns() + t), toward);
        }
    }
}
