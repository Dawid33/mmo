//! The path search (`game::pathfind`) on stud tiles, over a hand-made tile
//! source: a tile is `Open`, `Solid` or a `Floor(plates)`. The same rules on
//! real stud regions (tiles derived from bricks and recipes) are
//! `stud_tiles::paths_*` / `standable_*`.
use std::collections::BTreeMap;

use game::*;

/// A whole tile of anything.
const STONE: TileFill = TileFill::Solid;

#[derive(Default)]
struct Src {
    blocks: BTreeMap<TilePos, TileFill>,
    water: BTreeMap<TilePos, u16>,
}

impl Src {
    fn floor(size: i32) -> Self {
        let mut s = Self::default();
        for x in 0..size {
            for z in 0..size {
                s.blocks.insert([x, 0, z], STONE);
            }
        }
        s
    }
    fn wall(&mut self, x: i32, zs: std::ops::RangeInclusive<i32>) {
        for z in zs {
            for y in 1..=2 {
                self.blocks.insert([x, y, z], STONE);
            }
        }
    }
}

impl TileSource for Src {
    fn tile_fill(&self, p: TilePos) -> TileFill {
        self.blocks.get(&p).copied().unwrap_or(TileFill::Open)
    }
    fn water_units(&self, p: TilePos) -> u16 {
        self.water.get(&p).copied().unwrap_or(0)
    }
    fn is_bedrock(&self, _: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _: TilePos) -> bool {
        false
    }
    fn tree_id(&self, _: TilePos) -> u32 {
        0
    }
}

fn install() {
    crate::common::install_real_block_props();
}

fn path(s: &Src, a: TilePos, b: TilePos) -> Option<Vec<TilePos>> {
    find_path(s, a, b, MAX_PATH_NODES)
}

#[test]
fn straight_line_on_flat_floor() {
    let s = Src::floor(12);
    let p = path(&s, [1, 1, 1], [6, 1, 1]).unwrap();
    assert_eq!(p.len(), 6);
    assert_eq!(*p.last().unwrap(), [6, 1, 1]);
    for w in p.windows(2) {
        assert!((w[1][0] - w[0][0]).abs() + (w[1][2] - w[0][2]).abs() >= 1);
    }
}

#[test]
fn diagonal_is_used_and_cheaper() {
    let s = Src::floor(12);
    assert_eq!(path(&s, [1, 1, 1], [5, 1, 5]).unwrap().len(), 5);
}

#[test]
fn detours_around_a_wall() {
    let mut s = Src::floor(12);
    s.wall(3, 0..=4);
    let p = path(&s, [1, 1, 2], [5, 1, 2]).unwrap();
    assert!(p.iter().all(|t| !(t[0] == 3 && t[2] <= 4)));
    assert!(p.iter().any(|t| t[2] == 5));
}

/// A brick stair across the floor: a step of `STUD_STEP_PLATES` at x = 4 (stood on
/// in its own tile) up to a whole tile at x = 5 (stood on in the tile above), then
/// the drop of a tile (`STUD_DROP_PLATES`) back to the floor. One tile-high wall
/// without the step is not climbed: stud bodies walk up, they never plan a jump.
#[test]
fn climbs_a_brick_stair_and_drops_a_tile() {
    let mut s = Src::floor(12);
    for z in 0..12 {
        s.blocks.insert([4, 1, z], TileFill::Floor(STUD_STEP_PLATES as u8));
        s.blocks.insert([5, 1, z], STONE);
    }
    let p = path(&s, [1, 1, 1], [8, 1, 1]).unwrap();
    assert!(p.iter().any(|t| t[0] == 4 && t[1] == 1), "over the step, in its own tile: {p:?}");
    assert!(p.windows(2).any(|w| w[1][1] - w[0][1] == 1));
    assert!(p.windows(2).any(|w| w[1][1] - w[0][1] == -1));
    let mut wall = Src::floor(12);
    for z in 0..12 {
        wall.blocks.insert([5, 1, z], STONE);
    }
    assert!(path(&wall, [1, 1, 1], [8, 1, 1]).is_none(), "a whole tile is too high a step");
}

#[test]
fn unreachable_two_high_wall() {
    let mut s = Src::floor(12);
    for z in 0..12 {
        s.wall(4, z..=z);
    }
    assert!(path(&s, [1, 1, 1], [8, 1, 1]).is_none());
}

#[test]
fn unreachable_goal_in_water() {
    let mut s = Src::floor(12);
    s.water.insert([6, 1, 1], WATER_FULL);
    assert!(path(&s, [1, 1, 1], [6, 1, 1]).is_none());
}

#[test]
fn unreachable_goal_not_standable() {
    let s = Src::floor(12);
    assert!(path(&s, [1, 1, 1], [6, 4, 1]).is_none());
}

#[test]
fn no_corner_cutting() {
    let mut s = Src::floor(12);
    // Two pillars touching only diagonally: (3,1,3) and (4,1,4) (2 high).
    for y in 1..=2 {
        s.blocks.insert([3, y, 3], STONE);
        s.blocks.insert([4, y, 4], STONE);
    }
    let p = path(&s, [4, 1, 3], [3, 1, 4]).unwrap();
    assert!(p.len() > 2);
    for w in p.windows(2) {
        let (dx, dz) = (w[1][0] - w[0][0], w[1][2] - w[0][2]);
        assert!(dx == 0 || dz == 0 || (dx.abs() == 1 && dz.abs() == 1));
    }
    assert!(!p.windows(2).any(|w| w[0] == [4, 1, 3] && w[1] == [3, 1, 4]));
}

#[test]
fn budget_exhausted_is_none() {
    let s = Src::floor(12);
    assert!(find_path(&s, [1, 1, 1], [10, 1, 10], 4).is_none());
    assert!(find_path(&s, [1, 1, 1], [10, 1, 10], MAX_PATH_NODES).is_some());
}

#[test]
fn same_tile_and_out_of_region() {
    let s = Src::floor(12);
    assert_eq!(path(&s, [2, 1, 2], [2, 1, 2]).unwrap(), vec![[2, 1, 2]]);
    assert!(!standable(&s, [-1, 1, 0]));
}

#[test]
fn nearest_standable_picks_column_tile() {
    let mut s = Src::floor(12);
    s.blocks.insert([3, 5, 3], STONE);
    assert_eq!(nearest_standable(&s, [3, 3], None), Some([3, 6, 3]));
    assert_eq!(nearest_standable(&s, [3, 3], Some(0)), Some([3, 1, 3]));
    assert_eq!(nearest_standable(&s, [20, 20], None), None);
}

#[test]
fn deterministic() {
    let mut s = Src::floor(20);
    for x in (2..18).step_by(4) {
        s.wall(x, 0..=14);
        s.wall(x + 2, 5..=19);
    }
    let a = path(&s, [0, 1, 0], [19, 1, 19]);
    let b = path(&s, [0, 1, 0], [19, 1, 19]);
    assert!(a.is_some());
    assert_eq!(a, b);
}

/// Water of DF depth `depth` on the standing tile `[x, 1, z]`.
fn flood(s: &mut Src, x: i32, z: i32, depth: u8) {
    s.water.insert([x, 1, z], WATER_FULL * u16::from(depth) / 7);
}

#[test]
fn deep_water_is_not_standable() {
    install();
    let mut s = Src::floor(12);
    flood(&mut s, 5, 5, 4);
    assert!(!standable(&s, [5, 1, 5]));
    flood(&mut s, 6, 5, 3);
    assert!(standable(&s, [6, 1, 5]));
    assert!(path(&s, [1, 1, 5], [5, 1, 5]).is_none());
}

#[test]
fn shallow_water_is_wadable_at_a_cost() {
    install();
    let dry = Src::floor(12);
    let mut wet = Src::floor(12);
    // A band of shallows across the whole floor: no way round it.
    for x in 4..7 {
        for z in 0..12 {
            flood(&mut wet, x, z, 2);
        }
    }
    let (a, b) = ([1, 1, 1], [9, 1, 1]);
    let dry_path = path(&dry, a, b).unwrap();
    let wet_path = path(&wet, a, b).unwrap();
    assert_eq!(wet_path.len(), dry_path.len());
    let (w, d) = (path_cost(&wet, &wet_path).unwrap(), path_cost(&dry, &dry_path).unwrap());
    assert_eq!(w, d + 3 * WADE_COST);
}

#[test]
fn path_detours_around_a_pond() {
    install();
    let mut s = Src::floor(12);
    for x in 4..7 {
        for z in 4..7 {
            flood(&mut s, x, z, 7);
        }
    }
    let p = path(&s, [1, 1, 5], [9, 1, 5]).unwrap();
    assert!(p.iter().all(|t| !(4..7).contains(&t[0]) || !(4..7).contains(&t[2])), "{p:?}");
}

#[test]
fn wading_cost_makes_short_detours_win() {
    install();
    let mut s = Src::floor(12);
    flood(&mut s, 5, 1, 2);
    // Straight is 80; around the tile via two diagonals is 88 < 80 + WADE_COST.
    let p = path(&s, [1, 1, 1], [9, 1, 1]).unwrap();
    assert!(!p.contains(&[5, 1, 1]), "{p:?}");
}
