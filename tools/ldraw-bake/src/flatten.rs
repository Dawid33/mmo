//! Flatten an LDraw part's sub-file tree into world-space triangles and edge lines.
//!
//! `weldr` parses the files and resolves the references; this module walks the tree itself because
//! `weldr`'s own iterator drops the BFC meta commands and does not say which references are studs.

use anyhow::{Context, Result, bail};
use game::stud_assets::TriColor;
use std::path::PathBuf;
use weldr::{Command, FileRefResolver, ResolveError, SourceFile, SourceMap, SubFileRef};

/// Which stud / primitive set a bake uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// Plain `p/` (16-segment studs).
    Hi,
    /// Prefer `p/8/` (8-segment studs).
    Lo,
}

/// An LDraw library directory (`parts/`, `p/`, `LDConfig.ldr` below it).
#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Read a part file by its (alias-resolved) number, e.g. `3001` -> `parts/3001.dat`.
    pub fn part_text(&self, number: &str) -> Result<String> {
        let p = self.root.join("parts").join(format!("{number}.dat"));
        std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))
    }
}

struct Resolver<'a> {
    lib: &'a Library,
    detail: Detail,
}

impl FileRefResolver for Resolver<'_> {
    fn resolve(&self, filename: &str) -> Result<Vec<u8>, ResolveError> {
        let name = filename.replace('\\', "/").to_lowercase();
        let mut dirs: Vec<&str> = Vec::new();
        if self.detail == Detail::Lo && !name.contains('/') {
            dirs.push("p/8");
        }
        dirs.extend(["parts", "p", "models"]);
        for d in dirs {
            let p = self.lib.root.join(d).join(&name);
            if let Ok(bytes) = std::fs::read(&p) {
                return Ok(bytes);
            }
        }
        Err(ResolveError {
            filename: filename.to_string(),
            resolve_error: Some("not found in the LDraw library".into()),
        })
    }
}

/// A triangle in LDraw space (LDU, Y down), already wound CCW as seen from the front per BFC.
#[derive(Clone, Debug)]
pub struct FlatTri {
    pub v: [[f64; 3]; 3],
    pub color: TriColor,
    /// Translation (LDraw LDU) of the `stud.dat`-family reference it came through, if any.
    pub stud: Option<[f64; 3]>,
}

#[derive(Clone, Debug)]
pub struct FlatEdge {
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub stud: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct Flat {
    pub tris: Vec<FlatTri>,
    pub edges: Vec<FlatEdge>,
}

#[derive(Clone, Copy)]
struct Xf {
    m: [[f64; 3]; 3],
    t: [f64; 3],
}

impl Xf {
    const IDENTITY: Xf = Xf { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        let m = &self.m;
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + self.t[0],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + self.t[1],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + self.t[2],
        ]
    }

    fn then(&self, child: &Xf) -> Xf {
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c = (0..3).map(|k| self.m[i][k] * child.m[k][j]).sum();
            }
        }
        Xf { m, t: self.apply(child.t) }
    }

    fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
}

fn is_stud_family(filename: &str) -> bool {
    let norm = filename.replace('\\', "/").to_lowercase();
    let base = norm.rsplit('/').next().unwrap_or("");
    matches!(base, "stud.dat" | "stud2.dat" | "studa.dat")
}

fn resolve_color(code: u32, cur: TriColor) -> TriColor {
    match code {
        16 => cur,
        24 => TriColor::Edge,
        c => TriColor::Fixed(c as u16),
    }
}

fn v3(v: &weldr::Vec3) -> [f64; 3] {
    [v.x as f64, v.y as f64, v.z as f64]
}

struct Walk<'a> {
    map: &'a SourceMap,
    out: Flat,
}

impl Walk<'_> {
    fn file(&mut self, file: &SourceFile, xf: Xf, cur: TriColor, invert: bool, stud: Option<[f64; 3]>) -> Result<()> {
        let mut ccw = true;
        let mut invert_next = false;
        for cmd in &file.cmds {
            match cmd {
                Command::Comment(c) => {
                    let mut it = c.text.split_whitespace();
                    if it.next() == Some("BFC") {
                        for w in it {
                            match w {
                                "CW" => ccw = false,
                                "CCW" => ccw = true,
                                "INVERTNEXT" => invert_next = true,
                                _ => {}
                            }
                        }
                    }
                }
                Command::SubFileRef(r) => {
                    let SubFileRef::ResolvedRef(target) = &r.file else {
                        bail!("unresolved sub-file reference in {}", file.filename)
                    };
                    let target = target.get(self.map);
                    let local = Xf {
                        m: [v3(&r.row0), v3(&r.row1), v3(&r.row2)],
                        t: v3(&r.pos),
                    };
                    let child = xf.then(&local);
                    let inv = invert ^ invert_next ^ (local.det() < 0.0);
                    invert_next = false;
                    let st = stud.or_else(|| is_stud_family(&target.filename).then_some(child.t));
                    self.file(target, child, resolve_color(r.color, cur), inv, st)?;
                }
                Command::Line(l) => {
                    invert_next = false;
                    self.out.edges.push(FlatEdge {
                        a: xf.apply(v3(&l.vertices[0])),
                        b: xf.apply(v3(&l.vertices[1])),
                        stud,
                    });
                }
                Command::Triangle(t) => {
                    invert_next = false;
                    let c = resolve_color(t.color, cur);
                    self.tri(&xf, [v3(&t.vertices[0]), v3(&t.vertices[1]), v3(&t.vertices[2])], c, !ccw ^ invert, stud);
                }
                Command::Quad(q) => {
                    invert_next = false;
                    let c = resolve_color(q.color, cur);
                    let p: Vec<[f64; 3]> = q.vertices.iter().map(v3).collect();
                    self.tri(&xf, [p[0], p[1], p[2]], c, !ccw ^ invert, stud);
                    self.tri(&xf, [p[0], p[2], p[3]], c, !ccw ^ invert, stud);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn tri(&mut self, xf: &Xf, v: [[f64; 3]; 3], color: TriColor, flip: bool, stud: Option<[f64; 3]>) {
        let mut w = [xf.apply(v[0]), xf.apply(v[1]), xf.apply(v[2])];
        // A mirrored matrix reverses the winding in world space; the caller folded that into `flip`.
        if flip {
            w.swap(1, 2);
        }
        self.out.tris.push(FlatTri { v: w, color, stud });
    }
}

/// Flatten `part` (a part number like `3001`, or a file name) at the given detail.
pub fn flatten(lib: &Library, part: &str, detail: Detail) -> Result<Flat> {
    let name = if part.ends_with(".dat") { part.to_string() } else { format!("{part}.dat") };
    let resolver = Resolver { lib, detail };
    let mut map = SourceMap::new();
    let root = weldr::parse(&name, &resolver, &mut map).map_err(|e| anyhow::anyhow!("parsing {name}: {e:?}"))?;
    let mut w = Walk { map: &map, out: Flat::default() };
    w.file(root.get(&map), Xf::IDENTITY, TriColor::Own, false, None)?;
    Ok(w.out)
}

/// The file's alias target when its first line is `0 ~Moved to <number>`.
pub fn moved_to(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    let rest = first.strip_prefix('0')?.trim().strip_prefix("~Moved to")?;
    let n = rest.trim().trim_end_matches(".dat").to_lowercase();
    (!n.is_empty()).then_some(n)
}
