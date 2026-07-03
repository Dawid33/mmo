//! Documents which containers can undo an insert by removing the item.
//!
//! SlotMap and rapier's arenas hash slot versions / free lists, so
//! remove(insert(x)) does NOT restore the pre-insert state — undo closures for
//! those must restore a full snapshot instead (see Undo::change /
//! create_entity_safe). If a vendored fork changes this behavior, these tests
//! flag it.
use std::hash::Hash;

use rapier3d::prelude::{
    ColliderSet, ImpulseJointSet, IslandManager, MultibodyJointSet, RigidBodyBuilder, RigidBodySet,
};
use slotmapd::{new_key_type, SlotMap, SparseSecondaryMap};

fn h<T: Hash>(t: &T) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    t.hash(&mut hasher);
    hasher.finalize()
}

new_key_type! { struct K; }

#[test]
fn slotmap_remove_does_not_restore_hash() {
    let mut m: SlotMap<K, ()> = SlotMap::with_key();
    let before = h(&m);
    let k = m.insert(());
    m.remove(k);
    assert_ne!(before, h(&m), "slot versions/free list are part of the hash");
}

#[test]
fn sparse_secondary_remove_restores_hash() {
    let mut owner: SlotMap<K, ()> = SlotMap::with_key();
    let k = owner.insert(());
    let mut m: SparseSecondaryMap<K, Option<u32>> = SparseSecondaryMap::new();
    let before = h(&m);
    m.insert(k, Some(5));
    m.remove(k);
    assert_eq!(before, h(&m));
}

#[test]
fn slotmap_revert_insert_restores_hash_exactly() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    // Build some history so the free list is non-trivial.
    let a = m.insert(1);
    let _b = m.insert(2);
    m.remove(a);
    let before = h(&m);

    // Reuse-case insert (free list non-empty) then revert.
    let k = m.insert(3);
    m.revert_insert(k);
    assert_eq!(before, h(&m), "reuse-case revert_insert must be exact");

    // Append-case insert (drain free list first) then revert.
    let c = m.insert(4); // reuses slot a
    let before2 = h(&m);
    let d = m.insert(5); // appends
    m.revert_insert(d);
    assert_eq!(before2, h(&m), "append-case revert_insert must be exact");
    let _ = c;
}

#[test]
fn slotmap_revert_remove_restores_hash_exactly() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    let a = m.insert(1);
    let _b = m.insert(2);
    let before = h(&m);
    let v = m.remove(a).unwrap();
    m.revert_remove(a, v);
    assert_eq!(before, h(&m), "revert_remove must be exact");
    // The key must still resolve to the restored value.
    assert_eq!(m.get(a).copied(), Some(1));
}

#[test]
fn slotmap_lifo_revert_chain_restores_hash() {
    let mut m: SlotMap<K, u32> = SlotMap::with_key();
    let a = m.insert(1);
    m.remove(a);
    let before = h(&m);
    // insert, insert, remove — revert in reverse order.
    let x = m.insert(10);
    let y = m.insert(20);
    let vy = m.remove(y).unwrap();
    m.revert_remove(y, vy);
    m.revert_insert(y);
    m.revert_insert(x);
    assert_eq!(before, h(&m), "LIFO revert chain must be exact");
}

#[test]
fn rigidbodyset_revert_insert_restores_hash_exactly() {
    let mut bodies = RigidBodySet::new();
    // Slow path (empty arena grows storage).
    let s0 = h(&bodies);
    let st = bodies.alloc_state();
    let h1 = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.revert_insert(h1, st.0, st.1);
    assert_eq!(s0, h(&bodies), "slow-path (grow) revert must be exact");

    // Build history: occupied + freed slot, then fast-path insert + revert.
    let a = bodies.insert(RigidBodyBuilder::fixed().build());
    let mut islands = IslandManager::new();
    let mut colliders = ColliderSet::new();
    let mut ij = ImpulseJointSet::new();
    let mut mj = MultibodyJointSet::new();
    let _b = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.remove(a, &mut islands, &mut colliders, &mut ij, &mut mj, true);
    let s1 = h(&bodies);
    let st = bodies.alloc_state();
    let c = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.revert_insert(c, st.0, st.1);
    assert_eq!(s1, h(&bodies), "fast-path (reuse) revert must be exact");
}

#[test]
fn rigidbodyset_remove_does_not_restore_hash() {
    let mut bodies = RigidBodySet::new();
    let mut islands = IslandManager::new();
    let mut colliders = ColliderSet::new();
    let mut ij = ImpulseJointSet::new();
    let mut mj = MultibodyJointSet::new();
    let before = h(&bodies);
    let handle = bodies.insert(RigidBodyBuilder::fixed().build());
    bodies.remove(handle, &mut islands, &mut colliders, &mut ij, &mut mj, true);
    assert_ne!(before, h(&bodies), "arena generation/free list are hashed");
}
