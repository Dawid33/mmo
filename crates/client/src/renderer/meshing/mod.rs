use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, Task};

use super::block_textures::BlockTextureLayers;
use super::voxel_material::StandardVoxelMaterial;
use binary::PaddedVoxels;

pub mod ao;
pub mod binary;
pub mod emit;
pub mod lod;
pub mod neighbors;
pub mod pack;
pub mod queue;
pub mod records;
pub mod visibility;

/// Mesh a padded chunk snapshot. `None` = no visible faces (don't render).
pub fn build_chunk_mesh(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> Option<Mesh> {
    emit::quads_to_mesh(&binary::mesh_quads(padded), layers)
}

/// Everything the async mesh task produces for one chunk. Exactly one of
/// `mesh`/`records` is populated, per the `pull` flag `mesh_chunk` was called
/// with — `mesh` for the legacy `Mesh3d` path, `records` for the pulled
/// (mesh-free) path.
pub struct MeshOutput {
    pub mesh: Option<Mesh>,
    pub records: Option<Vec<records::QuadRecord>>,
    pub connectivity: u16,
}

/// Mesh OR records (per `pull`) + connectivity in one pass over the padded snapshot.
pub fn mesh_chunk(padded: &PaddedVoxels, layers: &BlockTextureLayers, pull: bool) -> MeshOutput {
    let quads = binary::mesh_quads(padded);
    let (mesh, records) = if pull {
        let r = records::quads_to_records(&quads, layers);
        (None, (!r.is_empty()).then_some(r))
    } else {
        (emit::quads_to_mesh(&quads, layers), None)
    };
    MeshOutput { mesh, records, connectivity: visibility::face_connectivity(padded) }
}

#[derive(Resource)]
pub struct ChunkMaterial(pub Handle<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>);

/// In-flight background mesh build for an entity, spawned by `queue::spawn_mesh_tasks`
/// and polled to completion by `apply_meshed_chunks`.
#[derive(Component)]
pub struct MeshingTask(Task<MeshOutput>);

/// Polls in-flight `MeshingTask`s to completion and attaches the resulting `Mesh3d`
/// (or strips it, for an all-air chunk), up to `budgets.uploads_per_frame` per frame,
/// tracked via the shared `FrameUploads` counter — remaining completed tasks attach
/// next frame. This system runs before `lod::apply_lod_meshes` in the chain and
/// always claims its uploads first, so full-res chunks never get starved of the
/// shared upload budget by LOD work. Also handles `VoxelData` removal (uncapped: a
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
) {
    for (e, mut task) in &mut tasks {
        if uploads.0 >= budgets.uploads_per_frame {
            break; // remaining completed tasks attach next frame
        }
        let Some(MeshOutput { mesh, records, connectivity }) = block_on(future::poll_once(&mut task.0)) else { continue };
        uploads.0 += 1;
        if let Some(records) = records {
            let mut ec = commands.entity(e);
            ec.insert((
                super::voxel_pull::ChunkQuads(std::sync::Arc::new(records)),
                bevy::camera::primitives::Aabb {
                    center: bevy::math::Vec3A::splat(16.0),
                    half_extents: bevy::math::Vec3A::splat(16.0),
                },
                bevy::camera::visibility::NoAutoAabb,
                visibility::ChunkConnectivity(connectivity),
            ));
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
                        center: bevy::math::Vec3A::splat(16.0),
                        half_extents: bevy::math::Vec3A::splat(16.0),
                    },
                    bevy::camera::visibility::NoAutoAabb,
                    visibility::ChunkConnectivity(connectivity),
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
            )>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;
    use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

    #[test]
    fn empty_chunk_yields_no_mesh() {
        let voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        assert!(build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), &BlockTextureLayers::default()).is_none());
    }

    #[test]
    fn single_voxel_yields_cube() {
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = 5 + 32 * 5 + 32 * 32 * 5;
        voxels[idx] = Voxel::new(BlockId(1));
        let mesh = build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), &BlockTextureLayers::default())
            .expect("mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
        assert_eq!(mesh.indices().unwrap().len(), 36, "6 faces x 2 tris x 3");
    }

    #[test]
    fn boundary_voxels_are_meshed() {
        // Regression: voxels on the chunk's outer shell (x/y/z = 0 or 31) must
        // produce faces. greedy_quads only meshes the *interior* of [min, max],
        // so meshing the bare 32^3 buffer with [0..31] silently drops the
        // boundary shell and every chunk renders with a 2-voxel gap to its
        // neighbours.
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[0 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let mesh = build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), &BlockTextureLayers::default())
            .expect("boundary voxel must yield a mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
    }

    #[test]
    fn full_chunk_mesh_spans_full_extent() {
        // A fully solid chunk must mesh as the whole 32-voxel cube, i.e. vertex
        // positions spanning [0, 32] on every axis — not shrunk to the interior.
        let voxels = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        let mesh = build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), &BlockTextureLayers::default())
            .expect("mesh");
        let VertexAttributeValues::Uint32x2(packed) =
            mesh.attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED).unwrap()
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

    #[test]
    fn async_meshing_attaches_mesh_eventually() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        app.update();
        // Evidence that meshing went through the pipeline on the first update:
        // the task was spawned (still in flight) — or already completed AND
        // applied, since `apply_meshed_chunks` chains right after
        // `spawn_mesh_tasks` and a tiny chunk's task can finish within the
        // same update. Only "neither" would mean the pipeline never ran.
        assert!(
            app.world().entity(e).contains::<MeshingTask>()
                || app.world().entity(e).contains::<Mesh3d>(),
            "meshing pipeline must have claimed the chunk on the first update"
        );
        settle_until_meshed(&mut app, e);
    }

    /// Runs `app.update()` (with a short sleep between attempts so the background
    /// meshing task gets a chance to make progress) until `e` has a `Mesh3d`, or
    /// panics after a generous wall-clock deadline. Deadline-based rather than
    /// iteration-counted: under `--include-ignored` the slow soak tests hog the
    /// shared AsyncComputeTaskPool, and a fixed ~500ms budget flaked ~50% of
    /// full-suite runs while passing instantly in isolation.
    fn settle_until_meshed(app: &mut App, e: Entity) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            app.update();
            if app.world().entity(e).contains::<Mesh3d>() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("mesh never attached within deadline");
    }

    #[test]
    fn pull_path_attaches_records_and_no_mesh() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .insert_resource(crate::renderer::voxel_pull::RenderPath { pull: true })
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            app.update();
            if let Some(q) = app.world().entity(e).get::<crate::renderer::voxel_pull::ChunkQuads>() {
                assert_eq!(q.0.len(), 6, "single voxel = 6 quads");
                assert!(!app.world().entity(e).contains::<Mesh3d>(), "pull path must not attach a mesh");
                assert!(app.world().entity(e).contains::<Visibility>(), "required for cave-culling writes");
                assert!(app.world().entity(e).contains::<bevy::camera::primitives::Aabb>());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "records never attached");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn same_frame_remove_and_reinsert_keeps_mesh() {
        use super::super::bridge::VoxelData;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>()
            .init_resource::<BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );

        let image_handle = app.world_mut().resource_mut::<Assets<Image>>().add(Image::default());
        let handle = app
            .world_mut()
            .resource_mut::<Assets<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>>()
            .add(ExtendedMaterial {
                base: StandardMaterial::default(),
                extension: StandardVoxelMaterial { voxels_texture: image_handle },
            });
        app.insert_resource(ChunkMaterial(handle));

        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = 5 + 32 * 5 + 32 * 32 * 5;
        voxels[idx] = Voxel::new(BlockId(1));

        let e = app.world_mut().spawn(VoxelData(voxels.clone())).id();
        settle_until_meshed(&mut app, e);
        assert!(app.world().entity(e).contains::<Mesh3d>(), "mesh should exist after initial insert");

        // Same-frame remove-then-reinsert, as `drain_region_updates` can produce when it
        // drains several buffered SetVoxelComponent(None)/Some(..) updates in one PreUpdate pass.
        // This must not flicker the mesh off even transiently: `apply_meshed_chunks`'s
        // `still_alive` guard must skip the stale removal within this very update, since
        // the entity's VoxelData (or a freshly queued MeshingTask) is still present.
        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.world_mut().entity_mut(e).insert(VoxelData(voxels));
        app.update();
        assert!(app.world().entity(e).contains::<Mesh3d>(), "mesh should survive same-frame remove+reinsert");
    }

    #[test]
    #[ignore = "slow: >100ms — run via just test-full"]
    fn removal_while_task_in_flight_clears_mesh() {
        use super::super::bridge::VoxelData;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );

        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(VoxelData(voxels)).id();

        // Queue the meshing task (MeshingTask attached after this update's commands flush),
        // then remove VoxelData permanently (no reinsert) — mirroring the bridge's
        // SetVoxelComponent(key, None) arm clearing a chunk to air while an earlier edit's
        // mesh task is still in flight.
        app.update();
        app.world_mut().entity_mut(e).remove::<VoxelData>();

        for _ in 0..100 {
            app.update();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!app.world().entity(e).contains::<Mesh3d>(), "ghost mesh must not be attached after VoxelData removal");
        assert!(!app.world().entity(e).contains::<MeshingTask>(), "in-flight task must be cancelled on VoxelData removal");
    }

    #[test]
    fn derives_and_meshes_a_generated_floor_chunk() {
        let chunk = game::Chunk::flat_floor(8);
        let voxels = game::derive_voxels(&chunk.blocks, &chunk.chisel);
        let mesh = build_chunk_mesh(&PaddedVoxels::from_interior(&voxels), &BlockTextureLayers::default());
        let mesh = mesh.expect("a floor chunk has solid voxels and must produce a mesh");
        assert!(
            mesh.indices().map(|i| i.len()).unwrap_or(0) > 0,
            "the meshed floor must have triangles"
        );
    }

    #[test]
    fn meshed_chunks_carry_their_connectivity_mask() {
        use super::visibility::{ChunkConnectivity, ALL_CONNECTED};

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        // A solid chunk with a single straight X tunnel: mask must be exactly
        // the ±X pair — proves the real task computed real connectivity.
        let mut voxels = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
        for x in 0..32 {
            voxels[x + 32 * 5 + 32 * 32 * 5] = Voxel::default();
        }
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        settle_until_meshed(&mut app, e);
        let mask = app.world().entity(e).get::<ChunkConnectivity>().expect("mask attached").0;
        use super::binary::Face;
        assert_eq!(mask, super::visibility::pair_bit(Face::PosX.index(), Face::NegX.index()));

        // All-air chunk: mesh stripped, mask = ALL_CONNECTED.
        let air = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let e2 = app.world_mut().spawn(crate::renderer::bridge::VoxelData(air)).id();
        // Air chunks never get Mesh3d; settle on the mask instead.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            app.update();
            if let Some(c) = app.world().entity(e2).get::<ChunkConnectivity>() {
                assert_eq!(c.0, ALL_CONNECTED);
                assert!(!app.world().entity(e2).contains::<Mesh3d>());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "mask never attached");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn meshed_chunks_get_a_constant_aabb() {
        use bevy::camera::primitives::Aabb;
        use bevy::camera::visibility::NoAutoAabb;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .init_resource::<queue::ChunkGrid>()
            .init_resource::<queue::RemeshQueue>()
            .init_resource::<queue::MeshBudgets>()
            .init_resource::<queue::FrameUploads>()
            .add_systems(
                Update,
                (queue::mark_remesh_dirty, queue::prune_chunk_grid, queue::spawn_mesh_tasks, apply_meshed_chunks)
                    .chain(),
            );
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        settle_until_meshed(&mut app, e);
        let aabb = app.world().entity(e).get::<Aabb>().expect("constant chunk Aabb inserted");
        assert_eq!(aabb.center, bevy::math::Vec3A::splat(16.0));
        assert_eq!(aabb.half_extents, bevy::math::Vec3A::splat(16.0));
        assert!(app.world().entity(e).contains::<NoAutoAabb>(), "auto-Aabb must not overwrite it");
    }
}
