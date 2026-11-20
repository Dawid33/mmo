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
use super::binary::{Face, Quad, cross_axes};
use super::pack;

/// Index patterns by (winding reversed?, ao flipped?). Corner order is
/// (0,0), (1,0), (0,1), (1,1) over (U, V); default diagonal splits 0-3,
/// flipped splits 1-2. Reversed faces (NegX, PosY, NegZ — where U×V points
/// inward) reverse each triangle. Verified by the winding test.
pub(crate) fn quad_indices(face: Face, flipped: bool) -> [u32; 6] {
    let reversed = matches!(face, Face::NegX | Face::PosY | Face::NegZ);
    match (reversed, flipped) {
        (false, false) => [0, 1, 3, 0, 3, 2],
        (false, true) => [0, 1, 2, 1, 3, 2],
        (true, false) => [0, 3, 1, 0, 2, 3],
        (true, true) => [0, 2, 1, 1, 2, 3],
    }
}

/// Corner order of a quad's four vertices, (cu, cv) over (U, V).
pub(crate) const CORNERS: [(u32, u32); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];

/// A terrain mesh's vertex + index data, before it becomes a Bevy `Mesh`:
/// what a compute worker sends back (`renderer::compute`; a `Mesh` does not
/// serialize), wrapped into a `Mesh` by [`MeshData::into_mesh`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeshData {
    pub packed: Vec<[u32; 2]>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn into_mesh(self) -> Mesh {
        // CPU copy freed after GPU extraction; colliders come from rapier, tests never extract
        // (MinimalPlugins), and the entity Aabb is inserted constant by `apply_meshed_chunks`
        // so nothing reads mesh data main-world after upload.
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(
            voxel_material::ATTRIBUTE_VOXEL_PACKED,
            VertexAttributeValues::Uint32x2(self.packed),
        );
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

pub fn quads_to_mesh(quads: &[Quad], layers: &BlockTextureLayers) -> Option<Mesh> {
    quads_to_mesh_data(quads, layers).map(MeshData::into_mesh)
}

/// [`quads_to_mesh`]'s data (`None` = no quads).
pub fn quads_to_mesh_data(quads: &[Quad], layers: &BlockTextureLayers) -> Option<MeshData> {
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
        for (i, (cu, cv)) in CORNERS.into_iter().enumerate() {
            let mut pos = origin;
            pos[ua] += cu * q.w;
            pos[va] += cv * q.h;
            packed.push(pack::pack(pos, q.face, q.ao[i], q.w, q.h, layer));
        }
    }

    Some(MeshData { packed, indices })
}
