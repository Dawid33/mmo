use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};

use super::block_textures::BlockTextureLayers;
use super::voxel_material::StandardVoxelMaterial;
use binary::PaddedVoxels;

pub mod ao;
pub mod binary;
pub mod chunk_mesh;
pub mod coarse;
pub mod emit;
pub mod interior;
pub mod lod;
pub mod lod_tiles;
pub mod neighbors;
pub mod pack;
pub mod queue;
pub mod records;
pub mod visibility;

/// Mesh a padded chunk snapshot. `None` = no visible faces (don't render).
pub fn build_chunk_mesh(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> Option<Mesh> {
    emit::quads_to_mesh(&binary::mesh_quads(padded, &binary::CutoutTable::installed()), layers)
}

/// Everything the async mesh task produces for one chunk. Exactly one of
/// `mesh`/`records` is populated, per the `pull` flag `mesh_chunk` was called
/// with — `mesh` for the legacy `Mesh3d` path, `records` for the pulled
/// (mesh-free) path.
pub struct MeshOutput {
    pub mesh: Option<Mesh>,
    pub records: Option<Vec<records::QuadRecord>>,
    pub connectivity: u16,
    pub quad_count: u32,
    /// Some quad faces into a dug space (`Quad::interior`): the x-ray view's
    /// interior pass draws this chunk (`InteriorFaces`).
    pub has_interior: bool,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainQuadCount(pub u32);

/// On a pulled brick whose records hold interior quads (`records::INTERIOR_BIT`),
/// so the x-ray's interior depth pass draws only these bricks — the cost
/// scales with the fortress, not the world. Kept in step with `ChunkQuads`
/// by `apply_meshed_chunks`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct InteriorFaces;

/// [`MeshOutput`] as a compute job returns it (`renderer::compute`): the
/// mesh still as data. [`MeshWire::into_output`] makes the `Mesh`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeshWire {
    pub mesh: Option<emit::MeshData>,
    pub records: Option<Vec<records::QuadRecord>>,
    pub connectivity: u16,
    pub quad_count: u32,
    pub has_interior: bool,
}

impl MeshWire {
    /// Nothing to draw (a buried brick, in either render mode).
    pub const EMPTY: MeshWire =
        MeshWire { mesh: None, records: None, connectivity: 0, quad_count: 0, has_interior: false };

    pub fn into_output(self) -> MeshOutput {
        MeshOutput {
            mesh: self.mesh.map(emit::MeshData::into_mesh),
            records: self.records,
            connectivity: self.connectivity,
            quad_count: self.quad_count,
            has_interior: self.has_interior,
        }
    }
}

impl MeshOutput {
    /// Whether applying this output creates or replaces GPU-backed terrain data.
    /// Empty chunks still carry connectivity, but must not consume the per-frame
    /// upload budget that exists to smooth actual buffer creation.
    pub fn requires_upload(&self) -> bool {
        self.mesh.is_some() || self.records.is_some()
    }
}

/// Mesh OR records (per `pull`) + connectivity in one pass over the padded snapshot.
/// Cutout blocks (leaves) come from the installed `game::block_props`.
pub fn mesh_chunk(padded: &PaddedVoxels, layers: &BlockTextureLayers, pull: bool) -> MeshOutput {
    mesh_chunk_classified(padded, layers, pull, None)
}

/// [`mesh_chunk`], marking the quads that face into dug space per `interior`
/// (pulled records only: the mesh path has no x-ray).
pub fn mesh_chunk_classified(
    padded: &PaddedVoxels,
    layers: &BlockTextureLayers,
    pull: bool,
    interior: Option<&interior::InteriorGround>,
) -> MeshOutput {
    mesh_chunk_classified_wire(padded, layers, pull, interior, true).into_output()
}

/// [`mesh_chunk_classified`] with the mesh still as data (compute jobs).
/// `connectivity`: whether to flood-fill the brick's open space for its
/// cave-culling mask ([`visibility::face_connectivity`]). Only `cull_chunks`
/// reads the mask and it is off unless asked for (`CullingConfig`), while the
/// flood was 44 % of a join's mesh-thread time (2026-11-19 perf capture);
/// without it the brick reports [`visibility::ALL_CONNECTED`], which is what
/// the culler assumes of a brick it knows nothing about.
pub fn mesh_chunk_classified_wire(
    padded: &PaddedVoxels,
    layers: &BlockTextureLayers,
    pull: bool,
    interior: Option<&interior::InteriorGround>,
    connectivity: bool,
) -> MeshWire {
    let cutout = binary::CutoutTable::installed();
    let quads = binary::mesh_quads_classified(padded, &cutout, interior.filter(|_| pull));
    let has_interior = quads.iter().any(|q| q.interior);
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
        connectivity: if connectivity {
            visibility::face_connectivity(padded, cutout)
        } else {
            visibility::ALL_CONNECTED
        },
        quad_count,
        has_interior,
    }
}

/// Half a render brick's edge: its constant `Aabb` is the local 0..BRICK_VOXELS cube.
const BRICK_HALF: f32 = game::BRICK_VOXELS as f32 / 2.0;

#[derive(Resource)]
pub struct ChunkMaterial(pub Handle<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>);

/// In-flight background mesh build for an entity, spawned by `queue::spawn_mesh_tasks`
/// and polled to completion by `apply_meshed_chunks`.
#[derive(Component)]
pub struct MeshingTask(Task<MeshOutput>);

/// Running tally of pull-path record memory as full-res chunks land, so a live
/// run prints the headline number the pull path exists for: 8-byte quad records
/// vs the legacy ~56-byte-per-quad packed vertices. Logged (`[pull-mem]`, info)
/// every 512 chunks by `apply_meshed_chunks`. Render-derived; never hashed.
/// Optional in the system so headless tests that skip registering it still run.
#[derive(Resource, Default)]
pub struct PullMemStats {
    pub chunks: usize,
    pub quads: u64,
}

/// Polls in-flight `MeshingTask`s to completion and attaches the resulting `Mesh3d`
/// (or strips it, for an all-air chunk), up to `budgets.uploads_per_frame` per frame,
/// tracked via the near-only `FrameUploads` counter — remaining completed tasks
/// attach next frame. Raster LOD has its own equally bounded far upload counter,
/// so neither lane can starve the other. Also handles `VoxelData` removal (uncapped: a
/// chunk that's gone must not linger with a stale mesh).
pub fn apply_meshed_chunks(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut MeshingTask)>,
    mut removed: RemovedComponents<super::bridge::VoxelData>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    still_alive: Query<Entity, With<super::bridge::VoxelData>>,
    budgets: Res<queue::MeshBudgets>,
    mut uploads: ResMut<queue::FrameUploads>,
    mut pull_mem: Option<ResMut<PullMemStats>>,
    render_config: Option<Res<super::config::TerrainRenderConfig>>,
    brick_of: Query<&super::bricks::BrickOf>,
) {
    // A brick of a sim chunk keeps its records and its chunk draws them all
    // as one unit (`chunk_mesh`), on every backend. An entity with no sim
    // chunk draws itself: its `ChunkQuads`, or on `PerChunk` (WebGL2) a mesh
    // expanded from them here.
    let per_chunk = chunk_mesh::per_chunk_backend(render_config.as_deref());
    for (e, mut task) in &mut tasks {
        if uploads.0 >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(output) = block_on(future::poll_once(&mut task.0)) else {
            continue;
        };
        let owner = brick_of.get(e).ok().map(|of| of.chunk);
        // A brick of a merged chunk uploads nothing itself: its chunk's
        // rebuild counts against the budget instead.
        if output.requires_upload() && owner.is_none() {
            uploads.0 += 1;
        }
        let MeshOutput {
            mesh,
            records,
            connectivity,
            quad_count,
            has_interior,
        } = output;
        let records = match (owner, per_chunk, records) {
            (Some(chunk), _, records) => {
                keep_brick_records(&mut commands, e, chunk, records, connectivity, quad_count);
                continue;
            }
            (None, true, Some(records)) => {
                draw_own_records(&mut commands, &mut meshes, material.as_deref(), e, &records, connectivity, quad_count);
                continue;
            }
            (None, _, records) => records,
        };
        if let Some(records) = records {
            if let Some(stats) = pull_mem.as_deref_mut() {
                stats.chunks += 1;
                stats.quads += records.len() as u64;
                if stats.chunks % 512 == 0 {
                    // 8 B/record now vs the legacy path's ~56 B/quad packed
                    // vertices (4 verts * 8 B position + 8 B packed attr,
                    // pre-index) — the pull path's headline memory win.
                    let pull_bytes = stats.quads * 8;
                    let legacy_bytes = stats.quads * 56;
                    info!(
                        "[pull-mem] {} chunks, {} quads: {} KiB records vs {} KiB legacy-mesh estimate (56 B/quad) — {:.1}x smaller",
                        stats.chunks,
                        stats.quads,
                        pull_bytes / 1024,
                        legacy_bytes / 1024,
                        legacy_bytes as f64 / pull_bytes.max(1) as f64,
                    );
                }
            }
            let mut ec = commands.entity(e);
            ec.insert((
                super::voxel_pull::ChunkQuads(std::sync::Arc::new(records)),
                bevy::camera::primitives::Aabb {
                    center: bevy::math::Vec3A::splat(BRICK_HALF),
                    half_extents: bevy::math::Vec3A::splat(BRICK_HALF),
                },
                bevy::camera::visibility::NoAutoAabb,
                visibility::ChunkConnectivity(connectivity),
                TerrainQuadCount(quad_count),
            ));
            if has_interior {
                ec.insert(InteriorFaces);
            } else {
                ec.remove::<InteriorFaces>();
            }
            ec.remove::<MeshingTask>();
            continue;
        }
        match mesh {
            Some(mesh) => {
                let mut ec = commands.entity(e);
                ec.insert((
                    Mesh3d(meshes.add(mesh)),
                    // Chunks always occupy within their 0..32 local cube; a
                    // constant conservative Aabb keeps frustum culling working
                    // without POSITION-derived bounds (required once Stage B
                    // drops the POSITION attribute) and independent of mesh
                    // data lifetime (RENDER_WORLD-only assets).
                    bevy::camera::primitives::Aabb {
                        center: bevy::math::Vec3A::splat(BRICK_HALF),
                        half_extents: bevy::math::Vec3A::splat(BRICK_HALF),
                    },
                    bevy::camera::visibility::NoAutoAabb,
                    visibility::ChunkConnectivity(connectivity),
                    TerrainQuadCount(quad_count),
                ));
                if let Some(material) = &material {
                    ec.insert(MeshMaterial3d(material.0.clone()));
                }
                ec.remove::<MeshingTask>();
            }
            None => {
                let mut ec = commands.entity(e);
                ec.insert(visibility::ChunkConnectivity(connectivity));
                ec.remove::<(
                    Mesh3d,
                    MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                    MeshingTask,
                    super::voxel_pull::ChunkQuads,
                    TerrainQuadCount,
                    InteriorFaces,
                )>();
            }
        }
    }
    // Bevy's removal log for a component persists for the whole frame even if the
    // component was re-inserted afterwards. So a remove-then-reinsert of VoxelData on
    // the same entity within one frame (which drain_region_updates can produce when it
    // drains several buffered SetVoxelComponent(None)/Some(..) updates in one PreUpdate
    // pass) would otherwise strip a still-valid entity's Mesh3d/MeshingTask here. The
    // bridge's remove-then-insert both flush (via Commands) before Update runs, so by
    // the time this system runs a same-frame reinsert already carries `VoxelData` again
    // — `VoxelData` presence is the sole authority on liveness. `MeshingTask` presence is
    // NOT a valid liveness signal: it can outlive a permanent removal (an earlier edit's
    // task is still in flight, polled to completion only in the loop above), so keying
    // off it would let a stale in-flight result attach a ghost mesh after `VoxelData` is
    // gone for good. Removing `MeshingTask` here alongside `Mesh3d` cancels that stale
    // work outright (dropping the `Task` cancels/discards its background job), so no
    // later poll can ever apply it.
    for e in removed.read() {
        if still_alive.contains(e) {
            continue;
        }
        if let Ok(mut ec) = commands.get_entity(e) {
            // ChunkConnectivity is stripped too: `mark_remesh_dirty` re-grids
            // a reinserted entity immediately, and a mask from its previous
            // life's content would gate BFS sight lines on geometry that no
            // longer renders (transient under-visibility) until the remesh
            // lands. Maskless = traverse-as-open, the conservative state.
            ec.remove::<(
                Mesh3d,
                MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                MeshingTask,
                visibility::ChunkConnectivity,
                super::voxel_pull::ChunkQuads,
                TerrainQuadCount,
                chunk_mesh::BrickRecords,
            )>();
            if let Some(chunk) = brick_of.get(e).ok().map(|of| of.chunk) {
                if let Ok(mut chunk) = commands.get_entity(chunk) {
                    chunk.insert(chunk_mesh::ChunkMeshDirty);
                }
            }
        }
    }
}

/// `apply_meshed_chunks` for a brick of a sim chunk: keep its records (or
/// none, when it meshed empty) for the chunk's merged rebuild and mark the
/// chunk (`chunk_mesh`). The brick itself draws nothing.
fn keep_brick_records(
    commands: &mut Commands,
    e: Entity,
    chunk: Entity,
    records: Option<Vec<records::QuadRecord>>,
    connectivity: u16,
    quad_count: u32,
) {
    let mut ec = commands.entity(e);
    ec.insert(visibility::ChunkConnectivity(connectivity));
    ec.remove::<(
        MeshingTask,
        InteriorFaces,
        super::voxel_pull::ChunkQuads,
        Mesh3d,
        MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
    )>();
    match records {
        Some(records) => {
            ec.insert((chunk_mesh::BrickRecords(std::sync::Arc::new(records)), TerrainQuadCount(quad_count)));
        }
        None => {
            ec.remove::<(chunk_mesh::BrickRecords, TerrainQuadCount)>();
        }
    }
    if let Ok(mut chunk) = commands.get_entity(chunk) {
        chunk.insert(chunk_mesh::ChunkMeshDirty);
    }
}

/// `apply_meshed_chunks` on `PerChunk` (WebGL2) for an entity with no sim
/// chunk: its records expanded into its own mesh.
fn draw_own_records(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Option<&ChunkMaterial>,
    e: Entity,
    records: &[records::QuadRecord],
    connectivity: u16,
    quad_count: u32,
) {
    let mut ec = commands.entity(e);
    ec.insert((visibility::ChunkConnectivity(connectivity), TerrainQuadCount(quad_count)));
    ec.remove::<(MeshingTask, InteriorFaces)>();
    match chunk_mesh::records_to_mesh_data(records) {
        Some(data) => {
            ec.insert((
                Mesh3d(meshes.add(data.into_mesh())),
                // Constant brick-cube bounds, as the pulled path's bricks.
                bevy::camera::primitives::Aabb {
                    center: bevy::math::Vec3A::splat(BRICK_HALF),
                    half_extents: bevy::math::Vec3A::splat(BRICK_HALF),
                },
                bevy::camera::visibility::NoAutoAabb,
            ));
            if let Some(material) = material {
                ec.insert(MeshMaterial3d(material.0.clone()));
            }
        }
        None => {
            ec.remove::<(Mesh3d, MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>)>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use game::{BlockId, Voxel, BRICK_VOXEL_COUNT};

    /// Regression: gaps between chunks, user report 2026-07-08 "there is a gap between chunks" (memory flat-world-ocean-spawn, 151d3c0b)
    #[test]
    fn boundary_voxels_are_meshed() {
        // Regression: voxels on the chunk's outer shell (x/y/z = 0 or 31) must
        // produce faces. greedy_quads only meshes the *interior* of [min, max],
        // so meshing the bare 32^3 buffer with [0..31] silently drops the
        // boundary shell and every chunk renders with a 2-voxel gap to its
        // neighbours.
        let mut voxels = vec![Voxel::default(); BRICK_VOXEL_COUNT];
        voxels[0 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let mesh = build_chunk_mesh(
            &PaddedVoxels::from_interior(&voxels),
            &BlockTextureLayers::default(),
        )
        .expect("boundary voxel must yield a mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
    }

    /// Regression: same user-reported chunk-gap bug 2026-07-08: fully solid chunks had no mesh (memory flat-world-ocean-spawn names this test)
    #[test]
    fn full_chunk_mesh_spans_full_extent() {
        // A fully solid chunk must mesh as the whole 32-voxel cube, i.e. vertex
        // positions spanning [0, 32] on every axis — not shrunk to the interior.
        let voxels = vec![Voxel::new(BlockId(1)); BRICK_VOXEL_COUNT];
        let mesh = build_chunk_mesh(
            &PaddedVoxels::from_interior(&voxels),
            &BlockTextureLayers::default(),
        )
        .expect("mesh");
        let VertexAttributeValues::Uint32x2(packed) = mesh
            .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
            .unwrap()
        else {
            panic!("packed attribute must be Uint32x2");
        };
        let positions: Vec<[u32; 3]> = packed.iter().map(|&p| pack::unpack(p).pos).collect();
        for axis in 0..3 {
            let lo = positions.iter().map(|p| p[axis]).min().unwrap();
            let hi = positions.iter().map(|p| p[axis]).max().unwrap();
            assert_eq!(lo, 0, "axis {axis} min");
            assert_eq!(hi, 32, "axis {axis} max");
        }
    }
}
