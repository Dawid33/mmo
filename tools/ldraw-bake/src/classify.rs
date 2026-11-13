//! Turn flattened LDraw geometry into the baked part: sim units, Y up, origin at the bbox min corner,
//! every triangle and edge tagged (`Face` / `Stud` / `Outer` / `Inner`), plus the cell size, stud cells and hull.

use crate::flatten::{Detail, Flat, Library, flatten, moved_to};
use anyhow::{Result, bail};
use game::stud_assets::{BakedEdge, BakedLod, BakedTri, EdgeTag, FaceDir, TriColor, TriTag};
use std::collections::BTreeSet;

/// 1 LDU in sim units.
pub const UNITS_PER_LDU: f64 = 0.4;
/// Stud pitch / plate height in LDU.
const CELL_LDU: [f64; 3] = [20.0, 8.0, 20.0];
/// Plane-membership tolerance, LDU (= 1e-3 sim units).
const PLANE_TOL_LDU: f64 = 1e-3 / UNITS_PER_LDU;

const DIRS: [FaceDir; 6] = [FaceDir::PosX, FaceDir::NegX, FaceDir::PosY, FaceDir::NegY, FaceDir::PosZ, FaceDir::NegZ];

/// Owned twin of `game::stud_assets::StudPartDef`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartDef {
    /// The number as listed in `parts.txt` (an alias keeps its listed number).
    pub number: String,
    pub name: String,
    pub size: [u8; 3],
    pub studs: Vec<[u8; 3]>,
    pub hull: Vec<[i16; 3]>,
}

pub struct Baked {
    pub def: PartDef,
    pub hi: BakedLod,
    pub lo: BakedLod,
    /// The file actually baked (`3023b` for the listed `3023`).
    pub source: String,
    pub author: String,
    pub license: String,
}

fn axis_sign(d: FaceDir) -> (usize, f64) {
    match d {
        FaceDir::PosX => (0, 1.0),
        FaceDir::NegX => (0, -1.0),
        FaceDir::PosY => (1, 1.0),
        FaceDir::NegY => (1, -1.0),
        FaceDir::PosZ => (2, 1.0),
        FaceDir::NegZ => (2, -1.0),
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn len(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// A triangle in part-local LDU, Y up, CCW from outside.
struct LocalTri {
    v: [[f64; 3]; 3],
    normal: [f64; 3],
    area: f64,
    color: TriColor,
    stud: Option<[f64; 3]>,
}

struct LocalLod {
    tris: Vec<LocalTri>,
    edges: Vec<([f64; 3], [f64; 3], Option<[f64; 3]>)>,
}

/// LDraw (Y down) -> Y up, shifted by `min`. The Y mirror reverses winding, so two vertices swap.
fn to_local(flat: &Flat, min: [f64; 3]) -> LocalLod {
    let map = |p: [f64; 3]| [p[0] - min[0], -p[1] - min[1], p[2] - min[2]];
    let mut tris = Vec::new();
    for t in &flat.tris {
        let (a, b, c) = (map(t.v[0]), map(t.v[2]), map(t.v[1]));
        let n = cross(sub(b, a), sub(c, a));
        let l = len(n);
        if l < 1e-9 {
            continue;
        }
        tris.push(LocalTri {
            v: [a, b, c],
            normal: [n[0] / l, n[1] / l, n[2] / l],
            area: l / 2.0,
            color: t.color,
            stud: t.stud.map(|s| map(s)),
        });
    }
    let edges = flat.edges.iter().map(|e| (map(e.a), map(e.b), e.stud.map(|s| map(s)))).collect();
    LocalLod { tris, edges }
}

/// The y-up LDU bbox of the non-stud geometry of a flat part.
fn nonstud_bbox(flat: &Flat) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for t in flat.tris.iter().filter(|t| t.stud.is_none()) {
        for p in t.v {
            let p = [p[0], -p[1], p[2]];
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
    }
    (lo, hi)
}

fn plane_value(d: FaceDir, ext: [f64; 3]) -> f64 {
    let (axis, sign) = axis_sign(d);
    if sign > 0.0 { ext[axis] } else { 0.0 }
}

fn on_plane(p: [f64; 3], d: FaceDir, ext: [f64; 3]) -> bool {
    let (axis, _) = axis_sign(d);
    (p[axis] - plane_value(d, ext)).abs() <= PLANE_TOL_LDU
}

fn face_of(t: &LocalTri, ext: [f64; 3]) -> Option<FaceDir> {
    DIRS.into_iter().find(|&d| {
        let (axis, sign) = axis_sign(d);
        t.normal[axis] * sign >= 0.999 && t.v.iter().all(|&p| on_plane(p, d, ext))
    })
}

fn cell_of(stud: [f64; 3], size: [u8; 3]) -> [u8; 3] {
    let cx = ((stud[0] / CELL_LDU[0] + 1e-6).floor().max(0.0) as u8).min(size[0].saturating_sub(1));
    let cz = ((stud[2] / CELL_LDU[2] + 1e-6).floor().max(0.0) as u8).min(size[2].saturating_sub(1));
    [cx, size[1], cz]
}

fn f32v(p: [f64; 3]) -> [f32; 3] {
    [(p[0] * UNITS_PER_LDU) as f32, (p[1] * UNITS_PER_LDU) as f32, (p[2] * UNITS_PER_LDU) as f32]
}

fn tag_edge(a: [f64; 3], b: [f64; 3], stud: Option<[f64; 3]>, ext: [f64; 3], size: [u8; 3]) -> EdgeTag {
    if let Some(s) = stud {
        return EdgeTag::Stud(cell_of(s, size));
    }
    let mut mask = 0u8;
    for d in DIRS {
        if on_plane(a, d, ext) && on_plane(b, d, ext) {
            mask |= 1 << d as u8;
        }
    }
    if mask == 0 { EdgeTag::Inner } else { EdgeTag::Faces(mask) }
}

/// Is `t` on the outside of the body: does every non-stud vertex of `l` lie on or behind its plane?
/// (A supporting plane of the body's convex hull — the slanted face of a slope, not the hollow under it.)
fn is_outer(t: &LocalTri, l: &LocalLod) -> bool {
    l.tris.iter().filter(|o| o.stud.is_none()).flat_map(|o| o.v).all(|p| {
        let d = sub(p, t.v[0]);
        t.normal[0] * d[0] + t.normal[1] * d[1] + t.normal[2] * d[2] <= PLANE_TOL_LDU
    })
}

fn bake_lod(l: &LocalLod, ext: [f64; 3], size: [u8; 3]) -> BakedLod {
    let tris = l
        .tris
        .iter()
        .map(|t| {
            let tag = match t.stud {
                Some(s) => TriTag::Stud(cell_of(s, size)),
                None => match face_of(t, ext) {
                    Some(d) => TriTag::Face(d),
                    None if is_outer(t, l) => TriTag::Outer,
                    None => TriTag::Inner,
                },
            };
            BakedTri {
                pos: [f32v(t.v[0]), f32v(t.v[1]), f32v(t.v[2])],
                normal: [t.normal[0] as f32, t.normal[1] as f32, t.normal[2] as f32],
                color: t.color,
                tag,
            }
        })
        .collect();
    let edges = l
        .edges
        .iter()
        .map(|&(a, b, s)| BakedEdge { a: f32v(a), b: f32v(b), tag: tag_edge(a, b, s, ext, size) })
        .collect();
    BakedLod { tris, edges }
}

/// Does the geometry fill its bbox? Every face except the (hollow) bottom must be fully covered.
fn is_box(l: &LocalLod, ext: [f64; 3]) -> bool {
    DIRS.into_iter().filter(|&d| d != FaceDir::NegY).all(|d| {
        let (axis, _) = axis_sign(d);
        let full = ext[(axis + 1) % 3] * ext[(axis + 2) % 3];
        let covered: f64 = l
            .tris
            .iter()
            .filter(|t| t.stud.is_none() && face_of(t, ext) == Some(d))
            .map(|t| t.area)
            .sum();
        covered >= full * 0.999
    })
}

/// Vertices of the convex hull of integer points (exact integer arithmetic): a point is kept when it is
/// a 2D hull vertex of some supporting plane's point set.
fn hull_vertices(points: &BTreeSet<[i16; 3]>) -> Vec<[i16; 3]> {
    let pts: Vec<[i64; 3]> = points.iter().map(|p| [p[0] as i64, p[1] as i64, p[2] as i64]).collect();
    let n = pts.len();
    let mut out: BTreeSet<[i16; 3]> = BTreeSet::new();
    let mut done: BTreeSet<Vec<usize>> = BTreeSet::new();
    let sub = |a: [i64; 3], b: [i64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cr = |a: [i64; 3], b: [i64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                let nrm = cr(sub(pts[j], pts[i]), sub(pts[k], pts[i]));
                if nrm == [0, 0, 0] {
                    continue;
                }
                let (mut pos, mut neg) = (false, false);
                let mut face = Vec::new();
                for (m, p) in pts.iter().enumerate() {
                    let d = sub(*p, pts[i]);
                    let dot = nrm[0] * d[0] + nrm[1] * d[1] + nrm[2] * d[2];
                    pos |= dot > 0;
                    neg |= dot < 0;
                    if dot == 0 {
                        face.push(m);
                    }
                }
                if (pos && neg) || !done.insert(face.clone()) {
                    continue;
                }
                // Drop the axis along which the normal is largest, hull in the other two.
                let drop = (0..3).max_by_key(|&a| nrm[a].abs()).unwrap();
                let (u, v) = ((drop + 1) % 3, (drop + 2) % 3);
                let mut q: Vec<(i64, i64, usize)> = face.iter().map(|&m| (pts[m][u], pts[m][v], m)).collect();
                q.sort();
                q.dedup();
                let turn = |o: &(i64, i64, usize), a: &(i64, i64, usize), b: &(i64, i64, usize)| {
                    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
                };
                let mut hull: Vec<(i64, i64, usize)> = Vec::new();
                for pass in 0..2 {
                    let start = hull.len();
                    let it: Vec<_> = if pass == 0 { q.clone() } else { q.iter().rev().cloned().collect() };
                    for p in it {
                        while hull.len() >= start + 2 && turn(&hull[hull.len() - 2], &hull[hull.len() - 1], &p) <= 0 {
                            hull.pop();
                        }
                        hull.push(p);
                    }
                    hull.pop();
                }
                for h in hull {
                    out.insert(points.iter().nth(h.2).copied().unwrap());
                }
            }
        }
    }
    out.into_iter().collect()
}

fn header_line(text: &str, key: &str) -> String {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix('0').map(str::trim))
        .find_map(|l| l.strip_prefix(key).map(|r| r.trim().to_string()))
        .unwrap_or_default()
}

/// Bake one listed part (following `~Moved to` aliases, keeping the listed number).
pub fn bake_part(lib: &Library, listed: &str) -> Result<Baked> {
    let mut source = listed.to_lowercase();
    let mut text = lib.part_text(&source)?;
    for _ in 0..4 {
        match moved_to(&text) {
            Some(t) => {
                source = t;
                text = lib.part_text(&source)?;
            }
            None => break,
        }
    }
    if moved_to(&text).is_some() {
        bail!("alias chain too long for {listed}");
    }
    let name = text
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix('0'))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let hi_flat = flatten(lib, &source, Detail::Hi)?;
    let lo_flat = flatten(lib, &source, Detail::Lo)?;
    if hi_flat.tris.iter().all(|t| t.stud.is_some()) {
        bail!("{listed}: no body geometry");
    }
    let (min, max) = nonstud_bbox(&hi_flat);
    let ext = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let mut size = [0u8; 3];
    for i in 0..3 {
        size[i] = (ext[i] / CELL_LDU[i]).round() as u8;
        if size[i] == 0 {
            bail!("{listed}: empty bbox axis {i}");
        }
    }
    let hi_local = to_local(&hi_flat, min);
    let lo_local = to_local(&lo_flat, min);
    let hi = bake_lod(&hi_local, ext, size);
    let lo = bake_lod(&lo_local, ext, size);

    let studs_of = |l: &LocalLod| -> Vec<[u8; 3]> {
        let s: BTreeSet<[u8; 3]> = l.tris.iter().filter_map(|t| t.stud).map(|s| cell_of(s, size)).collect();
        s.into_iter().collect()
    };
    let studs = studs_of(&hi_local);
    if studs != studs_of(&lo_local) {
        bail!("{listed}: hi and lo detail disagree on the stud cells");
    }

    let hull = if is_box(&hi_local, ext) {
        Vec::new()
    } else {
        let mut set = BTreeSet::new();
        for t in &hi_local.tris {
            if t.stud.is_none() && face_of(t, ext).is_some() {
                for p in t.v {
                    set.insert([p[0].round() as i16, p[1].round() as i16, p[2].round() as i16]);
                }
            }
        }
        hull_vertices(&set)
    };

    Ok(Baked {
        def: PartDef { number: listed.to_string(), name, size, studs, hull },
        hi,
        lo,
        source,
        author: header_line(&text, "Author:"),
        license: header_line(&text, "!LICENSE"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn lib() -> Option<Library> {
        let dir = std::env::var("LDRAW_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/ldraw"));
        if dir.join("parts").is_dir() {
            Some(Library::new(dir))
        } else {
            eprintln!("skipping: no LDraw library at {}", dir.display());
            None
        }
    }

    macro_rules! lib_or_skip {
        () => {
            match lib() {
                Some(l) => l,
                None => return,
            }
        };
    }

    fn bbox(tris: &[BakedTri], keep: impl Fn(&BakedTri) -> bool) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for t in tris.iter().filter(|t| keep(t)) {
            for p in t.pos {
                for i in 0..3 {
                    lo[i] = lo[i].min(p[i]);
                    hi[i] = hi[i].max(p[i]);
                }
            }
        }
        (lo, hi)
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
    }

    #[test]
    fn brick_2x4_matches_ldraw() {
        let lib = lib_or_skip!();
        let p = bake_part(&lib, "3001").unwrap();
        assert_eq!(p.def.size, [4, 3, 2]); // 80 x 24 x 40 LDU
        assert_eq!(p.def.studs.len(), 8);
        assert!(p.def.studs.iter().all(|s| s[1] == 3));
        assert!(p.def.hull.is_empty()); // box part
        let (lo, hi) = bbox(&p.hi.tris, |t| !matches!(t.tag, TriTag::Stud(_)));
        assert!(near(lo, [0.0, 0.0, 0.0]) && near(hi, [32.0, 9.6, 16.0]), "{lo:?} {hi:?}");
        let studs = |l: &BakedLod| l.tris.iter().filter(|t| matches!(t.tag, TriTag::Stud(_))).count();
        assert!(studs(&p.hi) > studs(&p.lo));
        assert!(studs(&p.lo) > 0);
    }

    #[test]
    fn plate_1x2_is_one_plate_tall() {
        let lib = lib_or_skip!();
        let p = bake_part(&lib, "3023").unwrap();
        assert_eq!(p.def.size, [2, 1, 1]);
        assert_eq!(p.def.number, "3023");
        assert_eq!(p.source, "3023b");
    }

    #[test]
    fn slope_has_hull_and_one_stud_row() {
        let lib = lib_or_skip!();
        let p = bake_part(&lib, "3040").unwrap();
        assert!(!p.def.hull.is_empty());
        assert_eq!(p.def.studs.len(), 1);
    }

    /// The slanted face of a slope is on no bbox face, yet it is the outside of the part: it must not be
    /// tagged like the hollow underside (the mesher draws `Inner` only where the underside shows).
    #[test]
    fn slope_face_is_outer_and_the_hollow_stays_inner() {
        let lib = lib_or_skip!();
        for number in ["3040", "3039"] {
            let p = bake_part(&lib, number).unwrap();
            for lod in [&p.hi, &p.lo] {
                let slanted = |t: &&BakedTri| t.normal[1] > 0.5 && t.normal[2] < -0.5;
                let outer: Vec<_> = lod.tris.iter().filter(|t| t.tag == TriTag::Outer).collect();
                assert!(!outer.is_empty(), "{number}: no outer triangle");
                assert!(outer.iter().all(slanted), "{number}: an outer triangle that is not the slanted face");
                assert!(
                    lod.tris.iter().filter(slanted).all(|t| t.tag == TriTag::Outer),
                    "{number}: a slanted outside triangle is not tagged Outer"
                );
            }
            assert!(p.hi.tris.iter().any(|t| t.tag == TriTag::Inner), "{number}: the hollow underside is gone");
        }
        // A box part has no outside but its bbox faces.
        let brick = bake_part(&lib, "3001").unwrap();
        assert!(brick.hi.tris.iter().all(|t| t.tag != TriTag::Outer));
        assert!(brick.hi.tris.iter().any(|t| t.tag == TriTag::Inner));
    }

    #[test]
    fn every_face_tri_lies_on_its_bbox_plane() {
        let lib = lib_or_skip!();
        let p = bake_part(&lib, "3001").unwrap();
        let ext = [32.0f32, 9.6, 16.0];
        let mut seen = BTreeSet::new();
        for t in &p.hi.tris {
            let TriTag::Face(d) = t.tag else { continue };
            seen.insert(d);
            let (axis, sign) = axis_sign(d);
            let plane = if sign > 0.0 { ext[axis] } else { 0.0 };
            assert!(t.pos.iter().all(|v| (v[axis] - plane).abs() < 1e-4), "{d:?} {t:?}");
            assert!(t.normal[axis] * sign as f32 > 0.99, "{d:?} normal {:?}", t.normal);
        }
        assert_eq!(seen.len(), 6, "all six bbox faces present: {seen:?}");
    }

    #[test]
    fn bake_is_deterministic() {
        let lib = lib_or_skip!();
        let a = bake_part(&lib, "3040").unwrap();
        let b = bake_part(&lib, "3040").unwrap();
        assert_eq!(a.hi, b.hi);
        assert_eq!(a.def, b.def);
    }

    #[test]
    fn hull_of_a_wedge_keeps_only_corners() {
        // A unit cube with an extra midpoint on one edge: the midpoint is not a vertex.
        let mut s = BTreeSet::new();
        for x in [0i16, 2] {
            for y in [0i16, 2] {
                for z in [0i16, 2] {
                    s.insert([x, y, z]);
                }
            }
        }
        s.insert([1, 0, 0]);
        s.insert([1, 1, 1]);
        assert_eq!(hull_vertices(&s).len(), 8);
    }
}
