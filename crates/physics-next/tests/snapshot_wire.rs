//! Fork hunks behind the game's slim join snapshot (`game/src/region_wire.rs`), which
//! leaves derivable `Voxels` collider shapes off the wire and rebuilds them on receipt:
//!
//! * `// mmo-fork: snapshot wire` in rapier's `collider.rs`: `Collider::swap_shape_raw`
//!   swaps a shape without touching change tracking (`set_shape` flags `SHAPE`).
//! * `// mmo-fork: leaves in chunk-id order` in parry's `voxels.rs`: `Voxels::new`
//!   builds its chunk BVH from the chunks in first-seen order, not hash-map order
//!   (which differs between native and wasm32), so a build is a pure function of its
//!   input on every target. A 2-leaf tree keeps its input order, which is what shows.
use std::hash::Hasher;

use rapier3d::parry::shape::Voxels;
use rapier3d::prelude::*;
use state_hash::StateHash;

fn hash_of(c: &Collider) -> u64 {
    let mut h = crc32fast::Hasher::new();
    c.state_hash(&mut h);
    h.finish()
}

#[test]
fn swap_shape_raw_leaves_change_tracking_alone() {
    let keys = [IVector::new(0, 0, 0), IVector::new(1, 0, 0)];
    let shape = || SharedShape::voxels(Vector::new(1.0, 1.0, 1.0), &keys);
    let mut c = ColliderBuilder::new(shape()).build();
    let before = hash_of(&c);
    let bytes = bincode::serialize(&c).unwrap();
    let old = c.swap_shape_raw(SharedShape::ball(0.0));
    assert!(old.as_voxels().is_some());
    assert_ne!(hash_of(&c), before, "the placeholder is another shape");
    c.swap_shape_raw(shape());
    assert_eq!(hash_of(&c), before, "an equal shape back: the same collider, flags included");
    assert!(bincode::serialize(&c).unwrap() == bytes);
}

#[test]
fn voxels_new_orders_its_chunks_as_first_seen() {
    // Two keys in two parry chunks (8 voxels wide), in both input orders.
    let (a, b) = (IVector::new(0, 0, 0), IVector::new(20, 0, 0));
    let first = |keys: &[IVector]| {
        let v = Voxels::new(Vector::new(1.0, 1.0, 1.0), keys);
        v.voxels().next().expect("a voxel").grid_coords
    };
    assert_eq!(first(&[a, b]), a);
    assert_eq!(first(&[b, a]), b);
}
