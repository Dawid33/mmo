//! Pure stud meshing (no ECS).
//!
//! **Terrain chunks** ([`mesh_chunk`]) are meshed for what is seen from afar,
//! with the brick look added only near the viewer:
//! - **Body**: every box-shaped brick (fillers and box parts — all natural
//!   ground and trees) contributes only the cell faces that look at a free
//!   cell, and coplanar faces of one colour are merged greedily across bricks
//!   into quads of four shared vertices ([`merged_box_faces`]). Parts that are
//!   not boxes (slopes) keep their baked hi-detail triangles, per exposed face.
//! - **Detail** ([`StudMeshOutput::top`], only when the job asks for it — the
//!   chunks near the viewer, `StudWorld::set_detail_centre`): the lo-detail
//!   studs on free stud cells and the baked brick edges as thin quads lifted
//!   off the faces (the "built from bricks" seam lines). Without it a chunk
//!   is flat-shaded boxes of the brick colours.
//!
//! The first launch drew everything everywhere, from the baked part
//! triangles: 8.8 M body and 7.9 M stud triangles per 256 m region (58 % of
//! the body seam-line quads, 22 % brick undersides), 40-byte unshared
//! vertices — 1.9 GB of vertex data a region (`measure.rs`).
//!
//! **Whole bricks** ([`body_mesh`], [`stud_mesh`], [`mesh_model`]: the build
//! ghost and falling bodies) are still the baked triangles copied, rotated and
//! translated, culled by their tags: each brick's hi-detail `Face(dir)` tris
//! where that face is exposed, `Outer` tris (a slope's slanted face) where a
//! face they lean toward is exposed, `Inner` tris where the underside is exposed,
//! seam-line quads on exposed faces; the lo-detail `Stud(cell)` tris of free
//! stud cells. There a face is exposed when ANY cell across it over the
//! brick's footprint is free.
//!
//! Positions are chunk-local sim units (the chunk entity sits at the chunk
//! origin). Cells past the chunk edge are read through [`StudNeighbours`].
//!
//! **Glass.** A brick whose LDraw colour is transparent ([`StudAssets::is_transparent`])
//! is drawn alpha-blended, so it goes — body and studs — into a third mesh
//! ([`StudMeshOutput::glass`]) and it hides nothing: opaque faces and studs are
//! culled against OPAQUE cells only (also across chunk faces — a [`Slab`] holds
//! the neighbour's opaque cells), so a face between glass and an opaque brick is
//! kept on the opaque side. A glass face is culled against opaque cells and, within
//! its chunk, glass of its own colour (one pane, no inner walls); glass of another
//! colour, or of the same colour across a chunk face, leaves it drawn. Glass
//! bricks are meshed whole (the baked triangles), with or without detail.
//!
//! Chunks are meshed off the main thread ([`StudMeshJob`], `renderer::compute`):
//! the job carries the chunk and, per face, the one layer of cells just across
//! it ([`FaceSlabs`]) — all the mesher reads of a neighbour. The baked parts are
//! a process global ([`stud_assets`]), decoded on first use in whichever wasm
//! instance (page or worker) runs the mesher.

use std::collections::BTreeMap;

use std::sync::OnceLock;

use bevy::color::{Color, ColorToComponents};
use serde::{Deserialize, Serialize};
use game::stud_assets::{BakedParts, EdgeTag, FaceDir, StudColor, TriColor, TriTag, BAKED_PARTS_VERSION};
use game::{plate_units, rotate_cell, StudBrick, StudChunk, StudOccupancy, StudShape};
use game::{STUD_CHUNK_PLATES, STUD_CHUNK_STUDS, STUD_UNITS};

/// Width of an edge-line quad, sim units.
pub const STUD_EDGE_WIDTH: f32 = 0.12;
/// How far an edge-line quad sits outward of its face, sim units.
pub const STUD_EDGE_LIFT: f32 = 0.01;
/// Vertex alpha of an edge-line quad on an opaque brick. Invisible (the opaque
/// material ignores alpha, masked it is far over the cutoff); it tells the
/// fortress shader a seam line from a brick face, so the line's back side is
/// never drawn as the cut's cap (`stud/material.rs`, `stud_terrain.wgsl`).
pub const STUD_EDGE_ALPHA: f32 = 0.9;

/// Source of the baked part table and colour table (committed; `just ldraw-bake`).
pub const STUD_PARTS_PATH: &str = "assets/studs/parts.bin";
pub const STUD_COLORS_PATH: &str = "assets/studs/colors.ron";
// Embedded so the web build (no filesystem) gets them too.
const PARTS_BIN: &[u8] = include_bytes!("../../../../../assets/studs/parts.bin");
const COLORS_RON: &str = include_str!("../../../../../assets/studs/colors.ron");

/// The baked parts and colours: decoded once per process (per wasm instance —
/// the page and every compute worker each decode their own on first use).
pub fn stud_assets() -> &'static StudAssets {
    static ASSETS: OnceLock<StudAssets> = OnceLock::new();
    ASSETS.get_or_init(StudAssets::load)
}

/// The baked LDraw parts and colours ([`stud_assets`]).
pub struct StudAssets {
    pub parts: BakedParts,
    pub colors: BTreeMap<u16, StudColor>,
}

impl StudAssets {
    /// Decode the embedded `parts.bin` / `colors.ron`. Panics (naming the
    /// file and the fix) when either is unreadable or `parts.bin` is stale.
    pub fn load() -> Self {
        let parts: BakedParts = game::decode_packet(PARTS_BIN)
            .unwrap_or_else(|e| panic!("{STUD_PARTS_PATH}: cannot decode ({e:?}); run `just ldraw-bake`"));
        assert_eq!(
            parts.version, BAKED_PARTS_VERSION,
            "{STUD_PARTS_PATH}: baked version {} != BAKED_PARTS_VERSION {}; run `just ldraw-bake`",
            parts.version, BAKED_PARTS_VERSION
        );
        assert_eq!(
            parts.parts.len(),
            game::stud_catalog::STUD_PARTS.len(),
            "{STUD_PARTS_PATH}: part count differs from stud_catalog; run `just ldraw-bake`"
        );
        let colors: Vec<StudColor> = ron::from_str(COLORS_RON)
            .unwrap_or_else(|e| panic!("{STUD_COLORS_PATH}: cannot parse ({e}); run `just ldraw-bake`"));
        Self { parts, colors: colors.into_iter().map(|c| (c.code, c)).collect() }
    }

    /// Linear RGBA of `rgb` (sRGB bytes), opaque.
    fn linear(rgb: [u8; 3]) -> [f32; 4] {
        Color::srgb_u8(rgb[0], rgb[1], rgb[2]).to_linear().to_f32_array()
    }

    /// Opacity of LDraw colour `code`, 0..=1 (opaque if unknown).
    pub fn alpha(&self, code: u16) -> f32 {
        self.colors.get(&code).map_or(1.0, |c| f32::from(c.alpha) / 255.0)
    }

    /// Whether bricks of stud material `material` are drawn blended: its LDraw
    /// colour has an alpha under 255.
    pub fn is_transparent(&self, material: u16) -> bool {
        self.colors.get(&game::stud_material_color(material)).is_some_and(|c| c.alpha < 255)
    }

    /// `(own, edge)` linear colours of LDraw colour `code` (grey if unknown).
    pub(super) fn brick_colors(&self, code: u16) -> ([f32; 4], [f32; 4]) {
        match self.colors.get(&code) {
            Some(c) => (Self::linear(c.rgb), Self::linear(c.edge)),
            None => (Self::linear([160, 160, 160]), Self::linear([51, 51, 51])),
        }
    }
}

/// Occupancy in chunk-local cells extended one step past the chunk (x/z −1
/// and 32, y −1 and 90). Asked only for out-of-chunk cells; unloaded or
/// across a region seam answers free.
pub struct StudNeighbours<'a> {
    pub get: &'a dyn Fn([i32; 3]) -> bool,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudMeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl StudMeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn tri(&mut self, p: [[f32; 3]; 3], n: [f32; 3], c: [f32; 4]) {
        let base = self.positions.len() as u32;
        for v in p {
            self.positions.push(v);
            self.normals.push(n);
            self.colors.push(c);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }

    /// Quad `q` (a loop) facing `n`, wound CCW seen from `n`'s side.
    fn quad(&mut self, q: [[f32; 3]; 4], n: [f32; 3], c: [f32; 4]) {
        let [a, b, cc, _] = q;
        let flip = dot(cross(sub(b, a), sub(cc, a)), n) < 0.0;
        let base = self.positions.len() as u32;
        for v in q {
            self.positions.push(v);
            self.normals.push(n);
            self.colors.push(c);
        }
        if flip {
            self.indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
        } else {
            self.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

/// The vertex alphas of one brick's triangles.
#[derive(Clone, Copy)]
struct Alphas {
    /// Faces, studs and baked edge geometry.
    face: f32,
    /// Seam-line quads.
    seam: f32,
}

/// An opaque brick: solid faces, seam lines marked by [`STUD_EDGE_ALPHA`].
const OPAQUE: Alphas = Alphas { face: 1.0, seam: STUD_EDGE_ALPHA };

impl Alphas {
    /// A brick of LDraw colour `code` in the glass mesh: all of it at the colour's opacity.
    fn glass(assets: &StudAssets, code: u16) -> Self {
        let a = assets.alpha(code);
        Self { face: a, seam: a }
    }
}

fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

const DIRS: [FaceDir; 6] = [FaceDir::PosX, FaceDir::NegX, FaceDir::PosY, FaceDir::NegY, FaceDir::PosZ, FaceDir::NegZ];

fn dir_vec(d: FaceDir) -> [f32; 3] {
    match d {
        FaceDir::PosX => [1.0, 0.0, 0.0],
        FaceDir::NegX => [-1.0, 0.0, 0.0],
        FaceDir::PosY => [0.0, 1.0, 0.0],
        FaceDir::NegY => [0.0, -1.0, 0.0],
        FaceDir::PosZ => [0.0, 0.0, 1.0],
        FaceDir::NegZ => [0.0, 0.0, -1.0],
    }
}

/// `d` turned `rot` quarter turns about +Y, the same way as [`rotate_vec`].
fn rotate_dir(rot: u8, d: FaceDir) -> FaceDir {
    use FaceDir::*;
    let quarter = |d| match d {
        PosX => PosZ,
        PosZ => NegX,
        NegX => NegZ,
        NegZ => PosX,
        y => y,
    };
    (0..rot & 3).fold(d, |d, _| quarter(d))
}

/// A direction turned like [`rotate_point_units`] (no translation).
fn rotate_vec(rot: u8, [x, y, z]: [f32; 3]) -> [f32; 3] {
    match rot & 3 {
        0 => [x, y, z],
        1 => [-z, y, x],
        2 => [-x, y, -z],
        _ => [z, y, -x],
    }
}

/// `game::rotate_point` in sim units: `p` relative to the unrotated bbox min,
/// `ex`/`ez` the UNROTATED x/z extents in units.
fn rotate_point_units(rot: u8, [x, y, z]: [f32; 3], ex: f32, ez: f32) -> [f32; 3] {
    match rot & 3 {
        0 => [x, y, z],
        1 => [ez - z, y, x],
        2 => [ex - x, y, ez - z],
        _ => [z, y, ex - x],
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Whether chunk-local cell `c` (possibly one step outside) is occupied.
fn occupied(occ: &StudOccupancy, n: &StudNeighbours, c: [i32; 3]) -> bool {
    let inside = (0..STUD_CHUNK_STUDS as i32).contains(&c[0])
        && (0..STUD_CHUNK_PLATES as i32).contains(&c[1])
        && (0..STUD_CHUNK_STUDS as i32).contains(&c[2]);
    if inside { occ.get(c) } else { (n.get)(c) }
}

/// Exposure of each face of `b` (indexed like [`DIRS`], world/rotated dirs):
/// any cell across that face over the footprint is free.
fn exposed_faces(b: &StudBrick, occ: &StudOccupancy, n: &StudNeighbours) -> [bool; 6] {
    exposed_faces_at(b.min.map(i32::from), b.size().map(i32::from), &|c| occupied(occ, n, c))
}

/// [`exposed_faces`] of the cell box `min .. min + size` over any occupancy.
fn exposed_faces_at(min: [i32; 3], size: [i32; 3], occupied: &dyn Fn([i32; 3]) -> bool) -> [bool; 6] {
    DIRS.map(|d| {
        // Axis of the face, and the cell coordinate just across it.
        let (axis, across) = match d {
            FaceDir::PosX => (0, min[0] + size[0]),
            FaceDir::NegX => (0, min[0] - 1),
            FaceDir::PosY => (1, min[1] + size[1]),
            FaceDir::NegY => (1, min[1] - 1),
            FaceDir::PosZ => (2, min[2] + size[2]),
            FaceDir::NegZ => (2, min[2] - 1),
        };
        let (u, v) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        (0..size[u]).any(|i| {
            (0..size[v]).any(|j| {
                let mut c = [0; 3];
                c[axis] = across;
                c[u] = min[u] + i;
                c[v] = min[v] + j;
                !occupied(c)
            })
        })
    })
}

fn dir_index(d: FaceDir) -> usize {
    DIRS.iter().position(|&x| x == d).unwrap_or(0)
}

/// The corner of cell `min` in units of its grid's frame (a chunk's, or a body model's).
fn cell_origin(min: [i32; 3]) -> [f32; 3] {
    [(min[0] * STUD_UNITS as i32) as f32, plate_units(min[1]), (min[2] * STUD_UNITS as i32) as f32]
}

/// Body of chunk `chunk`; see the module docs.
pub fn body_mesh(chunk: &StudChunk, n: &StudNeighbours, assets: &StudAssets) -> StudMeshData {
    body_mesh_with(chunk, &chunk.occupancy(), n, assets, &mut |_, _| {})
}

/// Studs on the exposed tops of `chunk`; see the module docs.
pub fn stud_mesh(chunk: &StudChunk, n: &StudNeighbours, assets: &StudAssets) -> StudMeshData {
    stud_mesh_with(chunk, &chunk.occupancy(), n, assets)
}

/// [`body_mesh`] on a prebuilt occupancy. `on_tri(brick, tag)` sees every
/// kept triangle's tag (with `Face` in world dirs), for tests.
pub(crate) fn body_mesh_with(
    chunk: &StudChunk,
    occ: &StudOccupancy,
    n: &StudNeighbours,
    assets: &StudAssets,
    on_tri: &mut dyn FnMut(&StudBrick, TriTag),
) -> StudMeshData {
    let mut out = StudMeshData::default();
    for b in chunk.bricks() {
        let exposed = exposed_faces(b, occ, n);
        emit_body(&mut out, b.min.map(i32::from), b, exposed, assets, OPAQUE, on_tri);
    }
    out
}

/// The whole body of brick `b` standing at cell `min` (`b.min` is not read), for the
/// faces `exposed` (indexed like [`DIRS`]): its baked triangles and its seam lines.
fn emit_body(
    out: &mut StudMeshData,
    min: [i32; 3],
    b: &StudBrick,
    exposed: [bool; 6],
    assets: &StudAssets,
    alphas: Alphas,
    on_tri: &mut dyn FnMut(&StudBrick, TriTag),
) {
    emit_faces(out, min, b, exposed, assets, alphas, on_tri);
    emit_seams(out, min, b, exposed, assets, alphas.seam);
}

/// How far an `Outer` triangle's unit normal must lean toward a face direction
/// to be seen through that face.
const OUTER_LEAN: f32 = 1e-3;

/// The baked body triangles of brick `b` standing at cell `min`, for the faces
/// `exposed`: a filler's box faces; a part's `Face` tris, its `Outer` tris (a
/// slope's slanted face) where a face they lean toward is exposed and, where
/// the underside shows, its `Inner` ones.
fn emit_faces(
    out: &mut StudMeshData,
    min: [i32; 3],
    b: &StudBrick,
    exposed: [bool; 6],
    assets: &StudAssets,
    alphas: Alphas,
    on_tri: &mut dyn FnMut(&StudBrick, TriTag),
) {
    if !exposed.iter().any(|&e| e) {
        return;
    }
    let (own, edge) = assets.brick_colors(game::stud_material_color(b.material));
    let (own, edge) = (with_alpha(own, alphas.face), with_alpha(edge, alphas.face));
    let o = cell_origin(min);
    match b.shape {
        StudShape::Filler { .. } => {
            let s = b.size();
            let hi = [
                o[0] + (s[0] as usize * STUD_UNITS) as f32,
                o[1] + plate_units(s[1] as i32),
                o[2] + (s[2] as usize * STUD_UNITS) as f32,
            ];
            for (i, d) in DIRS.into_iter().enumerate() {
                if !exposed[i] {
                    continue;
                }
                box_face(out, o, hi, d, own);
                on_tri(b, TriTag::Face(d));
                on_tri(b, TriTag::Face(d));
            }
        }
        StudShape::Part { part, rot } => {
            let (Some(def), Some(baked)) = (b.part(), assets.parts.parts.get(part as usize)) else {
                return;
            };
            let ex = (def.size[0] as usize * STUD_UNITS) as f32;
            let ez = (def.size[2] as usize * STUD_UNITS) as f32;
            let place = |p: [f32; 3]| add(rotate_point_units(rot, p, ex, ez), o);
            for t in &baked.hi.tris {
                let tag = match t.tag {
                    TriTag::Face(d) => {
                        let w = rotate_dir(rot, d);
                        if !exposed[dir_index(w)] {
                            continue;
                        }
                        TriTag::Face(w)
                    }
                    TriTag::Inner => {
                        if !exposed[dir_index(FaceDir::NegY)] {
                            continue;
                        }
                        TriTag::Inner
                    }
                    TriTag::Outer => {
                        // A slanted outside face shows through every face it leans toward.
                        let n = rotate_vec(rot, t.normal);
                        if !DIRS.into_iter().enumerate().any(|(i, d)| exposed[i] && dot(n, dir_vec(d)) > OUTER_LEAN) {
                            continue;
                        }
                        TriTag::Outer
                    }
                    TriTag::Stud(_) => continue,
                };
                let color = match t.color {
                    TriColor::Own => own,
                    TriColor::Edge => edge,
                    TriColor::Fixed(c) => with_alpha(assets.brick_colors(c).0, alphas.face),
                };
                out.tri(t.pos.map(place), rotate_vec(rot, t.normal), color);
                on_tri(b, tag);
            }
        }
    }
}

/// The seam lines of brick `b` standing at cell `min`: its baked edges that lie on a
/// face in `exposed` (indexed like [`DIRS`]), as thin quads in that face's plane.
/// Fillers have none.
fn emit_seams(out: &mut StudMeshData, min: [i32; 3], b: &StudBrick, exposed: [bool; 6], assets: &StudAssets, alpha: f32) {
    let StudShape::Part { part, rot } = b.shape else { return };
    if !exposed.iter().any(|&e| e) {
        return;
    }
    let (Some(def), Some(baked)) = (b.part(), assets.parts.parts.get(part as usize)) else { return };
    let seam = with_alpha(assets.brick_colors(game::stud_material_color(b.material)).1, alpha);
    let o = cell_origin(min);
    let ex = (def.size[0] as usize * STUD_UNITS) as f32;
    let ez = (def.size[2] as usize * STUD_UNITS) as f32;
    let place = |p: [f32; 3]| add(rotate_point_units(rot, p, ex, ez), o);
    for e in &baked.hi.edges {
        let EdgeTag::Faces(mask) = e.tag else { continue };
        let faces: Vec<FaceDir> =
            DIRS.into_iter().filter(|&d| mask & (1 << d as u8) != 0).map(|d| rotate_dir(rot, d)).collect();
        let (a, bb) = (place(e.a), place(e.b));
        for &f in &faces {
            if !exposed[dir_index(f)] {
                continue;
            }
            let other = faces.iter().copied().find(|&g| g != f);
            edge_quad(out, a, bb, f, other, seam);
        }
    }
}

/// The cell counts of a chunk along x, y, z.
const CHUNK_CELLS: [usize; 3] = [STUD_CHUNK_STUDS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS];

/// The faces of the opaque box-shaped bricks of `chunk` that look at a free cell
/// (`occ` ∪ `n`: the opaque cells), merged: per face plane, the exposed cell faces
/// of one LDraw colour are covered greedily by rectangles (runs along the first
/// in-face axis, extended along the second), each one quad of four vertices.
/// Brick boundaries are not kept — the seam lines are the detail mesh's.
fn merged_box_faces(out: &mut StudMeshData, chunk: &StudChunk, occ: &StudOccupancy, n: &StudNeighbours, assets: &StudAssets) {
    let [nx, ny, nz] = CHUNK_CELLS;
    // LDraw colour code + 1 of the box brick holding each cell, 0 = none.
    let index = |c: [usize; 3]| (c[1] * nz + c[2]) * nx + c[0];
    let mut cells = vec![0u32; nx * ny * nz];
    let mut any = false;
    for b in chunk.bricks().filter(|b| b.is_box()) {
        let key = u32::from(game::stud_material_color(b.material)) + 1;
        for c in b.cells() {
            cells[index(c.map(usize::from))] = key;
        }
        any = true;
    }
    if !any {
        return;
    }
    // Cell corner -> units of the chunk's frame, per axis.
    let units = |axis: usize, cell: usize| if axis == 1 { plate_units(cell as i32) } else { (cell * STUD_UNITS) as f32 };
    for (d, dir) in DIRS.into_iter().enumerate() {
        let (axis, positive) = dir_axis(d);
        let (ua, va) = face_axes(axis);
        let (nu, nv) = (CHUNK_CELLS[ua], CHUNK_CELLS[va]);
        let mut mask = vec![0u32; nu * nv];
        for s in 0..CHUNK_CELLS[axis] {
            let mut exposed = false;
            for v in 0..nv {
                for u in 0..nu {
                    let mut c = [0usize; 3];
                    c[axis] = s;
                    c[ua] = u;
                    c[va] = v;
                    let key = cells[index(c)];
                    let shows = key != 0 && {
                        let mut across = c.map(|x| x as i32);
                        across[axis] += if positive { 1 } else { -1 };
                        !occupied(occ, n, across)
                    };
                    mask[v * nu + u] = if shows { key } else { 0 };
                    exposed |= shows;
                }
            }
            if !exposed {
                continue;
            }
            let at = units(axis, if positive { s + 1 } else { s });
            for v in 0..nv {
                let mut u = 0;
                while u < nu {
                    let key = mask[v * nu + u];
                    if key == 0 {
                        u += 1;
                        continue;
                    }
                    let mut w = 1;
                    while u + w < nu && mask[v * nu + u + w] == key {
                        w += 1;
                    }
                    let mut h = 1;
                    while v + h < nv && (u..u + w).all(|i| mask[(v + h) * nu + i] == key) {
                        h += 1;
                    }
                    for row in v..v + h {
                        mask[row * nu + u..row * nu + u + w].fill(0);
                    }
                    let corner = |cu: usize, cv: usize| {
                        let mut p = [0.0; 3];
                        p[axis] = at;
                        p[ua] = units(ua, cu);
                        p[va] = units(va, cv);
                        p
                    };
                    let color = with_alpha(assets.brick_colors((key - 1) as u16).0, OPAQUE.face);
                    out.quad([corner(u, v), corner(u + w, v), corner(u + w, v + h), corner(u, v + h)], dir_vec(dir), color);
                    u += w;
                }
            }
        }
    }
}

/// The opaque meshes of a terrain chunk whose opaque cells are `occ`: the merged
/// box faces plus the baked triangles of the parts that are not boxes, and — with
/// `detail` — the studs and seam lines of every brick.
fn opaque_chunk_meshes(chunk: &StudChunk, occ: &StudOccupancy, n: &StudNeighbours, assets: &StudAssets, detail: bool) -> (StudMeshData, StudMeshData) {
    let (mut body, mut top) = (StudMeshData::default(), StudMeshData::default());
    merged_box_faces(&mut body, chunk, occ, n, assets);
    for b in chunk.bricks() {
        let boxy = b.is_box();
        if boxy && !(detail && matches!(b.shape, StudShape::Part { .. })) {
            continue;
        }
        let min = b.min.map(i32::from);
        let exposed = exposed_faces(b, occ, n);
        if !boxy {
            emit_faces(&mut body, min, b, exposed, assets, OPAQUE, &mut |_, _| {});
        }
        if detail {
            emit_seams(&mut top, min, b, exposed, assets, OPAQUE.seam);
            emit_studs(&mut top, min, b, &|c| !occupied(occ, n, c), assets, OPAQUE.face);
        }
    }
    (body, top)
}

/// One face of the box `lo..hi`, facing `d`.
fn box_face(out: &mut StudMeshData, lo: [f32; 3], hi: [f32; 3], d: FaceDir, c: [f32; 4]) {
    let (axis, at) = match d {
        FaceDir::PosX => (0, hi[0]),
        FaceDir::NegX => (0, lo[0]),
        FaceDir::PosY => (1, hi[1]),
        FaceDir::NegY => (1, lo[1]),
        FaceDir::PosZ => (2, hi[2]),
        FaceDir::NegZ => (2, lo[2]),
    };
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let corner = |a: f32, b: f32| {
        let mut p = [0.0; 3];
        p[axis] = at;
        p[u] = a;
        p[v] = b;
        p
    };
    out.quad(
        [corner(lo[u], lo[v]), corner(hi[u], lo[v]), corner(hi[u], hi[v]), corner(lo[u], hi[v])],
        dir_vec(d),
        c,
    );
}

/// The edge `a`–`b` drawn on face `f` as a [`STUD_EDGE_WIDTH`] quad lifted
/// [`STUD_EDGE_LIFT`] outward. On a bbox corner edge (also on face `other`)
/// it is inset onto `f` (away from `other`); otherwise centred on the line.
fn edge_quad(out: &mut StudMeshData, a: [f32; 3], b: [f32; 3], f: FaceDir, other: Option<FaceDir>, c: [f32; 4]) {
    let n = dir_vec(f);
    let e = sub(b, a);
    if dot(e, e) < 1e-8 {
        return;
    }
    let lift = scale(n, STUD_EDGE_LIFT);
    let (a, b) = (add(a, lift), add(b, lift));
    let q = match other {
        Some(g) => {
            let w = scale(dir_vec(g), -STUD_EDGE_WIDTH);
            [a, b, add(b, w), add(a, w)]
        }
        None => {
            let w = cross(n, e);
            let len = dot(w, w).sqrt();
            if len < 1e-6 {
                return;
            }
            let w = scale(w, STUD_EDGE_WIDTH * 0.5 / len);
            [sub(a, w), sub(b, w), add(b, w), add(a, w)]
        }
    };
    out.quad(q, n, c);
}

/// [`stud_mesh`] on a prebuilt occupancy.
pub(crate) fn stud_mesh_with(
    chunk: &StudChunk,
    occ: &StudOccupancy,
    n: &StudNeighbours,
    assets: &StudAssets,
) -> StudMeshData {
    let mut out = StudMeshData::default();
    for b in chunk.bricks() {
        emit_studs(&mut out, b.min.map(i32::from), b, &|c| !occupied(occ, n, c), assets, OPAQUE.face);
    }
    out
}

/// The stud-top triangles of brick `b` standing at cell `min` (`b.min` is not read),
/// for the stud cells `free` accepts.
fn emit_studs(
    out: &mut StudMeshData,
    min: [i32; 3],
    b: &StudBrick,
    free: &dyn Fn([i32; 3]) -> bool,
    assets: &StudAssets,
    alpha: f32,
) {
    let StudShape::Part { part, rot } = b.shape else { return };
    let (Some(def), Some(baked)) = (b.part(), assets.parts.parts.get(part as usize)) else { return };
    let (own, edge) = assets.brick_colors(game::stud_material_color(b.material));
    let (own, edge) = (with_alpha(own, alpha), with_alpha(edge, alpha));
    let o = cell_origin(min);
    let ex = (def.size[0] as usize * STUD_UNITS) as f32;
    let ez = (def.size[2] as usize * STUD_UNITS) as f32;
    // Free/occupied per stud cell, decided once.
    let mut keep_cell: BTreeMap<[u8; 3], bool> = BTreeMap::new();
    for t in &baked.lo.tris {
        let TriTag::Stud(cell) = t.tag else { continue };
        let keep = *keep_cell.entry(cell).or_insert_with(|| {
            let r = rotate_cell(rot, cell, def.size);
            free([min[0] + r[0] as i32, min[1] + r[1] as i32, min[2] + r[2] as i32])
        });
        if !keep {
            continue;
        }
        let color = match t.color {
            TriColor::Own => own,
            TriColor::Edge => edge,
            TriColor::Fixed(c) => with_alpha(assets.brick_colors(c).0, alpha),
        };
        let place = |p: [f32; 3]| add(rotate_point_units(rot, p, ex, ez), o);
        out.tri(t.pos.map(place), rotate_vec(rot, t.normal), color);
    }
}

/// One layer of cells just outside one face of a chunk: which of them are occupied
/// by an OPAQUE brick (glass hides nothing — see the module docs).
/// Indexed by the two in-face axes in axis order — `(y, z)` for the x faces, `(x, z)`
/// for the y faces, `(x, y)` for the z faces.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Slab {
    /// Nothing there (air, or not loaded).
    #[default]
    Free,
    /// Every cell occupied (buried ground).
    Solid,
    /// One bit per cell, `u + v * len(u)`.
    Bits(Vec<u64>),
}

/// The cell counts `(u, v)` of the slab across a face on `axis`.
fn slab_dims(axis: usize) -> (usize, usize) {
    match axis {
        0 => (STUD_CHUNK_PLATES, STUD_CHUNK_STUDS),
        1 => (STUD_CHUNK_STUDS, STUD_CHUNK_STUDS),
        _ => (STUD_CHUNK_STUDS, STUD_CHUNK_PLATES),
    }
}

/// The in-face axes `(u, v)` of a face on `axis`.
fn face_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// `(axis, positive)` of `DIRS[d]`.
fn dir_axis(d: usize) -> (usize, bool) {
    (d / 2, d % 2 == 0)
}

/// The slot (index into [`DIRS`]) of the face opposite `DIRS[d]`.
pub fn opposite_dir(d: usize) -> usize {
    d ^ 1
}

/// The chunk step of `DIRS[d]`: `[dx, dlayer, dz]`.
pub fn dir_step(d: usize) -> [i32; 3] {
    let (axis, positive) = dir_axis(d);
    let mut s = [0; 3];
    s[axis] = if positive { 1 } else { -1 };
    s
}

impl Slab {
    /// Whether the cell at in-face coordinates `(u, v)` of a face on `axis` is occupied.
    pub fn get(&self, axis: usize, u: i32, v: i32) -> bool {
        match self {
            Slab::Free => false,
            Slab::Solid => true,
            Slab::Bits(bits) => {
                let (nu, nv) = slab_dims(axis);
                if !(0..nu as i32).contains(&u) || !(0..nv as i32).contains(&v) {
                    return false;
                }
                let i = u as usize + v as usize * nu;
                bits.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
            }
        }
    }

    /// The chunk's own outermost layer on the side `DIRS[d]` (what the neighbour on
    /// that side sees across its opposite face).
    fn boundary(occ: &StudOccupancy, d: usize) -> Slab {
        let (axis, positive) = dir_axis(d);
        let (ua, va) = face_axes(axis);
        let (nu, nv) = slab_dims(axis);
        let len = [STUD_CHUNK_STUDS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS][axis] as i32;
        let at = if positive { len - 1 } else { 0 };
        let mut bits = vec![0u64; (nu * nv).div_ceil(64)];
        let mut count = 0;
        for v in 0..nv {
            for u in 0..nu {
                let mut c = [0; 3];
                c[axis] = at;
                c[ua] = u as i32;
                c[va] = v as i32;
                if occ.get(c) {
                    let i = u + v * nu;
                    bits[i / 64] |= 1 << (i % 64);
                    count += 1;
                }
            }
        }
        match count {
            0 => Slab::Free,
            n if n == nu * nv => Slab::Solid,
            _ => Slab::Bits(bits),
        }
    }
}

/// What a chunk mesh reads of its six face neighbours, in [`DIRS`] order
/// (+X, −X, +Y, −Y, +Z, −Z): the layer of cells just across each face.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceSlabs(pub [Slab; 6]);

impl FaceSlabs {
    /// A chunk's own six outermost layers: `boundaries(c).0[d]` is what the neighbour
    /// on side `DIRS[d]` puts in its slot [`opposite_dir`]`(d)`.
    pub fn boundaries(chunk: &StudChunk) -> FaceSlabs {
        let occ = opaque_occupancy(chunk, stud_assets());
        FaceSlabs(std::array::from_fn(|d| Slab::boundary(&occ, d)))
    }

    /// Occupancy of a cell one step outside the chunk (chunk-local cell coordinates);
    /// diagonal or farther cells read free.
    pub fn get(&self, c: [i32; 3]) -> bool {
        let len = [STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32, STUD_CHUNK_STUDS as i32];
        let mut out: Option<usize> = None;
        for axis in 0..3 {
            let d = if c[axis] == len[axis] {
                axis * 2
            } else if c[axis] == -1 {
                axis * 2 + 1
            } else if (0..len[axis]).contains(&c[axis]) {
                continue;
            } else {
                return false;
            };
            if out.replace(d).is_some() {
                return false;
            }
        }
        let Some(d) = out else { return false };
        let (axis, _) = dir_axis(d);
        let (ua, va) = face_axes(axis);
        self.0[d].get(axis, c[ua], c[va])
    }
}

/// A finished stud mesh: the opaque body, the opaque detail (`top`: stud tops, and for
/// a terrain chunk its seam lines — empty unless the job asked for detail), the glass (bodies
/// and studs of transparent bricks, vertex alpha = the colour's opacity), and where
/// its frame's origin sits from the entity it hangs under (zero for a chunk; minus
/// the centre of mass for a body model, whose pose is at it).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudMeshOutput {
    pub body: StudMeshData,
    pub top: StudMeshData,
    pub glass: StudMeshData,
    pub offset: [f32; 3],
}

/// The cells of `chunk` held by opaque bricks: what hides a face or a stud. The
/// chunk's whole occupancy when it holds no glass.
pub fn opaque_occupancy(chunk: &StudChunk, assets: &StudAssets) -> StudOccupancy {
    if !chunk.bricks().any(|b| assets.is_transparent(b.material)) {
        return chunk.occupancy();
    }
    let mut occ = StudOccupancy::default();
    for b in chunk.bricks().filter(|b| !assets.is_transparent(b.material)) {
        for c in b.cells() {
            occ.set(c.map(i32::from), true);
        }
    }
    occ
}

/// Stud meshing as a compute job (`renderer::compute`): everything it reads travels
/// by value, so it runs on a native pool thread or in a Web Worker alike.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StudMeshJob {
    /// One terrain chunk, faces culled inside it and against `neighbours`; with
    /// `detail`, its studs and seam lines too (the chunks near the viewer).
    Chunk { chunk: StudChunk, neighbours: FaceSlabs, detail: bool },
    /// A detached body's bricks (`game::stud_body`), unculled: every face and stud.
    Model(game::stud_body::StudModel),
}

/// Mesh one chunk against its neighbours' slabs (the job's body, also usable in place).
pub fn mesh_chunk(chunk: &StudChunk, neighbours: &FaceSlabs, detail: bool) -> StudMeshOutput {
    let get = |c: [i32; 3]| neighbours.get(c);
    mesh_chunk_with(chunk, &StudNeighbours { get: &get }, detail)
}

/// [`mesh_chunk`] over any reader of the cells past the chunk's faces.
pub(crate) fn mesh_chunk_with(chunk: &StudChunk, n: &StudNeighbours, detail: bool) -> StudMeshOutput {
    let assets = stud_assets();
    if !chunk.bricks().any(|b| assets.is_transparent(b.material)) {
        let (body, top) = opaque_chunk_meshes(chunk, &chunk.occupancy(), n, assets, detail);
        return StudMeshOutput { body, top, ..Default::default() };
    }
    // Opaque bricks among themselves (and the neighbours' opaque cells), as if
    // the glass were not there.
    let opaque = StudChunk::new(chunk.bricks().filter(|b| !assets.is_transparent(b.material)).copied()).unwrap_or_default();
    let occ = opaque.occupancy();
    let (body, top) = opaque_chunk_meshes(&opaque, &occ, n, assets, detail);
    let mut out = StudMeshOutput { body, top, ..Default::default() };
    // Glass: hidden by opaque cells and by glass of its own colour in this chunk.
    let mut glass_cells: BTreeMap<[i32; 3], u16> = BTreeMap::new();
    for b in chunk.bricks().filter(|b| assets.is_transparent(b.material)) {
        let code = game::stud_material_color(b.material);
        glass_cells.extend(b.cells().map(|c| (c.map(i32::from), code)));
    }
    for b in chunk.bricks().filter(|b| assets.is_transparent(b.material)) {
        let code = game::stud_material_color(b.material);
        let hidden = |c: [i32; 3]| occupied(&occ, n, c) || glass_cells.get(&c) == Some(&code);
        let min = b.min.map(i32::from);
        let exposed = exposed_faces_at(min, b.size().map(i32::from), &hidden);
        emit_body(&mut out.glass, min, b, exposed, assets, Alphas::glass(assets, code), &mut |_, _| {});
        emit_studs(&mut out.glass, min, b, &|c| !hidden(c), assets, assets.alpha(code));
    }
    out
}

/// Mesh a body model: every brick whole, in model units, offset so the model's
/// centre of mass (`game::stud_model_mass_properties`, where the body's pose is)
/// lands on the entity.
pub fn mesh_model(model: &game::stud_body::StudModel) -> StudMeshOutput {
    let assets = stud_assets();
    let com = game::stud_body::stud_model_mass_properties(model).local_com;
    let mut out = StudMeshOutput { offset: [-com.x, -com.y, -com.z], ..Default::default() };
    for mb in &model.bricks {
        let b = StudBrick { min: [0; 3], shape: mb.shape, material: mb.material, data: mb.data };
        let min = mb.min.map(i32::from);
        if assets.is_transparent(b.material) {
            let code = game::stud_material_color(b.material);
            emit_body(&mut out.glass, min, &b, [true; 6], assets, Alphas::glass(assets, code), &mut |_, _| {});
            emit_studs(&mut out.glass, min, &b, &|_| true, assets, assets.alpha(code));
            continue;
        }
        emit_body(&mut out.body, min, &b, [true; 6], assets, OPAQUE, &mut |_, _| {});
        emit_studs(&mut out.top, min, &b, &|_| true, assets, OPAQUE.face);
    }
    out
}

impl crate::renderer::compute::ComputeJob for StudMeshJob {
    type Output = StudMeshOutput;
    const KIND: crate::renderer::compute::JobKind = crate::renderer::compute::JobKind::StudMesh;

    fn run(self) -> StudMeshOutput {
        match self {
            StudMeshJob::Chunk { chunk, neighbours, detail } => mesh_chunk(&chunk, &neighbours, detail),
            StudMeshJob::Model(model) => mesh_model(&model),
        }
    }
}

/// FIFO of chunks to remesh, deduplicated; drained at a fixed budget.
#[derive(Debug, Clone)]
pub struct RemeshQueue<K: Ord + Copy> {
    order: std::collections::VecDeque<K>,
    queued: std::collections::BTreeSet<K>,
}

impl<K: Ord + Copy> Default for RemeshQueue<K> {
    fn default() -> Self {
        Self { order: Default::default(), queued: Default::default() }
    }
}

impl<K: Ord + Copy> RemeshQueue<K> {
    /// Queue `k` unless it is already waiting.
    pub fn push(&mut self, k: K) {
        if self.queued.insert(k) {
            self.order.push_back(k);
        }
    }

    /// The oldest `budget` entries, removed.
    pub fn take(&mut self, budget: usize) -> Vec<K> {
        let n = budget.min(self.order.len());
        let out: Vec<K> = self.order.drain(..n).collect();
        for k in &out {
            self.queued.remove(k);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game::stud_catalog::STUD_PARTS;

    fn assets() -> &'static StudAssets {
        stud_assets()
    }

    fn part(number: &str) -> u16 {
        STUD_PARTS.iter().position(|p| p.number == number).unwrap() as u16
    }

    fn brick(number: &str, min: [u8; 3]) -> StudBrick {
        StudBrick { min, shape: StudShape::Part { part: part(number), rot: 0 }, material: 4, data: 0 }
    }

    fn free(_: [i32; 3]) -> bool {
        false
    }

    /// Kept body tri tags of the brick at `min`.
    fn body_tags(chunk: &StudChunk, n: &StudNeighbours, min: [u8; 3]) -> Vec<TriTag> {
        let mut tags = Vec::new();
        body_mesh_with(chunk, &chunk.occupancy(), n, assets(), &mut |b, t| {
            if b.min == min {
                tags.push(t);
            }
        });
        tags
    }

    fn has_face(tags: &[TriTag], d: FaceDir) -> bool {
        tags.contains(&TriTag::Face(d))
    }

    fn lo_tris_per_stud(number: &str) -> usize {
        let lo = &assets().parts.parts[part(number) as usize].lo;
        let first = lo.tris.iter().find_map(|t| match t.tag {
            TriTag::Stud(c) => Some(c),
            _ => None,
        });
        lo.tris.iter().filter(|t| Some(t.tag) == first.map(TriTag::Stud)).count()
    }

    #[test]
    fn lone_brick_keeps_all_faces_and_studs() {
        let chunk = StudChunk::new([brick("3001", [4, 4, 4])]).unwrap();
        let n = StudNeighbours { get: &free };
        let tags = body_tags(&chunk, &n, [4, 4, 4]);
        for d in DIRS {
            assert!(has_face(&tags, d), "missing {d:?}");
        }
        assert!(!body_mesh(&chunk, &n, assets()).is_empty());
        let per = lo_tris_per_stud("3001");
        assert!(per > 0);
        assert_eq!(stud_mesh(&chunk, &n, assets()).indices.len() / 3, 8 * per);
    }

    #[test]
    fn stacked_bricks_cull_shared_faces() {
        let chunk = StudChunk::new([brick("3001", [4, 4, 4]), brick("3001", [4, 7, 4])]).unwrap();
        let n = StudNeighbours { get: &free };
        let lower = body_tags(&chunk, &n, [4, 4, 4]);
        let upper = body_tags(&chunk, &n, [4, 7, 4]);
        assert!(!has_face(&lower, FaceDir::PosY));
        assert!(has_face(&lower, FaceDir::NegY));
        assert!(!has_face(&upper, FaceDir::NegY));
        assert!(!upper.contains(&TriTag::Inner), "the upper's underside is covered");
        assert!(has_face(&upper, FaceDir::PosY));
        // Only the upper brick's studs remain.
        let per = lo_tris_per_stud("3001");
        assert_eq!(stud_mesh(&chunk, &n, assets()).indices.len() / 3, 8 * per);
    }

    #[test]
    fn neighbour_chunk_culls_edge_face() {
        let chunk = StudChunk::new([brick("3005", [31, 0, 5])]).unwrap();
        let open = StudNeighbours { get: &free };
        assert!(has_face(&body_tags(&chunk, &open, [31, 0, 5]), FaceDir::PosX));
        let wall = |c: [i32; 3]| c[0] == 32;
        let walled = StudNeighbours { get: &wall };
        let tags = body_tags(&chunk, &walled, [31, 0, 5]);
        assert!(!has_face(&tags, FaceDir::PosX));
        assert!(has_face(&tags, FaceDir::NegX));
    }

    #[test]
    fn buried_filler_draws_nothing() {
        let filler = StudBrick {
            min: [0, 0, 0],
            shape: StudShape::Filler { size: [STUD_CHUNK_STUDS as u8, STUD_CHUNK_PLATES as u8, STUD_CHUNK_STUDS as u8] },
            material: 4,
            data: 0,
        };
        let chunk = StudChunk::new([filler]).unwrap();
        let all = |_: [i32; 3]| true;
        let n = StudNeighbours { get: &all };
        assert!(body_mesh(&chunk, &n, assets()).is_empty());
        assert!(stud_mesh(&chunk, &n, assets()).is_empty());
        // Open on top: one face (two tris).
        let below = |c: [i32; 3]| c[1] < STUD_CHUNK_PLATES as i32;
        let n = StudNeighbours { get: &below };
        assert_eq!(body_mesh(&chunk, &n, assets()).indices.len(), 6);
    }

    #[test]
    fn rotated_brick_turns_its_faces() {
        // 3004 is 2 x 1; turned once it is 1 x 2, so +X of the part faces +Z.
        let b = StudBrick { min: [0, 0, 0], shape: StudShape::Part { part: part("3004"), rot: 1 }, material: 4, data: 0 };
        assert_eq!(b.size(), [1, 3, 2]);
        let chunk = StudChunk::new([b, brick("3005", [0, 0, 2])]).unwrap();
        let n = StudNeighbours { get: &free };
        let tags = body_tags(&chunk, &n, [0, 0, 0]);
        assert!(!has_face(&tags, FaceDir::PosZ));
        assert!(has_face(&tags, FaceDir::PosX));
        // Every position stays inside the brick's rotated box.
        let m = body_mesh(&StudChunk::new([b]).unwrap(), &n, assets());
        for p in &m.positions {
            assert!(p[0] >= -0.1 && p[0] <= 8.1 && p[2] >= -0.1 && p[2] <= 16.1, "{p:?}");
        }
    }

    /// The pre-job main-thread neighbour read: occupancy past the chunk edge from
    /// the six neighbours' whole occupancies ([`DIRS`] order). The reference the
    /// slab path must agree with.
    fn reference_get(nb: &[Option<StudOccupancy>; 6], c: [i32; 3]) -> bool {
        let split = |v: i32, len: usize| {
            let len = len as i32;
            if v < 0 {
                (-1, v + len)
            } else if v >= len {
                (1, v - len)
            } else {
                (0, v)
            }
        };
        let (dx, x) = split(c[0], STUD_CHUNK_STUDS);
        let (dy, y) = split(c[1], STUD_CHUNK_PLATES);
        let (dz, z) = split(c[2], STUD_CHUNK_STUDS);
        let slot = match (dx, dy, dz) {
            (1, 0, 0) => 0,
            (-1, 0, 0) => 1,
            (0, 1, 0) => 2,
            (0, -1, 0) => 3,
            (0, 0, 1) => 4,
            (0, 0, -1) => 5,
            _ => return false,
        };
        nb[slot].as_ref().is_some_and(|o| o.get([x, y, z]))
    }

    /// A busy chunk: bricks on every face, a filler, a rotated brick, a slope.
    fn busy_chunk() -> StudChunk {
        let top = STUD_CHUNK_PLATES as u8 - 3;
        let mut bricks = vec![
            brick("3001", [0, 0, 0]),
            brick("3001", [28, 0, 30]),
            brick("3005", [31, 10, 5]),
            brick("3005", [0, 10, 5]),
            brick("3005", [7, top, 7]),
            brick("3005", [9, 20, 31]),
            brick("3005", [9, 20, 0]),
            StudBrick::filler([8, 0, 8], [8, 6, 8], 2),
            StudBrick { min: [20, 0, 4], shape: StudShape::Part { part: part("3004"), rot: 1 }, material: 14, data: 0 },
        ];
        if let Some(slope) = STUD_PARTS.iter().position(|p| !p.hull.is_empty()) {
            bricks.push(StudBrick::part_at([16, 30, 16], slope as u16, 2, 1));
        }
        StudChunk::new(bricks).unwrap()
    }

    /// Six neighbour chunks with something along every shared face.
    fn busy_neighbours() -> [Option<StudChunk>; 6] {
        let top = STUD_CHUNK_PLATES as u8 - 1;
        [
            Some(StudChunk::new([StudBrick::filler([0, 10, 5], [1, 3, 1], 4), StudBrick::filler([0, 40, 0], [1, 9, 32], 4)]).unwrap()),
            Some(StudChunk::new([StudBrick::filler([31, 0, 0], [1, 90, 32], 4)]).unwrap()),
            Some(StudChunk::new([StudBrick::filler([7, 0, 7], [1, 1, 1], 4)]).unwrap()),
            Some(StudChunk::new([StudBrick::filler([0, top, 0], [16, 1, 16], 4)]).unwrap()),
            Some(StudChunk::new([StudBrick::filler([9, 20, 0], [1, 3, 1], 4)]).unwrap()),
            None,
        ]
    }

    #[test]
    fn the_job_meshes_exactly_what_the_main_thread_mesher_did() {
        let chunk = busy_chunk();
        let nbs = busy_neighbours();
        // The reference: whole neighbour occupancies behind a closure.
        let occs: [Option<StudOccupancy>; 6] = std::array::from_fn(|d| nbs[d].as_ref().map(StudChunk::occupancy));
        let get = |c: [i32; 3]| reference_get(&occs, c);
        let n = StudNeighbours { get: &get };
        // The job: each neighbour's facing boundary layer only.
        let slabs = FaceSlabs(std::array::from_fn(|d| {
            nbs[d].as_ref().map_or(Slab::Free, |c| FaceSlabs::boundaries(c).0[opposite_dir(d)].clone())
        }));
        for detail in [false, true] {
            let reference = mesh_chunk_with(&chunk, &n, detail);
            assert!(!reference.body.is_empty());
            assert_eq!(reference.top.is_empty(), !detail, "studs and seam lines only with detail");
            let job = StudMeshJob::Chunk { chunk: chunk.clone(), neighbours: slabs.clone(), detail };
            let out = crate::renderer::compute::ComputeJob::run(job);
            assert_eq!(out, reference);
            assert_eq!(out.offset, [0.0; 3]);
            // And it differs from the unculled mesh: the neighbours did hide something.
            let open = mesh_chunk(&chunk, &FaceSlabs::default(), detail);
            assert!(open.body.indices.len() > out.body.indices.len());
            if detail {
                assert!(stud_tris(&open.top) > stud_tris(&out.top), "the stud under the +Y neighbour's brick is culled");
            }
        }
        // The body does not depend on the detail.
        assert_eq!(mesh_chunk(&chunk, &slabs, false).body, mesh_chunk(&chunk, &slabs, true).body);
    }

    #[test]
    fn slabs_answer_as_the_neighbour_occupancy_on_every_face_cell() {
        let nbs = busy_neighbours();
        let occs: [Option<StudOccupancy>; 6] = std::array::from_fn(|d| nbs[d].as_ref().map(StudChunk::occupancy));
        let slabs = FaceSlabs(std::array::from_fn(|d| {
            nbs[d].as_ref().map_or(Slab::Free, |c| FaceSlabs::boundaries(c).0[opposite_dir(d)].clone())
        }));
        assert_eq!(slabs.0[1], Slab::Solid, "a full layer packs to one tag");
        assert_eq!(slabs.0[5], Slab::Free);
        let (w, h) = (STUD_CHUNK_STUDS as i32, STUD_CHUNK_PLATES as i32);
        for x in -1..=w {
            for y in -1..=h {
                for z in -1..=w {
                    let outside = [x == -1 || x == w, y == -1 || y == h, z == -1 || z == w].iter().filter(|&&o| o).count();
                    if outside == 0 {
                        continue;
                    }
                    assert_eq!(slabs.get([x, y, z]), reference_get(&occs, [x, y, z]), "{:?}", [x, y, z]);
                }
            }
        }
    }

    #[test]
    fn job_input_and_output_survive_the_worker_round_trip() {
        use crate::renderer::compute::{decode_output, encode_job, run_encoded, ComputeJob, JobKind};
        let slabs = FaceSlabs(std::array::from_fn(|d| {
            busy_neighbours()[d].as_ref().map_or(Slab::Free, |c| FaceSlabs::boundaries(c).0[opposite_dir(d)].clone())
        }));
        let job = StudMeshJob::Chunk { chunk: busy_chunk(), neighbours: slabs, detail: true };
        let bytes = encode_job(&job);
        assert_eq!(bincode::deserialize::<StudMeshJob>(&bytes).unwrap(), job);
        assert_eq!(JobKind::from_u8(JobKind::StudMesh as u8), Some(JobKind::StudMesh));
        let direct = job.clone().run();
        assert_eq!(decode_output::<StudMeshJob>(&run_encoded(StudMeshJob::KIND, &bytes)), direct);

        let model = game::stud_body::StudModel {
            bricks: vec![game::stud_body::StudModelBrick {
                min: [-3, 100, 40],
                shape: StudShape::Part { part: part("3001"), rot: 3 },
                material: 4,
                data: 7,
            }],
        };
        let job = StudMeshJob::Model(model);
        let direct = job.clone().run();
        assert!(!direct.body.is_empty() && !direct.top.is_empty());
        assert_eq!(decode_output::<StudMeshJob>(&run_encoded(StudMeshJob::KIND, &encode_job(&job))), direct);
    }

    #[test]
    fn hand_built_cases_cull_as_expected() {
        // A buried filler draws nothing; a filler with one open side draws one quad.
        let filler = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 90, 32], 4)]).unwrap();
        let buried = FaceSlabs(std::array::from_fn(|_| Slab::Solid));
        assert!(mesh_chunk(&filler, &buried, true).body.is_empty());
        let mut open_east = buried.clone();
        open_east.0[0] = Slab::Free;
        let m = mesh_chunk(&filler, &open_east, false).body;
        assert_eq!(m.indices.len(), 6);
        assert!(m.positions.iter().all(|p| p[0] == (STUD_CHUNK_STUDS * STUD_UNITS) as f32), "the +X face");
        // A region-edge brick against a neighbour slab holding exactly its cells.
        let edge = StudChunk::new([brick("3005", [31, 0, 5])]).unwrap();
        let tags_with = |slabs: &FaceSlabs| {
            let get = |c: [i32; 3]| slabs.get(c);
            body_tags(&edge, &StudNeighbours { get: &get }, [31, 0, 5])
        };
        assert!(has_face(&tags_with(&FaceSlabs::default()), FaceDir::PosX));
        let wall = StudChunk::new([StudBrick::filler([0, 0, 5], [1, 3, 1], 4)]).unwrap();
        let mut slabs = FaceSlabs::default();
        slabs.0[0] = FaceSlabs::boundaries(&wall).0[1].clone();
        assert!(!has_face(&tags_with(&slabs), FaceDir::PosX));
        // One cell lower than the brick is tall: a cell across is free, the face shows.
        let low = StudChunk::new([StudBrick::filler([0, 0, 5], [1, 2, 1], 4)]).unwrap();
        slabs.0[0] = FaceSlabs::boundaries(&low).0[1].clone();
        assert!(has_face(&tags_with(&slabs), FaceDir::PosX));
        // Stud tops only on free cells: a 2 x 4 brick under a 1 x 1 keeps 7 of 8.
        let per = lo_tris_per_stud("3001");
        let covered = StudChunk::new([brick("3001", [4, 4, 4]), brick("3005", [4, 7, 4])]).unwrap();
        let per_1x1 = lo_tris_per_stud("3005");
        assert_eq!(stud_tris(&mesh_chunk(&covered, &FaceSlabs::default(), true).top), 7 * per + per_1x1);
        // A slope part meshes (its faces are baked triangles, not a box).
        if let Some(slope) = STUD_PARTS.iter().position(|p| !p.hull.is_empty()) {
            let c = StudChunk::new([StudBrick::part_at([4, 0, 4], slope as u16, 0, 4)]).unwrap();
            assert!(mesh_chunk(&c, &FaceSlabs::default(), false).body.indices.len() > 12 * 3);
        }
    }

    /// A slope standing on a brick (its underside covered) still shows its slanted
    /// face, on every path that draws it; buried on the sides it leans toward, not.
    #[test]
    fn a_slope_on_a_brick_keeps_its_slanted_face() {
        let slope = |min: [u8; 3]| brick("3040", min);
        let base = brick("3001", [4, 0, 4]);
        let chunk = StudChunk::new([base, slope([4, 3, 4])]).unwrap();
        let n = StudNeighbours { get: &free };
        let tags = body_tags(&chunk, &n, [4, 3, 4]);
        assert!(!has_face(&tags, FaceDir::NegY), "the underside is covered");
        assert!(!tags.contains(&TriTag::Inner));
        assert!(tags.contains(&TriTag::Outer), "the slanted face is drawn");

        // The terrain path: the slanted face is in the body mesh as slanted normals.
        let slanted = |d: &StudMeshData| d.normals.iter().filter(|n| n[1] > 0.5 && n[1] < 0.9).count();
        assert!(slanted(&mesh_chunk(&chunk, &FaceSlabs::default(), false).body) >= 3);

        // Covered on top and in front (the two sides the 3040 face leans toward: +Y, -Z): hidden.
        let buried = StudChunk::new([
            base,
            slope([4, 3, 4]),
            brick("3001", [4, 6, 4]),
            brick("3010", [4, 3, 3]),
        ])
        .unwrap();
        assert!(!body_tags(&buried, &n, [4, 3, 4]).contains(&TriTag::Outer));
    }

    /// LDraw 47 (Trans_Clear) and 33 (Trans_Dark_Blue): two glass colours.
    const CLEAR: u16 = 47;
    const BLUE_GLASS: u16 = 33;

    fn tris(d: &StudMeshData) -> usize {
        d.indices.len() / 3
    }

    /// Triangles of a detail mesh that are seam-line quads (four vertices, two
    /// triangles, the marker alpha) ...
    fn seam_tris(d: &StudMeshData) -> usize {
        d.colors.iter().filter(|c| c[3] == STUD_EDGE_ALPHA).count() / 2
    }

    /// ... and those that are studs (the rest: three unshared vertices each).
    fn stud_tris(d: &StudMeshData) -> usize {
        tris(d) - seam_tris(d)
    }

    /// Summed area of a mesh's triangles, sim units squared.
    fn area(d: &StudMeshData) -> f32 {
        d.indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| d.positions[i as usize]);
                let n = cross(sub(b, a), sub(c, a));
                dot(n, n).sqrt() / 2.0
            })
            .sum()
    }

    #[test]
    fn the_colour_table_says_which_materials_are_glass() {
        let a = assets();
        assert!(a.is_transparent(CLEAR) && a.is_transparent(BLUE_GLASS));
        assert!((a.alpha(CLEAR) - 128.0 / 255.0).abs() < 1e-6);
        assert!(!a.is_transparent(4), "red plastic");
        assert_eq!(a.alpha(4), 1.0);
        for m in game::STUD_NATURALS.iter().enumerate().map(|(i, _)| game::NATURAL_BASE + i as u16) {
            let glass = a.colors[&game::stud_material_color(m)].alpha < 255;
            assert_eq!(a.is_transparent(m), glass, "natural {m:#x} follows its colour");
        }
    }

    #[test]
    fn glass_bricks_go_to_the_glass_mesh_and_opaque_faces_behind_them_stay() {
        // An opaque box with a glass box against its +X face and another on top.
        let rock = StudBrick::filler([0, 0, 0], [2, 3, 2], 4);
        let side = StudBrick::filler([2, 0, 0], [2, 3, 2], CLEAR);
        let lid = StudBrick::filler([0, 3, 0], [2, 3, 2], CLEAR);
        let out = mesh_chunk(&StudChunk::new([rock, side, lid]).unwrap(), &FaceSlabs::default(), false);
        let alone = mesh_chunk(&StudChunk::new([rock]).unwrap(), &FaceSlabs::default(), false);
        assert_eq!(out.body, alone.body, "the opaque box keeps every face, as if the glass were not there");
        assert_eq!(tris(&out.body), 12);
        assert!(alone.glass.is_empty());
        // Each glass box loses the one face against the rock; the two glass boxes
        // touch only along an edge.
        assert_eq!(tris(&out.glass), 2 * (12 - 2));
        let a = assets().alpha(CLEAR);
        assert!(out.glass.colors.iter().all(|c| c[3] == a), "glass carries its colour's opacity");
        assert!(out.body.colors.iter().all(|c| c[3] == 1.0));
        // A glass-only chunk has no opaque mesh at all.
        let pane = mesh_chunk(&StudChunk::new([side]).unwrap(), &FaceSlabs::default(), false);
        assert!(pane.body.is_empty() && pane.top.is_empty());
        assert_eq!(tris(&pane.glass), 12);
    }

    #[test]
    fn glass_culls_against_its_own_colour_only() {
        let a = StudBrick::filler([0, 0, 0], [2, 3, 2], CLEAR);
        let same = StudBrick::filler([2, 0, 0], [2, 3, 2], CLEAR);
        let other = StudBrick::filler([2, 0, 0], [2, 3, 2], BLUE_GLASS);
        let one_pane = mesh_chunk(&StudChunk::new([a, same]).unwrap(), &FaceSlabs::default(), false);
        assert_eq!(tris(&one_pane.glass), 24 - 4, "the wall between two bricks of one glass is gone");
        let two_panes = mesh_chunk(&StudChunk::new([a, other]).unwrap(), &FaceSlabs::default(), false);
        assert_eq!(tris(&two_panes.glass), 24, "two colours: both faces drawn");
    }

    #[test]
    fn studs_show_under_glass_and_glass_parts_keep_their_own() {
        let per = lo_tris_per_stud("3005");
        let under = brick("3005", [4, 0, 4]);
        let glass_part = StudBrick { material: CLEAR, ..brick("3005", [4, 3, 4]) };
        let out = mesh_chunk(&StudChunk::new([under, glass_part]).unwrap(), &FaceSlabs::default(), true);
        assert_eq!(stud_tris(&out.top), per, "the opaque brick's stud is seen through the glass");
        let glass_alone = mesh_chunk(&StudChunk::new([glass_part]).unwrap(), &FaceSlabs::default(), true);
        assert!(tris(&glass_alone.glass) > per, "a glass part: its body and its stud");
        assert!(tris(&out.glass) < tris(&glass_alone.glass), "its underside against the opaque brick is culled");
        // An opaque brick on a glass one: the glass stud under it is hidden.
        let on_glass = brick("3005", [4, 6, 4]);
        let capped = mesh_chunk(&StudChunk::new([glass_part, on_glass]).unwrap(), &FaceSlabs::default(), true);
        assert!(tris(&capped.glass) + per <= tris(&glass_alone.glass), "no stud, no top face");
        assert_eq!(stud_tris(&capped.top), per, "the opaque brick on top keeps its stud");
        // Glass is meshed whole whatever the detail.
        assert_eq!(mesh_chunk(&StudChunk::new([glass_part]).unwrap(), &FaceSlabs::default(), false).glass, glass_alone.glass);
    }

    #[test]
    fn seam_lines_are_marked_and_slabs_hold_opaque_cells_only() {
        // A part has baked edges: its seam quads carry the marker alpha, its faces
        // and studs 1. The seam lines are in the detail mesh, never in the body.
        let out = mesh_chunk(&StudChunk::new([brick("3001", [4, 4, 4])]).unwrap(), &FaceSlabs::default(), true);
        assert!(out.body.colors.iter().all(|c| c[3] == 1.0), "faces");
        assert!(out.top.colors.iter().any(|c| c[3] == STUD_EDGE_ALPHA), "seam lines");
        assert!(out.top.colors.iter().any(|c| c[3] == 1.0), "studs");
        assert!(out.top.colors.iter().all(|c| c[3] == 1.0 || c[3] == STUD_EDGE_ALPHA));
        // The whole-brick mesher (ghost, bodies) keeps them in the body.
        let whole = body_mesh(&StudChunk::new([brick("3001", [4, 4, 4])]).unwrap(), &StudNeighbours { get: &free }, assets());
        assert_eq!(seam_tris(&whole), seam_tris(&out.top));
        assert!(seam_tris(&whole) > 0);
        // A neighbour chunk full of glass is "free" across the shared face...
        let glass_wall = StudChunk::new([StudBrick::filler([0, 0, 0], [1, 90, 32], CLEAR)]).unwrap();
        assert_eq!(FaceSlabs::boundaries(&glass_wall).0[1], Slab::Free);
        // ... so the opaque face looking at it is drawn.
        let edge = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 90, 32], 4)]).unwrap();
        let mut slabs = FaceSlabs(std::array::from_fn(|_| Slab::Solid));
        slabs.0[0] = FaceSlabs::boundaries(&glass_wall).0[1].clone();
        assert_eq!(tris(&mesh_chunk(&edge, &slabs, false).body), 2, "only the face behind the glass");
        // A model routes its glass bricks the same way.
        let model = game::stud_body::StudModel {
            bricks: vec![
                game::stud_body::StudModelBrick { min: [0, 0, 0], shape: StudShape::Filler { size: [1, 1, 1] }, material: 4, data: 0 },
                game::stud_body::StudModelBrick { min: [1, 0, 0], shape: StudShape::Filler { size: [1, 1, 1] }, material: CLEAR, data: 0 },
            ],
        };
        let m = mesh_model(&model);
        assert_eq!((tris(&m.body), tris(&m.glass)), (12, 12));
    }

    /// A hole dug into filler ground and re-bricked (`game::stud_rebrick`): its floor
    /// shows stud tops, its walls and floor seam lines; the bare filler hole had neither.
    #[test]
    fn a_re_bricked_hole_shows_studs_and_seams() {
        let mut chunk = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 30, 32], game::MAT_STONE)]).unwrap();
        // One tile dug from the top of the ground: studs 8..10 x 4..6, plates 24..30.
        chunk.carve([8, 24, 4], [10, 30, 6]);
        let slabs = FaceSlabs(std::array::from_fn(|_| Slab::Solid));
        let bare = mesh_chunk(&chunk, &slabs, true);
        assert!(!bare.body.is_empty(), "the hole's faces");
        assert!(bare.top.is_empty(), "fillers have no studs and no seams");
        // The slots behind the floor and the four walls.
        for cell in [[8, 23, 4], [7, 24, 4], [10, 24, 4], [8, 24, 3], [8, 24, 6], [7, 27, 4], [10, 27, 4], [8, 27, 3], [8, 27, 6]] {
            let (lo, hi) = game::rebrick_slot(0, cell);
            chunk.rebrick_box(lo, hi);
        }
        let out = mesh_chunk(&chunk, &slabs, true);
        assert_eq!(tris(&out.body), tris(&bare.body), "the same faces show");
        // Stud tops only on up-facing free cells: the floor's four cells, and the top
        // of the wall bricks where they are the ground's surface (plate 30).
        let (floor_y, surface_y) = (plate_units(24), plate_units(30));
        let studs: Vec<[f32; 3]> = out.top.positions.iter().zip(&out.top.colors).filter(|(_, c)| c[3] == 1.0).map(|(p, _)| *p).collect();
        let s = STUD_UNITS as f32;
        let on_floor = |p: &[f32; 3]| p[1] < plate_units(25);
        assert!(studs.iter().any(on_floor), "stud tops on the floor");
        for p in &studs {
            if on_floor(p) {
                assert!(p[1] >= floor_y - 0.01, "a stud under the floor: {p:?}");
                assert!((8.0 * s..=10.0 * s).contains(&p[0]) && (4.0 * s..=6.0 * s).contains(&p[2]), "a stud under a wall: {p:?}");
            } else {
                assert!(p[1] >= surface_y - 0.01, "a stud inside a wall: {p:?}");
            }
        }
        assert!(seam_tris(&out.top) > 0, "seam lines between the bricks");
    }

    /// The face area of a box of `studs` x `plates` x `studs` cells, sim units squared.
    fn box_area([x, y, z]: [f32; 3]) -> f32 {
        let (x, y, z) = (x * STUD_UNITS as f32, plate_units(1) * y, z * STUD_UNITS as f32);
        2.0 * (x * y + y * z + x * z)
    }

    #[test]
    fn coplanar_faces_of_one_colour_merge_across_bricks() {
        let open = FaceSlabs::default();
        // A lone 2 x 4 brick: six quads of four shared vertices, no baked triangles.
        let one = mesh_chunk(&StudChunk::new([brick("3001", [4, 3, 4])]).unwrap(), &open, false);
        assert_eq!((tris(&one.body), one.body.positions.len()), (12, 24));
        assert!(one.top.is_empty(), "no detail asked");
        // A floor of 2 x 4 bricks over the whole chunk, one course: still one box.
        let size = STUD_PARTS[part("3001") as usize].size;
        assert_eq!(size, [4, 3, 2]);
        let floor: Vec<StudBrick> = (0..STUD_CHUNK_STUDS as u8 / 4)
            .flat_map(|i| (0..STUD_CHUNK_STUDS as u8 / 2).map(move |j| brick("3001", [i * 4, 0, j * 2])))
            .collect();
        assert_eq!(floor.len(), 128);
        let chunk = StudChunk::new(floor.clone()).unwrap();
        let m = mesh_chunk(&chunk, &open, false).body;
        assert_eq!((tris(&m), m.positions.len()), (12, 24), "128 bricks, six quads");
        assert!((area(&m) - box_area([32.0, 3.0, 32.0])).abs() < 1.0);
        // A second course on it, same colour: the sides merge up through both.
        let two: Vec<StudBrick> = floor.iter().copied().chain(floor.iter().map(|b| StudBrick { min: [b.min[0], 3, b.min[2]], ..*b })).collect();
        assert_eq!(tris(&mesh_chunk(&StudChunk::new(two).unwrap(), &open, false).body), 12);
        // Another colour does not merge: one brick of the floor recoloured splits
        // the top and the bottom around it, and (it is on the chunk's corner) two sides.
        let mut patch = floor.clone();
        patch[0].material = 14;
        let m = mesh_chunk(&StudChunk::new(patch).unwrap(), &open, false).body;
        assert!(tris(&m) > 12);
        assert!((area(&m) - box_area([32.0, 3.0, 32.0])).abs() < 1.0, "the same surface, cut differently");
        let (red, _) = assets().brick_colors(game::stud_material_color(4));
        let (yellow, _) = assets().brick_colors(game::stud_material_color(14));
        assert_ne!(red, yellow);
        assert!(m.colors.iter().any(|c| c[..3] == yellow[..3]) && m.colors.iter().any(|c| c[..3] == red[..3]));
        // The detail of the floor: every stud, and seam lines.
        let d = mesh_chunk(&chunk, &open, true);
        assert_eq!(d.body, mesh_chunk(&chunk, &open, false).body);
        assert_eq!(stud_tris(&d.top), 128 * 8 * lo_tris_per_stud("3001"));
        assert!(seam_tris(&d.top) > 0);
        // They are the whole-brick mesher's seam lines and studs, brick by brick.
        let n = StudNeighbours { get: &free };
        assert_eq!(seam_tris(&body_mesh(&chunk, &n, assets())), seam_tris(&d.top));
        assert_eq!(stud_mesh(&chunk, &n, assets()).indices.len() / 3, stud_tris(&d.top));
    }

    #[test]
    fn a_face_shows_only_where_the_cell_across_is_free() {
        let open = FaceSlabs::default();
        // A 1 x 1 brick on a 2 x 4: the big brick's top shows around it only, and
        // the small brick's underside not at all.
        let chunk = StudChunk::new([brick("3001", [4, 0, 4]), brick("3005", [4, 3, 4])]).unwrap();
        let m = mesh_chunk(&chunk, &open, false).body;
        let s = STUD_UNITS as f32;
        let hidden = 2.0 * s * s;
        assert!((area(&m) - (box_area([4.0, 3.0, 2.0]) + box_area([1.0, 3.0, 1.0]) - hidden)).abs() < 1.0);
        // No vertex lies strictly inside the covered cell's top face.
        let y = plate_units(3);
        assert!(!m.positions.iter().any(|p| p[1] == y && p[0] > 4.0 * s && p[0] < 5.0 * s && p[2] > 4.0 * s && p[2] < 5.0 * s));
        // Across a chunk face: a full chunk, buried but for its east side, against a neighbour whose
        // facing layer is solid up to half its height shows the upper half only.
        let column = StudChunk::new([StudBrick::filler([0, 0, 0], [32, 90, 32], 4)]).unwrap();
        let half = StudChunk::new([StudBrick::filler([0, 0, 0], [1, 45, 32], 4)]).unwrap();
        let mut slabs = FaceSlabs(std::array::from_fn(|_| Slab::Solid));
        slabs.0[0] = FaceSlabs::boundaries(&half).0[1].clone();
        let m = mesh_chunk(&column, &slabs, false).body;
        assert_eq!(tris(&m), 2, "one quad");
        let (lo, hi) = m.positions.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
        assert_eq!((lo, hi), (plate_units(45), plate_units(90)));
        assert!(m.positions.iter().all(|p| p[0] == (STUD_CHUNK_STUDS * STUD_UNITS) as f32));
        assert!(m.normals.iter().all(|n| *n == [1.0, 0.0, 0.0]));
    }

    #[test]
    fn every_merged_quad_faces_outward() {
        // A box in the middle of the chunk: each quad's winding agrees with its
        // normal, and its normal points away from the box's centre.
        let chunk = StudChunk::new([StudBrick::filler([8, 9, 8], [4, 6, 4], 4)]).unwrap();
        let m = mesh_chunk(&chunk, &FaceSlabs::default(), false).body;
        let centre = [10.0 * STUD_UNITS as f32, plate_units(12), 10.0 * STUD_UNITS as f32];
        assert_eq!(tris(&m), 12);
        for t in m.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize]);
            let n = m.normals[t[0] as usize];
            assert!(dot(cross(sub(b, a), sub(c, a)), n) > 0.0, "counter-clockwise seen from the normal's side");
            assert!(dot(sub(a, centre), n) > 0.0, "outward");
        }
    }

    #[test]
    fn mesher_respects_frame_budget() {
        let mut q = RemeshQueue::default();
        for k in 0..20u32 {
            q.push(k);
            q.push(k); // deduplicated
        }
        assert_eq!(q.take(8), (0..8).collect::<Vec<_>>());
        assert_eq!(q.take(8).len(), 8);
        assert_eq!(q.take(8), (16..20).collect::<Vec<_>>());
        assert!(q.is_empty());
    }
}
