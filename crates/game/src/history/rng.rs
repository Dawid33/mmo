//! Stateless hashed randomness for history rules: the same (tick, subject, salt)
//! gives the same number on every machine and replay, so there is no RNG state.

use super::calendar::CalendarTick;

/// splitmix64 finalizer (same mixing as `slime::roll`, copied so history does not depend on it).
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub const SALT_MARRY: u64 = 0x4857_0001;
pub const SALT_BIRTH: u64 = 0x4857_0002;
pub const SALT_SEX: u64 = 0x4857_0003;
pub const SALT_DEATH_AGE: u64 = 0x4857_0004;
pub const SALT_MIGRATE: u64 = 0x4857_0005;
pub const SALT_EMBARK: u64 = 0x4857_0006;
pub const SALT_PLACE: u64 = 0x4857_0007;
/// Founder ages at bootstrap.
pub const SALT_AGE: u64 = 0x4857_0008;
/// Where a caught-up site person lands around the region's landing column.
pub const SALT_SPAWN: u64 = 0x4857_0009;
/// Where along the edge a travelling party walks into a running region.
pub const SALT_EDGE: u64 = 0x4857_000A;

/// A 64-bit hash of (time, subject, salt).
pub fn roll(at: CalendarTick, subject: u64, salt: u64) -> u64 {
    mix(mix(at.0 ^ salt.rotate_left(32)) ^ subject)
}

/// [`roll`] with a history's seed folded into the subject, so two seeds give two histories.
pub fn seeded_roll(seed: u64, at: CalendarTick, subject: u64, salt: u64) -> u64 {
    roll(at, subject ^ seed.rotate_left(17), salt)
}

/// True with probability `per_mille / 1000`.
pub fn chance_per_mille(at: CalendarTick, subject: u64, salt: u64, per_mille: u32) -> bool {
    roll(at, subject, salt) % 1000 < u64::from(per_mille)
}
