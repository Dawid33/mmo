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
