//! Upgrade plan Task 4.4 Step 3: the forks' `StateHash` over every journaled
//! physics structure is the same number on native and on wasm32 (node, via
//! wasm-bindgen-test-runner) — `StateHash` pins every width (`usize` as u64),
//! where std `Hash` did not (Task 0.3), and the sim must agree bit for bit.
//!
//! The scene is small and uses no worldgen: a `Voxels` floor (the game's
//! terrain shape), a kinematic capsule walking over it, dynamic balls, a
//! jointed pair, and a dynamic body whose collider sits off its centre of mass
//! — its mass properties go through glamx's `SymmetricEigen3` (Review Focus 3:
//! std `acos`/`cos` there would be the first thing to differ across targets).
//!
//! Run both sides with `just determinism-wasm`. The constant changes only
//! in a commit whose message says why; print it with
//!
//! ```sh
//! STATE_HASH_WASM_PRINT=1 just test-one physics-next "--test state_hash_wasm -- --nocapture"
//! ```
use physics_next::{PhysicsWorld, STATE_PARTS};
use rapier3d::parry::math::IVector;
use rapier3d::prelude::*;

/// crc32 of the `StateHash` of all eight parts, in `PhysicsParts` order,
/// after [`SCENE_TICKS`] ticks, on every target.
const SCENE_STATE_HASH: u32 = 0x45F9_F5BD;
const SCENE_TICKS: usize = 120;

/// Runs the scene; returns the combined hash and each part's own.
fn scene() -> (u32, [u64; 8]) {
    let mut w = PhysicsWorld { gravity: Vector::new(0.0, -9.81, 0.0), ..Default::default() };
    w.params.dt = 0.05;
    let floor = w.bodies.insert(RigidBodyBuilder::fixed());
    let keys: Vec<IVector> = (0..24).flat_map(|x| (0..24).map(move |z| IVector::new(x, 0, z))).collect();
    w.colliders.insert_with_parent(ColliderBuilder::voxels(Vector::splat(1.0), &keys), floor, &mut w.bodies);
    for i in 0..3 {
        let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(6.0 + i as f32, 2.0 + i as f32, 6.0)));
        w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    }
    let a = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(12.0, 1.5, 4.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), a, &mut w.bodies);
    let b = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(13.2, 1.5, 4.0)));
    w.colliders.insert_with_parent(ColliderBuilder::ball(0.4), b, &mut w.bodies);
    let joint = SphericalJointBuilder::new().local_anchor1(Vector::new(0.6, 0.0, 0.0)).local_anchor2(Vector::new(-0.6, 0.0, 0.0));
    w.impulse_joints.insert(a, b, joint, true);
    // A tilted box off its body's origin: a non-diagonal inertia tensor.
    let offset = w.bodies.insert(RigidBodyBuilder::dynamic().translation(Vector::new(16.0, 3.0, 12.0)));
    let tilt = Pose::from_parts(Vector::new(0.7, 0.2, -0.3), Rotation::from_scaled_axis(Vector::new(0.4, 0.9, 0.2)));
    w.colliders.insert_with_parent(ColliderBuilder::cuboid(0.8, 0.3, 0.5).position(tilt), offset, &mut w.bodies);
    let k = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(Vector::new(2.0, 1.9, 6.0)));
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(0.5, 0.4), k, &mut w.bodies);

    let mut pipeline = PhysicsPipeline::new();
    for tick in 0..SCENE_TICKS {
        let x = 2.0 + (tick as f32 * 0.15).min(16.0);
        w.bodies.get_mut(k).unwrap().set_next_kinematic_translation(Vector::new(x, 1.9, 6.0));
        w.step(&mut pipeline);
    }
    assert!(w.bodies[k].translation().x > 17.0, "the capsule must walk the floor");
    assert!(w.bodies[offset].translation().y < 3.0, "the offset body must fall");
    assert!(w.narrow_phase.contact_pairs().next().is_some(), "the scene must be in contact");

    let parts = w.state_hashes();
    let mut all = crc32fast::Hasher::new();
    for p in parts {
        all.update(&p.to_le_bytes());
    }
    let combined = all.finalize();
    if print_hash() {
        log(&format!("SCENE_STATE_HASH = 0x{combined:08X}"));
        for (name, p) in STATE_PARTS.iter().zip(parts) {
            log(&format!("  {name}: 0x{p:08X}"));
        }
    }
    (combined, parts)
}

/// Native prints on request; wasm32 always prints (node has no env to ask).
#[cfg(not(target_arch = "wasm32"))]
fn print_hash() -> bool {
    std::env::var_os("STATE_HASH_WASM_PRINT").is_some()
}

#[cfg(target_arch = "wasm32")]
fn print_hash() -> bool {
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn log(line: &str) {
    println!("{line}");
}

/// `println!` goes nowhere on wasm32-unknown-unknown; the runner shows the
/// console.
#[cfg(target_arch = "wasm32")]
fn log(line: &str) {
    wasm_bindgen_test::console_log!("{line}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scene_state_hash_native() {
    let (hash, _) = scene();
    assert_eq!(hash, SCENE_STATE_HASH, "native scene StateHash 0x{hash:08X} != golden 0x{SCENE_STATE_HASH:08X}");
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn scene_state_hash_wasm() {
    let (hash, _) = scene();
    assert_eq!(hash, SCENE_STATE_HASH, "wasm32 scene StateHash 0x{hash:08X} != golden 0x{SCENE_STATE_HASH:08X}");
}
