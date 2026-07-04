use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use block_mesh::ndshape::ConstShape;
use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
use game::ChunkShape;
use game::Voxel;

use super::voxel_material::{self, StandardVoxelMaterial};
use super::VoxelTypeLayers;

pub fn build_chunk_mesh(voxels: &[Voxel], layers: &VoxelTypeLayers) -> Option<Mesh> {
    let mut buffer = GreedyQuadsBuffer::new(voxels.len());
    greedy_quads(voxels, &ChunkShape {}, [0; 3], [31; 3], &RIGHT_HANDED_Y_UP_CONFIG.faces, &mut buffer);
    if buffer.quads.num_quads() == 0 {
        return None;
    }
    let num_vertices = buffer.quads.num_quads() * 4;
    let mut indices = Vec::with_capacity(buffer.quads.num_quads() * 6);
    let mut positions = Vec::with_capacity(num_vertices);
    let mut normals = Vec::with_capacity(num_vertices);
    let mut uvs = Vec::with_capacity(num_vertices);
    let mut tex_indices: Vec<[u32; 3]> = Vec::with_capacity(num_vertices);
    let quad_uv = [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
    for (group, face) in buffer.quads.groups.iter().zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter()) {
        for quad in group.iter() {
            indices.extend_from_slice(&face.quad_mesh_indices(positions.len() as u32));
            positions.extend_from_slice(&face.quad_mesh_positions(quad, 1.0));
            normals.extend_from_slice(&face.quad_mesh_normals());
            for uv in quad_uv {
                uvs.push([uv[0] * quad.width as f32, uv[1] * quad.height as f32]);
            }
            let voxel_index = ChunkShape::linearize(*quad.minimum) as usize;
            let layer = layers.0.get(&voxels[voxel_index].kind).copied().unwrap_or(0);
            tex_indices.extend(std::iter::repeat([layer, layer, layer]).take(4));
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::Float32x3(normals));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    // The voxel shader unconditionally reads vertex color (see assets/shaders/voxel_texture.wgsl),
    // which requires bevy_pbr's mesh pipeline to define VERTEX_COLORS; that only happens when the
    // mesh actually carries this attribute, so emit an opaque-white filler rather than pruning it
    // out of voxel_material::vertex_layout().
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, VertexAttributeValues::Float32x4(vec![[1.0; 4]; num_vertices]));
    mesh.insert_attribute(voxel_material::ATTRIBUTE_TEX_INDEX, VertexAttributeValues::Uint32x3(tex_indices));
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
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
    layers: Res<VoxelTypeLayers>,
) {
    let pool = AsyncComputeTaskPool::get();
    for (e, voxels) in &changed {
        let voxels = voxels.0.clone();
        let layers = layers.0.clone();
        let task = pool.spawn(async move { build_chunk_mesh(&voxels, &VoxelTypeLayers(layers)) });
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
    use block_mesh::ndshape::ConstShape; // ndshape itself is no longer a direct dep (task 2)
    use game::ChunkShape;
    use game::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

    #[test]
    fn empty_chunk_yields_no_mesh() {
        let voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        assert!(build_chunk_mesh(&voxels, &VoxelTypeLayers::default()).is_none());
    }

    #[test]
    fn single_voxel_yields_cube() {
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = ChunkShape::linearize([5, 5, 5]) as usize;
        voxels[idx] = Voxel::new(VoxelType::Black);
        let mesh = build_chunk_mesh(&voxels, &VoxelTypeLayers::default()).expect("mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
        assert_eq!(mesh.indices().unwrap().len(), 36, "6 faces x 2 tris x 3");
    }

    #[test]
    fn async_meshing_attaches_mesh_eventually() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::VoxelTypeLayers>()
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[ChunkShape::linearize([5, 5, 5]) as usize] = Voxel::new(VoxelType::Black);
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
            .init_resource::<VoxelTypeLayers>()
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
        let idx = ChunkShape::linearize([5, 5, 5]) as usize;
        voxels[idx] = Voxel::new(VoxelType::Black);

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
    fn removal_while_task_in_flight_clears_mesh() {
        use super::super::bridge::VoxelData;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_resource::<crate::renderer::VoxelTypeLayers>()
            .add_systems(Update, (queue_meshing, apply_meshed_chunks));

        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        voxels[ChunkShape::linearize([5, 5, 5]) as usize] = Voxel::new(VoxelType::Black);
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
}
