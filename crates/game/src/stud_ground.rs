//! The stud ground recipe: what the terrain of one chunk column (32 x 32 stud columns)
//! is before anyone edits it, and the pure function that turns it into bricks.
//!
//! [`StudColumnGround::pristine_chunk`] depends only on the recipe of its own chunk
//! column and the layer, so the server, a predicting client and a neighbour's halo all
//! derive bit-identical chunks, and an unedited chunk never has to be stored or sent.
//! Bricks never cross a chunk edge and courses sit on the world plate grid (a chunk is
//! 30 courses tall), so bond patterns line up across chunk, layer and region edges.
//!
//! Heights are **plates** in the region's frame: plate 0 is the bottom of layer 0.

use serde::{Deserialize, Serialize};

use crate::stud::{StudBrick, StudChunk, StudShape, PLATES_PER_BRICK, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS};
use crate::stud_carve::box_part;
use crate::stud_material::{stud_material, MAT_BEDROCK};

/// Levels below a column's surface that the fortress view's level range and its
/// picks reach down to.
pub const UNDERGROUND_DEPTH: i32 = 60;

const N: usize = STUD_CHUNK_STUDS;
/// Stud columns of one chunk column.
pub const STUD_COLUMN_FOOTPRINT: usize = N * N;
/// `water_top` of a dry column.
pub const STUD_NO_WATER: i32 = i32::MIN;
/// Brick lengths tried along a strip, longest first.
const LENGTHS: [u8; 6] = [8, 6, 4, 3, 2, 1];
/// Longest first piece of a staggered strip (half of the longest brick): running bond.
const STAGGER_FIRST: u8 = 4;
/// Hidden ground is filled in tiles of this many studs a side.
const FILL_TILE: usize = 4;
/// Plates of real bricks kept over the fill: what shows after the surface, one course.
const SHELL_PLATES: i32 = PLATES_PER_BRICK as i32;
/// How much lower the ground just outside the chunk is assumed to be (it is not read:
/// a chunk depends on its own columns only), so edge columns keep real bricks deeper.
const EDGE_DROP_PLATES: i32 = 2 * PLATES_PER_BRICK as i32;
const _: () = assert!(N % FILL_TILE == 0);
const COURSES: i32 = (STUD_CHUNK_PLATES / PLATES_PER_BRICK as usize) as i32;
const _: () = assert!(STUD_CHUNK_PLATES % PLATES_PER_BRICK as usize == 0);

/// The soil stack of a column, top down: `cover_plates` of `cover`, then `soil_plates`
/// of `soil`; under it the recipe's rock strata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct StudProfile {
    pub cover: u16,
    pub cover_plates: u8,
    pub soil: u16,
    pub soil_plates: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudGroundError {
    WrongFootprint,
    UnknownProfile,
    UnknownMaterial,
    /// No formation, or tops not strictly descending.
    BadStrata,
}

/// The recipe of one chunk column. Index columns `z * 32 + x`.
///
/// `Default` is an empty placeholder for the undo-map plumbing only (never a stored
/// value): build real recipes with [`Self::new`] / [`Self::flat`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, state_hash::StateHash)]
pub struct StudColumnGround {
    /// One past the top solid plate of each stud column.
    surface: Box<[i32]>,
    /// One past the top water plate, or [`STUD_NO_WATER`].
    water_top: Box<[i32]>,
    /// Index into `profiles`.
    profile: Box<[u8]>,
    profiles: Vec<StudProfile>,
    /// Stratigraphic offset of each stud column, plates: rock at plate `p` is read from
    /// `strata` at `s = p - geo`.
    geo: Box<[i32]>,
    /// Rock formations top-down as `(top, material)`: a formation holds every `s` below
    /// its `top` down to the next one's; the last extends downward, the first upward.
    strata: Vec<(i32, u16)>,
    /// Plates below this are bedrock.
    bedrock: i32,
}

/// A side of a chunk column: −x, +x, −z, +z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudSide {
    West,
    East,
    North,
    South,
}

impl StudSide {
    pub const ALL: [StudSide; 4] = [StudSide::West, StudSide::East, StudSide::North, StudSide::South];

    /// The chunk column offset `(dx, dz)` to the neighbour on this side.
    pub fn offset(self) -> (i32, i32) {
        match self {
            StudSide::West => (-1, 0),
            StudSide::East => (1, 0),
            StudSide::North => (0, -1),
            StudSide::South => (0, 1),
        }
    }

    pub fn opposite(self) -> StudSide {
        match self {
            StudSide::West => StudSide::East,
            StudSide::East => StudSide::West,
            StudSide::North => StudSide::South,
            StudSide::South => StudSide::North,
        }
    }
}

/// The cliff-wall layers of chunk column `c` against the columns of `recipes` beside
/// it ([`StudColumnGround::wall_layers`] over its four sides): what a fresh region
/// materialises under `c`'s surface layers. Columns `recipes` does not hold (past the
/// region's edge, or outside a window) open nothing.
pub fn stud_wall_layers(
    recipes: &std::collections::BTreeMap<crate::ColumnCoords, &StudColumnGround>,
    c: crate::ColumnCoords,
) -> std::collections::BTreeSet<i32> {
    let mut out = std::collections::BTreeSet::new();
    let Some(g) = recipes.get(&c) else { return out };
    for side in StudSide::ALL {
        let (dx, dz) = side.offset();
        let (Some(x), Some(z)) = (c.x.checked_add_signed(dx as isize), c.z.checked_add_signed(dz as isize)) else { continue };
        if let Some(n) = recipes.get(&crate::ColumnCoords::new(x, z)) {
            out.extend(g.wall_layers(n.edge_low(side.opposite())));
        }
    }
    out
}

/// What a fresh stud region starts from (`Region::seed_stud_world`): a ground recipe per
/// chunk column, plus feature bricks (trees, structures) that are not a function of the
/// recipe, by the chunk they go into.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StudRegionSeed {
    pub ground: Vec<(crate::ColumnCoords, StudColumnGround)>,
    pub features: Vec<(crate::StudChunkCoords, Vec<StudBrick>)>,
}

impl StudRegionSeed {
    /// The chunks holding feature bricks: each one's pristine chunk (empty where the
    /// recipe has none) with its features placed in; an overlapping or invalid feature
    /// brick is dropped.
    pub fn feature_chunks(&self) -> std::collections::BTreeMap<crate::StudChunkCoords, StudChunk> {
        let mut out: std::collections::BTreeMap<crate::StudChunkCoords, StudChunk> = Default::default();
        for (c, bricks) in &self.features {
            let chunk = out.entry(*c).or_insert_with(|| {
                self.ground
                    .iter()
                    .find(|(col, _)| (col.x, col.z) == (c.x, c.z))
                    .and_then(|(_, g)| g.pristine_chunk(c.y))
                    .unwrap_or_default()
            });
            for b in bricks {
                let _ = chunk.place(*b);
            }
        }
        out
    }
}

/// The material the recipe rule ([`StudColumnGround::material_at`]) puts at the top
/// solid plate of a column with these values — for a reader that samples single
/// columns without building a recipe (far terrain, `worldgen::stud_lod_tile`).
pub fn stud_top_material(surface: i32, profile: &StudProfile, geo: i32, strata: &[(i32, u16)], bedrock: i32) -> u16 {
    let plate = surface.saturating_sub(1);
    if plate < bedrock {
        MAT_BEDROCK
    } else if profile.cover_plates > 0 {
        profile.cover
    } else if profile.soil_plates > 0 {
        profile.soil
    } else {
        let s = plate as i64 - geo as i64;
        let i = strata.partition_point(|&(top, _)| top as i64 > s).saturating_sub(1);
        strata.get(i).map_or(crate::stud_material::MAT_STONE, |&(_, m)| m)
    }
}

/// The layer holding region plate `plate`.
pub fn stud_layer_of(plate: i32) -> i32 {
    plate.div_euclid(STUD_CHUNK_PLATES as i32)
}

impl StudColumnGround {
    pub fn new(
        surface: Box<[i32]>,
        water_top: Box<[i32]>,
        profile: Box<[u8]>,
        profiles: Vec<StudProfile>,
        geo: Box<[i32]>,
        strata: Vec<(i32, u16)>,
        bedrock: i32,
    ) -> Result<Self, StudGroundError> {
        if [surface.len(), water_top.len(), profile.len(), geo.len()] != [STUD_COLUMN_FOOTPRINT; 4] {
            return Err(StudGroundError::WrongFootprint);
        }
        if strata.is_empty() || strata.windows(2).any(|w| w[0].0 <= w[1].0) {
            return Err(StudGroundError::BadStrata);
        }
        if profile.iter().any(|&p| p as usize >= profiles.len()) {
            return Err(StudGroundError::UnknownProfile);
        }
        let known = |m: u16| stud_material(m).is_some();
        if profiles.iter().any(|p| !known(p.cover) || !known(p.soil)) || strata.iter().any(|&(_, m)| !known(m)) {
            return Err(StudGroundError::UnknownMaterial);
        }
        Ok(Self { surface, water_top, profile, profiles, geo, strata, bedrock })
    }

    /// A flat column: every stud column at `surface` with one profile over one `rock`, dry.
    pub fn flat(surface: i32, profile: StudProfile, rock: u16, bedrock: i32) -> Self {
        Self::new(
            vec![surface; STUD_COLUMN_FOOTPRINT].into(),
            vec![STUD_NO_WATER; STUD_COLUMN_FOOTPRINT].into(),
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![profile],
            vec![0; STUD_COLUMN_FOOTPRINT].into(),
            vec![(i32::MAX, rock)],
            bedrock,
        )
        .expect("a flat recipe of known materials")
    }

    pub fn surface(&self, x: usize, z: usize) -> i32 {
        self.surface[z * N + x]
    }

    pub fn water_top(&self, x: usize, z: usize) -> Option<i32> {
        let w = self.water_top[z * N + x];
        (w != STUD_NO_WATER).then_some(w)
    }

    /// What the recipe puts at `plate` of stud column `(x, z)`; `None` is air.
    pub fn material_at(&self, x: usize, plate: i32, z: usize) -> Option<u16> {
        self.material(z * N + x, plate)
    }

    fn material(&self, i: usize, plate: i32) -> Option<u16> {
        let s = self.surface[i];
        if plate >= s {
            return None;
        }
        if plate < self.bedrock {
            return Some(MAT_BEDROCK);
        }
        let p = &self.profiles[self.profile[i] as usize];
        let depth = s - 1 - plate;
        Some(if depth < p.cover_plates as i32 {
            p.cover
        } else if depth < p.cover_plates as i32 + p.soil_plates as i32 {
            p.soil
        } else {
            self.rock(i, plate)
        })
    }

    /// Index of the formation holding stratigraphic height `s`.
    fn formation(&self, s: i64) -> usize {
        self.strata.partition_point(|&(top, _)| top as i64 > s).saturating_sub(1)
    }

    /// The rock of column `i` at `plate`.
    fn rock(&self, i: usize, plate: i32) -> u16 {
        self.strata[self.formation(plate as i64 - self.geo[i] as i64)].1
    }

    /// The layers holding a top solid plate of some column: the chunks a fresh region
    /// materialises. Everything under them stays implied until dug into.
    pub fn surface_layers(&self) -> std::ops::RangeInclusive<i32> {
        let lo = self.surface.iter().copied().min().unwrap_or(0);
        let hi = self.surface.iter().copied().max().unwrap_or(0);
        stud_layer_of(lo.saturating_sub(1))..=stud_layer_of(hi.saturating_sub(1))
    }

    /// The lowest `surface` along this chunk column's edge on `side`: where a
    /// neighbour across that face first meets this column's open cells.
    pub fn edge_low(&self, side: StudSide) -> i32 {
        let last = N - 1;
        (0..N)
            .map(|a| match side {
                StudSide::West => self.surface(0, a),
                StudSide::East => self.surface(last, a),
                StudSide::North => self.surface(a, 0),
                StudSide::South => self.surface(a, last),
            })
            .min()
            .unwrap_or(0)
    }

    /// The layers under this column's [`Self::surface_layers`] whose face a neighbour
    /// opens onto: the neighbour's facing edge lies as low as `facing_low` (its
    /// [`Self::edge_low`]), so from that plate's layer up this column's ground is a
    /// cliff wall in the open. A fresh region materialises them — implied ground has
    /// no bricks and no collider, and a wall of it would be a hole.
    pub fn wall_layers(&self, facing_low: i32) -> std::ops::Range<i32> {
        let top = *self.surface_layers().start();
        stud_layer_of(facing_low).max(-crate::stud::STUD_LAYER_LIMIT).min(top)..top
    }

    /// The unedited chunk of layer `y`, or `None` when the layer is all air here.
    pub fn pristine_chunk(&self, y: i32) -> Option<StudChunk> {
        let h_max = STUD_CHUNK_PLATES as i32;
        let base = y.checked_mul(h_max)?;
        let local = |plate: i32| plate.saturating_sub(base).clamp(0, h_max);
        let h: Vec<i32> = self.surface.iter().map(|&s| local(s)).collect();
        if h.iter().all(|&v| v == 0) {
            return None;
        }
        let mut out = Vec::new();

        // Everything under the deepest soil is rock. Where one formation holds all of it,
        // or the formations lie level across the chunk, it is a few fillers; where tilted
        // formations cross it, it is laid in courses like the soil.
        let rock_top = (0..STUD_COLUMN_FOOTPRINT)
            .map(|i| {
                let p = &self.profiles[self.profile[i] as usize];
                local(self.surface[i].saturating_sub(p.cover_plates as i32 + p.soil_plates as i32))
            })
            .min()
            .unwrap_or(0);
        let mut solid_to = rock_top - rock_top % PLATES_PER_BRICK as i32;
        let (geo_lo, geo_hi) = (self.geo.iter().copied().min().unwrap_or(0), self.geo.iter().copied().max().unwrap_or(0));
        let bed = local(self.bedrock).min(solid_to);
        let one_formation = solid_to > bed
            && self.formation(base as i64 + bed as i64 - geo_hi as i64)
                == self.formation(base as i64 + solid_to as i64 - 1 - geo_lo as i64);
        if solid_to > bed && !one_formation && geo_lo != geo_hi {
            solid_to = bed - bed % PLATES_PER_BRICK as i32;
        }
        let bed = bed.min(solid_to);
        let mut slab = |lo: i32, hi: i32, material: u16| {
            if hi > lo {
                out.push(StudBrick::filler([0, lo as u8, 0], [N as u8, (hi - lo) as u8, N as u8], material));
            }
        };
        slab(0, bed, MAT_BEDROCK);
        // Level formations: one slab per run of plates of the same rock.
        let mut lo = bed;
        while lo < solid_to {
            let rock = self.rock(0, base + lo);
            let mut hi = lo + 1;
            while hi < solid_to && self.rock(0, base + hi) == rock {
                hi += 1;
            }
            slab(lo, hi, rock);
            lo = hi;
        }

        // Hidden ground above the slabs: no face of it shows (every neighbouring column
        // stands at least a shell higher), so each tile of columns gets plain fillers,
        // one per run of courses of a single material. Courses where the tile's columns
        // differ (a soil boundary on a slope) fall through to real bricks.
        let hidden_to = |i: usize| {
            let (x, z) = ((i % N) as i32, (i / N) as i32);
            let mut low = h[i];
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let (nx, nz) = (x + dx, z + dz);
                let inside = (0..N as i32).contains(&nx) && (0..N as i32).contains(&nz);
                low = low.min(if inside { h[nz as usize * N + nx as usize] } else { h[i] - EDGE_DROP_PLATES });
            }
            low - SHELL_PLATES
        };
        let tiles = N / FILL_TILE;
        let first_course = solid_to / PLATES_PER_BRICK as i32;
        // Whether the fill took course `c` of tile `t`.
        let mut filled = vec![false; tiles * tiles * COURSES as usize];
        for tz in 0..tiles {
            for tx in 0..tiles {
                let cols: Vec<usize> = (0..FILL_TILE)
                    .flat_map(|dz| (0..FILL_TILE).map(move |dx| (tz * FILL_TILE + dz) * N + tx * FILL_TILE + dx))
                    .collect();
                let top = cols.iter().map(|&i| hidden_to(i)).min().unwrap_or(0).div_euclid(PLATES_PER_BRICK as i32);
                // The material every cell of course `c` of this tile shares, if they do.
                let uniform = |c: i32| {
                    let lo = base + c * PLATES_PER_BRICK as i32;
                    let m = self.material(cols[0], lo);
                    let same = cols.iter().all(|&i| (0..PLATES_PER_BRICK as i32).all(|dy| self.material(i, lo + dy) == m));
                    if same { m } else { None }
                };
                let mut c = first_course;
                while c < top {
                    let Some(m) = uniform(c) else {
                        c += 1;
                        continue;
                    };
                    let mut end = c + 1;
                    while end < top && uniform(end) == Some(m) {
                        end += 1;
                    }
                    for k in c..end {
                        filled[(tz * tiles + tx) * COURSES as usize + k as usize] = true;
                    }
                    let (lo, height) = (c * PLATES_PER_BRICK as i32, (end - c) * PLATES_PER_BRICK as i32);
                    out.push(StudBrick::filler(
                        [(tx * FILL_TILE) as u8, lo as u8, (tz * FILL_TILE) as u8],
                        [FILL_TILE as u8, height as u8, FILL_TILE as u8],
                        m,
                    ));
                    c = end;
                }
            }
        }
        let is_filled = |col: usize, course: i32| {
            let (tx, tz) = ((col % N) / FILL_TILE, (col / N) / FILL_TILE);
            filled[(tz * tiles + tx) * COURSES as usize + course as usize]
        };

        let mut mask = vec![None; STUD_COLUMN_FOOTPRINT];
        for course in first_course..COURSES {
            let lo = course * PLATES_PER_BRICK as i32;
            // World course parity: the bond direction alternates up through every layer.
            let along_x = (y as i64 * COURSES as i64 + course as i64).rem_euclid(2) == 0;
            // Whole bricks, then the one or two plates topping columns that end inside
            // this course.
            let layers = [(0, PLATES_PER_BRICK as i32, PLATES_PER_BRICK as i32), (0, 1, 1), (1, 1, 2)];
            for (i, (dy, height, need)) in layers.into_iter().enumerate() {
                let mut any = false;
                for (c, m) in mask.iter_mut().enumerate() {
                    let fill = h[c] - lo;
                    let whole = fill >= PLATES_PER_BRICK as i32;
                    let here = !is_filled(c, course) && fill >= need && (i == 0 || !whole);
                    *m = if here { self.material(c, base + lo + dy + height - 1) } else { None };
                    any |= here;
                }
                if any {
                    lay_layer(&mask, (lo + dy) as u8, height as u8, along_x, &mut out);
                }
            }
        }
        Some(StudChunk::from_disjoint(out))
    }
}

/// Covers the cells of `mask` (`z * 32 + x`, the material of each filled cell) at plate
/// `y` with box parts `height` plates tall: strips two studs wide along x (or z), each
/// run of one material cut longest-first, odd strips starting with a half-length piece;
/// cells a two-wide piece cannot take get one-wide pieces.
fn lay_layer(mask: &[Option<u16>], y: u8, height: u8, along_x: bool, out: &mut Vec<StudBrick>) {
    let at = |row: usize, l: usize| if along_x { mask[row * N + l] } else { mask[l * N + row] };
    let mut emit = |row: usize, mut l: usize, end: usize, width: u8, stagger: bool, material: u16| {
        let mut first = stagger;
        while l < end {
            let rest = (end - l) as u8;
            let cap = if first { STAGGER_FIRST } else { u8::MAX };
            first = false;
            let size = |len: u8| if along_x { [len, height, width] } else { [width, height, len] };
            let min = if along_x { [l as u8, y, row as u8] } else { [row as u8, y, l as u8] };
            let found = LENGTHS
                .iter()
                .filter(|&&len| len <= rest && len <= cap)
                .find_map(|&len| box_part(size(len)).map(|(part, rot)| (len, StudShape::Part { part, rot })));
            // The catalog has every 1 x 1 and 1 x 2; a filler keeps the cell solid if not.
            let (len, shape) = found.unwrap_or((1, StudShape::Filler { size: size(1) }));
            out.push(StudBrick { min, shape, material, data: 0 });
            l += len as usize;
        }
    };
    for strip in 0..N / 2 {
        let rows = [2 * strip, 2 * strip + 1];
        let stagger = strip % 2 == 1;
        // Cells left for one-wide pieces, per row.
        let mut single = [[None; N]; 2];
        let mut l = 0;
        while l < N {
            let (a, b) = (at(rows[0], l), at(rows[1], l));
            match a {
                Some(m) if a == b => {
                    let mut end = l + 1;
                    while end < N && at(rows[0], end) == a && at(rows[1], end) == a {
                        end += 1;
                    }
                    emit(rows[0], l, end, 2, stagger, m);
                    l = end;
                }
                _ => {
                    single[0][l] = a;
                    single[1][l] = b;
                    l += 1;
                }
            }
        }
        for (r, cells) in single.iter().enumerate() {
            let mut l = 0;
            while l < N {
                let Some(m) = cells[l] else {
                    l += 1;
                    continue;
                };
                let mut end = l + 1;
                while end < N && cells[end] == Some(m) {
                    end += 1;
                }
                emit(rows[r], l, end, 1, false, m);
                l = end;
            }
        }
    }
}
