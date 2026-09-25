//! Cell loading inside one region (spec:
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md).
//! A cell is a `cell_chunks` × `cell_chunks` square of chunk columns; the
//! server activates cells around players through `CellChanges` events.

use std::collections::BTreeSet;

use crate::{
    Chunk, ChunkCoords, EntityBundle, EntityKey, RegionCoords, Rollback, REGION_CHUNKS, REGION_SIZE,
};

/// Region-local cell address, `0..cells_per_side` on each axis.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
    serde::Serialize, serde::Deserialize,
)]
pub struct CellCoords {
    pub x: u32,
    pub z: u32,
}

impl CellCoords {
    pub fn new(x: u32, z: u32) -> Self {
        Self { x, z }
    }
}

/// Cells along one region edge. Panics unless `cell_chunks` is a power of
/// two that divides `REGION_CHUNKS`.
pub fn cells_per_side(cell_chunks: usize) -> u32 {
    assert!(
        cell_chunks.is_power_of_two() && cell_chunks <= REGION_CHUNKS,
        "cell_chunks {cell_chunks} must be a power of two dividing REGION_CHUNKS {REGION_CHUNKS}"
    );
    (REGION_CHUNKS / cell_chunks) as u32
}

/// Cell edge in world units (32 units per chunk).
pub fn cell_size_units(cell_chunks: usize) -> f32 {
    (cell_chunks * 32) as f32
}

pub fn cell_of_chunk(c: ChunkCoords, cell_chunks: usize) -> CellCoords {
    CellCoords::new((c.x / cell_chunks) as u32, (c.z / cell_chunks) as u32)
}

/// The cell containing region-local `(x, z)`, `None` outside the region.
/// `floor` semantics: a point exactly on an edge belongs to the higher cell.
pub fn cell_of_pos(x: f32, z: f32, cell_chunks: usize) -> Option<CellCoords> {
    if !(0.0..REGION_SIZE).contains(&x) || !(0.0..REGION_SIZE).contains(&z) {
        return None;
    }
    let size = cell_size_units(cell_chunks);
    Some(CellCoords::new((x / size).floor() as u32, (z / size).floor() as u32))
}

/// The cell containing block `pos` (block units), `None` outside the region.
pub fn cell_of_block(pos: [i32; 3], cell_chunks: usize) -> Option<CellCoords> {
    let cb = crate::CHUNK_BLOCKS as i32;
    let (cx, cz) = (pos[0].div_euclid(cb), pos[2].div_euclid(cb));
    let n = REGION_CHUNKS as i32;
    if cx < 0 || cz < 0 || cx >= n || cz >= n {
        return None;
    }
    Some(cell_of_chunk(ChunkCoords::new(cx as usize, 0, cz as usize), cell_chunks))
}

pub fn chebyshev(a: CellCoords, b: CellCoords) -> u32 {
    a.x.abs_diff(b.x).max(a.z.abs_diff(b.z))
}

/// Every cell within Chebyshev `radius` of `center`, clipped to the grid,
/// sorted.
pub fn cells_within(center: CellCoords, radius: u32, cell_chunks: usize) -> Vec<CellCoords> {
    let n = cells_per_side(cell_chunks);
    let lo = |v: u32| v.saturating_sub(radius);
    let hi = |v: u32| (v + radius).min(n - 1);
    let mut out = Vec::new();
    for x in lo(center.x)..=hi(center.x) {
        for z in lo(center.z)..=hi(center.z) {
            out.push(CellCoords::new(x, z));
        }
    }
    out
}

/// Whether a LOD cell of the viewer's HOME region should be hidden because
/// full-detail cells cover it (phase 1c: the home region gets LOD too, with
/// holes where its active cells render from the sim). `min_xz` is the LOD
/// cell's world-space minimum corner, `size` its edge in world units.
/// Hidden only if it lies inside `home` and EVERY game cell under it is
/// active: a partly covered LOD cell stays visible (overlap, never a hole).
pub fn lod_cell_masked(
    home: RegionCoords,
    active: &BTreeSet<CellCoords>,
    cell_chunks: usize,
    min_xz: [f32; 2],
    size: f32,
) -> bool {
    if active.is_empty() || cell_chunks == 0 {
        return false;
    }
    let o = home.world_offset();
    let (lx, lz) = (min_xz[0] - o[0], min_xz[1] - o[2]);
    if lx < 0.0 || lz < 0.0 || lx + size > REGION_SIZE || lz + size > REGION_SIZE {
        return false;
    }
    let cs = cell_size_units(cell_chunks);
    // Cells overlapped by [lx, lx+size) × [lz, lz+size).
    let (x0, z0) = ((lx / cs).floor() as u32, (lz / cs).floor() as u32);
    let (x1, z1) = (((lx + size) / cs).ceil() as u32, ((lz + size) / cs).ceil() as u32);
    (x0..x1).all(|x| (z0..z1).all(|z| active.contains(&CellCoords::new(x, z))))
}

/// Sim-side cell bookkeeping (tier-2 field `GameData.cells`). Mutated ONLY
/// raw, inside the `CellChanges` arm, which every replica forgets at once.
#[derive(Debug, Clone, Default, Hash, serde::Serialize, serde::Deserialize)]
pub struct CellState {
    /// 0 = legacy region (fully materialized, no cells).
    pub cell_chunks: usize,
    pub active: BTreeSet<CellCoords>,
}

impl CellState {
    pub fn enabled(&self) -> bool {
        self.cell_chunks != 0
    }
}

/// A cell's full content while it is not in the sim: the server's store
/// entry, and the activation payload clients materialize.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DormantCell {
    /// Sorted by `ChunkCoords`.
    pub chunks: Vec<(ChunkCoords, Chunk)>,
    /// Frozen non-player entities, in extraction order.
    pub entities: Vec<EntityBundle>,
}

impl DormantCell {
    /// crc32 of the bincode encoding — the store's change detector.
    pub fn content_crc(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(self).expect("dormant cell serializes"));
        h.finalize()
    }
}

// `Chunk` has no PartialEq; compare cells by their encoding.
impl PartialEq for DormantCell {
    fn eq(&self, other: &Self) -> bool {
        bincode::serialize(self).ok() == bincode::serialize(other).ok()
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellPayload {
    pub cell: CellCoords,
    /// Store version this payload was cut from.
    pub version: u64,
    /// `Rollback::cell_hash` of the cell as materialized in the SERVER sim.
    /// 0 when the server builds the event; the server's `handle_event`
    /// fills it before broadcast; clients verify it.
    pub materialized_hash: u32,
    pub content: DormantCell,
}

/// The one server-authored cell event per tick: deactivations first, then
/// activations, each sorted by `CellCoords`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellChanges {
    pub deactivate: Vec<CellCoords>,
    pub activate: Vec<CellPayload>,
}

impl CellChanges {
    pub fn is_empty(&self) -> bool {
        self.deactivate.is_empty() && self.activate.is_empty()
    }
}

impl Rollback {
    fn chunk_coords_of(&self, key: EntityKey) -> Option<ChunkCoords> {
        let handle = (*self.data.ecs.rigidbody.try_get(key))?;
        let t = self.data.physics.bodies.get(handle)?.translation();
        // Chunk bodies sit at exact multiples of 32 (seed_terrain_genesis / create_mesh).
        Some(ChunkCoords::new(
            (t.x.0 / 32.0) as usize,
            (t.y.0 / 32.0).floor() as i32,
            (t.z.0 / 32.0) as usize,
        ))
    }

    /// Chunk entities in `cell`, sorted by coordinates.
    pub fn cell_chunk_keys(&self, cell: CellCoords) -> Vec<(ChunkCoords, EntityKey)> {
        let cc = self.data.cells.cell_chunks;
        let mut out: Vec<_> = self
            .data
            .ecs
            .chunk
            .iter()
            .filter(|(_, c)| c.is_some())
            .filter_map(|(k, _)| self.chunk_coords_of(k).map(|c| (c, k)))
            .filter(|(c, _)| cell_of_chunk(*c, cc) == cell)
            .collect();
        out.sort_by_key(|(c, _)| (c.x, c.y, c.z));
        out
    }

    /// Kind-bearing entities (players, mobs) whose body is in `cell`,
    /// ghosts excluded, in key order.
    pub fn cell_entity_keys(&self, cell: CellCoords) -> Vec<EntityKey> {
        let cc = self.data.cells.cell_chunks;
        let ghosts: BTreeSet<EntityKey> = self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        self.data
            .ecs
            .kind
            .iter()
            .filter(|(k, kind)| kind.is_some() && !ghosts.contains(k))
            .filter_map(|(k, _)| {
                let h = (*self.data.ecs.rigidbody.try_get(k))?;
                let t = self.data.physics.bodies.get(h)?.translation();
                (cell_of_pos(t.x.0, t.z.0, cc) == Some(cell)).then_some(k)
            })
            .collect()
    }

    pub fn cell_has_player(&self, cell: CellCoords) -> bool {
        let players: BTreeSet<EntityKey> = self.data.player_entites.values().copied().collect();
        self.cell_entity_keys(cell).iter().any(|k| players.contains(k))
    }

    /// Hash of the cell AS MATERIALIZED: chunk content, entity keys, bodies
    /// and colliders of its terrain, plus its entities' bodies. Computed by
    /// the server after activation and re-computed by clients.
    pub fn cell_hash(&self, cell: CellCoords) -> u32 {
        use std::hash::Hash;
        let mut h = crc32fast::Hasher::new();
        cell.hash(&mut h);
        for (coords, key) in self.cell_chunk_keys(cell) {
            (coords.x, coords.y, coords.z).hash(&mut h);
            key.hash(&mut h);
            self.data.ecs.chunk.try_get(key).hash(&mut h);
            if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
                handle.hash(&mut h);
                if let Some(body) = self.data.physics.bodies.get(handle) {
                    body.hash(&mut h);
                    for ch in body.colliders() {
                        ch.hash(&mut h);
                        self.data.physics.colliders.get(*ch).hash(&mut h);
                    }
                }
            }
        }
        for key in self.cell_entity_keys(cell) {
            key.hash(&mut h);
            self.data.ecs.kind.try_get(key).hash(&mut h);
            if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
                self.data.physics.bodies.get(handle).hash(&mut h);
            }
        }
        h.finalize()
    }

    /// Activation. Terrain goes through the raw genesis path; entities
    /// through the (logged) arrival path. Only valid inside the
    /// `CellChanges` arm, whose transaction is forgotten at once.
    pub fn materialize_cell(&mut self, payload: &CellPayload) {
        let chunks = payload.content.chunks.clone();
        // Client regions hand the renderer each new chunk raw; it spawns the
        // chunk and derives voxels off-thread (budgeted, nearest first), as
        // for a join snapshot. Server regions have no render sink.
        let render = self.data.has_render_sink();
        let raw: Vec<(ChunkCoords, Chunk)> = if render { chunks.clone() } else { Vec::new() };
        self.seed_terrain_genesis(chunks);
        if render {
            let index: std::collections::BTreeMap<(usize, i32, usize), EntityKey> = self
                .cell_chunk_keys(payload.cell)
                .into_iter()
                .map(|(c, k)| ((c.x, c.y, c.z), k))
                .collect();
            for (c, chunk) in raw {
                let e = index[&(c.x, c.y, c.z)];
                let pose = crate::IsometryReal::translation(
                    parry3d::math::Real::from((c.x * 32) as f32),
                    parry3d::math::Real::from((c.y * 32) as f32),
                    parry3d::math::Real::from((c.z * 32) as f32),
                );
                self.data.send(crate::GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::AddTerrainChunk(e, pose, Box::new(chunk)),
                ));
            }
        }
        for bundle in payload.content.entities.iter().cloned() {
            self.apply_arrival(bundle);
        }
        self.data.cells.raw_mut().active.insert(payload.cell);
    }

    /// Deactivation: freeze entities, capture terrain, remove it raw.
    /// Caller guarantees no player is in the cell.
    pub fn dematerialize_cell(&mut self, cell: CellCoords, region: RegionCoords) -> DormantCell {
        let entities: Vec<EntityBundle> = self
            .cell_entity_keys(cell)
            .into_iter()
            .map(|k| self.extract_entity_safe(k, region))
            .collect();
        let chunk_keys = self.cell_chunk_keys(cell);
        let mut chunks = Vec::with_capacity(chunk_keys.len());
        for (coords, key) in chunk_keys {
            let chunk = self.data.ecs.chunk.try_get(key).clone().expect("chunk entity");
            chunks.push((coords, chunk));
            if self.data.has_render_sink() {
                self.data.send(crate::GameDataUpdate::new(
                    crate::GameDataTransactionKind::Do,
                    crate::GameDataUpdateKind::RemoveEntity(key),
                ));
            }
            self.remove_chunk_raw(key);
        }
        self.data.cells.raw_mut().active.remove(&cell);
        DormantCell { chunks, entities }
    }

    /// Region-local (x, z) of every player body, by ClientId.
    pub fn player_positions(&self) -> Vec<(f32, f32)> {
        self.data
            .player_entites
            .values()
            .filter_map(|e| {
                let h = (*self.data.ecs.rigidbody.try_get(*e))?;
                let t = self.data.physics.bodies.get(h)?.translation();
                Some((t.x.0, t.z.0))
            })
            .collect()
    }

    /// Cell-mode only: whether block `pos` lies in an active cell.
    /// Legacy regions: always true.
    pub fn block_in_active_cell(&self, pos: [i32; 3]) -> bool {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return true;
        }
        cell_of_block(pos, cc).is_some_and(|c| self.data.cells.active.contains(&c))
    }

    /// End-of-tick backstop: every non-player, non-ghost entity standing in
    /// a dormant cell is extracted (undo-tracked, so clients predict it) and
    /// returned for the server's store. Legacy regions: no-op.
    pub fn freeze_strays_safe(&mut self, region: RegionCoords) -> Vec<(CellCoords, EntityBundle)> {
        let cc = self.data.cells.cell_chunks;
        if cc == 0 {
            return Vec::new();
        }
        let players: BTreeSet<EntityKey> = self.data.player_entites.values().copied().collect();
        let ghosts: BTreeSet<EntityKey> = self.data.ghosts.iter().map(|(_, g)| g.entity).collect();
        let mut strays = Vec::new();
        for (k, kind) in self.data.ecs.kind.iter() {
            if kind.is_none() || players.contains(&k) || ghosts.contains(&k) {
                continue;
            }
            let Some(h) = *self.data.ecs.rigidbody.try_get(k) else { continue };
            let Some(body) = self.data.physics.bodies.get(h) else { continue };
            let t = body.translation();
            // Outside the region is the departure scan's job.
            if let Some(cell) = cell_of_pos(t.x.0, t.z.0, cc) {
                if !self.data.cells.active.contains(&cell) {
                    strays.push((cell, k));
                }
            }
        }
        strays
            .into_iter()
            .map(|(cell, k)| (cell, self.extract_entity_safe(k, region)))
            .collect()
    }

    /// Raw inverse of one `seed_terrain_genesis` chunk: body + attached
    /// colliders, component slots, entity slot.
    fn remove_chunk_raw(&mut self, key: EntityKey) {
        if let Some(handle) = *self.data.ecs.rigidbody.try_get(key) {
            let p = self.data.physics.raw_mut();
            p.bodies.raw_mut().remove(
                handle,
                p.islands.raw_mut(),
                p.colliders.raw_mut(),
                p.implules_joint_set.raw_mut(),
                p.multi_body_joint_set.raw_mut(),
                true,
            );
        }
        self.data.ecs.camera.raw_mut().remove(key);
        self.data.ecs.isometry.raw_mut().remove(key);
        self.data.ecs.rigidbody.raw_mut().remove(key);
        self.data.ecs.chunk.raw_mut().remove(key);
        self.data.ecs.kind.raw_mut().remove(key);
        self.data.ecs.motion.raw_mut().remove(key);
        self.data.ecs.slime.raw_mut().remove(key);
        self.data.ecs.entities.remove_raw(key);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn grid_divides_the_region() {
        assert_eq!(cells_per_side(crate::REGION_CHUNKS), 1);
        assert_eq!(cells_per_side(crate::REGION_CHUNKS / 4), 4);
        assert_eq!(cell_size_units(8), 256.0);
    }

    #[test]
    #[should_panic]
    fn non_dividing_cell_size_panics() {
        cells_per_side(3);
    }

    #[test]
    fn chunk_and_position_map_to_the_same_cell() {
        let c = ChunkCoords::new(9, -2, 17);
        assert_eq!(cell_of_chunk(c, 8), CellCoords::new(1, 2));
        // Chunk (9, _, 17) starts at voxel (288, _, 544).
        assert_eq!(cell_of_pos(288.0, 544.0, 8), Some(CellCoords::new(1, 2)));
    }

    #[test]
    fn a_position_exactly_on_a_cell_edge_belongs_to_the_higher_cell() {
        let edge = cell_size_units(8);
        assert_eq!(cell_of_pos(edge, 0.0, 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_pos(edge - 0.001, 0.0, 8), Some(CellCoords::new(0, 0)));
    }

    #[test]
    fn positions_outside_the_region_have_no_cell() {
        assert_eq!(cell_of_pos(-0.5, 10.0, 8), None);
        assert_eq!(cell_of_pos(crate::REGION_SIZE, 10.0, 8), None);
    }

    #[test]
    fn blocks_map_through_their_chunk() {
        // CHUNK_BLOCKS = 2 blocks per chunk: block 16 is chunk 8 → cell 1 at 8-chunk cells.
        assert_eq!(cell_of_block([16, 5, 0], 8), Some(CellCoords::new(1, 0)));
        assert_eq!(cell_of_block([-1, 5, 0], 8), None);
    }

    #[test]
    fn cells_within_is_clipped_and_sorted() {
        let n = cells_per_side(8);
        let around_corner = cells_within(CellCoords::new(0, 0), 1, 8);
        assert_eq!(
            around_corner,
            vec![CellCoords::new(0, 0), CellCoords::new(0, 1), CellCoords::new(1, 0), CellCoords::new(1, 1)]
        );
        let all = cells_within(CellCoords::new(1, 1), 10, 8);
        assert_eq!(all.len() as u32, n * n);
        assert!(all.windows(2).all(|w| w[0] < w[1]));
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::{Chunk, GameEventKind};

    fn sample() -> CellChanges {
        CellChanges {
            deactivate: vec![CellCoords::new(2, 3)],
            activate: vec![CellPayload {
                cell: CellCoords::new(0, 0),
                version: 4,
                materialized_hash: 0,
                content: DormantCell {
                    chunks: vec![(ChunkCoords::new(1, 0, 1), Chunk::flat_floor(8))],
                    entities: vec![],
                },
            }],
        }
    }

    #[test]
    fn cell_changes_are_a_server_event() {
        assert_eq!(GameEventKind::CellChanges(sample()).origin_client(), None);
    }

    #[test]
    fn cell_changes_round_trip_through_the_wire_codec() {
        let kind = GameEventKind::CellChanges(sample());
        let bytes = bincode::serialize(&kind).unwrap();
        let back: GameEventKind = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn content_crc_tracks_content() {
        let a = sample().activate.remove(0).content;
        let mut b = a.clone();
        assert_eq!(a.content_crc(), b.content_crc());
        b.chunks.clear();
        assert_ne!(a.content_crc(), b.content_crc());
        assert!(CellChanges::default().is_empty());
    }
}

#[cfg(test)]
mod lod_mask_tests {
    use super::*;

    fn active(cells: &[(u32, u32)]) -> BTreeSet<CellCoords> {
        cells.iter().map(|&(x, z)| CellCoords::new(x, z)).collect()
    }

    #[test]
    fn a_lod_cell_inside_an_active_cell_is_masked() {
        let home = RegionCoords::new(0, 0);
        let cell = cell_size_units(8); // 256 units
        // A 64-unit LOD cell in the middle of cell (1,1).
        let m = lod_cell_masked(home, &active(&[(1, 1)]), 8, [cell + 64.0, cell + 64.0], 64.0);
        assert!(m);
        assert!(!lod_cell_masked(home, &active(&[(0, 0)]), 8, [cell + 64.0, cell + 64.0], 64.0));
    }

    #[test]
    fn a_lod_cell_spanning_cells_is_masked_only_when_all_are_active() {
        let home = RegionCoords::new(0, 0);
        let cell = cell_size_units(8);
        // 512 units wide from the origin: covers cells (0,0),(1,0),(0,1),(1,1).
        let rect = [0.0, 0.0];
        assert!(!lod_cell_masked(home, &active(&[(0, 0), (1, 0), (0, 1)]), 8, rect, 2.0 * cell));
        assert!(lod_cell_masked(home, &active(&[(0, 0), (1, 0), (0, 1), (1, 1)]), 8, rect, 2.0 * cell));
    }

    #[test]
    fn lod_cells_of_other_regions_are_never_masked() {
        let home = RegionCoords::new(0, 0);
        let world = RegionCoords::new(1, 0).world_offset();
        assert!(!lod_cell_masked(home, &active(&[(0, 0)]), 8, [world[0], world[2]], 64.0));
    }

    #[test]
    fn home_region_offsets_are_respected() {
        let home = RegionCoords::new(-1, 2);
        let o = home.world_offset();
        assert!(lod_cell_masked(home, &active(&[(0, 0)]), 8, [o[0] + 8.0, o[2] + 8.0], 64.0));
        assert!(!lod_cell_masked(RegionCoords::new(0, 0), &active(&[(0, 0)]), 8, [o[0] + 8.0, o[2] + 8.0], 64.0));
    }
}
