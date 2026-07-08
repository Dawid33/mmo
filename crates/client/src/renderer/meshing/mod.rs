use bevy::mesh::Mesh;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};

use super::block_textures::BlockTextureLayers;
use super::voxel_material::StandardVoxelMaterial;
use binary::PaddedVoxels;

pub mod ao;
pub mod binary;
pub mod emit;
pub mod neighbors;

/// Mesh a padded chunk snapshot. `None` = no visible faces (don't render).
pub fn build_chunk_mesh(padded: &PaddedVoxels, layers: &BlockTextureLayers) -> Option<Mesh> {
    emit::quads_to_mesh(&binary::mesh_quads(padded), layers)
}

#[derive(Resource)]
pub struct ChunkMaterial(pub Handle<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>);

/// In-flight background mesh build for an entity, spawned by `queue_meshing` and
/// polled to completion by `apply_meshed_chunks`.
#[derive(Component)]
pub struct MeshingTask(Task<Option<Mesh>>);

/// Spawns a background meshing job whenever an entity's `VoxelData` changes. Inserting
/// a fresh `MeshingTask` replaces any prior in-flight task component-wise; the old
/// `Task` is dropped (and its background work cancelled/discarded) along with it, so
/// stale results can never land after a newer edit.
pub fn queue_meshing(
    mut commands: Commands,
    changed: Query<(Entity, &super::bridge::VoxelData), Changed<super::bridge::VoxelData>>,
    layers: Res<BlockTextureLayers>,
) {
    let pool = AsyncComputeTaskPool::get();
    for (e, voxels) in &changed {
        let padded = PaddedVoxels::from_interior(&voxels.0);
        let layers = layers.0.clone();
        let task = pool.spawn(async move { build_chunk_mesh(&padded, &BlockTextureLayers(layers)) });
        commands.entity(e).insert(MeshingTask(task));
    }
}

/// Polls in-flight `MeshingTask`s to completion and attaches the resulting `Mesh3d`
/// (or strips it, for an all-air chunk). Also handles `VoxelData` removal.
pub fn apply_meshed_chunks(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut MeshingTask)>,
    mut removed: RemovedComponents<super::bridge::VoxelData>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ChunkMaterial>>,
    still_alive: Query<Entity, With<super::bridge::VoxelData>>,
) {
    for (e, mut task) in &mut tasks {
        let Some(result) = block_on(future::poll_once(&mut task.0)) else { continue };
        match result {
            Some(mesh) => {
                let mut ec = commands.entity(e);
                ec.insert(Mesh3d(meshes.add(mesh)));
                if let Some(material) = &material {
                    ec.insert(MeshMaterial3d(material.0.clone()));
                }
                ec.remove::<MeshingTask>();
            }
            None => {
                commands.entity(e).remove::<(
                    Mesh3d,
                    MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                    MeshingTask,
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
            ec.remove::<(
                Mesh3d,
                MeshMaterial3d<ExtendedMaterial<StandardMaterial, StandardVoxelMaterial>>,
                MeshingTask,
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
        let VertexAttributeValues::Float32x3(positions) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions must be Float32x3");
        };
        for axis in 0..3 {
            let lo = positions.iter().map(|p| p[axis]).fold(f32::MAX, f32::min);
            let hi = positions.iter().map(|p| p[axis]).fold(f32::MIN, f32::max);
            assert_eq!(lo, 0.0, "axis {axis} min");
            assert_eq!(hi, 32.0, "axis {axis} max");
        }
    }

    #[test]
    fn async_meshing_attaches_mesh_eventually() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::block_textures::BlockTextureLayers>()
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[5 + 32 * 5 + 32 * 32 * 5] = Voxel::new(BlockId(1));
        let e = app.world_mut().spawn(crate::renderer::bridge::VoxelData(voxels)).id();
        app.update();
        // Non-racy evidence that meshing actually went through the async path rather
        // than, say, `queue_meshing` never running: the task must be queued (and not
        // yet resolved, since nothing has polled it before this first `Update`) before
        // we start looping to await its completion below.
        assert!(app.world().entity(e).contains::<MeshingTask>(), "MeshingTask should be queued after first update");
        for _ in 0..100 {
            app.update();
            if app.world().entity(e).contains::<Mesh3d>() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("mesh never attached");
    }

    /// Runs `app.update()` (with a short sleep between attempts so the background
    /// meshing task gets a chance to make progress) until `e` has a `Mesh3d`, or
    /// panics after 100 attempts.
    fn settle_until_meshed(app: &mut App, e: Entity) {
        for _ in 0..100 {
            app.update();
            if app.world().entity(e).contains::<Mesh3d>() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("mesh never attached");
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
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));

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
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));

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
}
