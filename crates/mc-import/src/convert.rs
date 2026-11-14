//! A classified block volume becomes a [`PrefabFile`]: trimmed to the build, air kept (outside) or carved
//! (enclosed), whole blocks merged into boxes, shaped cells and markers emitted, a [`Report`] of the losses.
//!
//! Per-cell work is array lookups: each palette entry is classified once into small tables, flood fill
//! and merge use bitsets and an explicit queue, nothing is allocated or looked up in a map per cell.

use crate::classify::{classify, Class};
use crate::report::Report;
use crate::{BlockEntity, BlockVolume, Table, Tag, MAPPING_VERSION};
use game::prefab::{
    FileBox, FileCarve, FileShape, FileShaped, GroundFit, ImportInfo, Marker, PrefabFile, PrefabMeta, Shape,
    PREFAB_FORMAT,
};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirMode {
    /// Air reachable from the box faces is kept, enclosed air is carved.
    Auto,
    Carve,
    Keep,
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub name: String,
    pub air: AirMode,
    /// A sub-box of the volume (min inclusive, max exclusive); `None` = all.
    pub region: Option<([u32; 3], [u32; 3])>,
    pub author: String,
    pub source: String,
    pub licence: String,
    pub source_hash: String,
    pub tags: Vec<String>,
}

#[derive(Clone, Copy)]
enum Kind {
    Air,
    Keep,
    Drop(&'static str),
    /// Index into the local material names.
    Block(u32),
    /// Index into the shaped list.
    Shaped(u32),
    /// Index into the marker list.
    Marker(u32),
}

struct MarkerInfo {
    kind: String,
    turn: u8,
    props: BTreeMap<String, String>,
    solid: Option<u32>,
}

const TABLE_LEN: usize = 1 << 16;
const NO_MAT: u32 = u32::MAX;
/// The merge code of a carved cell (block codes are `final palette index + 1`).
const CARVE_CODE: u32 = u32::MAX;

fn intern(names: &mut Vec<String>, name: &str) -> u32 {
    match names.iter().position(|n| n == name) {
        Some(i) => i as u32,
        None => {
            names.push(name.to_string());
            (names.len() - 1) as u32
        }
    }
}

struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }
    #[inline]
    fn get(&self, i: usize) -> bool {
        self.0[i >> 6] >> (i & 63) & 1 != 0
    }
    #[inline]
    fn set(&mut self, i: usize) {
        self.0[i >> 6] |= 1 << (i & 63);
    }
}

/// The trimmed box over the volume's cell array, with the per-palette tables.
struct Grid<'a> {
    blocks: &'a [u16],
    /// Volume index of the trimmed box's min corner, and the volume's strides.
    base: usize,
    stride_z: usize,
    stride_y: usize,
    t: [usize; 3],
    /// Passes the flood fill (air, keep, dropped, a marker without a solid).
    passable: &'a [bool],
    /// May become carved (passable and not Keep).
    carvable: &'a [bool],
    /// Merge code of a solid cell, 0 for others.
    code: &'a [u32],
    /// Carvable cells that stay (reached by the flood fill); `None` = carve every carvable cell.
    reached: Option<&'a Bits>,
    carve: bool,
}

impl Grid<'_> {
    #[inline]
    fn pid(&self, x: usize, y: usize, z: usize) -> usize {
        self.blocks[self.base + x + z * self.stride_z + y * self.stride_y] as usize
    }
    #[inline]
    fn tidx(&self, x: usize, y: usize, z: usize) -> usize {
        x + z * self.t[0] + y * self.t[0] * self.t[2]
    }
    #[inline]
    fn merge_code(&self, x: usize, y: usize, z: usize) -> u32 {
        let p = self.pid(x, y, z);
        let c = self.code[p];
        if c != 0 {
            return c;
        }
        if self.carve && self.carvable[p] && !self.reached.is_some_and(|r| r.get(self.tidx(x, y, z))) {
            return CARVE_CODE;
        }
        0
    }
}

/// `None` when the region holds nothing solid.
pub fn convert(volume: &BlockVolume, table: &Table, opts: &ConvertOptions) -> Option<(PrefabFile, Report)> {
    let size = volume.size.map(|v| v as usize);
    let total = size[0].checked_mul(size[1])?.checked_mul(size[2])?;
    if volume.blocks.len() < total {
        return None;
    }
    let (rmin, rmax) = match opts.region {
        Some((a, b)) => (a.map(|v| v as usize), [0, 1, 2].map(|i| (b[i] as usize).min(size[i]))),
        None => ([0; 3], size),
    };
    if (0..3).any(|i| rmin[i] >= rmax[i]) {
        return None;
    }

    // Classify each palette entry once.
    let mut names: Vec<String> = Vec::new();
    let mut shaped_list: Vec<(Shape, u32)> = Vec::new();
    let mut markers_list: Vec<MarkerInfo> = Vec::new();
    let mut kinds = vec![Kind::Air; TABLE_LEN];
    let mut mapped = vec![true; TABLE_LEN];
    for (i, state) in volume.palette.iter().enumerate().take(TABLE_LEN) {
        let c = classify(state, table);
        mapped[i] = c.mapped;
        kinds[i] = match c.class {
            Class::Air => Kind::Air,
            Class::Keep => Kind::Keep,
            Class::Dropped { why } => Kind::Drop(why),
            Class::Block { block } => Kind::Block(intern(&mut names, &block)),
            Class::Shaped { shape, block } => {
                shaped_list.push((shape, intern(&mut names, &block)));
                Kind::Shaped((shaped_list.len() - 1) as u32)
            }
            Class::Marker { kind, turn, solid } => {
                let mut props = state.props.clone();
                props.insert("mc".into(), state.name.clone());
                markers_list.push(MarkerInfo { kind, turn, props, solid: solid.map(|s| intern(&mut names, &s)) });
                Kind::Marker((markers_list.len() - 1) as u32)
            }
        };
    }
    let solid_of = |k: Kind| -> bool {
        matches!(k, Kind::Block(_) | Kind::Shaped(_) | Kind::Marker(_))
    };
    let mat_of = |k: Kind| -> u32 {
        match k {
            Kind::Block(m) => m,
            Kind::Shaped(s) => shaped_list[s as usize].1,
            Kind::Marker(m) => markers_list[m as usize].solid.unwrap_or(NO_MAT),
            _ => NO_MAT,
        }
    };
    let is_solid: Vec<bool> = kinds.iter().map(|&k| solid_of(k)).collect();

    // Pass 1: bounding box of the build, cells per palette entry, palette entries in order of first use.
    let mut counts = vec![0u64; TABLE_LEN];
    let mut seen = vec![false; TABLE_LEN];
    let mut order: Vec<usize> = Vec::new();
    let (mut lo, mut hi) = ([usize::MAX; 3], [0usize; 3]);
    let (stride_z, stride_y) = (size[0], size[0] * size[2]);
    for y in rmin[1]..rmax[1] {
        for z in rmin[2]..rmax[2] {
            let row = y * stride_y + z * stride_z;
            let cells = &volume.blocks[row + rmin[0]..row + rmax[0]];
            let mut any = false;
            let (mut first, mut last) = (0usize, 0usize);
            for (dx, &p) in cells.iter().enumerate() {
                let p = p as usize;
                counts[p] += 1;
                if is_solid[p] {
                    if !any {
                        first = dx;
                        any = true;
                    }
                    last = dx;
                    if !seen[p] {
                        seen[p] = true;
                        order.push(p);
                    }
                }
            }
            if any {
                lo[0] = lo[0].min(rmin[0] + first);
                hi[0] = hi[0].max(rmin[0] + last + 1);
                lo[1] = lo[1].min(y);
                hi[1] = hi[1].max(y + 1);
                lo[2] = lo[2].min(z);
                hi[2] = hi[2].max(z + 1);
            }
        }
    }
    if lo[0] == usize::MAX {
        return None;
    }
    let t = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let tcells = t[0] * t[1] * t[2];

    // Our palette, in order of first use; the merge code per palette entry.
    let mut palette: Vec<String> = Vec::new();
    let mut final_of = vec![NO_MAT; names.len()];
    for &p in &order {
        let m = mat_of(kinds[p]);
        if m != NO_MAT && final_of[m as usize] == NO_MAT {
            final_of[m as usize] = palette.len() as u32;
            palette.push(names[m as usize].clone());
        }
    }
    let mut code = vec![0u32; TABLE_LEN];
    let mut passable = vec![false; TABLE_LEN];
    let mut carvable = vec![false; TABLE_LEN];
    // A material nothing in the region uses has no final index; such an entry never occurs in a cell.
    let code_of = |m: u32| final_of[m as usize].wrapping_add(1);
    for p in 0..TABLE_LEN {
        match kinds[p] {
            Kind::Block(m) => code[p] = code_of(m),
            Kind::Marker(i) => match markers_list[i as usize].solid {
                Some(m) => code[p] = code_of(m),
                None => (passable[p], carvable[p]) = (true, true),
            },
            Kind::Air | Kind::Drop(_) => (passable[p], carvable[p]) = (true, true),
            Kind::Keep => passable[p] = true,
            Kind::Shaped(_) => {}
        }
    }

    let base = lo[0] + lo[2] * stride_z + lo[1] * stride_y;
    let mut grid = Grid {
        blocks: &volume.blocks,
        base,
        stride_z,
        stride_y,
        t,
        passable: &passable,
        carvable: &carvable,
        code: &code,
        reached: None,
        carve: opts.air != AirMode::Keep,
    };

    // Flood fill from the faces of the trimmed box.
    let reached;
    if opts.air == AirMode::Auto {
        let mut bits = Bits::new(tcells);
        let mut queue: VecDeque<u32> = VecDeque::new();
        let push = |bits: &mut Bits, q: &mut VecDeque<u32>, g: &Grid, x: usize, y: usize, z: usize| {
            let i = g.tidx(x, y, z);
            if !bits.get(i) && g.passable[g.pid(x, y, z)] {
                bits.set(i);
                q.push_back(i as u32);
            }
        };
        for y in 0..t[1] {
            for z in 0..t[2] {
                if y == 0 || y == t[1] - 1 || z == 0 || z == t[2] - 1 {
                    for x in 0..t[0] {
                        push(&mut bits, &mut queue, &grid, x, y, z);
                    }
                } else {
                    push(&mut bits, &mut queue, &grid, 0, y, z);
                    push(&mut bits, &mut queue, &grid, t[0] - 1, y, z);
                }
            }
        }
        while let Some(i) = queue.pop_front() {
            let i = i as usize;
            let (x, r) = (i % t[0], i / t[0]);
            let (z, y) = (r % t[2], r / t[2]);
            if x > 0 {
                push(&mut bits, &mut queue, &grid, x - 1, y, z);
            }
            if x + 1 < t[0] {
                push(&mut bits, &mut queue, &grid, x + 1, y, z);
            }
            if z > 0 {
                push(&mut bits, &mut queue, &grid, x, y, z - 1);
            }
            if z + 1 < t[2] {
                push(&mut bits, &mut queue, &grid, x, y, z + 1);
            }
            if y > 0 {
                push(&mut bits, &mut queue, &grid, x, y - 1, z);
            }
            if y + 1 < t[1] {
                push(&mut bits, &mut queue, &grid, x, y + 1, z);
            }
        }
        reached = bits;
        grid.reached = Some(&reached);
    }

    // Block entities by cell (volume coordinates): tens of thousands at most.
    let entities: BTreeMap<[i32; 3], &BlockEntity> = volume.block_entities.iter().map(|e| (e.pos, e)).collect();

    // Pass 2: merge boxes and carve boxes, emit shaped cells and markers, all in scan order.
    let mut claimed = Bits::new(tcells);
    let mut boxes = Vec::new();
    let mut carve = Vec::new();
    let mut shaped = Vec::new();
    let mut markers = Vec::new();
    let mut report = Report { cells: tcells as u64, ..Report::default() };
    for y in 0..t[1] {
        for z in 0..t[2] {
            for x in 0..t[0] {
                let p = grid.pid(x, y, z);
                let at = [x as i32, y as i32, z as i32];
                match kinds[p] {
                    Kind::Shaped(s) => {
                        let (shape, m) = &shaped_list[s as usize];
                        shaped.push(FileShaped {
                            at,
                            shape: FileShape::Named { shape: *shape, mat: final_of[*m as usize] as u16 },
                        });
                    }
                    Kind::Marker(m) => {
                        let info = &markers_list[m as usize];
                        let mut props = info.props.clone();
                        if info.kind == "sign" {
                            let pos = [(lo[0] + x) as i32, (lo[1] + y) as i32, (lo[2] + z) as i32];
                            if let Some(text) = entities.get(&pos).and_then(|e| sign_text(&e.nbt)) {
                                props.insert("text".into(), text);
                            }
                        }
                        *report.markers.entry(info.kind.clone()).or_insert(0) += 1;
                        markers.push(Marker { at, kind: info.kind.clone(), turn: info.turn, props });
                    }
                    _ => {}
                }
                let c = grid.merge_code(x, y, z);
                if c == 0 || claimed.get(grid.tidx(x, y, z)) {
                    continue;
                }
                let free = |x: usize, y: usize, z: usize| !claimed.get(grid.tidx(x, y, z)) && grid.merge_code(x, y, z) == c;
                let mut w = 1;
                while x + w < t[0] && free(x + w, y, z) {
                    w += 1;
                }
                let mut d = 1;
                while z + d < t[2] && (x..x + w).all(|xx| free(xx, y, z + d)) {
                    d += 1;
                }
                let mut h = 1;
                while y + h < t[1] && (z..z + d).all(|zz| (x..x + w).all(|xx| free(xx, y + h, zz))) {
                    h += 1;
                }
                for yy in y..y + h {
                    for zz in z..z + d {
                        for xx in x..x + w {
                            claimed.set(grid.tidx(xx, yy, zz));
                        }
                    }
                }
                let sz = [w as u32, h as u32, d as u32];
                let vol = (w * h * d) as u64;
                if c == CARVE_CODE {
                    carve.push(FileCarve { min: at, size: sz });
                    report.carved += vol;
                } else {
                    boxes.push(FileBox { min: at, size: sz, mat: (c - 1) as u16 });
                    report.blocks += vol;
                }
            }
        }
    }
    report.shaped = shaped.len() as u64;
    report.boxes = boxes.len();
    report.kept_air = tcells as u64 - report.blocks - report.shaped - report.carved;
    for (p, &n) in counts.iter().enumerate() {
        if n == 0 {
            continue;
        }
        match kinds[p] {
            Kind::Drop(why) => *report.dropped.entry(why.to_string()).or_insert(0) += n,
            Kind::Block(_) | Kind::Shaped(_) if !mapped[p] => {
                *report.unmapped.entry(volume.palette[p].name.clone()).or_insert(0) += n;
            }
            _ => {}
        }
    }

    let file = PrefabFile {
        format: PREFAB_FORMAT,
        name: opts.name.clone(),
        meta: PrefabMeta {
            author: opts.author.clone(),
            source: opts.source.clone(),
            licence: opts.licence.clone(),
            note: String::new(),
            import: Some(ImportInfo {
                source_hash: opts.source_hash.clone(),
                source_box: Some((lo.map(|v| v as i32), hi.map(|v| v as i32))),
                data_version: volume.data_version,
                mapping_version: MAPPING_VERSION,
            }),
        },
        size: t.map(|v| v as u32),
        anchor: [(t[0] / 2) as i32, 0, (t[2] / 2) as i32],
        palette,
        boxes,
        shaped,
        carve,
        markers,
        ground: GroundFit::Flat,
        tags: opts.tags.clone(),
    };
    Some((file, report))
}

/// A sign's plain text: `front_text.messages` (1.20+) or `Text1..Text4`; non-empty lines joined by a space.
fn sign_text(nbt: &Tag) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(msgs) = nbt.get("front_text").and_then(|f| f.get("messages")).and_then(Tag::as_list) {
        lines.extend(msgs.iter().take(4).filter_map(Tag::as_str).filter_map(json_text));
    } else {
        for k in ["Text1", "Text2", "Text3", "Text4"] {
            lines.extend(nbt.get(k).and_then(Tag::as_str).and_then(json_text));
        }
    }
    lines.retain(|l| !l.is_empty());
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// The plain text of a JSON text component: a bare string, or an object's top-level `"text"` string.
/// Anything else (garbage included) yields `None`.
pub fn json_text(src: &str) -> Option<String> {
    let mut p = Json { b: src.as_bytes(), i: 0 };
    p.ws();
    let out = match p.peek()? {
        b'"' => p.string()?,
        b'{' => p.object_text()?,
        _ => return None,
    };
    p.ws();
    (p.i == p.b.len()).then_some(out)
}

struct Json<'a> {
    b: &'a [u8],
    i: usize,
}

const JSON_MAX_DEPTH: usize = 16;

impl Json<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek() == Some(c)).then(|| self.i += 1)
    }
    fn hex4(&mut self) -> Option<u32> {
        let s = self.b.get(self.i..self.i + 4)?;
        let v = u32::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok()?;
        self.i += 4;
        Some(v)
    }
    /// A JSON string starting at the opening quote.
    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) && self.b.get(self.i..self.i + 2) == Some(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + lo.wrapping_sub(0xDC00))
                                    .unwrap_or('\u{fffd}')
                            } else {
                                char::from_u32(hi).unwrap_or('\u{fffd}')
                            }
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1f => return None,
                _ => out.push(c),
            }
        }
    }
    /// An object; the value of its top-level `"text"` when that is a string, else an empty string.
    fn object_text(&mut self) -> Option<String> {
        self.eat(b'{')?;
        let mut text = String::new();
        self.ws();
        if self.eat(b'}').is_some() {
            return Some(text);
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            if key == "text" && self.peek() == Some(b'"') {
                text = self.string()?;
            } else {
                self.skip(0)?;
            }
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(text);
                }
                _ => return None,
            }
        }
    }
    fn skip(&mut self, depth: usize) -> Option<()> {
        if depth > JSON_MAX_DEPTH {
            return None;
        }
        self.ws();
        match self.peek()? {
            b'"' => self.string().map(|_| ()),
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                self.i += 1;
                self.ws();
                if self.eat(close).is_some() {
                    return Some(());
                }
                loop {
                    self.ws();
                    if open == b'{' {
                        self.string()?;
                        self.ws();
                        self.eat(b':')?;
                    }
                    self.skip(depth + 1)?;
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        c if c == close => {
                            self.i += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            _ => {
                // number, true, false, null
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')) {
                    self.i += 1;
                }
                (self.i > start).then_some(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::json_text;

    #[test]
    fn json_forms() {
        assert_eq!(json_text(r#"{"text":"Zeus"}"#).as_deref(), Some("Zeus"));
        assert_eq!(json_text(r#""Zeus""#).as_deref(), Some("Zeus"));
        assert_eq!(json_text(r#"{"text":""}"#).as_deref(), Some(""));
        assert_eq!(json_text(r#""""#).as_deref(), Some(""));
        assert_eq!(json_text(r#"{"text":"a \"b\""}"#).as_deref(), Some("a \"b\""));
        assert_eq!(json_text(r#"{"extra":[{"text":"x"}],"bold":true,"text":"Hié"}"#).as_deref(), Some("Hié"));
        for bad in ["", "{", "}{", "\"abc", "{\"text\":", "[1]", "nul", "{\"a\":[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]}", "\u{0}"] {
            assert_eq!(json_text(bad), None, "{bad:?}");
        }
    }
}
