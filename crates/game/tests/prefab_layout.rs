use std::collections::BTreeMap;

use game::prefab::*;
use game::{Block, BlockId, BlockIndex, ChiselData, BLOCK_VOXELS, CHUNK_BLOCKS};

const STONE: BlockId = BlockId(2);
const OAK: BlockId = BlockId(50);
const GLASS: BlockId = BlockId(60);
const CB: i32 = CHUNK_BLOCKS as i32;

fn names(name: &str) -> Option<BlockId> {
    match name {
        "stone" => Some(STONE),
        "oak_planks" => Some(OAK),
        "glass" => Some(GLASS),
        _ => None,
    }
}

fn empty(name: &str, size: [u32; 3]) -> PrefabFile {
    PrefabFile {
        format: PREFAB_FORMAT,
        name: name.into(),
        meta: PrefabMeta::default(),
        size,
        anchor: [0, 0, 0],
        palette: vec!["stone".into(), "oak_planks".into(), "glass".into()],
        boxes: vec![],
        shaped: vec![],
        carve: vec![],
        markers: vec![],
        ground: GroundFit::Flat,
        tags: vec![],
    }
}

fn build(f: &PrefabFile) -> ResolvedPrefab {
    resolve(f, &names, Trust::Builtin).expect("resolves")
}

fn named(at: [i32; 3], shape: Shape, mat: u16) -> FileShaped {
    FileShaped { at, shape: FileShape::Named { shape, mat } }
}

fn busy() -> ResolvedPrefab {
    let mut f = empty("busy", [20, 9, 23]);
    f.anchor = [3, 1, 5];
    f.boxes.push(FileBox { min: [0, 0, 0], size: [20, 2, 23], mat: 0 });
    f.boxes.push(FileBox { min: [2, 2, 2], size: [10, 5, 12], mat: 1 });
    f.carve.push(FileCarve { min: [4, 2, 4], size: [5, 3, 6] });
    let stairs = |facing, corner| Shape::Stairs { facing, half: Half::Bottom, corner };
    f.shaped.push(named([13, 2, 3], stairs(Facing::North, StairCorner::Straight), 0));
    f.shaped.push(named([14, 2, 3], stairs(Facing::East, StairCorner::OuterLeft), 1));
    f.shaped.push(named([15, 2, 3], stairs(Facing::South, StairCorner::InnerRight), 2));
    f.shaped.push(named([16, 2, 3], stairs(Facing::West, StairCorner::OuterRight), 0));
    f.shaped.push(named(
        [17, 3, 9],
        Shape::Fence { sides: Sides { north: true, east: false, south: false, west: true } },
        1,
    ));
    f.shaped.push(FileShaped {
        at: [18, 3, 15],
        shape: FileShape::Cuboids {
            cuboids: vec![([0, 0, 0], [8, 4, 16], 0), ([8, 0, 4], [16, 9, 12], 1)],
            mats: vec![0, 2],
        },
    });
    build(&f)
}

type Entry = Option<(Block, Option<ChiselData>)>;

fn orientations() -> Vec<(u8, bool)> {
    (0..4u8).flat_map(|t| [(t, false), (t, true)]).collect()
}

/// An independent layout: every cell through `cell()` and `world_of_cell`.
fn brute(p: &ResolvedPrefab, at: &Placement) -> BTreeMap<[i32; 3], Entry> {
    let mut out = BTreeMap::new();
    for z in 0..p.size[2] as i32 {
        for y in 0..p.size[1] as i32 {
            for x in 0..p.size[0] as i32 {
                let w = world_of_cell(p, at, [x, y, z]);
                let e: Entry = match p.cell([x, y, z]) {
                    Cell::Keep => continue,
                    Cell::Carve => None,
                    Cell::Block(id) => Some((Block { id, data: [0; 3] }, None)),
                    Cell::Shaped(ResolvedShape::Named { shape, mat }) => {
                        let s = if at.mirror { shape.mirrored_x() } else { *shape };
                        Some((Block { id: *mat, data: [0; 3] }, Some(s.turned(at.turn).chisel(*mat))))
                    }
                    Cell::Shaped(ResolvedShape::Cuboids { cuboids, mats }) => {
                        let src = |vx: usize, vy: usize, vz: usize| {
                            cuboids.iter().rev().find_map(|(lo, hi, m)| {
                                let v = [vx, vy, vz];
                                (0..3).all(|a| lo[a] as usize <= v[a] && v[a] < hi[a] as usize).then_some(*m)
                            })
                        };
                        // the destination voxel d came from the source voxel s with turn(mirror(s)) = d
                        let mut table = vec![None; BLOCK_VOXELS * BLOCK_VOXELS * BLOCK_VOXELS];
                        for sz in 0..BLOCK_VOXELS {
                            for sy in 0..BLOCK_VOXELS {
                                for sx in 0..BLOCK_VOXELS {
                                    let mx = if at.mirror { BLOCK_VOXELS - 1 - sx } else { sx };
                                    let d = turn_voxel([mx, sy, sz], at.turn);
                                    table[d[0] + d[1] * BLOCK_VOXELS + d[2] * BLOCK_VOXELS * BLOCK_VOXELS] = src(sx, sy, sz);
                                }
                            }
                        }
                        let chisel = ChiselData::from_fn(mats.clone(), |x, y, z| {
                            table[x + y * BLOCK_VOXELS + z * BLOCK_VOXELS * BLOCK_VOXELS]
                        });
                        Some((Block { id: mats[0], data: [0; 3] }, Some(chisel)))
                    }
                };
                assert!(out.insert(w, e).is_none());
            }
        }
    }
    out
}

fn world_of(chunk: [i32; 3], i: BlockIndex) -> [i32; 3] {
    let (x, y, z) = i.xyz();
    [chunk[0] * CB + x as i32, chunk[1] * CB + y as i32, chunk[2] * CB + z as i32]
}

#[test]
fn chunks_reassemble_to_the_brute_force_layout() {
    let p = busy();
    for anchor in [[0, 0, 0], [-37, -5, 1000], [CB - 1, CB - 1, CB - 1]] {
        for (turn, mirror) in orientations() {
            let at = Placement { anchor, turn, mirror };
            let want = brute(&p, &at);
            let mut got: BTreeMap<[i32; 3], Entry> = BTreeMap::new();
            for chunk in chunks_touched(&p, &at) {
                let Some(stamp) = layout_chunk(&p, &at, chunk) else { continue };
                assert!(stamp.carve.windows(2).all(|w| w[0] < w[1]), "carve ascending");
                assert!(stamp.writes.windows(2).all(|w| w[0].0 < w[1].0), "writes ascending");
                for c in &stamp.carve {
                    assert!(stamp.writes.binary_search_by_key(c, |w| w.0).is_err(), "carve and writes disjoint");
                    assert!(got.insert(world_of(chunk, *c), None).is_none(), "block in two stamps");
                }
                for (i, b, ch) in &stamp.writes {
                    assert!(got.insert(world_of(chunk, *i), Some((*b, ch.clone()))).is_none(), "block in two stamps");
                }
            }
            assert_eq!(got, want, "anchor {anchor:?} turn {turn} mirror {mirror}");
        }
    }
}

#[test]
fn cell_of_world_inverts_world_of_cell() {
    let p = busy();
    for (turn, mirror) in orientations() {
        let at = Placement { anchor: [-7, 20, 33], turn, mirror };
        for cell in [[0, 0, 0], [19, 8, 22], [-5, 3, 70], [3, 1, 5], [7, -2, 11]] {
            assert_eq!(cell_of_world(&p, &at, world_of_cell(&p, &at, cell)), cell);
        }
    }
}

#[test]
fn bounds_and_chunks() {
    let mut f = empty("one", [1, 1, 1]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [1, 1, 1], mat: 0 });
    let p = build(&f);
    let at = Placement { anchor: [-1, -1, -1], turn: 0, mirror: false };
    assert_eq!(placement_bounds(&p, &at), ([-1, -1, -1], [0, 0, 0]));
    assert_eq!(chunks_touched(&p, &at), vec![[-1, -1, -1]]);
    assert!(layout_chunk(&p, &at, [-1, -1, -1]).is_some());
    assert!(layout_chunk(&p, &at, [0, 0, 0]).is_none());
    assert!(layout_chunk(&p, &at, [-2, -1, -1]).is_none());
}

fn one_stair(facing: Facing, corner: StairCorner) -> ResolvedPrefab {
    let mut f = empty("stair", [1, 1, 1]);
    f.shaped.push(named([0, 0, 0], Shape::Stairs { facing, half: Half::Bottom, corner }, 0));
    build(&f)
}

fn only_write(p: &ResolvedPrefab, at: &Placement) -> (Block, Option<ChiselData>) {
    let chunk = chunks_touched(p, at)[0];
    let stamp = layout_chunk(p, at, chunk).unwrap();
    assert_eq!(stamp.writes.len(), 1);
    let (_, b, c) = stamp.writes[0].clone();
    (b, c)
}

#[test]
fn a_turned_stair_is_the_stair_of_the_turned_facing() {
    let p = one_stair(Facing::North, StairCorner::Straight);
    let at = Placement { anchor: [0, 0, 0], turn: 1, mirror: false };
    let (b, c) = only_write(&p, &at);
    assert_eq!(b, Block { id: STONE, data: [0; 3] });
    let want = Shape::Stairs { facing: Facing::East, half: Half::Bottom, corner: StairCorner::Straight }.chisel(STONE);
    assert_eq!(c, Some(want));

    let p = one_stair(Facing::North, StairCorner::OuterLeft);
    let at = Placement { anchor: [0, 0, 0], turn: 0, mirror: true };
    let (_, c) = only_write(&p, &at);
    let want = Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::OuterRight }.chisel(STONE);
    assert_eq!(c, Some(want));
}

#[test]
fn turn_one_sends_the_north_edge_east() {
    let mut f = empty("row", [3, 1, 1]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [3, 1, 1], mat: 0 });
    let p = build(&f);
    let at = Placement { anchor: [10, 4, 10], turn: 1, mirror: false };
    let (dx, dz) = turn_xz(1, 0, 1);
    assert_eq!((dx, dz), (0, 1));
    for i in 0..3 {
        assert_eq!(world_of_cell(&p, &at, [i, 0, 0]), [10 + dx * i, 4, 10 + dz * i]);
    }
    let stamp = layout_chunk(&p, &at, [10_i32.div_euclid(CB), 4_i32.div_euclid(CB), 10_i32.div_euclid(CB)]).unwrap();
    let blocks: Vec<[i32; 3]> = stamp.writes.iter().map(|w| world_of([10_i32.div_euclid(CB), 4_i32.div_euclid(CB), 10_i32.div_euclid(CB)], w.0)).collect();
    assert_eq!(blocks, vec![[10, 4, 10], [10, 4, 11], [10, 4, 12]]);
}

#[test]
fn marker_follows_the_placement() {
    let mut f = empty("m", [4, 1, 1]);
    f.markers.push(Marker { at: [2, 0, 0], kind: "door".into(), turn: 0, props: Default::default() });
    f.markers.push(Marker { at: [2, 0, 0], kind: "door".into(), turn: 1, props: Default::default() });
    f.markers.push(Marker { at: [-60, 0, 0], kind: "far".into(), turn: 0, props: Default::default() });
    let p = build(&f);
    let at = Placement { anchor: [100, 5, 100], turn: 1, mirror: false };
    assert_eq!(marker_in_world(&p, &at, &p.markers[0]), ([100, 5, 102], 1));
    assert_eq!(marker_in_world(&p, &at, &p.markers[1]), ([100, 5, 102], 2));
    assert_eq!(marker_in_world(&p, &at, &p.markers[2]), ([100, 5, 40], 1));
    let at = Placement { anchor: [100, 5, 100], turn: 0, mirror: true };
    assert_eq!(marker_in_world(&p, &at, &p.markers[1]), ([98, 5, 100], 3));
}

#[test]
fn one_chunk_of_a_monument() {
    let mut f = empty("monument", [600, 200, 600]);
    f.boxes.push(FileBox { min: [0, 0, 0], size: [600, 200, 600], mat: 0 });
    let p = build(&f);
    let at = Placement { anchor: [0, 0, 0], turn: 0, mirror: false };
    let stamp = layout_chunk(&p, &at, [3, 2, 5]).unwrap();
    assert_eq!(stamp.writes.len(), CHUNK_BLOCKS * CHUNK_BLOCKS * CHUNK_BLOCKS);
    assert!(stamp.carve.is_empty());
}

/// Mirror, then turn, in literals. Cells (1,0,0) stone and (2,0,0) north-facing bottom OuterLeft stairs;
/// placement anchor [10, 5, 20], mirror, one clockwise turn. Mirror sends cell offset (2,0,0) to (-2,0,0),
/// one clockwise turn sends (x, z) = (-2, 0) to (0, -2): the stairs land at [10, 5, 18], the stone at
/// [10, 5, 19].
#[test]
fn mirror_then_turn_in_literals() {
    let mut f = empty("mt", [3, 1, 1]);
    f.boxes.push(FileBox { min: [1, 0, 0], size: [1, 1, 1], mat: 0 });
    f.shaped.push(named(
        [2, 0, 0],
        Shape::Stairs { facing: Facing::North, half: Half::Bottom, corner: StairCorner::OuterLeft },
        1,
    ));
    let p = build(&f);
    let at = Placement { anchor: [10, 5, 20], turn: 1, mirror: true };
    let mut got: BTreeMap<[i32; 3], Entry> = BTreeMap::new();
    for chunk in chunks_touched(&p, &at) {
        let Some(stamp) = layout_chunk(&p, &at, chunk) else { continue };
        for (i, b, c) in stamp.writes {
            got.insert(world_of(chunk, i), Some((b, c)));
        }
    }
    assert_eq!(got.len(), 2);
    assert_eq!(got[&[10, 5, 19]], Some((Block { id: STONE, data: [0; 3] }, None)));
    let (block, chisel) = got[&[10, 5, 18]].clone().unwrap();
    assert_eq!(block, Block { id: OAK, data: [0; 3] });
    let chisel = chisel.unwrap();
    // North/OuterLeft mirrored is north/OuterRight, turned once is east/OuterRight.
    let shape = Shape::Stairs { facing: Facing::East, half: Half::Bottom, corner: StairCorner::OuterRight };
    assert_eq!(chisel, shape.chisel(OAK));
    // By hand: north/OuterRight has its high quadrant at x >= half, z < half (north-east). One clockwise
    // turn takes north-east to south-east: x >= half, z >= half. The lower half is always full.
    let half = BLOCK_VOXELS / 2;
    assert_eq!(
        chisel,
        ChiselData::from_fn(vec![OAK], |x, y, z| (y < half || (x >= half && z >= half)).then_some(0))
    );
}

#[test]
fn cell_write_is_the_per_cell_transform() {
    let p = busy();
    let at = Placement { anchor: [0, 0, 0], turn: 3, mirror: true };
    assert_eq!(cell_write(Cell::Keep, &at), None);
    assert_eq!(cell_write(Cell::Carve, &at), Some(None));
    assert_eq!(cell_write(Cell::Block(STONE), &at), Some(Some((Block { id: STONE, data: [0; 3] }, None))));
    let Cell::Shaped(shape) = p.cell([14, 2, 3]) else { panic!("a stair is there") };
    let Some(Some((b, Some(c)))) = cell_write(Cell::Shaped(shape), &at) else { panic!("a shaped write") };
    let want = Shape::Stairs { facing: Facing::East, half: Half::Bottom, corner: StairCorner::OuterLeft }
        .mirrored_x()
        .turned(3);
    assert_eq!(b, Block { id: OAK, data: [0; 3] });
    assert_eq!(c, want.chisel(OAK));
}

#[test]
fn column_top_world_matches_the_layout() {
    let p = busy();
    for anchor in [[0, 0, 0], [-37, -5, 1000]] {
        for (turn, mirror) in orientations() {
            let at = Placement { anchor, turn, mirror };
            // brute force: per world column, the highest and lowest written (non-carve) block of layout_chunk
            let mut tops: BTreeMap<[i32; 2], (i32, i32)> = BTreeMap::new();
            for chunk in chunks_touched(&p, &at) {
                let Some(stamp) = layout_chunk(&p, &at, chunk) else { continue };
                for (i, _, _) in &stamp.writes {
                    let w = world_of(chunk, *i);
                    let e = tops.entry([w[0], w[2]]).or_insert((w[1], w[1]));
                    e.0 = e.0.max(w[1]);
                    e.1 = e.1.min(w[1]);
                }
            }
            let (min, max) = placement_bounds(&p, &at);
            for z in min[2] - 1..=max[2] {
                for x in min[0] - 1..=max[0] {
                    let inside = (min[0]..max[0]).contains(&x) && (min[2]..max[2]).contains(&z);
                    let top = column_top_world(&p, &at, x, z);
                    let bottom = column_bottom_world(&p, &at, x, z);
                    let Some(&(hi, lo)) = tops.get(&[x, z]) else {
                        assert_eq!((top, bottom), (None, None), "empty or outside column {x},{z}");
                        continue;
                    };
                    assert!(inside);
                    let mat = match p.cell(cell_of_world(&p, &at, [x, hi, z])) {
                        Cell::Block(id) => id,
                        Cell::Shaped(s) => s.top_material().unwrap(),
                        other => panic!("top cell {other:?}"),
                    };
                    assert_eq!(top, Some((hi, mat)), "top of {x},{z} anchor {anchor:?} turn {turn} mirror {mirror}");
                    assert_eq!(bottom, Some(lo), "bottom of {x},{z} anchor {anchor:?} turn {turn} mirror {mirror}");
                }
            }
        }
    }
}
