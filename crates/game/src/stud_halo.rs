//! The stud halo: `halo.rs` for stud terrain. Each region mirrors the stud chunks of
//! its neighbours' edge chunk columns (the same band, [`crate::halo::in_halo_band`]) as
//! fixed colliders, so a body crossing a seam has a floor on both sides and rays, reads
//! and edits see past the edge.
//!
//! **What travels.** Stud terrain is a ground recipe per chunk column plus the chunks
//! someone materialised (`stud_ground.rs`); an unedited chunk is a pure function of its
//! column's recipe. So the halo gets:
//! - on a **seed** ([`crate::HaloUpdate::replace`]): the neighbour's edge **recipes**
//!   ([`crate::HaloUpdate::stud_ground`]) and only the edge chunks the recipe cannot give
//!   back ([`crate::HaloUpdate::studs`]): edited ones, and ones materialised outside the
//!   recipe's surface layers. The receiver derives the pristine chunk of every surface
//!   layer itself (`StudColumnGround::pristine_chunk`) — exactly the chunks the owner
//!   materialised at genesis;
//! - on a **delta**: each edge chunk the owner wrote this slot, whole (also one a carve
//!   materialised), `None` when it is no longer materialised there.
//!
//! A seed comes from the neighbour's running state ([`Rollback::stud_edge`], answering
//! `RegionInput::HaloRequest`), from its parked / stored terrain
//! ([`crate::TerrainSnapshot::stud`], [`StudEdge::from_stored`]) or from worldgen for a
//! never-run one ([`StudEdge::from_seed`], `RegionRunner::set_stud_halo_generator`).
//!
//! **State.** `GameData::stud_halo` (chunk + its fixed body, by world chunk — `y` is the
//! stud layer) and `GameData::stud_halo_ground` (recipes, by world chunk column): ordinary
//! rollback state changed only by `GameEventKind::HaloUpdate`, hashed, replayed by the
//! predicting client in the same slot. Halo chunks are **not** entities: never in
//! `stud_index` or `Ecs::stud`, so nothing owner-side (edits, saves, the renderer's
//! chunk mirror) ever sees one, and the store never holds one (`portable.rs`). They are
//! rebuilt by the seeds of every region start.
//!
//! **Read-only.** A carve or placement that reaches a neighbour is an intent to it
//! ([`crate::Intent::CarveStud`], [`crate::Intent::PlaceStud`]); its halo delta brings
//! the result back. Ground the neighbour's edit opens onto in *this* region is
//! materialised here when that delta (or a seed listing the edited chunk) arrives
//! ([`Rollback::materialize_behind_stud_halo_safe`]) — the owner's own carve only grows
//! inside its region.
//!
//! **Renderer.** A halo chunk has no entity and is never drawn (the neighbour's region
//! draws its own chunks). Its content and the halo recipes are mirrored to a render
//! sink (`SetStudHalo`, `SetStudHaloGround` — render-only updates, no state), so the
//! client can cull faces at a region's edge and read tiles past it.

use std::collections::{BTreeMap, BTreeSet};

use rapier3d::math::{Pose, Vector};
use rapier3d::prelude::{RigidBodyBuilder, RigidBodyHandle};

use crate::halo::{halo_targets, in_halo_band, HALO_BODY_USER_DATA, HALO_CHUNKS};
use crate::stud::{StudChunk, StudChunkCoords, StudRayHit, STUD_CHUNK_HEIGHT_UNITS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};
use crate::stud_ground::{StudColumnGround, StudRegionSeed};
use crate::{
    ColumnCoords, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind, HaloUpdate, RegionCoords, Rollback,
    WorldChunkPos, CHUNK_UNITS, REGION_CHUNKS,
};

/// A region's stud cells per side (x and z).
pub const REGION_STUDS: i64 = (REGION_CHUNKS * STUD_CHUNK_STUDS) as i64;

/// A stud cell in WORLD cells: `[stud x, plate y, stud z]`, region `rc`'s cell `0` at
/// `rc * REGION_STUDS` on x and z; plates are not offset (regions tile only the plane).
pub type WorldStudCell = [i64; 3];

/// The world cell of region-local stud cell `cell` of `rc` (which may lie past its edge).
pub fn world_stud_cell(rc: RegionCoords, cell: [i64; 3]) -> WorldStudCell {
    [rc.x as i64 * REGION_STUDS + cell[0], cell[1], rc.z as i64 * REGION_STUDS + cell[2]]
}

/// `cell` in `rc`'s frame (may lie outside it).
pub fn local_stud_cell(rc: RegionCoords, cell: WorldStudCell) -> [i64; 3] {
    [cell[0] - rc.x as i64 * REGION_STUDS, cell[1], cell[2] - rc.z as i64 * REGION_STUDS]
}

/// The world chunk (`y` = stud layer) holding world stud cell `cell`.
pub fn stud_chunk_of_cell(cell: WorldStudCell) -> WorldChunkPos {
    let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
    let y = cell[1].div_euclid(ch).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    WorldChunkPos::new(cell[0].div_euclid(cw), y, cell[2].div_euclid(cw))
}

/// One halo stud chunk changed: its new content, `None` = not materialised at its owner.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudHaloDelta {
    /// World chunk; `y` is the stud layer.
    pub chunk: WorldChunkPos,
    pub data: Option<StudChunk>,
}

/// A mirrored stud chunk and the fixed body carrying its colliders.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, state_hash::StateHash)]
pub struct StudHaloChunk {
    /// `None`: the chunk went away; its body stays, colliders disabled.
    pub chunk: Option<StudChunk>,
    pub body: RigidBodyHandle,
}

/// The stud terrain of a region's edge chunk columns (the ones some neighbour mirrors),
/// in the region's own frame: what a halo seed is cut from.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudEdge {
    /// The edge columns' ground recipes.
    pub ground: Vec<(ColumnCoords, StudColumnGround)>,
    /// The edge chunks the recipes cannot give back: every materialised chunk that is
    /// not exactly its recipe's pristine chunk in a surface layer.
    pub chunks: Vec<(StudChunkCoords, StudChunk)>,
}

/// Whether some neighbour mirrors chunk column `(x, z)` of a region.
fn edge_column(x: usize, z: usize) -> bool {
    !halo_targets(x as i32, z as i32).is_empty()
}

/// Whether a receiver derives `chunk` at `c` by itself from recipe `g`: pristine, in a
/// surface layer.
fn derived(g: Option<&StudColumnGround>, c: StudChunkCoords, chunk: &StudChunk) -> bool {
    g.is_some_and(|g| g.surface_layers().contains(&c.y) && g.pristine_chunk(c.y).as_ref() == Some(chunk))
}

impl StudEdge {
    pub fn is_empty(&self) -> bool {
        self.ground.is_empty() && self.chunks.is_empty()
    }

    /// The edge of a never-run region, from what it would start from (`seed` may cover
    /// only some chunk columns: a halo window).
    pub fn from_seed(seed: &StudRegionSeed) -> Self {
        let recipes: BTreeMap<ColumnCoords, &StudColumnGround> = seed.ground.iter().map(|(c, g)| (*c, g)).collect();
        let mut chunks: BTreeMap<StudChunkCoords, StudChunk> = seed
            .feature_chunks()
            .into_iter()
            .filter(|(c, chunk)| edge_column(c.x, c.z) && !derived(recipes.get(&ColumnCoords::new(c.x, c.z)).copied(), *c, chunk))
            .collect();
        // The cliff walls genesis materialises (`stud_wall_layers`) against the columns
        // the seed holds: the whole region's for a whole seed, the band's for a window.
        for (c, g) in seed.ground.iter().filter(|(c, _)| edge_column(c.x, c.z)) {
            for y in crate::stud_ground::stud_wall_layers(&recipes, *c) {
                if let Some(chunk) = g.pristine_chunk(y) {
                    chunks.entry(StudChunkCoords { x: c.x, y, z: c.z }).or_insert(chunk);
                }
            }
        }
        let chunks = chunks.into_iter().collect();
        let ground = seed.ground.iter().filter(|(c, _)| edge_column(c.x, c.z)).cloned().collect();
        Self { ground, chunks }
    }

    /// The edge of a stored region, from its stored recipes and stud rows (`None` = a
    /// `Pristine` marker: materialised with exactly its recipe's chunk).
    pub fn from_stored(
        ground: &[(ColumnCoords, StudColumnGround)],
        rows: impl IntoIterator<Item = (StudChunkCoords, Option<StudChunk>)>,
    ) -> Self {
        let recipes: BTreeMap<ColumnCoords, &StudColumnGround> = ground.iter().map(|(c, g)| (*c, g)).collect();
        let mut chunks = Vec::new();
        for (c, row) in rows {
            if !edge_column(c.x, c.z) {
                continue;
            }
            let chunk = match row {
                Some(chunk) => chunk,
                None => match recipes.get(&ColumnCoords::new(c.x, c.z)).and_then(|g| g.pristine_chunk(c.y)) {
                    Some(chunk) => chunk,
                    None => continue,
                },
            };
            chunks.push((c, chunk));
        }
        chunks.sort_by_key(|(c, _)| *c);
        let ground = ground.iter().filter(|(c, _)| edge_column(c.x, c.z)).cloned().collect();
        Self { ground, chunks }
    }

    /// The part of this edge (region `from`'s) that neighbour `to` mirrors, as the stud
    /// lists of a seed: `(HaloUpdate::studs, HaloUpdate::stud_ground)`.
    pub(crate) fn seed_parts(
        &self,
        from: RegionCoords,
        offset: (i32, i32),
    ) -> (Vec<StudHaloDelta>, Vec<(WorldChunkPos, StudColumnGround)>) {
        let mirrors = |x: usize, z: usize| halo_targets(x as i32, z as i32).contains(&offset);
        let mut studs: Vec<StudHaloDelta> = self
            .chunks
            .iter()
            .filter(|(c, _)| mirrors(c.x, c.z))
            .map(|(c, chunk)| StudHaloDelta {
                chunk: WorldChunkPos::from_local(from, [c.x as i32, c.y, c.z as i32]),
                data: Some(chunk.clone()),
            })
            .collect();
        studs.sort_by_key(|d| d.chunk);
        let mut ground: Vec<(WorldChunkPos, StudColumnGround)> = self
            .ground
            .iter()
            .filter(|(c, _)| mirrors(c.x, c.z))
            .map(|(c, g)| (WorldChunkPos::from_local(from, [c.x as i32, 0, c.z as i32]), g.clone()))
            .collect();
        ground.sort_by_key(|(k, _)| *k);
        (studs, ground)
    }
}

thread_local! {
    /// Stud chunks written on this thread since the last [`take_touched_stud_chunks`]
    /// (the stud twin of `halo::touch_chunk`). Not state: never hashed, serialized or
    /// undone.
    static TOUCHED: std::cell::RefCell<Vec<StudChunkCoords>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Notes that the stud chunk at `c` was written (set, or created).
pub(crate) fn touch_stud_chunk(c: StudChunkCoords) {
    TOUCHED.with(|t| t.borrow_mut().push(c));
}

/// The stud chunks written on this thread since the previous call.
pub(crate) fn take_touched_stud_chunks() -> Vec<StudChunkCoords> {
    TOUCHED.with(|t| std::mem::take(&mut *t.borrow_mut()))
}

/// The origin (region-local units) of the stud chunk at local chunk `[x, layer, z]`,
/// which may lie past the region's edge.
fn local_origin(local: [i32; 3]) -> Vector {
    Vector::new(
        local[0] as f32 * CHUNK_UNITS as f32,
        local[1] as f32 * STUD_CHUNK_HEIGHT_UNITS as f32,
        local[2] as f32 * CHUNK_UNITS as f32,
    )
}

impl Rollback {
    /// This region's stud edge now (see [`StudEdge`]): what its neighbours' halo seeds
    /// are cut from.
    pub fn stud_edge(&self) -> StudEdge {
        let d = &self.data;
        let ground: Vec<(ColumnCoords, StudColumnGround)> =
            d.stud_ground.iter().filter(|(c, _)| edge_column(c.x, c.z)).map(|(c, g)| (*c, g.clone())).collect();
        let mut chunks = Vec::new();
        for (&c, &key) in d.stud_index.iter() {
            if !edge_column(c.x, c.z) {
                continue;
            }
            let Some(chunk) = d.ecs.stud.try_get(key).as_ref() else { continue };
            if !derived(d.stud_ground.get(&ColumnCoords::new(c.x, c.z)), c, chunk) {
                chunks.push((c, chunk.clone()));
            }
        }
        StudEdge { ground, chunks }
    }

    /// The halo deltas of own stud chunks `coords` (written since the last call), per
    /// neighbour offset that mirrors them: each chunk's content now.
    pub(crate) fn stud_halo_deltas_for(&self, coords: &BTreeSet<StudChunkCoords>) -> BTreeMap<(i32, i32), Vec<StudHaloDelta>> {
        let from = *self.data.region;
        let mut by_target: BTreeMap<(i32, i32), Vec<StudHaloDelta>> = BTreeMap::new();
        for &c in coords {
            let targets = halo_targets(c.x as i32, c.z as i32);
            if targets.is_empty() {
                continue;
            }
            let chunk = WorldChunkPos::from_local(from, [c.x as i32, c.y, c.z as i32]);
            let data = self.stud_chunk_at(c).cloned();
            for t in targets {
                by_target.entry(t).or_default().push(StudHaloDelta { chunk, data: data.clone() });
            }
        }
        // `coords` is ordered by (x, y, z), as `WorldChunkPos` is.
        by_target
    }

    /// The halo stud chunk at local chunk `[x, layer, z]` (outside the region), if the
    /// halo holds one with content.
    pub fn stud_halo_chunk_at(&self, local: [i32; 3]) -> Option<&StudChunk> {
        self.data.stud_halo.get(&WorldChunkPos::from_local(*self.data.region, local))?.chunk.as_ref()
    }

    /// The halo's mirrored stud chunks, by world chunk (`y` = layer).
    pub fn stud_halo(&self) -> &BTreeMap<WorldChunkPos, StudHaloChunk> {
        &self.data.stud_halo
    }

    /// The halo's stud ground recipes, by world chunk column (`y` 0).
    pub fn stud_halo_grounds(&self) -> &BTreeMap<WorldChunkPos, StudColumnGround> {
        &self.data.stud_halo_ground
    }

    /// The stud chunk at local chunk `[x, layer, z]` as this region sees it: its own
    /// materialised one, or the halo's past its edge. `None` for anything not
    /// materialised (implied ground has no bricks to hit) or outside own ∪ halo.
    pub fn stud_chunk_seen(&self, local: [i64; 3]) -> Option<&StudChunk> {
        let n = REGION_CHUNKS as i64;
        let y = i32::try_from(local[1]).ok()?;
        if (0..n).contains(&local[0]) && (0..n).contains(&local[2]) {
            return self.stud_chunk_at(StudChunkCoords { x: local[0] as usize, y, z: local[2] as usize });
        }
        let (x, z) = (i32::try_from(local[0]).ok()?, i32::try_from(local[2]).ok()?);
        self.stud_halo_chunk_at([x, y, z])
    }

    /// The material at region-local stud cell `cell` (`[stud x, plate y, stud z]`), own ∪
    /// halo: [`Self::stud_material_at`] inside the region; past its edge the halo's
    /// chunk, else the neighbour's recipe (implied ground). `None` is air, or outside
    /// own ∪ halo.
    pub fn stud_material_seen(&self, cell: [i64; 3]) -> Option<u16> {
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let plate = i32::try_from(cell[1]).ok()?;
        if (0..REGION_STUDS).contains(&cell[0]) && (0..REGION_STUDS).contains(&cell[2]) {
            return self.stud_material_at([cell[0] as i32, plate, cell[2] as i32]);
        }
        let chunk = [cell[0].div_euclid(cw) as i32, cell[1].div_euclid(ch) as i32, cell[2].div_euclid(cw) as i32];
        if !in_halo_band(chunk[0], chunk[2]) {
            return None;
        }
        let local = [cell[0].rem_euclid(cw) as u8, cell[1].rem_euclid(ch) as u8, cell[2].rem_euclid(cw) as u8];
        let key = WorldChunkPos::from_local(*self.data.region, chunk);
        if let Some(c) = self.data.stud_halo.get(&key).and_then(|h| h.chunk.as_ref()) {
            return c.brick_at(local).map(|b| b.material);
        }
        let g = self.data.stud_halo_ground.get(&WorldChunkPos::new(key.x, 0, key.z))?;
        g.material_at(local[0] as usize, plate, local[2] as usize)
    }

    /// The first occupied stud cell along a ray (region-local units) within `reach`
    /// units, over own ∪ halo chunks ([`Self::stud_chunk_seen`]); the hit's chunk is in
    /// this region's chunk frame and may lie past its edge.
    pub fn raycast_studs_seen(&self, origin: Vector, dir: Vector, reach: f32) -> Option<StudRayHit> {
        let n = REGION_CHUNKS as i64;
        let h = HALO_CHUNKS as i64;
        crate::stud::raycast_studs_within(|c| self.stud_chunk_seen(c), origin, dir, reach, -h..n + h)
    }

    /// The stud part of [`Self::apply_halo_update_safe`], undo-safe: the recipes; on a
    /// seed, the pristine chunk of every surface layer of `u.from`'s recipes and the
    /// drop of every other chunk of it that the seed does not list; then the listed
    /// chunks; then this region's implied ground facing an edited chunk's air
    /// ([`Self::materialize_behind_stud_halo_safe`]).
    pub(crate) fn apply_stud_halo_update_safe(&mut self, u: &HaloUpdate) {
        let region = *self.data.region;
        let ours = |k: &WorldChunkPos| {
            let local = k.local_in(region);
            k.to_local().0 == u.from && in_halo_band(local[0], local[2])
        };
        for (k, g) in &u.stud_ground {
            if !ours(k) || k.y != 0 {
                log::warn!("halo update from {:?}: stud ground for {k:?} outside its band; ignored", u.from);
                continue;
            }
            if self.data.stud_halo_ground.get(k) != Some(g) {
                if self.data.has_render_sink() {
                    let prior = self.data.stud_halo_ground.get(k).cloned();
                    self.data.emit_on_undo(GameDataUpdate::new(
                        GameDataTransactionKind::Undo,
                        GameDataUpdateKind::SetStudHaloGround(vec![(*k, prior)]),
                    ));
                    let now = GameDataUpdateKind::SetStudHaloGround(vec![(*k, Some(g.clone()))]);
                    self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, now));
                }
                self.data.stud_halo_ground.insert(*k, g.clone());
            }
        }
        let mut wanted: BTreeMap<WorldChunkPos, Option<StudChunk>> = BTreeMap::new();
        if u.replace {
            for (k, h) in self.data.stud_halo.iter() {
                if h.chunk.is_some() && k.to_local().0 == u.from {
                    wanted.insert(*k, None);
                }
            }
            for (k, g) in self.data.stud_halo_ground.iter() {
                if k.to_local().0 != u.from {
                    continue;
                }
                // Its surface layers, and under them the wall it shows this region's
                // lower edge column across the seam.
                let wall = self.own_column_facing(*k).map_or(0..0, |(_, own, side)| g.wall_layers(own.edge_low(side)));
                for y in wall.chain(g.surface_layers()) {
                    wanted.insert(WorldChunkPos::new(k.x, y, k.z), g.pristine_chunk(y));
                }
            }
        }
        // This region's own walls: an edge column standing over the neighbour's lower
        // ground materialises the layers that ground opens onto (undo-safe, like any
        // runtime chunk; the neighbour's halo takes them from this region's deltas).
        for (k, g) in &u.stud_ground {
            let Some((c, own, side)) = self.own_column_facing(*k) else { continue };
            let layers = own.wall_layers(g.edge_low(side.opposite()));
            let own = own.clone();
            for y in layers {
                let coords = StudChunkCoords { x: c.x, y, z: c.z };
                if self.stud_key_at(coords).is_none() {
                    if let Some(chunk) = own.pristine_chunk(y) {
                        self.create_stud_chunk_safe(coords, chunk);
                    }
                }
            }
        }
        let mut edited = Vec::new();
        for d in &u.studs {
            if !ours(&d.chunk) || !crate::stud::stud_layer_ok(d.chunk.y) {
                log::warn!("halo update from {:?}: stud chunk {:?} outside its band; ignored", u.from, d.chunk);
                continue;
            }
            wanted.insert(d.chunk, d.data.clone());
            edited.push(d.chunk);
        }
        for (k, data) in wanted {
            self.apply_stud_halo_delta_safe(&StudHaloDelta { chunk: k, data });
        }
        self.materialize_behind_stud_halo_safe(&edited);
        // ...and the ground of this region their open cells expose becomes real
        // bricks, as if the hole had been dug on this side of the seam.
        let (cw, ch) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
        let boxes: Vec<([i64; 3], [i64; 3])> = edited
            .iter()
            .map(|k| {
                let lo = k.local_in(region).map(i64::from);
                let lo = [lo[0] * cw, lo[1] * ch, lo[2] * cw];
                (lo, [lo[0] + cw, lo[1] + ch, lo[2] + cw])
            })
            .collect();
        self.rebrick_stud_exposed_safe(&boxes);
    }

    /// This region's chunk column across a face from halo column `k` (a world chunk
    /// column of the band), its recipe, and the side of it that faces `k`. `None` for a
    /// corner column (no shared face) or one outside the band.
    fn own_column_facing(&self, k: WorldChunkPos) -> Option<(ColumnCoords, &StudColumnGround, crate::stud_ground::StudSide)> {
        use crate::stud_ground::StudSide;
        let n = REGION_CHUNKS as i32;
        let inside = |c: i32| (0..n).contains(&c);
        let [lx, _, lz] = k.local_in(*self.data.region);
        let (own, side) = match (lx, lz) {
            (-1, z) if inside(z) => ([0, z], StudSide::West),
            (x, z) if x == n && inside(z) => ([n - 1, z], StudSide::East),
            (x, -1) if inside(x) => ([x, 0], StudSide::North),
            (x, z) if z == n && inside(x) => ([x, n - 1], StudSide::South),
            _ => return None,
        };
        let c = ColumnCoords::new(own[0] as usize, own[1] as usize);
        self.data.stud_ground.get(&c).map(|g| (c, g, side))
    }

    /// One halo stud chunk takes its new content, undo-safely: the stored chunk and
    /// the fixed colliders on its body (built the first time, synced in place after).
    /// A chunk outside this region's band, or unchanged, is ignored.
    pub(crate) fn apply_stud_halo_delta_safe(&mut self, d: &StudHaloDelta) {
        let local = d.chunk.local_in(*self.data.region);
        if !in_halo_band(local[0], local[2]) {
            log::debug!("stud halo delta {:?} outside the band of {:?}; ignored", d.chunk, *self.data.region);
            return;
        }
        let wanted = |chunk: &Option<StudChunk>| match chunk {
            Some(c) => crate::stud_ops::stud_colliders_tagged(c, HALO_BODY_USER_DATA),
            None => Vec::new(),
        };
        // Render-only mirror (the client's stud mesher and tile mirror); no state.
        let unchanged = match self.data.stud_halo.get(&d.chunk) {
            Some(h) => h.chunk == d.data,
            None => d.data.is_none(),
        };
        if !unchanged && self.data.has_render_sink() {
            let prior = self.data.stud_halo.get(&d.chunk).and_then(|h| h.chunk.clone());
            self.data.emit_on_undo(GameDataUpdate::new(
                GameDataTransactionKind::Undo,
                GameDataUpdateKind::SetStudHalo(vec![(d.chunk, prior)]),
            ));
            let now = GameDataUpdateKind::SetStudHalo(vec![(d.chunk, d.data.clone())]);
            self.data.send(GameDataUpdate::new(GameDataTransactionKind::Do, now));
        }
        match self.data.stud_halo.get(&d.chunk).map(|h| (h.chunk == d.data, h.body)) {
            Some((true, _)) => {}
            Some((false, body)) => {
                self.sync_body_colliders_to_safe(body, wanted(&d.data));
                self.data.stud_halo.insert(d.chunk, StudHaloChunk { chunk: d.data.clone(), body });
            }
            None => {
                if d.data.is_none() {
                    return;
                }
                let pose = Pose::from_translation(local_origin(local));
                let body = self.insert_body_safe(RigidBodyBuilder::fixed().pose(pose).user_data(HALO_BODY_USER_DATA).build());
                for collider in wanted(&d.data) {
                    self.attach_collider_safe(body, collider);
                }
                self.data.stud_halo.insert(d.chunk, StudHaloChunk { chunk: d.data.clone(), body });
            }
        }
    }

    /// After the neighbours' edited halo chunks `keys` changed: every chunk of this
    /// region that faces one across a seam, is still implied ground, and borders an
    /// empty cell of it is materialised (undo-safely) with its recipe's pristine chunk
    /// — a neighbour's dig never opens onto ground that has no bricks or collider. Its
    /// halo delta then brings the chunk to the neighbour. Corner chunks share no face
    /// with this region and are skipped.
    pub(crate) fn materialize_behind_stud_halo_safe(&mut self, keys: &[WorldChunkPos]) {
        let n = REGION_CHUNKS as i32;
        let last = STUD_CHUNK_STUDS as i32 - 1;
        let inside = |c: i32| (0..n).contains(&c);
        for &key in keys {
            let [lx, ly, lz] = key.local_in(*self.data.region);
            // The own chunk across the shared face, and the halo chunk's face plane
            // (axis, stud index) touching it.
            let (own, axis, plane) = match (lx, lz) {
                (-1, z) if inside(z) => ([0, z], 0, last),
                (x, z) if x == n && inside(z) => ([n - 1, z], 0, 0),
                (x, -1) if inside(x) => ([x, 0], 2, last),
                (x, z) if z == n && inside(x) => ([x, n - 1], 2, 0),
                _ => continue,
            };
            let coords = StudChunkCoords { x: own[0] as usize, y: ly, z: own[1] as usize };
            if self.stud_key_at(coords).is_some() {
                continue;
            }
            let Some(halo) = self.data.stud_halo.get(&key).and_then(|h| h.chunk.as_ref()) else { continue };
            let occ = halo.occupancy();
            let opens = (0..STUD_CHUNK_STUDS as i32).any(|a| {
                (0..STUD_CHUNK_PLATES as i32).any(|y| {
                    let mut p = [0, y, 0];
                    p[axis] = plane;
                    p[2 - axis] = a;
                    !occ.get(p)
                })
            });
            if !opens {
                continue;
            }
            let pristine =
                self.data.stud_ground.get(&ColumnCoords::new(coords.x, coords.z)).and_then(|g| g.pristine_chunk(coords.y));
            if let Some(chunk) = pristine {
                self.create_stud_chunk_safe(coords, chunk);
            }
        }
    }
}
