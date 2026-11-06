//! Terrain slopes (spec `docs/superpowers/specs/2026-11-06-terrain-slopes-design.md`,
//! risk 1): a body walks up a one-block rise through a slope block — every
//! straight facing head-on, outer and inner corners along their low diagonal —
//! without pressing Space, and back down without leaving the ground for more
//! than a couple of ticks; a whole-block rise with no slope still needs a jump.
//!
//! Each world is one chunk: a one-block floor (level 0) under a one-block
//! plateau (level 1) laid out in a canonical frame `(a, b)` where uphill is +a
//! (and +b for the corners), mirrored / transposed into world blocks. The
//! player stands at block corner (8, 8), 2 m short of the slope.
use crate::common;

use common::{body, key, teleport, ticks, world_of, Body};
use game::movement::{STEP_HEIGHT, UNITS_PER_METER};
use game::{slope_chisel, Block, BlockId, BlockIndex, Chunk, Key, Side, SlopeShape, World, CHUNK_BLOCKS};

const SOLID: BlockId = BlockId(1);
/// Canonical block coordinate of the first raised block (the slope's).
const EDGE: usize = 10;
/// Where the walks start and end on the low floor, metres (a block corner).
const START_M: f32 = 8.0;
/// How far past the rise's foot the walk up ends, metres along each axis.
const CLIMB_TO_M: f32 = 4.5;
/// Feet height slack, metres (the brief's tolerance).
const FEET_TOLERANCE_M: f32 = 0.05;
/// The most ticks in a row a body may be ungrounded on the way down.
const MAX_AIR_TICKS: usize = 2;
/// Walk-tick cap for one leg; a stuck body fails rather than spinning forever.
const MAX_LEG_TICKS: usize = 80;
/// Ticks after a leg for the body to land (a jump's whole arc fits).
const SETTLE_TICKS: usize = 30;

#[derive(Clone, Copy)]
enum Cell {
    Floor,
    Full,
    Slope(SlopeShape),
}

/// Canonical layouts: uphill toward +a (Straight), toward +a +b (corners).
#[derive(Clone, Copy, Debug)]
enum Layout {
    /// A plateau `a > EDGE` with a straight slope row at `a == EDGE`.
    Straight,
    /// [`Layout::Straight`] with a whole block where the slope row is.
    Step,
    /// A raised quadrant `a, b >= EDGE`: an outer corner at `(EDGE, EDGE)`.
    Outer,
    /// Raised everywhere but the quadrant `a, b < EDGE`: an inner corner at `(EDGE, EDGE)`.
    Inner,
}

impl Layout {
    /// The level-1 cell at canonical `(a, b)`; slope sides are canonical
    /// (`NegX` = toward −a, `NegZ` = toward −b).
    fn cell(self, a: usize, b: usize) -> Cell {
        let straight = |low| Cell::Slope(SlopeShape::Straight { low });
        match self {
            Layout::Straight | Layout::Step => match a {
                _ if a < EDGE => Cell::Floor,
                EDGE if matches!(self, Layout::Straight) => straight(Side::NegX),
                _ => Cell::Full,
            },
            Layout::Outer => match (a, b) {
                _ if a < EDGE || b < EDGE => Cell::Floor,
                (EDGE, EDGE) => Cell::Slope(SlopeShape::Outer { low_x: Side::NegX, low_z: Side::NegZ }),
                (EDGE, _) => straight(Side::NegX),
                (_, EDGE) => straight(Side::NegZ),
                _ => Cell::Full,
            },
            Layout::Inner => match (a, b) {
                _ if a < EDGE && b < EDGE => Cell::Floor,
                (EDGE, EDGE) => Cell::Slope(SlopeShape::Inner { low_x: Side::NegX, low_z: Side::NegZ }),
                (EDGE, _) if b < EDGE => straight(Side::NegX),
                (_, EDGE) if a < EDGE => straight(Side::NegZ),
                _ => Cell::Full,
            },
        }
    }
}

/// How the canonical frame sits in the world: `swap` puts a on z, `flip_*`
/// mirrors a world axis (block `b` <-> `CHUNK_BLOCKS - 1 - b`, i.e. about 8 m).
#[derive(Clone, Copy, Debug)]
struct Frame {
    swap: bool,
    flip_x: bool,
    flip_z: bool,
}

impl Frame {
    /// The frame whose uphill (+a) is the world side `up`.
    fn facing(up: Side) -> Self {
        let swap = matches!(up, Side::PosZ | Side::NegZ);
        Frame { swap, flip_x: up == Side::NegX, flip_z: up == Side::NegZ }
    }

    /// Canonical `(a, b)` of world block `(bx, bz)`.
    fn canonical(self, bx: usize, bz: usize) -> (usize, usize) {
        let m = |v: usize, flip: bool| if flip { CHUNK_BLOCKS - 1 - v } else { v };
        let (x, z) = (m(bx, self.flip_x), m(bz, self.flip_z));
        if self.swap { (z, x) } else { (x, z) }
    }

    /// The world side of canonical side `s`.
    fn side(self, s: Side) -> Side {
        let along_x = matches!(s, Side::PosX | Side::NegX) != self.swap;
        let positive = matches!(s, Side::PosX | Side::PosZ);
        let flip = if along_x { self.flip_x } else { self.flip_z };
        match (along_x, positive != flip) {
            (true, true) => Side::PosX,
            (true, false) => Side::NegX,
            (false, true) => Side::PosZ,
            (false, false) => Side::NegZ,
        }
    }

    fn shape(self, shape: SlopeShape) -> SlopeShape {
        // A canonical corner pairs an a-side with a b-side; after a swap the
        // b-side lies along x.
        let pair = |p: Side, q: Side| {
            let (p, q) = (self.side(p), self.side(q));
            if matches!(p, Side::PosX | Side::NegX) { (p, q) } else { (q, p) }
        };
        match shape {
            SlopeShape::Straight { low } => SlopeShape::Straight { low: self.side(low) },
            SlopeShape::Outer { low_x, low_z } => {
                let (low_x, low_z) = pair(low_x, low_z);
                SlopeShape::Outer { low_x, low_z }
            }
            SlopeShape::Inner { low_x, low_z } => {
                let (low_x, low_z) = pair(low_x, low_z);
                SlopeShape::Inner { low_x, low_z }
            }
        }
    }

    /// World metres of canonical metres `(a, b)` (the mirror is about 8 m).
    fn world_m(self, a: f32, b: f32) -> (f32, f32) {
        let edge = CHUNK_BLOCKS as f32;
        let (x, z) = if self.swap { (b, a) } else { (a, b) };
        (if self.flip_x { edge - x } else { x }, if self.flip_z { edge - z } else { z })
    }

    /// The keys walking `dir` (uphill, or back) at yaw 0: S is +z, W −z,
    /// D +x, A −x.
    fn keys(self, dir: Dir, uphill: bool) -> Vec<Key> {
        let key_for = |s: Side| match s {
            Side::PosX => Key::KeyD,
            Side::NegX => Key::KeyA,
            Side::PosZ => Key::KeyS,
            Side::NegZ => Key::KeyW,
        };
        let (pa, pb) = if uphill { (Side::PosX, Side::PosZ) } else { (Side::NegX, Side::NegZ) };
        let mut keys = Vec::new();
        if dir != Dir::B {
            keys.push(key_for(self.side(pa)));
        }
        if dir != Dir::A {
            keys.push(key_for(self.side(pb)));
        }
        keys
    }
}

/// A one-chunk world: floor at level 0, `layout`'s level 1 placed by `frame`.
fn slope_world(layout: Layout, frame: Frame) -> World {
    let chunk_edge = (CHUNK_BLOCKS * game::BLOCK_VOXELS) as f32;
    world_of(0.0..chunk_edge, 0.0..chunk_edge, |_, _| {
        let mut chunk = Chunk::flat_floor(game::BLOCK_VOXELS as u32);
        for bx in 0..CHUNK_BLOCKS {
            for bz in 0..CHUNK_BLOCKS {
                let (a, b) = frame.canonical(bx, bz);
                match layout.cell(a, b) {
                    Cell::Floor => {}
                    Cell::Full => chunk.set_block(bx, 1, bz, Block::new(SOLID)),
                    Cell::Slope(shape) => {
                        chunk.set_block(bx, 1, bz, Block::new(SOLID));
                        chunk.chisel.insert(BlockIndex::from_xyz(bx, 1, bz), slope_chisel(SOLID, frame.shape(shape)));
                    }
                }
            }
        }
        chunk
    })
}

/// The canonical direction of a walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    /// Along +a.
    A,
    /// Along +b.
    B,
    /// Along +a +b.
    Diagonal,
}

/// One leg's record: every tick's body, in order.
struct Leg(Vec<Body>);

impl Leg {
    fn last(&self) -> Body {
        *self.0.last().unwrap()
    }
    fn max_rise(&self) -> f32 {
        self.0.windows(2).map(|w| w[1].y - w[0].y).fold(0.0, f32::max)
    }
    fn longest_air(&self) -> usize {
        let (mut run, mut most) = (0, 0);
        for b in &self.0 {
            run = if b.grounded { 0 } else { run + 1 };
            most = most.max(run);
        }
        most
    }
}

/// Holds `keys` (plus Space when `jump`) until `done` holds or the cap runs
/// out, then releases them and lets the body settle.
fn walk(world: &mut World, keys: &[Key], jump: bool, done: impl Fn(Body) -> bool) -> Leg {
    let mut leg = vec![body(world)];
    for &k in keys {
        key(world, k, true);
    }
    if jump {
        key(world, Key::Space, true);
    }
    for _ in 0..MAX_LEG_TICKS {
        ticks(world, 1);
        leg.push(body(world));
        if done(*leg.last().unwrap()) {
            break;
        }
    }
    for &k in keys {
        key(world, k, false);
    }
    if jump {
        key(world, Key::Space, false);
    }
    for _ in 0..SETTLE_TICKS {
        ticks(world, 1);
        leg.push(body(world));
        if leg.last().unwrap().grounded {
            break;
        }
    }
    Leg(leg)
}

/// Canonical metres along `dir` of a world position.
fn progress(frame: Frame, dir: Dir, b: Body) -> f32 {
    let (x, z) = (b.x / UNITS_PER_METER, b.z / UNITS_PER_METER);
    let edge = CHUNK_BLOCKS as f32;
    let (x, z) = (if frame.flip_x { edge - x } else { x }, if frame.flip_z { edge - z } else { z });
    let (a, b) = if frame.swap { (z, x) } else { (x, z) };
    match dir {
        Dir::A => a,
        Dir::B => b,
        Dir::Diagonal => a.min(b),
    }
}

/// Puts the player at the start, walks up the rise without Space and back
/// down, asserting the climb and the descent.
fn assert_walks_up_and_down(layout: Layout, frame: Frame, diagonal: bool) {
    let dir = if diagonal { Dir::Diagonal } else { Dir::A };
    assert_walks_from(layout, frame, dir, (START_M, START_M));
}

/// [`assert_walks_up_and_down`] from canonical metres `start`, along `dir`.
fn assert_walks_from(layout: Layout, frame: Frame, dir: Dir, start: (f32, f32)) {
    let mut world = slope_world(layout, frame);
    let floor_y = body(&world).y;
    let (x, z) = frame.world_m(start.0, start.1);
    teleport(&mut world, x * UNITS_PER_METER, floor_y, z * UNITS_PER_METER);
    ticks(&mut world, 2);
    let what = format!("{layout:?} {frame:?} {dir:?}");
    let start = body(&world);
    assert!((start.y - floor_y).abs() < 0.01, "{what}: start must stand on level 0: {start:?}");

    let top = EDGE as f32 + CLIMB_TO_M;
    let up = walk(&mut world, &frame.keys(dir, true), false, |b| progress(frame, dir, b) >= top);
    let end = up.last();
    let feet_tol = FEET_TOLERANCE_M * UNITS_PER_METER;
    assert!(
        (end.y - (floor_y + UNITS_PER_METER)).abs() < feet_tol && progress(frame, dir, end) >= top - 0.5,
        "{what}: the body must end on the plateau without jumping: floor y {floor_y}, end {end:?}"
    );
    assert!(
        up.max_rise() <= STEP_HEIGHT * UNITS_PER_METER,
        "{what}: rose {} units in one tick (step height {})",
        up.max_rise(),
        STEP_HEIGHT * UNITS_PER_METER
    );

    let down = walk(&mut world, &frame.keys(dir, false), false, |b| progress(frame, dir, b) <= START_M);
    let back = down.last();
    assert!(
        (back.y - floor_y).abs() < feet_tol && back.grounded,
        "{what}: the body must return to level 0: floor y {floor_y}, end {back:?}"
    );
    assert!(
        down.longest_air() <= MAX_AIR_TICKS,
        "{what}: ungrounded {} ticks in a row on the way down",
        down.longest_air()
    );
}

const SIDES: [Side; 4] = [Side::PosX, Side::NegX, Side::PosZ, Side::NegZ];

#[test]
fn slope_walk_up_and_down_every_facing() {
    for up in SIDES {
        assert_walks_up_and_down(Layout::Straight, Frame::facing(up), false);
    }
}

#[test]
fn slope_walk_diagonal_onto_outer_and_inner() {
    for layout in [Layout::Outer, Layout::Inner] {
        for flip_x in [false, true] {
            for flip_z in [false, true] {
                assert_walks_up_and_down(layout, Frame { swap: false, flip_x, flip_z }, true);
            }
        }
    }
}

/// Head-on onto an outer corner from each of its two low cardinals, centred
/// on the corner's other axis (the edge between its high and low halves).
#[test]
fn slope_walk_onto_outer_from_each_low_cardinal() {
    let across = EDGE as f32 + 0.5;
    for flip_x in [false, true] {
        for flip_z in [false, true] {
            let frame = Frame { swap: false, flip_x, flip_z };
            assert_walks_from(Layout::Outer, frame, Dir::A, (START_M, across));
            assert_walks_from(Layout::Outer, frame, Dir::B, (across, START_M));
        }
    }
}

#[test]
fn full_block_still_needs_a_jump() {
    let frame = Frame::facing(Side::PosX);
    let mut world = slope_world(Layout::Step, frame);
    let floor_y = body(&world).y;
    teleport(&mut world, START_M * UNITS_PER_METER, floor_y, START_M * UNITS_PER_METER);
    ticks(&mut world, 2);
    let top = EDGE as f32 + CLIMB_TO_M;
    let keys = frame.keys(Dir::A, true);
    let tol = FEET_TOLERANCE_M * UNITS_PER_METER;

    let blocked = walk(&mut world, &keys, false, |b| progress(frame, Dir::A, b) >= top);
    let end = blocked.last();
    assert!(
        blocked.0.iter().all(|b| (b.y - floor_y).abs() < tol) && progress(frame, Dir::A, end) < EDGE as f32,
        "without Space the body must stay on level 0 short of the rise: {end:?}"
    );

    let climbed = walk(&mut world, &keys, true, |b| progress(frame, Dir::A, b) >= top);
    let end = climbed.last();
    assert!(
        (end.y - (floor_y + UNITS_PER_METER)).abs() < tol,
        "with Space held the body must climb the block: floor y {floor_y}, end {end:?}"
    );
}
