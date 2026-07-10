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

/// Sum of Perlin octaves at doubling frequencies, amplitude-weighted and
/// normalized. `first_octave` sets the base frequency `2^first_octave`
/// (negative => long wavelength); octave `i` runs at `2^(first_octave + i)`.
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
}
