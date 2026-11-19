//! Deterministic gradient noise. Pure `f64`: only + - * / , `floor`, and
//! integer `powi` — no transcendentals, no fma (see plan Global Constraints).

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

/// Second-stack input scale (Minecraft's `NormalNoise`): the two stacks are
/// sampled at slightly different frequencies so their lattices never line up.
const SECOND_STACK_SCALE: f64 = 1.0181268882175227;

/// Two summed stacks of Perlin octaves at doubling frequencies, weighted the
/// way Minecraft's `NormalNoise` weights them — which is what the octave tables
/// and spline control points in `pipeline.rs` assume:
///
/// - octave `i` weighs `amplitudes[i] * 2^-i` (times a constant), so the FIRST
///   octave — the longest wavelength — dominates and the rest is detail. With
///   flat `amplitudes[i]` weights the 250 m..1 km octaves drowned the 8 km
///   one and no landmass held together above ~1 km.
/// - the sum is scaled by a value factor instead of divided by the amplitude
///   sum, so a 6-9 octave noise has a standard deviation of ~0.5 and reaches
///   ±1.2. Normalizing to the amplitude sum gave sd ~0.1: the splines' land
///   and mountain points were never reached and the world was 99.7% ocean.
///
/// `first_octave` sets the base frequency `2^first_octave` (negative => long
/// wavelength); octave `i` runs at `2^(first_octave + i)`.
pub struct OctaveNoise {
    first: Vec<PerlinNoise>,
    second: Vec<PerlinNoise>,
    /// `amplitudes[i] * 2^(n-1-i) / (2^n - 1)`, precomputed from `new()`.
    weights: Vec<f64>,
    /// 2^(first_octave + i) — exact powers of two, precomputed from `new()`.
    freqs: Vec<f64>,
    /// `(1/6) / (0.1 * (1 + 1/n))` — Minecraft's expected-deviation rescale.
    value_factor: f64,
}

impl OctaveNoise {
    pub fn new(seed: u64, first_octave: i32, amplitudes: Vec<f64>) -> Self {
        let n = amplitudes.len();
        let mut state = seed;
        let first = (0..n).map(|_| PerlinNoise::new(splitmix64(&mut state))).collect();
        let second = (0..n).map(|_| PerlinNoise::new(splitmix64(&mut state))).collect();
        let lowest = 2f64.powi(n as i32 - 1) / (2f64.powi(n as i32) - 1.0);
        let weights = (0..n)
            .map(|i| amplitudes[i] * lowest / 2f64.powi(i as i32))
            .collect();
        let freqs = (0..n)
            .map(|i| 2f64.powi(first_octave + i as i32))
            .collect();
        let value_factor = (1.0 / 6.0) / (0.1 * (1.0 + 1.0 / n as f64));
        Self { first, second, weights, freqs, value_factor }
    }

    pub fn sample2(&self, x: f64, z: f64) -> f64 {
        let mut acc = 0.0;
        for i in 0..self.weights.len() {
            let freq = self.freqs[i];
            let a = self.first[i].sample2(x * freq, z * freq);
            let b = self.second[i]
                .sample2(x * freq * SECOND_STACK_SCALE, z * freq * SECOND_STACK_SCALE);
            acc += self.weights[i] * (a + b);
        }
        acc * self.value_factor
    }
}

