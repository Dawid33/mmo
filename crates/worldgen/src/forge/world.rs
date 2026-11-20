//! The simulated world grid, run parameters, and the initial condition.

use crate::forge::fields;
use crate::forge::knobs::Knobs;
use crate::forge::par;
use crate::forge::periodic::PeriodicX;
use crate::forge::plates;
use crate::noise::OctaveNoise;

/// The quick dev world's cell (`ForgeParams::default()`: the server's
/// missing-world forge, tests, `just forge-proto` without flags). The
/// main-menu generator picks its own, finer cell.
pub const DEFAULT_CELL_M: f64 = 32.0;
pub const DEFAULT_SEED: u64 = 0x00D5_C0DE_0206;

/// World size the noise wavelengths are written for: they are sampled at
/// `span_m / REFERENCE_SIZE_M` (`span_m`), so a bigger world is the same
/// shape scaled up.
pub const REFERENCE_SIZE_M: f64 = 16_000.0;

/// The one length a `width_m` x `height_m` world's size-relative lengths
/// (noise scale, belt widths, plate spacing) are shares of: its shorter
/// side, so a share fits either way.
pub fn span_m(width_m: f64, height_m: f64) -> f64 {
    width_m.min(height_m)
}

/// Each sea edge's coast distance varies by this share of `continent_coast`.
const COAST_DISTANCE_VARIATION: f64 = 0.4;
/// Landness of a continent's interior, far from any sea: caps the mass so
/// the interior is uniform before the coast noise.
const CONTINENT_INTERIOR_MASS: f64 = 1.0;
/// Continent centres keep at least this far apart (half-width units, the
/// shorter way round) when possible; z within this half-height box.
const CONTINENT_CENTRE_BOX: f64 = 0.55;
const CONTINENT_CENTRE_GAP: f64 = 0.6;

/// What lies past one edge of the map. Stored in the world file
/// (`WorldArtifact::edges`), so its variants are part of the format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EdgeKind {
    /// Sea: an ocean margin keeps the border drowned.
    Sea,
    /// Land running off the map: forged only by the deleted layouts; kept so
    /// the stored discriminants stay (a world with one is refused at load).
    Land,
    /// The map wraps round to the opposite edge (east <-> west). Appended
    /// last so the stored discriminants of `Sea` and `Land` stay.
    Wrap,
}

/// Edge order in `World::edges` and `PLANET_EDGES`: north (z = 0), east
/// (x = max), south (z = max), west (x = 0).
pub const EDGE_NAMES: [&str; 4] = ["north", "east", "south", "west"];

/// The edges of every forged world (the Planet): polar seas north and
/// south, east and west wrap round to each other.
pub const PLANET_EDGES: [EdgeKind; 4] =
    [EdgeKind::Sea, EdgeKind::Wrap, EdgeKind::Sea, EdgeKind::Wrap];

/// Proximity of a point (half-width units, -1..1 both ways) to each edge in
/// `edges` order: 1 on the edge, -1 on the opposite one.
fn edge_proximity(rx: f64, rz: f64) -> [f64; 4] {
    [-rz, rx, rz, -rx]
}

/// The Planet's landness before coast noise: > 0 is land. The coast sits
/// `coast[e]` half-heights in from each sea edge (north and south), and
/// seas cut the land along the Voronoi boundaries of `centres` — the
/// continent centres, spread round the whole width, each with its images one
/// width (2 half-widths) east and west.
struct ContinentalMass {
    coast: [f64; 4],
    sea: [bool; 4],
    centres: Vec<(f64, f64)>,
    channel: f64,
}

impl ContinentalMass {
    fn new(seed: u64, k: &Knobs, edges: &[EdgeKind; 4]) -> ContinentalMass {
        let sea = edges.map(|e| e == EdgeKind::Sea);
        let mut coast = [0.0; 4];
        for (e, c) in coast.iter_mut().enumerate() {
            let u = hash01(seed, 0xC0A5, e as u64) - 0.5;
            *c = k.continent_coast * (1.0 + COAST_DISTANCE_VARIATION * u);
        }
        let count = k.continent_count.round().clamp(2.0, 4.0) as usize;
        // x is a circle 2 half-widths round: centres anywhere on it, kept
        // apart the shorter way round.
        let wrap = PeriodicX::for_width(2.0);
        let mut centres: Vec<(f64, f64)> = Vec::new();
        let mut attempt = 0u64;
        while centres.len() < count {
            attempt += 1;
            let x = 2.0 * hash01(seed, 0xC0A6, attempt) - 1.0;
            let z = (2.0 * hash01(seed, 0xC0A7, attempt) - 1.0) * CONTINENT_CENTRE_BOX;
            let gap = |cx: f64, cz: f64| wrap.dx(cx, x).hypot(cz - z);
            let clear = attempt > 400
                || centres
                    .iter()
                    .all(|&(cx, cz)| gap(cx, cz) >= CONTINENT_CENTRE_GAP);
            if clear {
                centres.push((x, z));
            }
        }
        let centres = wrap.with_images(&centres, |c| c.0, |c, d| (c.0 + d, c.1));
        ContinentalMass {
            coast,
            sea,
            centres,
            channel: k.continent_sea_width,
        }
    }

    fn at(&self, rx: f64, rz: f64) -> f64 {
        let prox = edge_proximity(rx, rz);
        let coastal = (0..4)
            .filter(|&e| self.sea[e])
            .map(|e| 1.0 - prox[e] - self.coast[e])
            .fold(CONTINENT_INTERIOR_MASS, f64::min);
        let (mut a, mut b) = ((f64::INFINITY, 0usize), (f64::INFINITY, 0usize));
        for (j, &(cx, cz)) in self.centres.iter().enumerate() {
            let d = (rx - cx).powi(2) + (rz - cz).powi(2);
            if d < a.0 {
                b = a;
                a = (d, j);
            } else if d < b.0 {
                b = (d, j);
            }
        }
        let (ca, cb) = (self.centres[a.1], self.centres[b.1]);
        let gap = (cb.0 - ca.0).hypot(cb.1 - ca.1).max(1.0e-9);
        let to_strait = (b.0 - a.0) / (2.0 * gap);
        coastal.min(to_strait - self.channel)
    }
}

/// The Planet's landness at any point: the mass plus the coast noises
/// (scaled with the world, periodic in x), less the ocean margin along the
/// sea edges. `World::new` and `volcano_sites` share it.
struct ContinentalShape {
    edges: [EdgeKind; 4],
    mass: ContinentalMass,
    shape_lo: OctaveNoise,
    shape_hi: OctaveNoise,
    /// The periodicity of `shape_lo` / `shape_hi`.
    wrap_lo: PeriodicX,
    wrap_hi: PeriodicX,
    /// x is folded into the width.
    wrap: PeriodicX,
    half_x: f64,
    half_z: f64,
    scale: f64,
    coast_noise: (f64, f64),
    margin: (f64, f64),
}

impl ContinentalShape {
    fn new(p: &ForgeParams) -> ContinentalShape {
        let k = &p.knobs;
        let edges = PLANET_EDGES;
        let scale = p.span_m() / REFERENCE_SIZE_M;
        let wrap = PeriodicX::for_world(p);
        ContinentalShape {
            edges,
            mass: ContinentalMass::new(p.seed, k, &edges),
            wrap_lo: wrap.for_noise(8_192.0 * scale),
            wrap_hi: wrap.for_noise(1_024.0 * scale),
            wrap,
            shape_lo: OctaveNoise::new(p.seed ^ 0xF001, -13, vec![1.0, 0.6, 0.3]), // 8 km at the reference size
            shape_hi: OctaveNoise::new(p.seed ^ 0xF002, -10, vec![1.0, 0.5]),      // 1 km
            half_x: p.width_m * 0.5,
            half_z: p.height_m * 0.5,
            scale: p.span_m() / REFERENCE_SIZE_M,
            coast_noise: (k.coast_noise_lo, k.coast_noise_hi),
            margin: (
                k.ocean_margin_start,
                (k.ocean_margin_start + OCEAN_MARGIN_RAMP).min(0.99),
            ),
        }
    }

    /// (shape, landness) at (mx, mz) m: landness > 0 is land; shape < 0 is
    /// the sea's own (not the margin's) — shelf and abyss.
    fn at(&self, mx: f64, mz: f64) -> (f64, f64) {
        let mx = self.wrap.fold(mx);
        let (rx, rz) = (
            (mx - self.half_x) / self.half_x,
            (mz - self.half_z) / self.half_z,
        );
        let scale = self.scale;
        let lo = self
            .wrap_lo
            .sample(mx, mz, |x, z| self.shape_lo.sample2(x / scale, z / scale));
        let hi = self
            .wrap_hi
            .sample(mx, mz, |x, z| self.shape_hi.sample2(x / scale, z / scale));
        let shape = self.mass.at(rx, rz) + self.coast_noise.0 * lo + self.coast_noise.1 * hi;
        let prox = edge_proximity(rx, rz);
        let sea_prox = (0..4)
            .filter(|&e| self.edges[e] == EdgeKind::Sea)
            .map(|e| prox[e])
            .fold(-1.0, f64::max);
        (
            shape,
            shape - 2.0 * smoothstep(self.margin.0, self.margin.1, sea_prox),
        )
    }
}

/// The volcano sites of a world (`fields::volcano_sites_on`): on land at
/// least `fields::VOLCANO_MIN_LANDNESS` inland. From the seed and knobs only
/// — not the cell size — so every level of the erosion schedule raises the
/// same cones, and the world file stores the same list.
pub fn volcano_sites(p: &ForgeParams) -> Vec<[f32; 2]> {
    let shape = ContinentalShape::new(p);
    fields::volcano_sites_on(p, &|mx, mz| {
        shape.at(mx, mz).1 > fields::VOLCANO_MIN_LANDNESS
    })
}

/// The ocean margin ramps from fully open to fully drowned over this share of
/// the half-height past `Knobs::ocean_margin_start` (the Planet's sea edges
/// are north and south).
const OCEAN_MARGIN_RAMP: f64 = 0.13;

/// Lowest land height (m): erosion and diffusion clamp land to it too.
pub const LAND_FLOOR_M: f64 = 0.05;

const NEIGHBORS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

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
    /// The dev Planet (`play::DEV_WORLD_SIZE`) at `DEFAULT_CELL_M`, default
    /// seed and knobs, quiet.
    fn default() -> Self {
        let size = crate::forge::play::DEV_WORLD_SIZE;
        ForgeParams {
            width_m: size.width_m,
            height_m: size.height_m,
            cell_m: DEFAULT_CELL_M,
            seed: DEFAULT_SEED,
            knobs: Knobs::default(),
            quiet: true,
        }
    }
}

impl ForgeParams {
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
    let mut z =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
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
    /// Columns 0 and `nx - 1` are neighbours (every forged world: east and
    /// west are `EdgeKind::Wrap`; false only on hand-built grids). Every neighbour walk goes through `neighbors` or
    /// `step`; the outermost columns are then ordinary cells, not border.
    pub wrap_x: bool,
    /// Rock uplift rate (m/yr).
    pub uplift: Vec<f64>,
    /// Total rock uplift applied so far (m): the rock now at height y started
    /// at y - uplifted, which is what the geology column is indexed by.
    pub uplifted: Vec<f64>,
    /// Would-be land the ocean margin drowned (a big number = land is
    /// pressing against the polar edges; the seed search penalises it).
    pub margin_drowned: usize,
    /// What lies past each edge (`edges` order: N, E, S, W).
    pub edges: [EdgeKind; 4],
    /// Cells drainage ends at: the sea (never eroded, uplifted or diffused).
    /// Every border cell (`border_cells`) is one. Routing gives outlets no
    /// receiver.
    pub outlet: Vec<bool>,
}

impl World {
    /// The Planet's initial condition: the continents' landness plus coast
    /// noise (wavelengths scaled with the world, periodic in x), an ocean
    /// margin along the polar sea edges, and uplift belts along convergent
    /// plate boundaries (`plates`), plus a cone of uplift at each volcano
    /// (`volcano_sites`).
    pub fn new(p: &ForgeParams) -> World {
        let (nx, nz) = (p.nx(), p.nz());
        let k = &p.knobs;
        let scale = p.span_m() / REFERENCE_SIZE_M;
        let rough = OctaveNoise::new(p.seed ^ 0xF004, -8, vec![1.0, 1.0, 1.0]); // 256 m
        let wrap = PeriodicX::for_world(p);
        let wrap_rough = wrap.for_noise(256.0 * scale);
        let shape_of = ContinentalShape::new(p);
        let edges = shape_of.edges;
        let field = plates::plate_field(p.seed, p.width_m, p.height_m, nx, nz, k);
        // Each range with its images east and west.
        let ranges = plates::interior_ranges(p.seed, p.width_m, p.height_m, k);
        let volcanoes = wrap.with_images(
            &volcano_sites(p),
            |s| s[0] as f64,
            |s, d| [(s[0] as f64 + d) as f32, s[1]],
        );

        let mut w = World {
            nx,
            nz,
            dx: p.cell_m,
            h: vec![0.0; nx * nz],
            ocean: vec![false; nx * nz],
            wrap_x: edges[1] == EdgeKind::Wrap,
            uplift: vec![0.0; nx * nz],
            uplifted: vec![0.0; nx * nz],
            margin_drowned: 0,
            edges,
            outlet: vec![false; nx * nz],
        };
        for z in 0..nz {
            for x in 0..nx {
                let (mx, mz) = ((x as f64 + 0.5) * p.cell_m, (z as f64 + 0.5) * p.cell_m);
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
                let rough_at =
                    wrap_rough.sample(mx, mz, |x, z| rough.sample2(x / scale, z / scale));
                w.h[i] = (1.0 + 45.0 * smoothstep(0.0, 0.5, land) + 3.0 * rough_at).max(0.5);
                let interior = plates::interior_strength(&ranges, mx, mz, p.width_m, p.height_m, k);
                let belt = (field.convergence[i] as f64).max(interior);
                let coast_taper = smoothstep(0.0, 0.05, land);
                w.uplift[i] = k.uplift
                    * coast_taper
                    * (k.lowland_uplift_share
                        + (k.belt_uplift - k.lowland_uplift_share).max(0.0)
                            * belt.max(fields::cone(&volcanoes, mx, mz)));
            }
        }
        w.outlet.clone_from(&w.ocean);
        for i in w.border_cells() {
            assert!(w.outlet[i], "every border cell must be sea (cell {i})");
        }
        w
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

    /// True for a cell on the outermost ring of the grid — on a wrapping
    /// world only the first and last rows (its columns 0 and `nx - 1` are
    /// neighbours, not an edge).
    pub fn on_border(&self, i: usize) -> bool {
        let (x, z) = self.xz(i);
        z == 0 || z + 1 == self.nz || (!self.wrap_x && (x == 0 || x + 1 == self.nx))
    }

    /// Every cell of the outermost ring (corners more than once); on a
    /// wrapping world the first and last rows only.
    pub fn border_cells(&self) -> impl Iterator<Item = usize> + use<> {
        let (nx, nz) = (self.nx, self.nz);
        let columns = if self.wrap_x { 0 } else { nz };
        (0..nx)
            .flat_map(move |x| [x, (nz - 1) * nx + x])
            .chain((0..columns).flat_map(move |z| [z * nx, z * nx + nx - 1]))
    }

    /// The cell `dx` columns east and `dz` rows south of cell `i`: x wraps
    /// round on a wrapping world, z never. `None` off the north or south
    /// edge, or off east / west when the world does not wrap. The one
    /// wrap-aware offset every neighbour walk uses.
    #[inline]
    pub fn step(&self, i: usize, dx: i32, dz: i32) -> Option<usize> {
        let (x, z) = (
            (i % self.nx) as i64 + dx as i64,
            (i / self.nx) as i64 + dz as i64,
        );
        let (w, h) = (self.nx as i64, self.nz as i64);
        let x = if self.wrap_x { x.rem_euclid(w) } else { x };
        (x >= 0 && z >= 0 && x < w && z < h).then(|| (z * w + x) as usize)
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
    /// its outlets' heights (the sea floor); other land
    /// cells take this world's state. Per cell and in
    /// fixed order: the result does not depend on the thread count.
    pub fn upsample_onto(&self, mut fine: World) -> World {
        let (ncx, ncz, nfx) = (self.nx, self.nz, fine.nx);
        // Coarse sea cells read as sea level for land, as diffusion's shore
        // does: a coast cell must not inherit the shelf's depth.
        let coarse_h: Vec<f64> = self
            .h
            .iter()
            .zip(&self.ocean)
            .map(|(&h, &sea)| if sea { 0.0 } else { h })
            .collect();
        // Cell centers matched by real position (metres), not share of the
        // grid: a level whose `size / cell` rounds (16 km at 256 m: 62.5 ->
        // 63 cells) covers a little more or less than the fine grid, and a
        // share mapping would stretch it over the fine one. Fine cells past
        // the coarse grid's centers clamp to its edge — except along a
        // wrapping x, which samples circularly (column nc - 1 beside 0).
        let ratio = fine.dx / self.dx;
        let axis = |j: usize, nc: usize| {
            let u = ((j as f64 + 0.5) * ratio - 0.5).clamp(0.0, (nc - 1) as f64);
            let i0 = (u.floor() as usize).min(nc.saturating_sub(2));
            (i0, (i0 + 1).min(nc - 1), u - i0 as f64)
        };
        let circular = |j: usize, nc: usize| {
            let u = (j as f64 + 0.5) * ratio - 0.5;
            let f = u.floor();
            let i0 = (f as i64).rem_euclid(nc as i64) as usize;
            (i0, (i0 + 1) % nc, u - f)
        };
        let wrap_x = self.wrap_x && fine.wrap_x;
        let sample = |grid: &[f64], i: usize| {
            let x_axis = if wrap_x {
                circular(i % nfx, ncx)
            } else {
                axis(i % nfx, ncx)
            };
            let ((x0, x1, tx), (z0, z1, tz)) = (x_axis, axis(i / nfx, ncz));
            let row = |z: usize| grid[z * ncx + x0] * (1.0 - tx) + grid[z * ncx + x1] * tx;
            row(z0) * (1.0 - tz) + row(z1) * tz
        };
        // Outlets (the sea) keep the fine initial condition.
        let outlet = &fine.outlet;
        let cells = fine.cells();
        let h = par::map_range(cells, |i| {
            if outlet[i] {
                fine.h[i]
            } else {
                sample(&coarse_h, i).max(LAND_FLOOR_M)
            }
        });
        let uplifted = par::map_range(cells, |i| {
            if outlet[i] {
                0.0
            } else {
                sample(&self.uplifted, i)
            }
        });
        fine.h = h;
        fine.uplifted = uplifted;
        fine
    }

    /// 8-neighbors of cell `i` with their distance in meters (x wraps round
    /// on a wrapping world).
    pub fn neighbors(&self, i: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let (x, z) = ((i % self.nx) as i32, (i / self.nx) as i32);
        let (w, h) = (self.nx as i32, self.nz as i32);
        let wrap = self.wrap_x;
        NEIGHBORS.iter().filter_map(move |&(ox, oz)| {
            let (jx, jz) = (x + ox, z + oz);
            let jx = if wrap { jx.rem_euclid(w) } else { jx };
            (jx >= 0 && jz >= 0 && jx < w && jz < h).then(|| {
                let dist = if ox != 0 && oz != 0 {
                    std::f64::consts::SQRT_2
                } else {
                    1.0
                };
                ((jz * w + jx) as usize, dist * self.dx)
            })
        })
    }

    /// Cell center in world meters.
    pub fn center(&self, i: usize) -> (f64, f64) {
        (
            ((i % self.nx) as f64 + 0.5) * self.dx,
            ((i / self.nx) as f64 + 0.5) * self.dx,
        )
    }
}
