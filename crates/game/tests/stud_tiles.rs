//! Sim tiles on stud terrain (stud world plan P6a): tile fill over stud cells, the
//! nav rules, designate / dig / build / deconstruct, the fill op, break times, and a
//! body walking a move order over stud ground.
use crate::common;
use game::*;

/// Surface plate of the flat ground: the top of tile level 4, so a body stands in
/// tile level 5 on a floor at its bottom.
const TOP: i32 = 30;
const STAND: i32 = TOP / TILE_PLATES as i32;
/// Plates under this are bedrock: tile level -3 and down.
const BEDROCK: i32 = -12;
const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
const CLIENT: ClientId = 7;

fn filler(min: [u8; 3], size: [u8; 3], material: u16) -> StudBrick {
    StudBrick::filler(min, size, material)
}

fn flat() -> StudColumnGround {
    StudColumnGround::flat(TOP, SOIL, MAT_STONE, BEDROCK)
}

fn new_region() -> Region {
    let (send, _recv) = crossbeam::channel::unbounded();
    Region::new(Rollback::new(Some(send.clone())), Some(send), RegionCoords::new(0, 0), None)
}

/// A region of hand-built stud chunks (no recipe).
fn region_of(chunks: Vec<(StudChunkCoords, StudChunk)>) -> Region {
    let mut r = new_region();
    r.seed_studs(chunks);
    r
}

/// A region with the flat recipe under chunk columns `0..cols` on x and z.
fn flat_region(cols: usize) -> Region {
    let mut r = new_region();
    let ground = (0..cols).flat_map(|x| (0..cols).map(move |z| (ColumnCoords::new(x, z), flat()))).collect();
    r.seed_stud_world(StudRegionSeed { ground, features: Vec::new() });
    r
}

fn fill(r: &Region, p: TilePos) -> TileFill {
    r.data().tile_fill(p)
}

fn hash(r: &Region) -> u32 {
    state_hash(r.data())
}

fn order(r: &mut Region, o: Order) {
    r.handle_event(GameEventKind::Order(CLIENT, o)).unwrap();
}

fn designate(seq: u32, kind: DesignationKind, a: TilePos, b: TilePos) -> Order {
    Order::Designate { op: OpId { client: CLIENT, seq }, kind, priority: DEFAULT_PRIORITY, area: TileBox::new(a, b) }
}

// --- Tile fill -----------------------------------------------------------------

#[test]
fn tile_fill_classifies_hand_built_chunks() {
    let c0 = StudChunkCoords { x: 0, y: 0, z: 0 };
    let slope = stud_catalog::STUD_PARTS.iter().position(|p| !p.hull.is_empty()).expect("the catalog has a slope") as u16;
    let slope_brick = StudBrick::part_at([20, 0, 20], slope, 0, MAT_STONE);
    let chunk = StudChunk::new([
        // Tile [0,0,0]: every cell.
        filler([0, 0, 0], [2, 6, 2], MAT_STONE),
        // Tile [1,0,0]: one plate over the footprint.
        filler([2, 0, 0], [2, 1, 2], MAT_DIRT),
        // Tile [2,0,0]: half height.
        filler([4, 0, 0], [2, 3, 2], MAT_DIRT),
        // Tile [3,0,0]: one stud column to the top, the rest lower; sand is the topmost.
        filler([6, 0, 0], [1, 6, 1], MAT_SAND),
        filler([7, 0, 0], [1, 2, 1], MAT_DIRT),
        // Tile [4,0,0]: dirt under a stone cap: the topmost cell names the material.
        filler([8, 0, 0], [2, 5, 2], MAT_DIRT),
        filler([8, 5, 0], [2, 1, 2], MAT_STONE),
        // Tile [5,0,0]: a slab hanging at the tile's top (air under it).
        filler([10, 5, 0], [2, 1, 2], MAT_STONE),
        // Tile [6,0,0]: two columns at the same height: the lowest stud index wins.
        filler([13, 0, 0], [1, 4, 1], MAT_SAND),
        filler([12, 0, 1], [1, 4, 1], MAT_DIRT),
        slope_brick,
    ])
    .unwrap();
    let r = region_of(vec![(c0, chunk)]);
    let d = r.data();
    assert_eq!(fill(&r, [0, 0, 0]), TileFill::Solid);
    assert_eq!(d.tile_material([0, 0, 0]), Some(MAT_STONE));
    assert_eq!(fill(&r, [1, 0, 0]), TileFill::Floor(1));
    assert_eq!(fill(&r, [2, 0, 0]), TileFill::Floor(3));
    assert_eq!(fill(&r, [3, 0, 0]), TileFill::Floor(6), "a partial footprint reaching the top is not solid");
    assert_eq!(d.stud_tile([3, 0, 0]).tops, [6, 2, 0, 0]);
    assert_eq!(d.tile_material([3, 0, 0]), Some(MAT_SAND));
    assert_eq!(fill(&r, [4, 0, 0]), TileFill::Solid);
    assert_eq!(d.tile_material([4, 0, 0]), Some(MAT_STONE));
    assert_eq!(fill(&r, [5, 0, 0]), TileFill::Solid, "the footprint is filled to the top");
    assert_eq!(d.stud_tile([6, 0, 0]).tops, [0, 4, 4, 0]);
    assert_eq!(d.tile_material([6, 0, 0]), Some(MAT_SAND), "stud index 1 (x 1, z 0) before 2 (x 0, z 1)");
    // Open: in the chunk, above it, beside it, below it, and outside every layer.
    for p in [[9, 0, 9], [0, 1, 0], [0, 0, 1], [0, -1, 0], [40, 3, 40], [0, i32::MAX, 0], [0, i32::MIN, 0]] {
        assert_eq!(fill(&r, p), TileFill::Open, "{p:?}");
        assert_eq!(d.tile_material(p), None, "{p:?}");
    }
    // A slope part is partial however much of the tile its cell box covers, even with
    // every other cell of its tiles filled.
    let size = slope_brick.size();
    let last = [(20 + size[0] as i32 - 1) / 2, (size[1] as i32 - 1) / 6, (20 + size[2] as i32 - 1) / 2];
    let mut r = r;
    r.with_data(|d| {
        let filled = d.fill_stud_box_safe([20, 0, 20], [(last[0] + 1) * 2, (last[1] + 1) * 6, (last[2] + 1) * 2], MAT_STONE);
        assert_eq!(filled as i32, (last[0] - 9) * 2 * (last[1] + 1) * 6 * (last[2] - 9) * 2 - size.iter().map(|&v| v as i32).product::<i32>());
    });
    for x in 10..=last[0] {
        for z in 10..=last[2] {
            let t = r.data().stud_tile([x, 0, z]);
            assert!(t.partial, "tile {x},{z} holds the slope");
            assert!(matches!(t.fill(), TileFill::Floor(_)), "a slope tile is a partial floor, got {:?}", t.fill());
        }
    }
}

#[test]
fn tile_fill_reads_implied_ground_chunk_edges_and_layers() {
    let n = STUD_CHUNK_TILES as i32;
    // The recipe under chunk columns (0,0) and (1,0); a hand-built chunk one layer up
    // and one two layers down (not surface layers: only edits live there).
    let mut r = flat_region(1);
    r.with_data(|d| {
        d.seed_stud_ground_genesis(vec![(ColumnCoords::new(1, 0), flat())], Default::default());
        d.seed_stud_genesis(vec![(
            StudChunkCoords { x: 0, y: 1, z: 0 },
            // The lowest tile of layer 1 over tile column (15, 3).
            StudChunk::new([filler([30, 0, 6], [2, 6, 2], MAT_SAND)]).unwrap(),
        )]);
    });
    let d = r.data();
    assert!(d.stud_chunk_at(StudChunkCoords { x: 0, y: -1, z: 0 }).is_none(), "below the surface layer stays implied");
    // Surface tiles (materialised layer 0): grass on top, then the open air.
    assert_eq!(fill(&r, [3, STAND - 1, 3]), TileFill::Solid);
    assert_eq!(d.tile_material([3, STAND - 1, 3]), Some(MAT_GRASS));
    assert_eq!(fill(&r, [3, STAND, 3]), TileFill::Open);
    // Implied ground under the materialised band: dirt is 5 plates under the grass, so
    // level 3 (plates 18..24) is stone to the top; bedrock from level -3 down.
    for y in [3, 0, -1, -2] {
        assert_eq!(fill(&r, [3, y, 3]), TileFill::Solid, "level {y}");
        assert_eq!(d.tile_material([3, y, 3]), Some(MAT_STONE), "level {y}");
        assert!(!d.is_bedrock([3, y, 3]));
    }
    assert_eq!(d.tile_material([3, -3, 3]), Some(MAT_BEDROCK));
    assert!(d.is_bedrock([3, -3, 3]));
    // Across the chunk edge on x: the last tile of chunk 0 and the first of chunk 1.
    assert_eq!(fill(&r, [n - 1, STAND - 1, 0]), TileFill::Solid);
    assert_eq!(fill(&r, [n, STAND - 1, 0]), TileFill::Solid);
    assert_eq!(fill(&r, [2 * n, STAND - 1, 0]), TileFill::Open, "no recipe under chunk column 2");
    // Across the layer boundary: level 14 is the top of layer 0, level 15 the bottom of
    // layer 1 (the hand-built chunk); level -1 is the top tile of layer -1.
    let l = STUD_LAYER_TILES as i32;
    assert_eq!(fill(&r, [15, l - 1, 3]), TileFill::Open);
    assert_eq!(fill(&r, [15, l, 3]), TileFill::Solid);
    assert_eq!(d.tile_material([15, l, 3]), Some(MAT_SAND));
    assert_eq!(fill(&r, [15, l + 1, 3]), TileFill::Open);
    assert_eq!(d.column_band([15, 3]), Some((0, 2 * l - 1)), "layers 0 and 1");
    assert_eq!(d.column_band([n, 3]), Some((0, l - 1)), "the surface layer");
    assert_eq!(d.column_band([2 * n, 3]), None);
}

#[test]
fn a_recipe_surface_inside_a_tile_is_a_floor() {
    let mut r = new_region();
    // Stud columns step up one plate per stud along x from plate 24.
    let n = STUD_CHUNK_STUDS;
    let surface: Vec<i32> = (0..n * n).map(|i| 24 + (i % n) as i32).collect();
    let g = StudColumnGround::new(
        surface.into(),
        vec![STUD_NO_WATER; n * n].into(),
        vec![0; n * n].into(),
        vec![SOIL],
        vec![0; n * n].into(),
        vec![(i32::MAX, MAT_STONE)],
        BEDROCK,
    )
    .unwrap();
    r.seed_stud_world(StudRegionSeed { ground: vec![(ColumnCoords::new(0, 0), g.clone())], features: Vec::new() });
    // The materialised chunk and the bare recipe agree on every tile.
    let chunk = r.data().stud_chunk_at(StudChunkCoords { x: 0, y: 0, z: 0 }).unwrap().clone();
    for ty in 0..STUD_LAYER_TILES {
        for tx in 0..STUD_CHUNK_TILES {
            let (a, b) = (StudTile::of_chunk(&chunk, [tx, ty, 2]), StudTile::of_ground(&g, 0, [tx, ty, 2]));
            assert_eq!(a.tops, b.tops, "tile {tx},{ty}");
            assert_eq!(a.fill(), b.fill());
            assert_eq!(a.material(), b.material());
        }
    }
    // Tile x holds stud columns 2x and 2x+1: surfaces 24+2x and 25+2x.
    assert_eq!(r.data().stud_tile([0, 4, 2]).tops, [0, 1, 0, 1]);
    assert_eq!(fill(&r, [0, 4, 2]), TileFill::Floor(1));
    assert_eq!(fill(&r, [2, 4, 2]), TileFill::Floor(5));
    assert_eq!(r.data().stud_tile([3, 4, 2]).tops, [6, 6, 6, 6]);
    assert_eq!(r.data().stud_tile([3, 5, 2]).tops, [0, 1, 0, 1]);
}

#[test]
fn the_memoising_view_reads_what_the_region_reads() {
    let mut r = flat_region(1);
    r.with_data(|d| {
        assert!(!d.carve_stud_safe([3, 20, 3], [9, 29, 7]).is_empty());
        assert!(d.fill_stud_box_safe([12, 30, 12], [15, 34, 14], MAT_SAND) > 0);
    });
    let d = r.data();
    let view = d.tiles();
    assert_eq!(view.terrain(), TerrainKind::Stud);
    for y in -4..8 {
        for z in -1..9 {
            for x in -1..9 {
                let p = [x, y, z];
                assert_eq!(view.stud_tile(p), d.stud_tile(p), "{p:?}");
                assert_eq!(view.tile_fill(p), d.tile_fill(p));
                assert_eq!(view.tile_material(p), d.tile_material(p));
                assert_eq!(view.is_bedrock(p), d.is_bedrock(p));
            }
        }
    }
}

// --- Nav rules -----------------------------------------------------------------

/// Stud tiles from a function.
struct Src<F: Fn(TilePos) -> TileFill>(F);

impl<F: Fn(TilePos) -> TileFill> TileSource for Src<F> {
    fn block(&self, _p: TilePos) -> BlockId {
        BlockId::AIR
    }
    fn is_bedrock(&self, _p: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _p: TilePos) -> bool {
        false
    }
    fn tree_id(&self, _p: TilePos) -> u32 {
        0
    }
    fn tree_limb(&self, _p: TilePos) -> u8 {
        0
    }
    fn terrain(&self) -> TerrainKind {
        TerrainKind::Stud
    }
    fn tile_fill(&self, p: TilePos) -> TileFill {
        (self.0)(p)
    }
}

/// Solid ground up to level 0, open above, with `over` deciding levels 1 and up.
fn ground(over: impl Fn(TilePos) -> TileFill) -> Src<impl Fn(TilePos) -> TileFill> {
    Src(move |p: TilePos| if p[1] <= 0 { TileFill::Solid } else { over(p) })
}

const BUDGET: u32 = 4096;

#[test]
fn standable_follows_floor_height_and_headroom() {
    use TileFill::{Floor, Open, Solid};
    let at = [0, 1, 0];
    let with = |fills: &'static [(i32, TileFill)]| {
        ground(move |p| if (p[0], p[2]) == (0, 0) { fills.iter().find(|(y, _)| *y == p[1]).map_or(Open, |f| f.1) } else { Open })
    };
    // On solid ground: two open tiles (a 2.2 m body under a 2.4 m ceiling).
    assert!(standable(&with(&[]), at));
    assert!(standable(&with(&[(3, Solid)]), at), "a ceiling two tiles up");
    assert!(!standable(&with(&[(2, Solid)]), at), "a ceiling one tile up");
    assert!(!standable(&with(&[(2, Floor(1))]), at), "anything in the head tile blocks");
    assert!(!standable(&with(&[]), [0, 2, 0]), "open over open");
    assert!(!standable(&with(&[]), [0, 0, 0]), "solid");
    // On a low floor in the tile itself: up to the step height (3 plates).
    for k in 1..=3u8 {
        let src = ground(move |p| if p == [0, 1, 0] { Floor(k) } else { Open });
        assert!(standable(&src, at), "floor {k}");
        assert!(!standable(&src, [0, 2, 0]), "a low floor belongs to its own tile");
        assert_eq!(stand_offset_units(&src, at), plate_units(k as i32));
    }
    // A floor 1 plate up fits under a ceiling two tiles up; 2 and 3 plates need the
    // tile two above open as well.
    assert!(standable(&with(&[(1, Floor(1)), (3, Solid)]), at));
    assert!(!standable(&with(&[(1, Floor(2)), (3, Solid)]), at));
    assert!(!standable(&with(&[(1, Floor(3)), (3, Solid)]), at));
    assert!(standable(&with(&[(1, Floor(3)), (4, Solid)]), at));
    // A floor higher than a step: the body stands in the tile above, on it.
    for k in 4..=6u8 {
        let src = ground(move |p| if p == [0, 1, 0] { Floor(k) } else { Open });
        assert!(!standable(&src, at), "floor {k} is too high to stand in");
        assert!(standable(&src, [0, 2, 0]), "floor {k} carries the tile above");
        assert_eq!(stand_offset_units(&src, [0, 2, 0]), plate_units(k as i32 - 6));
    }
    // Every floor height has exactly one feet tile, and `feet_tile` finds it from feet
    // on the bare floor or on its stud caps.
    for surface in 0..24 {
        let (tile, k) = (surface / 6, surface % 6);
        let level = if k <= 3 { tile } else { tile + 1 };
        for lift in [0.0, stud_cap_units(), stud_cap_units() + 0.3] {
            let feet = rapier3d::math::Vector::new(4.0, plate_units(surface) + lift, 4.0);
            assert_eq!(TerrainKind::Stud.feet_tile(feet), [0, level, 0], "surface plate {surface}, lift {lift}");
            assert!(TerrainKind::Stud.on_level(level, feet.y));
            assert!(!TerrainKind::Stud.on_level(level + 1, feet.y) && !TerrainKind::Stud.on_level(level - 1, feet.y));
            assert_eq!(TerrainKind::feet_plate(feet.y), surface as i64);
        }
    }
}

#[test]
fn paths_run_straight_and_around_a_wall() {
    let open = ground(|_| TileFill::Open);
    let path = find_path(&open, [0, 1, 0], [5, 1, 0], BUDGET).unwrap();
    assert_eq!(path, (0..=5).map(|x| [x, 1, 0]).collect::<Vec<_>>());
    assert_eq!(path_cost(&open, &path), Some(5 * STEP_COST));
    assert!(walk_ups(&open, &path).iter().skip(1).all(|&w| w), "stud steps are walked, never jumped");

    // A wall two tiles high across x = 3 for |z| <= 2.
    let wall = |p: TilePos| p[0] == 3 && p[2].abs() <= 2 && p[1] <= 2;
    let walled = ground(move |p| if wall(p) { TileFill::Solid } else { TileFill::Open });
    let path = find_path(&walled, [0, 1, 0], [5, 1, 0], BUDGET).unwrap();
    assert!(path.iter().all(|&p| !wall(p) && p[1] == 1), "{path:?}");
    assert!(path.iter().any(|p| p[2].abs() == 3), "it goes round an end of the wall: {path:?}");
    assert!(path_cost(&walled, &path).unwrap() > 5 * STEP_COST);
    // A diagonal step may not cut the wall's corner.
    assert_eq!(path_cost(&walled, &[[2, 1, 3], [3, 1, 4]]), Some(DIAG_COST));
    assert_eq!(path_cost(&walled, &[[2, 1, 2], [3, 1, 3]]), None, "the wall's end tile is beside the step");
}

#[test]
fn paths_climb_a_brick_step_but_not_a_tile_wall() {
    use TileFill::{Floor, Open, Solid};
    // x >= 3 is one brick (3 plates) higher: a floor in the level-1 tiles.
    let step = ground(|p| if p[0] >= 3 && p[1] == 1 { Floor(3) } else { Open });
    let path = find_path(&step, [0, 1, 0], [5, 1, 0], BUDGET).unwrap();
    assert_eq!(path.len(), 6);
    assert_eq!(path_cost(&step, &path), Some(5 * STEP_COST), "a step up costs no climb");
    assert_eq!(path_cost(&step, &[[3, 1, 0], [2, 1, 0]]), Some(STEP_COST), "nor the step down");
    // x >= 3 is a whole tile (6 plates) higher: a wall.
    let wall = ground(|p| if p[0] >= 3 && p[1] == 1 { Solid } else { Open });
    assert!(standable(&wall, [5, 2, 0]));
    assert_eq!(find_path(&wall, [0, 1, 0], [5, 2, 0], BUDGET), None);
    // ... but walked off the other way: a one-tile drop, charged a climb.
    assert_eq!(path_cost(&wall, &[[3, 2, 0], [2, 1, 0]]), Some(STEP_COST + CLIMB_COST));
    // Two brick steps make the tile: a stair.
    let stair = ground(|p| match (p[0], p[1]) {
        (3, 1) => Floor(3),
        (x, 1) if x >= 4 => Solid,
        _ => Open,
    });
    let path = find_path(&stair, [0, 1, 0], [5, 2, 0], BUDGET).unwrap();
    assert_eq!(path, vec![[0, 1, 0], [1, 1, 0], [2, 1, 0], [3, 1, 0], [4, 2, 0], [5, 2, 0]]);
    // A 4-plate rise is over the step height; a 4-plate floor is stood on from above.
    let high = ground(|p| if p[0] >= 3 && p[1] == 1 { Floor(4) } else { Open });
    assert!(standable(&high, [3, 2, 0]));
    assert_eq!(find_path(&high, [0, 1, 0], [3, 2, 0], BUDGET), None);
    // A drop of two tiles is not a step of the path.
    let cliff = ground(|p| if p[0] >= 3 && p[1] <= 2 { Solid } else { Open });
    assert_eq!(find_path(&cliff, [5, 3, 0], [0, 1, 0], BUDGET), None);
}

#[test]
fn paths_need_a_corridor_one_tile_wide_and_two_high() {
    use TileFill::{Open, Solid};
    // Rock everywhere above the ground but a corridor along x at z = 0, two tiles high.
    let corridor = ground(|p| if p[2] == 0 && (1..=2).contains(&p[1]) { Open } else { Solid });
    let path = find_path(&corridor, [0, 1, 0], [6, 1, 0], BUDGET).unwrap();
    assert_eq!(path, (0..=6).map(|x| [x, 1, 0]).collect::<Vec<_>>());
    // The same with the ceiling one tile lower at x = 3: a one-tile-high gap.
    let gap = ground(|p| if p[2] == 0 && (1..=2).contains(&p[1]) && p != [3, 2, 0] { Open } else { Solid });
    assert_eq!(find_path(&gap, [0, 1, 0], [6, 1, 0], BUDGET), None);
    // A brick step inside a two-tile corridor leaves no room for the body on it.
    let stepped = ground(|p| match p {
        [3, 1, 0] => TileFill::Floor(3),
        _ if p[2] == 0 && (1..=2).contains(&p[1]) => Open,
        _ => Solid,
    });
    assert_eq!(find_path(&stepped, [0, 1, 0], [6, 1, 0], BUDGET), None);
    // ... and stepping up under a ceiling needs the room at the higher floor in the
    // lower column too.
    let low = ground(|p| match p {
        [x, 1, _] if x >= 3 => TileFill::Floor(3),
        [2, 3, 0] => Solid,
        _ => Open,
    });
    assert!(standable(&low, [2, 1, 0]) && standable(&low, [3, 1, 0]));
    assert_eq!(path_cost(&low, &[[2, 1, 0], [3, 1, 0]]), None);
    assert_eq!(path_cost(&low, &[[2, 1, 1], [3, 1, 1]]), Some(STEP_COST));
}

// --- Orders --------------------------------------------------------------------

#[test]
fn dig_order_opens_the_tile_and_rolls_back() {
    let mut r = flat_region(1);
    let (a, b) = ([4, STAND - 1, 4], [5, STAND - 1, 4]);
    let h0 = hash(&r);
    order(&mut r, designate(1, DesignationKind::Dig, a, b));
    assert_eq!(r.data().designations().len(), 2);
    let h1 = hash(&r);
    order(&mut r, Order::DevCompleteAll);
    for p in [a, b] {
        assert_eq!(fill(&r, p), TileFill::Open, "{p:?} dug");
    }
    // Neighbours on every side, and the tile under each, are intact.
    for p in [[3, STAND - 1, 4], [6, STAND - 1, 4], [4, STAND - 1, 3], [4, STAND - 1, 5], [4, STAND - 2, 4], [5, STAND - 2, 4]] {
        assert_eq!(fill(&r, p), TileFill::Solid, "{p:?} intact");
    }
    assert!(r.data().designations().is_empty() && r.data().constructed().is_empty());
    assert!(standable(r.data(), [4, STAND - 1, 4]), "the dug tile is walked into");
    // Exactly the two tiles' cells are gone from the chunk.
    let mut want = flat().pristine_chunk(0).unwrap();
    let removed: u32 =
        [8, 10].iter().flat_map(|&x| want.carve([x, 24, 8], [x + 2, 30, 10])).map(|(_, n)| n).sum();
    assert_eq!(removed, 2 * STUD_TILE_CELLS);
    assert!(r.data().stud_chunk_at(StudChunkCoords { x: 0, y: 0, z: 0 }) == Some(&want));
    r.rollback_last_event();
    assert_eq!(hash(&r), h1, "the completed dig undone");
    assert_eq!(fill(&r, a), TileFill::Solid);
    r.rollback_last_event();
    assert_eq!(hash(&r), h0, "the designation undone");
}

#[test]
fn digging_down_materialises_implied_ground_and_stops_at_bedrock() {
    let mut r = flat_region(1);
    let below = StudChunkCoords { x: 0, y: -1, z: 0 };
    assert!(r.data().stud_chunk_at(below).is_none());
    // A shaft from the surface through layer 0 into layer -1, down into the bedrock.
    order(&mut r, designate(1, DesignationKind::Dig, [4, STAND - 1, 4], [4, -4, 4]));
    let designated: Vec<i32> = r.data().designations().keys().map(|p| p[1]).collect();
    assert_eq!(designated, (-2..STAND).collect::<Vec<_>>(), "bedrock (levels -3, -4) refuses the designation");
    let summary = summarize(r.data(), DesignationKind::Dig, TileBox::new([4, STAND + 1, 4], [4, -4, 4]));
    assert_eq!(summary.valid, (STAND + 2) as u64);
    assert_eq!(summary.invalid.get(&Invalid::Bedrock), Some(&2));
    assert_eq!(summary.invalid.get(&Invalid::NothingToDig), Some(&2));
    let h1 = hash(&r);
    order(&mut r, Order::DevCompleteAll);
    for y in -2..STAND {
        assert_eq!(fill(&r, [4, y, 4]), TileFill::Open, "level {y}");
    }
    assert_eq!(fill(&r, [4, -3, 4]), TileFill::Solid, "bedrock stays");
    assert_eq!(fill(&r, [5, -1, 4]), TileFill::Solid, "the shaft's wall in the layer below");
    let chunk = r.data().stud_chunk_at(below).expect("the layer under the surface is materialised");
    assert!(chunk.brick_at([8, 89, 8]).is_none() && chunk.brick_at([10, 89, 8]).is_some());
    r.rollback_last_event();
    assert_eq!(hash(&r), h1);
    assert!(r.data().stud_chunk_at(below).is_none(), "the materialised chunk is undone");
    // A bedrock tile cannot be dug even when asked directly.
    r.with_data(|d| assert!(d.carve_stud_safe([8, -18, 8], [10, -12, 10]).is_empty()));
}

#[test]
fn build_fills_the_tile_and_deconstruct_opens_it() {
    common::install_real_block_props();
    let stone = block_props().id_named("stone").unwrap();
    let water = block_props().id_named("water").unwrap();
    assert_eq!(stud_material_of_block(stone), MAT_STONE);
    assert_eq!(stud_material_of_block(water), STUD_BUILD_FALLBACK_MATERIAL, "no stud material is called water");
    let mut r = flat_region(1);
    let (air, half, plastic) = ([8, STAND, 8], [10, STAND - 1, 8], [12, STAND, 8]);
    // `half`: the upper 3 plates of a surface tile carved away.
    r.with_data(|d| assert!(!d.carve_stud_safe([20, TOP - 3, 16], [22, TOP, 18]).is_empty()));
    assert_eq!(fill(&r, half), TileFill::Floor(3));
    let h0 = hash(&r);
    order(&mut r, designate(1, DesignationKind::Build(stone), air, half));
    order(&mut r, designate(2, DesignationKind::Build(water), plastic, plastic));
    // Air builds nothing; a solid tile takes no build.
    order(&mut r, designate(3, DesignationKind::Build(BlockId::AIR), [14, STAND, 8], [14, STAND, 8]));
    order(&mut r, designate(4, DesignationKind::Build(stone), [14, STAND - 1, 8], [14, STAND - 1, 8]));
    let tiles: Vec<TilePos> = r.data().designations().keys().copied().collect();
    assert_eq!(tiles.len(), 5, "the box from `air` to `half` minus its solid ground tiles, plus `plastic`: {tiles:?}");
    let h1 = hash(&r);
    order(&mut r, Order::DevCompleteAll);
    for p in [air, half, plastic] {
        assert_eq!(fill(&r, p), TileFill::Solid, "{p:?} built");
        assert!(r.data().is_constructed(p));
    }
    assert_eq!(r.data().tile_material(air), Some(MAT_STONE));
    assert_eq!(r.data().tile_material(half), Some(MAT_STONE), "stone over the dirt that was left");
    assert_eq!(r.data().stud_material_at([20, TOP - 4, 16]), Some(MAT_DIRT), "the cells already there are kept");
    assert_eq!(r.data().tile_material(plastic), Some(STUD_BUILD_FALLBACK_MATERIAL));
    assert_eq!(r.data().constructed().len(), 5);
    // The build is real parts, not fillers: an open tile is two 2 x 2 bricks.
    let chunk = r.data().stud_chunk_at(StudChunkCoords { x: 0, y: 0, z: 0 }).unwrap();
    let built: Vec<&StudBrick> = chunk.bricks().filter(|b| b.min[0] == 16 && b.min[2] == 16 && b.min[1] >= TOP as u8).collect();
    assert_eq!(built.len(), 2);
    assert!(built.iter().all(|b| matches!(b.shape, StudShape::Part { .. }) && b.size() == [2, 3, 2]));
    let h2 = hash(&r);
    // A built tile is deconstructed, not dug.
    assert_eq!(validate(r.data(), DesignationKind::Dig, air), Err(Invalid::Built));
    assert_eq!(validate(r.data(), DesignationKind::Deconstruct, [3, STAND - 1, 3]), Err(Invalid::NotBuilt));
    assert_eq!(validate(r.data(), DesignationKind::Build(stone), air), Err(Invalid::Occupied));
    assert_eq!(validate(r.data(), DesignationKind::Chop, air), Err(Invalid::NotTree));
    order(&mut r, designate(5, DesignationKind::Deconstruct, air, plastic));
    order(&mut r, Order::DevCompleteAll);
    for p in [air, plastic] {
        assert_eq!(fill(&r, p), TileFill::Open, "{p:?} deconstructed");
        assert!(!r.data().is_constructed(p));
    }
    assert!(r.data().is_constructed(half), "outside the deconstruct box");
    assert_eq!(fill(&r, [8, STAND - 1, 8]), TileFill::Solid, "the ground under the build is untouched");
    // Rollback bar over the whole sequence.
    r.rollback_last_event();
    r.rollback_last_event();
    assert_eq!(hash(&r), h2);
    r.rollback_last_event();
    assert_eq!(hash(&r), h1);
    for _ in 0..4 {
        r.rollback_last_event();
    }
    assert_eq!(hash(&r), h0);
}

#[test]
fn a_replica_replaying_the_orders_matches() {
    common::install_real_block_props();
    let script = [
        designate(1, DesignationKind::Dig, [2, STAND - 1, 2], [6, STAND - 3, 3]),
        designate(2, DesignationKind::Build(BlockId(2)), [8, STAND, 2], [9, STAND + 1, 2]),
        Order::DevCompleteAll,
        designate(3, DesignationKind::Deconstruct, [8, STAND, 2], [8, STAND + 1, 2]),
        Order::Erase { area: TileBox::new([8, STAND + 1, 2], [8, STAND + 1, 2]), only: None },
        Order::DevCompleteAll,
    ];
    let mut server = flat_region(1);
    for o in script {
        order(&mut server, o);
        server.commit_server_event();
    }
    // The predicting copy applies them, rolls everything back, replays.
    let mut client = flat_region(1);
    let h0 = hash(&client);
    for o in script {
        order(&mut client, o);
    }
    assert_eq!(hash(&client), hash(&server));
    for _ in script {
        client.rollback_last_event();
    }
    assert_eq!(hash(&client), h0);
    for o in script {
        order(&mut client, o);
    }
    assert_eq!(hash(&client), hash(&server));
    assert_eq!(fill(&server, [8, STAND, 2]), TileFill::Open);
    assert_eq!(fill(&server, [8, STAND + 1, 2]), TileFill::Solid, "its deconstruction was erased");
}

// --- The fill op ---------------------------------------------------------------

#[test]
fn fill_box_skips_occupied_cells_across_chunks_and_layers() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    let ground = (0..2).map(|x| (ColumnCoords::new(x, 0), flat())).collect();
    r.seed_stud_ground_genesis(ground, Default::default());
    r.set_body_scale_raw(movement::BodyScale::STUD);
    let (cw, ch) = (STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32);
    let h0 = state_hash(&r);
    // A box over the edge between chunk columns 0 and 1, from 2 plates under the
    // surface up through the layer boundary: the ground's cells stay.
    let (lo, hi) = ([cw - 3, TOP - 2, 4], [cw + 3, ch + 4, 7]);
    r.new_transaction();
    assert_eq!(r.fill_stud_box_safe(lo, hi, MAT_SAND), (6 * 3 * (ch + 4 - TOP)) as u32);
    for x in lo[0]..hi[0] {
        for z in lo[2]..hi[2] {
            for y in lo[1]..hi[1] {
                // (A ground part spanning grass and dirt reads as its top plate's grass.)
                let got = r.stud_material_at([x, y, z]);
                assert_eq!(got == Some(MAT_SAND), y >= TOP, "{x},{y},{z}: {got:?}");
                assert!(got.is_some());
            }
        }
    }
    assert_eq!(r.stud_material_at([lo[0] - 1, TOP, 4]), None);
    assert_eq!(r.stud_material_at([lo[0], hi[1], 4]), None);
    for c in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let chunk = r.stud_chunk_at(StudChunkCoords { x: c.0, y: c.1, z: 0 }).expect("all four chunks hold bricks");
        assert!(StudChunk::new(chunk.bricks().copied()).as_ref() == Ok(chunk), "valid and disjoint");
    }
    // Filling it again changes nothing; neither does an unknown material or an empty,
    // oversized or out-of-region box.
    let h1 = state_hash(&r);
    assert_eq!(r.fill_stud_box_safe(lo, hi, MAT_STONE), 0);
    assert_eq!(r.fill_stud_box_safe([0, 40, 0], [2, 46, 2], 0x7fff), 0);
    assert_eq!(r.fill_stud_box_safe([0, 40, 0], [0, 46, 2], MAT_STONE), 0);
    assert_eq!(r.fill_stud_box_safe([0, 40, 0], [40, 46, 2], MAT_STONE), 0);
    assert_eq!(r.fill_stud_box_safe([-4, 40, 0], [0, 46, 2], MAT_STONE), 0);
    assert_eq!(state_hash(&r), h1);
    r.rollback();
    assert_eq!(state_hash(&r), h0);
    assert!(r.stud_chunk_at(StudChunkCoords { x: 0, y: 1, z: 0 }).is_none());
}

#[test]
fn fill_order_fills_and_rolls_back() {
    let mut r = flat_region(1);
    let h0 = hash(&r);
    let fill_box = Order::FillStud { lo: [4, TOP, 4], hi: [8, TOP + 6, 6], material: MAT_SAND };
    order(&mut r, fill_box);
    assert_eq!(fill(&r, [2, STAND, 2]), TileFill::Solid);
    assert_eq!(r.data().tile_material([3, STAND, 2]), Some(MAT_SAND));
    let h1 = hash(&r);
    // Nothing left to fill, an unknown material, an inverted box: no change but the event.
    let bad = [
        fill_box,
        Order::FillStud { lo: [4, 40, 4], hi: [8, 46, 6], material: 0x7fff },
        Order::FillStud { lo: [8, 40, 4], hi: [4, 46, 6], material: MAT_SAND },
    ];
    let chunk = r.data().stud_chunk_at(StudChunkCoords { x: 0, y: 0, z: 0 }).unwrap().clone();
    for o in bad {
        order(&mut r, o);
        assert!(r.data().stud_chunk_at(StudChunkCoords { x: 0, y: 0, z: 0 }) == Some(&chunk), "{o:?} filled something");
        r.rollback_last_event();
        assert_eq!(hash(&r), h1);
    }
    r.rollback_last_event();
    assert_eq!(hash(&r), h0);
    assert_eq!(fill(&r, [2, STAND, 2]), TileFill::Open);
}

// --- Break time and yield ------------------------------------------------------

#[test]
fn break_time_and_yield_follow_the_material() {
    common::install_real_block_props();
    let props = block_props();
    let voxel = |name: &str| break_ticks(props, Block::new(props.id_named(name).unwrap()));
    // A whole tile: stone as long as a stone block, dirt close to a dirt block.
    assert_eq!(stud_break_ticks(MAT_STONE, STUD_TILE_CELLS), Some(voxel("stone")));
    assert_eq!((stud_break_ticks(MAT_DIRT, STUD_TILE_CELLS), voxel("dirt")), (Some(12), 15));
    assert_eq!(stud_break_ticks(MAT_STONE, STUD_TILE_CELLS / 2), Some(23), "half a tile, rounded up");
    assert_eq!(stud_break_ticks(MAT_SAND, 1), Some(1));
    assert_eq!(stud_break_ticks(MAT_BEDROCK, STUD_TILE_CELLS), None);
    assert_eq!(stud_break_ticks(0x7fff, 1), None);
    // A whole tile of the flat ground: grass over dirt reads as grass (hardness 2).
    let r = flat_region(1);
    assert_eq!(r.data().stud_tile_break_ticks([3, STAND - 1, 3]), stud_break_ticks(MAT_GRASS, STUD_TILE_CELLS));
    assert_eq!(r.data().stud_tile_break_ticks([3, 0, 3]), Some(45));
    assert_eq!(r.data().stud_tile_break_ticks([3, STAND, 3]), None, "open");
    assert_eq!(r.data().stud_tile_break_ticks([3, -3, 3]), None, "bedrock");
    // The yield is the same-named block's drop, one per tile's worth of cells.
    let item = |name: &str| ItemId::block(props.id_named(name).unwrap());
    let drops = stud_drops(&[
        (STUD_BUILD_FALLBACK_MATERIAL, 24),
        (MAT_GRASS, 24),
        (MAT_DIRT, 11),
        (MAT_STONE, 36),
        (MAT_SAND, 60),
    ]);
    assert_eq!(
        drops,
        vec![
            // Plastic has no block; grass drops dirt; 11 dirt cells round to none.
            ItemStack { item: item("dirt"), count: 1 },
            ItemStack { item: item("stone"), count: 2 },
            ItemStack { item: item("sand"), count: 3 },
        ]
    );
}

// --- Bodies --------------------------------------------------------------------

fn tick(r: &mut Region) {
    r.handle_event(GameEventKind::Tick).unwrap();
    r.forget_last_event();
}

fn body_feet(r: &Region, e: EntityKey) -> rapier3d::math::Vector {
    let d = r.data();
    let handle = d.ecs.rigidbody.try_get(e).unwrap();
    d.feet_of(d.physics.bodies.get(handle).unwrap().translation())
}

fn body_tile(r: &Region, e: EntityKey) -> TilePos {
    TerrainKind::Stud.feet_tile(body_feet(r, e))
}

/// Ticks until `e`'s walk is over (at most `max`).
fn walk_until_arrived(r: &mut Region, e: EntityKey, max: u32) {
    for _ in 0..max {
        tick(r);
        if r.data().ecs.walker.try_get(e).is_none() {
            return;
        }
    }
    panic!("still walking after {max} ticks, body on {:?}", body_tile(r, e));
}

fn walk_budget(metres: f32) -> u32 {
    (metres / movement::WALK_SPEED * 1.5 * 1000.0 / TICK_RATE as f32).ceil() as u32 + 20
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn bodies_walk_a_move_order_over_stud_ground() {
    let mut r = flat_region(REGION_CHUNKS);
    assert_eq!(r.data().terrain_kind(), TerrainKind::Stud);
    // A brick-high terrace over x >= 12 (tiles), and a wall with one doorway on the way.
    r.with_data(|d| {
        assert!(d.fill_stud_box_safe([24, TOP, 0], [48, TOP + 3, 32], MAT_STONE) > 0);
        assert!(d.fill_stud_box_safe([16, TOP, 0], [18, TOP + 12, 20], MAT_STONE) > 0);
    });
    r.handle_event(GameEventKind::CreateClient(CLIENT)).unwrap();
    r.forget_last_event();
    let e = *r.data().player_entites.get(&CLIENT).unwrap();
    let start = [4, STAND, 4];
    r.handle_event(GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(start) })).unwrap();
    r.forget_last_event();
    for _ in 0..10 {
        tick(&mut r);
    }
    assert_eq!(body_tile(&r, e), start, "the body is put on the stud floor");
    assert!(r.data().ecs.motion.try_get(e).as_ref().is_some_and(|m| m.grounded));
    let goal = Goal::local(RegionCoords::new(0, 0), [20, 4], None);
    r.handle_event(GameEventKind::Order(CLIENT, Order::MoveTo(goal))).unwrap();
    r.forget_last_event();
    let w = r.data().ecs.walker.try_get(e).clone().expect("MoveTo finds a path over the studs");
    assert!(w.to_goal);
    let here = RegionCoords::new(0, 0);
    let path: Vec<TilePos> = (0..w.path.len() as u32).map(|i| w.waypoint(i, here).unwrap()).collect();
    assert!(path.iter().any(|p| p[2] >= 10), "round the wall's end: {path:?}");
    assert_eq!(path.last(), Some(&[20, STAND, 4]), "onto the terrace: the same level, a floor 3 plates up");
    walk_until_arrived(&mut r, e, walk_budget(40.0));
    assert_eq!(body_tile(&r, e), [20, STAND, 4]);
    let feet = body_feet(&r, e).y;
    assert!((feet - plate_units(TOP + 3)).abs() < plate_units(1), "standing on the terrace, feet at {feet}");

    // A person takes the same walk on the same body.
    let person = history::PersonId(3);
    let xz = TerrainKind::Stud.column_centre([4, 6]);
    let mut p = None;
    r.with_data(|d| {
        p = d.spawn_person_safe(person, xz);
        assert!(d.walk_entity_safe(p.unwrap(), goal, here, None));
    });
    let p = p.unwrap();
    walk_until_arrived(&mut r, p, walk_budget(40.0) + 40);
    assert_eq!(body_tile(&r, p)[1], STAND);
    let at = body_tile(&r, p);
    assert!((at[0] - 20).abs() <= 1 && (at[2] - 4).abs() <= 1, "the person arrived beside the player, on {at:?}");
}

/// Pathfinding plans only moves the body can make: a stair of six brick-high (3-plate,
/// `STUD_STEP_PLATES`) risers on one-tile treads is planned as plain walking, and real
/// bodies starting from different tiles (so meeting each riser at different points of a
/// tick) climb it at walking pace — never stalled long enough for the stalled-walk
/// jump, never rising faster than a step (`movement::STUD_EDGE_HOLD_ANGLE`).
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn bodies_walk_up_a_brick_stair_without_jumping() {
    const RISERS: i32 = 6;
    let mut r = flat_region(REGION_CHUNKS);
    r.with_data(|d| {
        for i in 0..RISERS {
            let x = 24 + 2 * i;
            let hi_x = if i + 1 == RISERS { 64 } else { x + 2 };
            assert!(d.fill_stud_box_safe([x, TOP, 0], [hi_x, TOP + 3 * (i + 1), 32], MAT_STONE) > 0);
        }
    });
    let here = RegionCoords::new(0, 0);
    // Each body has its own goal: a body standing on a goal would block the next.
    for (n, start) in [[4, 3], [5, 7], [3, 9], [6, 13]].into_iter().enumerate() {
        let top = [12 + RISERS + 3, STAND + RISERS / 2, 2 + 4 * n as i32];
        let goal = Goal::local(here, [top[0], top[2]], None);
        let person = history::PersonId(10 + n as u32);
        let mut p = None;
        r.with_data(|d| {
            p = d.spawn_person_safe(person, TerrainKind::Stud.column_centre(start));
            assert!(d.walk_entity_safe(p.unwrap(), goal, here, None), "a path up the stair from {start:?}");
        });
        let p = p.unwrap();
        let w = r.data().ecs.walker.try_get(p).clone().unwrap();
        assert!(w.to_goal && w.walk_up.iter().skip(1).all(|&u| u), "every step of a stud path is walked");
        let budget = walk_budget(30.0);
        let (mut stalled, mut rise) = (0, 0.0f32);
        let mut prev = body_feet(&r, p).y;
        for t in 0.. {
            assert!(t < budget, "still walking after {budget} ticks, on {:?}", body_tile(&r, p));
            tick(&mut r);
            let y = body_feet(&r, p).y;
            rise = rise.max(y - prev);
            prev = y;
            match r.data().ecs.walker.try_get(p).as_ref() {
                Some(w) => stalled = stalled.max(w.stalled),
                None => break,
            }
        }
        assert!(stalled < WALK_UP_JUMP_AFTER_STALL_TICKS, "from {start:?}: stalled {stalled} ticks (the jump fallback)");
        assert!(rise <= plate_units(STUD_STEP_PLATES) + 0.5, "from {start:?}: rose {rise} units in a tick (a jump)");
        let at = body_tile(&r, p);
        assert_eq!(at[1], top[1], "from {start:?}: on the top landing, {at:?}");
        assert!((at[0] - top[0]).abs() <= 1 && (at[2] - top[2]).abs() <= 1, "from {start:?}: arrived on {at:?}");
    }
}
