//! The prefab file form: what `<name>.prefab.ron` and an export string hold.

use serde::{Deserialize, Serialize};

/// The file format this build writes and reads.
pub const PREFAB_FORMAT: u16 = 1;
/// Longest prefab name, in bytes.
pub const PREFAB_NAME_MAX: usize = 48;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefabFile {
    pub format: u16,
    pub name: String,
    pub meta: PrefabMeta,
    pub kind: PrefabKindFile,
}

/// Free text about a prefab; never read by the sim.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrefabMeta {
    pub author: String,
    pub note: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PrefabKindFile {
    Build(BuildFile),
    Prop(PropFile),
}

/// Grid bricks merged into terrain. Cells are `[stud x, plate y, stud z]` relative to
/// the anchor and may be negative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildFile {
    pub parts: Vec<BuildPartFile>,
    /// Solid fills, tiled into bricks at stamp time.
    pub boxes: Vec<BuildBoxFile>,
    /// Cleared before anything is laid.
    pub carve: Vec<CellBox>,
    /// Stored and shown, not interpreted.
    pub markers: Vec<Marker>,
}

/// One brick the author chose. `min` is the min cell of the footprint AFTER `rot`
/// quarter turns (as `StudBrick::min`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildPartFile {
    pub min: [i32; 3],
    /// LDraw part number, as in `STUD_PARTS`.
    pub part: String,
    pub rot: u8,
    pub material: MaterialName,
    pub data: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildBoxFile {
    pub min: [i32; 3],
    pub size: [u32; 3],
    pub material: MaterialName,
    pub data: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct CellBox {
    pub min: [i32; 3],
    pub size: [u32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialName {
    /// A plastic of this LDraw colour code.
    Plastic(u16),
    /// A natural material by its `STUD_NATURALS` name.
    Natural(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct Marker {
    pub at: [i32; 3],
    pub kind: String,
    pub turn: u8,
}

/// Off-grid assemblies living as instances. Stored now so files and the library can
/// hold them; resolving one is the props plan's work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropFile {
    pub parts: Vec<PropPartFile>,
    pub slots: Vec<SlotDef>,
    /// Footprint on the grid, studs.
    pub foot: [u8; 2],
    pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropPartFile {
    pub part: String,
    /// LDU, relative to the anchor.
    pub pos: [i32; 3],
    /// Unit quaternion `[x, y, z, w]`.
    pub rot: [f32; 4],
    pub colour: PropColour,
    pub solid: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropColour {
    Fixed(u16),
    Slot(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotDef {
    pub name: String,
    pub default: u16,
}

/// Which kind a prefab is, without its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefabKindTag {
    Build,
    Prop,
}

impl PrefabFile {
    pub fn kind_tag(&self) -> PrefabKindTag {
        match self.kind {
            PrefabKindFile::Build(_) => PrefabKindTag::Build,
            PrefabKindFile::Prop(_) => PrefabKindTag::Prop,
        }
    }

    /// Parts, boxes and carve boxes of a Build; parts of a Prop.
    pub fn element_count(&self) -> usize {
        match &self.kind {
            PrefabKindFile::Build(b) => b.parts.len() + b.boxes.len() + b.carve.len(),
            PrefabKindFile::Prop(p) => p.parts.len(),
        }
    }

    /// For showing in a list: the bounding box of a Build's parts and boxes (studs x,
    /// plates y, studs z; a part the catalog does not know counts as one cell), or a
    /// Prop's footprint as `[x, 0, z]`. Nothing is validated.
    pub fn extent(&self) -> [u32; 3] {
        let build = match &self.kind {
            PrefabKindFile::Build(b) => b,
            PrefabKindFile::Prop(p) => return [u32::from(p.foot[0]), 0, u32::from(p.foot[1])],
        };
        let part_size = |p: &BuildPartFile| {
            let size = crate::stud_catalog::STUD_PARTS.iter().find(|d| d.number == p.part).map_or([1; 3], |d| d.size);
            let [sx, sy, sz] = size.map(u32::from);
            if p.rot & 1 == 1 { [sz, sy, sx] } else { [sx, sy, sz] }
        };
        let boxes = build.parts.iter().map(|p| (p.min, part_size(p))).chain(build.boxes.iter().map(|b| (b.min, b.size)));
        let mut bounds: Option<([i64; 3], [i64; 3])> = None;
        for (min, size) in boxes {
            let lo = min.map(i64::from);
            let hi = [0, 1, 2].map(|i| lo[i] + i64::from(size[i]));
            bounds = Some(match bounds {
                None => (lo, hi),
                Some((l, h)) => ([0, 1, 2].map(|i| l[i].min(lo[i])), [0, 1, 2].map(|i| h[i].max(hi[i]))),
            });
        }
        bounds.map_or([0; 3], |(lo, hi)| [0, 1, 2].map(|i| (hi[i] - lo[i]).clamp(0, i64::from(u32::MAX)) as u32))
    }
}

/// A prefab name is a file stem: `^[a-z0-9][a-z0-9-]{0,47}$`.
pub fn prefab_name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    let plain = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty() && b.len() <= PREFAB_NAME_MAX && plain(&b[0]) && b.iter().all(|c| plain(c) || *c == b'-')
}
