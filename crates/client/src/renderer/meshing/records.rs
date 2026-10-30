//! Per-quad 8-byte records for the pulled (mesh-free) render path.
//! Layout (LOCKED, mirrored by assets/shaders/voxel_pull.wgsl — the shader
//! only decodes; this file is the source of truth):
//!   w0: x:6 | y:6 | z:6 | face:3 | ao0:2 | ao1:2 | ao2:2 | ao3:2 | flip:1 | cutout:1  (bit 31 zero)
//!   w1: w:6 | h:6 | layer:16                                                          (bits 28-31 zero)
//! x/y/z/face sit at the same bit positions as pack.rs word0, and w1 equals
//! pack.rs word1 exactly, so the two formats stay diffable while both live.
//! `cutout` (bit 30) marks an alpha-tested quad (`Quad::cutout`, leaves); a
//! chunk's records put every opaque quad first, then the cutout ones, and
//! `face_ends` entry 7 stores where the cutout range starts.

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

/// Index pattern shared by every quad of the indexed pulled draw: four
/// vertices per quad (`vertex_index = quad * 4 + local`), two triangles split
/// along the local 1–2 diagonal. One static index buffer repeats it.
pub const QUAD_INDEX_PATTERN: [u32; 6] = [0, 1, 2, 2, 1, 3];

/// Local vertex (0..4) → corner, per `reversed*2 + flip` variant: the
/// permutation that makes `QUAD_INDEX_PATTERN` produce exactly the triangles
/// (and winding) of the matching `CORNER_SLOTS` row. Indexed drawing shades 4
/// vertices per quad instead of 6. Mirrored in voxel_pull.wgsl and
/// voxel_pull_depth.wgsl; `indexed_corners_rebuild_the_slot_triangles` pins it.
pub const CORNER_PERM: [[u32; 4]; 4] = [
    [1, 3, 0, 2],
    [0, 1, 2, 3],
    [1, 0, 3, 2],
    [0, 2, 1, 3],
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
        | flip << 29
        | (q.cutout as u32) << CUTOUT_BIT;
    let w1 = q.w | q.h << 6 | layer << 12;
    QuadRecord([w0, w1])
}

/// w0 bit of the cutout flag.
pub const CUTOUT_BIT: u32 = 30;

/// Records come out opaque first, then cutout (drawn by their own pipeline),
/// each part grouped by face id (`Face::index()` order, stable within a
/// face), so the GPU cull can draw each opaque face direction as its own
/// range and skip the ones facing away from the camera — see `face_ends`.
pub fn quads_to_records(quads: &[Quad], layers: &BlockTextureLayers) -> Vec<QuadRecord> {
    let mut records: Vec<QuadRecord> = quads
        .iter()
        .map(|q| {
            let faces = layers.0.get(&q.block).copied().unwrap_or([0, 0, 0]);
            pack_record(q, pack::layer_for_face(faces, q.face))
        })
        .collect();
    records.sort_by_key(|r| record_order(*r));
    records
}

/// Whether a record is drawn alpha-tested (w0 bit `CUTOUT_BIT`).
pub fn record_is_cutout(rec: QuadRecord) -> bool {
    rec.0[0] >> CUTOUT_BIT & 1 == 1
}

/// Sort key of `quads_to_records`: cutout after opaque, then face.
fn record_order(rec: QuadRecord) -> u32 {
    (record_is_cutout(rec) as u32) << 3 | record_face(rec)
}

/// The face id (`Face::index()`) packed into a record's w0 bits 18..21.
pub fn record_face(rec: QuadRecord) -> u32 {
    (rec.0[0] >> 18) & 7
}

/// Cumulative end of each OPAQUE face's range in `quads_to_records` order:
/// face `f` occupies `ends[f-1]..ends[f]` (with `ends[-1]` = 0). Entry 6 is a
/// flag: records NOT in that order (a producer that skipped
/// `quads_to_records`) set it, and are then drawn whole, all opaque, with no
/// face culling. Entry 7 is where the cutout range starts (it runs to the
/// end; `len` when there is none) — equal to `ends[5]` when ordered.
pub fn face_ends(records: &[QuadRecord]) -> [u32; 8] {
    let len = records.len() as u32;
    let ordered = records.windows(2).all(|w| record_order(w[0]) <= record_order(w[1]));
    if !ordered {
        return [len, len, len, len, len, len, 1, len];
    }
    let mut ends = [0u32; 8];
    for rec in records.iter().filter(|r| !record_is_cutout(**r)) {
        let face = record_face(*rec) as usize;
        if face < 6 {
            ends[face] += 1;
        }
    }
    for f in 1..6 {
        ends[f] += ends[f - 1];
    }
    ends[6] = 0;
    ends[7] = ends[5];
    ends
}

/// Where a chunk's cutout records start (`face_ends` entry 7).
pub fn cutout_start(records: &[QuadRecord]) -> u32 {
    face_ends(records)[7]
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
    use super::super::super::block_textures::BlockTextureLayers;
    use super::super::binary::{Face, Quad};
    use super::super::emit;
    use super::super::pack;
    use super::*;
    use bevy::mesh::{Indices, VertexAttributeValues};
    use game::BlockId;

    fn sample_quad(face: Face, ao: [u8; 4]) -> Quad {
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

    #[test]
    fn record_decode_matches_the_indexed_mesh_oracle() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for face in [
            Face::PosX,
            Face::NegX,
            Face::PosY,
            Face::NegY,
            Face::PosZ,
            Face::NegZ,
        ] {
            // [3,0,0,3] triggers the AO flip rule (ao[0]+ao[3] > ao[1]+ao[2]).
            for ao in [[3u8, 3, 3, 3], [3, 0, 0, 3]] {
                let q = sample_quad(face, ao);
                let mesh = emit::quads_to_mesh(&[q.clone()], &layers).expect("one quad meshes");
                let VertexAttributeValues::Uint32x2(packed) = mesh
                    .attribute(crate::renderer::voxel_material::ATTRIBUTE_VOXEL_PACKED)
                    .unwrap()
                else {
                    panic!("packed attribute")
                };
                let Indices::U32(idx) = mesh.indices().unwrap() else {
                    panic!("u32 indices")
                };
                let rec = quads_to_records(&[q], &layers)[0];
                for (slot, &i) in idx.iter().enumerate() {
                    let oracle = pack::unpack(packed[i as usize]);
                    let got = decode_corner(rec, slot as u32);
                    assert_eq!(
                        got.pos, oracle.pos,
                        "{face:?} ao={ao:?} slot {slot}: position"
                    );
                    assert_eq!(got.ao, oracle.ao, "{face:?} ao={ao:?} slot {slot}: ao");
                    assert_eq!(got.layer, oracle.layer, "{face:?} slot {slot}: layer");
                    assert_eq!(got.face, face, "face survives the roundtrip");
                }
            }
        }
    }

    #[test]
    fn records_are_grouped_by_face_with_matching_ends() {
        let layers = BlockTextureLayers::default();
        let quads: Vec<Quad> = [Face::NegZ, Face::PosX, Face::PosY, Face::PosX, Face::NegZ]
            .into_iter()
            .map(|f| sample_quad(f, [3, 3, 3, 3]))
            .collect();
        let records = quads_to_records(&quads, &layers);
        let faces: Vec<u32> = records.iter().map(|r| record_face(*r)).collect();
        assert_eq!(faces, vec![0, 0, 2, 5, 5]);
        // PosX 0..2, NegX empty, PosY 2..3, NegY/PosZ empty, NegZ 3..5; no cutout.
        assert_eq!(face_ends(&records), [2, 2, 3, 3, 3, 5, 0, 5]);
    }

    #[test]
    fn ungrouped_records_fall_back_to_one_whole_range() {
        let layers = BlockTextureLayers::default();
        let a = quads_to_records(&[sample_quad(Face::NegZ, [3, 3, 3, 3])], &layers)[0];
        let b = quads_to_records(&[sample_quad(Face::PosX, [3, 3, 3, 3])], &layers)[0];
        assert_eq!(face_ends(&[a, b]), [2, 2, 2, 2, 2, 2, 1, 2], "flagged: draw whole, opaque");
    }

    #[test]
    fn indexed_corners_rebuild_the_slot_triangles() {
        // A triangle up to rotation (rotation keeps the winding).
        fn canon(t: [u32; 3]) -> [u32; 3] {
            let i = (0..3).min_by_key(|&i| t[i]).unwrap();
            [t[i], t[(i + 1) % 3], t[(i + 2) % 3]]
        }
        for (variant, slots) in CORNER_SLOTS.iter().enumerate() {
            let perm = CORNER_PERM[variant];
            let tri = |k: usize| {
                canon([
                    perm[QUAD_INDEX_PATTERN[k] as usize],
                    perm[QUAD_INDEX_PATTERN[k + 1] as usize],
                    perm[QUAD_INDEX_PATTERN[k + 2] as usize],
                ])
            };
            let mut got = [tri(0), tri(3)];
            let mut want = [
                canon([slots[0], slots[1], slots[2]]),
                canon([slots[3], slots[4], slots[5]]),
            ];
            got.sort();
            want.sort();
            assert_eq!(got, want, "variant {variant}");
        }
    }

    #[test]
    fn reserved_bits_stay_zero() {
        for cutout in [false, true] {
            let q = Quad { cutout, ..sample_quad(Face::NegZ, [3, 0, 0, 3]) };
            let rec = quads_to_records(&[q], &BlockTextureLayers::default())[0];
            assert_eq!(rec.0[0] >> 31, 0, "w0 bit 31 reserved");
            assert_eq!(rec.0[1] >> 28, 0, "w1 bits 28-31 reserved");
        }
    }

    #[test]
    fn cutout_flag_round_trips() {
        let mut layers = BlockTextureLayers::default();
        layers.0.insert(BlockId(1), [10, 20, 30]);
        for face in [Face::PosX, Face::NegY, Face::NegZ] {
            for ao in [[3u8, 3, 3, 3], [3, 0, 0, 3]] {
                let opaque = sample_quad(face, ao);
                let cut = Quad { cutout: true, ..opaque.clone() };
                let a = quads_to_records(&[opaque], &layers)[0];
                let b = quads_to_records(&[cut], &layers)[0];
                assert!(!record_is_cutout(a) && record_is_cutout(b), "{face:?}");
                assert_eq!(a.0[0] | 1 << CUTOUT_BIT, b.0[0], "only the flag bit differs");
                assert_eq!(a.0[1], b.0[1], "w1 (pack.rs word1 parity) is untouched");
                for slot in 0..6 {
                    let (da, db) = (decode_corner(a, slot), decode_corner(b, slot));
                    assert_eq!((da.pos, da.face, da.ao, da.layer), (db.pos, db.face, db.ao, db.layer));
                }
            }
        }
    }

    #[test]
    fn cutout_records_follow_the_opaque_ones() {
        let layers = BlockTextureLayers::default();
        let q = |face, cutout| Quad { cutout, ..sample_quad(face, [3, 3, 3, 3]) };
        let records = quads_to_records(
            &[q(Face::NegZ, true), q(Face::PosY, false), q(Face::PosX, true), q(Face::NegZ, false)],
            &layers,
        );
        let order: Vec<(bool, u32)> = records.iter().map(|r| (record_is_cutout(*r), record_face(*r))).collect();
        assert_eq!(order, vec![(false, 2), (false, 5), (true, 0), (true, 5)]);
        // Opaque PosY 0..1, NegZ 1..2; the cutout range starts at 2.
        assert_eq!(face_ends(&records), [0, 0, 1, 1, 1, 2, 0, 2]);
        assert_eq!(cutout_start(&records), 2);
    }

    #[test]
    fn eight_bytes_per_quad() {
        assert_eq!(std::mem::size_of::<QuadRecord>(), 8);
    }
}
