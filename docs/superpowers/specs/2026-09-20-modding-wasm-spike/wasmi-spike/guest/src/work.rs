// Shared workload: compiled into the wasm guest AND directly into the native host (baseline).
pub const N: usize = 1024;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ent {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub hp: u32,
    pub seed: u32,
}

pub const ZERO: Ent = Ent { pos: [0.0; 3], vel: [0.0; 3], hp: 0, seed: 0 };

pub fn init(ents: &mut [Ent; N]) {
    let mut s: u32 = 0x9E37_79B9;
    for e in ents.iter_mut() {
        for k in 0..3 {
            s ^= s << 13; s ^= s >> 17; s ^= s << 5;
            e.pos[k] = ((s >> 8) as f32) / 65536.0 - 128.0;
            e.vel[k] = 0.0;
        }
        e.hp = 100;
        e.seed = s;
    }
}

/// One "mod tick": per-entity steering toward the origin, integration, a branchy
/// integer rule, and a checksum over the resulting bits.
pub fn tick(ents: &mut [Ent; N]) -> u32 {
    let mut sum: u32 = 0;
    for e in ents.iter_mut() {
        let d = [-e.pos[0], -e.pos[1], -e.pos[2]];
        let len = libm::sqrtf(d[0] * d[0] + d[1] * d[1] + d[2] * d[2]);
        if len > 0.001 {
            for k in 0..3 {
                e.vel[k] += d[k] / len * 0.01;
            }
        }
        for k in 0..3 {
            e.vel[k] *= 0.995;
            e.pos[k] += e.vel[k] * 0.05;
        }
        e.seed ^= e.seed << 13; e.seed ^= e.seed >> 17; e.seed ^= e.seed << 5;
        if e.seed & 0xFF < 3 {
            e.hp = if e.hp > 1 { e.hp - 1 } else { 100 };
        }
        for k in 0..3 {
            sum = sum.wrapping_mul(0x0100_0193) ^ e.pos[k].to_bits();
        }
        sum = sum.wrapping_add(e.hp);
    }
    sum
}
