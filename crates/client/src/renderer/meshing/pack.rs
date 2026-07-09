//! The packed voxel vertex format: two u32s per vertex, mirrored bit-for-bit
//! by the decode in assets/shaders/voxel_texture.wgsl. Layout (LOCKED):
//!   word0: x:6 | y:6 | z:6 | face:3 | ao:2 | cu:1 | cv:1   (bits 25-31 zero)
//!   word1: w:6 | h:6 | layer:16                            (bits 28-31 zero)
//! Rust packs (emit.rs) and unpacks (tests only); the shader only unpacks.

use super::binary::Face;

/// Decoded form of a packed vertex — the Rust mirror of the WGSL decode,
/// used by tests to assert on mesh content without float attributes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnpackedVertex {
    pub pos: [u32; 3],
    pub face: Face,
    pub ao: u8,
    pub cu: u32,
    pub cv: u32,
    pub w: u32,
    pub h: u32,
    pub layer: u32,
}

pub fn pack(
    pos: [u32; 3],
    face: Face,
    ao: u8,
    cu: u32,
    cv: u32,
    w: u32,
    h: u32,
    layer: u32,
) -> [u32; 2] {
    debug_assert!(pos.iter().all(|&c| c <= 32) && w <= 32 && h <= 32);
    debug_assert!(ao <= 3 && cu <= 1 && cv <= 1 && layer <= 0xFFFF);
    let w0 = pos[0]
        | pos[1] << 6
        | pos[2] << 12
        | face.index() << 18
        | (ao as u32) << 21
        | cu << 23
        | cv << 24;
    let w1 = w | h << 6 | layer << 12;
    [w0, w1]
}

pub fn unpack(p: [u32; 2]) -> UnpackedVertex {
    let [w0, w1] = p;
    UnpackedVertex {
        pos: [w0 & 63, (w0 >> 6) & 63, (w0 >> 12) & 63],
        face: Face::from_index((w0 >> 18) & 7),
        ao: ((w0 >> 21) & 3) as u8,
        cu: (w0 >> 23) & 1,
        cv: (w0 >> 24) & 1,
        w: w1 & 63,
        h: (w1 >> 6) & 63,
        layer: (w1 >> 12) & 0xFFFF,
    }
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

/// The AO shade the vertex shader computes: `0.55 + 0.15 * ao`. Must equal
/// `ao::AO_CURVE[ao]` (test-pinned) — the WGSL hardcodes the same formula.
pub fn shade(ao: u8) -> f32 {
    super::ao::AO_CURVE[ao as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ao::AO_CURVE;

    #[test]
    fn roundtrips_across_the_full_domain() {
        // Exhaustive on the small fields, boundary values on the big ones.
        for face_i in 0..6u32 {
            let face = Face::from_index(face_i);
            for ao in 0..4u8 {
                for (cu, cv) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
                    for pos in [[0u32, 0, 0], [32, 32, 32], [1, 17, 32]] {
                        for (w, h) in [(1u32, 1u32), (32, 32), (5, 17)] {
                            for layer in [0u32, 1, 255, 65535] {
                                let p = pack(pos, face, ao, cu, cv, w, h, layer);
                                let u = unpack(p);
                                assert_eq!(
                                    (u.pos, u.face, u.ao, u.cu, u.cv, u.w, u.h, u.layer),
                                    (pos, face, ao, cu, cv, w, h, layer)
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reserved_bits_stay_zero() {
        let p = pack([32, 32, 32], Face::NegZ, 3, 1, 1, 32, 32, 65535);
        assert_eq!(p[0] >> 25, 0, "word0 bits 25-31 reserved");
        assert_eq!(p[1] >> 28, 0, "word1 bits 28-31 reserved");
    }

    #[test]
    fn shade_formula_matches_ao_curve() {
        // The WGSL computes 0.55 + 0.15*ao; it must equal ao::AO_CURVE exactly
        // or baked AO silently diverges between CPU and shader.
        for ao in 0..4usize {
            assert_eq!(shade(ao as u8), AO_CURVE[ao]);
        }
    }

    #[test]
    fn layer_collapse_matches_the_normal_pick_semantics() {
        // Shader used to pick: top (index 0) when normal.y > 0, side (1) when
        // y == 0, bottom (2) when y < 0.
        let faces = [10, 20, 30];
        assert_eq!(layer_for_face(faces, Face::PosY), 10);
        assert_eq!(layer_for_face(faces, Face::NegY), 30);
        for f in [Face::PosX, Face::NegX, Face::PosZ, Face::NegZ] {
            assert_eq!(layer_for_face(faces, f), 20);
        }
    }
}
