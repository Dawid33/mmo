//! Invariants for the cached content hash on the vendored parry Voxels
//! shape. See docs/superpowers/specs/2026-07-04-scalable-undo-hashing-design.md
use std::hash::{Hash, Hasher};
use std::time::Instant;

use game::na::{Point3, Vector3};
use game::parry::math::Real;
use game::parry::shape::Voxels;

fn vhash(v: &Voxels) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn cube(n: i32) -> Voxels {
    let mut coords = Vec::new();
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                coords.push(Point3::new(x, y, z));
            }
        }
    }
    Voxels::new(Vector3::repeat(Real::from(1.0)), &coords)
}

#[test]
fn hash_stable_across_clone_and_serde() {
    let v = cube(8);
    assert_eq!(vhash(&v), vhash(&v.clone()));
    let bytes = bincode::serialize(&v).unwrap();
    let de: Voxels = bincode::deserialize(&bytes).unwrap();
    assert_eq!(vhash(&v), vhash(&de));
}

#[test]
fn hash_tracks_content_edits_exactly() {
    let mut v = cube(8);
    let h0 = vhash(&v);
    // Removing a voxel changes the hash...
    v.set_voxel(Point3::new(3, 3, 3), false);
    let h1 = vhash(&v);
    assert_ne!(h0, h1);
    // ...and restoring the same content restores the same hash
    // (content-based, not history-based — required by the rollback bar).
    v.set_voxel(Point3::new(3, 3, 3), true);
    assert_eq!(h0, vhash(&v));
    // Different content, different hash (probabilistically).
    let mut w = cube(8);
    w.set_voxel(Point3::new(0, 0, 0), false);
    assert_ne!(vhash(&v), vhash(&w));
}

#[test]
fn hashing_is_cheap_regardless_of_voxel_count() {
    // 64^3 = 262k voxels. Pre-fix, one Hash walk costs ~1 ms in debug
    // (measured via perf on the live server); 2000 walks would exceed 1 s
    // by a wide margin. Post-fix a walk is a few field hashes.
    let v = cube(64);
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..2000 {
        acc = acc.wrapping_add(vhash(&v));
    }
    assert!(acc != 0, "keep the loop from being optimized out");
    assert!(
        t.elapsed().as_millis() < 500,
        "2000 hashes took {:?} — Voxels::hash still walks contents",
        t.elapsed()
    );
}
