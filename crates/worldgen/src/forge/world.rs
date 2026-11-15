//! The simulated world grid, run parameters, and the initial condition.

use crate::forge::knobs::{
    Knobs, LAYOUT_ARC, LAYOUT_ARCHIPELAGO, LAYOUT_CONTINENT, LAYOUT_CONTINENTS, LAYOUT_INLAND, LAYOUT_ONE_ISLAND,
};
use crate::forge::fields;
use crate::forge::par;
use crate::forge::plates;
use crate::noise::OctaveNoise;

pub const DEFAULT_SIZE_M: f64 = 16_000.0;
/// The quick dev world's cell (`ForgeParams::default()`: the server's
/// missing-world forge, tests, `just forge-proto` without flags). The
/// main-menu generator picks its own, finer cell.
pub const DEFAULT_CELL_M: f64 = 32.0;
pub const DEFAULT_SEED: u64 = 0x00D5_C0DE_0206;

/// World size the continental layouts' noise wavelengths are written for:
/// they are sampled at `span_m / REFERENCE_SIZE_M` (`span_m`), so a bigger
/// world is the same shape scaled up. The island layouts keep their metres
/// exactly.
pub const REFERENCE_SIZE_M: f64 = 16_000.0;

/// The one length a `width_m` x `height_m` world's size-relative lengths
/// (noise scale, belt widths, plate spacing, tapers) are shares of: its
/// shorter side, so a share fits either way. A square world's side.
pub fn span_m(width_m: f64, height_m: f64) -> f64 {
    width_m.min(height_m)
}

/// Belt uplift ramps from 0 at a land edge to full over about this distance
/// (the lowland share stays), so a range does not stand cut off at the edge.
pub const LAND_EDGE_TAPER_M: f64 = 2_000.0;
/// The taper length varies by up to this share either way along the edge
/// (a 4 km noise at the reference size), so where it ends is not a line.
const LAND_EDGE_TAPER_VARIATION: f64 = 0.5;
/// A land-edge outlet sits this much lower (rise/run) than the land cell just
/// inside it: the land runs on off the map, falling gently, so water leaves
/// across the edge instead of being dammed by it.
pub const LAND_EDGE_OUTLET_SLOPE: f64 = 0.004;
/// In a world too small for that, the taper is at most this share of its size.
const LAND_EDGE_TAPER_MAX_SHARE: f64 = 0.125;
/// Each sea edge's coast distance varies by this share of `continent_coast`.
const COAST_DISTANCE_VARIATION: f64 = 0.4;
/// Landness of a continental layout's interior, far from any sea: caps the
/// layout terms so the interior is uniform before the coast noise.
const CONTINENT_INTERIOR_MASS: f64 = 1.0;
/// Continents layout: centres are placed inside this half-width box, at
/// least `CONTINENT_CENTRE_GAP` apart (half-width units) when possible.
const CONTINENT_CENTRE_BOX: f64 = 0.55;
const CONTINENT_CENTRE_GAP: f64 = 0.6;
/// Inland layout: the inland sea's centre lies within this of the middle.
const INLAND_SEA_OFFSET: f64 = 0.3;

/// What lies past one edge of the map. Stored in the world file
/// (`WorldArtifact::edges`), so its variants are part of the format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EdgeKind {
    /// Sea: an ocean margin keeps the border drowned.
    Sea,
    /// Land running off the map: an open drainage boundary. Its border cells
    /// are outlets whose base level is their own height, which follows the
    /// land just inside (`World::follow_land_edges`).
    Land,
}

/// How `World::follow_land_edges` moves a land-edge outlet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeFollow {
    /// Exactly to the land inside it, less the outlet slope.
    Set,
    /// Only ever down to that.
    Lower,
}

/// Edge order in `World::edges` and `edges`: north (z = 0), east (x = max),
/// south (z = max), west (x = 0).
pub const EDGE_NAMES: [&str; 4] = ["north", "east", "south", "west"];

/// Which edges are sea. The island layouts are sea all round; the
/// continental ones take `Knobs::sea_edges` sea edges, which ones picked by
/// the seed — except inland, which has none.
pub fn edges(seed: u64, k: &Knobs) -> [EdgeKind; 4] {
    if !k.is_continental() {
        return [EdgeKind::Sea; 4];
    }
    let sea = if k.layout() == LAYOUT_INLAND { 0 } else { k.sea_edge_count() };
    let mut order = [0usize, 1, 2, 3];
    order.sort_by(|&a, &b| hash01(seed, 0xED6E, a as u64).total_cmp(&hash01(seed, 0xED6E, b as u64)));
    let mut out = [EdgeKind::Land; 4];
    for &e in &order[..sea] {
        out[e] = EdgeKind::Sea;
    }
    out
}

/// Proximity of a point (half-width units, -1..1 both ways) to each edge in
/// `edges` order: 1 on the edge, -1 on the opposite one.
fn edge_proximity(rx: f64, rz: f64) -> [f64; 4] {
    [-rz, rx, rz, -rx]
}

/// A continental layout's landness before coast noise: > 0 is land.
enum ContinentalMass {
    /// Coast `coast[e]` half-widths in from each sea edge.
    Continent { coast: [f64; 4], sea: [bool; 4] },
    /// As a continent, cut by seas along the Voronoi boundaries of `centres`.
    Continents { coast: [f64; 4], sea: [bool; 4], centres: Vec<(f64, f64)>, channel: f64 },
    /// Land all round an inland sea of `radius` at `centre`.
    Inland { centre: (f64, f64), radius: f64 },
}

impl ContinentalMass {
    fn new(seed: u64, k: &Knobs, edges: &[EdgeKind; 4]) -> ContinentalMass {
        let sea = edges.map(|e| e == EdgeKind::Sea);
        let mut coast = [0.0; 4];
        for (e, c) in coast.iter_mut().enumerate() {
            let u = hash01(seed, 0xC0A5, e as u64) - 0.5;
            *c = k.continent_coast * (1.0 + COAST_DISTANCE_VARIATION * u);
        }
        match k.layout() {
            LAYOUT_CONTINENTS => {
                let count = k.continent_count.round().clamp(2.0, 4.0) as usize;
                let mut centres: Vec<(f64, f64)> = Vec::new();
                let mut attempt = 0u64;
                while centres.len() < count {
                    attempt += 1;
                    let x = (2.0 * hash01(seed, 0xC0A6, attempt) - 1.0) * CONTINENT_CENTRE_BOX;
                    let z = (2.0 * hash01(seed, 0xC0A7, attempt) - 1.0) * CONTINENT_CENTRE_BOX;
                    let clear = attempt > 400
                        || centres.iter().all(|&(cx, cz)| (cx - x).hypot(cz - z) >= CONTINENT_CENTRE_GAP);
                    if clear {
                        centres.push((x, z));
                    }
                }
                ContinentalMass::Continents { coast, sea, centres, channel: k.continent_sea_width }
            }
            LAYOUT_INLAND => {
                let a = hash01(seed, 0xC0A8, 0) * std::f64::consts::TAU;
                let d = INLAND_SEA_OFFSET * hash01(seed, 0xC0A8, 1);
                ContinentalMass::Inland { centre: (d * a.cos(), d * a.sin()), radius: k.inland_sea_size }
            }
            layout => {
                debug_assert_eq!(layout, LAYOUT_CONTINENT);
                ContinentalMass::Continent { coast, sea }
            }
        }
    }

    fn at(&self, rx: f64, rz: f64) -> f64 {
        let prox = edge_proximity(rx, rz);
        let coastal = |coast: &[f64; 4], sea: &[bool; 4]| {
            (0..4).filter(|&e| sea[e]).map(|e| 1.0 - prox[e] - coast[e]).fold(CONTINENT_INTERIOR_MASS, f64::min)
        };
        match self {
            ContinentalMass::Continent { coast, sea } => coastal(coast, sea),
            ContinentalMass::Continents { coast, sea, centres, channel } => {
                let (mut a, mut b) = ((f64::INFINITY, 0usize), (f64::INFINITY, 0usize));
                for (j, &(cx, cz)) in centres.iter().enumerate() {
                    let d = (rx - cx).powi(2) + (rz - cz).powi(2);
                    if d < a.0 {
                        b = a;
                        a = (d, j);
                    } else if d < b.0 {
                        b = (d, j);
                    }
                }
                let (ca, cb) = (centres[a.1], centres[b.1]);
                let gap = (cb.0 - ca.0).hypot(cb.1 - ca.1).max(1.0e-9);
                let to_strait = (b.0 - a.0) / (2.0 * gap);
                coastal(coast, sea).min(to_strait - channel)
            }
            ContinentalMass::Inland { centre, radius } => {
                ((rx - centre.0).hypot(rz - centre.1) - radius).min(CONTINENT_INTERIOR_MASS)
            }
        }
    }
}

/// A continental layout's landness at any point: the layout's mass plus the
/// coast noises (scaled with the world), less the ocean margin along the sea
/// edges. `World::new` and `volcano_sites` share it.
struct ContinentalShape {
    edges: [EdgeKind; 4],
    mass: ContinentalMass,
    shape_lo: OctaveNoise,
    shape_hi: OctaveNoise,
    half_x: f64,
    half_z: f64,
    width_m: f64,
    height_m: f64,
    scale: f64,
    coast_noise: (f64, f64),
    margin: (f64, f64),
}

impl ContinentalShape {
    fn new(p: &ForgeParams) -> ContinentalShape {
        let k = &p.knobs;
        let edges = edges(p.seed, k);
        ContinentalShape {
            edges,
            mass: ContinentalMass::new(p.seed, k, &edges),
            // Same noises as the island layouts, sampled at the world's scale.
            shape_lo: OctaveNoise::new(p.seed ^ 0xF001, -13, vec![1.0, 0.6, 0.3]), // 8 km at the reference size
            shape_hi: OctaveNoise::new(p.seed ^ 0xF002, -10, vec![1.0, 0.5]), // 1 km
            half_x: p.width_m * 0.5,
            half_z: p.height_m * 0.5,
            width_m: p.width_m,
            height_m: p.height_m,
            scale: p.span_m() / REFERENCE_SIZE_M,
            coast_noise: (k.coast_noise_lo, k.coast_noise_hi),
            margin: (k.ocean_margin_start, (k.ocean_margin_start + OCEAN_MARGIN_RAMP).min(0.99)),
        }
    }

    /// (shape, landness) at (mx, mz) m: landness > 0 is land; shape < 0 is
    /// the sea's own (not the margin's) — shelf and abyss.
    fn at(&self, mx: f64, mz: f64) -> (f64, f64) {
        let (rx, rz) = ((mx - self.half_x) / self.half_x, (mz - self.half_z) / self.half_z);
        let (sx, sz) = (mx / self.scale, mz / self.scale);
        let shape = self.mass.at(rx, rz)
            + self.coast_noise.0 * self.shape_lo.sample2(sx, sz)
            + self.coast_noise.1 * self.shape_hi.sample2(sx, sz);
        let prox = edge_proximity(rx, rz);
        let sea_prox = (0..4).filter(|&e| self.edges[e] == EdgeKind::Sea).map(|e| prox[e]).fold(-1.0, f64::max);
        (shape, shape - 2.0 * smoothstep(self.margin.0, self.margin.1, sea_prox))
    }

    /// Distance (m) to the nearest land edge; infinite without one.
    fn to_land_edge(&self, mx: f64, mz: f64) -> f64 {
        // In `edges` order.
        let to_edge = [mz, self.width_m - mx, self.height_m - mz, mx];
        (0..4).filter(|&e| self.edges[e] == EdgeKind::Land).map(|e| to_edge[e]).fold(f64::INFINITY, f64::min)
    }
}

/// The volcano sites of a world (`fields::volcano_sites_on`): continental
/// layouts only (the island layouts have no plates, and their terrain stays
/// as it was), on land at least `fields::VOLCANO_MIN_LANDNESS` inland and so
/// far from any land edge that the cone ends before the edge's belt taper
/// starts. From the seed and knobs only — not the cell size — so every level
/// of the erosion schedule raises the same cones, and the world file stores
/// the same list.
pub fn volcano_sites(p: &ForgeParams) -> Vec<[f32; 2]> {
    if !p.knobs.is_continental() {
        return Vec::new();
    }
    let shape = ContinentalShape::new(p);
    let clearance = LAND_EDGE_TAPER_M.min(LAND_EDGE_TAPER_MAX_SHARE * p.span_m()) * (1.0 + LAND_EDGE_TAPER_VARIATION)
        + fields::VOLCANO_CONE_RADIUS_M;
    fields::volcano_sites_on(p, &|mx, mz| {
        shape.at(mx, mz).1 > fields::VOLCANO_MIN_LANDNESS && shape.to_land_edge(mx, mz) >= clearance
    })
}

/// The ocean margin ramps from fully open to fully drowned over this share of
/// the half-width past `Knobs::ocean_margin_start`.
const OCEAN_MARGIN_RAMP: f64 = 0.13;

/// In a multi-island layout the largest island is `MAIN_ISLAND_SHARE /
/// sqrt(count)` of `island_extent`, clamped: several islands have to share the
/// world one island had to itself, and more islands means smaller ones.
const MAIN_ISLAND_SHARE: f64 = 1.25;
const MAIN_ISLAND_SHARE_BOUNDS: (f64, f64) = (0.30, 0.62);
/// Island arcs are volcanic: the core of each island gets this share of the
/// peak uplift whether or not a mountain belt crosses it.
const ARC_CORE_UPLIFT: f64 = 0.85;
/// The smallest island, as a share of the largest.
const SMALLEST_ISLAND_SHARE: f64 = 0.22;
/// Islands keep this far inside the world (half-width units), margin included.
const ISLAND_REACH_LIMIT: f64 = 0.80;
/// Neighbouring islands blend over this much landness: low necks and
/// isthmuses instead of two circles touching at a point.
const ISLAND_BLEND: f64 = 0.07;
/// Island size the inland/relief rules were tuned on (the default single
/// island). Smaller islands are read as scaled copies of it, so an islet gets
/// hills in proportion instead of being a flat sandbank.
const REFERENCE_EXTENT: f64 = 0.66;

/// One island of the layout: center and radius in half-width units
/// (the world spans -1..1 on both axes).
struct Blob {
    x: f64,
    z: f64,
    r: f64,
}

/// Island placement for the chosen layout. One island = a single blob at the
/// center with radius `island_extent`, which reproduces the original formula
/// `island_extent - radial` bit for bit.
fn layout_blobs(seed: u64, k: &Knobs) -> Vec<Blob> {
    let layout = k.layout();
    if layout == LAYOUT_ONE_ISLAND {
        return vec![Blob { x: 0.0, z: 0.0, r: k.island_extent }];
    }
    let count = k.island_count.round().clamp(2.0, 14.0) as usize;
    let main = k.island_extent
        * (MAIN_ISLAND_SHARE / (count as f64).sqrt()).clamp(MAIN_ISLAND_SHARE_BOUNDS.0, MAIN_ISLAND_SHARE_BOUNDS.1);
    let size = |u: f64| main * (1.0 - k.island_size_variation * u).max(SMALLEST_ISLAND_SHARE);
    let fit = |mut b: Blob| {
        // Keep the whole island inside the world: shrink what sticks out.
        let d = (b.x * b.x + b.z * b.z).sqrt();
        b.r = b.r.min(ISLAND_REACH_LIMIT - d).max(0.06);
        b
    };
    let mut blobs: Vec<Blob> = Vec::new();
    if layout == LAYOUT_ARC {
        // Islands along a circular arc through the world center, largest in
        // the middle — the shape of a volcanic island chain.
        let turn = hash01(seed, 0xA2C, 0) * std::f64::consts::TAU;
        let radius = 1.3;
        let (cx, cz) = (radius * turn.cos(), radius * turn.sin());
        let half_angle = (k.island_spread / radius).min(1.0).asin();
        // Neighbours along the arc are this far apart; an island wider than
        // that (times the separation knob) would fuse the chain into one ridge.
        let spacing = 2.0 * radius * (half_angle / (count - 1).max(1) as f64).sin();
        for i in 0..count {
            let t = if count == 1 { 0.0 } else { 2.0 * i as f64 / (count - 1) as f64 - 1.0 };
            let jitter = (hash01(seed, 0xA2C, 1 + i as u64) - 0.5) * 0.5 / count as f64;
            let angle = turn + std::f64::consts::PI + (t + jitter) * half_angle;
            let off = (hash01(seed, 0xA2C, 100 + i as u64) - 0.5) * 0.10;
            let (x, z) = (cx + (radius + off) * angle.cos(), cz + (radius + off) * angle.sin());
            // The middle of the chain may crowd its neighbours a little.
            let room = 0.5 * spacing / k.island_separation * (1.45 - 0.45 * t.abs());
            blobs.push(fit(Blob { x, z, r: size(t.abs()).min(room) }));
        }
        return blobs;
    }
    debug_assert_eq!(layout, LAYOUT_ARCHIPELAGO);
    // Archipelago: the main island near the center, the rest scattered in a
    // disc, each accepted only if it keeps its distance from the others.
    let a = hash01(seed, 0xA2D, 0) * std::f64::consts::TAU;
    let d = 0.18 * k.island_spread * hash01(seed, 0xA2D, 1);
    blobs.push(fit(Blob { x: d * a.cos(), z: d * a.sin(), r: main }));
    let mut attempt = 0u64;
    while blobs.len() < count && attempt < 400 {
        attempt += 1;
        let angle = hash01(seed, 0xA2E, attempt) * std::f64::consts::TAU;
        let dist = k.island_spread * hash01(seed, 0xA2F, attempt).sqrt(); // uniform over the disc
        let u = hash01(seed, 0xA30, attempt);
        let candidate = fit(Blob { x: dist * angle.cos(), z: dist * angle.sin(), r: size(0.25 + 0.75 * u) });
        let clear = blobs.iter().all(|b| {
            let gap = ((b.x - candidate.x).powi(2) + (b.z - candidate.z).powi(2)).sqrt();
            gap >= (b.r + candidate.r) * k.island_separation
        });
        if clear {
            blobs.push(candidate);
        }
    }
    blobs
}

/// The layout at one point, before coast noise.
struct Landmass {
    /// Landness: > 0 inside an island (smooth union of the blobs).
    mass: f64,
    /// Radius of the island that shapes this point, blended smoothly between
    /// neighbours so nothing derived from it shows a seam in the sea.
    radius: f64,
    /// 1 at the center of the dominant island, 0 on its edge.
    core: f64,
}

fn landmass(blobs: &[Blob], rx: f64, rz: f64) -> Landmass {
    let (mut best, mut best_r, mut blended) = (f64::MIN, 1.0, f64::MIN);
    for b in blobs {
        let v = b.r - ((rx - b.x) * (rx - b.x) + (rz - b.z) * (rz - b.z)).sqrt();
        if v > best {
            best = v;
            best_r = b.r;
        }
        // Polynomial smooth maximum: equals max() once the two differ by ISLAND_BLEND.
        blended = if blended == f64::MIN {
            v
        } else {
            let h = (ISLAND_BLEND - (blended - v).abs()).max(0.0) / ISLAND_BLEND;
            blended.max(v) + h * h * ISLAND_BLEND * 0.25
        };
    }
    // Radius blended by closeness to each island (inverse-square of how far
    // behind the leader it is): continuous where the leader changes.
    let (mut sum_w, mut sum_r) = (0.0, 0.0);
    for b in blobs {
        let v = b.r - ((rx - b.x) * (rx - b.x) + (rz - b.z) * (rz - b.z)).sqrt();
        let w = 1.0 / ((0.03 + (best - v)) * (0.03 + (best - v)));
        sum_w += w;
        sum_r += w * b.r;
    }
    Landmass { mass: blended, radius: sum_r / sum_w, core: (best / best_r).clamp(0.0, 1.0) }
}

/// Lowest land height (m): erosion and diffusion clamp land to it too.
pub const LAND_FLOOR_M: f64 = 0.05;

const NEIGHBORS: [(i32, i32); 8] =
    [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

/// What a forge run needs: world size (x across `width_m`, z down
/// `height_m`), grid, seed and every knob.
#[derive(Clone)]
pub struct ForgeParams {
    pub width_m: f64,
    pub height_m: f64,
    pub cell_m: f64,
    pub seed: u64,
    /// Every tunable of the simulation (knobs.rs).
    pub knobs: Knobs,
    /// Skip the progress printouts.
    pub quiet: bool,
}

impl Default for ForgeParams {
    fn default() -> Self {
        ForgeParams::square(DEFAULT_SIZE_M, DEFAULT_CELL_M)
    }
}

impl ForgeParams {
    /// A `size_m` x `size_m` world at `cell_m`, default seed and knobs, quiet.
    pub fn square(size_m: f64, cell_m: f64) -> Self {
        ForgeParams {
            width_m: size_m,
            height_m: size_m,
            cell_m,
            seed: DEFAULT_SEED,
            knobs: Knobs::default(),
            quiet: true,
        }
    }

    /// Grid columns (x).
    pub fn nx(&self) -> usize {
        (self.width_m / self.cell_m).round() as usize
    }

    /// Grid rows (z).
    pub fn nz(&self) -> usize {
        (self.height_m / self.cell_m).round() as usize
    }

    /// `span_m` of this world.
    pub fn span_m(&self) -> f64 {
        span_m(self.width_m, self.height_m)
    }
}

pub fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Uniform hash in [0, 1) of (seed, a, b).
pub fn hash01(seed: u64, a: u64, b: u64) -> f64 {
    let mut z = seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// The simulated world: an `nx` x `nz` grid, row-major (`nz` rows of `nx`
/// columns, `i = z * nx + x`). Heights are meters relative to sea level
/// (engine y = SEA_LEVEL + h).
pub struct World {
    /// Columns (x, west to east).
    pub nx: usize,
    /// Rows (z, north to south).
    pub nz: usize,
    pub dx: f64,
    /// Ground height.
    pub h: Vec<f64>,
    /// Fixed sea cells: the base level everything drains to.
    pub ocean: Vec<bool>,
    /// Rock uplift rate (m/yr).
    pub uplift: Vec<f64>,
    /// Total rock uplift applied so far (m): the rock now at height y started
    /// at y - uplifted, which is what the geology column is indexed by.
    pub uplifted: Vec<f64>,
    /// Would-be land the ocean margin drowned (a big number = the island is
    /// pressing against the world edge; the seed search penalises it).
    pub margin_drowned: usize,
    /// What lies past each edge (`edges` order: N, E, S, W).
    pub edges: [EdgeKind; 4],
    /// Cells drainage ends at: the sea, and the border cells of land edges
    /// (never eroded, uplifted or diffused: they follow the land just inside,
    /// `follow_land_edges`). Every border cell is one. Routing gives outlets
    /// no receiver.
    pub outlet: Vec<bool>,
}

impl World {
    /// Island + uplift map from noise. Wavelength of a noise's first octave is
    /// 2^-first_octave meters (all sampled at scale 1).
    pub fn new(p: &ForgeParams) -> World {
        if p.knobs.is_continental() {
            return World::new_continental(p);
        }
        let (nx, nz) = (p.nx(), p.nz());
        let (half_x, half_z) = (p.width_m * 0.5, p.height_m * 0.5);
        let shape_lo = OctaveNoise::new(p.seed ^ 0xF001, -13, vec![1.0, 0.6, 0.3]); // 8 km
        let shape_hi = OctaveNoise::new(p.seed ^ 0xF002, -10, vec![1.0, 0.5]); // 1 km
        let belts = OctaveNoise::new(p.seed ^ 0xF003, -12, vec![1.0, 0.5]); // 4 km
        let k = &p.knobs;
        let blobs = layout_blobs(p.seed, k);
        let one_island = k.layout() == LAYOUT_ONE_ISLAND;
        let arc = k.layout() == LAYOUT_ARC;
        let rough = OctaveNoise::new(p.seed ^ 0xF004, -8, vec![1.0, 1.0, 1.0]); // 256 m

        let mut w = World {
            nx,
            nz,
            dx: p.cell_m,
            h: vec![0.0; nx * nz],
            ocean: vec![false; nx * nz],
            uplift: vec![0.0; nx * nz],
            uplifted: vec![0.0; nx * nz],
            margin_drowned: 0,
            edges: [EdgeKind::Sea; 4],
            outlet: Vec::new(),
        };
        for z in 0..nz {
            for x in 0..nx {
                let (mx, mz) = ((x as f64 + 0.5) * p.cell_m, (z as f64 + 0.5) * p.cell_m);
                let (rx, rz) = ((mx - half_x) / half_x, (mz - half_z) / half_z);
                // Superellipse (p = 4) distance to the rim: 1 on the world edge,
                // rounder than max(|x|, |z|) so the margin is not a visible frame.
                let edge = (rx.powi(4) + rz.powi(4)).sqrt().sqrt();
                // The coast-shaping noise fades toward the rim, so the island
                // wanders freely in the middle but stays clear of the margin.
                let reach = 1.0 - 0.75 * smoothstep(0.50, 0.95, edge);
                let lm = landmass(&blobs, rx, rz);
                // Everything tuned on the reference island is rescaled to the
                // island at hand (all three factors are exactly 1 for one island).
                let size = if one_island { 1.0 } else { lm.radius / REFERENCE_EXTENT };
                let relief_scale = 1.0 / size;
                // Coast noise at full strength fuses or drowns small islands.
                // The 8 km wander shrinks with the island (size^0.75); the
                // 1 km detail barely does (size^0.25) — it is what gives an
                // islet a coastline instead of a circle.
                let (wander, detail) = (size.sqrt() * size.sqrt().sqrt(), size.sqrt().sqrt());
                let shape = lm.mass
                    + reach
                        * (wander * k.coast_noise_lo * shape_lo.sample2(mx, mz)
                            + detail * k.coast_noise_hi * shape_hi.sample2(mx, mz));
                // "Landness": > 0 is land. Beyond the margin it is forced under
                // water, which is what guarantees "ocean all round".
                let margin_end = (k.ocean_margin_start + OCEAN_MARGIN_RAMP).min(0.99);
                let land = shape - 2.0 * smoothstep(k.ocean_margin_start, margin_end, edge);

                let i = z * nx + x;
                if land <= 0.0 {
                    w.ocean[i] = true;
                    w.h[i] = if shape < 0.0 {
                        (shape * 160.0 - 1.0).max(-90.0) // shelf, then abyss
                    } else {
                        w.margin_drowned += 1;
                        (land * 40.0 - 1.0).max(-20.0) // drowned by the margin: shoals
                    };
                    continue;
                }
                // Low initial relief: erosion, not noise, is to make the shape.
                // Small islands are read as scaled copies of the reference one.
                let inland_ness = land * relief_scale;
                w.h[i] = (1.0 + 45.0 * smoothstep(0.0, 0.5, inland_ness) + 3.0 * rough.sample2(mx, mz))
                    .max(0.5);
                // Mountain belts along the zero contour of a low-frequency
                // noise (long, linear), only inland; none on the coast.
                let belt_noise = belts.sample2(mx * k.belt_scale, mz * k.belt_scale);
                let belt = 1.0 - smoothstep(0.0, k.belt_width, belt_noise.abs());
                let inland = smoothstep(0.06, k.inland_gate.max(0.07), inland_ness);
                let volcanic = if arc { ARC_CORE_UPLIFT * smoothstep(0.35, 0.9, lm.core) } else { 0.0 };
                let coast_taper = smoothstep(0.0, 0.05, land);
                w.uplift[i] = k.uplift
                    * coast_taper
                    // Grouped as ((1 - share) * belt) * inland, exactly as before
                    // the layouts existed: regrouping shifts the last bit of the
                    // uplift, which the f64 discharge sums then carry into the
                    // world file.
                    * (k.lowland_uplift_share
                        + ((1.0 - k.lowland_uplift_share) * belt * inland)
                            .max((1.0 - k.lowland_uplift_share) * volcanic));
            }
        }
        for i in w.border_cells() {
            assert!(w.ocean[i], "ocean margin must reach the world edge");
        }
        w.outlet = w.ocean.clone();
        w
    }

    /// Continent / continents / inland: the layout's landness plus coast
    /// noise (wavelengths scaled with the world), an ocean margin along the
    /// sea edges only, and uplift belts along convergent plate boundaries
    /// (`plates`) tapering to 0 toward the land edges, plus a cone of uplift
    /// at each volcano (`volcano_sites`).
    fn new_continental(p: &ForgeParams) -> World {
        let (nx, nz) = (p.nx(), p.nz());
        let k = &p.knobs;
        let scale = p.span_m() / REFERENCE_SIZE_M;
        let rough = OctaveNoise::new(p.seed ^ 0xF004, -8, vec![1.0, 1.0, 1.0]); // 256 m
        let taper_noise = OctaveNoise::new(p.seed ^ 0xF005, -12, vec![1.0, 0.5]); // 4 km
        let shape_of = ContinentalShape::new(p);
        let edges = shape_of.edges;
        let field = plates::plate_field(p.seed, p.width_m, p.height_m, nx, nz, k);
        let ranges = plates::interior_ranges(p.seed, p.width_m, p.height_m, k);
        let taper_m = LAND_EDGE_TAPER_M.min(LAND_EDGE_TAPER_MAX_SHARE * p.span_m());
        let volcanoes = volcano_sites(p);

        let mut w = World {
            nx,
            nz,
            dx: p.cell_m,
            h: vec![0.0; nx * nz],
            ocean: vec![false; nx * nz],
            uplift: vec![0.0; nx * nz],
            uplifted: vec![0.0; nx * nz],
            margin_drowned: 0,
            edges,
            outlet: vec![false; nx * nz],
        };
        for z in 0..nz {
            for x in 0..nx {
                let (mx, mz) = ((x as f64 + 0.5) * p.cell_m, (z as f64 + 0.5) * p.cell_m);
                let (sx, sz) = (mx / scale, mz / scale);
                let (shape, land) = shape_of.at(mx, mz);

                let i = z * nx + x;
                if land <= 0.0 {
                    w.ocean[i] = true;
                    w.h[i] = if shape < 0.0 {
                        (shape * 160.0 - 1.0).max(-90.0) // shelf, then abyss
                    } else {
                        w.margin_drowned += 1;
                        (land * 40.0 - 1.0).max(-20.0) // drowned by the margin: shoals
                    };
                    continue;
                }
                w.h[i] = (1.0 + 45.0 * smoothstep(0.0, 0.5, land) + 3.0 * rough.sample2(sx, sz)).max(0.5);
                let interior = plates::interior_strength(&ranges, mx, mz, p.width_m, p.height_m, k);
                let belt = (field.convergence[i] as f64).max(interior);
                let to_land_edge = shape_of.to_land_edge(mx, mz);
                // Only the belts taper: the lowland share runs on to the edge,
                // so the land near it neither sinks below the edge nor rises
                // into a divide where a uniform taper would end.
                let taper = taper_m * (1.0 + LAND_EDGE_TAPER_VARIATION * taper_noise.sample2(sx, sz).clamp(-1.0, 1.0));
                let edge_taper = if to_land_edge.is_finite() { smoothstep(0.0, taper, to_land_edge) } else { 1.0 };
                let coast_taper = smoothstep(0.0, 0.05, land);
                w.uplift[i] = k.uplift
                    * coast_taper
                    * (k.lowland_uplift_share
                        + (k.belt_uplift - k.lowland_uplift_share).max(0.0)
                            * (belt * edge_taper).max(fields::cone(&volcanoes, mx, mz)));
            }
        }
        w.outlet.clone_from(&w.ocean);
        // (cell, its edge) for the four border runs; corners are on two.
        for x in 0..nx {
            for (i, e) in [(x, 0), ((nz - 1) * nx + x, 2)] {
                if edges[e] == EdgeKind::Land {
                    w.outlet[i] = true;
                }
            }
        }
        for z in 0..nz {
            for (i, e) in [(z * nx + nx - 1, 1), (z * nx, 3)] {
                if edges[e] == EdgeKind::Land {
                    w.outlet[i] = true;
                }
            }
        }
        for i in w.border_cells() {
            assert!(w.outlet[i], "every border cell must be sea or on a land edge (cell {i})");
        }
        w.follow_land_edges(EdgeFollow::Set);
        w
    }

    /// Hold every land-edge outlet at most `LAND_EDGE_OUTLET_SLOPE` below the
    /// land just inside it (sea level if that cell is sea): the edge is an
    /// open boundary that follows the land behind it DOWN — never a lip that
    /// dams it — but never up, so it stays a base level the uplifted land
    /// erodes toward (an edge that rose with the land would let the strip
    /// along it climb forever). `EdgeFollow::Set` places it exactly there (an
    /// initial condition). Reads only interior cells and writes only border
    /// cells, so the result does not depend on order. A no-op without land
    /// edges (islands). Called on every initial condition (`Set`), after
    /// every erosion step and after the post-erosion shaping (`Lower`).
    pub fn follow_land_edges(&mut self, mode: EdgeFollow) {
        if !self.edges.contains(&EdgeKind::Land) || self.nx < 3 || self.nz < 3 {
            return;
        }
        let inward = |c: usize, last: usize| if c == 0 { 1 } else if c == last { last - 1 } else { c };
        for i in self.border_cells() {
            if self.ocean[i] || !self.outlet[i] {
                continue;
            }
            let (x, z) = self.xz(i);
            let (ix, iz) = (inward(x, self.nx - 1), inward(z, self.nz - 1));
            let inner = self.index(ix, iz);
            let dist = if ix != x && iz != z { std::f64::consts::SQRT_2 * self.dx } else { self.dx };
            let target = if self.ocean[inner] {
                LAND_FLOOR_M
            } else {
                (self.h[inner] - LAND_EDGE_OUTLET_SLOPE * dist).max(LAND_FLOOR_M)
            };
            self.h[i] = match mode {
                EdgeFollow::Set => target,
                EdgeFollow::Lower => self.h[i].min(target),
            };
        }
    }

    /// Cells in the grid (`nx * nz`).
    pub fn cells(&self) -> usize {
        self.nx * self.nz
    }

    /// (x, z) of cell `i`.
    pub fn xz(&self, i: usize) -> (usize, usize) {
        (i % self.nx, i / self.nx)
    }

    /// Cell index of column `x`, row `z`.
    pub fn index(&self, x: usize, z: usize) -> usize {
        z * self.nx + x
    }

    /// True for a cell on the outermost ring of the grid.
    pub fn on_border(&self, i: usize) -> bool {
        let (x, z) = self.xz(i);
        x == 0 || z == 0 || x + 1 == self.nx || z + 1 == self.nz
    }

    /// Every cell of the outermost ring (corners more than once).
    pub fn border_cells(&self) -> impl Iterator<Item = usize> + use<> {
        let (nx, nz) = (self.nx, self.nz);
        (0..nx).flat_map(move |x| [x, (nz - 1) * nx + x]).chain((0..nz).flat_map(move |z| [z * nx, z * nx + nx - 1]))
    }

    /// This world's evolved state carried onto the finer grid of `p_fine`
    /// (the coarse-to-fine erosion schedule): `h` and `uplifted` bilinear;
    /// `uplift` and `ocean` (with the sea floor) recomputed at fine
    /// resolution from the same noise as `World::new`, so the coast and the
    /// belts are as sharp as a fine run's.
    pub fn upsample(&self, p_fine: &ForgeParams) -> World {
        self.upsample_onto(World::new(p_fine))
    }

    /// `upsample` onto an already built fine initial condition (`World::new`
    /// of the fine params), which keeps its `ocean`, `outlet`, `uplift` and
    /// its outlets' heights (sea floor, land-edge base levels); other land
    /// cells take this world's state. Per cell and in
    /// fixed order: the result does not depend on the thread count.
    pub fn upsample_onto(&self, mut fine: World) -> World {
        let (ncx, ncz, nfx) = (self.nx, self.nz, fine.nx);
        // Coarse sea cells read as sea level for land, as diffusion's shore
        // does: a coast cell must not inherit the shelf's depth.
        let coarse_h: Vec<f64> =
            self.h.iter().zip(&self.ocean).map(|(&h, &sea)| if sea { 0.0 } else { h }).collect();
        // Cell centers matched by real position (metres), not share of the
        // grid: a level whose `size / cell` rounds (16 km at 256 m: 62.5 ->
        // 63 cells) covers a little more or less than the fine grid, and a
        // share mapping would stretch it over the fine one. Fine cells past
        // the coarse grid's centers clamp to its edge.
        let ratio = fine.dx / self.dx;
        let axis = |j: usize, nc: usize| {
            let u = ((j as f64 + 0.5) * ratio - 0.5).clamp(0.0, (nc - 1) as f64);
            let i0 = (u.floor() as usize).min(nc.saturating_sub(2));
            (i0, (i0 + 1).min(nc - 1), u - i0 as f64)
        };
        let sample = |grid: &[f64], i: usize| {
            let ((x0, x1, tx), (z0, z1, tz)) = (axis(i % nfx, ncx), axis(i / nfx, ncz));
            let row = |z: usize| grid[z * ncx + x0] * (1.0 - tx) + grid[z * ncx + x1] * tx;
            row(z0) * (1.0 - tz) + row(z1) * tz
        };
        // Outlets (the sea, land-edge border cells) keep the fine initial
        // condition: a land edge's base level is its own height.
        let outlet = &fine.outlet;
        let cells = fine.cells();
        let h = par::map_range(cells, |i| if outlet[i] { fine.h[i] } else { sample(&coarse_h, i).max(LAND_FLOOR_M) });
        let uplifted = par::map_range(cells, |i| if outlet[i] { 0.0 } else { sample(&self.uplifted, i) });
        fine.h = h;
        fine.uplifted = uplifted;
        fine.follow_land_edges(EdgeFollow::Set);
        fine
    }

    /// 8-neighbors of cell `i` with their distance in meters.
    pub fn neighbors(&self, i: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let (x, z) = ((i % self.nx) as i32, (i / self.nx) as i32);
        let (w, h) = (self.nx as i32, self.nz as i32);
        NEIGHBORS.iter().filter_map(move |&(ox, oz)| {
            let (jx, jz) = (x + ox, z + oz);
            (jx >= 0 && jz >= 0 && jx < w && jz < h).then(|| {
                let dist = if ox != 0 && oz != 0 { std::f64::consts::SQRT_2 } else { 1.0 };
                ((jz * w + jx) as usize, dist * self.dx)
            })
        })
    }

    /// Cell center in world meters.
    pub fn center(&self, i: usize) -> (f64, f64) {
        (((i % self.nx) as f64 + 0.5) * self.dx, ((i / self.nx) as f64 + 0.5) * self.dx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsample_keeps_fine_mask_and_interpolates_land() {
        let fine_p = ForgeParams::square(4_096.0, 64.0);
        let coarse_p = ForgeParams { cell_m: 128.0, ..fine_p.clone() };
        let mut coarse = World::new(&coarse_p);
        // A recognisable evolved state: land raised by a ramp, some uplift spent.
        for i in 0..coarse.cells() {
            if !coarse.ocean[i] {
                coarse.h[i] += coarse.xz(i).0 as f64;
                coarse.uplifted[i] = 7.0;
            }
        }
        let up = coarse.upsample(&fine_p);
        let fresh = World::new(&fine_p);
        assert_eq!((up.nx, up.nz, up.dx), (fresh.nx, fresh.nz, fresh.dx));
        assert_eq!(up.ocean, fresh.ocean);
        assert_eq!(up.uplift, fresh.uplift);
        for i in 0..up.cells() {
            if up.ocean[i] {
                assert_eq!((up.h[i], up.uplifted[i]), (fresh.h[i], 0.0));
            } else {
                assert!(up.h[i] >= LAND_FLOOR_M);
                assert!(up.uplifted[i] <= 7.0 && up.uplifted[i] >= 0.0);
            }
        }
        // Away from the coast, the interpolated land carries the coarse ramp.
        let (x, z) = (up.nx / 2, up.nz / 2);
        let (fine_i, coarse_i) = (up.index(x, z), coarse.index(x / 2, z / 2));
        if !coarse.ocean[coarse_i] && !up.ocean[fine_i] {
            assert!((up.h[fine_i] - coarse.h[coarse_i]).abs() < 30.0);
            assert_eq!(up.uplifted[fine_i], 7.0);
        }
        // Same input, same bytes (parallel map, fixed order).
        assert_eq!(coarse.upsample(&fine_p).h, up.h);
    }

    /// A level whose size does not divide by its cell (16 km at 256 m: 62.5
    /// rounds to 63 cells, 16,128 m) maps onto the fine grid by position: a
    /// coarse ramp of height = 1 km + x metres comes out as the same ramp at
    /// the fine cell centers, not stretched by the grids' share.
    #[test]
    fn upsample_aligns_by_position_when_size_does_not_divide() {
        let fine_p = ForgeParams::square(16_000.0, 128.0);
        let coarse_p = ForgeParams { cell_m: 256.0, ..fine_p.clone() };
        let mut coarse = World::new(&coarse_p);
        assert_eq!((coarse.nx, World::new(&fine_p).nx), (63, 125), "the sizes must not divide");
        for i in 0..coarse.cells() {
            coarse.ocean[i] = false;
            coarse.h[i] = 1_000.0 + coarse.center(i).0;
        }
        let up = coarse.upsample(&fine_p);
        let first_center = coarse.dx * 0.5;
        let mut checked = 0;
        for i in 0..up.cells() {
            let (x, z) = up.xz(i);
            let (cx, _) = up.center(i);
            if up.outlet[i] || x < 4 || z < 4 || x + 4 >= up.nx || z + 4 >= up.nz || cx < first_center {
                continue;
            }
            assert!((up.h[i] - (1_000.0 + cx)).abs() < 1e-6, "cell {i}: {} vs {}", up.h[i], 1_000.0 + cx);
            checked += 1;
        }
        assert!(checked > 1_000, "checked {checked} cells");
    }

    fn rectangular() -> World {
        World::new(&ForgeParams { width_m: 8_192.0, height_m: 4_096.0, cell_m: 64.0, ..ForgeParams::default() })
    }

    #[test]
    fn rectangular_world_has_its_own_dimensions() {
        let w = rectangular();
        assert_eq!((w.nx, w.nz), (128, 64));
        assert_eq!((w.h.len(), w.cells()), (128 * 64, 128 * 64));
        assert_eq!(w.xz(w.index(100, 50)), (100, 50));
        assert!(w.center(w.index(127, 0)).0 > 8_000.0);
    }

    #[test]
    fn neighbors_respect_both_bounds() {
        let w = rectangular();
        let mut got: Vec<usize> = w.neighbors(w.index(127, 63)).map(|(j, _)| j).collect();
        got.sort_unstable();
        let mut want = vec![w.index(126, 63), w.index(127, 62), w.index(126, 62)];
        want.sort_unstable();
        assert_eq!(got, want);
    }

    fn continental(layout: u32, seed: u64, size_m: f64, cell_m: f64) -> ForgeParams {
        let mut p = ForgeParams { seed, ..ForgeParams::square(size_m, cell_m) };
        p.knobs.layout = layout as f64;
        p
    }

    /// Share of the cells in edge `e`'s outer 5% band that `pick` holds.
    fn band_share(w: &World, e: usize, pick: impl Fn(usize) -> bool) -> f64 {
        // Depth into the map and length along edge `e`.
        let (across, along) = if e % 2 == 0 { (w.nz, w.nx) } else { (w.nx, w.nz) };
        let depth = ((across as f64 * 0.05).ceil() as usize).max(1);
        let cells: Vec<usize> = (0..depth)
            .flat_map(|d| {
                (0..along).map(move |b| match e {
                    0 => w.index(b, d),
                    1 => w.index(w.nx - 1 - d, b),
                    2 => w.index(b, w.nz - 1 - d),
                    _ => w.index(d, b),
                })
            })
            .collect();
        cells.iter().filter(|&&i| pick(i)).count() as f64 / cells.len() as f64
    }

    #[test]
    fn continent_has_requested_sea_edges() {
        for seed in [1, 2, 3, DEFAULT_SEED] {
            let p = continental(LAYOUT_CONTINENT, seed, 16_000.0, 128.0);
            assert_eq!(p.knobs.sea_edge_count(), 2);
            let w = World::new(&p);
            assert_eq!(w.edges, edges(seed, &p.knobs));
            let sea: Vec<usize> = (0..4).filter(|&e| w.edges[e] == EdgeKind::Sea).collect();
            assert_eq!(sea.len(), 2, "seed {seed}: {:?}", w.edges);
            for e in 0..4 {
                let ocean = band_share(&w, e, |i| w.ocean[i]);
                if sea.contains(&e) {
                    assert!(ocean >= 0.95, "seed {seed}: sea edge {} is {ocean:.2} ocean", EDGE_NAMES[e]);
                } else {
                    assert!(1.0 - ocean >= 0.5, "seed {seed}: land edge {} is {:.2} land", EDGE_NAMES[e], 1.0 - ocean);
                }
            }
        }
        // The seed picks which edges: not the same pair for every seed.
        let picks: std::collections::BTreeSet<_> =
            (0..16).map(|seed| format!("{:?}", edges(seed, &continental(LAYOUT_CONTINENT, seed, 1.0, 1.0).knobs))).collect();
        assert!(picks.len() > 1);
    }

    #[test]
    fn inland_has_no_sea_edges() {
        let mut p = continental(LAYOUT_INLAND, 7, 16_000.0, 128.0);
        p.knobs.sea_edges = 4.0; // inland ignores it
        let w = World::new(&p);
        assert_eq!(w.edges, [EdgeKind::Land; 4]);
        for e in 0..4 {
            let land = band_share(&w, e, |i| !w.ocean[i]);
            assert!(land >= 0.9, "inland edge {} is only {land:.2} land", EDGE_NAMES[e]);
        }
        let cells = w.cells();
        let inner_sea = (0..cells).filter(|&i| w.ocean[i] && !w.on_border(i)).count();
        assert!(inner_sea > cells / 50, "an inland sea: {inner_sea} cells");
        // Continents: several landmasses, seas between them.
        let w = World::new(&continental(LAYOUT_CONTINENTS, 7, 16_000.0, 128.0));
        let land = (0..cells).filter(|&i| !w.ocean[i]).count();
        assert!(land > cells / 4 && land < cells * 9 / 10, "continents: {land} land cells");
    }

    /// Every land cell drains to the sea or off a land edge: following
    /// receivers ends at NO_RECEIVER, and only outlets have none — for the
    /// Priority-Flood and for `route_fast`, after some erosion.
    #[test]
    fn land_edge_drains_out() {
        use crate::forge::erosion::{route, step, StepScratch, FLOOD_EPSILON, NO_RECEIVER};
        let p = continental(LAYOUT_CONTINENT, DEFAULT_SEED, 8_192.0, 64.0);
        let mut w = World::new(&p);
        assert!(w.edges.contains(&EdgeKind::Land));
        let land_outlets = (0..w.cells()).filter(|&i| w.outlet[i] && !w.ocean[i]).count();
        assert!(land_outlets > w.nx.min(w.nz), "land edges give outlets: {land_outlets}");
        let geo = crate::forge::geology::Geology::new(&p, &w);
        let rain = crate::forge::climate::discharge_weight(&crate::forge::climate::compute(&w, &p), &w);
        let mut scratch = StepScratch::default();
        for _ in 0..12 {
            step(&mut w, &p, &geo, &rain, &mut scratch);
        }
        // Land-edge outlets sit just below the land inside them after every step.
        let before = w.h.clone();
        w.follow_land_edges(EdgeFollow::Lower);
        assert_eq!(before, w.h, "land-edge outlets follow the land behind them down");
        let mut fast = crate::forge::erosion::Routing::default();
        crate::forge::routing_fast::route_fast(&w, FLOOD_EPSILON, &mut StepScratch::default(), &mut fast);
        for (name, r) in [("priority-flood", route(&w, FLOOD_EPSILON)), ("route_fast", fast)] {
            let mut off_edge = 0;
            for i in 0..w.cells() {
                assert_eq!(r.receiver[i] == NO_RECEIVER, w.outlet[i], "{name}: cell {i}");
                if w.outlet[i] {
                    continue;
                }
                let (mut c, mut steps) = (i, 0);
                while r.receiver[c] != NO_RECEIVER {
                    c = r.receiver[c] as usize;
                    steps += 1;
                    assert!(steps <= w.cells(), "{name}: cell {i} loops");
                }
                off_edge += !w.ocean[c] as usize;
            }
            assert!(off_edge > 0, "{name}: some land drains off a land edge");
        }
    }

    /// High uplift sits on convergent plate boundaries (interior ranges are
    /// the rest).
    #[test]
    fn belts_follow_plate_boundaries() {
        for seed in [DEFAULT_SEED, 3] {
            let p = continental(LAYOUT_CONTINENT, seed, 16_000.0, 64.0);
            let w = World::new(&p);
            let field = plates::plate_field(p.seed, p.width_m, p.height_m, w.nx, w.nz, &p.knobs);
            // Convergent boundary cells, then the distance to them (BFS, m).
            let mut dist = vec![f64::INFINITY; w.cells()];
            let mut queue = std::collections::VecDeque::new();
            for i in 0..w.cells() {
                let convergent = field.convergence[i] > 0.0
                    && w.neighbors(i).any(|(j, _)| field.plate[j] != field.plate[i]);
                if convergent {
                    dist[i] = 0.0;
                    queue.push_back(i);
                }
            }
            while let Some(i) = queue.pop_front() {
                for (j, d) in w.neighbors(i) {
                    if dist[j] > dist[i] + d {
                        dist[j] = dist[i] + d;
                        queue.push_back(j);
                    }
                }
            }
            let max = w.uplift.iter().copied().fold(0.0, f64::max);
            let high: Vec<usize> = (0..w.cells()).filter(|&i| w.uplift[i] > 0.5 * max).collect();
            assert!(high.len() > 100, "seed {seed}: {} high-uplift cells", high.len());
            let near = high.iter().filter(|&&i| dist[i] <= 3_000.0).count();
            let share = near as f64 / high.len() as f64;
            assert!(share >= 0.7, "seed {seed}: only {share:.2} of high uplift within 3 km of a convergent boundary");
        }
    }

    /// The island layouts are untouched by the continental ones: the same
    /// knobs give the same initial condition, and the same erosion, bit for
    /// bit (crc32 of `h` / `uplift` recorded before the continental layouts).
    #[test]
    fn island_layout_unchanged() {
        let crc = |v: &[f64]| {
            let mut c = crc32fast::Hasher::new();
            for x in v {
                c.update(&x.to_bits().to_le_bytes());
            }
            c.finalize()
        };
        for (size, cell, h, uplift) in
            [(8_192.0, 64.0, 0xAA1B_B793u32, 0xC8F9_C2E9u32), (16_000.0, 128.0, 0xD952_A152, 0x9936_283D)]
        {
            let p = continental(LAYOUT_ONE_ISLAND, DEFAULT_SEED, size, cell);
            let w = World::new(&p);
            assert_eq!(w.edges, [EdgeKind::Sea; 4]);
            assert_eq!(w.outlet, w.ocean);
            assert_eq!((crc(&w.h), crc(&w.uplift)), (h, uplift), "{size} m / {cell} m");
        }
        // The island presets' climate: one latitude in the westerlies.
        let mut p = continental(LAYOUT_ONE_ISLAND, DEFAULT_SEED, 8_192.0, 64.0);
        p.knobs.pole = crate::forge::climate::POLE_NONE as f64;
        p.knobs.latitude_center_deg = 45.0;
        p.knobs.meander_amplitude_deg = 0.0;
        let mut w = World::new(&p);
        let geo = crate::forge::geology::Geology::new(&p, &w);
        let rain = crate::forge::climate::discharge_weight(&crate::forge::climate::compute(&w, &p), &w);
        let mut scratch = crate::forge::erosion::StepScratch::default();
        for _ in 0..15 {
            crate::forge::erosion::step(&mut w, &p, &geo, &rain, &mut scratch);
        }
        // Re-recorded with the latitude climate (Task 12): precipitation now
        // comes from the air's moisture (relaxing to a continental
        // equilibrium), blurred in metres over land only.
        assert_eq!(crc(&w.h), 0x5771_874C, "eroded island");
    }
}
