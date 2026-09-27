//! Upgrade plan Task 4.4 Step 3: `crates/game/tests/voxels_hash_cache.rs` on
//! the glam-era parry fork. `Voxels`' `StateHash` feeds `(voxel_size,
//! content_hash)`, where the content hash is a cached digest of (sorted chunk
//! key → voxel states), maintained by every mutator — so hashing a voxel shape
//! never walks its voxels, and equal content hashes equal whatever the
//! allocation history.
//!
//! Beyond the old file: the neighbour-state mutators (`propagate_voxel_change`,
//! `combine_voxel_states`, which the old fork did not track) and history
//! independence (same content reached by different edit orders).
use std::hash::Hasher;
use std::time::Instant;

use rapier3d::parry::math::{IVector, Vector};
use rapier3d::parry::shape::Voxels;
use state_hash::StateHash;

fn vhash(v: &Voxels) -> u64 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finish()
}

fn cube(n: i32) -> Voxels {
    let mut coords = Vec::new();
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                coords.push(IVector::new(x, y, z));
            }
        }
    }
    Voxels::new(Vector::splat(1.0), &coords)
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
    v.set_voxel(IVector::new(3, 3, 3), false);
    let h1 = vhash(&v);
    assert_ne!(h0, h1);
    // ...and restoring the same content restores the same hash
    // (content-based, not history-based — required by the rollback bar).
    v.set_voxel(IVector::new(3, 3, 3), true);
    assert_eq!(h0, vhash(&v));
    // Different content, different hash (probabilistically).
    let mut w = cube(8);
    w.set_voxel(IVector::new(0, 0, 0), false);
    assert_ne!(vhash(&v), vhash(&w));
}

#[test]
fn equal_content_hashes_equal_whatever_the_history() {
    // A chunk emptied and refilled lands in a recycled slot: different
    // allocation order and BVH, same content.
    let mut a = cube(4);
    let far = IVector::new(100, 0, 0);
    a.set_voxel(far, true);
    a.set_voxel(far, false);
    a.set_voxel(IVector::new(200, 0, 0), true);
    let mut b = cube(4);
    b.set_voxel(IVector::new(200, 0, 0), true);
    assert_eq!(vhash(&a), vhash(&b));
    let coords: Vec<_> = b.voxels().filter(|v| !v.state.is_empty()).map(|v| v.grid_coords).collect();
    assert_eq!(vhash(&Voxels::new(Vector::splat(1.0), &coords)), vhash(&b));
}

#[test]
fn voxel_size_is_hashed() {
    let a = cube(2);
    let mut b = cube(2);
    b.set_voxel_size(Vector::splat(2.0));
    assert_ne!(vhash(&a), vhash(&b));
}

#[test]
fn neighbour_state_merges_change_the_hash() {
    // Two shapes side by side: merging their boundary states sets the
    // neighbour bits of the facing voxels in both.
    let mut a = cube(2);
    let mut b = cube(2);
    let (ha, hb) = (vhash(&a), vhash(&b));
    a.combine_voxel_states(&mut b, IVector::new(2, 0, 0));
    assert_ne!(vhash(&a), ha, "combine_voxel_states must refresh self's content hash");
    assert_ne!(vhash(&b), hb, "combine_voxel_states must refresh other's content hash");
    let fresh_a = Voxels::new(Vector::splat(1.0), &a.voxels().filter(|v| !v.state.is_empty()).map(|v| v.grid_coords).collect::<Vec<_>>());
    assert_ne!(vhash(&a), vhash(&fresh_a), "the neighbour bits are content");

    let mut c = cube(2);
    let mut d = cube(2);
    let (hc, hd) = (vhash(&c), vhash(&d));
    d.set_voxel(IVector::new(0, 0, 0), false);
    let hd_edited = vhash(&d);
    assert_ne!(hd_edited, hd);
    c.set_voxel(IVector::new(2, 0, 0), true);
    let hc_edited = vhash(&c);
    assert_ne!(hc_edited, hc);
    c.propagate_voxel_change(&mut d, IVector::new(2, 0, 0), IVector::new(2, 0, 0));
    assert_ne!(vhash(&d), hd_edited, "propagate_voxel_change must refresh other's content hash");
    assert_ne!(vhash(&c), hc_edited, "propagate_voxel_change must refresh self's content hash");
}

#[test]
fn hashing_is_cheap_regardless_of_voxel_count() {
    // 64^3 = 262k voxels. A content walk is ~1 ms even in release; 2000 walks
    // would exceed 1 s by a wide margin. The cached hash is a few field hashes.
    let v = cube(64);
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..2000 {
        acc = acc.wrapping_add(vhash(&v));
    }
    assert!(acc != 0, "keep the loop from being optimized out");
    assert!(t.elapsed().as_millis() < 500, "2000 hashes took {:?} — Voxels::state_hash still walks contents", t.elapsed());
}
