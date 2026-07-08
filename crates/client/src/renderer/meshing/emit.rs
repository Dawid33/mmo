//! Quad list → Bevy `Mesh`. Vertex format unchanged from the block-mesh era
//! (POSITION/NORMAL/UV_0/COLOR/TEX_INDEX) except COLOR now carries baked AO
//! shading instead of constant white — the voxel shader already multiplies
//! vertex color, so AO renders with no shader change. Phase 2 replaces this
//! format wholesale (packed u32 attributes).

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};

use super::super::block_textures::BlockTextureLayers;
use super::super::voxel_material;
use super::ao::{ao_flips_quad, AO_CURVE};
use super::binary::{cross_axes, Face, Quad};

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
    let mut positions = Vec::with_capacity(n);
    let mut normals = Vec::with_capacity(n);
    let mut uvs = Vec::with_capacity(n);
    let mut colors = Vec::with_capacity(n);
    let mut tex_indices: Vec<[u32; 3]> = Vec::with_capacity(n);
    let mut indices = Vec::with_capacity(quads.len() * 6);

    for q in quads {
        let axis = q.face.axis();
        let (ua, va) = cross_axes(axis);
        let base = positions.len() as u32;
        indices.extend(quad_indices(q.face, ao_flips_quad(&q.ao)).map(|i| base + i));

        // The face plane sits at min[axis] for negative faces, +1 for positive.
        let mut origin = [q.min[0] as f32, q.min[1] as f32, q.min[2] as f32];
        if q.face.positive() {
            origin[axis] += 1.0;
        }
        for (i, (cu, cv)) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
            let mut pos = origin;
            pos[ua] += (cu * q.w) as f32;
            pos[va] += (cv * q.h) as f32;
            positions.push(pos);
            normals.push(q.face.normal());
            uvs.push([(cu * q.w) as f32, (cv * q.h) as f32]);
            let shade = AO_CURVE[q.ao[i] as usize];
            colors.push([shade, shade, shade, 1.0]);
        }
        let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
        tex_indices.extend(std::iter::repeat(faces).take(4));
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, VertexAttributeValues::Float32x3(positions));
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, VertexAttributeValues::Float32x3(normals));
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, VertexAttributeValues::Float32x4(colors));
    mesh.insert_attribute(voxel_material::ATTRIBUTE_TEX_INDEX, VertexAttributeValues::Uint32x3(tex_indices));
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use game::BlockId;

    fn unit_quad(face: Face, ao: [u8; 4]) -> Quad {
        Quad { min: [5, 6, 7], w: 2, h: 3, face, block: BlockId(1), ao }
    }

    fn tri_normals_positive(mesh: &Mesh, expected_normal: [f32; 3]) {
        let VertexAttributeValues::Float32x3(pos) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions are Float32x3")
        };
        let Indices::U32(idx) = mesh.indices().unwrap() else { panic!("u32 indices") };
        let n = Vec3::from(expected_normal);
        for tri in idx.chunks(3) {
            let [a, b, c] = [pos[tri[0] as usize], pos[tri[1] as usize], pos[tri[2] as usize]];
            let cross = (Vec3::from(b) - Vec3::from(a)).cross(Vec3::from(c) - Vec3::from(a));
            assert!(cross.dot(n) > 0.0, "triangle {tri:?} winds against its normal");
        }
    }

    #[test]
    fn winding_faces_outward_for_all_six_faces_default_and_flipped() {
        for face in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ] {
            for ao in [[3, 3, 3, 3], [3, 0, 0, 3]] {
                // [3,0,0,3] triggers the flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
                let mesh =
                    quads_to_mesh(&[unit_quad(face, ao)], &BlockTextureLayers::default())
                        .expect("one quad meshes");
                tri_normals_positive(&mesh, face.normal());
            }
        }
    }

    #[test]
    fn quad_geometry_spans_w_by_h_at_the_face_plane() {
        // +X quad at min (5,6,7), w=2 (along Y), h=3 (along Z): the face plane
        // sits at x = 6, y spans 6..8, z spans 7..10.
        let mesh = quads_to_mesh(&[unit_quad(Face::PosX, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x3(pos) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!()
        };
        assert!(pos.iter().all(|p| p[0] == 6.0));
        assert_eq!(pos.iter().map(|p| p[1] as i32).min(), Some(6));
        assert_eq!(pos.iter().map(|p| p[1] as i32).max(), Some(8));
        assert_eq!(pos.iter().map(|p| p[2] as i32).min(), Some(7));
        assert_eq!(pos.iter().map(|p| p[2] as i32).max(), Some(10));
    }

    #[test]
    fn ao_lands_in_vertex_color_via_curve() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [0, 1, 2, 3])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x4(colors) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
        else {
            panic!()
        };
        for (i, c) in colors.iter().enumerate() {
            let expect = AO_CURVE[i]; // corner i has ao level i in this quad
            assert_eq!(c[..3], [expect, expect, expect]);
            assert_eq!(c[3], 1.0);
        }
    }

    #[test]
    fn uvs_tile_by_quad_extent() {
        let mesh = quads_to_mesh(&[unit_quad(Face::PosY, [3; 4])], &BlockTextureLayers::default())
            .unwrap();
        let VertexAttributeValues::Float32x2(uvs) =
            mesh.attribute(Mesh::ATTRIBUTE_UV_0).unwrap()
        else {
            panic!()
        };
        assert_eq!(uvs, &vec![[0.0, 0.0], [2.0, 0.0], [0.0, 3.0], [2.0, 3.0]]);
    }

    #[test]
    fn empty_quad_list_yields_none() {
        assert!(quads_to_mesh(&[], &BlockTextureLayers::default()).is_none());
    }
}
