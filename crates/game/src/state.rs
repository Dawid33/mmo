//! The rollbackable simulation state: `GameData` plus the `#[rollback]`
//! invocation that generates the undo/transaction infrastructure
//! (`Rollback`, `Undo`, `UndoCell`, `UndoMap`, `UndoSlotMap`, ...) for it.
//! The generated code assumes `GameDataUpdate`/`GameDataUpdateKind`/
//! `GameDataTransactionKind` and `serde` are reachable at the crate root —
//! `lib.rs` re-exports this module to satisfy that.

use nalgebra as na;
use na::{Perspective3, Quaternion, Translation3, Unit};
use macros::rollback;
use block_mesh::ndshape::ConstShape;
use parry3d::math::{Point, RawReal, Real, Vector};
use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
use slotmapd::Key as _;
use slotmapd::{new_key_type, SlotMap, SparseSecondaryMap};
use std::sync::{Arc, atomic::AtomicUsize};

use crate::camera::Camera;
use crate::input::InputState;
use crate::protocol::{ColliderSpec, EntityBundle, GhostData, GHOST_TTL_TICKS};
use crate::registry::BlockId;
use crate::voxel::{Chunk, ChunkCoords, ChunkShape, Voxel};

pub type IsometryReal = na::Isometry<Real, na::Unit<na::Quaternion<Real>>, 3>;

pub type ClientId = usize;

new_key_type! { pub struct EntityKey; }
new_key_type! { pub struct PlayerKey; }

#[derive(Clone, Debug, Default)]
pub struct RollbackInfo {
    pub current: Arc<AtomicUsize>,
    pub oldest: Arc<AtomicUsize>,
}

impl RollbackInfo {
    pub fn new() -> Self {
        Self {
            current: Arc::new(AtomicUsize::new(0)),
            oldest: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone, Debug)]
pub enum GameDataUpdateKind {
    SetVoxelComponent(EntityKey, Option<Vec<Voxel>>),
    SetEntityPosition(EntityKey, IsometryReal),
    AddCameraComponent(EntityKey, ClientId, Perspective3<Real>, IsometryReal),
    RemoveCameraComponent(EntityKey),
    UpdateCameraViewProj(EntityKey, Perspective3<Real>),
    UpdateCameraViewMatrix(EntityKey, IsometryReal),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(ClientId, bool),
    SetEntityKind(EntityKey, Option<EntityKind>),
    /// Marks an entity as a ghost mirror of (source region, source key), or
    /// clears the mark on upgrade/expiry. The bridge uses it to hide ghosts
    /// whose source region is also loaded locally.
    SetGhostSource(EntityKey, Option<crate::protocol::RegionCoords>),
}

#[derive(Clone, Debug)]
pub struct GameDataUpdate {
    pub do_kind: GameDataTransactionKind,
    pub update_kind: GameDataUpdateKind,
}

impl GameDataUpdate {
    pub fn new(do_kind: GameDataTransactionKind, update_kind: GameDataUpdateKind) -> Self {
        Self {
            do_kind,
            update_kind,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

/// What an inventory slot holds. `Empty` is the default so `ItemStack` and
/// `Inventory` derive `Default` cleanly.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, Hash, PartialEq, Eq)]
pub enum ItemKind {
    #[default]
    Empty,
    Dirt,
    Stone,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, Hash, PartialEq)]
pub struct ItemStack {
    pub kind: ItemKind,
    pub count: u32,
}

/// A fixed-size slot list. Authoritative sim state, carried on `Client` so it
/// follows the player across region handoffs (Client rides `EntityBundle`).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize, Hash, PartialEq)]
pub struct Inventory {
    pub slots: Vec<Option<ItemStack>>,
}

impl Inventory {
    /// Nine empty hotbar slots.
    pub fn hotbar() -> Self {
        Self { slots: vec![None; 9] }
    }

    /// Nine slots seeded with test items (slice-1 render target).
    pub fn seeded() -> Self {
        let mut inv = Self::hotbar();
        inv.slots[0] = Some(ItemStack { kind: ItemKind::Dirt, count: 42 });
        inv.slots[3] = Some(ItemStack { kind: ItemKind::Stone, count: 7 });
        inv
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
    pub inventory: Inventory,
    pub inventory_open: bool,
}

/// What an entity *is*, for rendering and future gameplay. The player is the
/// first kind; NPC kinds extend this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, Hash)]
pub enum EntityKind {
    #[default]
    Player,
}

/// A live ghost mirror in this region, keyed in `GameData::ghosts` by its
/// source identity. `last_update_tick` drives the TTL reaper.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, Hash, PartialEq)]
#[module(crate)]
pub struct GhostEntry {
    pub entity: EntityKey,
    pub last_update_tick: usize,
}

#[rollback(GameData)]
mod game_data {
    use parry3d::math::{Real, Vector};
    use rapier3d::prelude::{
        CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
        IslandManager, MultibodyJointSet, NarrowPhase, RigidBodySet,
    };

    use super::{
        Camera, Chunk, Client, ClientId, Component, EntityKey, EntityKind, GhostEntry,
        IsometryReal, RigidBodyHandle, RollbackInfo, SlotMap,
    };
    use std::collections::BTreeMap;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        #[undo(cell)]
        tick: usize,
        #[undo(cell)]
        next_game_event_id: usize,
        #[undo(map)]
        player_entites: BTreeMap<ClientId, EntityKey>,
        #[undo(map)]
        clients: BTreeMap<ClientId, Client>,
        /// Ghost mirrors hosted here, by source identity.
        #[undo(map)]
        ghosts: BTreeMap<(crate::RegionCoords, EntityKey), GhostEntry>,
        /// Owned entities that arrived via handoff, by the identity they
        /// arrived under — makes replayed arrivals idempotent.
        #[undo(map)]
        arrivals: BTreeMap<(crate::RegionCoords, EntityKey), EntityKey>,
    }

    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<IsometryReal>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
        kind: Component<EntityKind>,
    }

    pub struct PhysicsState {
        bodies: RigidBodySet,
        broad_phase: DefaultBroadPhase,
        implules_joint_set: ImpulseJointSet,
        multi_body_joint_set: MultibodyJointSet,
        ccd_solver: CCDSolver,
        colliders: ColliderSet,
        gravity: Vector<Real>,
        integration_parameters: IntegrationParameters,
        islands: IslandManager,
        narrow_phase: NarrowPhase,
    }
}

pub use game_data::*;

impl GameData {
    pub fn into_game_update_iter(self) {}
}

impl Undo<Ecs> {
    pub fn create_entity_safe(&mut self) -> EntityKey {
        // CreateEntity / RemoveEntity emits ride the entities delta in both
        // directions — see #[emit] on the field.
        let key = self.entities.insert(());
        self.camera.insert_safe(key);
        self.isometry.insert_safe(key);
        self.rigidbody.insert_safe(key);
        self.chunk.insert_safe(key);
        self.kind.insert_safe(key);
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize, Hash)]
pub struct Component<T>
where
    T: 'static + Default,
{
    list: SparseSecondaryMap<EntityKey, Option<T>>,
}

/// crc32 of one component slot — the exact footprint of the single-key
/// insert_safe / set_safe / remove_safe mutations.
fn component_slot_digest<T: std::hash::Hash>(key: EntityKey, slot: Option<&Option<T>>) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    key.hash(&mut h);
    slot.hash(&mut h);
    h.finalize()
}

/// crc32 of one rigid-body slot + the set's allocator state — the exact
/// footprint of a journaled body insert (reverted via revert_insert).
fn body_slot_digest(
    alloc: (Option<u32>, u32),
    body: Option<&rapier3d::prelude::RigidBody>,
) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    alloc.hash(&mut h);
    body.hash(&mut h);
    h.finalize()
}

/// crc32 of the exact footprint of one `insert_with_parent`: the collider
/// set's allocator state, the inserted collider's slot, the parent body's
/// value (its collider list, mass properties and change flags all live
/// inside it), and whether the parent was already on the modified list
/// (insert_with_parent's tracking push is conditional on that).
fn collider_attach_digest(
    alloc: (Option<u32>, u32),
    slot: Option<&rapier3d::prelude::Collider>,
    parent: Option<&rapier3d::prelude::RigidBody>,
    parent_was_modified: bool,
) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    alloc.hash(&mut h);
    slot.hash(&mut h);
    parent.hash(&mut h);
    parent_was_modified.hash(&mut h);
    h.finalize()
}

/// crc32 of the exact footprint of one in-place body mutation through
/// `RigidBodySet::get_mut`: the body's value (pose, kinematic targets,
/// wake/sleep state and change flags all live inside it) and whether it was
/// already on the modified list (`get_mut`'s tracking push is conditional
/// on that).
fn body_write_digest(body: Option<&rapier3d::prelude::RigidBody>, was_modified: bool) -> u32 {
    use std::hash::Hash;
    let mut h = crc32fast::Hasher::new();
    body.hash(&mut h);
    was_modified.hash(&mut h);
    h.finalize()
}

impl Undo<rapier3d::prelude::RigidBodySet> {
    /// Mutate one existing body in place, undo-safely, with a footprint-sized
    /// log entry (no whole-`RigidBodySet` snapshot — `change()` here used to
    /// clone AND Full-hash every body in the set, per write, re-paid on every
    /// rollback/replay of the logging tick). The registered closure is the
    /// exact LIFO inverse of everything a `get_mut` + setter write touches:
    /// the body's own value (restored from the pre-write clone) and the set's
    /// modified-list membership iff this op's tracking push added it — the
    /// same restore_raw/pop_modified recipe as `attach_collider_safe`. Delta
    /// digests exactly that footprint; Full mode still whole-field-hashes at
    /// scope creation and verifies it at rollback, which is what proves the
    /// inverse misses nothing (tests/harnesses).
    pub fn mutate_body_safe(
        &mut self,
        handle: RigidBodyHandle,
        f: impl FnOnce(&mut rapier3d::prelude::RigidBody),
    ) {
        let pre_body = self
            .get(handle)
            .expect("mutate_body_safe: body missing")
            .clone();
        let was_modified = self.is_marked_modified(handle);
        let mut scope = self.undo_scope_digest();
        let pre = if scope.wants_delta_digest() {
            body_write_digest(Some(&pre_body), was_modified)
        } else {
            0
        };
        f(scope.get_mut(handle).expect("mutate_body_safe: body missing"));
        scope.register(pre, move |bodies, _| {
            bodies.restore_raw(handle, pre_body);
            if !was_modified {
                bodies.pop_modified(handle);
            }
            body_write_digest(bodies.get(handle), bodies.is_marked_modified(handle))
        });
    }
}

impl<T> Undo<Component<T>>
where
    T: 'static
        + Default
        + Clone
        + Send
        + std::hash::Hash
        + ::serde::Serialize
        + for<'a> ::serde::Deserialize<'a>,
{
    /// Creates the (empty) component entry for a new entity, undo-safely.
    /// SparseSecondaryMap remove(insert) is hash-exact (see hash_restore.rs),
    /// so a removal closure is a true inverse here.
    pub fn insert_safe(&mut self, key: EntityKey) {
        let pre = component_slot_digest::<T>(key, self.list.get(key)); // absent -> None
        self.undo_digest(pre, move |d, _| {
            d.list.remove(key);
            component_slot_digest(key, d.list.get(key))
        });
        self.raw_mut().list.insert(key, None);
    }

    pub fn set_safe(&mut self, key: EntityKey, item: Option<T>) {
        let old = self.list.get(key).cloned().unwrap();
        let pre = component_slot_digest(key, Some(&old));
        self.undo_digest(pre, move |d, _| {
            *d.list.get_mut(key).unwrap() = old.clone();
            component_slot_digest(key, d.list.get(key))
        });

        if let Some(item) = item {
            self.raw_mut().list.get_mut(key).unwrap().replace(item);
        } else {
            self.raw_mut().list.get_mut(key).unwrap().take();
        }
    }

    /// Removes an entity's slot from this component, undo-safely — the true
    /// inverse of `insert_safe` (do: remove the slot; undo: re-insert it with
    /// its prior value). `SparseSecondaryMap` insert(remove(x)) is hash-exact
    /// (see hash_restore.rs). Use this — not `set_safe(key, None)` — when an
    /// entity is destroyed: leaving a dangling `None` slot while the entities
    /// slotmap frees the index lets a same-transaction `create_entity_safe`
    /// reuse that index and clobber the slot, which is NOT LIFO-invertible
    /// (hash(before) != hash(after undo) on region-crossing reconcile).
    pub fn remove_safe(&mut self, key: EntityKey) {
        let old = self.list.get(key).cloned().unwrap();
        let pre = component_slot_digest(key, Some(&old));
        self.undo_digest(pre, move |d, _| {
            d.list.insert(key, old.clone());
            component_slot_digest(key, d.list.get(key))
        });
        self.raw_mut().list.remove(key);
    }
}

impl<T> Component<T>
where
    T: 'static + Default + Clone + Send,
{
    pub fn iter(&self) -> slotmapd::sparse_secondary::Iter<'_, EntityKey, Option<T>> {
        self.list.iter()
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn insert(&mut self, e: EntityKey, item: Option<T>) {
        self.list.insert(e, item);
    }

    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).unwrap()
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    pub fn try_get(&self, key: EntityKey) -> &Option<T> {
        self.list.get(key).unwrap()
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}

// Player spawn / capsule tunables (voxel units; 1 unit = 1/16 m). Exposed as
// named constants so they can be tuned without hunting through the spawn math.
/// Half-height of the player's capsule collider.
const PLAYER_CAPSULE_HALF_HEIGHT: f32 = 4.4;
/// Radius of the player's capsule collider.
const PLAYER_CAPSULE_RADIUS: f32 = 6.4;
/// Vertical gap left between the capsule bottom and the terrain surface at spawn.
const PLAYER_SPAWN_CLEARANCE: f32 = 3.6;
/// Region-local voxel column the player spawns over (region center: 256/2).
const PLAYER_SPAWN_X: f32 = 128.0;
const PLAYER_SPAWN_Z: f32 = 128.0;
/// Spawn height used only when the spawn column has no terrain (all air).
const PLAYER_SPAWN_FALLBACK_Y: f32 = 26.0;

impl Rollback {
    /// Runtime chunk insertion (undo-safe): two-scale decomposition only,
    /// unlike `seed_terrain_genesis`. Deliberately never collapses a
    /// fully-solid chunk to a single cuboid, so the same chunk content can
    /// yield a different collider shape depending on which path built it.
    /// That's safe because client and server always run the same path
    /// (create_mesh at runtime, seed_terrain_genesis at genesis) for the
    /// same chunk, so both sides still agree — but it's hash-relevant:
    /// unifying the two paths would shift state hashes.
    pub fn create_mesh(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        // Two-scale decomposition: whole solid blocks at block scale, chisel
        // detail at voxel scale. Covers exactly derive_voxels' solid cells.
        let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
        self.ecs.chunk.set_safe(e, Some(chunk));
        let position = IsometryReal::from_parts(
            Translation3::new(
                Real::from((coords.x * 32) as RawReal),
                Real::from((coords.y * 32) as RawReal),
                Real::from((coords.z * 32) as RawReal),
            ),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::fixed()
            .pose(position)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // Tier-2 with a true fork inverse + delta digest: reverts the arena
        // allocator exactly; verification hashes just this slot + alloc state.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope_digest();
        let handle = scope.insert(body);
        scope.register(
            body_slot_digest((prev_head, prev_len), None),
            move |bodies, _| {
                bodies.revert_insert(handle, prev_head, prev_len);
                body_slot_digest(bodies.alloc_state(), bodies.get(handle))
            },
        );
        self.ecs.rigidbody.set_safe(e, Some(handle));
        if !cells.blocks.is_empty() {
            self.attach_voxels_collider_safe(
                e,
                handle,
                &cells.blocks,
                Real::from(crate::BLOCK_VOXELS as RawReal),
            );
        }
        if !cells.voxels.is_empty() {
            self.attach_voxels_collider_safe(e, handle, &cells.voxels, Real::from(1.0));
        }
        e
    }

    /// Bulk terrain build for region genesis. Inserts every chunk's fixed body
    /// and collider WITHOUT the per-chunk whole-`PhysicsState` snapshot that
    /// `create_mesh` takes for undo-safe runtime edits. Genesis is the base
    /// state and is never rolled back below itself, so raw (unlogged) inserts
    /// are correct and turn the build from O(N^2) (a growing clone per chunk)
    /// into O(N). Fully-solid chunks get a single cuboid; partial chunks get
    /// up to two parry `Voxels` shapes — whole solid blocks at block scale
    /// (voxel_size = BLOCK_VOXELS) and chisel detail at voxel scale (1.0).
    /// A chunk solid purely via fully-occupied chisel no longer collapses to
    /// a cuboid (collision-identical, just uncollapsed).
    pub fn seed_terrain_genesis(&mut self, chunks: Vec<(ChunkCoords, Chunk)>) {
        for (coords, chunk) in chunks {
            let e = self.ecs.create_entity_safe();
            let cells = crate::collider_cells(&chunk.blocks, &chunk.chisel);
            self.ecs.chunk.set_safe(e, Some(chunk));
            let position = IsometryReal::from_parts(
                Translation3::new(
                    Real::from((coords.x * 32) as RawReal),
                    Real::from((coords.y * 32) as RawReal),
                    Real::from((coords.z * 32) as RawReal),
                ),
                Unit::<Quaternion<Real>>::identity(),
            );
            let body = RigidBodyBuilder::fixed()
                .pose(position)
                .user_data(e.data().as_ffi() as u128)
                .build();
            // Raw, unlogged insert: covered by the genesis base state.
            let handle = self.data.physics.raw_mut().bodies.raw_mut().insert(body);
            self.ecs.rigidbody.set_safe(e, Some(handle));
            if cells.is_empty() {
                continue;
            }
            let user_data = e.data().as_ffi() as u128;
            let mut colliders = Vec::with_capacity(2);
            if cells.is_full_cube() {
                // A fully-solid 32^3 chunk's collision surface is exactly a
                // 32x32x32 cuboid (half-extents 16, centered body-local 16,16,16).
                colliders.push(
                    ColliderBuilder::cuboid(Real::from(16.0), Real::from(16.0), Real::from(16.0))
                        .translation(Vector::new(
                            Real::from(16.0),
                            Real::from(16.0),
                            Real::from(16.0),
                        ))
                        .user_data(user_data)
                        .build(),
                );
            } else {
                // Two-scale decomposition: whole solid blocks at block scale
                // (voxel_size = BLOCK_VOXELS), chisel detail at voxel scale.
                if !cells.blocks.is_empty() {
                    colliders.push(
                        ColliderBuilder::voxels(
                            Vector::repeat(Real::from(crate::BLOCK_VOXELS as RawReal)),
                            &cells.blocks,
                        )
                        .user_data(user_data)
                        .build(),
                    );
                }
                if !cells.voxels.is_empty() {
                    colliders.push(
                        ColliderBuilder::voxels(Vector::repeat(Real::from(1.0)), &cells.voxels)
                            .user_data(user_data)
                            .build(),
                    );
                }
            }
            let phys = self.data.physics.raw_mut();
            for collider in colliders {
                phys.colliders
                    .raw_mut()
                    .insert_with_parent(collider, handle, phys.bodies.raw_mut());
            }
        }
    }

    /// Attach a voxel-grid collider to an entity's body. One cell per key;
    /// for voxel size `s`, cell g spans [g*s, (g+1)*s) in body-local space —
    /// scale 1.0 is the render mesh's voxel grid, scale `BLOCK_VOXELS` is
    /// the block grid. Entity-generic. Undo is the exact footprint inverse
    /// (see attach_collider_safe) — no whole-PhysicsState snapshot.
    pub fn attach_voxels_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        coords: &[Point<i32>],
        voxel_size: Real,
    ) {
        let collider = ColliderBuilder::voxels(Vector::repeat(voxel_size), coords)
            .user_data(e.data().as_ffi() as u128)
            .build();
        self.attach_collider_safe(body, collider);
    }

    /// Attach a capsule collider to an entity's body. Entity-generic: player
    /// today, NPCs later. Undo is the exact footprint inverse (see
    /// attach_collider_safe) — no whole-PhysicsState snapshot.
    pub fn attach_capsule_collider_safe(
        &mut self,
        e: EntityKey,
        body: RigidBodyHandle,
        half_height: f32,
        radius: f32,
    ) {
        let collider = ColliderBuilder::capsule_y(Real::from(half_height), Real::from(radius))
            .user_data(e.data().as_ffi() as u128)
            .build();
        self.attach_collider_safe(body, collider);
    }

    /// Attach `collider` to `body`, undo-safely, with a footprint-sized log
    /// entry (no whole-PhysicsState snapshot — that used to clone AND
    /// Full-hash the entire PhysicsState including the narrow-phase graphs,
    /// re-paid on every rollback/replay of the logging tick). The registered
    /// closure is the exact LIFO inverse of everything `insert_with_parent`
    /// touches: collider arena slot + alloc state + modified-list entry
    /// (ColliderSet::revert_insert), the parent body's value (collider list,
    /// mass props, change flags — all inside the clone), and the parent's
    /// modified-list membership iff this op's tracking push added it. Delta
    /// digests exactly that footprint; Full mode still whole-field-hashes at
    /// scope creation and verifies it at rollback, which is what proves the
    /// inverse misses nothing (tests/harnesses).
    fn attach_collider_safe(
        &mut self,
        body: RigidBodyHandle,
        collider: rapier3d::prelude::Collider,
    ) {
        let pre_parent = self
            .data
            .physics
            .bodies
            .get(body)
            .expect("attach_collider_safe: parent body missing")
            .clone();
        let parent_was_modified = self.data.physics.bodies.is_marked_modified(body);
        let (prev_head, prev_len) = self.data.physics.colliders.alloc_state();

        let mut scope = self.data.physics.undo_scope_digest();
        let pre = if scope.wants_delta_digest() {
            collider_attach_digest(
                (prev_head, prev_len),
                None, // slot vacant before the insert
                Some(&pre_parent),
                parent_was_modified,
            )
        } else {
            0
        };
        let handle = {
            let p = scope.raw_fields();
            p.colliders.insert_with_parent(collider, body, p.bodies)
        };
        scope.register(pre, move |phys, _| {
            let p = phys.raw_undo_parts();
            p.colliders.revert_insert(handle, prev_head, prev_len);
            p.bodies.restore_raw(body, pre_parent);
            if !parent_was_modified {
                p.bodies.pop_modified(body);
            }
            collider_attach_digest(
                p.colliders.alloc_state(),
                p.colliders.get(handle),
                p.bodies.get(body),
                p.bodies.is_marked_modified(body),
            )
        });
    }

    /// Highest solid voxel Y at region-local voxel column `(vx, vz)`, scanning
    /// the terrain chunks already built in this region. `None` if that column
    /// is all air or no chunk covers it. Voxel units — the same space as body
    /// and collider coordinates. Chunk entities don't store `ChunkCoords`, so
    /// the owning chunk column is recovered from each chunk body's translation
    /// (`coords * 32`). Pure read of built terrain, hence deterministic: client
    /// and server derive the same surface height.
    pub fn surface_voxel_y(&self, vx: i32, vz: i32) -> Option<i32> {
        let (cx, cz) = (vx.div_euclid(32), vz.div_euclid(32));
        let (lx, lz) = (vx.rem_euclid(32) as u32, vz.rem_euclid(32) as u32);
        let mut best: Option<i32> = None;
        for (key, chunk) in self.data.ecs.chunk.iter() {
            let Some(chunk) = chunk else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else {
                continue;
            };
            let Some(body) = self.data.physics.bodies.get(handle) else {
                continue;
            };
            let t = body.translation();
            let (bx, by, bz) = (t.x.0 as i32, t.y.0 as i32, t.z.0 as i32);
            if bx.div_euclid(32) != cx || bz.div_euclid(32) != cz {
                continue;
            }
            let voxels = crate::derive_voxels(&chunk.blocks, &chunk.chisel);
            for ly in (0..32u32).rev() {
                if voxels[ChunkShape::linearize([lx, ly, lz]) as usize].block != BlockId::AIR {
                    let abs = by + ly as i32;
                    best = Some(best.map_or(abs, |b| b.max(abs)));
                    break;
                }
            }
        }
        best
    }

    pub fn create_player_safe(&mut self, client_id: ClientId) {
        // Spawn on the terrain surface at the region-local spawn column, clear
        // of the capsule — real terrain has real elevation, so the old
        // hardcoded flat-floor height would bury the player. `surface + 1` is
        // the surface voxel's top face; capsule half-extent + clearance sits
        // the body above it. Falls back only if the column is all air.
        let spawn_y = match self.surface_voxel_y(PLAYER_SPAWN_X as i32, PLAYER_SPAWN_Z as i32) {
            Some(surface) => {
                (surface + 1) as f32
                    + PLAYER_CAPSULE_HALF_HEIGHT
                    + PLAYER_CAPSULE_RADIUS
                    + PLAYER_SPAWN_CLEARANCE
            }
            None => PLAYER_SPAWN_FALLBACK_Y,
        };
        let e = self.ecs.create_entity_safe();
        let position = IsometryReal::from_parts(
            Translation3::new(
                Real::from(PLAYER_SPAWN_X as RawReal),
                Real::from(spawn_y as RawReal),
                Real::from(PLAYER_SPAWN_Z as RawReal),
            ),
            Unit::<Quaternion<Real>>::identity(),
        );
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(position)
            .gravity_scale(Real::from(0.0))
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(Real::from(1.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // True fork inverse + delta digest, same shape as in create_mesh.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope_digest();
        let handle = scope.insert(body);
        scope.register(
            body_slot_digest((prev_head, prev_len), None),
            move |bodies, _| {
                bodies.revert_insert(handle, prev_head, prev_len);
                body_slot_digest(bodies.alloc_state(), bodies.get(handle))
            },
        );
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        // 21.6 units = 1.35 m at 1 unit = 1/16 m (1.35 blocks tall — shorter
        // side of the ~70%-of-a-2-block-ceiling human proportion).
        self.attach_capsule_collider_safe(e, handle, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS);
        let cam = Camera::new(handle);
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::AddCameraComponent(e, client_id, cam.proj_matrix, position),
        ));
        // LIFO: registered before set_safe, so the notification fires after
        // the camera value is restored.
        self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::RemoveCameraComponent(e),
        ));
        self.data.ecs.camera.set_safe(e, Some(cam));
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(EntityKind::Player)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(EntityKind::Player));
        self.data.player_entites.insert(client_id, e);
    }

    /// Forward inverse of the create path: undo-tracked removal of an
    /// entity's components, physics body+collider, and ECS slot. The
    /// physics removal has no exact surgical inverse (body removal touches
    /// colliders, islands, joints), so it rides a whole-PhysicsState
    /// snapshot — extractions are rare (one per boundary crossing).
    /// `cam_client`: the owning client if the entity has a camera, so the
    /// undo direction can re-emit AddCameraComponent for the renderer.
    pub fn remove_entity_safe(&mut self, key: EntityKey, cam_client: Option<ClientId>) {
        // Camera: Do-emit removal now; on undo, re-advertise it.
        if let Some(cam) = self.data.ecs.camera.try_get(key).clone() {
            let restored_pose = self
                .data
                .ecs
                .rigidbody
                .try_get(key)
                .and_then(|h| self.data.physics.bodies.get(h).map(|b| *b.position()))
                .unwrap_or_else(IsometryReal::identity);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::RemoveCameraComponent(key),
            ));
            self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::AddCameraComponent(
                    key,
                    cam_client.unwrap_or_default(),
                    cam.proj_matrix.clone(),
                    restored_pose,
                ),
            ));
        }
        // Kind: Do-emit clear; undo re-emits the old kind.
        if let Some(kind) = *self.data.ecs.kind.try_get(key) {
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityKind(key, None),
            ));
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetEntityKind(key, Some(kind)),
            ));
        }
        // Physics: remove body + attached colliders under a full snapshot.
        if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
            let p = self.data.physics.snapshot_raw();
            p.bodies.remove(
                handle,
                p.islands,
                p.colliders,
                p.implules_joint_set,
                p.multi_body_joint_set,
                true,
            );
        }
        // Remove every component slot this entity owns (symmetric to
        // create_entity_safe's insert_safe for camera/isometry/rigidbody/
        // chunk/kind). Removing the slots — not just nulling their values —
        // keeps the component maps in sync with the entities slotmap, so the
        // freed index can be reused later and still roll back bit-exact.
        // This MUST be unconditional: camera/kind carry a None slot for every
        // non-camera/non-kind entity, so gating their removal on a present
        // value (as the render-event emits above do) would leak a dangling
        // slot that a reused index later clobbers — non-LIFO-invertible,
        // hash(before) != hash(after undo) on rollback.
        self.data.ecs.camera.remove_safe(key);
        self.data.ecs.rigidbody.remove_safe(key);
        self.data.ecs.isometry.remove_safe(key);
        self.data.ecs.chunk.remove_safe(key);
        self.data.ecs.kind.remove_safe(key);
        // ECS slot last: the slotmap #[emit] fires RemoveEntity on the delta
        // (and CreateEntity again on undo).
        self.data.ecs.entities.remove(key);
    }

    /// Teleport a body, undo-safely. Footprint-sized entry: set_position also
    /// wakes the body / marks it modified, but all of that lives inside the
    /// body slot + modified-mark that `mutate_body_safe` restores.
    pub fn set_body_pose_safe(&mut self, key: EntityKey, pose: IsometryReal) {
        let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { return };
        self.data
            .physics
            .bodies
            .mutate_body_safe(handle, |b| b.set_position(pose, true));
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityPosition(key, pose),
        ));
    }

    /// Insert a kinematic character body + its collider for entity `e`,
    /// undo-tracked (the create_player_safe insert pattern generalized). The
    /// shared build used by transfer arrivals (`inject_body_safe`) and stage-2
    /// ghost hosting (`apply_ghost`): both need the same kinematic body from a
    /// pose + collider spec, so the builder chain lives here, once.
    fn build_kinematic_body_safe(
        &mut self,
        e: EntityKey,
        pose: IsometryReal,
        collider: &ColliderSpec,
    ) {
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(pose)
            .gravity_scale(Real::from(0.0))
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(Real::from(1.0))
            .can_sleep(false)
            .user_data(e.data().as_ffi() as u128)
            .build();
        // Tier-2 with a true fork inverse + delta digest, same shape as in
        // create_mesh.
        let (prev_head, prev_len) = self.data.physics.bodies.alloc_state();
        let mut scope = self.data.physics.bodies.undo_scope_digest();
        let handle = scope.insert(body);
        scope.register(
            body_slot_digest((prev_head, prev_len), None),
            move |bodies, _| {
                bodies.revert_insert(handle, prev_head, prev_len);
                body_slot_digest(bodies.alloc_state(), bodies.get(handle))
            },
        );
        self.data.ecs.rigidbody.set_safe(e, Some(handle));
        match collider {
            ColliderSpec::CapsuleY { half_height, radius } => {
                self.attach_capsule_collider_safe(e, handle, *half_height, *radius);
            }
        }
    }

    /// Body+collider insertion from a bundle — the create_player_safe insert
    /// pattern generalized to a transfer payload.
    fn inject_body_safe(&mut self, e: EntityKey, bundle: &EntityBundle) {
        self.build_kinematic_body_safe(e, bundle.isometry, &bundle.collider);
    }

    /// Ownership transfer INTO this region. Three paths:
    /// replayed identity → pose correction; ghost present → upgrade in
    /// place (same EntityKey — visual continuity); else → fresh create.
    pub fn apply_arrival(&mut self, bundle: EntityBundle) {
        let identity = (bundle.source_region, bundle.source_key);

        // Idempotency: a respawn-resnapshot replay must not duplicate.
        if let Some(&e) = self.data.arrivals.get(&identity) {
            if self.data.ecs.entities.contains_key(e) {
                self.set_body_pose_safe(e, bundle.isometry);
                return;
            }
        }

        let e = if let Some(entry) = self.data.ghosts.get(&identity).cloned() {
            // Upgrade-in-place: drop the ghost record, keep the entity.
            self.data.ghosts.remove(&identity);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(entry.entity, None),
            ));
            // The entity persists across rollback (it was baked as a ghost in a
            // prior transaction), so on undo we must re-apply the ghost mark —
            // otherwise the renderer keeps a phantom owned mirror. Compensating
            // emit rides `kind` (a tracked field; the ghosts map has no
            // emit_on_undo), fires LIFO after state is restored. source_region
            // == identity.0.
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetGhostSource(entry.entity, Some(bundle.source_region)),
            ));
            entry.entity
        } else {
            self.data.ecs.create_entity_safe()
        };

        // Kind (emit both directions, as create_player_safe does).
        self.data.send(GameDataUpdate::new(
            GameDataTransactionKind::Do,
            GameDataUpdateKind::SetEntityKind(e, Some(bundle.kind)),
        ));
        self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
            GameDataTransactionKind::Undo,
            GameDataUpdateKind::SetEntityKind(e, None),
        ));
        self.data.ecs.kind.set_safe(e, Some(bundle.kind));

        // Body + collider at the (already rebased) bundle pose. A stage-1
        // ghost has no body; if one exists (stage 2), correct its pose
        // instead of double-inserting.
        if self.data.ecs.rigidbody.try_get(e).is_some() {
            self.set_body_pose_safe(e, bundle.isometry);
        } else {
            self.inject_body_safe(e, &bundle);
        }

        // Camera + client attachment (players).
        if let Some((client_id, client)) = bundle.client.clone() {
            if bundle.has_camera {
                let handle = self.data.ecs.rigidbody.try_get(e).unwrap();
                let mut cam = Camera::new(handle);
                cam.yaw = bundle.yaw;
                cam.pitch = bundle.pitch;
                self.data.send(GameDataUpdate::new(
                    GameDataTransactionKind::Do,
                    GameDataUpdateKind::AddCameraComponent(
                        e, client_id, cam.proj_matrix.clone(), bundle.isometry,
                    ),
                ));
                self.data.ecs.camera.emit_on_undo(GameDataUpdate::new(
                    GameDataTransactionKind::Undo,
                    GameDataUpdateKind::RemoveCameraComponent(e),
                ));
                self.data.ecs.camera.set_safe(e, Some(cam));
            }
            self.data.clients.insert(client_id, client);
            self.data.player_entites.insert(client_id, e);
        }

        self.data.arrivals.insert(identity, e);
    }

    /// Margin mirror upsert. Stage 2: the ghost carries a kinematic body +
    /// collider (built from `GhostData.collider`) so it participates in the
    /// receiving region's physics — cross-boundary collision. A pose refresh
    /// moves that body; the arrival upgrade path reuses it (set_body_pose vs
    /// re-inject), and expiry removes it via remove_entity_safe.
    pub fn apply_ghost(&mut self, data: GhostData) {
        let identity = (data.source_region, data.source_key);
        let tick = *self.data.tick;
        if let Some(entry) = self.data.ghosts.get(&identity).cloned() {
            let mut refreshed = entry.clone();
            refreshed.last_update_tick = tick;
            self.data.ghosts.insert(identity, refreshed); // UndoMap logs the old value
            self.data.ecs.isometry.set_safe(entry.entity, Some(data.isometry));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(entry.entity, data.isometry),
            ));
            // Move the ghost's body to match (no-ops if it has none — safe
            // during a mixed-version rollout of parked blobs).
            self.set_body_pose_safe(entry.entity, data.isometry);
        } else {
            let e = self.data.ecs.create_entity_safe();
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityKind(e, Some(data.kind)),
            ));
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetEntityKind(e, None),
            ));
            self.data.ecs.kind.set_safe(e, Some(data.kind));
            self.data.ecs.isometry.set_safe(e, Some(data.isometry));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetEntityPosition(e, data.isometry),
            ));
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(e, Some(data.source_region)),
            ));
            // Stage 2: a kinematic body + collider so the ghost collides.
            self.build_kinematic_body_safe(e, data.isometry, &data.collider);
            self.data.ghosts.insert(
                identity,
                GhostEntry { entity: e, last_update_tick: tick },
            );
        }
    }

    /// TTL reaper: called once per tick. Covers the owner region parking,
    /// dying, or the entity leaving the margin.
    pub fn expire_ghosts(&mut self) {
        let tick = *self.data.tick;
        let expired: Vec<((crate::RegionCoords, EntityKey), EntityKey)> = self
            .data
            .ghosts
            .iter()
            .filter(|(_, g)| tick.saturating_sub(g.last_update_tick) > GHOST_TTL_TICKS)
            .map(|(k, g)| (*k, g.entity))
            .collect();
        for (k, e) in expired {
            self.data.ghosts.remove(&k);
            self.data.send(GameDataUpdate::new(
                GameDataTransactionKind::Do,
                GameDataUpdateKind::SetGhostSource(e, None),
            ));
            // remove_entity_safe's undo re-creates the entity (its slot #[emit]
            // fires CreateEntity on undo), so on rollback the ghost survives and
            // its mark must be restored. Registered BEFORE remove_entity_safe so
            // it replays LIFO-after the entity is re-created/re-mapped — else the
            // bridge would warn on an unmapped key. k.0 is the source region.
            self.data.ecs.kind.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetGhostSource(e, Some(k.0)),
            ));
            self.remove_entity_safe(e, None);
        }
    }

    /// The transferable shape spec of a body's first collider, or the player
    /// default. Non-capsule shapes don't cross the seam yet.
    fn collider_spec_of(&self, handle: RigidBodyHandle) -> crate::ColliderSpec {
        self.data
            .physics
            .bodies
            .get(handle)
            .and_then(|b| b.colliders().first().copied())
            .and_then(|ch| self.data.physics.colliders.get(ch))
            .and_then(|c| {
                c.shape().as_capsule().map(|cap| crate::ColliderSpec::CapsuleY {
                    half_height: cap.half_height().0,
                    radius: cap.radius.0,
                })
            })
            // Non-capsule shapes don't transfer yet; the player default.
            .unwrap_or(crate::ColliderSpec::CapsuleY {
                half_height: PLAYER_CAPSULE_HALF_HEIGHT,
                radius: PLAYER_CAPSULE_RADIUS,
            })
    }

    /// Assemble the bundle, then remove the entity — all undo-tracked.
    pub fn extract_entity_safe(
        &mut self,
        key: EntityKey,
        region: crate::RegionCoords,
    ) -> crate::EntityBundle {
        let kind = (*self.data.ecs.kind.try_get(key)).unwrap_or_default();
        let handle = (*self.data.ecs.rigidbody.try_get(key))
            .expect("transferable entities have bodies");
        let body = self.data.physics.bodies.get(handle).unwrap();
        let isometry = *body.position();
        let linvel = *body.linvel();
        let collider = self.collider_spec_of(handle);
        let client_id = self
            .data
            .player_entites
            .iter()
            .find(|(_, e)| **e == key)
            .map(|(c, _)| *c);
        let client = client_id.map(|c| (c, self.data.clients.get(&c).unwrap().clone()));
        let (yaw, pitch) = self
            .data
            .ecs
            .camera
            .try_get(key)
            .as_ref()
            .map(|c| (c.yaw, c.pitch))
            .unwrap_or_default();
        let bundle = crate::EntityBundle {
            kind,
            isometry,
            linvel,
            collider,
            has_camera: self.data.ecs.camera.try_get(key).is_some(),
            yaw,
            pitch,
            client,
            source_region: region,
            source_key: key,
        };
        if let Some(c) = client_id {
            self.data.player_entites.remove(&c);
            self.data.clients.remove(&c);
        }
        // This entity's own arrival identity (if it arrived here) is now stale.
        let stale: Vec<(crate::RegionCoords, EntityKey)> = self
            .data
            .arrivals
            .iter()
            .filter(|(_, e)| **e == key)
            .map(|(k, _)| *k)
            .collect();
        for k in stale {
            self.data.arrivals.remove(&k);
        }
        self.remove_entity_safe(key, client_id);
        bundle
    }

    /// Post-step boundary/margin scan. Iterates the kind component in key
    /// order (deterministic); terrain (kindless) and ghosts are excluded.
    /// Returns `(departures, ghost updates)` with ABSOLUTE target coords.
    pub fn scan_boundaries(
        &mut self,
        region: crate::RegionCoords,
    ) -> (
        Vec<(crate::EntityBundle, crate::RegionCoords)>,
        Vec<(crate::GhostData, crate::RegionCoords)>,
    ) {
        let ghost_keys: std::collections::BTreeSet<EntityKey> =
            self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        // Read-only pass: gather leavers/ghosts before any mutation, so the
        // immutable borrows of physics/ecs don't collide with extraction.
        let mut leavers: Vec<(EntityKey, crate::RegionCoords)> = Vec::new();
        let mut ghosts: Vec<(crate::GhostData, crate::RegionCoords)> = Vec::new();
        for (key, kind) in self.data.ecs.kind.iter() {
            let Some(kind) = kind else { continue };
            if ghost_keys.contains(&key) {
                continue;
            }
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            if let Some((dx, dz)) = crate::departure_offset(t.x.0, t.z.0) {
                leavers.push((key, crate::RegionCoords::new(region.x + dx, region.z + dz)));
            } else {
                for (dx, dz) in crate::ghost_offsets(t.x.0, t.z.0) {
                    ghosts.push((
                        crate::GhostData {
                            source_region: region,
                            source_key: key,
                            kind: *kind,
                            isometry: *body.position(),
                            linvel: *body.linvel(),
                            collider: self.collider_spec_of(handle),
                        },
                        crate::RegionCoords::new(region.x + dx, region.z + dz),
                    ));
                }
            }
        }
        // Mutating pass: extraction is undo-tracked, one per leaver.
        let mut departures = Vec::new();
        for (key, target) in leavers {
            departures.push((self.extract_entity_safe(key, region), target));
        }
        (departures, ghosts)
    }

    /// Terrain-only extraction for far-region views. Iterates chunk
    /// components; recovers each chunk's `ChunkCoords` from its fixed body
    /// translation (`create_mesh` places bodies at `(cx*32, cy*32, cz*32)`,
    /// exact). No entities/physics/undo state leaks out.
    pub fn far_snapshot(&self) -> crate::FarSnapshot {
        let mut chunks = Vec::new();
        for (key, maybe_chunk) in self.data.ecs.chunk.iter() {
            let Some(chunk) = maybe_chunk else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            // Real = OrderedFloat<f32>; `.0` unwraps to f32 (as scan_boundaries).
            let coords = ChunkCoords::new(
                (t.x.0 / 32.0) as usize,
                (t.y.0 / 32.0) as i32,
                (t.z.0 / 32.0) as usize,
            );
            chunks.push((coords, chunk.clone()));
        }
        // Sort by coordinate: `terrain_hash` bincode-serializes this Vec
        // positionally, so a deterministic order keeps the hash
        // order-insensitive and stable across sources (ECS insertion order
        // here vs. column-iteration order in `FarSnapshot::from_columns`).
        chunks.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        crate::FarSnapshot { chunks }
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn seeded_inventory_has_items_in_a_fixed_hotbar() {
        let inv = Inventory::seeded();
        assert_eq!(inv.slots.len(), 9);
        assert!(inv.slots.iter().filter(|s| s.is_some()).count() >= 2);
        // Client carries an inventory and it round-trips by value.
        let c = Client { inventory: inv.clone(), ..Client::default() };
        assert_eq!(c.inventory, inv);
    }

    #[test]
    fn inventory_mutation_rolls_back_bit_exact() {
        // Client is a tracked UndoMap value: get_mut logs the whole prior
        // Client, so an inventory edit is covered by one delta and inverts.
        let mut rb = Rollback::new(None);
        rb.new_transaction();
        rb.clients.insert(0, Client { inventory: Inventory::seeded(), ..Client::default() });
        rb.forget();
        let before = crate::state_hash(&rb);

        rb.new_transaction();
        {
            let c = rb.clients.get_mut(&0).unwrap();
            c.inventory.slots[0] = Some(ItemStack { kind: ItemKind::Stone, count: 5 });
        }
        rb.rollback();
        assert_eq!(before, crate::state_hash(&rb), "inventory edit must invert bit-exact");
    }
}

#[cfg(test)]
mod delta_digest_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static MODE_LOCK: Mutex<()> = Mutex::new(());
    struct ModeGuard(#[allow(dead_code)] MutexGuard<'static, ()>);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            set_hash_mode(HashMode::Full);
        }
    }
    fn delta_mode() -> ModeGuard {
        let g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_hash_mode(HashMode::Delta);
        ModeGuard(g)
    }
    fn rb() -> (Rollback, crossbeam::channel::Receiver<GameDataUpdate>) {
        let (send, recv) = crossbeam::channel::unbounded();
        (Rollback::new(Some(send)), recv)
    }

    #[test]
    fn tier2_digest_roundtrip_passes() {
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        r.new_transaction();
        r.ecs.camera.undo_digest(42, |_d, _s| 42);
        r.rollback(); // matching digest: no panic
    }

    #[test]
    #[should_panic(expected = "Hash verification failed (delta)")]
    fn tier2_digest_mismatch_panics() {
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        r.new_transaction();
        r.ecs.camera.undo_digest(42, |_d, _s| 43);
        r.rollback();
    }

    #[test]
    fn attach_collider_rolls_back_bit_exact() {
        // Holds MODE_LOCK for both legs; the guard restores Full on drop.
        let _m = delta_mode();
        let (mut r, _recv) = rb();

        // Setup (forgotten): an entity with a fixed body, inserted through
        // the same surgical path build_kinematic_body_safe uses.
        set_hash_mode(HashMode::Full);
        r.new_transaction();
        let e = r.ecs.create_entity_safe();
        let body = {
            let fixed = rapier3d::prelude::RigidBodyBuilder::fixed()
                .user_data(e.data().as_ffi() as u128)
                .build();
            let (prev_head, prev_len) = r.data.physics.bodies.alloc_state();
            let mut scope = r.data.physics.bodies.undo_scope_digest();
            let handle = scope.insert(fixed);
            scope.register(
                body_slot_digest((prev_head, prev_len), None),
                move |bodies, _| {
                    bodies.revert_insert(handle, prev_head, prev_len);
                    body_slot_digest(bodies.alloc_state(), bodies.get(handle))
                },
            );
            handle
        };
        r.forget();

        // Full leg: whole-field verification at rollback proves the surgical
        // inverse misses no PhysicsState-wide state.
        let before = crate::state_hash(&r);
        r.new_transaction();
        r.attach_capsule_collider_safe(e, body, 0.5, 0.25);
        assert_ne!(before, crate::state_hash(&r), "attach must change state");
        r.rollback();
        assert_eq!(before, crate::state_hash(&r), "Full-mode attach must invert bit-exact");

        // Delta leg (production mode): the footprint-digest path must verify
        // and invert identically, for the voxels variant too.
        set_hash_mode(HashMode::Delta);
        let before = crate::state_hash(&r);
        r.new_transaction();
        r.attach_voxels_collider_safe(
            e,
            body,
            &[parry3d::math::Point::new(0, 0, 0)],
            Real::from(1.0),
        );
        r.rollback();
        assert_eq!(before, crate::state_hash(&r), "Delta-mode attach must invert bit-exact");
    }

    #[test]
    fn plain_undo_closures_stay_full_verified_under_delta() {
        // Unknown-footprint tier-2 must not silently lose checking: change()
        // logs a whole-field snapshot and is still whole-field verified.
        let _m = delta_mode();
        let (mut r, _recv) = rb();
        let before = crate::state_hash(&r);
        r.new_transaction();
        r.tick.update(|t| *t += 1); // cell — stamps Full per Task 2
        r.ecs.camera.change();      // tier-2 snapshot
        r.rollback();
        assert_eq!(before, crate::state_hash(&r));
    }
}
