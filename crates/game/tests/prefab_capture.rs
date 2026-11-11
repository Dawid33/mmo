//! `capture_build`: the editor's save — the world's bricks around the anchor, minus the
//! ground they stand on, back into a Build file.

use std::collections::BTreeSet;

use crate::common::prefabs::{arch, build_file, empty_build, part, resolved};
use crate::common::{r0, region_of, uniform_seed, FLOOR_PLATES};
use game::stud_catalog::STUD_PARTS;
use game::{
    capture_build, merge_boxes, prefab_to_ron, BuildBoxFile, BuildFile, CaptureError, CellBox, GameEventKind, Marker,
    MaterialName, Order, PrefabCommand, PrefabFile, PrefabKindFile, PrefabRef, Region, StudBrick, StudChunk,
    StudChunkCoords, WorldChunkPos, PREFAB_MAX_PARTS,
};

/// Mid chunk (1, 0, 1), on the test ground's surface: the bounds stay inside region 0.
const ANCHOR: [i64; 3] = [48, FLOOR_PLATES as i64, 48];

fn region() -> Region {
    let mut r = region_of(r0(), uniform_seed());
    r.handle_event(GameEventKind::CreateClient(0)).unwrap();
    r
}

fn stamp(r: &mut Region, file: &PrefabFile) {
    let p = resolved(file);
    let key = p.key();
    for cmd in [
        PrefabCommand::Register(Box::new(p)),
        PrefabCommand::Stamp { prefab: PrefabRef::Registered(key), anchor: ANCHOR, turn: 0 },
        PrefabCommand::Deregister(key),
    ] {
        r.handle_event(GameEventKind::Prefab(0, cmd)).unwrap();
    }
}

fn coords(c: WorldChunkPos) -> Option<StudChunkCoords> {
    let n = game::REGION_CHUNKS as i64;
    ((0..n).contains(&c.x) && (0..n).contains(&c.z)).then(|| StudChunkCoords { x: c.x as usize, y: c.y, z: c.z as usize })
}

/// The chunk as the sim sees it (materialised or implied).
fn chunk_of(r: &Region) -> impl Fn(WorldChunkPos) -> Option<StudChunk> + '_ {
    |c| r.data().stud_chunk_or_pristine(coords(c)?).map(|c| c.into_owned())
}

/// The untouched ground: the same recipe everywhere.
fn pristine(c: WorldChunkPos) -> Option<StudChunk> {
    coords(c)?;
    crate::common::stud_flat(FLOOR_PLATES).pristine_chunk(c.y)
}

fn capture(r: &Region, file: &PrefabFile) -> Result<BuildFile, CaptureError> {
    let PrefabKindFile::Build(b) = &file.kind else { unreachable!() };
    capture_build(chunk_of(r), pristine, ANCHOR, b.carve.clone(), b.markers.clone())
}

fn with_build(file: &PrefabFile, build: BuildFile) -> PrefabFile {
    PrefabFile { kind: PrefabKindFile::Build(build), ..file.clone() }
}

fn stone(min: [i32; 3], size: [u32; 3]) -> BuildBoxFile {
    BuildBoxFile { min, size, material: MaterialName::Natural("stone".into()), data: 0 }
}

/// Parts, a stone box above the ground, a pit with a brick in it, a marker.
fn house() -> PrefabFile {
    build_file(
        "house",
        BuildFile {
            parts: vec![part("3001", [0, -6, 0], 0), part("3001", [0, 0, 0], 1), part("3040", [4, 0, 2], 2), part("3007", [-8, 3, -2], 0)],
            boxes: vec![stone([-2, 0, 6], [8, 6, 3])],
            carve: vec![CellBox { min: [-2, -6, -2], size: [8, 6, 6] }],
            markers: vec![Marker { at: [1, 0, 8], kind: "door".into(), turn: 1 }],
        },
    )
}

#[test]
fn stamp_then_capture_gives_the_same_file() {
    for f in [arch(), house()] {
        let mut r = region();
        stamp(&mut r, &f);
        let got = capture(&r, &f).unwrap();
        // Byte-identical: what the editor writes for a prefab opened and saved unchanged.
        assert_eq!(prefab_to_ron(&with_build(&f, got)), prefab_to_ron(&f), "{}", f.name);
    }
}

#[test]
fn natural_bricks_come_back_as_boxes() {
    let f = build_file("slab", BuildFile { boxes: vec![stone([-4, 0, -4], [8, 3, 4])], ..empty_build() });
    let mut r = region();
    stamp(&mut r, &f);
    let got = capture(&r, &f).unwrap();
    assert!(got.parts.is_empty());
    assert_eq!(got.boxes, vec![stone([-4, 0, -4], [8, 3, 4])]);
}

#[test]
fn baseplate_is_not_captured() {
    let mut r = region();
    let empty = build_file("empty", empty_build());
    let got = capture(&r, &empty).unwrap();
    assert!(got.parts.is_empty() && got.boxes.is_empty());
    // Nor is ground a dig re-bricked: a pit in the baseplate saves as nothing.
    let lo = [ANCHOR[0] as i32, FLOOR_PLATES - 9, ANCHOR[2] as i32];
    r.handle_event(GameEventKind::Order(0, Order::CarveStud { lo, hi: [lo[0] + 6, FLOOR_PLATES, lo[2] + 4] })).unwrap();
    assert!(r.data().stud_chunk_or_pristine(StudChunkCoords { x: 1, y: 0, z: 1 }).unwrap().brick_at([16, 25, 16]).is_none());
    let got = capture(&r, &empty).unwrap();
    assert!(got.parts.is_empty() && got.boxes.is_empty(), "{got:?}");
}

#[test]
fn an_unloaded_chunk_refuses_the_save() {
    let r = region();
    let none = |_: WorldChunkPos| None;
    assert!(matches!(capture_build(none, pristine, ANCHOR, Vec::new(), Vec::new()), Err(CaptureError::ChunkNotLoaded(_))));
    // One missing chunk under the prefab is enough.
    let hole = WorldChunkPos::new(1, 0, 1);
    let all_but_one = |c: WorldChunkPos| if c == hole { None } else { chunk_of(&r)(c) };
    assert_eq!(capture_build(all_but_one, pristine, ANCHOR, Vec::new(), Vec::new()), Err(CaptureError::ChunkNotLoaded(hole)));
}

#[test]
fn over_the_cap_is_refused_with_the_count() {
    // 4097 1 x 1 plates in the air chunk above the anchor.
    let plate = STUD_PARTS.iter().position(|p| p.number == "3024").unwrap() as u16;
    let above = WorldChunkPos::new(1, 1, 1);
    let plates: Vec<StudBrick> = (0..PREFAB_MAX_PARTS + 1)
        .map(|i| StudBrick::part_at([(i % 32) as u8, (i / 1024) as u8, (i / 32 % 32) as u8], plate, 0, 4))
        .collect();
    let full = StudChunk::new(plates).unwrap();
    let r = region();
    let chunk = |c: WorldChunkPos| if c == above { Some(full.clone()) } else { chunk_of(&r)(c) };
    let e = capture_build(chunk, pristine, ANCHOR, Vec::new(), Vec::new()).unwrap_err();
    assert!(matches!(e, CaptureError::TooManyParts(4097)), "{e:?}");
}

#[test]
fn a_brick_outside_the_bounds_refuses_the_save() {
    // One stud past the bounds (48 studs from the anchor), in the next chunk.
    let mut r = region();
    let p = STUD_PARTS.iter().position(|p| p.number == "3005").unwrap() as u16;
    let brick = StudBrick::part_at([0, FLOOR_PLATES as u8, 16], p, 0, 4);
    r.handle_event(GameEventKind::Order(0, Order::PlaceStudAt { chunk: WorldChunkPos::new(3, 0, 1), brick })).unwrap();
    assert_eq!(capture(&r, &build_file("x", empty_build())), Err(CaptureError::OutOfBounds));
}

#[test]
fn output_is_canonical_whatever_order_the_bricks_were_laid_in() {
    let f = house();
    let mut shuffled = f.clone();
    let PrefabKindFile::Build(b) = &mut shuffled.kind else { unreachable!() };
    b.parts.reverse();
    let (mut a, mut b) = (region(), region());
    stamp(&mut a, &f);
    stamp(&mut b, &shuffled);
    assert_eq!(capture(&a, &f), capture(&b, &f));
}

#[test]
fn merge_boxes_is_a_fixed_point() {
    // An L of cells: two boxes, and merging their cells again gives the same two.
    let mut cells = BTreeSet::new();
    for x in 0..6 {
        for z in 0..2 {
            cells.insert([x, 0, z]);
        }
    }
    for z in 2..5 {
        cells.insert([0, 0, z]);
        cells.insert([1, 0, z]);
    }
    let b = merge_boxes(cells.clone());
    assert_eq!(b.len(), 2);
    let again: BTreeSet<[i32; 3]> = b
        .iter()
        .flat_map(|&(min, size)| {
            (0..size[0]).flat_map(move |x| (0..size[1]).flat_map(move |y| (0..size[2]).map(move |z| [min[0] + x, min[1] + y, min[2] + z])))
        })
        .collect();
    assert_eq!(again, cells);
    assert_eq!(merge_boxes(again), b);
}

#[test]
fn a_brick_above_the_bounds_refuses_the_save() {
    // The bounds end 180 plates over the anchor (plate 210 here, in layer 2); a brick
    // in the layer above them is seen and refused, not left out of the file.
    let r = region();
    let p = STUD_PARTS.iter().position(|p| p.number == "3005").unwrap() as u16;
    let high = WorldChunkPos::new(1, 3, 1);
    let stray = StudChunk::new([StudBrick::part_at([16, 0, 16], p, 0, 4)]).unwrap();
    let chunk = |c: WorldChunkPos| if c == high { Some(stray.clone()) } else { chunk_of(&r)(c) };
    assert_eq!(capture_build(chunk, pristine, ANCHOR, Vec::new(), Vec::new()), Err(CaptureError::OutOfBounds));
}
