//! The client's index of stud terrain across every loaded region, kept in step
//! incrementally (never rebuilt per frame): which stud chunk entity sits at each
//! world chunk, each chunk's six boundary layers ([`FaceSlabs`]), the stud halo
//! of each region (the neighbours' edge chunks, for culling and reads past a
//! region's edge when the neighbour is not loaded) and the ground recipes.
//!
//! It is also the remesh schedule. A chunk is dirty when it changed or when the
//! layer a face neighbour shows it changed; `stamp` counts those events, a mesh
//! job remembers the stamp it was built from, and a result whose stamp is no
//! longer the chunk's leaves the chunk queued — so an edit that lands while a job
//! is in flight is never lost, and repeated edits coalesce into one more job.
//!
//! Halo chunks are data only: they have no entity and are never drawn.
//!
//! It also decides which chunks are meshed with **detail** (studs and seam
//! lines — `mesh.rs`): those near the viewer ([`StudWorld::set_detail_centre`]).
//! A chunk that comes into reach is queued for a job with detail; one that
//! leaves just has its detail mesh taken away.
//!
//! And it counts what each region draws ([`StudWorld::region_meshed`]), for the
//! one `[perf] event=stud_region_meshed` line a region logs when its first
//! meshes are all in.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use bevy::prelude::*;
use game::{ColumnCoords, RegionId, StudChunk, StudColumnGround, TerrainKind, WorldChunkPos};

use super::mesh::{dir_step, opposite_dir, FaceSlabs, RemeshQueue, Slab, StudMeshJob};

/// One region's stud ground recipes, shared with the tile mirror and the water pass.
pub type StudGround = Arc<BTreeMap<ColumnCoords, StudColumnGround>>;

/// What the bridge tells the stud mirror (and, through it, the tile mirror).
#[derive(Message, Debug, Clone)]
pub enum StudWorldMsg {
    /// A region snapshot arrived (or was replaced): its terrain kind, ground
    /// recipes and whole stud halo.
    Region {
        region: RegionId,
        terrain: TerrainKind,
        ground: StudGround,
        halo: Vec<(WorldChunkPos, Option<StudChunk>)>,
        halo_ground: Vec<(WorldChunkPos, Option<StudColumnGround>)>,
    },
    /// `region`'s stud halo changed (`SetStudHalo` / `SetStudHaloGround`, do and
    /// undo alike): chunks and recipes set (`Some`) or gone (`None`).
    Halo {
        region: RegionId,
        chunks: Vec<(WorldChunkPos, Option<StudChunk>)>,
        ground: Vec<(WorldChunkPos, Option<StudColumnGround>)>,
    },
}

impl StudWorldMsg {
    /// The message for a region snapshot.
    pub fn of_snapshot(region: RegionId, data: &game::GameData) -> Self {
        StudWorldMsg::Region {
            region,
            terrain: TerrainKind::of(*data.body_scale),
            ground: Arc::new(data.stud_ground_recipes().clone()),
            halo: data.stud_halo_chunks().iter().map(|(k, h)| (*k, h.chunk.clone())).collect(),
            halo_ground: data.stud_halo_recipes().iter().map(|(k, g)| (*k, Some(g.clone()))).collect(),
        }
    }

    /// The message a region's render update carries, if any.
    pub fn from_update(region: RegionId, kind: &game::GameDataUpdateKind) -> Option<Self> {
        use game::GameDataUpdateKind as K;
        Some(match kind {
            K::SetStudHalo(chunks) => StudWorldMsg::Halo { region, chunks: chunks.clone(), ground: Vec::new() },
            K::SetStudHaloGround(ground) => StudWorldMsg::Halo { region, chunks: Vec::new(), ground: ground.clone() },
            _ => return None,
        })
    }
}

/// The viewer must move this far (sim units) before the detail set is looked at
/// again: a quarter of a chunk.
pub const STUD_DETAIL_STEP_UNITS: f32 = game::CHUNK_UNITS as f32 / 4.0;
/// A chunk keeps its detail until its centre is this much (sim units) farther
/// than the radius it gained it at, so a viewer pacing at the edge of a chunk's
/// reach does not remesh it back and forth: half a chunk.
pub const STUD_DETAIL_KEEP_MARGIN_UNITS: f32 = game::CHUNK_UNITS as f32 / 2.0;

/// What one chunk's last landed mesh draws.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChunkDrawn {
    pub body_tris: u32,
    pub body_verts: u32,
    pub detail_tris: u32,
    pub detail_verts: u32,
}

/// What a region's stud chunks draw, summed ([`StudWorld::region_meshed`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RegionDrawn {
    pub chunks: usize,
    /// Chunks with a body mesh / a detail mesh.
    pub body_meshes: usize,
    pub detail_meshes: usize,
    pub body_tris: u64,
    pub body_verts: u64,
    pub detail_tris: u64,
    pub detail_verts: u64,
}

/// A loaded stud chunk.
#[derive(Debug, Clone)]
struct ChunkEntry {
    entity: Entity,
    /// Its own outermost layers (what each face neighbour culls against).
    slabs: FaceSlabs,
    /// Bumped every time the chunk is marked dirty.
    stamp: u64,
    /// Its last landed mesh; `None` until the first one lands.
    drawn: Option<ChunkDrawn>,
}

/// A mirrored halo chunk of some host region.
#[derive(Debug, Clone)]
struct HaloEntry {
    chunk: StudChunk,
    slabs: FaceSlabs,
}

/// What to do with a finished mesh job ([`StudWorld::finish`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finished {
    /// The chunk is gone (or another entity took its place): drop the result.
    Drop,
    /// Show it; the chunk is up to date.
    Apply,
    /// Show it (it is newer than what is drawn), but the chunk changed while the
    /// job was in flight: it stays queued for another.
    ApplyStale,
}

#[derive(Resource, Default)]
pub struct StudWorld {
    chunks: BTreeMap<WorldChunkPos, ChunkEntry>,
    by_entity: HashMap<Entity, WorldChunkPos>,
    /// Halo chunks by world chunk, per host region (a corner chunk is mirrored by
    /// several; they hold the same content).
    halo: BTreeMap<WorldChunkPos, BTreeMap<RegionId, HaloEntry>>,
    /// Halo recipes by world chunk column (`y` 0), per host region.
    halo_ground: BTreeMap<WorldChunkPos, BTreeMap<RegionId, StudColumnGround>>,
    ground: BTreeMap<RegionId, StudGround>,
    terrain: BTreeMap<RegionId, TerrainKind>,
    queue: RemeshQueue<WorldChunkPos>,
    in_flight: BTreeSet<WorldChunkPos>,
    /// Bumped when a recipe set arrives or leaves (the water pass rebuilds on it).
    ground_epoch: u64,
    /// The world chunks meshed with detail (loaded or not).
    detail: BTreeSet<WorldChunkPos>,
    /// `(centre, radius)` (world sim units) `detail` was last cut for.
    detail_at: Option<(Vec3, f32)>,
    /// Regions whose first meshes have all landed and were reported.
    reported: BTreeSet<RegionId>,
}

/// The world chunk of stud chunk `coords` of `region`.
pub fn world_chunk(region: RegionId, coords: game::StudChunkCoords) -> WorldChunkPos {
    WorldChunkPos::from_local(region, [coords.x as i32, coords.y, coords.z as i32])
}

fn step(key: WorldChunkPos, d: usize) -> WorldChunkPos {
    let s = dir_step(d);
    WorldChunkPos::new(key.x + s[0] as i64, key.y + s[1], key.z + s[2] as i64)
}

impl StudWorld {
    /// The entity of the loaded stud chunk at `key`.
    pub fn entity_at(&self, key: WorldChunkPos) -> Option<Entity> {
        self.chunks.get(&key).map(|c| c.entity)
    }

    /// The world chunk `entity` is filed under.
    pub fn key_of(&self, entity: Entity) -> Option<WorldChunkPos> {
        self.by_entity.get(&entity).copied()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Chunks waiting for a mesh job.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// The terrain kind of a loaded region (voxel until its snapshot says otherwise).
    pub fn terrain(&self, region: RegionId) -> TerrainKind {
        self.terrain.get(&region).copied().unwrap_or_default()
    }

    /// A loaded region's stud ground recipes.
    pub fn ground(&self, region: RegionId) -> Option<&StudGround> {
        self.ground.get(&region)
    }

    /// Every loaded region with stud recipes.
    pub fn grounds(&self) -> impl Iterator<Item = (RegionId, &StudGround)> {
        self.ground.iter().map(|(r, g)| (*r, g))
    }

    /// See the field.
    pub fn ground_epoch(&self) -> u64 {
        self.ground_epoch
    }

    /// The halo chunk at `key`, as any host region mirrors it.
    pub fn halo_chunk(&self, key: WorldChunkPos) -> Option<&StudChunk> {
        self.halo.get(&key).and_then(|hosts| hosts.values().next()).map(|h| &h.chunk)
    }

    /// The recipe of world chunk column `(x, z)`: its own region's when loaded, else
    /// a halo's.
    pub fn recipe(&self, x: i64, z: i64) -> Option<&StudColumnGround> {
        let (rc, c) = WorldChunkPos::new(x, 0, z).to_local();
        if let Some(g) = self.ground.get(&rc).and_then(|g| g.get(&ColumnCoords::new(c.x, c.z))) {
            return Some(g);
        }
        self.halo_ground.get(&WorldChunkPos::new(x, 0, z)).and_then(|hosts| hosts.values().next())
    }

    fn mark_dirty(&mut self, key: WorldChunkPos) {
        if let Some(c) = self.chunks.get_mut(&key) {
            c.stamp += 1;
            self.queue.push(key);
        }
    }

    fn mark_neighbours_dirty(&mut self, key: WorldChunkPos) {
        for d in 0..6 {
            self.mark_dirty(step(key, d));
        }
    }

    /// A stud chunk entity appeared or its content changed: refile it, queue it, and
    /// queue each face neighbour whose view of it (the boundary layer) changed.
    pub fn set_chunk(&mut self, entity: Entity, key: WorldChunkPos, chunk: &StudChunk) {
        if let Some(old) = self.by_entity.insert(entity, key) {
            if old != key {
                self.remove_key_of(entity, old);
            }
        }
        let slabs = FaceSlabs::boundaries(chunk);
        let prior = self.chunks.get(&key).map(|c| (c.entity, c.slabs.clone(), c.stamp));
        if let Some((other, ..)) = prior {
            if other != entity {
                // Its job (if any) will come back for the wrong entity and be dropped.
                self.by_entity.remove(&other);
                self.in_flight.remove(&key);
            }
        }
        // A replaced entity keeps counting from the old stamp, so a job in flight for
        // the old content can never look current.
        let stamp = prior.as_ref().map_or(0, |p| p.2);
        for d in 0..6 {
            if prior.as_ref().is_none_or(|p| p.1 .0[d] != slabs.0[d]) {
                self.mark_dirty(step(key, d));
            }
        }
        let drawn = prior.as_ref().filter(|p| p.0 == entity).and_then(|_| self.chunks.get(&key).and_then(|c| c.drawn));
        self.chunks.insert(key, ChunkEntry { entity, slabs, stamp, drawn });
        self.mark_dirty(key);
    }

    fn remove_key_of(&mut self, entity: Entity, key: WorldChunkPos) {
        if self.chunks.get(&key).is_some_and(|c| c.entity == entity) {
            self.chunks.remove(&key);
            self.in_flight.remove(&key);
            self.mark_neighbours_dirty(key);
        }
    }

    /// A stud chunk entity went away.
    pub fn remove_entity(&mut self, entity: Entity) {
        if let Some(key) = self.by_entity.remove(&entity) {
            self.remove_key_of(entity, key);
        }
    }

    /// Queue every loaded chunk in or beside world chunk column `(x, z)`: its implied
    /// ground (what an unloaded chunk there stands for) may have changed.
    fn dirty_around_column(&mut self, x: i64, z: i64) {
        for (cx, cz) in [(x, z), (x + 1, z), (x - 1, z), (x, z + 1), (x, z - 1)] {
            let keys: Vec<WorldChunkPos> = self
                .chunks
                .range(WorldChunkPos::new(cx, i32::MIN, cz)..=WorldChunkPos::new(cx, i32::MAX, cz))
                .map(|(k, _)| *k)
                .collect();
            for k in keys {
                self.mark_dirty(k);
            }
        }
    }

    /// Apply one bridge message.
    pub fn apply(&mut self, msg: &StudWorldMsg) {
        match msg {
            StudWorldMsg::Region { region, terrain, ground, halo, halo_ground } => {
                self.drop_host(*region);
                self.terrain.insert(*region, *terrain);
                if ground.is_empty() {
                    self.ground.remove(region);
                } else {
                    self.ground.insert(*region, ground.clone());
                }
                self.ground_epoch += 1;
                self.apply_halo(*region, halo, halo_ground);
            }
            StudWorldMsg::Halo { region, chunks, ground } => {
                // Before the region's snapshot: that will carry it.
                if self.terrain.contains_key(region) {
                    self.apply_halo(*region, chunks, ground);
                }
            }
        }
    }

    fn apply_halo(
        &mut self,
        host: RegionId,
        chunks: &[(WorldChunkPos, Option<StudChunk>)],
        ground: &[(WorldChunkPos, Option<StudColumnGround>)],
    ) {
        for (k, g) in ground {
            let key = WorldChunkPos::new(k.x, 0, k.z);
            match g {
                Some(g) => {
                    self.halo_ground.entry(key).or_default().insert(host, g.clone());
                }
                None => {
                    if let Some(hosts) = self.halo_ground.get_mut(&key) {
                        hosts.remove(&host);
                        if hosts.is_empty() {
                            self.halo_ground.remove(&key);
                        }
                    }
                }
            }
            self.dirty_around_column(k.x, k.z);
        }
        for (k, chunk) in chunks {
            match chunk {
                Some(c) => {
                    let entry = HaloEntry { chunk: c.clone(), slabs: FaceSlabs::boundaries(c) };
                    self.halo.entry(*k).or_default().insert(host, entry);
                }
                None => {
                    if let Some(hosts) = self.halo.get_mut(k) {
                        hosts.remove(&host);
                        if hosts.is_empty() {
                            self.halo.remove(k);
                        }
                    }
                }
            }
            self.mark_neighbours_dirty(*k);
        }
    }

    /// Forget everything `host` mirrored and its own recipes.
    fn drop_host(&mut self, host: RegionId) {
        let chunks: Vec<WorldChunkPos> =
            self.halo.iter().filter(|(_, hosts)| hosts.contains_key(&host)).map(|(k, _)| *k).collect();
        for k in chunks {
            if let Some(hosts) = self.halo.get_mut(&k) {
                hosts.remove(&host);
                if hosts.is_empty() {
                    self.halo.remove(&k);
                }
            }
            self.mark_neighbours_dirty(k);
        }
        let columns: Vec<WorldChunkPos> =
            self.halo_ground.iter().filter(|(_, hosts)| hosts.contains_key(&host)).map(|(k, _)| *k).collect();
        for k in columns {
            if let Some(hosts) = self.halo_ground.get_mut(&k) {
                hosts.remove(&host);
                if hosts.is_empty() {
                    self.halo_ground.remove(&k);
                }
            }
            self.dirty_around_column(k.x, k.z);
        }
        if self.ground.remove(&host).is_some() {
            self.ground_epoch += 1;
        }
        self.terrain.remove(&host);
        self.reported.remove(&host);
    }

    /// Drop the state of regions no longer loaded.
    pub fn prune_regions(&mut self, loaded: impl Fn(&RegionId) -> bool) {
        let gone: Vec<RegionId> = self.terrain.keys().filter(|r| !loaded(r)).copied().collect();
        for r in gone {
            self.drop_host(r);
        }
    }

    /// What the chunk at `key` culls against across its face `DIRS[d]`: the loaded
    /// neighbour chunk, else a halo mirror of it, else its implied ground (solid
    /// below its column's surface layers, per the recipe), else nothing.
    pub fn neighbour_slab(&self, key: WorldChunkPos, d: usize) -> Slab {
        let n = step(key, d);
        if let Some(c) = self.chunks.get(&n) {
            return c.slabs.0[opposite_dir(d)].clone();
        }
        if let Some(h) = self.halo.get(&n).and_then(|hosts| hosts.values().next()) {
            return h.slabs.0[opposite_dir(d)].clone();
        }
        match self.recipe(n.x, n.z) {
            Some(g) if n.y < *g.surface_layers().start() => Slab::Solid,
            _ => Slab::Free,
        }
    }

    /// The six [`Self::neighbour_slab`]s of `key`.
    pub fn neighbour_slabs(&self, key: WorldChunkPos) -> FaceSlabs {
        FaceSlabs(std::array::from_fn(|d| self.neighbour_slab(key, d)))
    }

    /// Up to `budget` queued chunks to start jobs for, oldest first, as
    /// `(key, entity, stamp)`; they count as in flight until [`Self::finish`]. A
    /// chunk whose job is still in flight stays queued (one job per chunk at a time).
    pub fn take_jobs(&mut self, budget: usize) -> Vec<(WorldChunkPos, Entity, u64)> {
        let mut out = Vec::new();
        let mut waiting = Vec::new();
        while out.len() < budget {
            let Some(key) = self.queue.take(1).pop() else { break };
            let Some(c) = self.chunks.get(&key) else { continue };
            if self.in_flight.contains(&key) {
                waiting.push(key);
                continue;
            }
            self.in_flight.insert(key);
            out.push((key, c.entity, c.stamp));
        }
        for key in waiting {
            self.queue.push(key);
        }
        out
    }

    /// The job input for `key` now.
    pub fn job(&self, key: WorldChunkPos, chunk: &StudChunk) -> StudMeshJob {
        StudMeshJob::Chunk { chunk: chunk.clone(), neighbours: self.neighbour_slabs(key), detail: self.wants_detail(key) }
    }

    /// Whether the chunk at `key` is drawn with studs and seam lines.
    pub fn wants_detail(&self, key: WorldChunkPos) -> bool {
        self.detail.contains(&key)
    }

    /// Chunks (loaded or not) in the detail set.
    pub fn detail_count(&self) -> usize {
        self.detail.len()
    }

    /// Whether the detail set must be cut again for `centre` (`(point, radius)`,
    /// world sim units; `None` = no viewer): it appeared, went, changed its radius,
    /// or moved [`STUD_DETAIL_STEP_UNITS`] since the last cut.
    pub fn detail_stale(&self, centre: Option<(Vec3, f32)>) -> bool {
        match (self.detail_at, centre) {
            (None, None) => false,
            (Some((at, r)), Some((c, radius))) => r != radius || at.distance(c) >= STUD_DETAIL_STEP_UNITS,
            _ => true,
        }
    }

    /// Cut the detail set for `centre`: the chunks whose centre is within its radius
    /// (`systems::stud_tops_near`), plus those that had detail and are within
    /// [`STUD_DETAIL_KEEP_MARGIN_UNITS`] more. A chunk that gained detail is queued
    /// (its next job carries the flag); the ones that lost it are returned, for the
    /// caller to take their detail meshes away — no job needed.
    pub fn set_detail_centre(&mut self, centre: Option<(Vec3, f32)>) -> Vec<WorldChunkPos> {
        self.detail_at = centre;
        let mut next = BTreeSet::new();
        if let Some((c, radius)) = centre {
            let keep = radius + STUD_DETAIL_KEEP_MARGIN_UNITS;
            let (w, h) = (game::CHUNK_UNITS as f32, game::STUD_CHUNK_HEIGHT_UNITS as f32);
            // Chunks whose centre can lie within `keep` of `c`: a centre is half a
            // chunk from its min corner.
            let span = |v: f32, size: f32| (((v - keep) / size - 0.5).floor() as i64, ((v + keep) / size - 0.5).ceil() as i64);
            let ((x0, x1), (y0, y1), (z0, z1)) = (span(c.x, w), span(c.y, h), span(c.z, w));
            for x in x0..=x1 {
                for z in z0..=z1 {
                    for y in y0..=y1 {
                        let Ok(y) = i32::try_from(y) else { continue };
                        let key = WorldChunkPos::new(x, y, z);
                        let origin = world_chunk_origin(key);
                        let near = super::systems::stud_tops_near(origin, Some((c, radius)));
                        if near || (self.detail.contains(&key) && super::systems::stud_tops_near(origin, Some((c, keep)))) {
                            next.insert(key);
                        }
                    }
                }
            }
        }
        let lost: Vec<WorldChunkPos> = self.detail.difference(&next).copied().collect();
        let gained: Vec<WorldChunkPos> = next.difference(&self.detail).copied().collect();
        self.detail = next;
        for key in gained {
            self.mark_dirty(key);
        }
        for key in &lost {
            if let Some(d) = self.chunks.get_mut(key).and_then(|c| c.drawn.as_mut()) {
                (d.detail_tris, d.detail_verts) = (0, 0);
            }
        }
        lost
    }

    /// Record what the mesh that just landed on `key` draws.
    pub fn set_drawn(&mut self, key: WorldChunkPos, drawn: ChunkDrawn) {
        if let Some(c) = self.chunks.get_mut(&key) {
            c.drawn = Some(drawn);
        }
    }

    /// Whether some loaded region has not been reported by [`Self::region_meshed`] yet.
    pub fn regions_unreported(&self) -> bool {
        self.reported.len() < self.terrain.len()
    }

    /// The loaded regions whose every chunk has had its first mesh land, each
    /// returned ONCE (until the region is dropped) with what it draws.
    pub fn region_meshed(&mut self) -> Vec<(RegionId, RegionDrawn)> {
        let mut sums: BTreeMap<RegionId, Option<RegionDrawn>> = BTreeMap::new();
        for (key, c) in &self.chunks {
            let region = key.to_local().0;
            if self.reported.contains(&region) {
                continue;
            }
            let sum = sums.entry(region).or_insert_with(|| Some(RegionDrawn::default()));
            match (sum.as_mut(), c.drawn) {
                (Some(s), Some(d)) => {
                    s.chunks += 1;
                    s.body_meshes += usize::from(d.body_tris > 0);
                    s.detail_meshes += usize::from(d.detail_tris > 0);
                    s.body_tris += u64::from(d.body_tris);
                    s.body_verts += u64::from(d.body_verts);
                    s.detail_tris += u64::from(d.detail_tris);
                    s.detail_verts += u64::from(d.detail_verts);
                }
                // A chunk still waiting for its first mesh: not this time.
                _ => *sum = None,
            }
        }
        let done: Vec<(RegionId, RegionDrawn)> = sums.into_iter().filter_map(|(r, s)| Some((r, s?))).collect();
        self.reported.extend(done.iter().map(|(r, _)| *r));
        done
    }

    /// A job started for (`key`, `entity`) at `stamp` came back.
    pub fn finish(&mut self, key: WorldChunkPos, entity: Entity, stamp: u64) -> Finished {
        match self.chunks.get(&key) {
            Some(c) if c.entity == entity => {
                self.in_flight.remove(&key);
                if c.stamp == stamp {
                    Finished::Apply
                } else {
                    // Marked dirty since: it is in the queue (pushed again in case
                    // the queue was drained past it while it was in flight).
                    self.queue.push(key);
                    Finished::ApplyStale
                }
            }
            _ => Finished::Drop,
        }
    }
}

/// The client's `WorldView::stud_raycast`: the first occupied stud cell along a ray
/// (`origin` in `frame`'s local sim units, `reach` sim units) over every loaded
/// region's stud chunks and, where a neighbour is not loaded, the stud halo — so the
/// ray crosses region edges. `chunk_of` reads a loaded chunk entity's bricks.
pub fn stud_world_raycast<'a>(
    world: &'a StudWorld,
    chunk_of: impl Fn(Entity) -> Option<&'a StudChunk>,
    frame: RegionId,
    origin: game::rapier3d::math::Vector,
    dir: game::rapier3d::math::Vector,
    reach: f32,
) -> Option<game::StudWorldHit> {
    let n = game::REGION_CHUNKS as i64;
    let lookup = |[x, y, z]: [i64; 3]| -> Option<&'a StudChunk> {
        let key = WorldChunkPos::from_local(frame, [x as i32, i32::try_from(y).ok()?, z as i32]);
        match world.entity_at(key) {
            Some(e) => chunk_of(e),
            None => world.halo_chunk(key),
        }
    };
    // The frame region and its eight neighbours.
    let hit = game::raycast_studs_within(lookup, origin, dir, reach, -n..2 * n)?;
    let [x, y, z] = hit.chunk;
    let chunk = WorldChunkPos::from_local(frame, [x as i32, y as i32, z as i32]);
    Some(game::StudWorldHit { chunk, cell: hit.cell, normal: hit.normal, t: hit.t })
}

/// The min corner of world chunk `key` in world sim units.
pub fn world_chunk_origin(key: WorldChunkPos) -> Vec3 {
    Vec3::new(
        (key.x * game::CHUNK_UNITS as i64) as f32,
        (key.y as i64 * game::STUD_CHUNK_HEIGHT_UNITS as i64) as f32,
        (key.z * game::CHUNK_UNITS as i64) as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{StudBrick, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};

    fn e(n: u32) -> Entity {
        Entity::from_raw_u32(n).unwrap()
    }

    fn key(x: i64, y: i32, z: i64) -> WorldChunkPos {
        WorldChunkPos::new(x, y, z)
    }

    fn full() -> StudChunk {
        StudChunk::new([StudBrick::filler([0; 3], [STUD_CHUNK_STUDS as u8, STUD_CHUNK_PLATES as u8, STUD_CHUNK_STUDS as u8], 4)])
            .unwrap()
    }

    fn one(min: [u8; 3]) -> StudChunk {
        StudChunk::new([StudBrick::filler(min, [1, 1, 1], 4)]).unwrap()
    }

    fn drain(w: &mut StudWorld) -> Vec<WorldChunkPos> {
        let jobs = w.take_jobs(usize::MAX);
        for (k, en, stamp) in &jobs {
            assert_eq!(w.finish(*k, *en, *stamp), Finished::Apply);
        }
        jobs.into_iter().map(|j| j.0).collect()
    }

    #[test]
    fn an_edit_queues_the_chunk_and_only_the_neighbours_that_see_it() {
        let mut w = StudWorld::default();
        let (a, b, c) = (key(0, 0, 0), key(1, 0, 0), key(0, 0, 1));
        w.set_chunk(e(1), a, &StudChunk::default());
        w.set_chunk(e(2), b, &StudChunk::default());
        w.set_chunk(e(3), c, &StudChunk::default());
        assert_eq!(drain(&mut w).len(), 3);
        // A brick on a's +X boundary: a and b, not c.
        w.set_chunk(e(1), a, &one([31, 5, 5]));
        assert_eq!(drain(&mut w), vec![b, a]);
        // An interior edit: a alone.
        let mut inner = one([31, 5, 5]);
        inner.place(StudBrick::filler([10, 10, 10], [1, 1, 1], 4)).unwrap();
        w.set_chunk(e(1), a, &inner);
        assert_eq!(drain(&mut w), vec![a]);
        // Removing a queues both neighbours.
        w.remove_entity(e(1));
        let mut again = drain(&mut w);
        again.sort();
        assert_eq!(again, vec![c, b]);
    }

    #[test]
    fn repeated_edits_coalesce_and_a_stale_result_requeues() {
        let mut w = StudWorld::default();
        let a = key(0, 0, 0);
        w.set_chunk(e(1), a, &one([1, 1, 1]));
        w.set_chunk(e(1), a, &one([2, 1, 1]));
        w.set_chunk(e(1), a, &one([3, 1, 1]));
        let jobs = w.take_jobs(8);
        assert_eq!(jobs.len(), 1, "three edits, one job");
        let (_, en, stamp) = jobs[0];
        // Edited while the job is in flight: not submitted twice…
        w.set_chunk(e(1), a, &one([4, 1, 1]));
        assert!(w.take_jobs(8).is_empty(), "one job per chunk at a time");
        // …and the old result does not close the chunk.
        assert_eq!(w.finish(a, en, stamp), Finished::ApplyStale);
        let jobs = w.take_jobs(8);
        assert_eq!(jobs.len(), 1, "the newer content gets its own job");
        assert_eq!(w.finish(a, jobs[0].1, jobs[0].2), Finished::Apply);
        assert_eq!(w.queued(), 0);
    }

    #[test]
    fn a_result_for_a_gone_or_replaced_chunk_is_dropped() {
        let mut w = StudWorld::default();
        let a = key(0, 0, 0);
        w.set_chunk(e(1), a, &one([1, 1, 1]));
        let (_, en, stamp) = w.take_jobs(1)[0];
        w.remove_entity(e(1));
        assert_eq!(w.finish(a, en, stamp), Finished::Drop);
        // A new entity at the same place (a re-sent snapshot) is a new chunk.
        w.set_chunk(e(1), a, &one([1, 1, 1]));
        let (_, en, stamp) = w.take_jobs(1)[0];
        w.set_chunk(e(9), a, &one([1, 1, 1]));
        assert_eq!(w.finish(a, en, stamp), Finished::Drop);
        assert_eq!(w.key_of(e(1)), None);
        assert_eq!(w.take_jobs(1).len(), 1);
    }

    #[test]
    fn the_job_budget_is_honoured_oldest_first() {
        let mut w = StudWorld::default();
        for i in 0..5 {
            w.set_chunk(e(i + 1), key(i as i64 * 3, 0, 0), &one([1, 1, 1]));
        }
        let first = w.take_jobs(2);
        assert_eq!(first.iter().map(|j| j.0.x).collect::<Vec<_>>(), vec![0, 3]);
        assert_eq!(w.take_jobs(2).iter().map(|j| j.0.x).collect::<Vec<_>>(), vec![6, 9]);
        assert_eq!(w.queued(), 1);
    }

    #[test]
    fn a_region_edge_culls_against_the_loaded_neighbour_then_the_halo_then_the_recipe() {
        let n = game::REGION_CHUNKS as i64;
        let home = RegionId::new(0, 0);
        let east = RegionId::new(1, 0);
        let a = key(n - 1, 0, 0); // home's east edge chunk
        let b = key(n, 0, 0); // east's west edge chunk
        let mut w = StudWorld::default();
        w.set_chunk(e(1), a, &full());
        assert_eq!(w.neighbour_slab(a, 0), Slab::Free, "nothing known east of the edge");

        // The halo of home mirrors east's edge chunk.
        let region = |region, halo: Vec<(WorldChunkPos, Option<StudChunk>)>| StudWorldMsg::Region {
            region,
            terrain: TerrainKind::Stud,
            ground: Arc::new(BTreeMap::new()),
            halo,
            halo_ground: Vec::new(),
        };
        w.apply(&region(home, vec![(b, Some(full()))]));
        assert_eq!(w.neighbour_slab(a, 0), Slab::Solid);
        assert!(w.take_jobs(8).iter().any(|j| j.0 == a), "the halo's arrival re-meshes the edge chunk");
        assert_eq!(w.entity_at(b), None, "a halo chunk is data, never an entity");

        // The neighbour region loads: its chunk wins over the halo mirror.
        w.set_chunk(e(2), b, &one([0, 3, 4]));
        assert!(matches!(w.neighbour_slab(a, 0), Slab::Bits(_)));
        assert!(w.neighbour_slab(a, 0).get(0, 3, 4));
        assert_eq!(w.neighbour_slab(b, 1), Slab::Solid, "and it culls against home's chunk");

        // It unloads again: back to the halo.
        w.remove_entity(e(2));
        assert_eq!(w.neighbour_slab(a, 0), Slab::Solid);
        // The halo chunk goes too (undo of the seed): free.
        w.apply(&StudWorldMsg::Halo { region: home, chunks: vec![(b, None)], ground: Vec::new() });
        assert_eq!(w.neighbour_slab(a, 0), Slab::Free);

        // Implied ground: an unloaded chunk under the recipe's surface layers is solid.
        let profile = game::StudProfile { cover: 4, cover_plates: 1, soil: 4, soil_plates: 1 };
        let surface = 2 * STUD_CHUNK_PLATES as i32 + 10; // in layer 2
        let g = StudColumnGround::flat(surface, profile, 4, i32::MIN / 2);
        w.apply(&StudWorldMsg::Halo { region: home, chunks: Vec::new(), ground: vec![(key(n, 0, 0), Some(g.clone()))] });
        assert_eq!(w.neighbour_slab(a, 0), Slab::Solid, "layer 0 of a column whose surface is in layer 2");
        assert_eq!(w.neighbour_slab(key(n - 1, 2, 0), 0), Slab::Free, "a surface layer is never implied");
        let ground: StudGround = Arc::new([(ColumnCoords::new(0, 0), g)].into_iter().collect());
        w.apply(&StudWorldMsg::Region { region: east, terrain: TerrainKind::Stud, ground, halo: vec![], halo_ground: vec![] });
        assert_eq!(w.neighbour_slab(key(n, 1, 0), 3), Slab::Solid, "own recipe: the layer below is buried");
        assert_eq!(w.neighbour_slab(key(n, 2, 0), 2), Slab::Free);

        // Unloading home drops what it mirrored.
        w.prune_regions(|r| *r == east);
        assert!(w.halo_chunk(b).is_none());
        assert_eq!(w.terrain(home), TerrainKind::Voxel);
        assert_eq!(w.terrain(east), TerrainKind::Stud);
    }
    fn chunk_centre(k: WorldChunkPos) -> Vec3 {
        world_chunk_origin(k)
            + Vec3::new(game::CHUNK_UNITS as f32, game::STUD_CHUNK_HEIGHT_UNITS as f32, game::CHUNK_UNITS as f32) * 0.5
    }

    #[test]
    fn detail_follows_the_viewer_and_only_a_gain_costs_a_job() {
        let mut w = StudWorld::default();
        let (a, far) = (key(0, 0, 0), key(40, 0, 0));
        w.set_chunk(e(1), a, &one([1, 1, 1]));
        w.set_chunk(e(2), far, &one([1, 1, 1]));
        assert_eq!(drain(&mut w).len(), 2);
        assert!(!w.wants_detail(a), "no viewer yet");
        assert!(matches!(w.job(a, &one([1, 1, 1])), StudMeshJob::Chunk { detail: false, .. }));
        assert!(!w.detail_stale(None));

        // A viewer at a's centre: a (and unloaded chunks around it) gain detail; a is
        // queued, the far chunk is not.
        let radius = 2.0 * game::CHUNK_UNITS as f32;
        let at = Some((chunk_centre(a), radius));
        assert!(w.detail_stale(at));
        assert!(w.set_detail_centre(at).is_empty(), "nothing had detail to lose");
        assert!(w.wants_detail(a) && w.wants_detail(key(1, 0, 0)) && w.wants_detail(key(-2, 0, 0)));
        assert!(!w.wants_detail(key(3, 0, 0)) && !w.wants_detail(far));
        assert!(matches!(w.job(a, &one([1, 1, 1])), StudMeshJob::Chunk { detail: true, .. }));
        assert_eq!(drain(&mut w), vec![a]);
        // A chunk that loads inside the reach is meshed with detail from its first job.
        let b = key(1, 0, 0);
        w.set_chunk(e(3), b, &one([1, 1, 1]));
        assert!(matches!(w.job(b, &one([1, 1, 1])), StudMeshJob::Chunk { detail: true, .. }));
        drain(&mut w);

        // Standing still, or a small step: nothing is looked at again.
        assert!(!w.detail_stale(at));
        let nudged = Some((chunk_centre(a) + Vec3::X * (STUD_DETAIL_STEP_UNITS - 1.0), radius));
        assert!(!w.detail_stale(nudged));
        assert!(w.detail_stale(Some((chunk_centre(a), radius + 1.0))), "another radius (first person on / off)");

        // The viewer walks east. Just past a's reach a keeps its detail (the margin) ...
        let step = |d: f32| Some((chunk_centre(a) + Vec3::X * d, radius));
        assert!(w.detail_stale(step(radius + 1.0)));
        assert!(w.set_detail_centre(step(radius + 1.0)).iter().all(|k| *k != a));
        assert!(w.wants_detail(a));
        assert_eq!(w.queued(), 0, "a and b had detail already: no job");
        // ... and past the margin it loses it: returned, not queued.
        let lost = w.set_detail_centre(step(radius + STUD_DETAIL_KEEP_MARGIN_UNITS + 1.0));
        assert!(lost.contains(&a));
        assert!(!w.wants_detail(a) && w.wants_detail(b));
        assert_eq!(w.queued(), 0, "losing detail needs no job");
        // Coming back inside the margin does not give it back; inside the radius does.
        assert!(w.set_detail_centre(step(radius + 1.0)).is_empty());
        assert!(!w.wants_detail(a), "the margin only keeps");
        w.set_detail_centre(step(radius - 1.0));
        assert!(w.wants_detail(a));
        assert_eq!(drain(&mut w), vec![a]);

        // The viewer goes away: everything loses its detail.
        let lost = w.set_detail_centre(None);
        assert!(lost.contains(&a) && lost.contains(&b));
        assert_eq!(w.detail_count(), 0);
        assert!(!w.detail_stale(None));
    }

    #[test]
    fn a_region_is_reported_once_when_every_first_mesh_has_landed() {
        let mut w = StudWorld::default();
        let home = RegionId { x: 0, z: 0 };
        let ground: StudGround = Arc::new(BTreeMap::new());
        w.apply(&StudWorldMsg::Region { region: home, terrain: TerrainKind::Stud, ground, halo: vec![], halo_ground: vec![] });
        let (a, b) = (key(0, 0, 0), key(1, 0, 0));
        w.set_chunk(e(1), a, &one([1, 1, 1]));
        w.set_chunk(e(2), b, &one([1, 1, 1]));
        assert!(w.regions_unreported());
        assert!(w.region_meshed().is_empty(), "nothing landed");
        w.set_drawn(a, ChunkDrawn { body_tris: 12, body_verts: 24, detail_tris: 30, detail_verts: 80 });
        assert!(w.region_meshed().is_empty(), "b is still waiting");
        w.set_drawn(b, ChunkDrawn { body_tris: 10, body_verts: 20, ..Default::default() });
        let want = RegionDrawn { chunks: 2, body_meshes: 2, detail_meshes: 1, body_tris: 22, body_verts: 44, detail_tris: 30, detail_verts: 80 };
        assert_eq!(w.region_meshed(), vec![(home, want)]);
        assert!(!w.regions_unreported());
        assert!(w.region_meshed().is_empty(), "once");
        // An edit keeps what is drawn until the next mesh lands; a reload reports again.
        w.set_chunk(e(1), a, &one([2, 1, 1]));
        assert!(w.region_meshed().is_empty());
        w.prune_regions(|_| false);
        assert!(!w.regions_unreported(), "nothing loaded");
    }
}
