//! Types of the baked LDraw stud-part assets (`assets/studs/parts.bin`, `colors.ron`) and of the
//! generated part catalog row (`stud_catalog.rs`). Written by `tools/ldraw-bake`, read by the client.
//!
//! Units: LDraw stud pitch 20 LDU = 8 sim units, plate 8 LDU, brick 24 LDU; 1 LDU = 0.4 sim units.

use serde::{Deserialize, Serialize};

/// Bump when the layout of [`BakedParts`] or its meaning changes.
pub const BAKED_PARTS_VERSION: u32 = 1;

/// One row of the generated part table (`stud_catalog::STUD_PARTS`). Not serialized.
#[derive(Debug, Clone, PartialEq)]
pub struct StudPartDef {
    /// LDraw part number as listed in `parts.txt` (an alias keeps its listed number).
    pub number: &'static str,
    pub name: &'static str,
    /// Unrotated bounding cells: studs x, plates y, studs z.
    pub size: [u8; 3],
    /// Cells `[x, size[1], z]` (the cell above the top) that carry a top stud.
    pub studs: &'static [[u8; 3]],
    /// Convex-hull points in LDU relative to the bounding-box min corner, Y up.
    /// Empty = box part (collides as its cells).
    pub hull: &'static [[i16; 3]],
}

/// The whole of `parts.bin`; `parts[i]` is `STUD_PARTS[i]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakedParts {
    pub version: u32,
    pub parts: Vec<BakedPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakedPart {
    pub number: String,
    pub hi: BakedLod,
    pub lo: BakedLod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakedLod {
    pub tris: Vec<BakedTri>,
    pub edges: Vec<BakedEdge>,
}

/// Positions in sim units, part-local, origin at the bbox min corner, Y up, CCW front faces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakedTri {
    pub pos: [[f32; 3]; 3],
    pub normal: [f32; 3],
    pub color: TriColor,
    pub tag: TriTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriColor {
    /// LDraw colour 16: the part's own colour.
    Own,
    /// LDraw colour 24: the edge colour.
    Edge,
    Fixed(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FaceDir {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriTag {
    /// Lies on a face of the part's bounding box.
    Face(FaceDir),
    /// A top stud on the given cell.
    Stud([u8; 3]),
    /// Underside tubes, hollow, anything inside.
    Inner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BakedEdge {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub tag: EdgeTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeTag {
    /// On one or two bbox faces: bit per [`FaceDir`] (`1 << dir as u8`).
    Faces(u8),
    Stud([u8; 3]),
    Inner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudColor {
    pub code: u16,
    pub name: String,
    pub rgb: [u8; 3],
    pub edge: [u8; 3],
    pub alpha: u8,
    pub finish: Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Finish {
    Solid,
    Transparent,
    Chrome,
    Pearl,
    Metal,
    Rubber,
    Other,
}
