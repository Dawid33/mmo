//! Periodicity in x for a world that wraps east–west (the Planet layout):
//! every field that is a FUNCTION OF POSITION — the noises, the plate sites
//! and boundaries, continent centres, volcano and lake sites, the meander —
//! must meet itself at the seam (x = 0 is x = width), so the seam is an
//! ordinary place.
//!
//! - A noise is made periodic by `PeriodicX::sample`: it is the noise itself
//!   everywhere but the last `band_m` before the seam, where it turns
//!   smoothly (`cos θ`, `sin θ`, θ = π/2 · smoothstep across the band) into
//!   the same noise one period back, `f(x − period)` — which is what x = 0
//!   reads. `cos² + sin² = 1` keeps the noise's variance and the smoothstep
//!   keeps its slope continuous.
//! - Geometry (Voronoi sites, segments, centres) gets IMAGES at x ± period
//!   (`with_images`) before any nearest-site or distance query, and queries
//!   use folded x.
//!
//! `PeriodicX::for_world` is `None` for every layout but the Planet; with
//! `None` every call site keeps its old code path and floating-point
//! expression order exactly.

use crate::forge::knobs::Knobs;
use crate::forge::world::{smoothstep, EdgeKind, ForgeParams};
use crate::forged::WorldGlobals;

/// x is periodic with period `period_m`; a noise blends over the last
/// `band_m` before the seam.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeriodicX {
    pub period_m: f64,
    pub band_m: f64,
}

impl PeriodicX {
    /// `Some` for a Planet (period = its width, band a quarter of it until a
    /// noise narrows it with `for_noise`), `None` for every other layout.
    pub fn for_world(p: &ForgeParams) -> Option<PeriodicX> {
        PeriodicX::for_knobs(&p.knobs, p.width_m)
    }

    /// `for_world` from the knobs and the width alone (callers that hold no
    /// `ForgeParams`).
    pub fn for_knobs(k: &Knobs, width_m: f64) -> Option<PeriodicX> {
        k.is_planet().then(|| PeriodicX::new(width_m))
    }

    /// For the runtime, which has no `ForgeParams`: `Some` when the east edge
    /// wraps.
    pub fn from_globals(g: &WorldGlobals) -> Option<PeriodicX> {
        PeriodicX::for_edges(&g.edges, g.width_m)
    }

    /// `Some` when `edges` (N, E, S, W) wrap east–west.
    pub fn for_edges(edges: &[EdgeKind; 4], width_m: f64) -> Option<PeriodicX> {
        (edges[1] == EdgeKind::Wrap).then(|| PeriodicX::new(width_m))
    }

    fn new(period_m: f64) -> PeriodicX {
        PeriodicX { period_m, band_m: 0.25 * period_m }
    }

    /// This period with the blend band of a noise whose longest wavelength is
    /// `wavelength_m`: `min(period / 4, 4 × wavelength)`.
    pub fn for_noise(&self, wavelength_m: f64) -> PeriodicX {
        PeriodicX { period_m: self.period_m, band_m: (0.25 * self.period_m).min(4.0 * wavelength_m) }
    }

    /// `x` folded into `[0, period)`.
    pub fn fold(&self, x: f64) -> f64 {
        let f = x.rem_euclid(self.period_m);
        // rem_euclid of a tiny negative can round up to the period itself.
        if f >= self.period_m { 0.0 } else { f }
    }

    /// The shortest signed `b − a` round the circle, in `(−period/2, period/2]`.
    pub fn dx(&self, a: f64, b: f64) -> f64 {
        let d = self.fold(b - a);
        if d > 0.5 * self.period_m { d - self.period_m } else { d }
    }

    /// `f` made periodic in x: `f(x, z)` (x folded) outside the last `band_m`
    /// before the seam; inside, `cos θ · f(x, z) + sin θ · f(x − period, z)`
    /// with θ = π/2 · smoothstep(t), t running 0 → 1 across the band.
    pub fn sample(&self, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64 {
        let x = self.fold(x);
        let start = self.period_m - self.band_m;
        if x < start {
            return f(x, z);
        }
        let theta = std::f64::consts::FRAC_PI_2 * smoothstep(start, self.period_m, x);
        theta.cos() * f(x, z) + theta.sin() * f(x - self.period_m, z)
    }

    /// [`Self::sample`] in exact arithmetic only (+ − × ÷, sqrt), for the
    /// runtime sampler (`forged.rs`), which may not call `cos` / `sin`: the
    /// weights are `(1 − s, s)` for s = smoothstep(t) across the band,
    /// divided by their norm, so their squares still sum to 1 (the noise
    /// keeps its variance) and they meet `f(x − period)` — what x = 0 reads —
    /// at the seam.
    pub fn sample_exact(&self, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64 {
        let x = self.fold(x);
        let start = self.period_m - self.band_m;
        if x < start {
            return f(x, z);
        }
        let s = smoothstep(start, self.period_m, x);
        let (a, b) = (1.0 - s, s);
        let norm = (a * a + b * b).sqrt();
        (a / norm) * f(x, z) + (b / norm) * f(x - self.period_m, z)
    }

    /// Each item plus its copies at x − period and x + period
    /// (`shift(item, ∓period)`), in that order per item: entry `j` is a copy
    /// of `items[j / 3]`. `x_of` says where an item stands (checked finite).
    pub fn with_images<T: Clone>(&self, items: &[T], x_of: impl Fn(&T) -> f64, shift: impl Fn(&T, f64) -> T) -> Vec<T> {
        let mut out = Vec::with_capacity(3 * items.len());
        for item in items {
            debug_assert!(x_of(item).is_finite(), "an item with no position");
            out.push(item.clone());
            out.push(shift(item, -self.period_m));
            out.push(shift(item, self.period_m));
        }
        out
    }
}

/// `f(x, z)` with `wrap`'s periodicity, or exactly `f(x, z)` without.
pub fn sample_or(wrap: Option<&PeriodicX>, x: f64, z: f64, f: impl Fn(f64, f64) -> f64) -> f64 {
    match wrap {
        Some(w) => w.sample(x, z, f),
        None => f(x, z),
    }
}
