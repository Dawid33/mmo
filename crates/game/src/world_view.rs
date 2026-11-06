//! The read side of the seam-transparent substrate (spec §3.3): gameplay
//! reads blocks, entities and rays through a [`WorldView`] in WORLD
//! coordinates — its region's own state plus the read-only halo of its
//! neighbours (`halo.rs`) — and writes what it does not own through
//! `intents::Effects`. No region key, region-local coordinate or mirror
//! bookkeeping leaks through it.
//!
//! Units: positions are world sim units (`BLOCK_VOXELS` per block, the
//! region's origin at `RegionCoords * REGION_SIZE`), as `f64` so any region
//! of the `i32` grid keeps sub-voxel precision; blocks are
//! [`WorldBlockPos`].
use crate::{Block, EntityId, EntityKey, EntityKind, Rollback, Tick, WorldBlockPos, HALO_BLOCKS, REGION_SIZE};

/// An axis-aligned box in world sim units, `min` inclusive, `max` exclusive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldAabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl WorldAabb {
    /// The box holding every position.
    pub fn everything() -> Self {
        Self { min: [f64::NEG_INFINITY; 3], max: [f64::INFINITY; 3] }
    }

    pub fn contains(&self, p: [f64; 3]) -> bool {
        (0..3).all(|a| self.min[a] <= p[a] && p[a] < self.max[a])
    }
}

/// A ray in world sim units; `dir` is a unit vector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldRay {
    pub origin: [f64; 3],
    pub dir: [f32; 3],
}

/// The first solid block a ray enters and the face it entered through
/// (outward unit normal; zero when the ray starts inside it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub block: WorldBlockPos,
    pub normal: [i32; 3],
}

/// A read-only entity: owned by the viewing region, or a halo mirror of a
/// neighbour's.
#[derive(Clone, Copy)]
pub struct EntityRef<'a> {
    id: EntityId,
    key: EntityKey,
    mirror: bool,
    data: &'a Rollback,
}

impl EntityRef<'_> {
    pub fn id(&self) -> EntityId {
        self.id
    }

    /// Whether this is a halo mirror (its owner is a neighbour).
    pub fn is_mirror(&self) -> bool {
        self.mirror
    }

    pub fn kind(&self) -> EntityKind {
        match self.data.halo_entity(self.id).filter(|_| self.mirror) {
            Some(m) => m.kind,
            None => (*self.data.ecs.kind.try_get(self.key)).unwrap_or_default(),
        }
    }

    /// Body centre, world sim units.
    pub fn world_translation(&self) -> [f64; 3] {
        world_translation(self.data, self.key).expect("an EntityRef has a body")
    }
}

impl std::fmt::Debug for EntityRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntityRef").field("id", &self.id).field("mirror", &self.mirror).finish()
    }
}

/// What gameplay systems read (spec §3.3).
pub trait WorldView {
    /// The world tick being simulated.
    fn tick(&self) -> Tick;
    /// The block at `p`, own region ∪ halo (implied ground included); `None`
    /// outside them.
    fn block(&self, p: WorldBlockPos) -> Option<Block>;
    /// An owned entity, or a halo mirror.
    fn entity(&self, id: EntityId) -> Option<EntityRef<'_>>;
    /// Owned entities and mirrors whose body centre lies in `aabb`, sorted by
    /// `EntityId`.
    fn entities_in(&self, aabb: WorldAabb) -> Vec<EntityRef<'_>>;
    /// The first solid block along `ray` within `max_blocks` (own ∪ halo).
    /// A ray longer than the halo panics in debug: a long reach must be an
    /// intent, not a read.
    fn raycast(&self, ray: WorldRay, max_blocks: f32) -> Option<Hit>;
}

/// One region's [`WorldView`] ([`Rollback::view`]).
#[derive(Clone, Copy)]
pub struct RegionView<'a> {
    data: &'a Rollback,
}

impl Rollback {
    pub fn view(&self) -> RegionView<'_> {
        RegionView { data: self }
    }
}

/// The region's origin in world sim units.
fn origin(data: &Rollback) -> [f64; 3] {
    let rc = *data.region;
    [rc.x as f64 * REGION_SIZE as f64, 0.0, rc.z as f64 * REGION_SIZE as f64]
}

fn world_translation(data: &Rollback, key: EntityKey) -> Option<[f64; 3]> {
    let handle = (*data.ecs.rigidbody.try_get(key))?;
    let t = data.physics.bodies.get(handle)?.translation();
    let o = origin(data);
    Some([o[0] + t.x as f64, t.y as f64, o[2] + t.z as f64])
}

impl<'a> RegionView<'a> {
    fn get(&self, id: EntityId) -> Option<EntityRef<'a>> {
        let data = self.data;
        let (key, mirror) = match data.id_index.get(&id) {
            Some(&key) => (key, false),
            None => (data.halo_entity(id)?.entity, true),
        };
        world_translation(data, key)?;
        Some(EntityRef { id, key, mirror, data })
    }
}

impl WorldView for RegionView<'_> {
    fn tick(&self) -> Tick {
        *self.data.tick
    }

    fn block(&self, p: WorldBlockPos) -> Option<Block> {
        self.data.halo_block(p)
    }

    fn entity(&self, id: EntityId) -> Option<EntityRef<'_>> {
        self.get(id)
    }

    fn entities_in(&self, aabb: WorldAabb) -> Vec<EntityRef<'_>> {
        let ids: std::collections::BTreeSet<EntityId> =
            self.data.id_index.keys().chain(self.data.halo_entities.keys()).copied().collect();
        ids.into_iter()
            .filter_map(|id| self.get(id))
            .filter(|e| aabb.contains(e.world_translation()))
            .collect()
    }

    fn raycast(&self, ray: WorldRay, max_blocks: f32) -> Option<Hit> {
        debug_assert!(max_blocks <= HALO_BLOCKS as f32, "a {max_blocks}-block ray reads past the halo: send an intent");
        let o = origin(self.data);
        let local = rapier3d::math::Vector::new(
            (ray.origin[0] - o[0]) as f32,
            ray.origin[1] as f32,
            (ray.origin[2] - o[2]) as f32,
        );
        let dir = rapier3d::math::Vector::new(ray.dir[0], ray.dir[1], ray.dir[2]);
        let hit = self.data.raycast_block_in(local, dir, max_blocks)?;
        let [x, y, z] = hit.block;
        Some(Hit {
            block: WorldBlockPos::from_local(*self.data.region, crate::BlockPos::new(x, y, z)),
            normal: hit.normal,
        })
    }
}
