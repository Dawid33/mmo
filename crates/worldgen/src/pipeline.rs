//! Our tuned density graph: island falloff/erosion/warped ridges ->
//! offset/factor/jaggedness -> depth -> final_density. The mechanism follows
//! Tectonic (continent noise folded through `abs`, erosion flattening, a
//! peaks-and-valleys fold of a domain-warped ridge noise for mountain ranges);
//! the numbers are hand-tuned against `just world-map`, not ported (see the
//! design spec and docs/superpowers/specs/2026-09-20-worldgen-continents-findings.md).
//! Since the island phase (plan 2026-09-25-cell-loading-phase3-island-worldgen)
//! the continent term is the island falloff of `islands.rs` — see
//! `island_terms`; the continents noise only warps coasts.

use std::sync::Arc;

use crate::density::NoiseSet;
#[cfg(test)]
use crate::density::{DensityFn, IslandValue};
use crate::islands::{
    IslandField, FULL_RELIEF_M, ISLAND_COAST_WARP, ISLAND_COAST_WAVELENGTH, ISLAND_MIN_RELIEF,
    ISLAND_REACH, ISLAND_RELIEF_FRAC,
};
use crate::noise::OctaveNoise;
use crate::spline::{FlatSpline, Spline};

pub const SEA_LEVEL: f64 = 64.0;

/// Topmost block of the water column: water fills `by <= SEA_TOP_BLOCK`, so
/// the water SURFACE is the plane `y = SEA_LEVEL` — the density field's own
/// zero-height reference, and a multiple of every LOD cell size (64 blocks =
/// 1024 voxels; `sea_surface_sits_on_every_lod_lattice`). Any-solid LOD
/// downsampling rounds a surface up to its cell's top, so a sea one block
/// higher (water through block 64, surface at 65) stood 3 and 15 blocks proud
/// in the factor-64/256 rings: a dark wall along every ring boundary at sea.
pub const SEA_TOP_BLOCK: i32 = SEA_LEVEL as i32 - 1;

// Noise indices into NoiseSet.
const N_CONTINENTS: usize = 0;
const N_EROSION: usize = 1;
const N_RIDGES: usize = 2;
const N_WARP_X: usize = 3;
const N_WARP_Z: usize = 4;
const N_JAGGED: usize = 5;

// Tuning constants (hand-picked, not ported). Re-render with `just world-map`
// (try --span 65536 and --span 12000) and re-run examples/seed_scan.rs after
// touching any of them or the splines below.
const MIN_OFFSET: f64 = -1.0;
const MAX_OFFSET: f64 = 2.0;
const HEIGHT_SCALE: f64 = 96.0; // meters of surface swing per offset unit
const DEPTH_PER_BLOCK: f64 = 1.0 / 32.0; // density falloff per meter of height

/// Landmass size relative to Tectonic's. At 1.0 the continent noise's longest
/// wavelength is 1024 / 0.13 ≈ 7.9 km and landmasses come out 5-10 km across —
/// big islands, with a 2 km LOD horizon. 3.0 gives 15-25 km continents with
/// room for mountain belts inland. Scales erosion with it, so plains and
/// highlands stay continent-sized features.
const CONTINENT_SCALE: f64 = 3.0;
/// Horizontal size of every macro feature — continents, erosion basins,
/// mountain ranges and the ridge warp — as a wavelength multiplier: 0.5 packs
/// the same landscape into half the distance (heights unchanged, so slopes
/// steepen ×2). Tuned against the region size: at 2048-unit (128 m) regions
/// the 1.0 world read as too spread out. Block-scale jaggedness is local
/// detail and does not follow it.
const FEATURE_SCALE: f64 = 0.5;
/// Offset units the mountain term adds at full strength (deep inland, lowest
/// erosion, on a ridge line): × HEIGHT_SCALE = 168 m on top of the base
/// plateau, for peaks around SEA_LEVEL + 190 m. The world band tops out at
/// y = 320, so ~2.3 is the ceiling before MAX_OFFSET flattens summits.
const MOUNTAIN_OFFSET: f64 = 1.75;
/// How far (meters) the ridge noise's sample point is pushed around by the
/// warp noises. This is what bends round ridge loops into long sinuous ranges;
/// 0 gives ring-shaped mountains, much more than the ridge wavelength (~1.2 km)
/// smears them into marble.
const RIDGE_WARP_M: f64 = 1400.0 * FEATURE_SCALE;
/// Share of the base (non-mountain) elevation that survives maximum erosion:
/// `base * (EROSION_BASE_FLOOR + (1 - EROSION_BASE_FLOOR) * erosion_gate)`.
const EROSION_BASE_FLOOR: f64 = 0.55;
const EROSION_BASE_SWING: f64 = 1.0 - EROSION_BASE_FLOOR;
/// Peaks-and-valleys fold of |ridges|, after vanilla's but asymmetric:
/// `pv = min(PV_RISE * |r| - 1, PV_BACK_TOP - PV_BACK * |r|)`. pv = -1 (a
/// valley floor) at ridges = 0, climbing to pv = 1 (a crest) along the contour
/// |ridges| = PV_CREST — contour LINES of the noise become ridge lines, with
/// valleys between them. Past the crest vanilla falls back at the same rate,
/// which sinks a crater into every wide ridge; the gentle PV_BACK slope leaves
/// a high massif behind the crest line instead.
const PV_CREST: f64 = 0.6666667;
const PV_RISE: f64 = 2.0 / PV_CREST; // -1 at 0 -> 1 at the crest
const PV_BACK: f64 = 0.8;
const PV_BACK_TOP: f64 = 1.0 + PV_BACK * PV_CREST;

// Noise-sampling offset (meters), applied to (x, z) before every noise read.
// The world origin must NOT sit on the noise lattice zero: Perlin noise is 0
// at lattice points for every seed, which pinned spawn (region (0,0)) to the
// seed-independent "noise = 0" terrain — mid-ocean. Any large non-special
// values work; they just have to be applied consistently.
const SAMPLE_OFFSET_X_M: f64 = 100_000.0;
const SAMPLE_OFFSET_Z_M: f64 = 50_000.0;

// Vertical density ramp (the `neg_y` term): carries `-y` across the world band,
// clamped at the ends. Used by BOTH the density graph node and ColumnProfile so
// they stay bit-identical.
const RAMP_FROM_Y: f64 = -64.0;
const RAMP_TO_Y: f64 = 320.0;
const RAMP_FROM_V: f64 = 64.0;
const RAMP_TO_V: f64 = -320.0;

// Noise xz scales (used by both the graph builders and the fused path).
// Wavelength of a noise's first octave = 2^-first_octave / xz_scale meters.
// Wavelengths quoted at FEATURE_SCALE = 1; multiply by it for the real ones.
const XZ_SCALE_EROSION: f64 = 0.25 / (CONTINENT_SCALE * FEATURE_SCALE); // ~12.3 km
const XZ_SCALE_RIDGES: f64 = 0.22 / FEATURE_SCALE; // ~1.2 km between parallel ranges
const XZ_SCALE_WARP: f64 = 0.35 / FEATURE_SCALE; // ~1.5 km
const XZ_SCALE_JAGGED: f64 = 1.0; // 64 m and 32 m

/// Wavelength (m) of an `OctaveNoise`'s base octave per unit of input scale
/// (the continents noise: 0.0867 scale ↔ ~23.6 km).
const NOISE_UNIT_WAVELENGTH_M: f64 = 2048.0;
/// The continents curve's coastline (raw continents = 0).
const COAST_C: f64 = 0.55;
/// Island centre's pseudo-continentalness, chosen so the linear falloff hits
/// deep sea (c = 0) at exactly `ISLAND_REACH` warped radii.
const ISLAND_CORE_C: f64 = COAST_C * ISLAND_REACH / (ISLAND_REACH - 1.0);

/// The island layer at SAMPLE coordinates `(sx, sz)` (world + sample offset):
/// `(c, k)` — pseudo-|continentalness| for the continents curve (0 = deep
/// sea, `COAST_C` = the coast, `ISLAND_CORE_C` = the centre) and the land
/// relief multiplier. Outside any island: deep sea, full relief. The coast
/// radius is warped by the continents noise at a wavelength proportional to
/// the island, so every size gets a comparably ragged coast. Shared by
/// `column()` and the `DensityFn::Island` oracle node, so both paths are one
/// function (bit-exact by construction).
pub(crate) fn island_terms(field: &IslandField, noises: &NoiseSet, sx: f64, sz: f64) -> (f64, f64) {
    let (x, z) = (sx - SAMPLE_OFFSET_X_M, sz - SAMPLE_OFFSET_Z_M);
    let Some(island) = field.island_at(x, z) else { return (0.0, 1.0) };
    let scale = NOISE_UNIT_WAVELENGTH_M / (ISLAND_COAST_WAVELENGTH * island.radius);
    let coast = noises.0[N_CONTINENTS].sample2(x * scale, z * scale).clamp(-1.0, 1.0);
    let radius = island.radius * (1.0 + ISLAND_COAST_WARP * coast);
    let (dx, dz) = (x - island.centre[0], z - island.centre[1]);
    let d = (dx * dx + dz * dz).sqrt() / radius;
    let c = (ISLAND_CORE_C - (ISLAND_CORE_C - COAST_C) * d).max(0.0);
    let k = (ISLAND_RELIEF_FRAC * island.radius / FULL_RELIEF_M).clamp(ISLAND_MIN_RELIEF, 1.0);
    (c, k)
}

// Graph-tree builders below (`continentalness`/`erosion`/`ridges` through
// `final_density_graph`) are the test oracle only: `TerrainPipeline::column`/
// `final_density` no longer walk the `DensityFn` tree at runtime (see the
// fused evaluation in `impl TerrainPipeline`), so these are `#[cfg(test)]`
// and exist to keep `fused_column_matches_density_graphs_bit_exact` (and
// `column_profile_matches_graph_bit_exact`) honest. They also stay as the
// spec for future graph features (caves, continent/island `RangeChoice` —
// see the roadmap note on `DensityFn::Min`/`Max`/`RangeChoice` in density.rs).
/// Island pseudo-|continentalness| (the continents noise now only warps
/// coasts, inside `island_terms`).
#[cfg(test)]
fn island_continent(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::Island { field: f.clone(), value: IslandValue::Continent }
}
#[cfg(test)]
fn island_relief(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::Island { field: f.clone(), value: IslandValue::Relief }
}
#[cfg(test)]
fn erosion() -> DensityFn {
    DensityFn::Noise { idx: N_EROSION, xz_scale: XZ_SCALE_EROSION }
}
/// Ridge noise, domain-warped by two independent low-frequency noises.
#[cfg(test)]
fn ridges() -> DensityFn {
    let shift = |idx| {
        Box::new(DensityFn::mul(
            DensityFn::Noise { idx, xz_scale: XZ_SCALE_WARP },
            DensityFn::Const(RIDGE_WARP_M),
        ))
    };
    DensityFn::WarpedNoise {
        idx: N_RIDGES,
        xz_scale: XZ_SCALE_RIDGES,
        shift_x: shift(N_WARP_X),
        shift_z: shift(N_WARP_Z),
    }
}
#[cfg(test)]
fn jagged() -> DensityFn {
    DensityFn::Noise { idx: N_JAGGED, xz_scale: XZ_SCALE_JAGGED }
}

fn curve(points: &[(f64, f64, f64)]) -> Spline {
    Spline::Curve {
        points: points
            .iter()
            .map(|&(location, value, derivative)| Spline::point(location, Spline::constant(value), derivative))
            .collect(),
    }
}

/// |continentalness| -> raw continents: -1 deep ocean, 0 the coastline, 1+ deep
/// interior. Laid out on the noise's measured distribution (sd ~0.5, so |c| has
/// median ~0.37 and p90 ~0.9): the coast at |c| = 0.55 puts ~30% of the world
/// above water. Move that point to trade land for ocean.
fn continents_curve_spline() -> Spline {
    curve(&[
        (0.0, -1.0, 0.0),
        (0.35, -0.5, 1.6),
        (0.55, 0.0, 2.2),
        (0.9, 0.5, 1.2),
        (1.5, 1.0, 0.6),
        (2.2, 1.2, 0.0),
    ])
}

/// raw continents -> base elevation in offset units (× HEIGHT_SCALE meters):
/// abyss, shelf, a drop-off just offshore, low coastal plains, then a slow
/// climb to an interior plateau at ~+58 m. Mountains are added on top, not here.
///
/// The drop-off (-0.03 -> 0) is there for the LOD rings: any-solid
/// downsampling rounds the sea floor up a cell, so water shallower than a cell
/// (16 m at factor 256) reads as sand flats from afar. raw moves ~0.0002 per
/// meter, so without it that band was ~1 km wide; with it, ~150 m.
fn offset_base_spline() -> Spline {
    curve(&[
        (-1.0, -0.62, 0.0),
        (-0.5, -0.45, 0.3),
        (-0.12, -0.34, 0.5),
        (-0.03, -0.27, 1.5),
        (0.0, 0.015, 0.9),
        (0.12, 0.07, 0.3),
        (0.5, 0.22, 0.4),
        (1.0, 0.5, 0.5),
        (1.2, 0.6, 0.0),
    ])
}

/// raw continents -> mountain strength: none within ~a kilometer of the
/// coast, full strength deep inland.
fn inland_gate_spline() -> Spline {
    curve(&[(0.1, 0.0, 0.0), (0.35, 0.35, 1.8), (0.7, 1.0, 0.0)])
}

/// erosion -> how much relief survives: 1 in low-erosion highlands, down to a
/// floor in heavily eroded lowlands. Scales mountains fully and the base
/// partly. The floor is what keeps inland lowlands rolling (ridge lines ~20 m
/// high) instead of dead flat.
fn erosion_gate_spline() -> Spline {
    curve(&[(-0.9, 1.0, 0.0), (-0.35, 0.75, -0.8), (0.1, 0.25, -0.8), (0.6, 0.12, 0.0)])
}

/// peaks-and-valleys -> height share: flat valley floors (slope 0 at pv = -1),
/// broad shoulders, sharp crests (slope stays positive into pv = 1).
fn peaks_spline() -> Spline {
    curve(&[(-1.0, 0.0, 0.0), (-0.3, 0.12, 0.4), (0.4, 0.55, 0.9), (1.0, 1.0, 0.6)])
}

fn factor_spline() -> Spline {
    curve(&[(-1.0, 0.6, 0.0), (0.0, 1.2, 0.0), (1.0, 3.0, 0.0)])
}

fn jaggedness_gate_spline() -> Spline {
    curve(&[(0.0, 0.0, 0.0), (1.0, 0.6, 0.0)])
}

/// Island falloff through the continents curve: -1 deep sea between islands,
/// 0 at the coast, rising to the island centre.
#[cfg(test)]
fn raw_continents(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::spline_of(island_continent(f), continents_curve_spline())
}

#[cfg(test)]
fn erosion_gate() -> DensityFn {
    DensityFn::spline_of(erosion(), erosion_gate_spline())
}

/// `min(PV_RISE * |r| - 1, PV_BACK_TOP - PV_BACK * |r|)`, see PV_CREST.
#[cfg(test)]
fn peaks_and_valleys() -> DensityFn {
    DensityFn::min(
        DensityFn::add(
            DensityFn::mul(DensityFn::Const(PV_RISE), DensityFn::abs(ridges())),
            DensityFn::Const(-1.0),
        ),
        DensityFn::add(
            DensityFn::Const(PV_BACK_TOP),
            DensityFn::mul(DensityFn::Const(-PV_BACK), DensityFn::abs(ridges())),
        ),
    )
}

/// Mountain strength in 0..=1: inland AND low erosion AND on a ridge line.
#[cfg(test)]
fn mountains(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::mul(
        DensityFn::mul(DensityFn::spline_of(raw_continents(f), inland_gate_spline()), erosion_gate()),
        DensityFn::spline_of(DensityFn::clamp(peaks_and_valleys(), -1.0, 1.0), peaks_spline()),
    )
}

/// Surface height offset (in offset units): the continent's base elevation,
/// partly flattened by erosion, plus the mountain ranges.
#[cfg(test)]
fn offset(f: &Arc<IslandField>) -> DensityFn {
    // b = base(raw) * (floor + swing * egate)
    let b = || {
        DensityFn::mul(
            DensityFn::spline_of(raw_continents(f), offset_base_spline()),
            DensityFn::add(
                DensityFn::Const(EROSION_BASE_FLOOR),
                DensityFn::mul(DensityFn::Const(EROSION_BASE_SWING), erosion_gate()),
            ),
        )
    };
    // offset = clamp((max(b, 0) * k + min(b, 0)) + (mountains * MOUNTAIN_OFFSET) * k):
    // island relief k scales land only, never the sea floor.
    DensityFn::clamp(
        DensityFn::add(
            DensityFn::add(
                DensityFn::mul(DensityFn::max(b(), DensityFn::Const(0.0)), island_relief(f)),
                DensityFn::min(b(), DensityFn::Const(0.0)),
            ),
            DensityFn::mul(
                DensityFn::mul(mountains(f), DensityFn::Const(MOUNTAIN_OFFSET)),
                island_relief(f),
            ),
        ),
        MIN_OFFSET,
        MAX_OFFSET,
    )
}

/// Vertical stretch factor (always positive): tall in low-erosion inland.
#[cfg(test)]
fn factor(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::spline_of(offset(f), factor_spline())
}

/// Jaggedness: short-wavelength surface roughness, nonzero mainly at high
/// offset — a few meters on mountain flanks, centimeters on the plains.
#[cfg(test)]
fn jaggedness(f: &Arc<IslandField>) -> DensityFn {
    DensityFn::mul(
        DensityFn::spline_of(offset(f), jaggedness_gate_spline()),
        DensityFn::abs(jagged()),
    )
}

/// final_density = (surface_height - y) * DEPTH_PER_BLOCK * factor + jaggedness.
/// `surface_height = SEA_LEVEL + offset*HEIGHT_SCALE` (meters); the value is
/// positive below the surface, negative above. `neg_y` is a wide linear ramp
/// carrying `-y` across the whole world band.
#[cfg(test)]
fn final_density_graph(f: &Arc<IslandField>) -> DensityFn {
    let neg_y = DensityFn::YGradient {
        from_y: RAMP_FROM_Y,
        to_y: RAMP_TO_Y,
        from_v: RAMP_FROM_V,
        to_v: RAMP_TO_V,
    };
    let surface = DensityFn::add(
        DensityFn::mul(offset(f), DensityFn::Const(HEIGHT_SCALE)),
        DensityFn::add(DensityFn::Const(SEA_LEVEL), neg_y),
    );
    DensityFn::add(
        DensityFn::mul(
            DensityFn::mul(surface, DensityFn::Const(DEPTH_PER_BLOCK)),
            factor(f),
        ),
        jaggedness(f),
    )
}

/// Noise construction shared by `TerrainPipeline::new` and the oracle test.
/// Per-noise sub-seeds derived from the world seed. The continents/erosion/
/// ridges octave tables are Minecraft's (`OctaveNoise` weighs them the way
/// Minecraft does); the warp and jagged noises are our own.
fn pipeline_noises(seed: u64) -> NoiseSet {
    NoiseSet(vec![
        OctaveNoise::new(seed ^ 0x00C0, -10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
        OctaveNoise::new(seed ^ 0x00E0, -10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
        OctaveNoise::new(seed ^ 0x00D0, -8, vec![1.0, 2.0, 1.0]),
        OctaveNoise::new(seed ^ 0x00A1, -9, vec![1.0, 0.5]),
        OctaveNoise::new(seed ^ 0x00A2, -9, vec![1.0, 0.5]),
        OctaveNoise::new(seed ^ 0x00A3, -6, vec![1.0, 1.0]),
    ])
}

pub struct TerrainPipeline {
    noises: NoiseSet,
    islands: Arc<IslandField>,
    continents_curve: FlatSpline,
    offset_base: FlatSpline,
    inland_gate: FlatSpline,
    erosion_gate: FlatSpline,
    peaks: FlatSpline,
    factor_curve: FlatSpline,
    jaggedness_gate: FlatSpline,
}

impl TerrainPipeline {
    pub fn new(seed: u64) -> Self {
        Self::with_islands(seed, IslandField::new(seed))
    }

    /// A pipeline with an explicit island layout (tests, tools: other spawn
    /// island sizes than the build dial's).
    pub fn with_islands(seed: u64, islands: IslandField) -> Self {
        Self {
            noises: pipeline_noises(seed),
            islands: Arc::new(islands),
            continents_curve: FlatSpline::from_spline(&continents_curve_spline()),
            offset_base: FlatSpline::from_spline(&offset_base_spline()),
            inland_gate: FlatSpline::from_spline(&inland_gate_spline()),
            erosion_gate: FlatSpline::from_spline(&erosion_gate_spline()),
            peaks: FlatSpline::from_spline(&peaks_spline()),
            factor_curve: FlatSpline::from_spline(&factor_spline()),
            jaggedness_gate: FlatSpline::from_spline(&jaggedness_gate_spline()),
        }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        // ColumnProfile::density_at reproduces final_density_graph bit-for-bit,
        // so the 3D entry point is just the 2D profile plus the cheap y
        // arithmetic. Pinned against the graph by
        // `column_profile_matches_graph_bit_exact` (profile vs graph) and
        // `fused_column_matches_density_graphs_bit_exact` (this fn vs graph).
        self.column(x, z).density_at(y)
    }

    /// Fused evaluation of the offset/factor/jaggedness graphs: each noise is
    /// sampled ONCE (the graphs share the whole `offset` subtree, which the
    /// tree-walk evaluator recomputes many times over). Every f64 op below
    /// mirrors the graph's op order exactly — guarded bit-for-bit by
    /// `fused_column_matches_density_graphs_bit_exact`.
    pub fn column(&self, x: f64, z: f64) -> ColumnProfile {
        let (x, z) = (x + SAMPLE_OFFSET_X_M, z + SAMPLE_OFFSET_Z_M);
        let n = &self.noises.0;
        let (c, k) = island_terms(&self.islands, &self.noises, x, z);
        let ero = n[N_EROSION].sample2(x * XZ_SCALE_EROSION, z * XZ_SCALE_EROSION);
        // WarpedNoise: shift = Mul(Noise, Const(RIDGE_WARP_M)), sampled at (x + shift) * scale.
        let wx = x + n[N_WARP_X].sample2(x * XZ_SCALE_WARP, z * XZ_SCALE_WARP) * RIDGE_WARP_M;
        let wz = z + n[N_WARP_Z].sample2(x * XZ_SCALE_WARP, z * XZ_SCALE_WARP) * RIDGE_WARP_M;
        let ridge = n[N_RIDGES].sample2(wx * XZ_SCALE_RIDGES, wz * XZ_SCALE_RIDGES);
        let jag = n[N_JAGGED].sample2(x * XZ_SCALE_JAGGED, z * XZ_SCALE_JAGGED);

        let raw = self.continents_curve.eval(c);
        let egate = self.erosion_gate.eval(ero);
        // pv = Min(Add(Mul(RISE, |r|), -1), Add(BACK_TOP, Mul(-BACK, |r|)))
        let pv = (PV_RISE * ridge.abs() + -1.0).min(PV_BACK_TOP + -PV_BACK * ridge.abs());
        let mountains = (self.inland_gate.eval(raw) * egate) * self.peaks.eval(pv.clamp(-1.0, 1.0));
        // offset = clamp((max(b, 0) * k + min(b, 0)) + (mountains * MOUNTAIN_OFFSET) * k)
        let b = self.offset_base.eval(raw) * (EROSION_BASE_FLOOR + EROSION_BASE_SWING * egate);
        let offset = ((b.max(0.0) * k + b.min(0.0)) + (mountains * MOUNTAIN_OFFSET) * k)
            .clamp(MIN_OFFSET, MAX_OFFSET);
        let factor = self.factor_curve.eval(offset);
        let jaggedness = self.jaggedness_gate.eval(offset) * jag.abs();

        ColumnProfile { offset_h: offset * HEIGHT_SCALE, factor, jaggedness, unbounded: false }
    }
}

/// Lattice spacing (blocks == meters) for per-region profile interpolation.
/// The height field's shortest xz wavelength is ~290 m (ridges top octave
/// 2^-6 * 0.22), though the peaks-and-valleys fold creases it along crests;
/// the jagged noise's shortest is 32 m, so 4 m sampling is 4x past ITS
/// Nyquist — roughness, not shape, is what a coarser lattice would lose.
/// Raise for speed, lower toward 1 for fidelity; must divide the region block
/// span (64).
pub const PROFILE_LATTICE_BLOCKS: usize = 4;

/// `column()` sampled on a K-meter corner lattice, bilinearly interpolated.
/// Corners are pure functions of world coordinates, so grids built by
/// neighboring regions agree bit-for-bit along shared edges (no seams).
pub struct ProfileGrid {
    origin_x: f64,
    origin_z: f64,
    step: f64,
    n: usize, // corners per axis
    offset_h: Vec<f64>,
    factor: Vec<f64>,
    jaggedness: Vec<f64>,
    /// Whether the corners are unbounded (forged) profiles; carried to every
    /// interpolated profile.
    unbounded: bool,
}

impl TerrainPipeline {
    /// Sample column() on a (span/K + 1)^2 corner lattice starting at world
    /// meters (origin_x, origin_z), K = PROFILE_LATTICE_BLOCKS meters.
    pub fn profile_grid(&self, origin_x: f64, origin_z: f64, span_m: f64) -> ProfileGrid {
        let step = PROFILE_LATTICE_BLOCKS as f64;
        let cells = (span_m / step).ceil() as usize;
        let n = cells + 1;
        let mut offset_h = Vec::with_capacity(n * n);
        let mut factor = Vec::with_capacity(n * n);
        let mut jaggedness = Vec::with_capacity(n * n);
        for i in 0..n {
            for j in 0..n {
                let prof =
                    self.column(origin_x + i as f64 * step, origin_z + j as f64 * step);
                offset_h.push(prof.offset_h);
                factor.push(prof.factor);
                jaggedness.push(prof.jaggedness);
            }
        }
        ProfileGrid { origin_x, origin_z, step, n, offset_h, factor, jaggedness, unbounded: false }
    }
}

impl ProfileGrid {
    /// The same corner lattice as `TerrainPipeline::profile_grid`, filled from
    /// any per-point profile function (forged worlds).
    pub fn from_fn(
        origin_x: f64,
        origin_z: f64,
        span_m: f64,
        profile: impl Fn(f64, f64) -> ColumnProfile,
    ) -> ProfileGrid {
        let step = PROFILE_LATTICE_BLOCKS as f64;
        let n = (span_m / step).ceil() as usize + 1;
        let mut offset_h = Vec::with_capacity(n * n);
        let mut factor = Vec::with_capacity(n * n);
        let mut jaggedness = Vec::with_capacity(n * n);
        let mut unbounded = None;
        for i in 0..n {
            for j in 0..n {
                let prof = profile(origin_x + i as f64 * step, origin_z + j as f64 * step);
                offset_h.push(prof.offset_h);
                factor.push(prof.factor);
                jaggedness.push(prof.jaggedness);
                assert!(
                    *unbounded.get_or_insert(prof.unbounded) == prof.unbounded,
                    "a profile grid mixes bounded and unbounded profiles"
                );
            }
        }
        let unbounded = unbounded.unwrap_or(false);
        ProfileGrid { origin_x, origin_z, step, n, offset_h, factor, jaggedness, unbounded }
    }

    /// Bilinear profile at world meters (x, z); must lie within the sampled span.
    pub fn profile_at(&self, x: f64, z: f64) -> ColumnProfile {
        let fx = (x - self.origin_x) / self.step;
        let fz = (z - self.origin_z) / self.step;
        // Clamp so queries exactly on the far edge stay in the last cell.
        let i = (fx.floor() as usize).min(self.n - 2);
        let j = (fz.floor() as usize).min(self.n - 2);
        let (tx, tz) = (fx - i as f64, fz - j as f64);
        let idx = |i: usize, j: usize| i * self.n + j;
        let (a, b, c, d) = (idx(i, j), idx(i + 1, j), idx(i, j + 1), idx(i + 1, j + 1));
        // Fixed op order: lerp z of (lerp x) — deterministic across machines.
        // t==0.0/t==1.0 are special-cased: the far-edge clamp above (`.min(n - 2)`)
        // makes t exactly 1.0 for queries on the grid's max corner, and
        // `a + 1.0 * (b - a)` is not guaranteed bit-identical to `b` under IEEE754
        // (float add/mul aren't associative) — without this, corner queries on the
        // high edge would fail bit-exactness against direct column() calls.
        fn lerp(t: f64, a: f64, b: f64) -> f64 {
            if t == 0.0 {
                a
            } else if t == 1.0 {
                b
            } else {
                a + t * (b - a)
            }
        }
        let bilerp = |f: &[f64]| lerp(tz, lerp(tx, f[a], f[b]), lerp(tx, f[c], f[d]));
        ColumnProfile {
            offset_h: bilerp(&self.offset_h),
            factor: bilerp(&self.factor),
            jaggedness: bilerp(&self.jaggedness),
            unbounded: self.unbounded,
        }
    }
}

/// The 2D (x,z-only) part of the density field for one column, precomputed so a
/// whole vertical column can be evaluated with cheap arithmetic. `density_at(y)`
/// reproduces `TerrainPipeline::final_density(x, y, z)` bit-for-bit.
pub struct ColumnProfile {
    offset_h: f64, // offset value * HEIGHT_SCALE
    factor: f64,
    jaggedness: f64,
    /// A forged profile: no density ramp, so ground stands at any height
    /// (`density_at` uses `neg_y = -y` everywhere). Procedural profiles are
    /// bounded: the ramp clamps above `RAMP_TO_Y` / below `RAMP_FROM_Y`,
    /// which is what keeps the procedural world bit-identical.
    pub(crate) unbounded: bool,
}

impl ColumnProfile {
    /// A profile whose surface is exactly `surface_y` (world meters), with no
    /// jaggedness: what a forged world (`forged.rs`) hands the generator, which
    /// knows the height outright instead of deriving it from noise.
    pub fn from_surface(surface_y: f64) -> Self {
        ColumnProfile { offset_h: surface_y - SEA_LEVEL, factor: 1.0, jaggedness: 0.0, unbounded: true }
    }

    /// Whether this is an unbounded (forged) profile — see the field.
    pub fn is_unbounded(&self) -> bool {
        self.unbounded
    }

    /// Height where the density crosses zero, valid inside the ramp band
    /// (RAMP_FROM_Y..RAMP_TO_Y), where `neg_y` is (up to rounding) `-y` —
    /// and everywhere for an unbounded profile.
    pub fn surface_y(&self) -> f64 {
        SEA_LEVEL + self.offset_h + self.jaggedness / (DEPTH_PER_BLOCK * self.factor)
    }

    pub fn density_at(&self, y: f64) -> f64 {
        // Must match final_density_graph's arithmetic EXACTLY, including float
        // grouping/associativity (f64 add/mul are not associative). The graph does:
        //   surface  = (offset*HEIGHT_SCALE) + (SEA_LEVEL + neg_y)
        //   density  = ((surface * DEPTH_PER_BLOCK) * factor) + jaggedness
        let neg_y = if self.unbounded {
            -y
        } else {
            let t = ((y - RAMP_FROM_Y) / (RAMP_TO_Y - RAMP_FROM_Y)).clamp(0.0, 1.0);
            RAMP_FROM_V + t * (RAMP_TO_V - RAMP_FROM_V)
        };
        let surface = self.offset_h + (SEA_LEVEL + neg_y);
        (surface * DEPTH_PER_BLOCK) * self.factor + self.jaggedness
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let p = TerrainPipeline::new(12345);
        assert_eq!(p.final_density(10.0, 64.0, 20.0), p.final_density(10.0, 64.0, 20.0));
    }

    #[test]
    fn density_decreases_with_height() {
        // Same column: deep underground is denser (more solid) than the sky.
        let p = TerrainPipeline::new(1);
        let low = p.final_density(0.0, 0.0, 0.0);
        let high = p.final_density(0.0, 300.0, 0.0);
        assert!(low > high, "low={low} high={high}");
    }

    /// The shape of the world over ~190 km (several root slots): islands in
    /// open sea — a small land share, and mountains only on the big islands.
    #[test]
    fn world_is_islands_in_open_sea() {
        for seed in [7, worldgen_test_seed()] {
            let p = TerrainPipeline::new(seed);
            let (mut land, mut mountain, mut total) = (0, 0, 0);
            let step = 3137.0;
            for i in 0..60 {
                for j in 0..60 {
                    let prof = p.column(i as f64 * step - 90_000.0, j as f64 * step - 90_000.0);
                    land += (prof.density_at(SEA_LEVEL + 2.0) > 0.0) as usize;
                    mountain += (prof.density_at(SEA_LEVEL + 100.0) > 0.0) as usize;
                    total += 1;
                }
            }
            let share = land as f64 / total as f64;
            assert!((0.01..0.2).contains(&share), "seed {seed}: land share {share:.3}");
            assert!(mountain * 4 <= land, "seed {seed}: {mountain}/{land} — mountains everywhere");
        }
    }

    #[test]
    fn the_spawn_island_is_land_ringed_by_sea() {
        for cells in [1, 4, 16, 64] {
            let field = IslandField::with_spawn_cells(crate::WORLD_SEED, cells);
            let island = field.spawn_island();
            let p = TerrainPipeline::with_islands(crate::WORLD_SEED, field);
            let [cx, cz] = island.centre;
            assert!(p.column(cx, cz).density_at(SEA_LEVEL + 0.5) > 0.0, "{cells} cells: centre is land");
            for a in 0..16 {
                let t = a as f64 * std::f64::consts::TAU / 16.0;
                let r = island.reach() * 1.05;
                let (x, z) = (cx + r * t.cos(), cz + r * t.sin());
                assert!(p.column(x, z).density_at(SEA_LEVEL - 0.5) < 0.0, "{cells} cells: sea at ({x}, {z})");
            }
        }
    }

    #[test]
    fn small_islands_have_small_relief() {
        let field = IslandField::with_spawn_cells(crate::WORLD_SEED, 4); // 256 m
        let island = field.spawn_island();
        let p = TerrainPipeline::with_islands(crate::WORLD_SEED, field);
        let mut top: f64 = f64::MIN;
        for i in -20..=20 {
            for j in -20..=20 {
                let (x, z) = (island.centre[0] + i as f64 * 8.0, island.centre[1] + j as f64 * 8.0);
                top = top.max(p.column(x, z).offset_h);
            }
        }
        assert!(top < 40.0, "a 256 m island rises {top:.1} m above sea level");
    }

    #[test]
    fn terrain_is_continuous_across_slot_edges() {
        let field = IslandField::new(worldgen_test_seed());
        let p = TerrainPipeline::new(worldgen_test_seed());
        let mut edges = 0;
        for i in 0..200 {
            let (x, z) = (i as f64 * 977.0 - 97_000.0, i as f64 * 613.0 + 41_000.0);
            let Some(island) = field.island_at(x, z) else { continue };
            let ex = island.slot_min[0];
            for k in 0..10 {
                let ez = island.slot_min[1] + (k as f64 + 0.5) * island.slot_size / 10.0;
                let (a, b) = (p.column(ex - 0.01, ez), p.column(ex + 0.01, ez));
                assert!((a.offset_h - b.offset_h).abs() < 0.05, "step at slot edge x={ex} z={ez}");
                edges += 1;
            }
        }
        assert!(edges > 100);
    }

    #[test]
    fn legacy_spawn_column_is_land() {
        // Legacy (non-cell) regions spawn at the constant column; the default
        // spawn island must cover it.
        let p = TerrainPipeline::new(crate::WORLD_SEED);
        let blocks = |units: f32| units as f64 / game::BLOCK_VOXELS as f64;
        let (x, z) = (blocks(game::PLAYER_SPAWN_X), blocks(game::PLAYER_SPAWN_Z));
        assert!(p.column(x, z).density_at(SEA_LEVEL + 0.5) > 0.0);
    }

    /// Oracle fixtures: a 256 m spawn island, so the sample grids below cross
    /// its coast into the sea.
    fn oracle(seed: u64) -> (TerrainPipeline, Arc<IslandField>) {
        let field = IslandField::with_spawn_cells(seed, 4);
        (TerrainPipeline::with_islands(seed, field.clone()), Arc::new(field))
    }

    #[test]
    fn column_profile_matches_graph_bit_exact() {
        // Must compare against the DensityFn graph, NEVER against
        // p.final_density(): final_density() is itself column()-backed since
        // the fusion, so that comparison would be X == X (tautological).
        let (p, field) = oracle(999);
        let noises = pipeline_noises(999);
        let graph = final_density_graph(&field);
        for i in 0..20 {
            for j in 0..20 {
                let x = i as f64 * 13.0;
                let z = j as f64 * 17.0;
                let prof = p.column(x, z);
                for k in 0..12 {
                    let y = -64.0 + k as f64 * 34.0; // spans the world band incl. ends
                    assert_eq!(
                        prof.density_at(y),
                        graph.eval(x + SAMPLE_OFFSET_X_M, y, z + SAMPLE_OFFSET_Z_M, &noises),
                        "profile/graph mismatch at ({x}, {y}, {z})"
                    );
                }
            }
        }
    }

    fn worldgen_test_seed() -> u64 { 0x00D5_C0DE_8D78 } // test helper, next to the test

    fn splitmix_for_test(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[test]
    fn profile_grid_exact_at_corners_and_close_between() {
        let p = TerrainPipeline::new(worldgen_test_seed());
        let grid = p.profile_grid(-32.0, 96.0, 64.0);
        let k = PROFILE_LATTICE_BLOCKS as f64;
        // Corners: bit-exact vs direct column().
        for i in 0..=(64.0 / k) as usize {
            for j in 0..=(64.0 / k) as usize {
                let (x, z) = (-32.0 + i as f64 * k, 96.0 + j as f64 * k);
                let (g, e) = (grid.profile_at(x, z), p.column(x, z));
                assert_eq!(g.offset_h, e.offset_h);
                assert_eq!(g.factor, e.factor);
                assert_eq!(g.jaggedness, e.jaggedness);
            }
        }
        // Interior: interpolation error tiny relative to terrain scale. The field's
        // shortest wavelength is ~123 m, so a 4 m bilinear cell is ~30x past
        // Nyquist; 0.5 m of height (offset_h) is a generous ceiling.
        let mut s = 3u64;
        let mut worst: f64 = 0.0;
        for _ in 0..500 {
            let x = -32.0 + ((splitmix_for_test(&mut s) % 6400) as f64) / 100.0;
            let z = 96.0 + ((splitmix_for_test(&mut s) % 6400) as f64) / 100.0;
            worst = worst.max((grid.profile_at(x, z).offset_h - p.column(x, z).offset_h).abs());
        }
        assert!(worst < 0.5, "bilinear offset_h error too large: {worst}");
    }

    #[test]
    fn profile_grid_continuous_across_region_edges() {
        // Adjacent regions build independent grids; along the shared edge both
        // interpolate from identical corner samples (pure fn of world coords),
        // so profiles must match bit-for-bit -> no terrain seams.
        let p = TerrainPipeline::new(worldgen_test_seed());
        let a = p.profile_grid(0.0, 0.0, 64.0); // region covering x in [0, 64]
        let b = p.profile_grid(64.0, 0.0, 64.0); // neighbor covering x in [64, 128]
        for j in 0..=64 {
            let z = j as f64;
            let (pa, pb) = (a.profile_at(64.0, z), b.profile_at(64.0, z));
            assert_eq!(pa.offset_h, pb.offset_h);
            assert_eq!(pa.factor, pb.factor);
            assert_eq!(pa.jaggedness, pb.jaggedness);
        }
    }

    #[test]
    fn fused_column_matches_density_graphs_bit_exact() {
        let (p, field) = oracle(worldgen_test_seed());
        let noises = pipeline_noises(worldgen_test_seed());
        let (graph, offset_g, factor_g, jaggedness_g) =
            (final_density_graph(&field), offset(&field), factor(&field), jaggedness(&field));
        for i in 0..64 {
            for j in 0..64 {
                let (x, z) = (i as f64 * 7.3 - 200.0, j as f64 * 11.1 - 150.0);
                let (sx, sz) = (x + SAMPLE_OFFSET_X_M, z + SAMPLE_OFFSET_Z_M);
                let prof = p.column(x, z);
                assert_eq!(prof.offset_h, offset_g.eval(sx, 0.0, sz, &noises) * HEIGHT_SCALE);
                assert_eq!(prof.factor, factor_g.eval(sx, 0.0, sz, &noises));
                assert_eq!(prof.jaggedness, jaggedness_g.eval(sx, 0.0, sz, &noises));
                for y in [-64.0, 0.0, 63.5, 64.5, 200.0, 320.0] {
                    assert_eq!(p.final_density(x, y, z), graph.eval(sx, y, sz, &noises));
                }
            }
        }
    }
}
