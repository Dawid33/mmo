//! Per-quad 8-byte records for the pulled (mesh-free) render path.
//! Layout (LOCKED, mirrored by assets/shaders/voxel_pull.wgsl — the shader
//! only decodes; this file is the source of truth):
//!   w0: x:6 | y:6 | z:6 | face:3 | ao0:2 | ao1:2 | ao2:2 | ao3:2 | flip:1  (bits 30-31 zero)
//!   w1: w:6 | h:6 | layer:16                                               (bits 28-31 zero)
//! x/y/z/face sit at the same bit positions as pack.rs word0, and w1 equals
//! pack.rs word1 exactly, so the two formats stay diffable while both live.

use super::super::block_textures::BlockTextureLayers;
use super::ao::ao_flips_quad;
use super::binary::{cross_axes, Face, Quad};
use super::pack;

#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(transparent)]
pub struct QuadRecord(pub [u32; 2]);

/// Slot→corner tables indexed by `reversed*2 + flip`; corner order is
/// (0,0),(1,0),(0,1),(1,1) over (U,V). These are emit::quad_indices' four
/// patterns verbatim — the parity test enforces that they never drift.
pub const CORNER_SLOTS: [[u32; 6]; 4] = [
    [0, 1, 3, 0, 3, 2], // default winding, default diagonal
    [0, 1, 2, 1, 3, 2], // default winding, AO-flipped diagonal
    [0, 3, 1, 0, 2, 3], // reversed winding (NegX/PosY/NegZ), default diagonal
    [0, 2, 1, 1, 2, 3], // reversed winding, AO-flipped diagonal
];

pub fn pack_record(q: &Quad, layer: u32) -> QuadRecord {
    // The face plane sits at min[axis] for negative faces, +1 for positive —
    // same origin rule as emit::quads_to_mesh.
    let mut origin = q.min;
    if q.face.positive() {
        origin[q.face.axis()] += 1;
    }
    debug_assert!(origin.iter().all(|&c| c <= 33) && q.w <= 33 && q.h <= 33);
    debug_assert!(q.ao.iter().all(|&a| a <= 3) && layer <= 0xFFFF);
    let flip = ao_flips_quad(&q.ao) as u32;
    let w0 = origin[0]
        | origin[1] << 6
        | origin[2] << 12
        | q.face.index() << 18
        | (q.ao[0] as u32) << 21
        | (q.ao[1] as u32) << 23
        | (q.ao[2] as u32) << 25
        | (q.ao[3] as u32) << 27
        | flip << 29;
    let w1 = q.w | q.h << 6 | layer << 12;
    QuadRecord([w0, w1])
}

pub fn quads_to_records(quads: &[Quad], layers: &BlockTextureLayers) -> Vec<QuadRecord> {
    quads
        .iter()
        .map(|q| {
            let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
            pack_record(q, pack::layer_for_face(faces, q.face))
        })
        .collect()
}

/// Decoded corner — the Rust mirror of the WGSL reconstruction, used by tests
/// (and only tests) as the bridge between record bits and expected geometry.
pub struct DecodedCorner {
    pub pos: [u32; 3],
    pub face: Face,
    pub ao: u8,
    pub layer: u32,
}

pub fn decode_corner(rec: QuadRecord, slot: u32) -> DecodedCorner {
    let [w0, w1] = rec.0;
    let face_id = (w0 >> 18) & 7;
    let flip = (w0 >> 29) & 1;
    // Reversed faces: NegX (1), PosY (2), NegZ (5) — from emit::quad_indices.
    let reversed = matches!(face_id, 1 | 2 | 5) as u32;
    let corner = CORNER_SLOTS[(reversed * 2 + flip) as usize][slot as usize];
    let (cu, cv) = (corner & 1, corner >> 1);
    let face = Face::from_index(face_id);
    let (ua, va) = cross_axes(face.axis());
    let mut pos = [w0 & 63, (w0 >> 6) & 63, (w0 >> 12) & 63];
    pos[ua] += cu * (w1 & 63);
    pos[va] += cv * ((w1 >> 6) & 63);
    DecodedCorner {
        pos,
        face,
        ao: ((w0 >> (21 + 2 * corner)) & 3) as u8,
        layer: (w1 >> 12) & 0xFFFF,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::binary::{Face, Quad};
    use super::super::emit;
    use super::super::pack;
    use super::super::super::block_textures::BlockTextureLayers;
    use bevy::mesh::{Indices, VertexAttributeValues};
    use game::BlockId;

    fn sample_quad(face: Face, ao: [u8; 4]) -> Quad {
        Quad { min: [5, 6, 7], w: 2, h: 3, face, block: BlockId(1), ao }
    }

    #[test]
    fn record_decode_matches_the_indexed_mesh_oracle() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for face in [Face::PosX, Face::NegX, Face::PosY, Face::NegY, Face::PosZ, Face::NegZ] {
            // [3,0,0,3] triggers the AO flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
            for ao in [[3u8, 3, 3, 3], [3, 0, 0, 3]] {
                let q = sample_quad(face, ao);
                let mesh = emit::quads_to_mesh(&[q.clone()], &layers).expect("one quad meshes");
                let VertexAttributeValues::Uint32x2(packed) = mesh
                    .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
                    .unwrap()
                else { panic!("packed attribute") };
                let Indices::U32(idx) = mesh.indices().unwrap() else { panic!("u32 indices") };
                let rec = quads_to_records(&[q], &layers)[0];
                for (slot, &i) in idx.iter().enumerate() {
                    let oracle = pack::unpack(packed[i as usize]);
                    let got = decode_corner(rec, slot as u32);
                    assert_eq!(got.pos, oracle.pos, "{face:?} ao={ao:?} slot {slot}: position");
                    assert_eq!(got.ao, oracle.ao, "{face:?} ao={ao:?} slot {slot}: ao");
                    assert_eq!(got.layer, oracle.layer, "{face:?} slot {slot}: layer");
                    assert_eq!(got.face, face, "face survives the roundtrip");
                }
            }
        }
    }

    #[test]
    fn reserved_bits_stay_zero() {
        let q = sample_quad(Face::NegZ, [3, 0, 0, 3]);
        let rec = quads_to_records(&[q], &BlockTextureLayers::default())[0];
        assert_eq!(rec.0[0] >> 30, 0, "w0 bits 30-31 reserved");
        assert_eq!(rec.0[1] >> 28, 0, "w1 bits 28-31 reserved");
    }

    #[test]
    fn eight_bytes_per_quad() {
        assert_eq!(std::mem::size_of::<QuadRecord>(), 8);
    }
}
