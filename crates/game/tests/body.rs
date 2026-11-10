//! The generic dynamic body (`game::body`, `EntityKind::Body`): it falls
//! under the world's gravity, comes to rest asleep on the floor, and every
//! tick of its tumble rolls back bit-exact.
use crate::common::{floor_seed, install_real_block_props, r0, FLOOR_PLATES, TILE_W};
use game::body::BODY_SETTLE_TICKS;
use game::rapier3d::math::Vector;
use game::{
    state_hash, ColliderSpec, EntityKey, EntityKind, GameEventKind, Pose, Region, Rollback, REGION_SIZE,
};

/// One metre (a tile's width), sim units.
const B: f32 = TILE_W;
/// The floor's top surface (flat stud ground at `FLOOR_PLATES`), units: 3.2 units a
/// plate, exact in `f32` for 30 plates.
pub const FLOOR_TOP: f32 = FLOOR_PLATES as f32 * 3.2;

/// The y of a body's lowest point resting on the studded floor: on the stud caps.
pub fn caps_top() -> f32 {
    FLOOR_TOP + game::stud_cap_units()
}

/// A region of flat stud ground edge to edge, no players.
pub fn floored_region() -> Region {
    install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.seed_stud_world(floor_seed(0.0..REGION_SIZE, 0.0..REGION_SIZE, FLOOR_PLATES));
    region
}

/// A 1 m cube.
pub fn cube() -> ColliderSpec {
    ColliderSpec::Cuboid { half_extents: Vector::splat(B / 2.0) }
}

/// Spawns a body in a forgotten transaction.
pub fn spawn(region: &mut Region, pose: Pose, collider: ColliderSpec, linvel: Vector, angvel: Vector) -> EntityKey {
    let mut e = None;
    region.with_data(|r| e = Some(r.spawn_body_safe(pose, collider, linvel, angvel)));
    e.unwrap()
}

pub fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

pub fn live_body(region: &Region, e: EntityKey) -> &game::rapier3d::dynamics::RigidBody {
    let r = region.data();
    r.physics.bodies.get(r.ecs.rigidbody.try_get(e).expect("a body")).expect("a live body")
}

#[test]
fn body_falls_and_sleeps() {
    let mut region = floored_region();
    let centre = REGION_SIZE / 2.0;
    let pose = Pose::from_translation(Vector::new(centre, FLOOR_TOP + B / 2.0 + 3.0 * B, centre));
    let e = spawn(&mut region, pose, cube(), Vector::ZERO, Vector::ZERO);
    assert_eq!(*region.data().ecs.kind.try_get(e), Some(EntityKind::Body));
    assert!(live_body(&region, e).is_dynamic());
    let mut slept_at = None;
    for t in 0..BODY_SETTLE_TICKS {
        tick(&mut region);
        if live_body(&region, e).is_sleeping() {
            slept_at = Some(t);
            break;
        }
    }
    let body = live_body(&region, e);
    assert!(slept_at.is_some(), "asleep within {BODY_SETTLE_TICKS} ticks (y {})", body.translation().y);
    // A studded floor: the cube rests on the stud caps.
    let rest = caps_top() + B / 2.0;
    // 1 cm = 0.16 units.
    assert!((body.translation().y - rest).abs() < 0.01 * B, "resting on the floor: y {} vs {rest}", body.translation().y);
}

#[test]
fn rollback_mid_fall_is_bit_exact() {
    assert_eq!(game::hash_mode(), game::HashMode::Full, "this binary runs under Full");
    let mut region = floored_region();
    let centre = REGION_SIZE / 2.0;
    let pose = Pose::from_translation(Vector::new(centre, FLOOR_TOP + 2.5 * B, centre));
    let e = spawn(&mut region, pose, cube(), Vector::new(20.0, 0.0, 5.0), Vector::new(1.0, 2.0, 3.0));
    for _ in 0..2 {
        tick(&mut region);
    }
    let before = state_hash(region.data());
    const N: usize = 10;
    for _ in 0..N {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    let after = state_hash(region.data());
    assert_ne!(before, after, "the body moved");
    for _ in 0..N {
        region.rollback_last_event();
    }
    assert_eq!(before, state_hash(region.data()), "the tumble rolls back bit-exact");
    for _ in 0..N {
        region.handle_event(GameEventKind::Tick).unwrap();
    }
    assert_eq!(after, state_hash(region.data()), "replaying the tumble reproduces it");
    assert!(region.data().ecs.entities.contains_key(e));
}

#[test]
fn spawn_rolls_back_bit_exact() {
    install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut rb = Rollback::new(Some(send));
    let before = state_hash(&rb);
    let centre = REGION_SIZE / 2.0;
    let pose = Pose::from_translation(Vector::new(centre, FLOOR_TOP + 2.0 * B, centre));
    rb.new_transaction();
    rb.spawn_body_safe(pose, cube(), Vector::ZERO, Vector::ZERO);
    assert_ne!(before, state_hash(&rb));
    rb.rollback();
    assert_eq!(before, state_hash(&rb), "the spawn rolls back bit-exact");
}

/// `Order::DevDropBody`: a 1 m cube [`game::body::DEV_DROP_HEIGHT_BLOCKS`]
/// metres above `at` (whose level the order reads in metres, on stud terrain
/// too), spinning a little (at most
/// [`game::body::DEV_DROP_SPIN_RAD_PER_S`] about each axis, rolled from the
/// tick — the same on every replay); a tile off the region's columns is
/// refused.
#[test]
fn dev_drop_body_drops_a_spinning_cube_above_the_tile() {
    use game::body::{DEV_DROP_HEIGHT_BLOCKS, DEV_DROP_SPIN_RAD_PER_S};
    let bodies = |region: &Region| -> Vec<EntityKey> {
        let r = region.data();
        r.id_index.values().copied().filter(|k| *r.ecs.kind.try_get(*k) == Some(EntityKind::Body)).collect()
    };
    let drop = |region: &mut Region, at: game::TilePos| {
        region.handle_event(GameEventKind::Order(0, game::Order::DevDropBody { at })).unwrap();
        region.forget_last_event();
    };
    let mut region = floored_region();
    // A level (metres) above the floor.
    let at = [20, (FLOOR_TOP / B).ceil() as i32 + 1, 24];
    drop(&mut region, at);
    let all = bodies(&region);
    assert_eq!(all.len(), 1, "one body dropped");
    let b = live_body(&region, all[0]);
    let t = b.translation();
    let want = Vector::new(20.5 * B, (at[1] + DEV_DROP_HEIGHT_BLOCKS) as f32 * B + B / 2.0, 24.5 * B);
    assert_eq!(t, want);
    let w = b.angvel();
    assert!(w != Vector::ZERO, "it spins");
    assert!(w.abs().max_element() <= DEV_DROP_SPIN_RAD_PER_S, "a little: {w:?}");
    let collider = region.data().physics.colliders.get(b.colliders()[0]).unwrap();
    assert_eq!(ColliderSpec::of_shape(collider.shape()), Some(cube()));
    // The same order on the same tick spins the same way.
    let mut again = floored_region();
    drop(&mut again, at);
    assert_eq!(live_body(&again, bodies(&again)[0]).angvel(), w);
    // Off the region's columns: refused.
    drop(&mut region, [-1, at[1], 24]);
    drop(&mut region, [game::REGION_BLOCKS as i32, at[1], 24]);
    assert_eq!(bodies(&region).len(), 1, "off-region drops refused");
}

/// A body whose collider is a shape that cannot cross (no `ColliderSpec`)
/// fails loudly at extraction instead of arriving as a player capsule.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "no ColliderSpec")]
fn unsupported_collider_shape_fails_loudly() {
    let mut region = floored_region();
    let pose = Pose::from_translation(Vector::new(REGION_SIZE / 2.0, FLOOR_TOP + B, REGION_SIZE / 2.0));
    let e = spawn(&mut region, pose, cube(), Vector::ZERO, Vector::ZERO);
    region.with_data(|r| {
        let handle = r.data.ecs.rigidbody.try_get(e).unwrap();
        let ch = r.data.physics.bodies.get(handle).unwrap().colliders()[0];
        r.data.physics.colliders.mutate_collider_safe(ch, |c| c.set_shape(game::rapier3d::prelude::SharedShape::cylinder(4.0, 8.0)));
        let _ = r.extract_entity_safe(e);
    });
}

/// A dev-dropped 1 m cube over a pool 6 tiles deep ends floating (its AABB
/// straddles the surface); the same cube dropped on dry ground keeps, after
/// every tick, exactly the weight and damping `body_state` gave it.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn dropped_cube_floats_and_dry_bodies_unchanged() {
    use game::body::{BODY_ANGULAR_DAMPING, BODY_GRAVITY};
    use game::mass_props::{mass_properties, BODY_DEFAULT_DENSITY};
    use game::movement::UNITS_PER_METER;
    const DEPTH: i32 = 6;
    let only_body = |region: &Region| -> EntityKey {
        let r = region.data();
        let all: Vec<EntityKey> =
            r.id_index.values().copied().filter(|k| *r.ecs.kind.try_get(*k) == Some(EntityKind::Body)).collect();
        assert_eq!(all.len(), 1);
        all[0]
    };
    let drop = |region: &mut Region, at: game::TilePos| {
        region.handle_event(GameEventKind::Order(0, game::Order::DevDropBody { at })).unwrap();
        region.forget_last_event();
    };
    let aabb_y = |region: &Region, e: EntityKey| {
        let b = live_body(region, e);
        let c = region.data().physics.colliders.get(b.colliders()[0]).unwrap();
        let aabb = c.compute_aabb();
        (aabb.mins.y, aabb.maxs.y)
    };

    // Over water: recipe water standing 6 tiles deep on the floor's top.
    use crate::common::water::{floor_with_water, level_top_plates, WaterBox};
    use crate::common::STAND;
    install_real_block_props();
    let pool = WaterBox { xs: 16..32, zs: 16..32, surface: STAND - 1, water_top: STAND - 1 + DEPTH };
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut wet = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    wet.seed_stud_world(floor_with_water(r0(), &[pool]));
    let surface = game::plate_units(level_top_plates(STAND - 1 + DEPTH));
    // The order's level is in metres: the first whole metre above the water.
    drop(&mut wet, [24, (surface / B).ceil() as i32, 24]);
    let e = only_body(&wet);
    // Settled: asleep, or slower than 0.1 m/s for a second — so the
    // straddle below is the float level, not a phase of a bob.
    const STILL_UNITS_PER_S: f32 = 0.1 * B;
    let still_ticks_needed = (1000 / game::TICK_RATE) as usize;
    let mut still = 0;
    for _ in 0..BODY_SETTLE_TICKS * 3 {
        tick(&mut wet);
        let b = live_body(&wet, e);
        let v = b.linvel();
        still = if b.is_sleeping() || v.dot(v) < STILL_UNITS_PER_S * STILL_UNITS_PER_S { still + 1 } else { 0 };
        if still >= still_ticks_needed {
            break;
        }
    }
    assert!(still >= still_ticks_needed, "the cube settles in the water: {:?}", live_body(&wet, e).linvel());
    let (lo, hi) = aabb_y(&wet, e);
    eprintln!("cube AABB y {} .. {} blocks, surface {}", lo / B, hi / B, surface / B);
    assert!(lo < surface && hi > surface, "floating, straddling the surface: {} .. {} blocks", lo / B, hi / B);

    // On dry ground: the spawn's weight and damping, bit for bit, every tick.
    let mut dry = floored_region();
    drop(&mut dry, [24, (FLOOR_TOP / B).ceil() as i32, 24]);
    let e = only_body(&dry);
    let mass = mass_properties(&cube(), BODY_DEFAULT_DENSITY).mass();
    let weight = Vector::new(0.0, -mass * BODY_GRAVITY * UNITS_PER_METER, 0.0);
    for t in 0..BODY_SETTLE_TICKS {
        tick(&mut dry);
        let b = live_body(&dry, e);
        assert_eq!(b.user_force().to_array().map(f32::to_bits), weight.to_array().map(f32::to_bits), "tick {t}: weight");
        assert_eq!(b.linear_damping().to_bits(), 0.0f32.to_bits(), "tick {t}: no drag");
        assert_eq!(b.angular_damping().to_bits(), BODY_ANGULAR_DAMPING.to_bits(), "tick {t}: angular damping");
    }
}
