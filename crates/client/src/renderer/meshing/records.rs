//! Per-quad 8-byte records for the pulled (mesh-free) render path.
//! Layout (LOCKED, mirrored by assets/shaders/voxel_pull.wgsl — the shader
//! only decodes; this file is the source of truth):
//!   w0: x:6 | y:6 | z:6 | face:3 | ao0:2 | ao1:2 | ao2:2 | ao3:2 | flip:1 | cutout:1 | interior:1
//!   w1: w:6 | h:6 | layer:11 | bx:3 | by:3 | bz:3
//! x/y/z are brick-local (0..=33); `bx/by/bz` place the brick inside a MERGED
//! record list — a sim chunk's 8³ bricks or a LOD tile's 8³ cells
//! (`chunk_mesh`, `lod_tiles`) — so the shader adds `b * 32` (0 for a lone
//! brick). Layers are block-texture array layers (56 in 2026-11; 11 bits = 2048).
//! w1 equals pack.rs word1 exactly; w0 differs (pack.rs has 9-bit positions
//! for merged sim-chunk meshes). `chunk_mesh::records_to_mesh_data` expands
//! records into pack.rs vertices for the WebGL2 per-chunk path.
//! `cutout` (bit 30) marks an alpha-tested quad (`Quad::cutout`, leaves); a
//! chunk's records put every opaque quad first, then the cutout ones, and
//! `face_ends` entry 7 stores where the cutout range starts. `interior`
//! (bit 31) marks a quad facing into a dug space (`Quad::interior`): the
//! cut-off x-ray view draws those into its interior depth pass and opens the
//! terrain in front of them (`voxel_pull/xray.rs`). It is not a sort key.

use super::super::block_textures::BlockTextureLayers;
use super::ao::ao_flips_quad;
use super::binary::{Face, Quad, cross_axes};
use super::pack;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    bytemuck::Pod,
    bytemuck::Zeroable,
    serde::Serialize,
    serde::Deserialize,
)]
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
pub const CORNER_PERM: [[u32; 4]; 4] = [[1, 3, 0, 2], [0, 1, 2, 3], [1, 0, 3, 2], [0, 2, 1, 3]];

pub fn pack_record(q: &Quad, layer: u32) -> QuadRecord {
    // The face plane sits at min[axis] for negative faces, +1 for positive —
    // same origin rule as emit::quads_to_mesh.
    let mut origin = q.min;
    if q.face.positive() {
        origin[q.face.axis()] += 1;
    }
    debug_assert!(origin.iter().all(|&c| c <= 33) && q.w <= 33 && q.h <= 33);
    debug_assert!(q.ao.iter().all(|&a| a <= 3) && layer <= LAYER_MAX);
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
        | (q.cutout as u32) << CUTOUT_BIT
        | (q.interior as u32) << INTERIOR_BIT;
    let w1 = q.w | q.h << 6 | layer << 12;
    QuadRecord([w0, w1])
}

/// Largest texture layer a record holds (11 bits).
pub const LAYER_MAX: u32 = 0x7FF;
/// w1 bit where the 9-bit brick placement (`bx | by << 3 | bz << 6`) starts.
pub const BRICK_SHIFT: u32 = 23;

/// `rec` placed at brick `local` (each axis 0..8) of a merged record list.
pub fn with_brick(rec: QuadRecord, local: [u8; 3]) -> QuadRecord {
    debug_assert!(local.iter().all(|&c| c < 8));
    let brick = local[0] as u32 | (local[1] as u32) << 3 | (local[2] as u32) << 6;
    let w1 = rec.0[1] & !(0x1FF << BRICK_SHIFT) | brick << BRICK_SHIFT;
    QuadRecord([rec.0[0], w1])
}

/// The brick placement of a record (each axis 0..8).
pub fn record_brick(rec: QuadRecord) -> [u32; 3] {
    let b = rec.0[1] >> BRICK_SHIFT;
    [b & 7, (b >> 3) & 7, (b >> 6) & 7]
}

/// Merge bricks' record lists into one, each placed at its brick, in
/// `quads_to_records` order (opaque then cutout, each by face), as
/// `face_ends` needs — the shader draws the opaque and cutout ranges with
/// different pipelines, so an unsorted merge would draw leaves opaque.
pub fn merge_records<'a>(
    parts: impl IntoIterator<Item = ([u8; 3], &'a [QuadRecord])>,
) -> Vec<QuadRecord> {
    let mut out: Vec<QuadRecord> = Vec::new();
    for (local, records) in parts {
        out.extend(records.iter().map(|&r| with_brick(r, local)));
    }
    out.sort_by_key(|r| record_order(*r));
    out
}

/// w0 bit of the cutout flag.
pub const CUTOUT_BIT: u32 = 30;
/// w0 bit of the interior (faces dug space) flag.
pub const INTERIOR_BIT: u32 = 31;

/// Whether a record faces into a dug space (w0 bit `INTERIOR_BIT`).
pub fn record_is_interior(rec: QuadRecord) -> bool {
    rec.0[0] >> INTERIOR_BIT & 1 == 1
}

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
    let ordered = records
        .windows(2)
        .all(|w| record_order(w[0]) <= record_order(w[1]));
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
    let brick = record_brick(rec);
    let mut pos = [0, 1, 2].map(|a| ((w0 >> (6 * a)) & 63) + brick[a] * 32);
    pos[ua] += cu * (w1 & 63);
    pos[va] += cv * ((w1 >> 6) & 63);
    DecodedCorner {
        pos,
        face,
        ao: ((w0 >> (21 + 2 * corner)) & 3) as u8,
        layer: (w1 >> 12) & LAYER_MAX,
    }
}
