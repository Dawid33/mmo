//! The generic dynamic body (`game::body`, `EntityKind::Body`): it falls
//! under the world's gravity, comes to rest asleep on the floor, and every
//! tick of its tumble rolls back bit-exact.
use crate::common::{floor_chunks, install_real_block_props, r0};
use game::body::BODY_SETTLE_TICKS;
use game::rapier3d::math::Vector;
use game::{
    state_hash, ColliderSpec, EntityKey, EntityKind, GameEventKind, Pose, Region, Rollback, BLOCK_VOXELS, REGION_SIZE,
};

const B: f32 = BLOCK_VOXELS as f32;
/// The floor's top surface (one block deep from y 0), units.
const FLOOR_TOP: f32 = B;

/// A region floored one block deep edge to edge, no players.
pub fn floored_region() -> Region {
    install_real_block_props();
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(floor_chunks(0.0..REGION_SIZE, 0.0..REGION_SIZE, BLOCK_VOXELS as u32)));
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
    let rest = FLOOR_TOP + B / 2.0;
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
/// above the tile, spinning a little (at most
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
    let at = [20, 1, 24];
    drop(&mut region, at);
    let all = bodies(&region);
    assert_eq!(all.len(), 1, "one body dropped");
    let b = live_body(&region, all[0]);
    let t = b.translation();
    let want = Vector::new(20.5 * B, (1 + DEV_DROP_HEIGHT_BLOCKS) as f32 * B + B / 2.0, 24.5 * B);
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
    drop(&mut region, [-1, 1, 24]);
    drop(&mut region, [game::REGION_BLOCKS as i32, 1, 24]);
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
