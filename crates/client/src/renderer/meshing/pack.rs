//! The packed voxel vertex format: two u32s per vertex, mirrored bit-for-bit
//! by the decode in assets/shaders/voxel_texture.wgsl. Layout (LOCKED):
//!   word0: x:9 | y:9 | z:9 | face:3 | ao:2                 (all 32 bits)
//!   word1: w:6 | h:6 | layer:16                            (bits 28-31 zero)
//! Positions are 9 bits so a merged sim-chunk mesh (`chunk_mesh.rs`: every
//! brick's quads offset by `local * BRICK_VOXELS`, up to 7·32 + 33 = 257) fits
//! as well as a single brick or LOD brick (0..=33). The shader derives UVs from
//! the decoded position (block-cell UVs) and does not read w/h; they stay
//! packed for tests. (Until 2026-11-02 word0 was x:6|y:6|z:6|face:3|ao:2|cu:1|cv:1.)
//! Rust packs (emit.rs, chunk_mesh.rs) and unpacks (tests only); the shader only unpacks.

use super::binary::Face;

/// Largest packed coordinate (9 bits).
pub const POS_MAX: u32 = 511;

/// Decoded form of a packed vertex — the Rust mirror of the WGSL decode,
/// used by tests to assert on mesh content without float attributes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnpackedVertex {
    pub pos: [u32; 3],
    pub face: Face,
    pub ao: u8,
    pub w: u32,
    pub h: u32,
    pub layer: u32,
}

pub fn pack(pos: [u32; 3], face: Face, ao: u8, w: u32, h: u32, layer: u32) -> [u32; 2] {
    debug_assert!(pos.iter().all(|&c| c <= POS_MAX) && w <= 63 && h <= 63);
    debug_assert!(ao <= 3 && layer <= 0xFFFF);
    let w0 = pos[0] | pos[1] << 9 | pos[2] << 18 | face.index() << 27 | (ao as u32) << 30;
    let w1 = w | h << 6 | layer << 12;
    [w0, w1]
}

pub fn unpack(p: [u32; 2]) -> UnpackedVertex {
    let [w0, w1] = p;
    UnpackedVertex {
        pos: [w0 & POS_MAX, (w0 >> 9) & POS_MAX, (w0 >> 18) & POS_MAX],
        face: Face::from_index((w0 >> 27) & 7),
        ao: ((w0 >> 30) & 3) as u8,
        w: w1 & 63,
        h: (w1 >> 6) & 63,
        layer: (w1 >> 12) & 0xFFFF,
    }
}

/// `p` moved by `offset` (in voxels): a brick-local vertex made chunk-local.
pub fn offset(p: [u32; 2], offset: [u32; 3]) -> [u32; 2] {
    let mut v = unpack(p);
    for a in 0..3 {
        v.pos[a] += offset[a];
    }
    pack(v.pos, v.face, v.ao, v.w, v.h, v.layer)
}

/// Collapse the per-block `[top, side, bottom]` layer triple to the single
/// layer this quad's face shows (same semantics as the old shader's
/// normal-based pick).
pub fn layer_for_face(faces: [u32; 3], face: Face) -> u32 {
    match face {
        Face::PosY => faces[0],
        Face::NegY => faces[2],
        _ => faces[1],
    }
}

/// The AO shade the vertex shader computes: `0.55 + 0.15 * ao`. Must match
/// `ao::AO_CURVE[ao]` within 1e-6 (test-pinned; the f32 sum rounds one ulp
/// off the curve literals) — the WGSL hardcodes the same formula.
pub fn shade(ao: u8) -> f32 {
    0.55 + 0.15 * ao as f32
}

