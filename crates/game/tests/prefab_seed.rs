//! `stamp_build_into_seed`: a Build prefab written into a region's worldgen seed.

use crate::common::prefabs::{arch, build_file, build_of, empty_build, part, resolved};
use crate::common::{uniform_seed, FLOOR_PLATES};
use game::{
    stamp_build_into_seed, BuildFile, CellBox, RegionCoords, ResolvedBuild, StudBrick, StudChunkCoords, StudRegionSeed,
    REGION_BLOCKS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, TILE_STUDS,
};

const RC: RegionCoords = RegionCoords { x: 0, z: 0 };
/// The plastic the fixtures are made of.
const RED: u16 = 4;

fn region_studs() -> i64 {
    (REGION_BLOCKS * TILE_STUDS) as i64
}

fn anchor_on_surface() -> [i64; 3] {
    [40, FLOOR_PLATES as i64, 40]
}

fn build(b: BuildFile) -> ResolvedBuild {
    build_of(&resolved(&build_file("t", b))).clone()
}

/// The brick covering world cell `cell`, read from `seed`'s chunks as region `rc` would lay them.
fn brick_at_world(seed: &StudRegionSeed, rc: RegionCoords, cell: [i64; 3]) -> Option<StudBrick> {
    let n = STUD_CHUNK_STUDS as i64;
    let local = [cell[0] - rc.x as i64 * region_studs(), cell[1], cell[2] - rc.z as i64 * region_studs()];
    if local[0] < 0 || local[2] < 0 {
        return None;
    }
    let c = StudChunkCoords {
        x: (local[0] / n) as usize,
        y: local[1].div_euclid(STUD_CHUNK_PLATES as i64) as i32,
        z: (local[2] / n) as usize,
    };
    let in_chunk = [(local[0] % n) as u8, local[1].rem_euclid(STUD_CHUNK_PLATES as i64) as u8, (local[2] % n) as u8];
    seed.feature_chunks().get(&c).and_then(|chunk| chunk.brick_at(in_chunk).copied())
}

fn red_cells(seed: &StudRegionSeed) -> usize {
    seed.feature_chunks().values().flat_map(|c| c.bricks()).filter(|b| b.material == RED).map(|b| b.cells().count()).sum()
}

#[test]
fn stamp_appears_in_feature_chunks() {
    let mut seed = uniform_seed();
    assert!(seed.feature_chunks().is_empty());
    let report = stamp_build_into_seed(&mut seed, RC, build_of(&resolved(&arch())), anchor_on_surface(), 0);
    assert_eq!((report.skipped_split, report.chunks_in_region), (0, 1));
    let b = brick_at_world(&seed, RC, anchor_on_surface()).expect("a brick on the anchor");
    assert_eq!(b.material, RED);
    // The ground under it is still there.
    let mut below = anchor_on_surface();
    below[1] -= 1;
    assert!(brick_at_world(&seed, RC, below).is_some_and(|b| b.material != RED));
    // Two posts of 2 x 4 x 3 and a lintel of 8 x 2 x 3.
    assert_eq!(red_cells(&seed), 2 * 24 + 48);
}

#[test]
fn carve_removes_ground_before_laying() {
    let p = build(BuildFile {
        parts: vec![part("3001", [0, -3, 0], 0)],
        carve: vec![CellBox { min: [0, -3, 0], size: [6, 3, 2] }],
        ..empty_build()
    });
    let mut seed = uniform_seed();
    stamp_build_into_seed(&mut seed, RC, &p, anchor_on_surface(), 0);
    let a = anchor_on_surface();
    // The brick sits in the carved pit, and the rest of the pit is open.
    assert_eq!(brick_at_world(&seed, RC, [a[0], a[1] - 3, a[2]]).map(|b| b.material), Some(RED));
    assert_eq!(brick_at_world(&seed, RC, [a[0] + 5, a[1] - 1, a[2]]), None);
    assert!(brick_at_world(&seed, RC, [a[0] + 6, a[1] - 1, a[2]]).is_some());
}

#[test]
fn only_this_regions_chunks_are_kept() {
    // One stud inside the region's east edge, a 2 x 8 brick pointing east across the seam.
    let p = build(BuildFile { parts: vec![part("3007", [0, 0, 0], 0)], ..empty_build() });
    let anchor = [region_studs() - 1, FLOOR_PLATES as i64, 40];
    let east = RegionCoords { x: RC.x + 1, z: RC.z };
    let (mut a_seed, mut b_seed) = (uniform_seed(), uniform_seed());
    let a = stamp_build_into_seed(&mut a_seed, RC, &p, anchor, 0);
    let b = stamp_build_into_seed(&mut b_seed, east, &p, anchor, 0);
    assert!(a.chunks_in_region >= 1 && b.chunks_in_region >= 1);
    assert_eq!(red_cells(&a_seed), 2 * 3);
    assert_eq!(red_cells(&a_seed) + red_cells(&b_seed), 2 * 8 * 3);
    assert_eq!(brick_at_world(&a_seed, RC, anchor).map(|b| b.material), Some(RED));
    assert_eq!(brick_at_world(&b_seed, east, [anchor[0] + 1, anchor[1], anchor[2]]).map(|b| b.material), Some(RED));
    // A region the prefab does not reach keeps nothing.
    let mut far = uniform_seed();
    let none = stamp_build_into_seed(&mut far, RegionCoords { x: 5, z: 5 }, &p, anchor, 0);
    assert_eq!(none.chunks_in_region, 0);
    assert_eq!(far, uniform_seed());
}

#[test]
fn stamps_apply_before_features() {
    let p = build(BuildFile { parts: vec![part("3001", [0, 0, 0], 0)], ..empty_build() });
    let mut seed = uniform_seed();
    stamp_build_into_seed(&mut seed, RC, &p, anchor_on_surface(), 0);
    // A feature brick of another colour on the same cells: it is the one dropped.
    let (c, stamp) = seed.stamps[0].clone();
    let mut feature = stamp.bricks[0];
    feature.material = 1;
    let mut beside = feature;
    beside.min[0] += 8;
    seed.features.push((c, vec![feature, beside]));
    let chunk = &seed.feature_chunks()[&c];
    assert_eq!(chunk.brick_at(feature.min).map(|b| b.material), Some(RED));
    assert_eq!(chunk.brick_at(beside.min).map(|b| b.material), Some(1));
}

#[test]
fn a_split_slope_is_reported() {
    let p = build(BuildFile { parts: vec![part("3040", [0, 0, 0], 0)], ..empty_build() });
    let mut seed = uniform_seed();
    let report = stamp_build_into_seed(&mut seed, RC, &p, [40, FLOOR_PLATES as i64, 31], 0);
    assert_eq!(report.skipped_split, 1);
    assert_eq!(red_cells(&seed), 0);
}
