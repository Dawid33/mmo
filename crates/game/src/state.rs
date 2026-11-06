//! The rollbackable simulation state: `GameData` plus the `#[rollback]`
//! invocation that generates the undo/transaction infrastructure
//! (`Rollback`, `Undo`, `UndoCell`, `UndoMap`, `UndoSlotMap`, ...) for it.
//! The generated code assumes `GameDataUpdate`/`GameDataUpdateKind`/
//! `GameDataTransactionKind` and `serde` are reachable at the crate root —
//! `lib.rs` re-exports this module to satisfy that.

use macros::rollback;
use rapier3d::math::{IVector, Pose, Vector};
use rapier3d::prelude::{ColliderBuilder, RigidBodyBuilder, RigidBodyHandle};
use state_hash::StateHash;
use slotmapd::Key as _;
use slotmapd::{new_key_type, SlotMap, SparseSecondaryMap};
use std::sync::{Arc, atomic::AtomicUsize};

use crate::camera::{Camera, Perspective3};
use crate::input::InputState;
use crate::protocol::{ColliderSpec, EntityBundle, GhostData, GHOST_TTL_TICKS};
use crate::registry::BlockId;
use crate::block::CHUNK_UNITS;
use crate::block::{Block, BlockIndex, ChiselData};
use crate::voxel::{Chunk, ChunkCoords, Voxel};

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

/// One block's full new state in a terrain edit: what `SetBlocks` carries.
/// The renderer writes the block's 16³ voxels from it (its chisel materials,
/// else `block.id` throughout).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BlockDelta {
    pub index: BlockIndex,
    pub block: Block,
    pub chisel: Option<ChiselData>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum GameDataUpdateKind {
    SetVoxelComponent(EntityKey, Option<Vec<Voxel>>),
    /// Blocks of a terrain chunk that changed, each as its full new state.
    /// An edit sends the new states on Do and the old ones on Undo.
    SetBlocks(EntityKey, Vec<BlockDelta>),
    /// A terrain chunk added after the snapshot (implied ground an edit
    /// materialised): the renderer spawns it at its pose and derives its
    /// voxels off-thread, budgeted.
    AddTerrainChunk(EntityKey, Pose, Box<Chunk>),
    SetEntityPosition(EntityKey, Pose),
    AddCameraComponent(EntityKey, ClientId, Perspective3, Pose),
    RemoveCameraComponent(EntityKey),
    UpdateCameraViewProj(EntityKey, Perspective3),
    UpdateCameraViewMatrix(EntityKey, Pose),
    CreateEntity(EntityKey),
    RemoveEntity(EntityKey),
    SetFreeCam(ClientId, bool),
    SetEntityKind(EntityKey, Option<EntityKind>),
    /// Marks an entity as a ghost mirror of (source region, source key), or
    /// clears the mark on upgrade/expiry. The bridge uses it to hide ghosts
    /// whose source region is also loaded locally.
    SetGhostSource(EntityKey, Option<crate::protocol::RegionCoords>),
    /// The entity's animation state (`anim.rs`), set (`Some`) or gone. The
    /// renderer poses the entity's model from it with `anim::sample`.
    SetAnimator(EntityKey, Option<crate::anim::Animator>),
    /// A voxel body (`voxel_body.rs`) spawned: terrain blocks detached into
    /// one dynamic body. `Pose` is the body's, at the model's centre of
    /// mass: model block `o` occupies body-local units
    /// `o * BLOCK_VOXELS - com .. + BLOCK_VOXELS`, `com` being
    /// `voxel_body::model_mass_properties(model).local_com`. Sent on Do
    /// only; the entity's `RemoveEntity` covers undo and the despawn.
    AddVoxelBody(EntityKey, Pose, Box<crate::voxel_body::VoxelModel>),
    /// Designations set (`Some`) or cleared (`None`) per tile.
    SetDesignations(Vec<(crate::TilePos, Option<crate::Designation>)>),
    /// Tiles that became (`true`) or stopped being (`false`) constructions.
    SetConstructed(Vec<(crate::TilePos, bool)>),
    /// Item piles (`item_piles.rs`) set (`Some`, the whole new pile) or gone
    /// (`None`) per cell. Do carries the new views, Undo the prior ones.
    SetItemPiles(Vec<(crate::edit::BlockPos, Option<crate::PileView>)>),
    /// Render-only, Do only: pickups this tick (`item_piles::tick_pickups_safe`)
    /// — who took what from which cell — so the client can fly the items into
    /// the picker. A replay re-sends it with the same tick (dedupe on that).
    ItemsPickedUp(Vec<crate::PickupView>),
    /// Render-only, Do only: items that came to rest somewhere other than
    /// where they started (a pile falling or pushed up, a throw) — piles have
    /// no physics, so the client animates the jump. Dedupe replays on `tick`.
    ItemsLanded(Vec<crate::PileFlight>),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

#[derive(Copy, Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum GameDataTransactionKind {
    Do,
    Undo,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, StateHash, PartialEq)]
pub struct ItemStack {
    pub item: crate::ItemId,
    pub count: u32,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, StateHash, PartialEq)]
#[module(crate)]
pub struct Client {
    pub input: InputState,
    pub fps_cam_mode: bool,
    /// Fortress (commander camera) or Adventure (one body walked by
    /// orders); `adventure.rs`.
    pub mode: crate::PlayMode,
    pub inventory: crate::Inventory,
    pub inventory_open: bool,
    /// The selected hotbar slot (`0..HOTBAR_SLOTS`).
    pub selected: u8,
    /// The stack held on the mouse cursor (inventory clicks).
    pub cursor: Option<ItemStack>,
    /// The block being mined (Adventure, left held; `mining.rs`).
    pub mining: Option<crate::Mining>,
    /// No mining progress before this tick (set after each break).
    pub break_cooldown_until: u64,
    /// The container (chest cell, region-local) whose screen is open
    /// (`containers.rs`).
    pub open_container: Option<crate::edit::BlockPos>,
}

impl Client {
    /// Clears what only means something in the region the client is in:
    /// mining (a region-local target and a start tick of that region's
    /// clock), the break cooldown (an absolute tick of that clock — each
    /// region's tick starts at 0 at genesis, so it would block mining in a
    /// younger region for as long as the age gap) and the open chest (a
    /// region-local cell). Called wherever a client enters a region from
    /// elsewhere: a seam arrival, a restore, a move.
    pub fn reset_region_local(&mut self) {
        self.mining = None;
        self.break_cooldown_until = 0;
        self.open_container = None;
    }

    /// Puts the cursor stack back into the inventory; returns what does not
    /// fit (the cursor is then empty — the caller drops or keeps the rest).
    pub fn stow_cursor(&mut self) -> Option<ItemStack> {
        let mut stack = self.cursor.take()?;
        stack.count = self.inventory.add(stack);
        (stack.count > 0).then_some(stack)
    }

    /// Leaves the survival screens: the inventory closes (the cursor stack
    /// goes back into it; the rest is returned for the caller to drop), any
    /// chest closes and mining stops. Used when the play mode changes to
    /// Fortress.
    pub fn leave_survival(&mut self) -> Option<ItemStack> {
        self.inventory_open = false;
        self.open_container = None;
        self.mining = None;
        self.stow_cursor()
    }
}

/// What an entity *is*, for rendering and future gameplay. The player is the
/// first kind; NPC kinds extend this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, StateHash)]
pub enum EntityKind {
    #[default]
    Player,
    /// Ambient hopping wildlife (`slime.rs`). `size` is Minecraft's 1, 2 or 4:
    /// it never changes, so it lives in the kind, which also carries it to
    /// the renderer and onto ghosts.
    Slime { size: u8 },
    /// One of the world's people (`people.rs`, `history`): a dwarf-rigged
    /// body with no camera and no client. The id ties the body to its
    /// history record.
    Person { person: crate::history::PersonId },
}

/// A live ghost mirror in this region, keyed in `GameData::ghosts` by its
/// source identity. `last_update_tick` drives the TTL reaper.
#[derive(Default, Debug, serde::Serialize, serde::Deserialize, Clone, ::borrow::Partial, StateHash, PartialEq)]
#[module(crate)]
pub struct GhostEntry {
    pub entity: EntityKey,
    pub last_update_tick: crate::Tick,
}

#[rollback(GameData)]
mod game_data {
    use rapier3d::math::Vector;
    use rapier3d::prelude::{
        CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IslandManager,
        MultibodyJointSet, NarrowPhase, RigidBodySet, SoftBodySet,
    };

    use super::{
        Camera, Chunk, ChunkIndexKey, Client, ClientId, Component, EntityKey,
        PhysicsParams,
        EntityKind, GhostEntry, Pose, RigidBodyHandle, RollbackInfo, SlotMap,
    };
    use std::collections::BTreeMap;

    pub struct GameData {
        ecs: Ecs,
        physics: PhysicsState,
        #[undo(cell)]
        tick: crate::Tick,
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
        /// Terrain chunk entity by [`ChunkIndexKey`]. Replaces the scans
        /// that recovered chunk coordinates from body translations.
        #[undo(map)]
        chunk_index: BTreeMap<ChunkIndexKey, EntityKey>,
        /// Per-column ground recipes from worldgen: what lies below each
        /// column's materialised band. Written only at genesis
        /// (`seed_ground_genesis`), never mutated, so it never needs undo; the
        /// `#[undo(map)]` wrapper is only map plumbing, its logged mutators
        /// must not be used.
        #[undo(map)]
        ground: BTreeMap<crate::ColumnCoords, crate::ColumnGround>,
        /// Fortress orders waiting on tiles (`orders.rs`).
        #[undo(map)]
        designations: BTreeMap<crate::TilePos, crate::Designation>,
        /// Tiles whose block was built by an order (what `Deconstruct` may
        /// take down).
        #[undo(map)]
        constructed: BTreeMap<crate::TilePos, ()>,
        /// The body each history person has in this region (`people.rs`).
        #[undo(map)]
        person_entities: BTreeMap<crate::history::PersonId, EntityKey>,
        /// Dropped items, one pile per resting cell (`item_piles.rs`):
        /// never entities, never bodies.
        #[undo(map)]
        item_piles: BTreeMap<crate::edit::BlockPos, crate::ItemPile>,
        /// Expiry index of `item_piles`: `(expire_tick, cell)`, so expiry
        /// and the cap's eviction touch only the earliest piles.
        #[undo(map)]
        pile_expiry: BTreeMap<(u64, crate::edit::BlockPos), ()>,
        /// Block inventories (chests) by cell (`containers.rs`).
        #[undo(map)]
        containers: BTreeMap<crate::edit::BlockPos, crate::Container>,
    }

    pub struct Ecs {
        #[undo(slotmap)]
        #[emit(insert = CreateEntity, remove = RemoveEntity)]
        entities: SlotMap<EntityKey, ()>,
        camera: Component<Camera>,
        isometry: Component<Pose>,
        rigidbody: Component<RigidBodyHandle>,
        chunk: Component<Chunk>,
        kind: Component<EntityKind>,
        motion: Component<crate::Motion>,
        slime: Component<crate::slime::Slime>,
        /// Sim-owned animation state (`anim.rs`).
        animator: Component<crate::anim::Animator>,
        /// Collider per rig bone, re-posed from the animator every tick.
        bones: Component<crate::anim::BoneColliders>,
        /// Terrain detached into a falling body (`voxel_body.rs`).
        voxel_body: Component<crate::voxel_body::VoxelBody>,
        /// An adventurer's walk toward a `MoveTo` goal (`adventure.rs`).
        walker: Component<crate::Walker>,
        /// When an idle person next wanders (`people.rs`).
        wander: Component<crate::people::Wander>,
    }

    pub struct PhysicsState {
        bodies: RigidBodySet,
        broad_phase: DefaultBroadPhase,
        implules_joint_set: ImpulseJointSet,
        multi_body_joint_set: MultibodyJointSet,
        ccd_solver: CCDSolver,
        colliders: ColliderSet,
        gravity: Vector,
        integration_parameters: PhysicsParams,
        islands: IslandManager,
        narrow_phase: NarrowPhase,
        /// Rapier 0.36 compiles soft bodies in; the game has none. Kept
        /// empty (the step journal and its `StateHash` assert it).
        soft_bodies: SoftBodySet,
    }
}

pub use game_data::*;

/// Rapier's integration parameters as the game runs them: its defaults, but
/// with `dt` = [`crate::movement::TICK_DT`] (one step per region tick) and
/// `length_unit` = [`crate::movement::PHYSICS_LENGTH_UNIT`] (plan D6), so
/// a default-built `PhysicsState` (the rollback derives `Default`) already has
/// it. Serializes and hashes exactly as the inner parameters (a newtype);
/// derefs to them wherever rapier takes `&IntegrationParameters`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize, StateHash)]
pub struct PhysicsParams(pub rapier3d::prelude::IntegrationParameters);

impl Default for PhysicsParams {
    fn default() -> Self {
        Self(rapier3d::prelude::IntegrationParameters {
            dt: crate::movement::TICK_DT,
            length_unit: crate::movement::PHYSICS_LENGTH_UNIT,
            ..Default::default()
        })
    }
}

impl std::ops::Deref for PhysicsParams {
    type Target = rapier3d::prelude::IntegrationParameters;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PhysicsParams {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// `(x, z, y)` of a chunk — the key of `GameData::chunk_index`.
pub type ChunkIndexKey = (usize, usize, i32);

/// Chunk coords recovered from a chunk body's translation (bodies sit at exact
/// multiples of `CHUNK_UNITS`). The only place coords are derived from a body.
pub fn chunk_coords_from_translation(t: Vector) -> ChunkCoords {
    let u = CHUNK_UNITS as f32;
    ChunkCoords::new((t.x / u) as usize, (t.y / u).floor() as i32, (t.z / u) as usize)
}

pub fn chunk_index_key(c: ChunkCoords) -> ChunkIndexKey {
    (c.x, c.z, c.y)
}

pub fn chunk_coords_of_index_key((x, z, y): ChunkIndexKey) -> ChunkCoords {
    ChunkCoords::new(x, y, z)
}

impl GameData {
    pub fn into_game_update_iter(self) {}

    pub fn designations(&self) -> &std::collections::BTreeMap<crate::TilePos, crate::Designation> {
        &self.designations
    }

    pub fn constructed(&self) -> &std::collections::BTreeMap<crate::TilePos, ()> {
        &self.constructed
    }

    /// Dropped-item piles by resting cell.
    pub fn item_piles(&self) -> &std::collections::BTreeMap<crate::edit::BlockPos, crate::ItemPile> {
        &self.item_piles
    }

    /// Block inventories (chests) by cell.
    pub fn containers(&self) -> &std::collections::BTreeMap<crate::edit::BlockPos, crate::Container> {
        &self.containers
    }

    /// The piles' expiry index, `(expire_tick, cell)` in expiry order.
    pub fn pile_expiry(&self) -> &std::collections::BTreeMap<(u64, crate::edit::BlockPos), ()> {
        &self.pile_expiry
    }

    /// The per-column ground recipes (for the client's tile mirror).
    pub fn ground_recipes(&self) -> &std::collections::BTreeMap<crate::ColumnCoords, crate::ColumnGround> {
        &self.ground
    }
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
        self.motion.insert_safe(key);
        self.slime.insert_safe(key);
        self.animator.insert_safe(key);
        self.bones.insert_safe(key);
        self.voxel_body.insert_safe(key);
        self.walker.insert_safe(key);
        self.wander.insert_safe(key);
        key
    }

    /// A genesis terrain chunk: only the `rigidbody` and `chunk` slots. The
    /// other components have no slot at all (`try_get` reads `None`), so the
    /// per-tick scans over `kind`/`slime`/`motion` walk the handful of mobile
    /// entities instead of every chunk (~300k over open sea: `scan_boundaries`
    /// + `freeze_strays_safe` + the slime cap were ~20% of a region tick).
    /// Chunks leave through `remove_chunk_raw`, never `remove_entity_safe`.
    pub(crate) fn create_chunk_entity_raw(&mut self) -> EntityKey {
        let key = self.entities.insert_raw(());
        self.rigidbody.raw_mut().insert(key, None);
        self.chunk.raw_mut().insert(key, None);
        key
    }
}

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize, StateHash)]
pub struct Component<T>
where
    T: 'static + Default,
{
    list: SparseSecondaryMap<EntityKey, Option<T>>,
    /// Always `None`: what `try_get` lends for an entity with no slot here
    /// (terrain chunks carry only `rigidbody` + `chunk` slots — see
    /// `create_chunk_entity_raw`). Not state: skipped by serde, constant.
    #[serde(skip)]
    none: Option<T>,
}

/// crc32 of one component slot — the exact footprint of the single-key
/// insert_safe / set_safe / remove_safe mutations.
fn component_slot_digest<T: StateHash>(key: EntityKey, slot: Option<&Option<T>>) -> u32 {
    let mut h = crate::StateHasher::new();
    key.state_hash(&mut h);
    slot.state_hash(&mut h);
    h.finalize()
}

/// crc32 of one rigid-body slot + the set's allocator state — the exact
/// footprint of a journaled body insert (reverted via revert_insert).
fn body_slot_digest(
    alloc: (Option<u32>, u32),
    body: Option<&rapier3d::prelude::RigidBody>,
) -> u32 {
    let mut h = crate::StateHasher::new();
    alloc.state_hash(&mut h);
    body.state_hash(&mut h);
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
    let mut h = crate::StateHasher::new();
    alloc.state_hash(&mut h);
    slot.state_hash(&mut h);
    parent.state_hash(&mut h);
    parent_was_modified.state_hash(&mut h);
    h.finalize()
}

/// crc32 of the exact footprint of one in-place body mutation through
/// `RigidBodySet::get_mut`: the body's value (pose, kinematic targets,
/// wake/sleep state and change flags all live inside it) and whether it was
/// already on the modified list (`get_mut`'s tracking push is conditional
/// on that).
fn body_write_digest(body: Option<&rapier3d::prelude::RigidBody>, was_modified: bool) -> u32 {
    let mut h = crate::StateHasher::new();
    body.state_hash(&mut h);
    was_modified.state_hash(&mut h);
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

/// crc32 of the exact footprint of one in-place collider mutation through
/// `ColliderSet::get_mut`: the collider's value and whether it was already on
/// the modified list. The collider twin of [`body_write_digest`].
fn collider_write_digest(collider: Option<&rapier3d::prelude::Collider>, was_modified: bool) -> u32 {
    let mut h = crate::StateHasher::new();
    collider.state_hash(&mut h);
    was_modified.state_hash(&mut h);
    h.finalize()
}

impl Undo<rapier3d::prelude::ColliderSet> {
    /// Mutate one existing collider in place (shape, enabled flag, pose
    /// relative to its parent), undo-safely, with a footprint-sized log
    /// entry. The collider twin of [`Undo::<RigidBodySet>::mutate_body_safe`]:
    /// the inverse restores the collider's value from a pre-write clone and
    /// pops the modified-list push iff this write made it. Shape changes are
    /// picked up by the next physics step, whose journal rolls back its own
    /// broad/narrow-phase updates.
    pub fn mutate_collider_safe(
        &mut self,
        handle: rapier3d::prelude::ColliderHandle,
        f: impl FnOnce(&mut rapier3d::prelude::Collider),
    ) {
        let pre_collider = self
            .get(handle)
            .expect("mutate_collider_safe: collider missing")
            .clone();
        let was_modified = self.is_marked_modified(handle);
        let mut scope = self.undo_scope_digest();
        let pre = if scope.wants_delta_digest() {
            collider_write_digest(Some(&pre_collider), was_modified)
        } else {
            0
        };
        f(scope.get_mut(handle).expect("mutate_collider_safe: collider missing"));
        scope.register(pre, move |colliders, _| {
            colliders.restore_raw(handle, pre_collider);
            if !was_modified {
                colliders.pop_modified(handle);
            }
            collider_write_digest(colliders.get(handle), colliders.is_marked_modified(handle))
        });
    }
}

impl<T> Undo<Component<T>>
where
    T: 'static
        + Default
        + Clone
        + Send
        + StateHash
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

/// Writes one block's full state (block + chisel entry) into a chunk. Going
/// through `ChunkBlocks::set` keeps the grid canonical in both directions.
fn write_chunk_block(chunk: &mut Chunk, index: BlockIndex, block: Block, chisel: Option<ChiselData>) {
    chunk.blocks.set(index, block);
    match chisel {
        Some(c) => {
            chunk.chisel.insert(index, c);
        }
        None => {
            chunk.chisel.remove(&index);
        }
    }
}

impl Undo<Component<Chunk>> {
    /// Sets one block of chunk entity `key` (its block and chisel entry),
    /// undo-safely, without copying the chunk. The inverse writes back only
    /// the old block and old chisel entry for `index`. That is exact because
    /// `ChunkBlocks` is canonical — a function of its block contents — and
    /// `ChunkBlocks::set` normalises in both directions (a differing write
    /// densifies a `Uniform` grid, a `Dense` grid whose blocks all become
    /// equal collapses), so restoring the old contents restores the old
    /// value; the chisel `BTreeMap` is restored entry-for-entry. Delta mode
    /// digests the chunk's slot, as `set_safe` does; Off and Full never read
    /// the digest, so neither side computes it there.
    ///
    /// Panics if `key` has no chunk: callers write only into an existing
    /// chunk entity (`write_block_safe` spawns one first for a new slot).
    pub fn set_chunk_block_safe(
        &mut self,
        key: EntityKey,
        index: BlockIndex,
        block: Block,
        chisel: Option<ChiselData>,
    ) {
        let chunk = self
            .list
            .get(key)
            .and_then(Option::as_ref)
            .expect("set_chunk_block_safe: no chunk at key");
        let old_block = chunk.blocks.get(index);
        let old_chisel = chunk.chisel.get(&index).cloned();
        let delta = crate::hash_mode() == crate::HashMode::Delta;
        let pre = if delta { component_slot_digest(key, self.list.get(key)) } else { 0 };
        self.undo_digest(pre, move |d, _| {
            let chunk = d.list.get_mut(key).and_then(Option::as_mut).expect("set_chunk_block_safe undo: chunk gone");
            write_chunk_block(chunk, index, old_block, old_chisel);
            // Full/Off never read the returned digest.
            if delta { component_slot_digest(key, d.list.get(key)) } else { 0 }
        });
        let chunk = self
            .raw_mut()
            .list
            .get_mut(key)
            .and_then(Option::as_mut)
            .expect("set_chunk_block_safe: no chunk at key");
        write_chunk_block(chunk, index, block, chisel);
    }

    /// [`Self::set_chunk_block_safe`] for many blocks of one chunk in one
    /// undo entry: the inverse writes back each old block and chisel entry
    /// (exact for the same reason — canonical grid, entry-for-entry chisel).
    /// `writes` must not repeat an index.
    pub fn set_chunk_blocks_safe(&mut self, key: EntityKey, writes: Vec<(BlockIndex, Block, Option<ChiselData>)>) {
        let chunk = self
            .list
            .get(key)
            .and_then(Option::as_ref)
            .expect("set_chunk_blocks_safe: no chunk at key");
        let olds: Vec<(BlockIndex, Block, Option<ChiselData>)> = writes
            .iter()
            .map(|&(index, _, _)| (index, chunk.blocks.get(index), chunk.chisel.get(&index).cloned()))
            .collect();
        let delta = crate::hash_mode() == crate::HashMode::Delta;
        let pre = if delta { component_slot_digest(key, self.list.get(key)) } else { 0 };
        self.undo_digest(pre, move |d, _| {
            let chunk = d.list.get_mut(key).and_then(Option::as_mut).expect("set_chunk_blocks_safe undo: chunk gone");
            for (index, block, chisel) in olds.iter().rev() {
                write_chunk_block(chunk, *index, *block, chisel.clone());
            }
            if delta { component_slot_digest(key, d.list.get(key)) } else { 0 }
        });
        let chunk = self
            .raw_mut()
            .list
            .get_mut(key)
            .and_then(Option::as_mut)
            .expect("set_chunk_blocks_safe: no chunk at key");
        for (index, block, chisel) in writes {
            write_chunk_block(chunk, index, block, chisel);
        }
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

    /// Drop the entity's slot; `None` if it had none (terrain chunks only
    /// carry some slots) or an empty one.
    pub fn remove(&mut self, item: EntityKey) -> Option<T> {
        self.list.remove(item).flatten()
    }

    pub fn get(&self, key: EntityKey) -> &T {
        self.list.get(key).unwrap().as_ref().unwrap()
    }

    /// The entity's slot; `None` also for an entity without one (terrain
    /// chunks have no camera/isometry/kind/motion/slime slots).
    pub fn try_get(&self, key: EntityKey) -> &Option<T> {
        self.list.get(key).unwrap_or(&self.none)
    }

    pub fn get_mut(&mut self, key: EntityKey) -> &mut T {
        self.list.get_mut(key).unwrap().as_mut().unwrap()
    }
}

// Player spawn / capsule tunables (voxel units; 1 unit = 1/16 m). Exposed as
// named constants so they can be tuned without hunting through the spawn math.
// The capsule dimensions derive from the metre tunables in `movement.rs`.
use crate::movement::{PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
/// Vertical gap left between the capsule bottom and the terrain surface at spawn.
const PLAYER_SPAWN_CLEARANCE: f32 = 3.6;
/// Region-local voxel (x, z) of the centre of block column `[bx, bz]`.
pub fn block_column_xz([bx, bz]: [u32; 2]) -> (f32, f32) {
    let bv = crate::BLOCK_VOXELS as f32;
    ((bx as f32 + 0.5) * bv, (bz as f32 + 0.5) * bv)
}

/// Region-local voxel column the player spawns over. Not tied to the region
/// center: the world seed was scanned (examples/seed_scan.rs) for dry land at
/// exactly this column, so it stays fixed even though REGION_SIZE grew.
pub const PLAYER_SPAWN_X: f32 = 128.0;
pub const PLAYER_SPAWN_Z: f32 = 128.0;
/// Spawn height used only when the spawn column has no terrain (all air).
const PLAYER_SPAWN_FALLBACK_Y: f32 = 26.0;

impl Rollback {
    /// Where this region spawns players (region-local units).
    pub fn spawn_xz(&self) -> (f32, f32) {
        (PLAYER_SPAWN_X, PLAYER_SPAWN_Z)
    }

    pub(crate) fn chunk_coords_of(&self, key: EntityKey) -> Option<ChunkCoords> {
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let t = self.data.physics.bodies.get(handle)?.translation();
        // Chunk bodies sit at exact multiples of CHUNK_UNITS (seed_terrain_genesis / create_mesh).
        Some(crate::chunk_coords_from_translation(t))
    }

    /// The terrain chunk entity at `coords`, if any.
    pub fn chunk_key_at(&self, coords: ChunkCoords) -> Option<EntityKey> {
        self.data.chunk_index.get(&chunk_index_key(coords)).copied()
    }

    /// Runtime chunk insertion (undo-safe): multi-scale decomposition only,
    /// unlike `seed_terrain_genesis`. Deliberately never collapses a
    /// fully-solid chunk to a single cuboid, so the same chunk content can
    /// yield a different collider shape depending on which path built it.
    /// That's safe because client and server always run the same path
    /// (create_mesh at runtime, seed_terrain_genesis at genesis) for the
    /// same chunk, so both sides still agree — but it's hash-relevant:
    /// unifying the two paths would shift state hashes.
    pub fn create_mesh(&mut self, coords: ChunkCoords, chunk: Chunk) -> EntityKey {
        let e = self.ecs.create_entity_safe();
        // Multi-scale decomposition: whole solid blocks at block scale, solid
        // 8³ chisel octants at half-block scale, other chisel detail at voxel scale. Covers exactly
        // derive_voxels' solid cells.
        let cells = crate::collider_cells(&chunk);
        self.ecs.chunk.set_safe(e, Some(chunk));
        self.chunk_index.insert(chunk_index_key(coords), e);
        let position = Pose::from_translation(Vector::new((coords.x * CHUNK_UNITS) as f32, (coords.y * CHUNK_UNITS as i32) as f32, (coords.z * CHUNK_UNITS) as f32));
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
        for (size, keys) in cells.grids() {
            self.attach_voxels_collider_safe(e, handle, keys, size as f32);
        }
        e
    }

    /// Bulk terrain build for region genesis. Inserts every chunk's fixed body
    /// and collider WITHOUT the per-chunk whole-`PhysicsState` snapshot that
    /// `create_mesh` takes for undo-safe runtime edits. Genesis is the base
    /// state and is never rolled back below itself, so raw (unlogged) inserts
    /// are correct and turn the build from O(N^2) (a growing clone per chunk)
    /// into O(N). Fully-solid chunks get a single cuboid; partial chunks get
    /// up to three parry `Voxels` shapes — whole solid blocks at block scale
    /// (voxel_size = BLOCK_VOXELS), solid 8³ chisel octants at half-block scale
    /// (HALF_BLOCK_VOXELS) and chisel detail at voxel scale (1.0).
    /// A chunk solid purely via fully-occupied chisel no longer collapses to
    /// a cuboid (collision-identical, just uncollapsed).
    /// The ECS writes (entity + component slots) are raw for the same reason
    /// the physics inserts are — no undo entries, no digests, no renderer
    /// emits.
    pub fn seed_terrain_genesis(&mut self, chunks: Vec<(ChunkCoords, Chunk)>) {
        for (coords, chunk) in chunks {
            let e = self.ecs.create_chunk_entity_raw();
            let cells = crate::collider_cells(&chunk);
            self.ecs.chunk.raw_mut().insert(e, Some(chunk));
            self.chunk_index.insert_raw(chunk_index_key(coords), e);
            let position = Pose::from_translation(Vector::new((coords.x * CHUNK_UNITS) as f32, (coords.y * CHUNK_UNITS as i32) as f32, (coords.z * CHUNK_UNITS) as f32));
            let body = RigidBodyBuilder::fixed()
                .pose(position)
                .user_data(e.data().as_ffi() as u128)
                .build();
            // Raw, unlogged insert: covered by the genesis base state.
            let handle = self.data.physics.raw_mut().bodies.raw_mut().insert(body);
            self.ecs.rigidbody.raw_mut().insert(e, Some(handle));
            if cells.is_empty() {
                continue;
            }
            let user_data = e.data().as_ffi() as u128;
            let mut colliders = Vec::with_capacity(3);
            if cells.is_full_cube() {
                // A fully-solid chunk's collision surface is exactly one
                // CHUNK_UNITS-edge cuboid, centred body-local at its half edge.
                let h = CHUNK_UNITS as f32 / 2.0;
                colliders.push(
                    ColliderBuilder::cuboid(h, h, h)
                        .translation(Vector::new(h, h, h))
                        .user_data(user_data)
                        .build(),
                );
            } else {
                // Multi-scale decomposition: whole solid blocks at block
                // scale, solid 8³ chisel octants at half-block scale, other
                // chisel detail at voxel scale.
                for (size, keys) in cells.grids() {
                    colliders.push(
                        ColliderBuilder::voxels(Vector::splat(size as f32), keys)
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

    /// Genesis-only: raw, unlogged insert of the per-column ground recipes.
    pub fn seed_ground_genesis(&mut self, ground: Vec<(crate::ColumnCoords, crate::ColumnGround)>) {
        for (coords, g) in ground {
            self.ground.insert_raw(coords, g);
        }
    }

    /// The block the ground recipe implies at `local` of chunk `coords`,
    /// when that chunk is not materialised. `None` for a chunk entity, a
    /// column without ground, or a sub-chunk inside the band.
    pub fn implied_block(&self, coords: ChunkCoords, local: (usize, usize, usize)) -> Option<crate::Block> {
        if self.chunk_index.contains_key(&chunk_index_key(coords)) {
            return None;
        }
        crate::ground::implied_block(self.ground.get(&crate::ColumnCoords::new(coords.x, coords.z)), coords, local)
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
        coords: &[IVector],
        voxel_size: f32,
    ) {
        let collider = ColliderBuilder::voxels(Vector::splat(voxel_size), coords)
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
        let collider = ColliderBuilder::capsule_y(half_height, radius)
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
    pub(crate) fn attach_collider_safe(
        &mut self,
        body: RigidBodyHandle,
        collider: rapier3d::prelude::Collider,
    ) -> rapier3d::prelude::ColliderHandle {
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
        });        handle
    }

    /// Highest solid voxel Y at region-local voxel column `(vx, vz)`, scanning
    /// the terrain chunks already built in this region. `None` if that column
    /// is all air or no chunk covers it. Voxel units — the same space as body
    /// and collider coordinates. Chunk entities don't store `ChunkCoords`, so
    /// the owning chunk column is recovered from each chunk body's translation
    /// (`coords * CHUNK_UNITS`). Pure read of built terrain, hence deterministic: client
    /// and server derive the same surface height.
    pub fn surface_voxel_y(&self, vx: i32, vz: i32) -> Option<i32> {
        self.surface_voxel(vx, vz).map(|(y, _)| y)
    }

    /// [`Self::surface_voxel_y`] plus the material of that topmost voxel.
    pub fn surface_voxel(&self, vx: i32, vz: i32) -> Option<(i32, BlockId)> {
        let (cx, cz) = (vx.div_euclid(CHUNK_UNITS as i32), vz.div_euclid(CHUNK_UNITS as i32));
        let (lx, lz) = (vx.rem_euclid(CHUNK_UNITS as i32) as u32, vz.rem_euclid(CHUNK_UNITS as i32) as u32);
        if cx < 0 || cz < 0 {
            return None;
        }
        let (cx, cz) = (cx as usize, cz as usize);
        // Chunks of this column, highest first: the first with any solid voxel
        // in the column holds the surface. Chunk Y comes from the index key.
        for (&(_, _, cy), &e) in self.data.chunk_index.range((cx, cz, i32::MIN)..=(cx, cz, i32::MAX)).rev() {
            let Some(chunk) = self.data.ecs.chunk.try_get(e).as_ref() else { continue };
            if let Some((ly, block)) = chunk.column_top(lx, lz) {
                return Some((cy * CHUNK_UNITS as i32 + ly as i32, block));
            }
        }
        None
    }

    pub fn create_player_safe(&mut self, client_id: ClientId) {
        self.create_player_at_safe(client_id, self.spawn_xz());
    }

    /// Body-centre height a player spawns at over region-local `xz` (voxel
    /// units): on the terrain surface, clear of the capsule — real terrain
    /// has real elevation, so a hardcoded flat-floor height would bury the
    /// player. `surface + 1` is the surface voxel's top face; capsule
    /// half-extent + clearance sits the body above it. Falls back only if
    /// the column is all air.
    pub fn player_spawn_y(&self, (x, z): (f32, f32)) -> f32 {
        match self.surface_voxel_y(x as i32, z as i32) {
            Some(surface) => {
                (surface + 1) as f32
                    + PLAYER_CAPSULE_HALF_HEIGHT
                    + PLAYER_CAPSULE_RADIUS
                    + PLAYER_SPAWN_CLEARANCE
            }
            None => PLAYER_SPAWN_FALLBACK_Y,
        }
    }

    /// [`Self::create_player_safe`] over region-local `xz` (voxel units).
    pub fn create_player_at_safe(&mut self, client_id: ClientId, xz: (f32, f32)) {
        let (spawn_x, spawn_z) = xz;
        let spawn_y = self.player_spawn_y(xz);
        self.create_player_body_safe(client_id, Vector::new(spawn_x, spawn_y, spawn_z));
    }

    /// `client_id`'s player body (camera, capsule, rig), its centre at
    /// region-local `translation` (voxel units). The `clients` entry is the
    /// caller's.
    pub fn create_player_body_safe(&mut self, client_id: ClientId, translation: Vector) {
        let e = self.ecs.create_entity_safe();
        let position = Pose::from_translation(translation);
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(position)
            .gravity_scale(0.0)
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(1.0)
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
        // movement::PLAYER_HEIGHT tall (1.8 m = 28.8 units): two blocks.
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
        self.data.ecs.motion.set_safe(e, Some(crate::Motion::default()));
        // Drawn as a dwarf whose bones collide (`anim.rs`); feet at the
        // bottom of the capsule.
        self.attach_rig_safe(
            e,
            crate::anim::Animator::idle(crate::anim::RigId::Dwarf),
            -(PLAYER_CAPSULE_HALF_HEIGHT + PLAYER_CAPSULE_RADIUS),
        );
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
                .unwrap_or_else(Pose::identity);
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
        // Animator: on undo, re-advertise it (the renderer draws the model).
        if let Some(animator) = *self.data.ecs.animator.try_get(key) {
            self.data.ecs.animator.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetAnimator(key, Some(animator)),
            ));
        }
        // Voxel body: on undo, re-advertise its model (the entity's
        // CreateEntity alone would come back empty).
        if let Some(vb) = self.data.ecs.voxel_body.try_get(key).clone() {
            let pose = self
                .data
                .ecs
                .rigidbody
                .try_get(key)
                .and_then(|h| self.data.physics.bodies.get(h).map(|b| *b.position()))
                .unwrap_or_else(Pose::identity);
            self.data.ecs.voxel_body.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::AddVoxelBody(key, pose, Box::new(vb.model)),
            ));
        }
        // A terrain chunk leaves the index (before its body goes: the body
        // is where its coordinates come from).
        if self.data.ecs.chunk.try_get(key).is_some() {
            if let Some(coords) = self.chunk_coords_of(key) {
                self.data.chunk_index.remove(&chunk_index_key(coords));
            }
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
                p.soft_bodies,
                true,
            );
        }
        // Remove every component slot this entity owns (symmetric to
        // create_entity_safe's insert_safe for camera/isometry/rigidbody/
        // chunk/kind/motion/slime). Removing the slots — not just nulling their values —
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
        self.data.ecs.motion.remove_safe(key);
        self.data.ecs.slime.remove_safe(key);
        self.data.ecs.animator.remove_safe(key);
        self.data.ecs.bones.remove_safe(key);
        self.data.ecs.voxel_body.remove_safe(key);
        self.data.ecs.walker.remove_safe(key);
        self.data.ecs.wander.remove_safe(key);
        // ECS slot last: the slotmap #[emit] fires RemoveEntity on the delta
        // (and CreateEntity again on undo).
        self.data.ecs.entities.remove(key);
    }

    /// Teleport a body, undo-safely. Footprint-sized entry: set_position also
    /// wakes the body / marks it modified, but all of that lives inside the
    /// body slot + modified-mark that `mutate_body_safe` restores.
    pub fn set_body_pose_safe(&mut self, key: EntityKey, pose: Pose) {
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

    /// Insert `body` into the body set, undo-safely: the true fork inverse
    /// (`revert_insert` restores the arena allocator exactly) with a delta
    /// digest of just this slot + the allocator state — the insert every
    /// body builder here uses.
    pub(crate) fn insert_body_safe(&mut self, body: rapier3d::prelude::RigidBody) -> RigidBodyHandle {
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
        handle
    }

    /// Insert a kinematic character body + its collider for entity `e`,
    /// undo-tracked (the create_player_safe insert pattern generalized). The
    /// shared build used by transfer arrivals (`inject_body_safe`) and stage-2
    /// ghost hosting (`apply_ghost`): both need the same kinematic body from a
    /// pose + collider spec, so the builder chain lives here, once.
    pub(crate) fn build_kinematic_body_safe(
        &mut self,
        e: EntityKey,
        pose: Pose,
        collider: &ColliderSpec,
    ) {
        let body = RigidBodyBuilder::kinematic_position_based()
            .pose(pose)
            .gravity_scale(0.0)
            .enabled_rotations(true, true, true)
            .ccd_enabled(false)
            .angular_damping(1.0)
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
    /// Returns the arrived entity, `None` for a replay (only a pose fix).
    pub fn apply_arrival(&mut self, bundle: EntityBundle) -> Option<EntityKey> {
        let identity = (bundle.source_region, bundle.source_key);

        // Idempotency: a respawn-resnapshot replay must not duplicate.
        if let Some(&e) = self.data.arrivals.get(&identity) {
            if self.data.ecs.entities.contains_key(e) {
                self.set_body_pose_safe(e, bundle.isometry);
                return None;
            }
        }

        // A person already embodied here (under another identity): the
        // duplicate arrival is ignored. After the replay branch, so a replay
        // of the same arrival still corrects the pose.
        if let EntityKind::Person { person } = bundle.kind {
            if self.data.person_entities.contains_key(&person) {
                return None;
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

        // Movement state (velocity, flight) carries across the seam.
        self.data.ecs.motion.set_safe(e, bundle.motion);
        // So does a mob's behaviour state.
        self.data.ecs.slime.set_safe(e, bundle.slime);
        // A rig: animator + bone colliders, unless the entity already has
        // them (a replayed arrival).
        if let (Some(animator), None) = (bundle.animator, self.data.ecs.bones.try_get(e)) {
            self.attach_rig_safe(e, animator, animator.foot_y);
        }
        // An adventurer's walk; its sender-local path is re-planned by
        // `resume_walker_safe` once the arrival is in.
        if bundle.walker.is_some() {
            self.data.ecs.walker.set_safe(e, bundle.walker.clone());
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
            // Mining, its cooldown and an open chest belong to the region
            // the client came from.
            let mut client = client;
            client.reset_region_local();
            self.data.clients.insert(client_id, client);
            self.data.player_entites.insert(client_id, e);
        }

        if let EntityKind::Person { person } = bundle.kind {
            self.data.person_entities.insert(person, e);
        }
        self.data.arrivals.insert(identity, e);
        Some(e)
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
            .filter(|(_, g)| tick.saturating_sub(g.last_update_tick) > GHOST_TTL_TICKS as crate::Tick)
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
                    half_height: cap.half_height(),
                    radius: cap.radius,
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
        let linvel = body.linvel();
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
            motion: *self.data.ecs.motion.try_get(key),
            slime: *self.data.ecs.slime.try_get(key),
            animator: *self.data.ecs.animator.try_get(key),
            walker: self.data.ecs.walker.try_get(key).clone(),
            client,
            source_region: region,
            source_key: key,
        };
        if let EntityKind::Person { person } = kind {
            self.data.person_entities.remove(&person);
        }
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
    ///
    /// With a `map`, a body past a side whose neighbour is off the map (the
    /// world's outer seam) is put back on the seam instead of leaving:
    /// nothing hands off out of a finite world.
    pub fn scan_boundaries(
        &mut self,
        region: crate::RegionCoords,
        map: Option<crate::MapExtent>,
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
        let mut held: Vec<(EntityKey, Pose)> = Vec::new();
        // Whether the neighbour at offset (dx, dz) is off the map: a body
        // past that side is held on it.
        let closed = |dx: i32, dz: i32| crate::off_map(map, crate::RegionCoords::new(region.x + dx, region.z + dz));
        for (key, kind) in self.data.ecs.kind.iter() {
            let Some(kind) = kind else { continue };
            if ghost_keys.contains(&key) {
                continue;
            }
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let mut t = body.translation();
            let size = crate::REGION_SIZE;
            let hold = |v: f32, lo_closed: bool, hi_closed: bool| {
                if v < 0.0 && lo_closed {
                    0.0
                } else if v > size && hi_closed {
                    size
                } else {
                    v
                }
            };
            let (hx, hz) = (hold(t.x, closed(-1, 0), closed(1, 0)), hold(t.z, closed(0, -1), closed(0, 1)));
            if (hx, hz) != (t.x, t.z) {
                t.x = hx;
                t.z = hz;
                held.push((key, Pose::from_parts(t, body.position().rotation)));
            }
            if let Some((dx, dz)) = crate::departure_offset(t.x, t.z) {
                leavers.push((key, crate::RegionCoords::new(region.x + dx, region.z + dz)));
            } else {
                for (dx, dz) in crate::ghost_offsets(t.x, t.z) {
                    ghosts.push((
                        crate::GhostData {
                            source_region: region,
                            source_key: key,
                            kind: *kind,
                            isometry: *body.position(),
                            linvel: body.linvel(),
                            collider: self.collider_spec_of(handle),
                        },
                        crate::RegionCoords::new(region.x + dx, region.z + dz),
                    ));
                }
            }
        }
        // Mutating pass: holds and extractions are undo-tracked.
        for (key, pose) in held {
            self.set_body_pose_safe(key, pose);
        }
        let mut departures = Vec::new();
        for (key, target) in leavers {
            departures.push((self.extract_entity_safe(key, region), target));
        }
        (departures, ghosts)
    }

    /// Change detector for the terrain `terrain_snapshot` would return, without
    /// building it: no chunk is cloned and nothing is serialized. The region
    /// thread polls this every `TERRAIN_HASH_INTERVAL_TICKS`, so its cost is
    /// paid forever on every running region — the clone + bincode of a full
    /// snapshot measured 45–110 ms per check against a 50 ms tick (2026-09-19
    /// capture), which is why the snapshot is now built only on a change.
    ///
    /// Each chunk contributes `crc32(coords, content)`; contributions are
    /// summed, so ECS iteration order does not matter (the snapshot sorts for
    /// the same reason). Coordinates are unique per chunk, so equal content in
    /// two places cannot cancel out. Only ever compared with an earlier value
    /// from the same process — not a wire or cross-machine hash.
    pub fn terrain_fingerprint(&self) -> u32 {
        let mut sum = 0u32;
        for (key, _) in self.data.ecs.chunk.iter() {
            sum = sum.wrapping_add(self.chunk_terrain_term(key).unwrap_or(0));
        }
        sum
    }

    /// One chunk's `terrain_fingerprint` contribution; `None` for a key
    /// without a terrain chunk body.
    pub fn chunk_terrain_term(&self, key: EntityKey) -> Option<u32> {
        let chunk = self.data.ecs.chunk.try_get(key).as_ref()?;
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let body = self.data.physics.bodies.get(handle)?;
        let t = body.translation();
        let mut h = crate::StateHasher::new();
        // Same coordinate derivation as `terrain_snapshot`.
        let c = chunk_coords_from_translation(t);
        (c.x as i64, c.y as i64, c.z as i64).state_hash(&mut h);
        chunk.state_hash(&mut h);
        Some(h.finalize())
    }

    /// Terrain-only extraction for far-region views. Iterates chunk
    /// components; recovers each chunk's `ChunkCoords` from its fixed body
    /// translation (`create_mesh` places bodies at `(cx*CHUNK_UNITS, cy*CHUNK_UNITS, cz*CHUNK_UNITS)`,
    /// exact). No entities/physics/undo state leaks out.
    pub fn terrain_snapshot(&self) -> crate::TerrainSnapshot {
        let mut chunks = Vec::new();
        for (key, maybe_chunk) in self.data.ecs.chunk.iter() {
            let Some(chunk) = maybe_chunk else { continue };
            let Some(handle) = *self.data.ecs.rigidbody.try_get(key) else { continue };
            let Some(body) = self.data.physics.bodies.get(handle) else { continue };
            let t = body.translation();
            let coords = chunk_coords_from_translation(t);
            chunks.push((coords, chunk.clone()));
        }
        // Sort by coordinate: `terrain_hash` bincode-serializes this Vec
        // positionally, so a deterministic order keeps the hash
        // order-insensitive and stable across sources (ECS insertion order
        // here vs. column-iteration order in `TerrainSnapshot::from_columns`).
        chunks.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        crate::TerrainSnapshot { chunks }
    }
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn starting_kit_is_a_full_size_inventory() {
        let inv = crate::Inventory::starting_kit();
        assert_eq!(inv.slots.len(), crate::INVENTORY_SLOTS);
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
        rb.clients.insert(0, Client { inventory: crate::Inventory::starting_kit(), ..Client::default() });
        rb.forget();
        let before = crate::state_hash(&rb);

        rb.new_transaction();
        {
            let c = rb.clients.get_mut(&0).unwrap();
            c.inventory.slots[0] = Some(ItemStack { item: crate::ItemId::block(crate::BlockId(2)), count: 5 });
        }
        rb.rollback();
        assert_eq!(before, crate::state_hash(&rb), "inventory edit must invert bit-exact");
    }
}

#[cfg(test)]
pub(crate) mod delta_digest_tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static MODE_LOCK: Mutex<()> = Mutex::new(());
    pub(crate) struct ModeGuard(#[allow(dead_code)] MutexGuard<'static, ()>);
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            set_hash_mode(HashMode::Full);
        }
    }
    /// `HashMode::Delta` until the guard drops (then back to the default
    /// Full), serialised against every other lib test that switches modes.
    pub(crate) fn delta_mode() -> ModeGuard {
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
            &[IVector::new(0, 0, 0)],
            1.0,
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

#[cfg(test)]
mod physics_params_tests {
    use super::*;

    /// Plan D6: every region's physics runs with the game's length unit.
    #[test]
    fn default_physics_state_uses_the_game_length_unit() {
        let r = Rollback::new(None);
        assert_eq!(r.physics.integration_parameters.length_unit, crate::movement::PHYSICS_LENGTH_UNIT);
        assert_ne!(crate::movement::PHYSICS_LENGTH_UNIT, 1.0, "D6 moves it off rapier's default");
    }

    /// One physics step per region tick: a kinematic body's derived `linvel`
    /// (Δpose / dt, carried in `EntityBundle`/`GhostData`) is in units/s, and
    /// a dynamic body falls in real time (H10, the user's decision).
    #[test]
    fn default_physics_state_steps_one_tick() {
        let r = Rollback::new(None);
        assert_eq!(r.physics.integration_parameters.dt, crate::movement::TICK_DT);
    }
}
