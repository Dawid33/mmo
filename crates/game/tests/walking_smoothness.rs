//! A body walking over flat stud terrain must move level and at constant
//! speed, whatever the terrain is assembled from. Terrain colliders are many
//! boxes laid flush — the cells and stud caps inside a chunk collider, chunk
//! colliders side by side — and every seam between two boxes can read as an
//! edge: the character controller slides down its leaning normal into the
//! floor, falls through the skin for a tick, then trips on the next box and
//! pops up ungrounded. Steps, walls and low ceilings at a chunk border, and
//! plate terraces, behave as steps, walls and ceilings do anywhere.
use crate::common;

use common::{
    approach_seam, body, key, seam_row, seam_z, spawn_x_band, spawn_z_walk, stud_flat, terraces, ticks, world_of,
    CHUNK_EDGE, FLOOR_PLATES, STAND,
};
use game::movement::{BodyScale, CHARACTER_SKIN, TICK_DT, UNITS_PER_METER, WALK_SPEED};
use game::{Key, World, TILE_PLATES};

/// Distance covered by one tick of walking.
const STEP: f32 = WALK_SPEED * UNITS_PER_METER * TICK_DT;
/// Slack on per-tick motion: the controller re-settles onto its skin by a
/// fraction of a millimetre now and then.
const LEVEL: f32 = 0.005;
/// A plate's height, sim units.
fn plate() -> f32 {
    game::plate_units(1)
}

/// The z extent (world units) a walk of that many ticks from the spawn needs.
fn rows_for(walk_ticks: usize) -> std::ops::Range<f32> {
    spawn_z_walk(walk_ticks as f32 * STEP)
}

/// A world one chunk either side of the spawn column in x, over `z_units`
/// along z: all a walk along z from the spawn touches.
fn world(z_units: std::ops::Range<f32>, ground: impl Fn(usize, usize) -> game::StudColumnGround) -> World {
    world_of(spawn_x_band(), z_units, ground)
}

/// Walks +z (S at yaw 0) for `n` ticks after sidestepping `side` ticks along
/// +x, asserting level, straight, constant-speed, grounded motion throughout.
/// `side = 0` walks the x = 128 seam between two chunk columns.
fn assert_level_walk(world: &mut World, side: usize, n: usize) {
    // Up to three chunk borders; a walk too short for that crosses as many as
    // it is long.
    assert_level_walk_across(world, side, n, 3.min((n as f32 * STEP / CHUNK_EDGE) as usize));
}

/// [`assert_level_walk`] that must cross at least `borders` chunk borders.
fn assert_level_walk_across(world: &mut World, side: usize, n: usize, borders: usize) {
    key(world, Key::KeyD, true);
    ticks(world, side);
    key(world, Key::KeyD, false);
    ticks(world, 2);
    key(world, Key::KeyS, true);
    ticks(world, 1);
    let start = body(world);
    let mut prev = start;
    for tick in 0..n {
        ticks(world, 1);
        let now = body(world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        assert!((now.z - prev.z - STEP).abs() < LEVEL, "speed hitched at {at}");
        assert!((now.x - start.x).abs() < LEVEL, "drifted sideways at {at}");
        prev = now;
    }
    let crossed = (prev.z / CHUNK_EDGE).floor() - (start.z / CHUNK_EDGE).floor();
    assert!(crossed >= borders as f32, "the walk must cross chunk borders: {start:?} -> {prev:?}");
}

/// Ticks of a walk long enough to also catch what only shows up now and then.
const LONG: usize = 80;

#[test]
fn flat_floor_is_level_across_chunk_borders() {
    for side in [0, 4] {
        assert_level_walk(&mut world(rows_for(LONG), |_, _| stud_flat(FLOOR_PLATES)), side, LONG);
    }
}

#[test]
fn flat_floor_is_level_across_a_chunk_border() {
    // Small and short: a chunk either side of the seam ahead of the spawn,
    // the walk starting just short of it.
    let mut world = world(seam_z() - 32.0..seam_z() + 32.0, |_, _| stud_flat(FLOOR_PLATES));
    approach_seam(&mut world, 32.0);
    assert_level_walk_across(&mut world, 0, 12, 1);
}

#[test]
fn diagonal_walk_is_level() {
    let x = common::spawn_x_band();
    let mut world = world_of(x.start..x.end + CHUNK_EDGE, rows_for(20), |_, _| stud_flat(FLOOR_PLATES));
    key(&mut world, Key::KeyS, true);
    key(&mut world, Key::KeyD, true);
    ticks(&mut world, 1);
    let mut prev = body(&world);
    for tick in 0..20 {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "lost the ground at {at}");
        assert!((now.y - prev.y).abs() < LEVEL, "bobbed at {at}");
        let moved = ((now.x - prev.x).powi(2) + (now.z - prev.z).powi(2)).sqrt();
        assert!((moved - STEP).abs() < LEVEL, "speed hitched at {at}");
        prev = now;
    }
}

#[test]
fn low_step_at_a_chunk_border_is_walked_up() {
    // One plate (0.2 m) up at the seam ahead of the spawn, under the stud
    // body's step height.
    let stepped = |_, z: usize| stud_flat(if z >= seam_row() { FLOOR_PLATES + 1 } else { FLOOR_PLATES });
    let mut world = world(rows_for(25), stepped);
    approach_seam(&mut world, 32.0);
    let low = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let high = body(&world);
    assert!(high.z > seam_z() + 40.0, "the step must not stop the walk: {high:?}");
    assert!(high.grounded);
    assert!((high.y - low.y - plate()).abs() < 0.05, "must stand on the step: {low:?} -> {high:?}");
}

#[test]
fn full_tile_wall_at_a_chunk_border_blocks() {
    // One tile (1.2 m) up at the seam ahead of the spawn: above the step height.
    let walled = |_, z: usize| stud_flat(if z >= seam_row() { FLOOR_PLATES + TILE_PLATES as i32 } else { FLOOR_PLATES });
    let mut world = world(rows_for(25), walled);
    approach_seam(&mut world, 32.0);
    let before = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    // Stopped a capsule radius and its skin short of the wall.
    let gap = BodyScale::STUD.capsule_radius() + CHARACTER_SKIN;
    assert!((seam_z() - after.z - gap).abs() < 0.5, "stands against the wall ({gap} units off it): {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the wall must not lift the body: {after:?}");
}

/// Flat ground and, from the chunk seam ahead of the spawn on, a one-tile-thick
/// ceiling over the walking line whose underside is `clear_tiles` tiles above
/// the floor.
fn tunnel(clear_tiles: i32) -> World {
    let mut world = world(rows_for(25), |_, _| stud_flat(FLOOR_PLATES));
    let (w, cb) = (common::TILE_W, game::CHUNK_BLOCKS as i32);
    let x0 = (game::PLAYER_SPAWN_X / w) as i32;
    let z0 = seam_row() as i32 * cb;
    world.regions.get_mut(&common::r0()).unwrap().with_data(|d| {
        // Two tiles either side of the walking line, half a chunk deep.
        for x in x0 - 2..=x0 + 2 {
            for z in z0..z0 + cb / 2 {
                assert!(common::fill_tile(d, [x, STAND + clear_tiles, z]) > 0, "ceiling at {x},{z}");
            }
        }
    });
    approach_seam(&mut world, 32.0);
    world
}

#[test]
fn player_walks_through_a_two_tile_tunnel() {
    // The 2.2 m body must fit a gap two tiles (2.4 m) high.
    let mut world = tunnel(2);
    let before = body(&world);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    assert!(after.z > seam_z() + 15.0, "a two-tile tunnel must not stop the body: {after:?}");
    assert!(after.grounded);
    assert!((after.y - before.y).abs() < LEVEL, "the ceiling must not push the body: {after:?}");
}

#[test]
fn player_does_not_fit_a_one_tile_tunnel() {
    let mut world = tunnel(1);
    key(&mut world, Key::KeyS, true);
    ticks(&mut world, 25);
    let after = body(&world);
    assert!(after.z < seam_z(), "a one-tile gap is lower than the body: {after:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_down_terraces_stays_on_the_ground() {
    // The slide along level ground ends with no downward part, which used to
    // keep snap-to-ground from engaging: a tick of hovering past each edge,
    // ungrounded, then a drop.
    // Three seams ahead: the walk is as long as it takes to pass them.
    let end_z = seam_z() + 2.0 * CHUNK_EDGE;
    let walk = ((end_z - game::PLAYER_SPAWN_Z) / STEP).ceil() as usize + 2;
    let mut world = terraces(rows_for(walk));
    key(&mut world, Key::KeyS, true);
    let mut prev = body(&world);
    for tick in 0..walk {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y < LEVEL, "rose on the way down at {at}");
        assert!(prev.y - now.y < plate() + 0.1, "fell more than a terrace at {at}");
        prev = now;
    }
    assert!(prev.z > end_z, "must have walked down three terraces: {prev:?}");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_up_terraces_stays_on_the_ground() {
    // Sliding over a terrace's edge overshoots it; the body must be put back
    // down on the terrace within the tick rather than fall for the next one.
    // Start a few ticks short of a chunk border, going back toward -z: 20
    // units into the second chunk row.
    let z0 = game::PLAYER_SPAWN_Z.max(CHUNK_EDGE + 20.0);
    let border = ((z0 / CHUNK_EDGE).ceil() - 1.0) * CHUNK_EDGE;
    let mut world = terraces((border - CHUNK_EDGE).max(0.0)..z0 + CHUNK_EDGE);
    if z0 != game::PLAYER_SPAWN_Z {
        let b = body(&world);
        common::teleport(&mut world, b.x, b.y, z0);
        ticks(&mut world, 4);
    }
    let start = body(&world);
    key(&mut world, Key::KeyW, true);
    let mut prev = start;
    for tick in 0..15 {
        ticks(&mut world, 1);
        let now = body(&world);
        let at = format!("tick {tick}: {prev:?} -> {now:?}");
        assert!(now.grounded, "left the ground at {at}");
        assert!(now.y - prev.y > -LEVEL, "dropped on the way up at {at}");
        prev = now;
    }
    // Standing on the terrace it has reached (`common::terraces`: a plate
    // down per chunk row): its stud caps + capsule half-extent + skin.
    let floor = common::floor_y(FLOOR_PLATES + 12 - (prev.z / CHUNK_EDGE).floor() as i32);
    assert!(prev.z < border && prev.y > start.y + plate() - 0.1, "must have climbed: {start:?} -> {prev:?}");
    let s = BodyScale::STUD;
    let half_extent = s.capsule_half_height() + s.capsule_radius();
    let stand = floor + game::stud_cap_units() + half_extent + CHARACTER_SKIN;
    assert!((prev.y - stand).abs() < 0.05, "must stand on the terrace: {prev:?} vs {stand}");
}
