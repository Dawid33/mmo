# TerrainPipeline::column Extreme Optimization Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut `TerrainPipeline::column()`'s share of `generate_region` from ~860ms (96% of 894ms/region) to under ~5ms by eliminating redundant noise evaluation and replacing per-voxel-column profile sampling with lattice interpolation.

**Architecture:** Three bit-exact refactors (flat SoA splines, a fused `column()` that samples each noise once, branch-free precomputed noise internals), then one deliberately output-changing change (a `ProfileGrid` — sample `column()` on a 4-block lattice per region and bilinearly interpolate everywhere `generate_region` needs a profile). The `DensityFn` graph vocabulary stays as the authoring/spec language; tests pin the fused path bit-exactly to the graph.

**Tech Stack:** Rust, `crates/worldgen` only (plus justfile recipes). No new dependencies.

## Measured Baseline (2026-07-10, release, this machine)

From `cargo run -p worldgen --release --example column_bench`:

| Metric | Value |
|---|---|
| `column()` | 817.6 ns/call |
| `density_at()` | 2.2 ns/call |
| `final_density()` | 827.0 ns/call |
| `generate_region` | 894 ms |
| `column()` share | ~96% (1.05M calls: 4,096 surface-scan + 1,048,576 straddle = 4096×256) |

**Cost anatomy of one `column()` call:** the three sub-graphs (`offset_g`, `factor_g`, `jaggedness_g`) are independent trees, and `factor`/`jaggedness` each embed a full copy of the `offset` subtree. Per call that's **42 Perlin octave samples where only 16 are unique** (continents 7, erosion 6, ridges 3) — a 2.6× pure redundancy — plus `2f64.powi()` recomputed per octave per sample, boxed-tree pointer chasing, and boxed recursive spline nodes.

## Results (2026-07-10, release, this machine — all 6 tasks landed)

| Metric | Baseline | Final | Factor |
|---|---|---|---|
| `column()` | 817.6 ns/call | 198.7 ns/call | 4.1× |
| `column()` calls per `generate_region` | 1,052,672 | 289 (17×17 lattice) | 3642× |
| `generate_region` | 894 ms | 65.5 ms | 13.6× |
| `density_at()` | 2.2 ns | 2.0 ns | — |

Per-task progression: `column()` ~818ns → ~217ns after the fusion (Task 3, 42→16 Perlin samples) → ~173–199ns after the noise internals (Task 4; the spread is thermal/turbo variance across runs, see Task 1 note). `generate_region` 894ms → 283ms (fusion) → ~65ms (ProfileGrid — 289 lattice `column()` calls instead of 1.05M, bilinear interpolation for the rest). The remaining ~65ms is block/chisel filling and allocation, not density evaluation (modeled residual column() cost: 0.06ms = 0.1%).

Tasks 2–4 were bit-exact (world output byte-identical, pinned by oracle tests against the `DensityFn` graphs). Task 5 changed world output deterministically (sub-voxel smoothing); the spawn seed was re-verified — spawn surface y = 70 (sea 64), `WORLD_SEED` unchanged. Live GUI smoke (region-boundary seams, terrain character) pending.

**Why lattice interpolation is safe:** the highest-frequency noise content is ~123–128 m wavelength (continents: 2⁻⁴ top octave × 0.13 xz_scale ≈ 1/123 m⁻¹; erosion: 2⁻⁵ × 0.25 = 1/128; ridges: 2⁻⁶ × 0.5 = 1/128). A 4 m lattice oversamples the field ~30× past Nyquist; bilinear error is O((cell/wavelength)²) ≈ 10⁻³ of amplitude — sub-voxel in height terms.

## Global Constraints

- Pure `f64`: only `+ - * /`, `floor`, integer `powi` — no transcendentals, no FMA, no `f32` in the field math (cross-machine determinism for rollback netcode).
- `generate_region` must remain a pure deterministic function of `(coords, seed)` — same output on every machine.
- Tasks 1–4 must be **bit-exact** (world output unchanged, byte-for-byte). Task 5 **changes world output** (deterministically) and requires the seed re-check step included in it.
- `ColumnProfile::density_at(y)` must keep reproducing the density graph bit-for-bit (existing test `column_profile_matches_graph_bit_exact` stays green throughout).
- Keep the justfile in sync with any new/changed commands (CLAUDE.md rule).
- Tag any new test slower than 100ms with `#[ignore = "slow: >100ms — run via just test-full"]`.
- Fast loop is `just test`; final gate is `just test-full`.

---

### Task 1: Benchmark harness + justfile recipes

The micro-benchmark `crates/worldgen/examples/column_bench.rs` already exists in the working tree (uncommitted). Commit it and wire up the missing recipes: `region_load_cost.rs`'s doc header says `just bench-region`, but no such recipe exists.

**Files:**
- Commit (already written): `crates/worldgen/examples/column_bench.rs`
- Modify: `justfile` (next to the existing `worldgen-bench` / `collider-bench` recipes, ~line 150)

**Interfaces:**
- Produces: `just bench-column` and `just bench-region` — the before/after measurement commands every later task uses.

- [ ] **Step 1: Add the justfile recipes**

Insert after the existing `collider-bench` recipe:

```make
# Micro-benchmark of TerrainPipeline::column and its share of generate_region.
bench-column:
    {{cargo}} run --release -p worldgen --example column_bench

# Headless region-load cost breakdown (gen/derive/serde/mesh).
bench-region:
    {{cargo}} test -p worldgen --release --test region_load_cost -- --ignored --nocapture
```

- [ ] **Step 2: Verify both recipes run**

Run: `just bench-column` — expect the four baseline lines (column ns/call, density_at, final_density, generate_region + share).
Run: `just bench-region` — expect the `--- region load cost ---` table.

- [ ] **Step 3: Commit**

```bash
git add crates/worldgen/examples/column_bench.rs justfile
git commit -m "perf(worldgen): column() micro-bench + bench-column/bench-region recipes

Baseline: column()=818ns, 1.05M calls/region = 96% of generate_region (894ms)."
```

---

### Task 2: FlatSpline — SoA spline evaluation (bit-exact)

Every spline the pipeline uses has only `Spline::Const` control-point values, but `Spline::eval` walks `Box<Spline>` per point. Add a flattened form with parallel arrays and the identical Hermite arithmetic.

**Files:**
- Modify: `crates/worldgen/src/spline.rs`

**Interfaces:**
- Consumes: `Spline`, `Point` (existing).
- Produces: `pub struct FlatSpline` with `pub fn from_spline(s: &Spline) -> FlatSpline` (panics on non-`Const` control values — none exist in this pipeline) and `pub fn eval(&self, coord: f64) -> f64` bit-identical to `Spline::eval`.

- [ ] **Step 1: Write the failing test** (append to `spline.rs` tests)

```rust
#[test]
fn flat_spline_matches_boxed_bit_exact() {
    let s = Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(-0.9), 0.0),
            Spline::point(-0.2, Spline::constant(-0.3), 0.5),
            Spline::point(0.0, Spline::constant(0.02), 0.6),
            Spline::point(0.6, Spline::constant(0.9), 1.0),
            Spline::point(1.2, Spline::constant(1.8), 0.5),
        ],
    };
    let f = FlatSpline::from_spline(&s);
    // Dense sweep past both ends, across every segment, and exactly on knots.
    let mut c = -2.0;
    while c <= 2.0 {
        assert_eq!(f.eval(c), s.eval(c), "mismatch at coord {c}");
        c += 0.001953125; // 2^-9: exact in f64, no drift
    }
    for knot in [-1.0, -0.2, 0.0, 0.6, 1.2] {
        assert_eq!(f.eval(knot), s.eval(knot));
    }
}
```

- [ ] **Step 2: Run it, verify it fails to compile** (no `FlatSpline`)

Run: `cargo test -p worldgen flat_spline_matches -- --nocapture`
Expected: compile error `cannot find type FlatSpline`.

- [ ] **Step 3: Implement `FlatSpline`** (append to `spline.rs`)

```rust
/// `Spline::Curve` flattened to parallel arrays for the common case where
/// every control-point value is `Const`. Arithmetic is copied verbatim from
/// `Spline::eval` so results are bit-identical.
pub struct FlatSpline {
    locations: Box<[f64]>,
    values: Box<[f64]>,
    derivatives: Box<[f64]>,
}

impl FlatSpline {
    /// Panics if any control-point value is not `Spline::Const` — the v1
    /// pipeline never nests curves; revisit if a nested spline ever lands.
    pub fn from_spline(s: &Spline) -> FlatSpline {
        let points = match s {
            Spline::Const(v) => {
                return FlatSpline {
                    locations: Box::new([0.0]),
                    values: Box::new([*v]),
                    derivatives: Box::new([0.0]),
                }
            }
            Spline::Curve { points } => points,
        };
        let values = points
            .iter()
            .map(|p| match p.value.as_ref() {
                Spline::Const(v) => *v,
                Spline::Curve { .. } => panic!("FlatSpline: nested curve control value"),
            })
            .collect();
        FlatSpline {
            locations: points.iter().map(|p| p.location).collect(),
            values,
            derivatives: points.iter().map(|p| p.derivative).collect(),
        }
    }

    pub fn eval(&self, coord: f64) -> f64 {
        let (locs, vals, ders) = (&self.locations, &self.values, &self.derivatives);
        let n = locs.len();
        if coord <= locs[0] {
            return vals[0] + ders[0] * (coord - locs[0]);
        }
        if coord >= locs[n - 1] {
            return vals[n - 1] + ders[n - 1] * (coord - locs[n - 1]);
        }
        let mut i = 0;
        while coord >= locs[i + 1] {
            i += 1;
        }
        let (l0, l1) = (locs[i], locs[i + 1]);
        let (k0, k1) = (vals[i], vals[i + 1]);
        let span = l1 - l0;
        let t = (coord - l0) / span;
        let m0 = ders[i] * span;
        let m1 = ders[i + 1] * span;
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * k0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * k1
            + (t3 - t2) * m1
    }
}
```

Note the single-`Const` case returns a 1-point flat spline; `Spline::eval` on `Const` ignores the coord entirely, and a 1-point curve here hits the `coord <= locs[0]` or `coord >= locs[0]` arm returning `v + 0.0 * (coord)` = `v` exactly, so it's still bit-identical.

- [ ] **Step 4: Run the test, verify it passes**

Run: `cargo test -p worldgen flat_spline_matches`
Expected: PASS. Also run `cargo test -p worldgen` (whole crate stays green).

- [ ] **Step 5: Commit**

```bash
git add crates/worldgen/src/spline.rs
git commit -m "perf(worldgen): FlatSpline SoA evaluation, bit-identical to boxed Spline"
```

---

### Task 3: Fused `column()` — one sample per noise (bit-exact, ~2.5–3×)

`column()` currently evaluates three independent `DensityFn` trees; the `offset` subtree is embedded in all three, so continents (7 octaves) and erosion (6 octaves) are each sampled **three times**. Fuse: sample each noise once, apply the splines directly, mirroring the graph's exact f64 op order. The graph builders stay (they are the spec of the terrain shape and the test oracle); `TerrainPipeline` stops evaluating them at runtime.

**Files:**
- Modify: `crates/worldgen/src/pipeline.rs`

**Interfaces:**
- Consumes: `FlatSpline::from_spline`, `FlatSpline::eval` (Task 2); existing `NoiseSet`, `OctaveNoise::sample2`.
- Produces: `TerrainPipeline::column(&self, x: f64, z: f64) -> ColumnProfile` and `final_density(&self, x, y, z) -> f64` — same signatures, bit-identical results. Internal `struct TerrainPipeline { noises, continents_curve, offset_base, erosion_mod, factor_curve, jaggedness_gate }`.

- [ ] **Step 1: Extract the spline definitions so graph and fused path share one source of truth**

In `pipeline.rs`, pull each inline `Spline::Curve { … }` out of `raw_continents()` / `offset()` / `factor()` / `jaggedness()` into named private functions, and make the graph builders use them (identical points — cut/paste, no value changes):

```rust
fn continents_curve_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(0.4, Spline::constant(0.6), 1.0),
            Spline::point(1.0, Spline::constant(1.2), 0.0),
        ],
    }
}

fn erosion_mod_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(1.0), 0.0),
            Spline::point(0.2, Spline::constant(0.5), 0.0),
            Spline::point(1.0, Spline::constant(0.15), 0.0),
        ],
    }
}

fn offset_base_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(-0.9), 0.0),
            Spline::point(-0.2, Spline::constant(-0.3), 0.5),
            Spline::point(0.0, Spline::constant(0.02), 0.6),
            Spline::point(0.6, Spline::constant(0.9), 1.0),
            Spline::point(1.2, Spline::constant(1.8), 0.5),
        ],
    }
}

fn factor_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(-1.0, Spline::constant(0.6), 0.0),
            Spline::point(0.0, Spline::constant(1.2), 0.0),
            Spline::point(1.0, Spline::constant(3.0), 0.0),
        ],
    }
}

fn jaggedness_gate_spline() -> Spline {
    Spline::Curve {
        points: vec![
            Spline::point(0.0, Spline::constant(0.0), 0.0),
            Spline::point(1.0, Spline::constant(0.6), 0.0),
        ],
    }
}
```

Also name the noise scales (they're needed twice now):

```rust
const XZ_SCALE_CONTINENTS: f64 = 0.13;
const XZ_SCALE_EROSION: f64 = 0.25;
const XZ_SCALE_RIDGES: f64 = 0.5;
```

and use them in `continentalness()` / `erosion()` / `ridges()`.

Run: `cargo test -p worldgen` — all green (pure extraction, no behavior change).

- [ ] **Step 2: Write the oracle test pinning fused == graph, bit-exact** (append to `pipeline.rs` tests)

This passes both before and after the fusion — its job is to make the swap provably bit-exact and to keep guarding the fused path forever.

```rust
#[test]
fn fused_column_matches_density_graphs_bit_exact() {
    let p = TerrainPipeline::new(worldgen_test_seed());
    let noises = pipeline_noises(worldgen_test_seed());
    let (graph, offset_g, factor_g, jaggedness_g) =
        (final_density_graph(), offset(), factor(), jaggedness());
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
```

This needs two extractions in `pipeline.rs` (module-private; the test lives in the same module):

```rust
fn worldgen_test_seed() -> u64 { 0x00D5_C0DE_8D78 } // test helper, next to the test

/// Noise construction shared by TerrainPipeline::new and the oracle test.
fn pipeline_noises(seed: u64) -> NoiseSet {
    NoiseSet(vec![
        OctaveNoise::new(seed ^ 0x00C0, -10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
        OctaveNoise::new(seed ^ 0x00E0, -10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
        OctaveNoise::new(seed ^ 0x00D0, -8, vec![1.0, 2.0, 1.0]),
    ])
}
```

with `TerrainPipeline::new` changed to call `pipeline_noises(seed)` instead of inlining the vec.

Run: `cargo test -p worldgen fused_column_matches` — PASS (current `column()` is graph-backed, so this is the pre-fusion sanity run). If this test exceeds 100ms, tag it `#[ignore = "slow: >100ms — run via just test-full"]` and use `just test-full` for the gates below.

- [ ] **Step 3: Swap the implementation**

Replace the `TerrainPipeline` struct and impl:

```rust
pub struct TerrainPipeline {
    noises: NoiseSet,
    continents_curve: FlatSpline,
    offset_base: FlatSpline,
    erosion_mod: FlatSpline,
    factor_curve: FlatSpline,
    jaggedness_gate: FlatSpline,
}

impl TerrainPipeline {
    pub fn new(seed: u64) -> Self {
        Self {
            noises: pipeline_noises(seed),
            continents_curve: FlatSpline::from_spline(&continents_curve_spline()),
            offset_base: FlatSpline::from_spline(&offset_base_spline()),
            erosion_mod: FlatSpline::from_spline(&erosion_mod_spline()),
            factor_curve: FlatSpline::from_spline(&factor_spline()),
            jaggedness_gate: FlatSpline::from_spline(&jaggedness_gate_spline()),
        }
    }

    pub fn final_density(&self, x: f64, y: f64, z: f64) -> f64 {
        // ColumnProfile::density_at reproduces final_density_graph bit-for-bit
        // (see `column_profile_matches_graph_bit_exact`), so the 3D entry point
        // is just the 2D profile plus the cheap y arithmetic.
        self.column(x, z).density_at(y)
    }

    /// Fused evaluation of the offset/factor/jaggedness graphs: each noise is
    /// sampled ONCE (the graphs share the whole `offset` subtree, which the
    /// tree-walk evaluator recomputed three times). Every f64 op below mirrors
    /// the graph's op order exactly — guarded bit-for-bit by
    /// `fused_column_matches_density_graphs_bit_exact`.
    pub fn column(&self, x: f64, z: f64) -> ColumnProfile {
        let (x, z) = (x + SAMPLE_OFFSET_X_M, z + SAMPLE_OFFSET_Z_M);
        let n = &self.noises.0;
        let cont = n[N_CONTINENTS].sample2(x * XZ_SCALE_CONTINENTS, z * XZ_SCALE_CONTINENTS);
        let ero = n[N_EROSION].sample2(x * XZ_SCALE_EROSION, z * XZ_SCALE_EROSION);
        let ridge = n[N_RIDGES].sample2(x * XZ_SCALE_RIDGES, z * XZ_SCALE_RIDGES);

        // raw_continents = OCEAN_OFFSET + curve(|continents|)   (Add(Const, SplineOf))
        let raw = OCEAN_OFFSET + self.continents_curve.eval(cont.abs());
        // offset = clamp(base(raw) * erosion_mod(erosion))
        let offset = (self.offset_base.eval(raw) * self.erosion_mod.eval(ero))
            .clamp(MIN_OFFSET, MAX_OFFSET);
        let factor = self.factor_curve.eval(offset);
        let jaggedness = self.jaggedness_gate.eval(offset) * ridge.abs();

        ColumnProfile { offset_h: offset * HEIGHT_SCALE, factor, jaggedness }
    }
}
```

Delete the now-unused `graph`/`offset_g`/`factor_g`/`jaggedness_g` fields. The graph builder fns (`raw_continents`, `offset`, `factor`, `jaggedness`, `final_density_graph`) stay — mark them `#[cfg(test)]` if the compiler flags them dead, with a comment pointing at the oracle test and the caves/RangeChoice roadmap note in `density.rs`.

- [ ] **Step 4: Run the full gate**

Run: `cargo test -p worldgen`
Expected: ALL PASS — in particular `fused_column_matches_density_graphs_bit_exact` and `column_profile_matches_graph_bit_exact` (bit-exactness proof) and `lib.rs`'s `deterministic_same_seed`.

- [ ] **Step 5: Measure**

Run: `just bench-column`
Expected: `column()` drops from ~818ns to roughly 280–350ns (42→16 Perlin samples plus tree-walk removal); `generate_region` to roughly 300–400ms. Record the numbers in the commit message.

- [ ] **Step 6: Commit**

```bash
git add crates/worldgen/src/pipeline.rs
git commit -m "perf(worldgen): fuse column() — sample each noise once, FlatSplines

42 -> 16 Perlin samples/column; bit-exact vs the density graphs (oracle test).
column(): 818ns -> <measured>; generate_region: 894ms -> <measured>."
```

---

### Task 4: OctaveNoise internals — frequency table + branch-free gradients (bit-exact)

`OctaveNoise::sample2` recomputes `2f64.powi(first_octave + i)` per octave per call; powers of two are exact in f64, so a precomputed table is bit-identical. `grad2`'s 8-way `match` becomes a table dot-product — also bit-identical for this call site (inputs are never `-0.0`; sign-of-zero on the *output* can differ at exact-lattice points, which `==`/`assert_eq!` on f64 treats as equal, and which cannot change any `density > 0` comparison or serialized world byte).

**Files:**
- Modify: `crates/worldgen/src/noise.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: same public API (`OctaveNoise::new(seed, first_octave, amplitudes)`, `sample2`), bit-identical outputs.

- [ ] **Step 1: Write the failing equivalence test** (append to `noise.rs` tests)

The reference implementations are frozen copies of today's code, kept inside the test so the equivalence is checked against the *old* arithmetic forever:

```rust
#[test]
fn optimized_noise_matches_reference_bit_exact() {
    // Frozen pre-optimization reference of grad2 + octave frequency math.
    fn ref_grad2(hash: u8, x: f64, y: f64) -> f64 {
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
    for h in 0u8..=255 {
        let mut s = h as u64 * 0x9E37_79B9 + 1;
        for _ in 0..64 {
            let a = splitmix64(&mut s);
            let x = (a >> 11) as f64 / (1u64 << 53) as f64; // xf domain [0,1)
            let y = (splitmix64(&mut s) >> 11) as f64 / (1u64 << 53) as f64;
            for (gx, gy) in [(x, y), (x - 1.0, y), (x, y - 1.0), (x - 1.0, y - 1.0), (0.0, y), (x, 0.0)] {
                assert_eq!(grad2(h, gx, gy), ref_grad2(h, gx, gy), "h={h} x={gx} y={gy}");
            }
        }
    }

    // Octave frequency table vs on-the-fly powi across the pipeline's shapes.
    for (first, amps) in [
        (-10, vec![1.75, 1.0, 2.0, 3.0, 2.0, 2.0, 1.0]),
        (-10, vec![2.0, 1.75, 1.5, 1.5, 1.3, 1.0]),
        (-8, vec![1.0, 2.0, 1.0]),
    ] {
        let o = OctaveNoise::new(0xD5, first, amps.clone());
        let mut s = 7u64;
        for _ in 0..2048 {
            let x = ((splitmix64(&mut s) >> 40) as f64) * 0.37 - 3000.0;
            let z = ((splitmix64(&mut s) >> 40) as f64) * 0.53 - 2000.0;
            // Reference: recompute with explicit powi, old-style.
            let mut acc = 0.0;
            for (i, oct) in o.octaves.iter().enumerate() {
                let freq = 2f64.powi(first + i as i32);
                acc += o.amplitudes[i] * oct.sample2(x * freq, z * freq);
            }
            assert_eq!(o.sample2(x, z), acc / o.norm, "at ({x}, {z})");
        }
    }
}
```

(`octaves`/`amplitudes`/`norm` are private fields — the test lives in the same module, so access is fine.)

- [ ] **Step 2: Run it — grad half passes (it's the current impl), octave half passes too; this is the pre-change pin**

Run: `cargo test -p worldgen optimized_noise_matches`
Expected: PASS (pin established). It "fails" in the TDD sense only after a wrong optimization — that's its job.

- [ ] **Step 3: Apply the optimizations**

```rust
/// 8-direction 2D gradient dot, chosen by the low 3 bits of the hash.
/// Table-dot form of the old 8-way match; multiplies by ±1.0/0.0 are exact,
/// so results are bit-identical (sign-of-zero aside, which f64 == ignores).
const GRAD2: [(f64, f64); 8] = [
    (1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0),
    (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0),
];

fn grad2(hash: u8, x: f64, y: f64) -> f64 {
    let (gx, gy) = GRAD2[(hash & 7) as usize];
    gx * x + gy * y
}
```

```rust
pub struct OctaveNoise {
    octaves: Vec<PerlinNoise>,
    amplitudes: Vec<f64>,
    /// 2^(first_octave + i) — exact powers of two, precomputed from `new()`.
    freqs: Vec<f64>,
    norm: f64,
}

impl OctaveNoise {
    pub fn new(seed: u64, first_octave: i32, amplitudes: Vec<f64>) -> Self {
        let mut state = seed;
        let octaves = (0..amplitudes.len())
            .map(|_| PerlinNoise::new(splitmix64(&mut state)))
            .collect();
        let freqs = (0..amplitudes.len())
            .map(|i| 2f64.powi(first_octave + i as i32))
            .collect();
        let norm = amplitudes.iter().sum::<f64>().max(1.0);
        Self { octaves, amplitudes, freqs, norm }
    }

    pub fn sample2(&self, x: f64, z: f64) -> f64 {
        let mut acc = 0.0;
        for (i, oct) in self.octaves.iter().enumerate() {
            let freq = self.freqs[i];
            acc += self.amplitudes[i] * oct.sample2(x * freq, z * freq);
        }
        acc / self.norm
    }
}
```

(The test's reference loop uses `first + i as i32` directly, so keep the test's own `first` binding — the struct no longer stores `first_octave`.)

Do **not** fold `/ self.norm` into the amplitudes: `(a·s₁ + b·s₂)/n ≠ (a/n)·s₁ + (b/n)·s₂` in f64 — that would change world output for a negligible win.

- [ ] **Step 4: Run the gates**

Run: `cargo test -p worldgen`
Expected: ALL PASS, including both bit-exactness pins.

- [ ] **Step 5: Measure**

Run: `just bench-column`
Expected: a further ~10–25% off `column()` (powi loops and branch misprediction removed). Record numbers.

- [ ] **Step 6: Commit**

```bash
git add crates/worldgen/src/noise.rs
git commit -m "perf(worldgen): precompute octave freqs, branch-free grad table (bit-exact)

column(): <before> -> <measured>."
```

---

### Task 5: ProfileGrid — lattice sampling + bilinear interpolation (CHANGES WORLD OUTPUT)

The straddle pass calls `column()` for every 1/16-m voxel column (256 per surface block, 1.05M per region) and the surface scan once per block column (4,096), but the field's shortest wavelength is ~123 m. Sample `column()` on a 4-block lattice (17×17 = 289 calls/region) and bilinearly interpolate the three profile fields everywhere else. Deterministic (pure f64, fixed op order, corners are pure functions of world coordinates — so adjacent regions agree along shared edges), but the produced world differs slightly from today's: this task includes the spawn-seed re-check.

**Files:**
- Modify: `crates/worldgen/src/pipeline.rs` (add `ProfileGrid`, `TerrainPipeline::profile_grid`)
- Modify: `crates/worldgen/src/lib.rs` (`generate_region`, `surface_block_y`, `place_straddle` take the grid)

**Interfaces:**
- Consumes: `TerrainPipeline::column` (Task 3), `ColumnProfile { offset_h, factor, jaggedness }` (make these three fields `pub(crate)` or add a constructor — the grid builds `ColumnProfile` values).
- Produces: `pub const PROFILE_LATTICE_BLOCKS: usize = 4;` and

```rust
impl TerrainPipeline {
    /// Sample column() on a (span/K + 1)^2 corner lattice starting at world
    /// meters (origin_x, origin_z), K = PROFILE_LATTICE_BLOCKS meters.
    pub fn profile_grid(&self, origin_x: f64, origin_z: f64, span_m: f64) -> ProfileGrid;
}
pub struct ProfileGrid { /* origin, step, n, SoA fields */ }
impl ProfileGrid {
    /// Bilinear profile at world meters (x, z); must lie within the sampled span.
    pub fn profile_at(&self, x: f64, z: f64) -> ColumnProfile;
}
```

- [ ] **Step 1: Write the failing tests** (append to `pipeline.rs` tests)

```rust
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
    let a = p.profile_grid(0.0, 0.0, 64.0);   // region covering x in [0, 64]
    let b = p.profile_grid(64.0, 0.0, 64.0);  // neighbor covering x in [64, 128]
    for j in 0..=64 {
        let z = j as f64;
        let (pa, pb) = (a.profile_at(64.0, z), b.profile_at(64.0, z));
        assert_eq!(pa.offset_h, pb.offset_h);
        assert_eq!(pa.factor, pb.factor);
        assert_eq!(pa.jaggedness, pb.jaggedness);
    }
}
```

with a tiny local helper (test-only RNG so we don't depend on noise internals):

```rust
fn splitmix_for_test(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
```

- [ ] **Step 2: Run them, verify they fail to compile** (no `ProfileGrid`)

Run: `cargo test -p worldgen profile_grid`
Expected: compile error `cannot find profile_grid`.

- [ ] **Step 3: Implement `ProfileGrid`** (in `pipeline.rs`)

```rust
/// Lattice spacing (blocks == meters) for per-region profile interpolation.
/// The density field's shortest xz wavelength is ~123 m (continents top
/// octave 2^-4 * 0.13; erosion 2^-5 * 0.25; ridges 2^-6 * 0.5 — all ~1/128),
/// so 4 m sampling is ~30x past Nyquist. Raise for speed, lower toward 1 for
/// fidelity; must divide the region block span (64).
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
}

impl TerrainPipeline {
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
        ProfileGrid { origin_x, origin_z, step, n, offset_h, factor, jaggedness }
    }
}

impl ProfileGrid {
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
        fn lerp(t: f64, a: f64, b: f64) -> f64 {
            a + t * (b - a)
        }
        let bilerp = |f: &[f64]| lerp(tz, lerp(tx, f[a], f[b]), lerp(tx, f[c], f[d]));
        ColumnProfile {
            offset_h: bilerp(&self.offset_h),
            factor: bilerp(&self.factor),
            jaggedness: bilerp(&self.jaggedness),
        }
    }
}
```

Note corner queries: at `tx == 0.0`, `lerp(0, a, b) = a + 0*(b-a) = a` exactly, so corner bit-exactness holds without special-casing (guarded by the test).

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p worldgen profile_grid`
Expected: PASS.

- [ ] **Step 5: Route `generate_region` through the grid** (in `lib.rs`)

```rust
fn surface_block_y(grid: &ProfileGrid, wx: i32, wz: i32) -> i32 {
    let prof = grid.profile_at(block_center_m(wx), block_center_m(wz));
    let mut by = SCAN_TOP;
    while by >= SCAN_BOTTOM {
        if prof.density_at(block_center_m(by)) > 0.0 {
            return by;
        }
        by -= 1;
    }
    SCAN_BOTTOM - 1
}
```

In `generate_region`, right after constructing the pipeline, build one grid covering the whole region footprint:

```rust
let p = TerrainPipeline::new(seed);
let grid = p.profile_grid(region_ox as f64, region_oz as f64, REGION_BLOCKS as f64);
```

(move the `region_ox`/`region_oz` lets above this), pass `&grid` to `surface_block_y` and `place_straddle` in place of `&p`, and in `place_straddle` replace `let prof = p.column(mx, mz);` with `let prof = grid.profile_at(mx, mz);` (parameter becomes `grid: &ProfileGrid`). Import `ProfileGrid` alongside `TerrainPipeline` in `lib.rs`'s use of `crate::pipeline`. `ProfileGrid` and `PROFILE_LATTICE_BLOCKS` must be exported from `pipeline` (they are `pub` already; `pipeline` is a `pub mod`).

Voxel centers and block centers all lie strictly inside the region span (max = 63 + 15.5/16 < 64), so every query is in-lattice.

- [ ] **Step 6: Full test gate**

Run: `cargo test -p worldgen && just test`
Expected: ALL PASS. `deterministic_same_seed` / `different_seed_differs` / `columns_contain_solid_ground` are self-consistency checks and survive the output change. If any client/game test pins a specific generated terrain byte-for-byte (check `crates/game/tests/spawn_surface.rs` and client convergence harnesses), update its expectation — flag in the commit message.

- [ ] **Step 7: Re-check the spawn seed (world output changed)**

Run: `cargo run -p worldgen --example seed_scan --release`
Expected output starts with `current WORLD_SEED 0x0000D5C0DE8D78: spawn surface y = <y> (sea = 64)`. The spawn criteria are `sea+4 <= y <= sea+40` (dry, not a mountain top). The lattice moves `offset_h` by well under a meter, so the current seed should still qualify — if `y` falls outside the band, pick the first new candidate the scan prints and update `WORLD_SEED` in `crates/worldgen/src/lib.rs`.

- [ ] **Step 8: Measure**

Run: `just bench-column && just bench-region`
Expected: `generate_region` drops to roughly 35–60ms (289 `column()` calls instead of 1.05M; the remaining time is block/chisel filling, which this plan doesn't touch). Record all numbers.

- [ ] **Step 9: Commit**

```bash
git add crates/worldgen/src/pipeline.rs crates/worldgen/src/lib.rs
git commit -m "perf(worldgen): ProfileGrid — 4m lattice + bilinear profiles for generate_region

column() calls/region: 1.05M -> 289. generate_region: <before> -> <measured>.
CHANGES WORLD OUTPUT (sub-voxel smoothing of <4m detail; min noise wavelength
~123m). Seed re-checked via examples/seed_scan.rs: spawn y = <y>."
```

---

### Task 6: Final verification + live smoke

**Files:** none new (measurement + docs sync only).

- [ ] **Step 1: Full suite**

Run: `just test-full`
Expected: ALL PASS.

- [ ] **Step 2: Record the final before/after table**

Run `just bench-column` and `just bench-region`; append a dated results table to this plan document under "Results" (baseline table above is the "before").

- [ ] **Step 3: Live smoke**

Run `just run` (server + client), fly toward unloaded regions and across a region boundary; verify no terrain seams at region edges and no visible change in terrain character. On a wasm-capable day, also sanity-check the browser build's worldgen-ring hitching (it calls `generate_region` on the async pool — the 15–25× gen speedup lands there automatically).

- [ ] **Step 4: Commit any doc/justfile stragglers**

```bash
git add docs/superpowers/plans/2026-07-10-column-profile-optimization.md
git commit -m "docs(worldgen): column optimization results"
```

---

## Deferred / Rejected (researched, deliberately not in this plan)

- **Analytic surface solve.** `density_at` is monotone-decreasing piecewise-linear in y (factor > 0 always), so `surface_block_y`'s ~250-iteration scan could be O(1) algebra. Measured cost after Task 5: ~4096 × 385 × 2.2ns ≈ 3.5ms/region — do it only if the scan ever shows up in a profile again.
- **Tape/SSA compilation of `DensityFn` + batched (SIMD) evaluation + interval pruning** (the fidget/libfive architecture; Minecraft's `interpolated`/`flat_cache` caching is the same idea specialized). Overkill while the graph is 2D-only and fused by hand — **the trigger is the caves roadmap**: `min(terrain, caves)` with 3D noise makes per-voxel 3D evaluation return, and then a flattened tape with hash-consing CSE, per-op batch loops over point buffers (auto-vectorizable; IEEE-exact ops stay deterministic, incl. wasm SIMD128 `f64x2`), and interval evaluation to prune inactive `Min`/`Max`/`Clamp`/`RangeChoice` branches per chunk is the right engine. Fidget's measurements (see Appendix) show interval pruning shortens tapes >10× and, once pruned, an interpreter is within ~25% of a JIT — i.e. the pruning is the win, JIT stays unjustified. Revisit as its own design spec when 3D nodes land.
- **LOD-direct generation for `worldgen_rings`.** Far rings call full `generate_region` (chisel straddles included) and then downsample. Generating coarse cells straight from a `ProfileGrid` would skip straddles entirely — a further large win for wasm hitching, but it's a client/consumer redesign, not a `column()` change. Separate plan. **Determinism note (2026-07-10, from Dawid): only worldgen (`generate_region`, anything entering the simulated world) must be deterministic — rendering does not.** A render-only LOD path is therefore free to use f32, FMA, SIMD noise libs (FastNoise2-style), or approximations that would be forbidden in `generate_region`.
- **Folding `/norm` into amplitudes, FMA, f32.** All change bit output (or violate the pure-f64 determinism constraint) for single-digit-ns gains. Rejected.
- **JIT (cranelift) of the density graph.** The graph is fixed at build time; the fused Rust path *is* the compiled form. Rejected.
- **Rayon inside `generate_region`.** Server regions already generate on dedicated threads; wasm has no threads. Work reduction (this plan) is the fix. Rejected for now.
- **Caching `TerrainPipeline` per seed.** Construction is ~16 Perlin permutation shuffles (microseconds). Not worth a static. Rejected.

---

## Appendix: Prior Art (researched 2026-07-10)

How this plan's choices map onto the field; citations for the follow-up specs.

**Minecraft 1.18+ density function engine** — the same DAG-of-nodes problem, solved with two orthogonal levers this plan mirrors:
- `cache_once` = eval-time CSE for multiply-referenced subtrees → our Task 3 fusion (the `offset` subtree referenced 3×).
- `cache_2d` (compute Y-independent nodes once per column) → our `ColumnProfile` already does this.
- `interpolated` cells: full density evaluated only at 4×8×4-block cell corners, trilinearly interpolated inside (~16× fewer expensive evals, corners shared between neighboring cells) and `flat_cache` (once per 4×4 columns) → our Task 5 `ProfileGrid` is the 2D specialization of exactly this.
- Ecosystem note: worldgen-optimizer mods (Noisium, zFastNoise) find the *chunk-fill loop* is the next hotspot after noise — matches our post-Task-5 expectation that block/chisel filling dominates.
- https://minecraft.wiki/w/Density_function · https://www.alanzucconi.com/2022/06/05/minecraft-world-generation/ · https://github.com/Steveplays28/noisium

**Fidget / libfive (Matt Keeter)** — implicit-surface eval as flattened SSA tape + interval-arithmetic pruning:
- Interval eval over a spatial region records which `min`/`max` branches can be active and drops the rest: per-region tapes >10× shorter (254 → ~20 clauses at 8×8 tiles).
- Interpreter vs JIT on 7867-expr model: 5.8s vs 182ms brute-force (31×), but **6ms vs 4.6ms once interval pruning is applied (~25%)** — pruning, not codegen, is the lever. This is why this plan rejects JIT and defers the tape engine until `Min`/`RangeChoice` (caves) exist to prune.
- https://www.mattkeeter.com/projects/fidget/ · https://github.com/mkeeter/fidget · https://www.mattkeeter.com/research/mpr/keeter_mpr20.pdf

**SIMD noise + f64 determinism** — FastNoise2/simdnoise get up to ~7× over scalar but are **f32 + FMA** (FMA fuses rounding → cross-machine divergence), so they're unusable under our determinism bar. However, non-relaxed wasm SIMD128 `f64x2` (and native SSE2 `__m128d` add/mul/min/max) are IEEE-defined bit-for-bit — a hand-rolled 2-wide f64 path stays deterministic as long as it avoids FMA/rsqrt/transcendentals. Option for the batched tape engine later; not worth it for 289 lattice calls/region now.
- https://jordanpeck.me/2016/09/fastnoise-simd/ · https://github.com/Auburn/FastNoiseSIMD · https://github.com/WebAssembly/relaxed-simd/issues/44

**Vectorized interpreters (MonetDB/X100 lineage)** — evaluating one op across a `[f64; BATCH]` buffer before the next op amortizes tree/dispatch overhead by the batch size (~10× reported for expression eval) and autovectorizes. The batch axis for us would be "all lattice points of a region per op" — composes with the tape + interval design above.
- https://www.techbuddies.io/2026/01/16/designing-a-vectorized-query-execution-engine-modern-tradeoffs-and-patterns/ · https://arxiv.org/pdf/1710.03462
