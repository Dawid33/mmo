//! Upgrade plan Task 4.4 Step 3: the forks' `StateHash` agrees with the
//! serialized state it summarises.
//!
//! The failure modes are a hash that differs for logically equal states (it
//! hashes a cache, or something that doesn't survive a round trip) and one
//! that agrees for different states (it misses a field). So, per journaled
//! structure (`PhysicsWorld::state_hashes`, the seven parts plus the empty
//! soft-body set):
//! - a bincode round trip (parking a region, a snapshot on the wire) keeps
//!   every hash (Review Focus 5);
//! - between consecutive ticks, the bytes changed iff the hash changed;
//! - `dyn Shape` hashes its concrete type, not just its fields.
//!
//! The journal fuzzer (`journal_fuzz.rs`) runs the same byte/hash agreement
//! check over its wake/sleep/merge/removal/joint ticks.
use physics_next::{PhysicsWorld, STATE_PARTS, hash_disagreements};
use rapier3d::parry::math::IVector;
use rapier3d::parry::shape::{Cone, Cylinder, SharedShape};
use rapier3d::prelude::*;
use state_hash::StateHash;

fn crc<T: StateHash + ?Sized>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finish()
}

/// A voxels floor (the game's terrain shape), a cuboid tile, a few dynamic
/// balls, a jointed pair and a kinematic capsule pushing through them.
fn scene() -> (PhysicsWorld, RigidBodyHandle) {
    let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    w.params.dt = 0.05;
    let floor = w.bodies.insert(RigidBodyBuilder::fixed());
    let keys: Vec<IVector> = (0..16).flat_map(|x| (0..16).map(move |z| IVector::new(x, 0, z))).collect();
    w.colliders.insert_with_parent(ColliderBuilder::voxels(Vector::splat(1.0), &keys), floor, &mut w.bodies);
    let tile = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(24.0, 0.0, 8.0)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(8.0, 1.0, 8.0), tile, &mut w.bodies);
    for i in 0..4 {
        let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(4.0 + i as f32 * 1.1, 2.0 + i as f32, 8.0)));
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    }
    let a = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(10.0, 1.5, 4.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), a, &mut w.bodies);
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(11.2, 1.5, 4.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    let joint = SphericalJointBuilder::new().local_anchor1(Vector::new(0.6, 0.0, 0.0)).local_anchor2(Vector::new(-0.6, 0.0, 0.0));
    w.impulse_joints.insert(a, b, joint, true);
    let k = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(Vector::new(0.0, 1.9, 8.0)));
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(0.5, 0.4), k, &mut w.bodies);
    (w, k)
}

fn round_trip(w: &PhysicsWorld) -> PhysicsWorld {
    fn rt<T: serde::Serialize + serde::de::DeserializeOwned>(t: &T) -> T {
        bincode::deserialize(&bincode::serialize(t).unwrap()).unwrap()
    }
    PhysicsWorld {
        gravity: w.gravity,
        params: w.params,
        islands: rt(&w.islands),
        broad_phase: rt(&w.broad_phase),
        narrow_phase: rt(&w.narrow_phase),
        bodies: rt(&w.bodies),
        colliders: rt(&w.colliders),
        impulse_joints: rt(&w.impulse_joints),
        multibody_joints: rt(&w.multibody_joints),
        soft_bodies: rt(&w.soft_bodies),
        ccd: CCDSolver::new(),
    }
}

/// Gate: mmo-fork StateHash hunks: serde-skipped workspaces #[state_hash(skip)], pairs/joint_link_locs hashed sorted (broad_phase_bvh, persistent.rs) — round trip keeps every hash (57c11c39)
#[test]
fn round_trip_keeps_every_hash_and_bytes_track_hashes_tick_to_tick() {
    let (mut w, k) = scene();
    let mut pipeline = PhysicsPipeline::new();
    // Before the first step: joint island events are pending (serde-skipped).
    let mut prev = (w.state_bytes(), w.state_hashes());
    for tick in 0..60 {
        let x = (tick as f32 * 0.4).min(20.0);
        w.bodies.get_mut(k).unwrap().set_next_kinematic_translation(Vector::new(x, 1.9, 8.0));
        w.step(&mut pipeline);
        let now = (w.state_bytes(), w.state_hashes());
        let rt = round_trip(&w);
        assert_eq!(rt.state_bytes(), now.0, "tick {tick}: bincode round trip is not a fixed point");
        let lost: Vec<_> = (0..8).filter(|&i| rt.state_hashes()[i] != now.1[i]).map(|i| STATE_PARTS[i]).collect();
        assert!(lost.is_empty(), "tick {tick}: a bincode round trip changed the hash of {lost:?}");
        let bad = hash_disagreements(&prev, &now);
        assert!(bad.is_empty(), "tick {tick}: bytes and hash disagree on whether {bad:?} changed");
        prev = now;
    }
}

/// Gate: mmo-fork hunk parry shape.rs:444 'StateHash — the concrete shape's type first' / DynStateHash supertrait (57c11c39)
#[test]
fn dyn_shape_hashes_its_concrete_type() {
    // Identical field bytes (half_height, radius), different shapes.
    let cone = SharedShape::new(Cone::new(1.0, 2.0));
    let cylinder = SharedShape::new(Cylinder::new(1.0, 2.0));
    assert_ne!(crc(&cone), crc(&cylinder));
    assert_eq!(crc(&cone), crc(&SharedShape::new(Cone::new(1.0, 2.0))));
    assert_ne!(crc(&cone), crc(&SharedShape::new(Cone::new(1.0, 2.5))));
}

/// Gate: mmo-fork hunk parry bvh_tree.rs:1438 BvhNodeVec hand StateHash (length + node byte image) (57c11c39)
#[test]
fn bvh_hash_tracks_tree_changes() {
    use rapier3d::parry::bounding_volume::Aabb;
    use rapier3d::parry::partitioning::Bvh;
    let aabb = |i: u32| Aabb::new(Vector::new(i as f32 * 2.0, 0.0, 0.0), Vector::new(i as f32 * 2.0 + 1.0, 1.0, 1.0));
    let tree = |n: u32| {
        let mut t = Bvh::new();
        for i in 0..n {
            t.insert(aabb(i), i);
        }
        t
    };
    let t = tree(16);
    assert_eq!(crc(&t), crc(&tree(16)), "identical op sequences");
    assert_eq!(crc(&t), crc(&t.clone()));
    let mut moved = t.clone();
    moved.insert(aabb(100), 3);
    assert_ne!(crc(&t), crc(&moved), "a moved leaf");
    let mut removed = t.clone();
    removed.remove(5);
    assert_ne!(crc(&t), crc(&removed), "a removed leaf");
}

/// Gate: mmo-fork hunk 'StateHash — hashed although serde-skipped: pending work' in impulse/multibody joint sets (57c11c39)
/// `ImpulseJointSet::island_events` is serde-skipped but is pending work the
/// next step consumes (H7): two sets with equal bytes, one with a pending
/// joint-created event, are different states and must hash differently.
#[test]
fn pending_joint_work_is_hashed_although_not_serialized() {
    let mut bodies = RigidBodySet::new();
    let a = bodies.insert(RigidBodyBuilder::dynamic());
    let b = bodies.insert(RigidBodyBuilder::dynamic());
    let mut joints = ImpulseJointSet::new();
    joints.insert(a, b, SphericalJointBuilder::new(), true);
    let round_tripped: ImpulseJointSet = bincode::deserialize(&bincode::serialize(&joints).unwrap()).unwrap();
    assert_eq!(bincode::serialize(&round_tripped).unwrap(), bincode::serialize(&joints).unwrap());
    assert_ne!(crc(&joints), crc(&round_tripped), "the pending island event is state");
}

/// The scene without the voxels floor: `Voxels` hashes its content digest,
/// not its chunk BVH or free lists, by design (`voxels_hash_cache.rs`).
fn cuboid_scene() -> PhysicsWorld {
    let (mut w, k) = scene();
    let floor = w.colliders.iter().find(|(_, c)| c.shape().as_voxels().is_some()).unwrap().0;
    w.colliders.remove(floor, &mut w.islands, &mut w.bodies, &mut w.soft_bodies, true);
    let tile = w.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(8.0, 0.0, 8.0)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(8.0, 1.0, 8.0), tile, &mut w.bodies);
    let mut pipeline = PhysicsPipeline::new();
    for tick in 0..20 {
        w.bodies.get_mut(k).unwrap().set_next_kinematic_translation(Vector::new(tick as f32 * 0.4, 1.9, 8.0));
        w.step(&mut pipeline);
    }
    w
}

fn stepped_hashes(w: &PhysicsWorld) -> Option<[u64; 8]> {
    let mut w = round_trip(w);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        w.step(&mut PhysicsPipeline::new());
        w.state_hashes()
    }))
    .ok()
}

/// Gate: mmo-fork StateHash derives (all ~200 field-level hunks): byte-flip check that no serialized field is missed by the hash (57c11c39)
/// A missed field hides behind fields that change every tick (poses, the
/// broad phase's `frame_index`), so this flips each serialized byte of each
/// part in turn: either the hash sees the change, or the change is not state
/// — the flipped world steps to the same hashed future (e.g. glamx
/// `Pose3::padding`, "never read"). A flip that doesn't deserialize, or panics
/// the step, is a different state and needs nothing.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_serialized_byte_is_seen_by_the_hash_or_changes_nothing() {
    let w = cuboid_scene();
    let base = w.state_hashes();
    let base_future = stepped_hashes(&w).expect("the base world steps");
    let bytes = w.state_bytes();
    let mut unseen = Vec::new();
    let mut invisible = 0;
    // Not the soft-body set: its hash refuses a non-empty set (the game has none).
    for part in 0..7 {
        for i in 0..bytes[part].len() {
            let mut flipped = bytes[part].clone();
            flipped[i] ^= 1;
            let mut v = round_trip(&w);
            let ok = match part {
                0 => bincode::deserialize(&flipped).map(|x| v.islands = x).is_ok(),
                1 => bincode::deserialize(&flipped).map(|x| v.broad_phase = x).is_ok(),
                2 => bincode::deserialize(&flipped).map(|x| v.narrow_phase = x).is_ok(),
                3 => bincode::deserialize(&flipped).map(|x| v.bodies = x).is_ok(),
                4 => bincode::deserialize(&flipped).map(|x| v.colliders = x).is_ok(),
                5 => bincode::deserialize(&flipped).map(|x| v.impulse_joints = x).is_ok(),
                _ => bincode::deserialize(&flipped).map(|x| v.multibody_joints = x).is_ok(),
            };
            if !ok || v.state_hashes()[part] != base[part] {
                continue;
            }
            invisible += 1;
            if let Some(future) = stepped_hashes(&v) {
                if future != base_future {
                    unseen.push((STATE_PARTS[part], i));
                }
            }
        }
    }
    assert!(unseen.is_empty(), "flips the hash can't see but the next step can: {unseen:?} (of {invisible} hash-invisible flips)");
}
