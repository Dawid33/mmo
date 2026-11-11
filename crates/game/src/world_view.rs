//! The read side of the seam-transparent substrate (spec §3.3): gameplay
//! reads blocks, entities and rays through a [`WorldView`] in WORLD
//! coordinates — its region's own state plus the read-only halo of its
//! neighbours (`halo.rs`) — and writes what it does not own through
//! `intents::Effects`. No region key, region-local coordinate or mirror
//! bookkeeping leaks through it.
//!
//! Units: positions are world sim units (`BLOCK_UNITS` per block, the
//! region's origin at `RegionCoords * REGION_SIZE`), as `f64` so any region
//! of the `i32` grid keeps sub-voxel precision; blocks are
//! [`WorldBlockPos`].
use crate::{Container, EntityId, EntityKey, EntityKind, ItemPile, Rollback, Tick, WorldBlockPos, REGION_SIZE};

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
    /// An owned entity, or a halo mirror.
    fn entity(&self, id: EntityId) -> Option<EntityRef<'_>>;
    /// Owned entities and mirrors whose body centre lies in `aabb`, sorted by
    /// `EntityId`.
    fn entities_in(&self, aabb: WorldAabb) -> Vec<EntityRef<'_>>;
    /// The item pile resting in cell `p`: own, or the halo's read-only
    /// mirror of a neighbour's (`item_piles.rs`); `None` elsewhere. Taking
    /// from or adding to a neighbour's is an intent.
    fn pile(&self, p: WorldBlockPos) -> Option<&ItemPile>;
    /// The chest in cell `p`: own, or the halo's read-only mirror of a
    /// neighbour's (`containers.rs`); `None` elsewhere. Taking from or
    /// putting into it is an intent.
    fn container(&self, p: WorldBlockPos) -> Option<&Container>;
    /// Water units in cell `p`, own ∪ halo (`Rollback::water_units_at`); 0
    /// elsewhere.
    fn water_units(&self, p: WorldBlockPos) -> u16;
    /// Stud terrain: the material at world stud cell `cell`, own region ∪ stud halo
    /// (`stud_halo.rs`; the recipe's implied ground included); `None` is air, or
    /// outside them.
    fn stud_material_at(&self, cell: crate::WorldStudCell) -> Option<u16>;
    /// Stud terrain: the first occupied stud cell along `ray` within `max_units` sim
    /// units, over the materialised chunks of own ∪ stud halo. A ray longer than the
    /// halo panics in debug: a long reach must be an intent, not a read.
    fn stud_raycast(&self, ray: WorldRay, max_units: f32) -> Option<StudWorldHit>;
}

/// What a [`WorldView::stud_raycast`] met: the chunk (world chunk, `y` = stud layer —
/// the viewing region's or a neighbour's), the cell in it, the face it entered through
/// (the unit step back toward the origin; zero when the ray started inside) and the
/// distance along the ray in sim units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StudWorldHit {
    pub chunk: crate::WorldChunkPos,
    pub cell: [u8; 3],
    pub normal: [i8; 3],
    pub t: f32,
}

impl StudWorldHit {
    /// The brick a build click would place against this hit and the world chunk it
    /// goes into (`stud::placement_for_ray`): the body of an `Order::PlaceStudAt`.
    pub fn placement(&self, part: u16, rot: u8, material: u16) -> Option<(crate::WorldChunkPos, crate::StudBrick)> {
        let hit = crate::StudRayHit {
            chunk: [self.chunk.x, self.chunk.y as i64, self.chunk.z],
            cell: self.cell,
            normal: self.normal,
            t: self.t,
        };
        let ([x, y, z], brick) = crate::stud::placement_for_ray(&hit, part, rot, material)?;
        Some((crate::WorldChunkPos::new(x, y as i32, z), brick))
    }
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

    fn pile(&self, p: WorldBlockPos) -> Option<&ItemPile> {
        self.data.pile_seen(p)
    }

    fn container(&self, p: WorldBlockPos) -> Option<&Container> {
        self.data.container_seen(p)
    }

    fn water_units(&self, p: WorldBlockPos) -> u16 {
        self.data.water_units_at(p)
    }

    fn stud_material_at(&self, cell: crate::WorldStudCell) -> Option<u16> {
        self.data.stud_material_seen(crate::local_stud_cell(*self.data.region, cell))
    }

    fn stud_raycast(&self, ray: WorldRay, max_units: f32) -> Option<StudWorldHit> {
        debug_assert!(max_units <= crate::halo::HALO_UNITS, "a {max_units}-unit stud ray reads past the halo: send an intent");
        let o = origin(self.data);
        let local = rapier3d::math::Vector::new(
            (ray.origin[0] - o[0]) as f32,
            ray.origin[1] as f32,
            (ray.origin[2] - o[2]) as f32,
        );
        let dir = rapier3d::math::Vector::new(ray.dir[0], ray.dir[1], ray.dir[2]);
        let hit = self.data.raycast_studs_seen(local, dir, max_units)?;
        let [x, y, z] = hit.chunk;
        let chunk = crate::WorldChunkPos::from_local(*self.data.region, [x as i32, y as i32, z as i32]);
        Some(StudWorldHit { chunk, cell: hit.cell, normal: hit.normal, t: hit.t })
    }
}
