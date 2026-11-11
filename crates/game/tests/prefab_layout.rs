//! `layout_build`: a Build prefab turned, moved to an anchor and cut at chunk edges.

use std::collections::BTreeSet;

use crate::common::prefabs::{build_file, build_of, empty_build, part, resolved, rich};
use game::stud_catalog::STUD_PARTS;
use game::{
    layout_build, turn_cell, BuildBoxFile, BuildFile, BuildLayout, CellBox, ChunkStamp, MaterialName, ResolvedBuild,
    StudBrick, StudChunk, WorldChunkPos, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
};

fn build(b: BuildFile) -> ResolvedBuild {
    build_of(&resolved(&build_file("t", b))).clone()
}

fn parts(list: Vec<game::BuildPartFile>) -> ResolvedBuild {
    build(BuildFile { parts: list, ..empty_build() })
}

fn part_id(number: &str) -> u16 {
    STUD_PARTS.iter().position(|p| p.number == number).unwrap() as u16
}

fn brick(number: &str, min: [u8; 3]) -> StudBrick {
    StudBrick::part_at(min, part_id(number), 0, 4)
}

fn to_world(c: WorldChunkPos, cell: [u8; 3]) -> [i64; 3] {
    [
        c.x * STUD_CHUNK_STUDS as i64 + cell[0] as i64,
        c.y as i64 * STUD_CHUNK_PLATES as i64 + cell[1] as i64,
        c.z * STUD_CHUNK_STUDS as i64 + cell[2] as i64,
    ]
}

fn world_cells(l: &BuildLayout) -> BTreeSet<[i64; 3]> {
    let mut out = BTreeSet::new();
    for (c, s) in &l.chunks {
        for b in &s.bricks {
            for cell in b.cells() {
                assert!(out.insert(to_world(*c, cell)), "a cell is laid twice");
            }
        }
    }
    out
}

fn world_stud_cells(l: &BuildLayout) -> BTreeSet<[i64; 3]> {
    l.chunks.iter().flat_map(|(c, s)| s.bricks.iter().flat_map(|b| b.stud_cells().into_iter().map(|cell| to_world(*c, cell)))).collect()
}

fn turned(cells: BTreeSet<[i64; 3]>, anchor: [i64; 3], turn: u8) -> BTreeSet<[i64; 3]> {
    cells
        .into_iter()
        .map(|c| {
            let rel = [(c[0] - anchor[0]) as i32, (c[1] - anchor[1]) as i32, (c[2] - anchor[2]) as i32];
            let t = turn_cell(rel, turn);
            [anchor[0] + t[0] as i64, anchor[1] + t[1] as i64, anchor[2] + t[2] as i64]
        })
        .collect()
}

fn total_bricks(l: &BuildLayout) -> usize {
    l.chunks.values().map(|s| s.bricks.len()).sum()
}

#[test]
fn turn_cell_four_times_is_identity() {
    let c = [3, 7, -5];
    assert_eq!(turn_cell(c, 0), c);
    assert_eq!(turn_cell(c, 1), [5, 7, 3]);
    let mut c4 = c;
    for _ in 0..4 {
        c4 = turn_cell(c4, 1);
    }
    assert_eq!(c4, c);
    for t in 0..4u8 {
        assert_eq!(turn_cell(turn_cell(c, t), (4 - t) % 4), c);
        assert_eq!(turn_cell(turn_cell(c, 1), t), turn_cell(c, (t + 1) % 4));
    }
}

#[test]
fn one_brick_lands_on_the_anchor() {
    let l = layout_build(&parts(vec![part("3001", [0, 0, 0], 0)]), [40, 95, 70], 0);
    let s = &l.chunks[&WorldChunkPos::new(1, 1, 2)];
    assert_eq!(s.bricks[0].min, [8, 5, 6]);
    assert_eq!(s.bricks[0], StudBrick::part_at([8, 5, 6], part_id("3001"), 0, 4));
    assert_eq!(l.chunks.len(), 1);
    assert_eq!(l.skipped_split, 0);
}

#[test]
fn turning_agrees_with_brick_rotation() {
    // Slope 3040 has studs on one side only, so a wrong turn direction shows.
    for rot in 0..4 {
        let p = parts(vec![part("3040", [2, 0, 1], rot), part("3001", [-6, 3, 4], rot)]);
        let anchor = [16, 0, 16];
        let base = layout_build(&p, anchor, 0);
        assert_eq!(base.skipped_split, 0);
        for t in 1..4 {
            let l = layout_build(&p, anchor, t);
            assert_eq!(l.skipped_split, 0);
            assert_eq!(world_cells(&l), turned(world_cells(&base), anchor, t), "cells, rot {rot} turn {t}");
            assert_eq!(world_stud_cells(&l), turned(world_stud_cells(&base), anchor, t), "studs, rot {rot} turn {t}");
        }
    }
}

#[test]
fn box_brick_across_a_chunk_edge_is_retiled() {
    let l = layout_build(&parts(vec![part("3007", [0, 0, 0], 0)]), [28, 0, 0], 0); // spans x 28..36
    assert_eq!(l.chunks.len(), 2);
    assert_eq!(l.skipped_split, 0);
    assert_eq!(world_cells(&l).len(), 2 * 8 * 3);
    assert!(l.chunks.values().flat_map(|s| &s.bricks).all(|b| b.material == 4));
}

#[test]
fn slope_across_a_chunk_edge_is_skipped_and_counted() {
    // 3040 is 1 x 3 x 2: from z = 31 it would straddle the z edge.
    let p = parts(vec![part("3040", [0, 0, 0], 0), part("3005", [5, 0, 0], 0)]);
    let l = layout_build(&p, [0, 0, 31], 0);
    assert_eq!(l.skipped_split, 1);
    assert_eq!(total_bricks(&l), 1);
}

#[test]
fn negative_cells_go_to_lower_chunks() {
    let l = layout_build(&parts(vec![part("3005", [-1, -1, -1], 0)]), [0, 0, 0], 0);
    assert!(l.chunks.contains_key(&WorldChunkPos::new(-1, -1, -1)));
    assert_eq!(world_cells(&l), BTreeSet::from([[-1, -1, -1], [-1, 0, -1], [-1, 1, -1]]));
}

#[test]
fn boxes_are_tiled_per_chunk() {
    let b = build(BuildFile {
        boxes: vec![BuildBoxFile { min: [-4, 0, 0], size: [40, 3, 2], material: MaterialName::Natural("stone".into()), data: 0 }],
        ..empty_build()
    });
    for t in 0..4 {
        let l = layout_build(&b, [30, 0, 0], t);
        assert_eq!(world_cells(&l).len(), 40 * 3 * 2);
        assert!(l.chunks.len() >= 2);
    }
}

#[test]
fn carve_boxes_are_cut_per_chunk() {
    let b = build(BuildFile { carve: vec![CellBox { min: [-2, -1, 0], size: [4, 2, 1] }], ..empty_build() });
    let l = layout_build(&b, [32, 90, 0], 0);
    // x 30..34 and y 89..91: four chunks, each with its own slice (the chunks beside
    // them hold only the shell).
    assert_eq!(l.chunks.values().filter(|s| !s.carve.is_empty()).count(), 4);
    assert!(l.chunks[&WorldChunkPos::new(0, 0, -1)].carve.is_empty());
    assert_eq!(l.chunks[&WorldChunkPos::new(0, 0, 0)].carve, vec![([30, 89, 0], [2, 1, 1])]);
    assert_eq!(l.chunks[&WorldChunkPos::new(1, 1, 0)].carve, vec![([0, 0, 0], [2, 1, 1])]);
}

#[test]
fn output_is_canonical() {
    let p = build_of(&resolved(&rich())).clone();
    let mut shuffled = p.clone();
    shuffled.parts.reverse();
    assert_eq!(layout_build(&p, [5, 5, 5], 2).chunks, layout_build(&shuffled, [5, 5, 5], 2).chunks);
}

#[test]
fn apply_carves_then_places_and_counts_overlaps() {
    let mut c = StudChunk::new([brick("3001", [0, 0, 0])]).unwrap();
    let s = ChunkStamp {
        carve: vec![([0, 0, 0], [4, 3, 2])],
        bricks: vec![brick("3001", [0, 0, 0]), brick("3001", [10, 0, 0]), brick("3001", [10, 0, 0])],
        ..Default::default()
    };
    let applied = s.apply(0, &mut c);
    assert_eq!((applied.skipped, applied.placed, applied.changed), (1, 2, true));
    assert_eq!(c.len(), 2);
}

#[test]
fn apply_skips_invalid_bricks_instead_of_panicking() {
    let mut c = StudChunk::default();
    let s = ChunkStamp {
        carve: vec![([30, 80, 30], [200, 200, 200])],
        bricks: vec![StudBrick::part_at([31, 0, 0], u16::MAX, 0, 4), brick("3001", [30, 0, 0]), brick("3005", [0, 0, 0])],
        shell: vec![([0, 0, 0], [255, 255, 255])],
    };
    assert_eq!(s.apply(0, &mut c).skipped, 2);
    assert_eq!(c.len(), 1);
}

#[test]
fn a_carve_box_gets_a_shell_one_cell_thick() {
    // A 4 x 3 x 2 box well inside one chunk: six face slabs, no edges or corners.
    let b = build(BuildFile { carve: vec![CellBox { min: [0, 0, 0], size: [4, 3, 2] }], ..empty_build() });
    let l = layout_build(&b, [8, 30, 8], 0);
    let s = &l.chunks[&WorldChunkPos::new(0, 0, 0)];
    assert_eq!(s.carve, vec![([8, 30, 8], [4, 3, 2])]);
    let mut shell = s.shell.clone();
    shell.sort();
    assert_eq!(
        shell,
        vec![
            ([7, 30, 8], [1, 3, 2]),
            ([8, 29, 8], [4, 1, 2]),
            ([8, 30, 7], [4, 3, 1]),
            ([8, 30, 10], [4, 3, 1]),
            ([8, 33, 8], [4, 1, 2]),
            ([12, 30, 8], [1, 3, 2]),
        ]
    );
    // A box that ends on a chunk edge puts its shell into the chunk across it.
    let l = layout_build(&b, [28, 30, 8], 0);
    let across = &l.chunks[&WorldChunkPos::new(1, 0, 0)];
    assert!(across.carve.is_empty() && across.bricks.is_empty());
    assert_eq!(across.shell, vec![([0, 30, 8], [1, 3, 2])]);
}

#[test]
fn apply_rebricks_the_fillers_a_carve_exposes() {
    // A chunk of one stone slab 30 plates thick; a pit 4 x 3 x 2 dug into its top.
    let slab = StudBrick::filler([0, 0, 0], [32, 30, 32], game::MAT_STONE);
    let b = build(BuildFile { carve: vec![CellBox { min: [0, -3, 0], size: [4, 3, 2] }], ..empty_build() });
    let l = layout_build(&b, [8, 30, 8], 0);
    let mut c = StudChunk::new([slab]).unwrap();
    let before = c.occupancy();
    let applied = l.chunks[&WorldChunkPos::new(0, 0, 0)].apply(0, &mut c);
    assert!(applied.changed && applied.carved == 4 * 3 * 2);
    // The pit is open, everything else is as solid as before.
    for cell in [[8u8, 27, 8], [11, 29, 9]] {
        assert!(c.brick_at(cell).is_none());
    }
    let mut after = c.occupancy();
    for x in 8..12 {
        for y in 27..30 {
            for z in 8..10 {
                assert!(!after.get([x, y, z]));
                after.set([x, y, z], true);
            }
        }
    }
    assert_eq!(after, before);
    // The cells behind every face of the pit are real bricks now, as a dug wall's are.
    for wall in [[7u8, 28, 8], [12, 28, 9], [9, 28, 7], [9, 28, 10], [9, 26, 8]] {
        let b = c.brick_at(wall).expect("ground behind the pit");
        assert!(matches!(b.shape, game::StudShape::Part { .. }), "{wall:?} is still a filler");
    }
    // Deep ground the pit does not touch stays a filler.
    assert!(matches!(c.brick_at([20, 5, 20]).unwrap().shape, game::StudShape::Filler { .. }));
}
