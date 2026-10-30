//! Quad list → Bevy `Mesh`. Packed vertex format (Phase 2 Stage B): a single
//! `ATTRIBUTE_VOXEL_PACKED` (Uint32x2, 8 bytes/vertex) carrying
//! position/face/ao/corner/extent/layer — see `pack.rs` for the bit layout.
//! There is no POSITION attribute; the shader decodes the corner position
//! from the packed word (both the forward and prepass vertex stages).

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material;
use super::ao::ao_flips_quad;
use super::binary::{cross_axes, Face, Quad};
use super::pack;

/// Index patterns by (winding reversed?, ao flipped?). Corner order is
/// (0,0), (1,0), (0,1), (1,1) over (U, V); default diagonal splits 0-3,
/// flipped splits 1-2. Reversed faces (NegX, PosY, NegZ — where U×V points
/// inward) reverse each triangle. Verified by the winding test.
fn quad_indices(face: Face, flipped: bool) -> [u32; 6] {
    let reversed = matches!(face, Face::NegX | Face::PosY | Face::NegZ);
    match (reversed, flipped) {
        (false, false) => [0, 1, 3, 0, 3, 2],
        (false, true) => [0, 1, 2, 1, 3, 2],
        (true, false) => [0, 3, 1, 0, 2, 3],
        (true, true) => [0, 2, 1, 1, 2, 3],
    }
}

pub fn quads_to_mesh(quads: &[Quad], layers: &BlockTextureLayers) -> Option<Mesh> {
    if quads.is_empty() {
        return None;
    }
    let n = quads.len() * 4;
    let mut packed: Vec<[u32; 2]> = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(quads.len() * 6);

    for q in quads {
        let axis = q.face.axis();
        let (ua, va) = cross_axes(axis);
        let base = packed.len() as u32;
        indices.extend(quad_indices(q.face, ao_flips_quad(&q.ao)).map(|i| base + i));

        let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
        let layer = pack::layer_for_face(faces, q.face);

        // The face plane sits at min[axis] for negative faces, +1 for positive.
        let mut origin = [q.min[0], q.min[1], q.min[2]];
        if q.face.positive() {
            origin[axis] += 1;
        }
        for (i, (cu, cv)) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)]
            .into_iter()
            .enumerate()
        {
            let mut pos = origin;
            pos[ua] += cu * q.w;
            pos[va] += cv * q.h;
            packed.push(pack::pack(pos, q.face, q.ao[i], cu, cv, q.w, q.h, layer));
        }
    }

    // CPU copy freed after GPU extraction; colliders come from rapier, tests never extract
    // (MinimalPlugins), and the entity Aabb is inserted constant by `apply_meshed_chunks`
    // so nothing reads mesh data main-world after upload.
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(
        voxel_material::ATTRIBUTE_VOXEL_PACKED,
        VertexAttributeValues::Uint32x2(packed),
    );
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use game::BlockId;

    fn unit_quad(face: Face, ao: [u8; 4]) -> Quad {
        Quad {
            min: [5, 6, 7],
            w: 2,
            h: 3,
            face,
            block: BlockId(1),
            ao,
            cutout: false,
        }
    }

    fn tri_normals_positive(mesh: &Mesh, expected_normal: [f32; 3]) {
        // Positions come from the packed decode — the same source the shader uses.
        let pos: Vec<Vec3> = packed_of(mesh)
            .iter()
            .map(|v| Vec3::new(v.pos[0] as f32, v.pos[1] as f32, v.pos[2] as f32))
            .collect();
        let Indices::U32(idx) = mesh.indices().unwrap() else {
            panic!("u32 indices")
        };
        let n = Vec3::from(expected_normal);
        for tri in idx.chunks(3) {
            let [a, b, c] = [
                pos[tri[0] as usize],
                pos[tri[1] as usize],
                pos[tri[2] as usize],
            ];
            let cross = (b - a).cross(c - a);
            assert!(
                cross.dot(n) > 0.0,
                "triangle {tri:?} winds against its normal"
            );
        }
    }

    #[test]
    fn winding_faces_outward_for_all_six_faces_default_and_flipped() {
        for face in [
            Face::PosX,
            Face::NegX,
            Face::PosY,
            Face::NegY,
            Face::PosZ,
            Face::NegZ,
        ] {
            for ao in [[3, 3, 3, 3], [3, 0, 0, 3]] {
                // [3,0,0,3] triggers the flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
                let mesh = quads_to_mesh(&[unit_quad(face, ao)], &BlockTextureLayers::default())
                    .expect("one quad meshes");
                tri_normals_positive(&mesh, face.normal());
            }
        }
    }

    #[test]
    fn quad_geometry_spans_w_by_h_at_the_face_plane() {
        // +X quad at min (5,6,7), w=2 (along Y), h=3 (along Z): the face plane
        // sits at x = 6, y spans 6..8, z spans 7..10.
        let mesh = quads_to_mesh(
            &[unit_quad(Face::PosX, [3; 4])],
            &BlockTextureLayers::default(),
        )
        .unwrap();
        let pos: Vec<[u32; 3]> = packed_of(&mesh).iter().map(|v| v.pos).collect();
        assert!(pos.iter().all(|p| p[0] == 6));
        assert_eq!(pos.iter().map(|p| p[1]).min(), Some(6));
        assert_eq!(pos.iter().map(|p| p[1]).max(), Some(8));
        assert_eq!(pos.iter().map(|p| p[2]).min(), Some(7));
        assert_eq!(pos.iter().map(|p| p[2]).max(), Some(10));
    }

    fn packed_of(mesh: &Mesh) -> Vec<crate::renderer::meshing::pack::UnpackedVertex> {
        let VertexAttributeValues::Uint32x2(packed) = mesh
            .attribute(voxel_material::ATTRIBUTE_VOXEL_PACKED)
            .unwrap()
        else {
            panic!("packed attribute is Uint32x2")
        };
        packed
            .iter()
            .map(|&p| crate::renderer::meshing::pack::unpack(p))
            .collect()
    }

    #[test]
    fn packed_carries_ao_face_and_extent() {
        let mesh = quads_to_mesh(
            &[unit_quad(Face::PosY, [0, 1, 2, 3])],
            &BlockTextureLayers::default(),
        )
        .unwrap();
        let verts = packed_of(&mesh);
        assert_eq!(verts.len(), 4);
        for (i, v) in verts.iter().enumerate() {
            assert_eq!(
                v.ao as usize, i,
                "corner i carries ao level i for this quad"
            );
            assert_eq!(v.face, Face::PosY);
            assert_eq!((v.w, v.h), (2, 3));
        }
        // Corner order (cu, cv) = (0,0),(1,0),(0,1),(1,1).
        assert_eq!(
            verts.iter().map(|v| (v.cu, v.cv)).collect::<Vec<_>>(),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
    }

    #[test]
    fn packed_layer_uses_face_collapsed_triple() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for (face, expect) in [(Face::PosY, 10u32), (Face::NegX, 20), (Face::NegY, 30)] {
            let mesh = quads_to_mesh(&[unit_quad(face, [3; 4])], &layers).unwrap();
            assert!(packed_of(&mesh).iter().all(|v| v.layer == expect));
        }
    }

    #[test]
    fn packed_format_is_8_bytes_per_vertex() {
        let mesh = quads_to_mesh(
            &[unit_quad(Face::PosY, [3; 4])],
            &BlockTextureLayers::default(),
        )
        .unwrap();
        let total: usize = mesh
            .attributes()
            .map(|(_, values)| values.get_bytes().len())
            .sum();
        assert_eq!(total / mesh.count_vertices(), 8, "one Uint32x2 per vertex");
        assert_eq!(mesh.attributes().count(), 1, "exactly VOXEL_PACKED");
    }

    #[test]
    fn empty_quad_list_yields_none() {
        assert!(quads_to_mesh(&[], &BlockTextureLayers::default()).is_none());
    }
}
