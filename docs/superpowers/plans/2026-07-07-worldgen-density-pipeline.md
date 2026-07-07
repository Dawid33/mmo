# Density-Pipeline Worldgen Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `crates/worldgen`'s flat parity floors with continent-scale terrain built from a composable density-function graph driven by low-frequency noise pushed through nested Hermite splines, resolved to smooth per-voxel surfaces.

**Architecture:** Build a reusable engine — deterministic Perlin/octave `noise`, a nested-`Spline` type with cubic-Hermite eval, and a `DensityFn` node graph — then a `pipeline` that wires our own tuned splines into `final_density`. `generate_region` locates each block-column's surface with a coarse per-block scan and smooths the straddle block at voxel resolution, packing into the existing `Column`/`Chunk`/`ChiselData` types. A world seed and a `Materials` set are threaded through the generator closure on both the server and the offline client.

**Tech Stack:** Rust (edition 2024), the vendored `game` crate's block/voxel model, `crc32fast`/`bincode` for determinism tests. No new external dependencies.

## Global Constraints

- **Determinism (hard bar):** the noise/spline/density path uses **only** IEEE-basic `f64` ops (`+ - * /`), `floor`, and `f64::powi` (integer power). **Forbidden anywhere in the worldgen path:** `f64::mul_add`/fma, `std` transcendentals (`sin`/`cos`/`exp`/`powf`/`sqrt`), and any `rayon`/parallel accumulation. Must be bit-identical across x86-64, ARM, and wasm32.
- `worldgen::generate_region` stays a **pure** function of its arguments — no clocks, no ambient RNG, no global state.
- Scale model (from `crates/game/src/block.rs`): `1 block = 16³ voxels = 1 m³` (`BLOCK_VOXELS = 16`); a `Chunk` is `CHUNK_BLOCKS = 2` blocks per axis; a region is `REGION_CHUNKS = 8` chunks = **16 blocks = 16 m** per axis (`REGION_SIZE = 256.0` voxels). Density constants are in **meters (blocks)**; a voxel at world-voxel coordinate `v` has meter coordinate `v / 16.0`.
- World frame: `SEA_LEVEL = 64` (blocks), density solid where `final_density > 0`.
- Keep the root `justfile` in sync with any new build/test/bench invocation.

---

## File Structure

- Create `crates/worldgen/src/noise.rs` — `PerlinNoise`, `OctaveNoise` (deterministic, seeded).
- Create `crates/worldgen/src/spline.rs` — `Spline` (scalar or nested), cubic-Hermite `eval`.
- Create `crates/worldgen/src/density.rs` — `DensityFn` enum, `NoiseSet`, `Ctx`, `eval`.
- Create `crates/worldgen/src/pipeline.rs` — `TerrainPipeline`: builds our tuned graph + constants, exposes `final_density(x,y,z)`.
- Create `crates/worldgen/src/material.rs` — `Materials` struct, `material_at(depth_below_surface, block_y)`.
- Modify `crates/worldgen/src/lib.rs` — module decls; rewrite `generate_region` (coarse-find + voxel-smooth + `Column` assembly).
- Modify `crates/worldgen/src/main.rs` — extend region-gen bench.
- Modify `crates/worldgen/tests/region_load_cost.rs` — update to the new signature.
- Modify `assets/blocks/blocks.ron` — add `grass`, `sand`, `water` block defs.
- Modify `crates/server/src/lib.rs:~247` — resolve `Materials`, pass a seed into the generator closure.
- Modify `crates/client/src/local_server.rs:~35-38` — same seed + `Materials` wiring for the offline world.

---

## Task 1: Deterministic noise (`noise.rs`)

**Files:**
- Create: `crates/worldgen/src/noise.rs`
- Modify: `crates/worldgen/src/lib.rs` (add `mod noise;`)

**Interfaces:**
- Produces: `PerlinNoise::new(seed: u64) -> PerlinNoise`, `PerlinNoise::sample2(&self, x: f64, z: f64) -> f64` (range ≈ [-1, 1]); `OctaveNoise::new(seed: u64, first_octave: i32, amplitudes: Vec<f64>) -> OctaveNoise`, `OctaveNoise::sample2(&self, x: f64, z: f64) -> f64` (normalized ≈ [-1, 1]).

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/noise.rs`:

```rust
//! Deterministic gradient noise. Pure `f64`: only + - * / , `floor`, and
//! integer `powi` — no transcendentals, no fma (see plan Global Constraints).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perlin_is_seeded_and_bounded() {
        let n = PerlinNoise::new(42);
        // Determinism: same input -> same output.
        assert_eq!(n.sample2(1.5, 2.5), n.sample2(1.5, 2.5));
        // Bounded roughly in [-1, 1] over a grid of samples.
        for i in 0..50 {
            for j in 0..50 {
                let v = n.sample2(i as f64 * 0.37, j as f64 * 0.37);
                assert!(v >= -1.5 && v <= 1.5, "out of range: {v}");
            }
        }
        // Different seed -> different field.
        assert_ne!(PerlinNoise::new(1).sample2(3.3, 4.4), PerlinNoise::new(2).sample2(3.3, 4.4));
    }

    #[test]
    fn octave_sums_and_normalizes() {
        let o = OctaveNoise::new(7, -3, vec![1.0, 0.5, 0.25]);
        assert_eq!(o.sample2(10.0, 20.0), o.sample2(10.0, 20.0)); // deterministic
        for i in 0..40 {
            let v = o.sample2(i as f64 * 1.1, i as f64 * -0.7);
            assert!(v >= -1.5 && v <= 1.5, "octave out of range: {v}");
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen noise:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type PerlinNoise` / `OctaveNoise`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/worldgen/src/noise.rs` (above the `#[cfg(test)]` module):

```rust
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// 8-direction 2D gradient dot, chosen by the low 3 bits of the hash.
fn grad2(hash: u8, x: f64, y: f64) -> f64 {
    match hash & 7 {
        0 => x + y,
        1 => x - y,
        2 => -x + y,
        3 => -x - y,
        4 => x,
        5 => -x,
        6 => y,
        _ => -y,
    }
}

/// Classic Perlin gradient noise over a seeded 256-entry permutation.
pub struct PerlinNoise {
    perm: [u8; 512],
}

impl PerlinNoise {
    pub fn new(seed: u64) -> Self {
        let mut p: [u8; 256] = [0; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        // Fisher-Yates shuffle driven by splitmix64.
        let mut state = seed ^ 0xA0761D_6478BD_642F;
        for i in (1..256).rev() {
            let j = (splitmix64(&mut state) % (i as u64 + 1)) as usize;
            p.swap(i, j);
        }
        let mut perm = [0u8; 512];
        for i in 0..512 {
            perm[i] = p[i & 255];
        }
        Self { perm }
    }

    pub fn sample2(&self, x: f64, z: f64) -> f64 {
        let xi = x.floor();
        let zi = z.floor();
        let xf = x - xi;
        let zf = z - zi;
        let xi = (xi as i64 & 255) as usize;
        let zi = (zi as i64 & 255) as usize;
        let u = fade(xf);
        let v = fade(zf);
        let p = &self.perm;
        let a = p[xi] as usize + zi;
        let b = p[xi + 1] as usize + zi;
        let x1 = lerp(u, grad2(p[a], xf, zf), grad2(p[b], xf - 1.0, zf));
        let x2 = lerp(u, grad2(p[a + 1], xf, zf - 1.0), grad2(p[b + 1], xf - 1.0, zf - 1.0));
        lerp(v, x1, x2)
    }
}

/// Sum of Perlin octaves at doubling frequencies, amplitude-weighted and
/// normalized. `first_octave` sets the base frequency `2^first_octave`
/// (negative => long wavelength); octave `i` runs at `2^(first_octave + i)`.
pub struct OctaveNoise {
    octaves: Vec<PerlinNoise>,
    amplitudes: Vec<f64>,
    first_octave: i32,
    norm: f64,
}

impl OctaveNoise {
    pub fn new(seed: u64, first_octave: i32, amplitudes: Vec<f64>) -> Self {
        let mut state = seed;
        let octaves = (0..amplitudes.len())
            .map(|_| PerlinNoise::new(splitmix64(&mut state)))
            .collect();
        let norm = amplitudes.iter().sum::<f64>().max(1.0);
        Self { octaves, amplitudes, first_octave, norm }
    }

    pub fn sample2(&self, x: f64, z: f64) -> f64 {
        let mut acc = 0.0;
        for (i, oct) in self.octaves.iter().enumerate() {
            let freq = 2f64.powi(self.first_octave + i as i32);
            acc += self.amplitudes[i] * oct.sample2(x * freq, z * freq);
        }
        acc / self.norm
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen noise:: 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/noise.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): deterministic Perlin + octave noise"
```

---

## Task 2: Nested spline with Hermite eval (`spline.rs`)

**Files:**
- Create: `crates/worldgen/src/spline.rs`
- Modify: `crates/worldgen/src/lib.rs` (add `mod spline;`)

**Interfaces:**
- Produces: `Spline` with variants `Const(f64)` and `Curve { points: Vec<Point> }`; `Point { location: f64, value: Box<Spline>, derivative: f64 }`; `Spline::eval(&self, coord: f64) -> f64`. Helpers `Spline::constant(f64)` and `Spline::point(location, value: Spline, derivative)`.

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/spline.rs`:

```rust
//! Cubic-Hermite splines whose control-point values may themselves be splines
//! (recursion is load-bearing). Pure `f64` (see plan Global Constraints).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_ignores_coord() {
        assert_eq!(Spline::constant(3.0).eval(-99.0), 3.0);
        assert_eq!(Spline::constant(3.0).eval(99.0), 3.0);
    }

    #[test]
    fn passes_through_control_points() {
        // Two flat points at (0 -> 0) and (1 -> 10).
        let s = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(0.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(10.0)), derivative: 0.0 },
            ],
        };
        assert_eq!(s.eval(0.0), 0.0);
        assert_eq!(s.eval(1.0), 10.0);
        // Monotone between, and flat-derivative midpoint sits at the Hermite mean.
        let mid = s.eval(0.5);
        assert!(mid > 0.0 && mid < 10.0, "mid={mid}");
    }

    #[test]
    fn clamps_outside_range() {
        let s = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(2.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(8.0)), derivative: 0.0 },
            ],
        };
        // Linear extrapolation by the endpoint derivative (0 here) => clamps flat.
        assert_eq!(s.eval(-5.0), 2.0);
        assert_eq!(s.eval(5.0), 8.0);
    }

    #[test]
    fn nested_value_is_evaluated_at_same_coord() {
        // Outer point's value is itself a spline; eval it at the same coord.
        let inner = Spline::Curve {
            points: vec![
                Point { location: 0.0, value: Box::new(Spline::constant(0.0)), derivative: 0.0 },
                Point { location: 1.0, value: Box::new(Spline::constant(100.0)), derivative: 0.0 },
            ],
        };
        let outer = Spline::Curve {
            points: vec![Point { location: 0.0, value: Box::new(inner), derivative: 0.0 }],
        };
        // Single point => returns that point's value, evaluated at the coord.
        assert_eq!(outer.eval(1.0), 100.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen spline:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type Spline` / `Point`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/worldgen/src/spline.rs`:

```rust
/// A control point: `value` is itself a spline (may be `Const`), evaluated at
/// the outer coordinate; `derivative` is the slope used for Hermite blending.
pub struct Point {
    pub location: f64,
    pub value: Box<Spline>,
    pub derivative: f64,
}

pub enum Spline {
    Const(f64),
    Curve { points: Vec<Point> },
}

impl Spline {
    pub fn constant(v: f64) -> Spline {
        Spline::Const(v)
    }

    pub fn point(location: f64, value: Spline, derivative: f64) -> Point {
        Point { location, value: Box::new(value), derivative }
    }

    pub fn eval(&self, coord: f64) -> f64 {
        let points = match self {
            Spline::Const(v) => return *v,
            Spline::Curve { points } => points,
        };
        if points.is_empty() {
            return 0.0;
        }
        // Below the first point: linear by its derivative.
        let first = &points[0];
        if coord <= first.location {
            return first.value.eval(coord) + first.derivative * (coord - first.location);
        }
        // Above the last point: linear by its derivative.
        let last = &points[points.len() - 1];
        if coord >= last.location {
            return last.value.eval(coord) + last.derivative * (coord - last.location);
        }
        // Find the bracketing segment [p0, p1].
        let mut i = 0;
        while coord >= points[i + 1].location {
            i += 1;
        }
        let p0 = &points[i];
        let p1 = &points[i + 1];
        let (l0, l1) = (p0.location, p1.location);
        let v0 = p0.value.eval(coord);
        let v1 = p1.value.eval(coord);
        let span = l1 - l0;
        let t = (coord - l0) / span;
        // Cubic Hermite basis (Minecraft's spline interpolation).
        let k0 = v0;
        let k1 = v1;
        let m0 = p0.derivative * span;
        let m1 = p1.derivative * span;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * k0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * k1
            + (t3 - t2) * m1
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen spline:: 2>&1 | tail -20`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/spline.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): nested cubic-Hermite spline"
```

---

## Task 3: Density-function graph (`density.rs`)

**Files:**
- Create: `crates/worldgen/src/density.rs`
- Modify: `crates/worldgen/src/lib.rs` (add `mod density;`)

**Interfaces:**
- Consumes: `noise::OctaveNoise`, `spline::Spline`.
- Produces: `NoiseSet(Vec<OctaveNoise>)`; `DensityFn` enum (`Const`, `Noise{idx,xz_scale}`, `YGradient{from_y,to_y,from_v,to_v}`, `Add/Mul/Min/Max(Box,Box)`, `Abs(Box)`, `Clamp{input,min,max}`, `SplineOf{coord:Box, spline:Spline}`, `RangeChoice{input:Box,min,max,in_range:Box,out_range:Box}`); `DensityFn::eval(&self, x: f64, y: f64, z: f64, noises: &NoiseSet) -> f64`. Convenience ctors `DensityFn::add/mul/min/max/clamp/spline_of/range_choice/abs`.

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/density.rs`:

```rust
//! Composable density functions evaluated at world-meter (x, y, z). Pure `f64`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::OctaveNoise;
    use crate::spline::Spline;

    fn empty_noises() -> NoiseSet {
        NoiseSet(vec![])
    }

    #[test]
    fn arithmetic_nodes() {
        let n = empty_noises();
        let two = DensityFn::Const(2.0);
        let three = DensityFn::Const(3.0);
        assert_eq!(DensityFn::add(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 5.0);
        assert_eq!(DensityFn::mul(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 6.0);
        assert_eq!(DensityFn::min(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 2.0);
        assert_eq!(DensityFn::max(two.clone(), three.clone()).eval(0.0, 0.0, 0.0, &n), 3.0);
        assert_eq!(DensityFn::abs(DensityFn::Const(-4.0)).eval(0.0, 0.0, 0.0, &n), 4.0);
    }

    #[test]
    fn clamp_and_gradient() {
        let n = empty_noises();
        let c = DensityFn::clamp(DensityFn::Const(9.0), -1.0, 1.0);
        assert_eq!(c.eval(0.0, 0.0, 0.0, &n), 1.0);
        // Gradient: at from_y -> from_v, at to_y -> to_v, clamped outside.
        let g = DensityFn::YGradient { from_y: 0.0, to_y: 100.0, from_v: 0.0, to_v: 10.0 };
        assert_eq!(g.eval(0.0, 0.0, 0.0, &n), 0.0);
        assert_eq!(g.eval(0.0, 100.0, 0.0, &n), 10.0);
        assert_eq!(g.eval(0.0, 50.0, 0.0, &n), 5.0);
        assert_eq!(g.eval(0.0, -20.0, 0.0, &n), 0.0);
        assert_eq!(g.eval(0.0, 200.0, 0.0, &n), 10.0);
    }

    #[test]
    fn range_choice_selects() {
        let n = empty_noises();
        let rc = DensityFn::range_choice(
            DensityFn::Const(0.5),
            0.0,
            1.0,
            DensityFn::Const(100.0), // in range
            DensityFn::Const(-100.0), // out of range
        );
        assert_eq!(rc.eval(0.0, 0.0, 0.0, &n), 100.0);
    }

    #[test]
    fn spline_of_uses_input_as_coord() {
        let n = empty_noises();
        let s = Spline::Curve {
            points: vec![
                crate::spline::Spline::point(0.0, Spline::constant(0.0), 0.0),
                crate::spline::Spline::point(1.0, Spline::constant(10.0), 0.0),
            ],
        };
        let d = DensityFn::spline_of(DensityFn::Const(1.0), s);
        assert_eq!(d.eval(0.0, 0.0, 0.0, &n), 10.0);
    }

    #[test]
    fn noise_node_reads_noiseset() {
        let noises = NoiseSet(vec![OctaveNoise::new(1, -2, vec![1.0])]);
        let d = DensityFn::Noise { idx: 0, xz_scale: 1.0 };
        assert_eq!(d.eval(5.0, 0.0, 6.0, &noises), d.eval(5.0, 0.0, 6.0, &noises));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen density:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type NoiseSet` / `DensityFn`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/worldgen/src/density.rs`:

```rust
use crate::noise::OctaveNoise;
use crate::spline::Spline;

/// The set of named noises a `DensityFn::Noise{idx}` indexes into. The pipeline
/// owns the index assignment (see `pipeline.rs`).
pub struct NoiseSet(pub Vec<OctaveNoise>);

pub enum DensityFn {
    Const(f64),
    /// 2D noise `idx`, sampled at `(x, z) * xz_scale` (meters).
    Noise { idx: usize, xz_scale: f64 },
    /// Linear ramp in `y` from `from_v` at `from_y` to `to_v` at `to_y`, clamped.
    YGradient { from_y: f64, to_y: f64, from_v: f64, to_v: f64 },
    Add(Box<DensityFn>, Box<DensityFn>),
    Mul(Box<DensityFn>, Box<DensityFn>),
    Min(Box<DensityFn>, Box<DensityFn>),
    Max(Box<DensityFn>, Box<DensityFn>),
    Abs(Box<DensityFn>),
    Clamp { input: Box<DensityFn>, min: f64, max: f64 },
    SplineOf { coord: Box<DensityFn>, spline: Spline },
    RangeChoice {
        input: Box<DensityFn>,
        min: f64,
        max: f64,
        in_range: Box<DensityFn>,
        out_range: Box<DensityFn>,
    },
}

impl Clone for DensityFn {
    fn clone(&self) -> Self {
        match self {
            DensityFn::Const(v) => DensityFn::Const(*v),
            DensityFn::Noise { idx, xz_scale } => DensityFn::Noise { idx: *idx, xz_scale: *xz_scale },
            DensityFn::YGradient { from_y, to_y, from_v, to_v } => {
                DensityFn::YGradient { from_y: *from_y, to_y: *to_y, from_v: *from_v, to_v: *to_v }
            }
            DensityFn::Add(a, b) => DensityFn::Add(a.clone(), b.clone()),
            DensityFn::Mul(a, b) => DensityFn::Mul(a.clone(), b.clone()),
            DensityFn::Min(a, b) => DensityFn::Min(a.clone(), b.clone()),
            DensityFn::Max(a, b) => DensityFn::Max(a.clone(), b.clone()),
            DensityFn::Abs(a) => DensityFn::Abs(a.clone()),
            DensityFn::Clamp { input, min, max } => {
                DensityFn::Clamp { input: input.clone(), min: *min, max: *max }
            }
            DensityFn::SplineOf { coord, spline } => {
                DensityFn::SplineOf { coord: coord.clone(), spline: spline.clone() }
            }
            DensityFn::RangeChoice { input, min, max, in_range, out_range } => DensityFn::RangeChoice {
                input: input.clone(),
                min: *min,
                max: *max,
                in_range: in_range.clone(),
                out_range: out_range.clone(),
            },
        }
    }
}

impl DensityFn {
    pub fn add(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Add(Box::new(a), Box::new(b)) }
    pub fn mul(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Mul(Box::new(a), Box::new(b)) }
    pub fn min(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Min(Box::new(a), Box::new(b)) }
    pub fn max(a: DensityFn, b: DensityFn) -> DensityFn { DensityFn::Max(Box::new(a), Box::new(b)) }
    pub fn abs(a: DensityFn) -> DensityFn { DensityFn::Abs(Box::new(a)) }
    pub fn clamp(a: DensityFn, min: f64, max: f64) -> DensityFn {
        DensityFn::Clamp { input: Box::new(a), min, max }
    }
    pub fn spline_of(coord: DensityFn, spline: Spline) -> DensityFn {
        DensityFn::SplineOf { coord: Box::new(coord), spline }
    }
    pub fn range_choice(input: DensityFn, min: f64, max: f64, in_range: DensityFn, out_range: DensityFn) -> DensityFn {
        DensityFn::RangeChoice {
            input: Box::new(input),
            min,
            max,
            in_range: Box::new(in_range),
            out_range: Box::new(out_range),
        }
    }

    pub fn eval(&self, x: f64, y: f64, z: f64, noises: &NoiseSet) -> f64 {
        match self {
            DensityFn::Const(v) => *v,
            DensityFn::Noise { idx, xz_scale } => noises.0[*idx].sample2(x * xz_scale, z * xz_scale),
            DensityFn::YGradient { from_y, to_y, from_v, to_v } => {
                let t = ((y - from_y) / (to_y - from_y)).clamp(0.0, 1.0);
                from_v + t * (to_v - from_v)
            }
            DensityFn::Add(a, b) => a.eval(x, y, z, noises) + b.eval(x, y, z, noises),
            DensityFn::Mul(a, b) => a.eval(x, y, z, noises) * b.eval(x, y, z, noises),
            DensityFn::Min(a, b) => a.eval(x, y, z, noises).min(b.eval(x, y, z, noises)),
            DensityFn::Max(a, b) => a.eval(x, y, z, noises).max(b.eval(x, y, z, noises)),
            DensityFn::Abs(a) => a.eval(x, y, z, noises).abs(),
            DensityFn::Clamp { input, min, max } => input.eval(x, y, z, noises).clamp(*min, *max),
            DensityFn::SplineOf { coord, spline } => spline.eval(coord.eval(x, y, z, noises)),
            DensityFn::RangeChoice { input, min, max, in_range, out_range } => {
                let v = input.eval(x, y, z, noises);
                if v >= *min && v < *max {
                    in_range.eval(x, y, z, noises)
                } else {
                    out_range.eval(x, y, z, noises)
                }
            }
        }
    }
}
```

Also add `#[derive(Clone)]`-free note: `Spline` needs `Clone` for `DensityFn: Clone`. Add `#[derive(Clone)]` to `Spline` and `Point` in `spline.rs` (edit their definitions to `#[derive(Clone)]\npub struct Point {…}` and `#[derive(Clone)]\npub enum Spline {…}`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen density:: 2>&1 | tail -20`
Expected: PASS (5 tests). If it complains `Spline` is not `Clone`, confirm the `#[derive(Clone)]` edits above were applied.

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/density.rs crates/worldgen/src/spline.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): DensityFn graph evaluator"
```

---

## Task 4: Terrain pipeline (`pipeline.rs`)

**Files:**
- Create: `crates/worldgen/src/pipeline.rs`
- Modify: `crates/worldgen/src/lib.rs` (add `mod pipeline;`)

**Interfaces:**
- Consumes: `density::{DensityFn, NoiseSet}`, `spline::Spline`, `noise::OctaveNoise`.
- Produces: `pub const SEA_LEVEL: f64 = 64.0`; `TerrainPipeline` with `TerrainPipeline::new(seed: u64) -> TerrainPipeline` and `TerrainPipeline::final_density(&self, x: f64, y: f64, z: f64) -> f64` (x/y/z in meters; solid where `> 0`).

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/pipeline.rs`:

```rust
//! Our tuned density graph: continentalness/erosion/ridges -> offset/factor/
//! jaggedness -> depth -> final_density. Numbers here are hand-tuned, not
//! ported from Tectonic (see the design spec).

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

    #[test]
    fn ocean_is_the_majority() {
        // Over a wide area at sea level, most columns' surface is below sea
        // level (ocean): sample density just above sea level and expect mostly air.
        let p = TerrainPipeline::new(7);
        let mut air = 0;
        let mut total = 0;
        let step = 47.0; // meters, coprime-ish with wavelengths to spread samples
        for i in 0..40 {
            for j in 0..40 {
                let x = i as f64 * step;
                let z = j as f64 * step;
                if p.final_density(x, SEA_LEVEL + 2.0, z) <= 0.0 {
                    air += 1;
                }
                total += 1;
            }
        }
        assert!(air * 2 > total, "expected majority air above sea level, got {air}/{total}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen pipeline:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type TerrainPipeline`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/worldgen/src/pipeline.rs`. These constants/splines are a tunable starting point; adjust magnitudes if the shape tests need it.

```rust
use crate::density::{DensityFn, NoiseSet};
use crate::noise::OctaveNoise;
use crate::spline::Spline;

pub const SEA_LEVEL: f64 = 64.0;

// Noise indices into NoiseSet.
const N_CONTINENTS: usize = 0;
const N_EROSION: usize = 1;
const N_RIDGES: usize = 2;

// Tuning constants (hand-picked, not ported).
const OCEAN_OFFSET: f64 = -0.5;
const MIN_OFFSET: f64 = -1.0;
const MAX_OFFSET: f64 = 2.0;
const HEIGHT_SCALE: f64 = 96.0; // meters of surface swing per offset unit
const DEPTH_PER_BLOCK: f64 = 1.0 / 32.0; // density falloff per meter of height

fn continentalness() -> DensityFn {
    DensityFn::Noise { idx: N_CONTINENTS, xz_scale: 0.13 }
}
fn erosion() -> DensityFn {
    DensityFn::Noise { idx: N_EROSION, xz_scale: 0.25 }
}
fn ridges() -> DensityFn {
    DensityFn::Noise { idx: N_RIDGES, xz_scale: 0.5 }
}

/// abs(continentalness) pushed through a rising curve, minus the ocean bias.
/// Negative over most of the map (ocean); positive where |continents| is large.
fn raw_continents() -> DensityFn {
    let s = Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(0.4, Spline::constant(0.6), 1.0),
            Spline::point(1.0, Spline::constant(1.2), 0.0),
        ],
    };
    DensityFn::add(
        DensityFn::Const(OCEAN_OFFSET),
        DensityFn::spline_of(DensityFn::abs(continentalness()), s),
    )
}

/// Surface height offset (in offset units) as a spline on raw_continents,
/// its inland high end pulled down by erosion (flat plains vs mountains).
fn offset() -> DensityFn {
    let erosion_mod = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(1.0), 0.0), // low erosion -> tall
            Spline::point(0.2, Spline::constant(0.5), 0.0),
            Spline::point(1.0, Spline::constant(0.15), 0.0), // high erosion -> flat
        ],
    };
    let base = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(-0.9), 0.0), // deep ocean
            Spline::point(-0.2, Spline::constant(-0.3), 0.5), // shelf
            Spline::point(0.0, Spline::constant(0.02), 0.6),  // coast ~ sea level
            Spline::point(0.6, Spline::constant(0.9), 1.0),   // inland
            Spline::point(1.2, Spline::constant(1.8), 0.5),   // highland
        ],
    };
    // offset = base(raw) * erosion_mod(erosion), clamped.
    DensityFn::clamp(
        DensityFn::mul(
            DensityFn::spline_of(raw_continents(), base),
            DensityFn::spline_of(erosion(), erosion_mod),
        ),
        MIN_OFFSET,
        MAX_OFFSET,
    )
}

/// Vertical stretch factor (always positive): tall in low-erosion inland.
fn factor() -> DensityFn {
    let s = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(0.6), 0.0),
            Spline::point(0.0, Spline::constant(1.2), 0.0),
            Spline::point(1.0, Spline::constant(3.0), 0.0),
        ],
    };
    DensityFn::spline_of(offset(), s)
}

/// Jaggedness: a ridges-driven bump, nonzero mainly at high offset.
fn jaggedness() -> DensityFn {
    let gate = Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(1.0, Spline::constant(0.6), 0.0),
        ],
    };
    DensityFn::mul(
        DensityFn::spline_of(offset(), gate),
        DensityFn::abs(ridges()),
    )
}

/// final_density = (surface_height - y) * DEPTH_PER_BLOCK * factor + jaggedness.
/// `surface_height = SEA_LEVEL + offset*HEIGHT_SCALE` (meters); the value is
/// positive below the surface, negative above. `neg_y` is a wide linear ramp
/// carrying `-y` across the whole world band.
fn final_density_graph() -> DensityFn {
    let neg_y = DensityFn::YGradient { from_y: -64.0, to_y: 320.0, from_v: 64.0, to_v: -320.0 };
    let surface = DensityFn::add(
        DensityFn::mul(offset(), DensityFn::Const(HEIGHT_SCALE)),
        DensityFn::add(DensityFn::Const(SEA_LEVEL), neg_y),
    );
    DensityFn::add(
        DensityFn::mul(
            DensityFn::mul(surface, DensityFn::Const(DEPTH_PER_BLOCK)),
            factor(),
        ),
        jaggedness(),
    )
}

pub struct TerrainPipeline {
    noises: NoiseSet,
    graph: DensityFn,
}

impl TerrainPipeline {
    pub fn new(seed: u64) -> Self {
        // Per-noise sub-seeds derived from the world seed; amplitudes are our
        // own (long-wavelength continents, medium erosion, short ridges).
        let noises = NoiseSet(vec![
            OctaveNoise::new(seed ^ 0x00C0, -10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
            OctaveNoise::new(seed ^ 0x00E0, -10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
            OctaveNoise::new(seed ^ 0x00D0, -8, vec![1.0, 2.0, 1.0]),
        ]);
        Self { noises, graph: final_density_graph() }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        self.graph.eval(x, y, z, &self.noises)
    }
}
```

> **Implementer note:** these magnitudes are a starting point — tune against the shape tests. If `ocean_is_the_majority` fails, raise `OCEAN_OFFSET` magnitude (more negative) or lower the `base` spline's coast value; if `density_decreases_with_height` fails, confirm `neg_y` ramps from positive (low y) to negative (high y). Delete any unused `HEIGHT_SCALE`/`DEPTH_PER_BLOCK`-adjacent binding clippy flags.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen pipeline:: 2>&1 | tail -20`
Expected: PASS (3 tests). Tune constants per the implementer note if a shape test fails.

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/pipeline.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): tuned terrain density pipeline"
```

---

## Task 5: Surface materials (`material.rs`)

**Files:**
- Create: `crates/worldgen/src/material.rs`
- Modify: `crates/worldgen/src/lib.rs` (add `mod material;`)

**Interfaces:**
- Consumes: `game::BlockId`.
- Produces: `#[derive(Clone, Copy)] Materials { stone, dirt, grass, sand, water: BlockId }`; `Materials::material_at(&self, depth_below_surface: i32, block_y: i32) -> BlockId` — `depth_below_surface == 0` is the top solid block.

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/material.rs`:

```rust
//! Surface material assignment: a pure function of depth-below-surface and
//! block Y. No noise in v1 (see design spec §5).

#[cfg(test)]
mod tests {
    use super::*;
    use game::BlockId;

    fn mats() -> Materials {
        Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
        }
    }

    #[test]
    fn top_block_is_grass_inland() {
        let m = mats();
        assert_eq!(m.material_at(0, 100), BlockId(3)); // grass on top, well above sea
        assert_eq!(m.material_at(1, 100), BlockId(1)); // dirt just below
        assert_eq!(m.material_at(4, 100), BlockId(2)); // stone deeper
    }

    #[test]
    fn beaches_are_sand_near_sea_level() {
        let m = mats();
        // Top block at/just above sea level -> sand.
        assert_eq!(m.material_at(0, 64), BlockId(4));
        assert_eq!(m.material_at(0, 65), BlockId(4));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen material:: 2>&1 | tail -20`
Expected: FAIL — `cannot find type Materials`.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/worldgen/src/material.rs`:

```rust
use crate::pipeline::SEA_LEVEL;
use game::BlockId;

#[derive(Clone, Copy)]
pub struct Materials {
    pub stone: BlockId,
    pub dirt: BlockId,
    pub grass: BlockId,
    pub sand: BlockId,
    pub water: BlockId,
}

impl Materials {
    /// Block placed at a solid voxel/block. `depth_below_surface == 0` is the
    /// topmost solid block of a column.
    pub fn material_at(&self, depth_below_surface: i32, block_y: i32) -> BlockId {
        let sea = SEA_LEVEL as i32;
        if depth_below_surface == 0 {
            // Beach band around sea level, else grass.
            if block_y <= sea + 1 {
                self.sand
            } else {
                self.grass
            }
        } else if depth_below_surface <= 3 {
            // Sand keeps going a few blocks down on beaches; dirt inland.
            if block_y <= sea + 1 {
                self.sand
            } else {
                self.dirt
            }
        } else {
            self.stone
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen material:: 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/material.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): surface material assignment"
```

---

## Task 6: Block manifest — grass, sand, water

**Files:**
- Modify: `assets/blocks/blocks.ron`

**Interfaces:**
- Produces: registry names `grass`, `sand`, `water` resolvable via `BlockRegistry::id_of`.

- [ ] **Step 1: Write the failing test**

Add to `crates/worldgen/src/material.rs` `tests` module:

```rust
    #[test]
    fn manifest_defines_terrain_materials() {
        let manifest = include_str!("../../../assets/blocks/blocks.ron");
        let reg = game::BlockRegistry::from_ron(manifest).expect("valid manifest");
        for name in ["stone", "dirt", "grass", "sand", "water"] {
            assert!(reg.id_of(name).is_some(), "manifest must define {name}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen manifest_defines 2>&1 | tail -20`
Expected: FAIL — `grass`/`sand`/`water` unresolved (assert fires).

- [ ] **Step 3: Add the block defs**

Edit `assets/blocks/blocks.ron`. Uncomment/replace the grass line and add sand + water so the block list reads (keep existing `air`/`dirt`/`stone`, reuse `dirt.png` as placeholder art where a texture is missing):

```
        (id: 0, name: "air",   textures: Untextured),
        (id: 1, name: "dirt",  textures: All("dirt.png")),
        (id: 2, name: "stone", textures: All("dirt.png")), // placeholder art until stone.png exists
        (id: 3, name: "grass", textures: All("dirt.png")), // placeholder art until grass textures exist
        (id: 4, name: "sand",  textures: All("dirt.png")), // placeholder art until sand.png exists
        (id: 5, name: "water", textures: All("dirt.png")), // placeholder art until water.png exists
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen manifest_defines 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add assets/blocks/blocks.ron crates/worldgen/src/material.rs
git commit -m "feat(blocks): add grass, sand, water block defs"
```

---

## Task 7: Rewrite `generate_region` (coarse-find + voxel-smooth)

**Files:**
- Modify: `crates/worldgen/src/lib.rs` (replace `generate_region` + `floor_height`; keep module decls from earlier tasks)

**Interfaces:**
- Consumes: `pipeline::{TerrainPipeline, SEA_LEVEL}`, `material::Materials`, `game::{Block, BlockId, Chunk, ChiselData, BlockIndex, Column, ColumnCoords, RegionCoords, REGION_CHUNKS, CHUNK_BLOCKS, BLOCK_VOXELS}`.
- Produces: `pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)>`.

- [ ] **Step 1: Write the failing test**

Replace the `#[cfg(test)] mod tests` block in `crates/worldgen/src/lib.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use game::{Block, BlockId, Column, ColumnCoords, ReadVol, BlockPos, REGION_CHUNKS};

    fn mats() -> crate::material::Materials {
        crate::material::Materials {
            stone: BlockId(2),
            dirt: BlockId(1),
            grass: BlockId(3),
            sand: BlockId(4),
            water: BlockId(5),
        }
    }

    fn crc(cols: &[(ColumnCoords, Column)]) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(&bincode::serialize(cols).unwrap());
        h.finalize()
    }

    #[test]
    fn produces_sixty_four_columns() {
        let cols = generate_region(RegionCoords::new(0, 0), 1, mats());
        assert_eq!(cols.len(), REGION_CHUNKS * REGION_CHUNKS);
    }

    #[test]
    fn deterministic_same_seed() {
        let a = generate_region(RegionCoords::new(-3, 7), 99, mats());
        let b = generate_region(RegionCoords::new(-3, 7), 99, mats());
        assert_eq!(crc(&a), crc(&b));
    }

    #[test]
    fn different_seed_differs() {
        let a = generate_region(RegionCoords::new(0, 0), 1, mats());
        let b = generate_region(RegionCoords::new(0, 0), 2, mats());
        assert_ne!(crc(&a), crc(&b));
    }

    #[test]
    fn columns_contain_solid_ground() {
        // Some voxel in the region is solid (not an all-air world).
        let cols = generate_region(RegionCoords::new(5, 5), 3, mats());
        let any_solid = cols.iter().any(|(_, col)| {
            (0..64).any(|y| col.get(BlockPos::new(0, y, 0)).id != BlockId::AIR)
        });
        assert!(any_solid, "expected some solid ground in the region");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p worldgen --lib tests:: 2>&1 | tail -20`
Expected: FAIL — `generate_region` arity mismatch / `material` path.

- [ ] **Step 3: Write the implementation**

Replace the old `floor_height` + `generate_region` (lines ~10-37 of `lib.rs`) with:

```rust
use game::{
    Block, BlockId, BlockIndex, ChiselData, Chunk, Column, ColumnCoords, RegionCoords, SubChunk,
    BLOCK_VOXELS, CHUNK_BLOCKS, REGION_CHUNKS,
};

use crate::material::Materials;
use crate::pipeline::{TerrainPipeline, SEA_LEVEL};

// Region span in blocks: REGION_CHUNKS chunks * CHUNK_BLOCKS blocks/chunk.
const REGION_BLOCKS: i32 = (REGION_CHUNKS * CHUNK_BLOCKS) as i32; // 16
// Coarse vertical scan bounds (blocks). Terrain lives well inside this band.
const SCAN_TOP: i32 = 320;
const SCAN_BOTTOM: i32 = -64;

/// Meter coordinate of a block center at world block index `b`.
fn block_center_m(b: i32) -> f64 {
    b as f64 + 0.5
}

/// Highest solid block Y in a 1 m column (world block coords), or `SCAN_BOTTOM - 1`
/// if the whole column is air. Coarse per-block scan (design §4 step 1).
fn surface_block_y(p: &TerrainPipeline, wx: i32, wz: i32) -> i32 {
    let (x, z) = (block_center_m(wx), block_center_m(wz));
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if p.final_density(x, block_center_m(by), z) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}

/// The full 8x8 column grid for one region. Pure and deterministic.
pub fn generate_region(coords: RegionCoords, seed: u64, mats: Materials) -> Vec<(ColumnCoords, Column)> {
    let p = TerrainPipeline::new(seed);
    let sea = SEA_LEVEL as i32;
    let air = Block::new(BlockId::AIR);
    let stone = Block::new(mats.stone);
    let region_ox = coords.x * REGION_BLOCKS; // world block origin X
    let region_oz = coords.z * REGION_BLOCKS;

    let mut columns = Vec::with_capacity(REGION_CHUNKS * REGION_CHUNKS);
    for cx in 0..REGION_CHUNKS {
        for cz in 0..REGION_CHUNKS {
            // This Column covers CHUNK_BLOCKS x CHUNK_BLOCKS blocks.
            let col_ox = region_ox + (cx * CHUNK_BLOCKS) as i32;
            let col_oz = region_oz + (cz * CHUNK_BLOCKS) as i32;

            // Per-block-column surface heights within this footprint.
            let mut surfaces = [[SCAN_BOTTOM - 1; CHUNK_BLOCKS]; CHUNK_BLOCKS];
            let mut lo = SCAN_TOP;
            let mut hi = SCAN_BOTTOM - 1;
            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let s = surface_block_y(&p, col_ox + lx as i32, col_oz + lz as i32);
                    surfaces[lx][lz] = s;
                    lo = lo.min(s);
                    hi = hi.max(s.max(sea)); // include the water column up to sea level
                }
            }
            if hi < lo {
                // Fully air/ocean-floor below scan: emit an all-stone-below / air column.
                columns.push((ColumnCoords::new(cx, cz), Column::new(0, stone, air, vec![])));
                continue;
            }

            // Materialize block Ys [lo ..= hi]; below reads as stone sentinel,
            // above as air. Build per-block, chiseling the surface straddle.
            let base_sub_y = lo.div_euclid(CHUNK_BLOCKS as i32);
            let top_sub_y = hi.div_euclid(CHUNK_BLOCKS as i32);
            let n_sub = (top_sub_y - base_sub_y + 1) as usize;
            let mut chunks: Vec<Chunk> = (0..n_sub).map(|_| Chunk::default()).collect();

            for lx in 0..CHUNK_BLOCKS {
                for lz in 0..CHUNK_BLOCKS {
                    let wx = col_ox + lx as i32;
                    let wz = col_oz + lz as i32;
                    let surface = surfaces[lx][lz];
                    for by in lo..=hi {
                        let sub_i = (by.div_euclid(CHUNK_BLOCKS as i32) - base_sub_y) as usize;
                        let local_by = by.rem_euclid(CHUNK_BLOCKS as i32) as usize;
                        let bi = BlockIndex::from_xyz(lx, local_by, lz);
                        let chunk = &mut chunks[sub_i];
                        if by < surface {
                            // Solid interior/subsurface.
                            let depth = surface - by;
                            chunk.blocks[bi.0 as usize] = Block::new(mats.material_at(depth, by));
                        } else if by == surface {
                            // Surface straddle: chisel at voxel resolution (design §4 step 2).
                            let top_mat = mats.material_at(0, by);
                            place_straddle(chunk, bi, lx, local_by, lz, wx, by, wz, &p, top_mat);
                        } else if by <= sea {
                            // Above ground but below sea level -> water.
                            chunk.blocks[bi.0 as usize] = Block::new(mats.water);
                        }
                        // else: air (Chunk::default leaves it air).
                    }
                }
            }

            let subs: Vec<SubChunk> = chunks.into_iter().map(|c| SubChunk::Dense(Box::new(c))).collect();
            // `below = stone` so normalize() trims full-stone bottom slices to the
            // sentinel; `above = air` trims air tops.
            let column = Column::new(base_sub_y, stone, air, subs);
            columns.push((ColumnCoords::new(cx, cz), column));
        }
    }
    columns
}

/// Fill one block at voxel resolution from the density field: solid voxels below
/// the local surface become `top_mat`, the rest air. Produces a chiseled block.
#[allow(clippy::too_many_arguments)]
fn place_straddle(
    chunk: &mut Chunk,
    bi: BlockIndex,
    lx: usize,
    local_by: usize,
    lz: usize,
    wx: i32,
    by: i32,
    wz: i32,
    p: &TerrainPipeline,
    top_mat: BlockId,
) {
    let mut chisel = ChiselData::new(vec![top_mat]);
    let mut any = false;
    for vx in 0..BLOCK_VOXELS {
        for vy in 0..BLOCK_VOXELS {
            for vz in 0..BLOCK_VOXELS {
                // Meter coord of this voxel center: block index + (voxel + 0.5)/16.
                let mx = wx as f64 + (vx as f64 + 0.5) / BLOCK_VOXELS as f64;
                let my = by as f64 + (vy as f64 + 0.5) / BLOCK_VOXELS as f64;
                let mz = wz as f64 + (vz as f64 + 0.5) / BLOCK_VOXELS as f64;
                let solid = p.final_density(mx, my, mz) > 0.0;
                chisel.set(vx, vy, vz, solid, 0);
                any |= solid;
            }
        }
    }
    if any {
        // A chiseled block carries a non-air `Block` id + the chisel mask.
        chunk.blocks[bi.0 as usize] = Block::new(top_mat);
        chunk.chisel.insert(bi, chisel);
    }
    let _ = (lx, local_by, lz); // indices already folded into `bi`
}
```

> **Implementer note:** confirm the exact `ChiselData::set` signature and that a chiseled block needs both a non-air `blocks[bi]` entry and a `chisel` map entry (mirror `Chunk::flat_floor_with` in `crates/game/src/block.rs`). If `Chunk`'s `blocks`/`chisel` fields are private, add a `Chunk::from_blocks_chisel` constructor in `game` (small, in the same spirit as `flat_floor_with`) rather than reaching into private fields.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p worldgen --lib 2>&1 | tail -25`
Expected: PASS (all lib tests, including determinism + solid-ground).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/lib.rs crates/game/src/block.rs
git commit -m "feat(worldgen): density-driven region generation with smooth surfaces"
```

---

## Task 8: Thread seed + materials through the generator closures

**Files:**
- Modify: `crates/server/src/lib.rs` (~lines 238-249)
- Modify: `crates/client/src/local_server.rs` (~lines 30-40)
- Modify: `crates/worldgen/tests/region_load_cost.rs` (update to the new signature)
- Modify: `justfile` if a worldgen invocation string changed (verify only)

**Interfaces:**
- Consumes: `worldgen::generate_region(coords, seed, Materials)`, `worldgen::material::Materials`.
- Produces: server + offline client generating identical terrain from a shared seed.

- [ ] **Step 1: Update the server closure**

In `crates/server/src/lib.rs`, replace the `stone`/generator block (around the `let stone = registry.id_of("stone")…` and `Box::new(move |rc| worldgen::generate_region(rc, stone))` lines) with:

```rust
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    let materials = worldgen::material::Materials {
        stone: resolve("stone"),
        dirt: resolve("dirt"),
        grass: resolve("grass"),
        sand: resolve("sand"),
        water: resolve("water"),
    };
    const WORLD_SEED: u64 = 0x00D5_C0DE_2026;
    let mut manager = game::WorldManager::new(
        region_threads::ThreadRegionSpawner::default(),
        Box::new(move |rc| worldgen::generate_region(rc, WORLD_SEED, materials)),
        server_send,
        region_out_send,
    );
```

Ensure `worldgen::material::Materials` is accessible: in `crates/worldgen/src/lib.rs` the module is declared `mod material;`; change it to `pub mod material;` (and `pub mod pipeline;` so `SEA_LEVEL`/`TerrainPipeline` are reachable if needed). Leave `noise`/`spline`/`density` private.

- [ ] **Step 2: Update the offline client closure**

In `crates/client/src/local_server.rs`, mirror the same change (it currently resolves `stone` and calls `generate_region(rc, stone)`):

```rust
    let resolve = |name: &str| {
        registry
            .id_of(name)
            .unwrap_or_else(|| panic!("block manifest must define \"{name}\""))
    };
    let materials = worldgen::material::Materials {
        stone: resolve("stone"),
        dirt: resolve("dirt"),
        grass: resolve("grass"),
        sand: resolve("sand"),
        water: resolve("water"),
    };
    const WORLD_SEED: u64 = 0x00D5_C0DE_2026;
    // …then in the WorldManager::new generator arg:
        Box::new(move |rc| worldgen::generate_region(rc, WORLD_SEED, materials)),
```

- [ ] **Step 3: Update the region-load-cost test**

Open `crates/worldgen/tests/region_load_cost.rs`; wherever it calls `generate_region(rc, stone)`, construct a `Materials` and pass `(rc, seed, materials)`:

```rust
    let materials = worldgen::material::Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
    };
    let cols = worldgen::generate_region(rc, 1, materials);
```

- [ ] **Step 4: Build the whole workspace and run affected tests**

Run: `cargo build --workspace --bins 2>&1 | tail -20`
Expected: builds clean (server + client compile against the new signature).

Run: `cargo test -p worldgen 2>&1 | tail -20`
Expected: PASS (all worldgen tests, including the integration test).

- [ ] **Step 5: Commit**

```bash
git add crates/server/src/lib.rs crates/client/src/local_server.rs crates/worldgen/tests/region_load_cost.rs crates/worldgen/src/lib.rs
git commit -m "feat(worldgen): thread world seed + materials through generators"
```

---

## Task 9: Region-gen bench

**Files:**
- Modify: `crates/worldgen/src/main.rs`
- Modify: `justfile` (add a `worldgen-bench` recipe if none exists)

**Interfaces:**
- Consumes: `worldgen::generate_region`, `worldgen::material::Materials`.

- [ ] **Step 1: Update the bench binary**

Rewrite `crates/worldgen/src/main.rs` to time a full multi-region generation with the new signature (keep whatever region count / reporting the old bench used; the key is exercising coarse-find + voxel-smooth):

```rust
use std::time::Instant;
use worldgen::material::Materials;

fn main() {
    let materials = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
    };
    let seed = 0x00D5_C0DE_2026;
    let n = 8; // regions per axis
    let start = Instant::now();
    let mut total_cols = 0usize;
    for x in 0..n {
        for z in 0..n {
            let cols = worldgen::generate_region(game::RegionCoords::new(x, z), seed, materials);
            total_cols += cols.len();
        }
    }
    let dt = start.elapsed();
    println!(
        "generated {} regions ({} columns) in {:?} ({:.2} regions/s)",
        n * n,
        total_cols,
        dt,
        (n * n) as f64 / dt.as_secs_f64()
    );
}
```

- [ ] **Step 2: Add a justfile recipe**

Add to `justfile` (near the other build/test recipes):

```
# Time full region generation (coarse-find + voxel-smooth surfaces)
worldgen-bench:
    {{cargo}} run --release -p worldgen --bin worldgen
```

- [ ] **Step 3: Run the bench**

Run: `cargo run --release -p worldgen --bin worldgen 2>&1 | tail -5`
Expected: prints a "generated N regions … in …" line and exits 0.

- [ ] **Step 4: Full test sweep**

Run: `cargo test -p worldgen && cargo test -p game 2>&1 | tail -15`
Expected: worldgen and game suites PASS (game determinism unaffected).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/main.rs justfile
git commit -m "build(worldgen): region-gen bench + just recipe"
```

---

## Final verification

- [ ] Run `cargo clippy -p worldgen 2>&1 | tail -20` — no warnings (delete any dead scaffolding noted in Task 4).
- [ ] Run `just build` (or `cargo build --workspace --bins`) — clean.
- [ ] Run `just run` and visually confirm terrain generates (continents/coasts, not flat floors). This is the end-to-end check that the pipeline reads as terrain in the client.
- [ ] Confirm determinism: `cargo test -p worldgen deterministic` passes, and note in the commit that the same `f64` path runs on wasm32 (offline client).

---

## Self-review notes (coverage against the spec)

- Spec §1 module layout → Tasks 1-7 create every module (`noise`/`spline`/`density`/`pipeline`/`material`/`lib`).
- Spec §2 engine → Tasks 1-3. §3 pipeline → Task 4. §4 coarse-find+voxel-smooth → Task 7. §5 materials → Task 5 (+ Task 6 manifest). §6 determinism → Global Constraints + Task 7 determinism tests + Task 9 wasm note. §7 seed threading → Task 8. §8 testing → per-task tests + Task 9 bench. §9 deferred features → not implemented, by design.
- Open items from the spec deliberately left as implementer calls (world-frame numbers, cache wrappers, material thicknesses, water id) are surfaced inline in Tasks 4/5/6/7 rather than blocking.
```
