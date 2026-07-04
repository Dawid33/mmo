use bevy::asset::RenderAssetUsages;
use bevy::ecs::entity::EntityHashSet;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
use game::ChunkShape;
use rollback::Voxel;

pub fn build_chunk_mesh(voxels: &[Voxel]) -> Option<Mesh> {
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
    let quad_uv = [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
    for (group, face) in buffer.quads.groups.iter().zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter()) {
        for quad in group.iter() {
            indices.extend_from_slice(&face.quad_mesh_indices(positions.len() as u32));
            positions.extend_from_slice(&face.quad_mesh_positions(quad, 1.0));
            normals.extend_from_slice(&face.quad_mesh_normals());
            for uv in quad_uv {
                uvs.push([uv[0] * quad.width as f32, uv[1] * quad.height as f32]);
            }
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::Float32x3(normals));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

#[derive(Resource)]
pub struct ChunkMaterial(pub Handle<StandardMaterial>);

pub fn mesh_chunks(
    mut commands: Commands,
    changed: Query<(Entity, &super::bridge::VoxelData), Changed<super::bridge::VoxelData>>,
    mut removed: RemovedComponents<super::bridge::VoxelData>,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Res<ChunkMaterial>,
) {
    let mut handled = EntityHashSet::default();
    for (e, voxels) in &changed {
        handled.insert(e);
        match build_chunk_mesh(&voxels.0) {
            Some(mesh) => {
                commands.entity(e).insert((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.0.clone())));
            }
            None => {
                commands.entity(e).remove::<Mesh3d>();
            }
        }
    }
    // Bevy's removal log for a component persists for the whole frame even if the
    // component was re-inserted afterwards, and `Changed` on the re-inserted component
    // is independently true. So a remove-then-reinsert of VoxelData on the same entity
    // within one frame (which drain_region_updates can produce when it drains several
    // buffered SetVoxelComponent(None)/Some(..) updates in one PreUpdate pass) would
    // otherwise queue insert-then-remove here, leaving valid VoxelData with no Mesh3d.
    // The changed-branch's own Some/None decision is authoritative when both fire in
    // the same frame, so skip entities it already handled.
    for e in removed.read() {
        if handled.contains(&e) {
            continue;
        }
        if let Ok(mut ec) = commands.get_entity(e) {
            ec.remove::<Mesh3d>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use block_mesh::ndshape::ConstShape; // ndshape itself is no longer a direct dep (task 2)
    use game::ChunkShape;
    use rollback::{Voxel, VoxelType, CHUNK_VOXEL_COUNT};

    #[test]
    fn empty_chunk_yields_no_mesh() {
        let voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        assert!(build_chunk_mesh(&voxels).is_none());
    }

    #[test]
    fn single_voxel_yields_cube() {
        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = ChunkShape::linearize([5, 5, 5]) as usize;
        voxels[idx] = Voxel::new(VoxelType::Black);
        let mesh = build_chunk_mesh(&voxels).expect("mesh");
        assert_eq!(mesh.count_vertices(), 24, "6 faces x 4 verts");
        assert_eq!(mesh.indices().unwrap().len(), 36, "6 faces x 2 tris x 3");
    }

    #[test]
    fn same_frame_remove_and_reinsert_keeps_mesh() {
        use super::super::bridge::VoxelData;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_systems(Update, mesh_chunks);

        let handle = app.world_mut().resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial::default());
        app.insert_resource(ChunkMaterial(handle));

        let mut voxels = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
        let idx = ChunkShape::linearize([5, 5, 5]) as usize;
        voxels[idx] = Voxel::new(VoxelType::Black);

        let e = app.world_mut().spawn(VoxelData(voxels.clone())).id();
        app.update();
        assert!(app.world().entity(e).contains::<Mesh3d>(), "mesh should exist after initial insert");

        // Same-frame remove-then-reinsert, as `drain_region_updates` can produce when it
        // drains several buffered SetVoxelComponent(None)/Some(..) updates in one PreUpdate pass.
        app.world_mut().entity_mut(e).remove::<VoxelData>();
        app.world_mut().entity_mut(e).insert(VoxelData(voxels));
        app.update();
        assert!(app.world().entity(e).contains::<Mesh3d>(), "mesh should survive same-frame remove+reinsert");
    }
}
