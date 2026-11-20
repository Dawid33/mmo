use std::collections::{BTreeMap, BTreeSet};

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::math::Vec3A;
use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{Task, block_on, futures_lite::future};
use game::Voxel;
pub use game::lod::{LodCell, build_far_lod_cells, downsample2};

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material::StandardVoxelMaterial;
use super::binary::{self, Face, Quad};
use super::emit;
use super::neighbors::gather_padded;
use super::queue::MeshBudgets;
use super::records;
use super::{ChunkMaterial, MeshOutput, MeshWire, TerrainQuadCount};

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
pub(crate) fn lod_quads(grid: &[Voxel], neighbors: &BTreeMap<[i32; 3], &[Voxel]>) -> Vec<Quad> {
    padded_lod_quads(&gather_padded(grid, neighbors))
}

/// [`lod_quads`] for a streamed LOD cell (a key's grid, wired to the other
/// grids of its key): the same mesh without the underside of the terrain's
/// crust (`neighbors::fill_under_crust`).
pub(crate) fn lod_cell_quads(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
) -> Vec<Quad> {
    let mut padded = gather_padded(grid, neighbors);
    super::neighbors::fill_under_crust(&mut padded, neighbors);
    padded_lod_quads(&padded)
}

fn padded_lod_quads(padded: &super::binary::PaddedVoxels) -> Vec<Quad> {
    // LOD stays opaque: coarse cells of leaves are solid foliage.
    let mut quads = binary::mesh_quads(padded, &binary::CutoutTable::NONE);
    for q in &mut quads {
        if q.face.axis() != 1 && is_border_side_quad(q) && !wall_below(padded, q) {
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
    let (lo, len) = if axis == 0 {
        (q.min[2], q.h)
    } else {
        (q.min[0], q.w)
    };
    (lo..lo + len).any(|t| {
        let mut c = [q.min[0] as usize + 1, y, q.min[2] as usize + 1];
        c[if axis == 0 { 2 } else { 0 }] = t as usize + 1;
        let mut o = c;
        o[axis] = (c[axis] as isize + out) as usize;
        p.is_solid(c[0], c[1], c[2]) && !p.is_solid(o[0], o[1], o[2])
    })
}

/// Mesh a coarse LOD grid as it is (no crust fill — `lod_cell_quads` is what
/// streamed cells use). See `lod_quads` for the bias/skirt contract.
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
/// One LOD cell grid to mesh, with its packed neighbours (a compute job,
/// `renderer::compute`): the dense expansions happen in the job.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct LodMeshJob {
    pub packed: game::lod::PackedLodGrid,
    pub neighbors: BTreeMap<[i32; 3], game::lod::PackedLodGrid>,
    pub layers: BlockTextureLayers,
    pub pull: bool,
}

impl super::super::compute::ComputeJob for LodMeshJob {
    type Output = MeshWire;
    const KIND: super::super::compute::JobKind = super::super::compute::JobKind::LodMesh;

    fn run(self) -> MeshWire {
        let grid = decode_lod_grid(&self.packed);
        let neigh: BTreeMap<[i32; 3], Vec<Voxel>> = self
            .neighbors
            .iter()
            .map(|(&off, packed)| (off, decode_lod_grid(packed)))
            .collect();
        let neighbor_refs: BTreeMap<[i32; 3], &[Voxel]> =
            neigh.iter().map(|(&k, v)| (k, v.as_slice())).collect();
        mesh_lod_output(&grid, &neighbor_refs, &self.layers, self.pull)
    }
}

fn mesh_lod_output(
    grid: &[Voxel],
    neighbors: &BTreeMap<[i32; 3], &[Voxel]>,
    layers: &BlockTextureLayers,
    pull: bool,
) -> MeshWire {
    let quads = lod_cell_quads(grid, neighbors);
    let quad_count = u32::try_from(quads.len()).expect("terrain quad count fits u32");
    let (mesh, records) = if pull {
        let r = records::quads_to_records(&quads, layers);
        (None, (!r.is_empty()).then_some(r))
    } else {
        (emit::quads_to_mesh_data(&quads, layers), None)
    };
    MeshWire {
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
/// the async mesh tasks ever need the expansion — so they do it.
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

/// Observable backlog of raster LOD mesh builds. Benchmark readiness must
/// include queued work, not only spawned tasks.
#[derive(Resource, Default)]
pub struct LodBuildQueue {
    raster: BTreeSet<Entity>,
}

impl LodBuildQueue {
    pub fn enqueue_raster(&mut self, entity: Entity) {
        self.raster.insert(entity);
    }

    pub fn remove(&mut self, entity: Entity) {
        self.raster.remove(&entity);
    }

    /// Whether `entity` waits for a raster mesh build.
    pub fn contains_raster(&self, entity: Entity) -> bool {
        self.raster.contains(&entity)
    }

    pub fn pending_count(&self) -> usize {
        self.raster.len()
    }

    pub(crate) fn raster_batch(&self, capacity: usize) -> Vec<Entity> {
        self.raster.iter().copied().take(capacity).collect()
    }

    pub(crate) fn retain_raster(&mut self, mut keep: impl FnMut(Entity) -> bool) {
        self.raster.retain(|entity| keep(*entity));
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

pub(crate) fn far_task_capacity(max_in_flight: usize, lod_in_flight: usize) -> usize {
    max_in_flight.saturating_sub(lod_in_flight)
}

/// Drain changed `LodVoxelData` into mesh tasks, FIFO (no distance ranking —
/// LOD is inherently low-priority background work). Raster LOD owns a bounded
/// far lane of `MeshBudgets.max_in_flight`; the independently bounded
/// full-resolution lane does not consume this capacity.
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
    changed: Query<Entity, Changed<LodVoxelData>>,
    grids: Query<(&LodVoxelData, Option<&LodNeighbors>)>,
    layers: Res<BlockTextureLayers>,
) {
    far_uploads.0 = 0;
    for e in changed.iter() {
        commands.entity(e).remove::<LodRasterReady>();
        queue.enqueue_raster(e);
    }
    queue.retain_raster(|e| grids.contains(e));
    if queue.pending_count() == 0 {
        return;
    }
    let capacity = far_task_capacity(budgets.max_in_flight, lod_in_flight.iter().count());
    if capacity == 0 {
        return;
    }
    // Records on every backend: the pulled path draws them, the per-chunk
    // path merges a root's cells into tiles (`lod_tiles`).
    let pull = true;

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
        let job = LodMeshJob {
            packed: voxels.0.clone(),
            neighbors: packed_neigh,
            layers: layers.clone(),
            pull,
        };
        let task = super::super::compute::spawn(job, MeshWire::into_output);
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
    render_config: Option<Res<super::super::config::TerrainRenderConfig>>,
    grouped: Query<(), With<super::lod_tiles::LodGroup>>,
) {
    // A cell of a LOD root keeps its records for its tile's merged list
    // (`lod_tiles`), on every backend; one with no group draws its own.
    let per_chunk = super::chunk_mesh::per_chunk_backend(render_config.as_deref());
    for (e, mut task) in &mut tasks {
        if uploads.0 >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(output) = block_on(future::poll_once(&mut task.0)) else {
            continue;
        };
        let tiled = grouped.contains(e);
        if output.requires_upload() && !tiled {
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
        if tiled {
            // Ready (`LodRasterReady`) once its tile is drawn.
            ec.insert(super::lod_tiles::LodTileDirty);
            match records {
                Some(records) => {
                    ec.insert((
                        super::chunk_mesh::BrickRecords(std::sync::Arc::new(records)),
                        TerrainQuadCount(quad_count),
                    ));
                }
                None => {
                    ec.remove::<(super::chunk_mesh::BrickRecords, TerrainQuadCount)>();
                }
            }
            continue;
        }
        ec.insert(LodRasterReady);
        let (records, mesh) = match (per_chunk, records) {
            // No tile to join: draws its own records.
            (true, Some(records)) => (
                None,
                super::chunk_mesh::records_to_mesh_data(&records).map(emit::MeshData::into_mesh),
            ),
            (_, records) => (records, mesh),
        };
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

    use game::{BRICK_VOXEL_COUNT, BlockId};

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

    /// Regression: grass/dirt flicker along LOD edges, live-confirmed by the user 2026-09-25 (memory cell-lod-flicker-resolved, 3acd148b)
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
        assert!(
            overlaps.is_empty(),
            "coplanar overlapping side quads: {overlaps:?}"
        );
        // The floor seam is still covered: border walls reach biased y=0.
        for face in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert!(
                quads.iter().any(|q| q.face == face && q.min[1] == 0),
                "{face:?}: the bottom wall keeps its skirt to y=0"
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

    use super::super::queue::MeshBudgets;
    use super::super::queue::tests::{pipeline_app, wait_until};

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

    /// Regression: LOD strips, seen live 2026-09-19 (64 of ~480 cells per root; c9507621, memory lod-ring-strip-bug)
    /// Regression (2026-09-19, live): a LOD root arrives as hundreds of cells
    /// in one frame. The raster lane meshes `max_in_flight` of them per batch
    /// and must keep the rest queued — a `queue.remove()` on every changed
    /// cell once left only the first batch of each root meshed, so
    /// neighbouring regions rendered as a thin strip.
    #[test]
    fn raster_mode_meshes_every_cell_of_a_root_larger_than_the_task_budget() {
        let mut app = pipeline_app(MeshBudgets {
            max_in_flight: 8,
            uploads_per_frame: 8,
        });
        let budget = app.world().resource::<MeshBudgets>().max_in_flight;
        let cells: Vec<Entity> = (0..budget * 3)
            .map(|i| {
                spawn_lod_chunk(
                    &mut app,
                    Vec3::ZERO,
                    Vec3::X * 64.0 * i as f32,
                    2,
                    floor_grid(),
                )
            })
            .collect();

        let all_ready = |app: &mut App| {
            cells
                .iter()
                .all(|&cell| app.world().entity(cell).contains::<LodRasterReady>())
        };
        assert!(
            wait_until(&mut app, all_ready),
            "only {} of {} cells were ever meshed",
            cells
                .iter()
                .filter(|&&cell| app.world().entity(cell).contains::<LodRasterReady>())
                .count(),
            cells.len()
        );
        assert_eq!(app.world().resource::<LodBuildQueue>().pending_count(), 0);
    }
}
