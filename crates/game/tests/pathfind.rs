use std::collections::BTreeMap;

use game::*;

const STONE: BlockId = BlockId(2);
const WATER: BlockId = BlockId(5);
const LEAVES: BlockId = BlockId(15);

#[derive(Default)]
struct Src {
    blocks: BTreeMap<TilePos, BlockId>,
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
    fn block(&self, p: TilePos) -> BlockId {
        self.blocks.get(&p).copied().unwrap_or(BlockId::AIR)
    }
    fn is_bedrock(&self, _: TilePos) -> bool {
        false
    }
    fn edge_locked(&self, _: TilePos) -> bool {
        false
    }
    fn is_constructed(&self, _: TilePos) -> bool {
        false
    }
    fn tree_id(&self, _: TilePos) -> u32 {
        0
    }
    fn tree_limb(&self, _: TilePos) -> u8 {
        0
    }
}

fn install() {
    let reg = BlockRegistry::from_ron(include_str!("../../../assets/blocks/blocks.ron")).unwrap();
    install_block_props(reg.props());
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

#[test]
fn climbs_one_block_step() {
    let mut s = Src::floor(12);
    for z in 0..12 {
        s.blocks.insert([4, 1, z], STONE);
    }
    let p = path(&s, [1, 1, 1], [6, 1, 1]).unwrap();
    assert!(p.windows(2).any(|w| w[1][1] - w[0][1] == 1));
    assert!(p.windows(2).any(|w| w[1][1] - w[0][1] == -1));
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
    s.blocks.insert([6, 0, 1], WATER);
    s.blocks.insert([6, 1, 1], WATER);
    assert!(path(&s, [1, 1, 1], [6, 1, 1]).is_none());
}

#[test]
fn unreachable_goal_not_standable() {
    let s = Src::floor(12);
    assert!(path(&s, [1, 1, 1], [6, 4, 1]).is_none());
}

#[test]
fn leaves_are_not_a_floor() {
    install();
    let mut s = Src::floor(12);
    s.blocks.insert([6, 1, 1], LEAVES);
    assert!(!standable(&s, [6, 2, 1]));
    assert!(standable(&s, [6, 1, 1]), "leaves are passable, the stone below is a floor");
}

#[test]
fn no_corner_cutting() {
    let mut s = Src::floor(12);
    // Two blocks touching only diagonally: (3,1,3) and (4,1,4) (2 high).
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
