//! The resolved prefab form and the checks that guard it.

use serde::{Deserialize, Serialize};

use super::{
    prefab_name_ok, BuildBoxFile, BuildFile, BuildPartFile, CellBox, Marker, MaterialName, PrefabFile, PrefabKey,
    PrefabKindFile, PrefabMeta, PREFAB_FORMAT, PREFAB_HALF_STUDS, PREFAB_MARKER_KIND_MAX, PREFAB_MAX_BOXES,
    PREFAB_MAX_MARKERS, PREFAB_MAX_PARTS, PREFAB_MAX_PLATE, PREFAB_MIN_PLATE,
};
use crate::stud_catalog::{STUD_COLORS, STUD_PARTS};
use crate::{stud_material, NATURAL_BASE};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct ResolvedPrefab {
    pub name: String,
    pub kind: ResolvedKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub enum ResolvedKind {
    Build(ResolvedBuild),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct ResolvedBuild {
    pub parts: Vec<BuildPart>,
    pub boxes: Vec<BuildBox>,
    pub carve: Vec<CellBox>,
    pub markers: Vec<Marker>,
}

/// As [`BuildPartFile`], with `part` an index into `STUD_PARTS` and `material` a stud material id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct BuildPart {
    pub min: [i32; 3],
    pub part: u16,
    pub rot: u8,
    pub material: u16,
    pub data: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct BuildBox {
    pub min: [i32; 3],
    pub size: [u32; 3],
    pub material: u16,
    pub data: u16,
}

/// What is wrong with a prefab. An index is the element's position in its own list
/// (`parts` for rotations; `parts`, `boxes` or `carve` for bounds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    BadName,
    BadFormat(u16),
    UnknownPart(String),
    UnknownMaterial(String),
    BadRotation(usize),
    OutOfBounds(usize),
    TooManyParts(usize),
    /// Solid boxes plus carve boxes.
    TooManyBoxes(usize),
    EmptyBox(usize),
    TooManyMarkers(usize),
    BadMarker(usize),
    /// A player's prefab carries a non-zero brick `data`.
    DataNotAllowed,
    PropsNotSupported,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::BadName => write!(f, "the name must be 1-48 of a-z, 0-9 and '-', not starting with '-'"),
            Problem::BadFormat(v) => write!(f, "unknown file format {v}"),
            Problem::UnknownPart(p) => write!(f, "unknown part {p}"),
            Problem::UnknownMaterial(m) => write!(f, "unknown material {m}"),
            Problem::BadRotation(i) => write!(f, "part {i} has a rotation outside 0..4"),
            Problem::OutOfBounds(i) => write!(f, "element {i} reaches outside the prefab bounds"),
            Problem::TooManyParts(n) => write!(f, "{n} parts (the limit is {PREFAB_MAX_PARTS})"),
            Problem::TooManyBoxes(n) => write!(f, "{n} boxes (the limit is {PREFAB_MAX_BOXES})"),
            Problem::EmptyBox(i) => write!(f, "box {i} has a zero size"),
            Problem::TooManyMarkers(n) => write!(f, "{n} markers (the limit is {PREFAB_MAX_MARKERS})"),
            Problem::BadMarker(i) => write!(f, "marker {i} is invalid"),
            Problem::DataNotAllowed => write!(f, "only the game's own prefabs may tag bricks with data"),
            Problem::PropsNotSupported => write!(f, "prop prefabs are not supported yet"),
        }
    }
}

/// Who vouches for a prefab. Only the game's own prefabs may tag bricks with `data`
/// (a stamped player brick must never pose as part of a tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Builtin,
    Player,
}

/// Whether the cell box `min .. min + size` lies inside the Build bounds.
fn in_bounds(min: [i32; 3], size: [u32; 3]) -> bool {
    let lo = [-PREFAB_HALF_STUDS, PREFAB_MIN_PLATE, -PREFAB_HALF_STUDS];
    let hi = [PREFAB_HALF_STUDS, PREFAB_MAX_PLATE, PREFAB_HALF_STUDS];
    (0..3).all(|i| min[i] >= lo[i] && i64::from(min[i]) + i64::from(size[i]) <= i64::from(hi[i]))
}

/// The footprint of `STUD_PARTS[part]` after `rot` quarter turns.
fn turned_size(part: usize, rot: u8) -> [u32; 3] {
    let [sx, sy, sz] = STUD_PARTS[part].size.map(u32::from);
    if rot & 1 == 1 { [sz, sy, sx] } else { [sx, sy, sz] }
}

/// The count caps, checked before anything walks the lists (hostile input stays cheap).
fn count_problems(parts: usize, boxes: usize, carve: usize, markers: usize) -> Vec<Problem> {
    let mut out = Vec::new();
    if parts > PREFAB_MAX_PARTS {
        out.push(Problem::TooManyParts(parts));
    }
    if boxes + carve > PREFAB_MAX_BOXES {
        out.push(Problem::TooManyBoxes(boxes + carve));
    }
    if markers > PREFAB_MAX_MARKERS {
        out.push(Problem::TooManyMarkers(markers));
    }
    out
}

fn check_carve_and_markers(carve: &[CellBox], markers: &[Marker], out: &mut Vec<Problem>) {
    for (i, c) in carve.iter().enumerate() {
        if c.size.contains(&0) {
            out.push(Problem::EmptyBox(i));
        } else if !in_bounds(c.min, c.size) {
            out.push(Problem::OutOfBounds(i));
        }
    }
    for (i, m) in markers.iter().enumerate() {
        if m.turn >= 4 || m.kind.len() > PREFAB_MARKER_KIND_MAX || !in_bounds(m.at, [1, 1, 1]) {
            out.push(Problem::BadMarker(i));
        }
    }
}

fn material_id(m: &MaterialName) -> Result<u16, Problem> {
    match m {
        MaterialName::Plastic(c) if STUD_COLORS.binary_search(c).is_ok() => Ok(*c),
        MaterialName::Plastic(c) => Err(Problem::UnknownMaterial(c.to_string())),
        MaterialName::Natural(n) => crate::stud_material_id(n).ok_or_else(|| Problem::UnknownMaterial(n.clone())),
    }
}

fn material_name(id: u16) -> MaterialName {
    if id >= NATURAL_BASE {
        MaterialName::Natural(stud_material(id).map_or_else(|| format!("#{id}"), |m| m.name.to_string()))
    } else {
        MaterialName::Plastic(id)
    }
}

/// File form → resolved form. Never panics; every problem found is listed.
/// With [`Trust::Player`] every `data` is forced to 0.
pub fn resolve(file: &PrefabFile, trust: Trust) -> Result<ResolvedPrefab, Vec<Problem>> {
    let build = match &file.kind {
        PrefabKindFile::Build(b) => b,
        PrefabKindFile::Prop(_) => return Err(vec![Problem::PropsNotSupported]),
    };
    let mut problems = Vec::new();
    if file.format != PREFAB_FORMAT {
        problems.push(Problem::BadFormat(file.format));
    }
    if !prefab_name_ok(&file.name) {
        problems.push(Problem::BadName);
    }
    let counts = count_problems(build.parts.len(), build.boxes.len(), build.carve.len(), build.markers.len());
    if !counts.is_empty() {
        problems.extend(counts);
        return Err(problems);
    }
    let data = |d: u16| if trust == Trust::Player { 0 } else { d };
    let mut out = ResolvedBuild { carve: build.carve.clone(), markers: build.markers.clone(), ..Default::default() };
    for (i, p) in build.parts.iter().enumerate() {
        let part = STUD_PARTS.iter().position(|d| d.number == p.part);
        if part.is_none() {
            problems.push(Problem::UnknownPart(p.part.clone()));
        }
        let material = material_id(&p.material).map_err(|e| problems.push(e)).ok();
        if p.rot >= 4 {
            problems.push(Problem::BadRotation(i));
            continue;
        }
        let Some(part) = part else { continue };
        if !in_bounds(p.min, turned_size(part, p.rot)) {
            problems.push(Problem::OutOfBounds(i));
        }
        if let Some(material) = material {
            out.parts.push(BuildPart { min: p.min, part: part as u16, rot: p.rot, material, data: data(p.data) });
        }
    }
    for (i, b) in build.boxes.iter().enumerate() {
        let material = material_id(&b.material).map_err(|e| problems.push(e)).ok();
        if b.size.contains(&0) {
            problems.push(Problem::EmptyBox(i));
        } else if !in_bounds(b.min, b.size) {
            problems.push(Problem::OutOfBounds(i));
        }
        if let Some(material) = material {
            out.boxes.push(BuildBox { min: b.min, size: b.size, material, data: data(b.data) });
        }
    }
    check_carve_and_markers(&build.carve, &build.markers, &mut problems);
    if problems.is_empty() {
        Ok(ResolvedPrefab { name: file.name.clone(), kind: ResolvedKind::Build(out) })
    } else {
        Err(problems)
    }
}

/// Resolved form → file form. Total: an id outside the tables (an unvalidated prefab)
/// becomes a name no table holds, so resolving the result reports it.
pub fn unresolve(p: &ResolvedPrefab, meta: PrefabMeta) -> PrefabFile {
    let ResolvedKind::Build(b) = &p.kind;
    let number = |part: u16| STUD_PARTS.get(part as usize).map_or_else(|| format!("#{part}"), |d| d.number.to_string());
    let build = BuildFile {
        parts: b
            .parts
            .iter()
            .map(|p| BuildPartFile {
                min: p.min,
                part: number(p.part),
                rot: p.rot,
                material: material_name(p.material),
                data: p.data,
            })
            .collect(),
        boxes: b
            .boxes
            .iter()
            .map(|x| BuildBoxFile { min: x.min, size: x.size, material: material_name(x.material), data: x.data })
            .collect(),
        carve: b.carve.clone(),
        markers: b.markers.clone(),
    };
    PrefabFile { format: PREFAB_FORMAT, name: p.name.clone(), meta, kind: PrefabKindFile::Build(build) }
}

impl ResolvedPrefab {
    /// Every check [`resolve`] makes, on ids instead of names: what a server runs on a
    /// resolved prefab it did not build itself. [`Trust::Player`] also requires every
    /// `data` to be 0.
    pub fn validate(&self, trust: Trust) -> Result<(), Vec<Problem>> {
        let ResolvedKind::Build(b) = &self.kind;
        let mut problems = Vec::new();
        if !prefab_name_ok(&self.name) {
            problems.push(Problem::BadName);
        }
        let counts = count_problems(b.parts.len(), b.boxes.len(), b.carve.len(), b.markers.len());
        if !counts.is_empty() {
            problems.extend(counts);
            return Err(problems);
        }
        let material_ok = |m: u16, data: u16, problems: &mut Vec<Problem>| {
            if stud_material(m).is_none() {
                problems.push(Problem::UnknownMaterial(m.to_string()));
            }
            if trust == Trust::Player && data != 0 {
                problems.push(Problem::DataNotAllowed);
            }
        };
        for (i, p) in b.parts.iter().enumerate() {
            material_ok(p.material, p.data, &mut problems);
            if p.rot >= 4 {
                problems.push(Problem::BadRotation(i));
                continue;
            }
            if p.part as usize >= STUD_PARTS.len() {
                problems.push(Problem::UnknownPart(p.part.to_string()));
            } else if !in_bounds(p.min, turned_size(p.part as usize, p.rot)) {
                problems.push(Problem::OutOfBounds(i));
            }
        }
        for (i, x) in b.boxes.iter().enumerate() {
            material_ok(x.material, x.data, &mut problems);
            if x.size.contains(&0) {
                problems.push(Problem::EmptyBox(i));
            } else if !in_bounds(x.min, x.size) {
                problems.push(Problem::OutOfBounds(i));
            }
        }
        check_carve_and_markers(&b.carve, &b.markers, &mut problems);
        if problems.is_empty() { Ok(()) } else { Err(problems) }
    }

    /// The content address: the `StateHash` of the resolved form.
    pub fn key(&self) -> PrefabKey {
        use state_hash::StateHash;
        let mut h = crc32fast::Hasher::new();
        self.state_hash(&mut h);
        PrefabKey(h.finalize())
    }

    /// What the per-player budget counts: parts, boxes and carve boxes.
    pub fn element_count(&self) -> usize {
        let ResolvedKind::Build(b) = &self.kind;
        b.parts.len() + b.boxes.len() + b.carve.len()
    }
}
