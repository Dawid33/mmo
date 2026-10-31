use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
pub use game::lod::{build_far_lod_cells, downsample2, LodCell};
use game::Voxel;

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material::StandardVoxelMaterial;
use super::binary::{self, Face, Quad};
use super::emit;
use super::neighbors::gather_padded;
#[cfg(test)]
use super::pack;
#[cfg(test)]
use super::queue::FrameUploads;
use super::queue::MeshBudgets;
use super::records;
#[cfg(test)]
use super::MeshingTask;
use super::{ChunkMaterial, MeshOutput, TerrainQuadCount};

/// Does `q`'s plane sit on the 32-grid's outer edge along its own face axis?
/// Only X/Z side faces are ever skirted — Y (top/bottom) faces never are.
fn is_border_side_quad(q: &Quad) -> bool {
    match q.face {
        Face::NegX => q.min[0] == 0,
        Face::PosX => q.min[0] == 31, // positive face plane sits at min+1 == 32
        Face::NegZ => q.min[2] == 0,
        Face::PosZ => q.min[2] == 31,
        _ => false,
    }
}

/// Extend a border side-quad one cell downward: undo one unit of the
/// caller's +1 Y-bias (`min[1] -= 1`) and grow whichever extent maps to Y for
/// this face's axis. Per `cross_axes` (binary.rs): X faces have (U=Y, V=Z),
/// so Y rides `w`; Z faces have (U=X, V=Y), so Y rides `h`. Y faces (axis 1)
/// are never skirted — callers must not pass one.
fn skirt_extend(q: &mut Quad) {
    debug_assert_ne!(q.face.axis(), 1, "Y faces are never skirted");
    q.min[1] -= 1;
    match q.face.axis() {
        0 => q.w += 1,
        2 => q.h += 1,
        _ => unreachable!("skirt_extend called on a non-side face"),
    }
}

/// Gather padding from same-scale neighbors, greedy mesh, bias every quad up
/// by one cell (`min[1] += 1`), then skirt border side-quads one cell back
/// down to the true floor so adjacent LOD rings (or ring boundaries at
/// different scales) never show a seam gap at y=0. Resulting quads span Y
/// range 0..=33 — callers translate `Transform.y` down by the scale factor
/// to compensate (Task 3). Shared by `mesh_lod_grid` and `mesh_lod_output`.
///
/// A skirt is skipped where the row just below the quad already shows a wall
/// on the same plane (e.g. grass over dirt: two quads, since greedy meshing
/// never merges different blocks) — the skirt would lie coplanar on it and
/// z-fight. A seam can only open where that row is NOT an exposed wall.
fn lod_quads(grid: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> Vec<Quad> {
    let padded = gather_padded(grid, neighbors);
    // LOD stays opaque: coarse cells of leaves are solid foliage.
    let mut quads = binary::mesh_quads(&padded, &binary::CutoutTable::NONE);
    for q in &mut quads {
        if q.face.axis() != 1 && is_border_side_quad(q) && !wall_below(&padded, q) {
            q.min[1] += 1;
            skirt_extend(q);
        } else {
            q.min[1] += 1;
        }
    }
    quads
}

/// Does any cell in the row just below side quad `q` (unbiased grid coords)
/// show a face on `q`'s plane, facing `q`'s way? Row -1 reads the padding
/// (the neighbour below, air when not wired).
fn wall_below(p: &super::binary::PaddedVoxels, q: &Quad) -> bool {
    let axis = q.face.axis();
    let out: isize = match q.face {
        Face::PosX | Face::PosZ => 1,
        _ => -1,
    };
    // Padded coords: grid + 1. The row below the quad's bottom is min.y - 1.
    let y = q.min[1] as usize; // (min.y - 1) + 1
    let (lo, len) = if axis == 0 { (q.min[2], q.h) } else { (q.min[0], q.w) };
    (lo..lo + len).any(|t| {
        let mut c = [q.min[0] as usize + 1, y, q.min[2] as usize + 1];
        c[if axis == 0 { 2 } else { 0 }] = t as usize + 1;
        let mut o = c;
        o[axis] = (c[axis] as isize + out) as usize;
        p.is_solid(c[0], c[1], c[2]) && !p.is_solid(o[0], o[1], o[2])
    })
}

/// Mesh a coarse LOD grid. See `lod_quads` for the bias/skirt contract.
pub fn mesh_lod_grid(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
) -> Option<Mesh> {
    emit::quads_to_mesh(&lod_quads(grid, neighbors), layers)
}

/// Mesh OR records (per `pull`) for a coarse LOD grid — the pull-aware
/// counterpart to `mesh_lod_grid`, used by `spawn_lod_mesh_tasks`.
/// `connectivity` is always 0: LOD chunks never carry a connectivity mask
/// (the field only exists so `MeshOutput` can be shared with full-res).
fn mesh_lod_output(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
    pull: bool,
) -> MeshOutput {
    let quads = lod_quads(grid, neighbors);
    let quad_count = u32::try_from(quads.len()).expect("terrain quad count fits u32");
    let (mesh, records) = if pull {
        let r = records::quads_to_records(&quads, layers);
        (None, (!r.is_empty()).then_some(r))
    } else {
        (emit::quads_to_mesh(&quads, layers), None)
    };
    MeshOutput {
        mesh,
        records,
        connectivity: 0,
        quad_count,
        has_interior: false,
    }
}

/// Scale factor of this LOD chunk relative to a full-res chunk (2, 4, 8, ...).
/// Distinguishes coarse LOD render entities from full-res chunks; LOD chunks
/// never carry `ChunkKey` and never enter the world-keyed `ChunkGrid`.
#[derive(Component, Copy, Clone)]
pub struct LodChunk {
    pub factor: u32,
}

/// One LOD chunk cell's coarse voxel grid, kept PACKED (the RLE it arrived
/// in). Same layout contract as full-res `VoxelData` once decoded
/// (`x + 32*y + 32*32*z`), and the same reasoning as `bridge::VoxelData`: a
/// dense grid is 64 KiB, the production ring table holds ~20 000 cells
/// (~1.3 GB, over half the client's RSS when measured 2026-09-20), and only
/// the async mesh / brick tasks ever need the expansion — so they do it.
#[derive(Component)]
pub struct LodVoxelData(pub game::lod::PackedLodGrid);

impl LodVoxelData {
    pub fn from_dense(grid: &[Voxel]) -> Self {
        Self(game::lod::PackedLodGrid::encode(grid))
    }

    /// The dense 32^3 grid. Payloads are validated when spawned
    /// (`PackedLodGrid::is_well_formed`), so a malformed run list cannot get
    /// here in practice; if one did, it reads as air instead of panicking
    /// inside a task.
    pub fn decode(&self) -> Vec<Voxel> {
        decode_lod_grid(&self.0)
    }
}

/// See [`LodVoxelData::decode`]; free function so tasks can decode the packed
/// grids they were handed without borrowing the component.
pub fn decode_lod_grid(packed: &game::lod::PackedLodGrid) -> Vec<Voxel> {
    packed
        .decode()
        .unwrap_or_else(|| vec![Voxel::default(); game::BRICK_VOXEL_COUNT])
}

/// Optional same-batch neighbor refs for padding gather, set by the spawner.
/// Entities named here must be LOD siblings at the same `factor` — offsets
/// follow the same `[-1,0,1]³` face-direction convention as `gather_padded`.
#[derive(Component, Default)]
pub struct LodNeighbors(pub BTreeMap<[i32; 3], Entity>);

/// In-flight background LOD mesh build, spawned by `spawn_lod_mesh_tasks` and
/// polled to completion by `apply_lod_meshes`.
#[derive(Component)]
pub struct LodMeshTask(Task<MeshOutput>);

/// Observable backlog shared by raster LOD meshing and native brick builds.
/// Benchmark readiness must include queued work, not only spawned tasks.
#[derive(Resource, Default)]
pub struct LodBuildQueue {
    raster: BTreeSet<Entity>,
    brick: BTreeSet<Entity>,
}

impl LodBuildQueue {
    pub fn enqueue_raster(&mut self, entity: Entity) {
        self.raster.insert(entity);
        self.brick.remove(&entity);
    }

    pub fn enqueue_brick(&mut self, entity: Entity) {
        self.brick.insert(entity);
        self.raster.remove(&entity);
    }

    pub fn remove(&mut self, entity: Entity) {
        self.raster.remove(&entity);
        self.brick.remove(&entity);
    }

    pub fn pending_count(&self) -> usize {
        self.raster.len() + self.brick.len()
    }

    pub fn raster_pending_count(&self) -> usize {
        self.raster.len()
    }

    pub fn brick_pending_count(&self) -> usize {
        self.brick.len()
    }

    pub(crate) fn raster_batch(&self, capacity: usize) -> Vec<Entity> {
        self.raster.iter().copied().take(capacity).collect()
    }

    pub(crate) fn brick_batch(&self, capacity: usize) -> Vec<Entity> {
        self.brick.iter().copied().take(capacity).collect()
    }

    pub(crate) fn brick_entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.brick.iter().copied()
    }

    pub(crate) fn retain_raster(&mut self, mut keep: impl FnMut(Entity) -> bool) {
        self.raster.retain(|entity| keep(*entity));
    }

    pub(crate) fn retain_brick(&mut self, mut keep: impl FnMut(Entity) -> bool) {
        self.brick.retain(|entity| keep(*entity));
    }
}

/// Raster build completion, including all-air grids which intentionally have
/// no `Mesh3d` or `ChunkQuads` component.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct LodRasterReady;

/// Far/raster GPU uploads attached this frame. This is intentionally separate
/// from near `FrameUploads`: each lane is bounded by the same configured cap,
/// while neither can starve the other.
#[derive(Resource, Default)]
pub struct FarFrameUploads(pub usize);

pub(crate) fn far_task_capacity(
    max_in_flight: usize,
    lod_in_flight: usize,
    brick_in_flight: usize,
) -> usize {
    max_in_flight.saturating_sub(lod_in_flight + brick_in_flight)
}

/// Drain changed `LodVoxelData` into mesh tasks, FIFO (no distance ranking —
/// LOD is inherently low-priority background work). Raster LOD and brick
/// preparation share a bounded far lane of `MeshBudgets.max_in_flight`; the
/// independently bounded full-resolution lane does not consume this capacity.
///
/// The `Local` queue (not a bare `Changed<LodVoxelData>` filter alone) matters:
/// Bevy consumes a system's change-detection window every time it runs, so an
/// entity skipped this frame by the budget cap would silently drop out of
/// `Changed<>` on the next run if we didn't enqueue every changed entity,
/// unconditionally, before applying the cap.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn spawn_lod_mesh_tasks(
    mut commands: Commands,
    mut queue: ResMut<LodBuildQueue>,
    budgets: Res<MeshBudgets>,
    mut far_uploads: ResMut<FarFrameUploads>,
    lod_in_flight: Query<(), With<LodMeshTask>>,
    brick_in_flight: Query<(), With<super::super::far_voxel::FarBrickBuildTask>>,
    changed: Query<Entity, Changed<LodVoxelData>>,
    grids: Query<(&LodVoxelData, Option<&LodNeighbors>)>,
    raster_fallbacks: Query<(), With<super::super::far_voxel::FarRasterFallback>>,
    new_raster_fallbacks: Query<Entity, Added<super::super::far_voxel::FarRasterFallback>>,
    layers: Res<BlockTextureLayers>,
    render_config: Option<Res<super::super::config::TerrainRenderConfig>>,
) {
    far_uploads.0 = 0;
    let brick_mode = render_config
        .as_deref()
        .is_some_and(|config| config.far == super::super::config::FarRenderBackend::BrickRaymarch);
    for e in changed.iter() {
        commands.entity(e).remove::<LodRasterReady>();
        if brick_mode && !raster_fallbacks.contains(e) {
            queue.enqueue_brick(e);
        } else {
            queue.enqueue_raster(e);
        }
    }
    for e in new_raster_fallbacks.iter() {
        queue.enqueue_raster(e);
    }
    queue.retain_raster(|e| grids.contains(e) && (!brick_mode || raster_fallbacks.contains(e)));
    if queue.raster_pending_count() == 0 {
        return;
    }
    let capacity = far_task_capacity(
        budgets.max_in_flight,
        lod_in_flight.iter().count(),
        brick_in_flight.iter().count(),
    );
    if capacity == 0 {
        return;
    }
    let pull = render_config
        .as_deref()
        .is_none_or(|config| config.near.uses_quad_records());

    let pool = AsyncComputeTaskPool::get();
    let batch = queue.raster_batch(capacity);
    for e in batch {
        queue.remove(e);
        let Ok((voxels, lod_neighbors)) = grids.get(e) else {
            continue;
        };
        // Packed clones only (a few hundred bytes each); the 64 KiB dense
        // expansions of this cell and its up-to-26 neighbors happen in the
        // task, off the main thread.
        let mut packed_neigh: BTreeMap<[i32; 3], game::lod::PackedLodGrid> = BTreeMap::new();
        if let Some(ln) = lod_neighbors {
            for (&off, &ne) in &ln.0 {
                if let Ok((nv, _)) = grids.get(ne) {
                    packed_neigh.insert(off, nv.0.clone());
                }
            }
        }
        let packed = voxels.0.clone();
        let layers = layers.0.clone();
        let task = pool.spawn(async move {
            let grid = decode_lod_grid(&packed);
            let neigh: BTreeMap<[i32; 3], Vec<Voxel>> = packed_neigh
                .iter()
                .map(|(&off, packed)| (off, decode_lod_grid(packed)))
                .collect();
            let neighbor_refs: BTreeMap<[i32; 3], &[Voxel]> =
                neigh.iter().map(|(&k, v)| (k, v.as_slice())).collect();
            mesh_lod_output(&grid, &neighbor_refs, &BlockTextureLayers(layers), pull)
        });
        commands.entity(e).insert(LodMeshTask(task));
    }
}

/// Poll in-flight `LodMeshTask`s to completion and attach the resulting
/// `Mesh3d`, up to the independent far upload budget (`FarFrameUploads`).
/// Near mesh attachment uses its own equally bounded counter and cannot consume
/// this lane. Inserts the shared `ChunkMaterial`, a biased `Aabb` covering the
/// mesh's `0..=33` Y range, and `NoAutoAabb` — no `ChunkKey`, no
/// `ChunkConnectivity`, no grid entry: LOD chunks render but stay outside the
/// world-keyed grid and the cave-culling BFS entirely.
pub fn apply_lod_meshes(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut LodMeshTask)>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    budgets: Res<MeshBudgets>,
    mut uploads: ResMut<FarFrameUploads>,
) {
    for (e, mut task) in &mut tasks {
        if uploads.0 >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(output) = block_on(future::poll_once(&mut task.0)) else {
            continue;
        };
        if output.requires_upload() {
            uploads.0 += 1;
        }
        let MeshOutput {
            mesh,
            records,
            quad_count,
            ..
        } = output;
        let mut ec = commands.entity(e);
        ec.remove::<LodMeshTask>();
        ec.insert(LodRasterReady);
        if let Some(records) = records {
            ec.insert((
                super::super::voxel_pull::ChunkQuads(std::sync::Arc::new(records)),
                // Skirted LOD meshes span y = 0..=33 (see `lod_quads`'s doc
                // comment): biased up one cell from the standard 0..32 cube
                // plus the +1 unbias headroom, so the Aabb is offset and
                // grown by half a cell on Y relative to the full-res chunk's
                // constant Aabb.
                Aabb {
                    center: Vec3A::new(16.0, 16.5, 16.0),
                    half_extents: Vec3A::new(16.0, 16.5, 16.0),
                },
                NoAutoAabb,
                TerrainQuadCount(quad_count),
            ));
            continue;
        }
        match mesh {
            Some(mesh) => {
                ec.insert((
                    Mesh3d(meshes.add(mesh)),
                    // See the pull-path Aabb comment above for the y-bias rationale.
                    Aabb {
                        center: Vec3A::new(16.0, 16.5, 16.0),
                        half_extents: Vec3A::new(16.0, 16.5, 16.0),
                    },
                    NoAutoAabb,
                    TerrainQuadCount(quad_count),
                ));
                if let Some(material) = &material {
                    ec.insert(MeshMaterial3d(material.0.clone()));
                }
            }
            None => {
                ec.remove::<(
                    Mesh3d,
                    MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                    super::super::voxel_pull::ChunkQuads,
                    TerrainQuadCount,
                )>();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::{BlockId, BRICK_VOXEL_COUNT};

    /// Two side quads on the same plane, facing the same way, whose rects
    /// overlap: coplanar duplicates that z-fight (flicker) under any camera
    /// jitter.
    fn overlapping_side_quads(quads: &[Quad]) -> Vec<(Quad, Quad)> {
        let rect = |q: &Quad| {
            let (u, v) = binary::cross_axes(q.face.axis());
            (q.min[u], q.min[u] + q.w, q.min[v], q.min[v] + q.h)
        };
        let mut out = Vec::new();
        for (i, a) in quads.iter().enumerate() {
            for b in &quads[i + 1..] {
                let axis = a.face.axis();
                if axis == 1 || a.face != b.face || a.min[axis] != b.min[axis] {
                    continue;
                }
                let (a0, a1, a2, a3) = rect(a);
                let (b0, b1, b2, b3) = rect(b);
                if a0 < b1 && b0 < a1 && a2 < b3 && b2 < a3 {
                    out.push((a.clone(), b.clone()));
                }
            }
        }
        out
    }

    /// A border wall of grass over dirt meshes as two quads on one plane
    /// (greedy meshing never merges different blocks). Skirting the upper one
    /// down a row laid it over the lower one's top row: same plane, same
    /// facing, different texture — the grass/dirt flicker along LOD cell
    /// edges, flipped by a ~0.002-unit camera wobble.
    #[test]
    fn skirts_never_overlap_a_coplanar_wall_below() {
        let mut grid = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for (i, v) in grid.iter_mut().enumerate() {
            let y = (i / 32) % 32;
            if y <= 10 {
                *v = Voxel::new(BlockId(if y < 10 { 1 } else { 2 }));
            }
        }
        let quads = lod_quads(&grid, &BTreeMap::new());
        let overlaps = overlapping_side_quads(&quads);
        assert!(overlaps.is_empty(), "coplanar overlapping side quads: {overlaps:?}");
        // The floor seam is still covered: border walls reach biased y=0.
        for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert!(
                quads.iter().any(|q| q.face == face && q.min[1] == 0),
                "{face:?}: the bottom wall keeps its skirt to y=0"
            );
        }
    }

    fn base_quad(face: Face) -> Quad {
        Quad {
            min: [10, 20, 30],
            w: 4,
            h: 7,
            face,
            block: BlockId(1),
            ao: [3; 4],
            cutout: false,
            interior: false,
        }
    }

    /// LOD cells stay packed in the ECS: a dense grid is 64 KiB per cell, and
    /// the production ring table holds ~20 000 cells (~1.3 GB, more than half
    /// the client's RSS when this was measured).
    #[test]
    fn lod_voxel_data_is_packed_and_round_trips_exactly() {
        let mut grid = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for (i, v) in grid.iter_mut().enumerate() {
            let y = (i / 32) % 32;
            if y < 12 {
                *v = Voxel::new(BlockId(if y < 10 { 1 } else { 3 }));
            }
        }
        let data = LodVoxelData::from_dense(&grid);
        assert_eq!(data.decode(), grid, "packed storage must decode to the exact grid");
        let bytes = data.0 .0.len() * std::mem::size_of::<(u16, BlockId)>();
        assert!(bytes < 2048, "a layered terrain cell packs to {bytes} B, not 64 KiB");
        // A malformed run list (only possible if validation was skipped) decodes
        // to air rather than panicking inside a mesh task.
        let broken = LodVoxelData(game::lod::PackedLodGrid(vec![(7, BlockId(1))]));
        assert_eq!(broken.decode(), vec![Voxel::default(); BRICK_VOXEL_COUNT]);
    }

    #[test]
    fn skirt_extend_grows_the_y_mapped_extent_per_face() {
        // X faces: cross_axes(0) = (U=1/Y, V=2/Z) — Y rides `w`.
        for face in [Face::NegX, Face::PosX] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!(
                (q.w, q.h),
                (5, 7),
                "{face:?}: w (Y-mapped) grows, h untouched"
            );
        }
        // Z faces: cross_axes(2) = (U=0/X, V=1/Y) — Y rides `h`.
        for face in [Face::NegZ, Face::PosZ] {
            let mut q = base_quad(face);
            skirt_extend(&mut q);
            assert_eq!(q.min, [10, 19, 30], "{face:?}: min[1] decrements by one");
            assert_eq!(
                (q.w, q.h),
                (4, 8),
                "{face:?}: h (Y-mapped) grows, w untouched"
            );
        }
    }

    #[test]
    fn skirt_extend_leaves_other_fields_untouched() {
        let mut q = base_quad(Face::PosX);
        skirt_extend(&mut q);
        assert_eq!(q.min[0], 10, "non-Y min components untouched");
        assert_eq!(q.min[2], 30, "non-Y min components untouched");
        assert_eq!(q.block, BlockId(1));
        assert_eq!(q.ao, [3; 4]);
    }

    #[test]
    #[should_panic]
    fn skirt_extend_rejects_y_faces_in_debug() {
        // Y faces are never side-skirted; calling this on one is a caller bug.
        let mut q = base_quad(Face::PosY);
        skirt_extend(&mut q);
    }

    #[test]
    fn is_border_side_quad_matches_the_four_side_faces_at_the_grid_edge() {
        // Border cases.
        assert!(is_border_side_quad(&Quad {
            min: [0, 5, 5],
            ..base_quad(Face::NegX)
        }));
        assert!(is_border_side_quad(&Quad {
            min: [31, 5, 5],
            ..base_quad(Face::PosX)
        }));
        assert!(is_border_side_quad(&Quad {
            min: [5, 5, 0],
            ..base_quad(Face::NegZ)
        }));
        assert!(is_border_side_quad(&Quad {
            min: [5, 5, 31],
            ..base_quad(Face::PosZ)
        }));
        // Interior (one cell in from the edge): not a border quad.
        assert!(!is_border_side_quad(&Quad {
            min: [1, 5, 5],
            ..base_quad(Face::NegX)
        }));
        assert!(!is_border_side_quad(&Quad {
            min: [30, 5, 5],
            ..base_quad(Face::PosX)
        }));
        assert!(!is_border_side_quad(&Quad {
            min: [5, 5, 1],
            ..base_quad(Face::NegZ)
        }));
        assert!(!is_border_side_quad(&Quad {
            min: [5, 5, 30],
            ..base_quad(Face::PosZ)
        }));
        // Y faces are never border side-quads, even sitting on the top/bottom plane.
        assert!(!is_border_side_quad(&Quad {
            min: [5, 0, 5],
            ..base_quad(Face::NegY)
        }));
        assert!(!is_border_side_quad(&Quad {
            min: [5, 31, 5],
            ..base_quad(Face::PosY)
        }));
    }

    fn packed_of(mesh: &Mesh) -> Vec<pack::UnpackedVertex> {
        use bevy::mesh::VertexAttributeValues;
        let VertexAttributeValues::Uint32x2(packed) = mesh
            .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
            .unwrap()
        else {
            panic!("packed attribute is Uint32x2")
        };
        packed.iter().map(|&p| pack::unpack(p)).collect()
    }

    #[test]
    fn skirted_mesh_shifts_up_and_extends_border_side_faces_down() {
        // Full-solid 32^3 grid, no neighbors (air shell): every outer face is
        // visible and each of the 6 faces merges to one quad.
        let voxels = vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT];
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("solid grid meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts
                .iter()
                .filter(|v| v.face == face)
                .map(|v| v.pos[1])
                .collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(
                ys.iter().copied().min(),
                Some(0),
                "{face:?}: skirt reaches y=0"
            );
            assert_eq!(
                ys.iter().copied().max(),
                Some(33),
                "{face:?}: biased top reaches y=33"
            );
        }
        let top_ys: Vec<u32> = verts
            .iter()
            .filter(|v| v.face == Face::PosY)
            .map(|v| v.pos[1])
            .collect();
        assert!(!top_ys.is_empty());
        assert!(
            top_ys.iter().all(|&y| y == 33),
            "top face sits at biased y=33, not skirted"
        );

        let bottom_ys: Vec<u32> = verts
            .iter()
            .filter(|v| v.face == Face::NegY)
            .map(|v| v.pos[1])
            .collect();
        assert!(!bottom_ys.is_empty());
        assert!(
            bottom_ys.iter().all(|&y| y == 1),
            "bottom face sits at biased y=1, not skirted"
        );
    }

    #[test]
    fn interior_side_faces_are_not_skirted() {
        // A lone solid cell away from every grid edge: its side faces must
        // stay at the biased unit span, with no skirt reaching y=0.
        let mut voxels = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
        let mesh = mesh_lod_grid(&voxels, &neighbors, &BlockTextureLayers::default())
            .expect("lone voxel meshes");
        let verts = packed_of(&mesh);

        for face in [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ] {
            let ys: Vec<u32> = verts
                .iter()
                .filter(|v| v.face == face)
                .map(|v| v.pos[1])
                .collect();
            assert!(!ys.is_empty(), "{face:?} must be present");
            assert_eq!(
                ys.iter().copied().min(),
                Some(6),
                "{face:?}: biased, unextended"
            );
            assert_eq!(
                ys.iter().copied().max(),
                Some(7),
                "{face:?}: biased, unextended"
            );
            assert!(
                ys.iter().all(|&y| y != 0),
                "{face:?}: no skirt geometry at y=0"
            );
        }
    }

    // --- Entity plumbing: components, queue, apply (Task 3) ---------------
    //
    // These run the real production chain (`pipeline_app`, which now also
    // registers the two LOD systems ahead of `cull_chunks`) against a real
    // LOD chunk entity, per the spawner contract: children of a region root,
    // `Transform { translation: local_origin - Y*factor, scale: splat(factor) }`
    // plus `(LodChunk, LodVoxelData, Visibility::default())`.

    use super::super::queue::tests::{pipeline_app, spawn_chunk, voxels_with, wait_until};
    use super::super::queue::{ChunkGrid, ChunkKey, MeshBudgets};
    use super::super::visibility::ChunkConnectivity;

    /// A solid floor at y=0 across the whole 32³ grid: enough content for
    /// `mesh_lod_grid` to produce a mesh, simple enough to reason about.
    fn floor_grid() -> Vec<Voxel> {
        let mut v = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        for z in 0..32 {
            for x in 0..32 {
                v[x + 32 * 32 * z] = Voxel::new(BlockId(1));
            }
        }
        v
    }

    /// Spawns a region root plus one LOD chunk child, exactly per the
    /// spawner contract Tasks 4-5 will follow.
    fn spawn_lod_chunk(
        app: &mut App,
        root_at: Vec3,
        local_origin: Vec3,
        factor: u32,
        grid: Vec<Voxel>,
    ) -> Entity {
        let root = app
            .world_mut()
            .spawn((Transform::from_translation(root_at), Visibility::default()))
            .id();
        app.world_mut()
            .spawn((
                Transform {
                    translation: local_origin - Vec3::Y * factor as f32,
                    scale: Vec3::splat(factor as f32),
                    ..default()
                },
                Visibility::default(),
                LodChunk { factor },
                LodVoxelData::from_dense(&grid),
                ChildOf(root),
            ))
            .id()
    }

    #[test]
    fn lod_chunks_mesh_and_scale_without_entering_the_grid() {
        let mut app = pipeline_app(MeshBudgets::default());

        // A camera plus a keyed full-res wall/behind pair so cull_chunks' BFS
        // has something to actively hide. Without this, an empty ChunkGrid and
        // no active camera make cull_chunks early-return (visibility.rs:166,
        // :169) before ever reaching the loop that would leave a keyless
        // entity's Visibility untouched — the Inherited assertion below would
        // then be vacuously true regardless of whether the BFS logic itself
        // respects keylessness. Positioning the wall/behind pair confirms the
        // BFS genuinely ran and culled something in this same update.
        app.world_mut().spawn((
            Camera3d::default(),
            Camera {
                is_active: true,
                ..Default::default()
            },
            GlobalTransform::from(Transform::from_translation(Vec3::new(8.0, 8.0, 8.0))),
        ));
        let _wall = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(32.0, 0.0, 0.0),
            vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT],
        );
        let behind = spawn_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(64.0, 0.0, 0.0),
            voxels_with(&[[5, 5, 5]]),
        );

        let e = spawn_lod_chunk(
            &mut app,
            Vec3::new(1000.0, 0.0, 0.0),
            Vec3::ZERO,
            2,
            floor_grid(),
        );

        assert!(
            wait_until(&mut app, |app| {
                app.world().entity(e).contains::<Mesh3d>()
                    && app
                        .world()
                        .entity(behind)
                        .get::<Visibility>()
                        .is_some_and(|v| *v == Visibility::Hidden)
            }),
            "LOD chunk must mesh AND the BFS must actually hide the chunk behind the full-res wall \
             (proof the cull pass really ran, not just that it was registered)"
        );

        let tf = *app
            .world()
            .entity(e)
            .get::<Transform>()
            .expect("has a transform");
        assert_eq!(
            tf.translation,
            Vec3::new(0.0, -2.0, 0.0),
            "translation biased down by factor"
        );
        assert_eq!(tf.scale, Vec3::splat(2.0), "scaled by factor");

        let aabb = app
            .world()
            .entity(e)
            .get::<Aabb>()
            .expect("biased Aabb attached");
        assert_eq!(aabb.center, Vec3A::new(16.0, 16.5, 16.0));
        assert_eq!(aabb.half_extents, Vec3A::new(16.0, 16.5, 16.0));
        assert!(app.world().entity(e).contains::<NoAutoAabb>());

        // Keyless assertions: the heart of this scenario. The grid is no
        // longer empty overall (the wall/behind full-res chunks key into it,
        // which is what gives the BFS something to cull) — the LOD chunk
        // specifically must never be one of its values.
        assert!(
            app.world()
                .resource::<ChunkGrid>()
                .0
                .values()
                .all(|&v| v != e),
            "LOD chunks never enter the world-keyed ChunkGrid"
        );
        assert!(
            !app.world().entity(e).contains::<ChunkKey>(),
            "LOD chunks stay keyless"
        );
        assert!(
            !app.world().entity(e).contains::<ChunkConnectivity>(),
            "LOD chunks carry no connectivity mask"
        );
        assert_eq!(
            *app.world().entity(e).get::<Visibility>().unwrap(),
            Visibility::Inherited,
            "cull_chunks' With<ChunkKey> filter never touches a keyless entity, \
             even while its BFS actively hides other chunks this same pass"
        );
    }

    #[test]
    fn saturated_full_res_lane_does_not_starve_raster_lod_lane() {
        // uploads_per_frame: 0 keeps the full-res MeshingTask attached
        // indefinitely (apply_meshed_chunks never gets to poll it away), so
        // the near lane's single slot stays occupied across every subsequent
        // update. Raster LOD owns a separate far lane with the same bound.
        let mut app = pipeline_app(MeshBudgets {
            max_in_flight: 1,
            uploads_per_frame: 0,
        });
        let full_res = spawn_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, voxels_with(&[[5, 5, 5]]));
        let lod = spawn_lod_chunk(
            &mut app,
            Vec3::ZERO,
            Vec3::new(320.0, 0.0, 0.0),
            2,
            floor_grid(),
        );

        for _ in 0..3 {
            app.update();
        }

        assert!(
            app.world().entity(full_res).contains::<MeshingTask>(),
            "full-res task holds the only slot (uploads_per_frame=0 keeps it attached)"
        );
        assert!(
            app.world().entity(lod).contains::<LodMeshTask>(),
            "raster LOD claims its far-lane slot while the near lane is saturated"
        );
    }

    #[test]
    fn near_upload_counter_does_not_consume_far_upload_lane() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .insert_resource(MeshBudgets {
                max_in_flight: 1,
                uploads_per_frame: 1,
            })
            .insert_resource(FrameUploads(1))
            .init_resource::<FarFrameUploads>()
            .add_systems(Update, apply_lod_meshes);
        let task = AsyncComputeTaskPool::get().spawn(async {
            MeshOutput {
                mesh: None,
                records: None,
                connectivity: 0,
                quad_count: 0,
                has_interior: false,
            }
        });
        let lod = app.world_mut().spawn(LodMeshTask(task)).id();

        assert!(super::super::queue::tests::wait_until(&mut app, |app| {
            app.world().entity(lod).contains::<LodRasterReady>()
        }));
    }

    #[test]
    fn raster_and_brick_tasks_share_one_bounded_far_lane() {
        assert_eq!(far_task_capacity(64, 0, 0), 64);
        assert_eq!(far_task_capacity(64, 40, 10), 14);
        assert_eq!(far_task_capacity(64, 64, 64), 0);
    }

    #[test]
    fn changed_lod_voxel_data_supersedes_an_in_flight_task() {
        let mut app = pipeline_app(MeshBudgets::default());
        let e = spawn_lod_chunk(&mut app, Vec3::ZERO, Vec3::ZERO, 2, floor_grid());
        assert!(
            wait_until(&mut app, |app| app.world().entity(e).contains::<Mesh3d>()),
            "initial LOD mesh must land"
        );
        let old_mesh = app
            .world()
            .entity(e)
            .get::<Mesh3d>()
            .expect("meshed")
            .0
            .clone();

        let mut changed_grid = floor_grid();
        changed_grid[5 + 32 * 20 + 32 * 32 * 5] = Voxel::new(BlockId(2));
        *app.world_mut().get_mut::<LodVoxelData>(e).unwrap() = LodVoxelData::from_dense(&changed_grid);

        assert!(
            wait_until(&mut app, |app| {
                app.world()
                    .entity(e)
                    .get::<Mesh3d>()
                    .is_some_and(|m| m.0 != old_mesh)
            }),
            "changed LodVoxelData must supersede the in-flight task and produce a new mesh handle"
        );
    }

    #[test]
    fn lod_build_queue_reports_raster_and_brick_backlogs() {
        let raster = Entity::from_bits(101);
        let brick = Entity::from_bits(102);
        let mut queue = LodBuildQueue::default();

        queue.enqueue_raster(raster);
        queue.enqueue_brick(brick);
        assert_eq!(queue.pending_count(), 2);
        assert_eq!(queue.raster_pending_count(), 1);
        assert_eq!(queue.brick_pending_count(), 1);

        queue.remove(raster);
        assert_eq!(queue.pending_count(), 1);
    }
}
