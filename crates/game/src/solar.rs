//! Sun geometry by date, latitude and local solar time (Phase 6, Task 1).
//!
//! `f64` with `libm` only (`sin`, `cos`, `acos`), never the inherent
//! transcendental methods, each expression in a fixed order — identical on
//! native and wasm32. Inputs are the season module's fixed-point values:
//! latitude in Q16 degrees (north positive), the year fraction and the local
//! solar time (`season::solar_frac_q16`) in Q16 of a year / day. The year
//! starts at the northern spring equinox (declination 0, rising).

use std::f64::consts::PI;

/// Earth's axial tilt (obliquity), degrees. *Sourced* (Wikipedia: Position of the Sun).
pub const AXIAL_TILT_DEG: f64 = 23.44;

const Q16: f64 = 65536.0;

/// Q16 degrees → radians.
fn q16_deg_to_rad(q16: i64) -> f64 {
    (q16 as f64 / Q16) * (PI / 180.0)
}

/// Declination (rad): `tilt · sin(2π · year_frac)`; the year starts at the northern spring equinox.
pub fn declination_rad(year_frac_q16: u16) -> f64 {
    let tilt = AXIAL_TILT_DEG * (PI / 180.0);
    tilt * libm::sin(2.0 * PI * (year_frac_q16 as f64 / Q16))
}

/// The hour angle (rad, 0 at local noon, positive in the afternoon).
fn hour_angle_rad(solar_frac_q16: u16) -> f64 {
    2.0 * PI * (solar_frac_q16 as f64 / Q16 - 0.5)
}

/// sin(elevation) = sin φ sin δ + cos φ cos δ cos h, h = 2π · (solar_frac − 0.5).
pub fn sun_elevation_sin(lat_q16: i64, year_frac_q16: u16, solar_frac_q16: u16) -> f64 {
    sun_direction(lat_q16, year_frac_q16, solar_frac_q16)[1]
}

/// Unit vector TOWARD the sun in engine axes (+x east, +y up, +z south).
///
/// In local east / north / up: `east = −cos δ sin h`, `north = cos φ sin δ −
/// sin φ cos δ cos h`, `up = sin φ sin δ + cos φ cos δ cos h` (a rotation of
/// the celestial unit vector, so its length is 1 up to rounding).
pub fn sun_direction(lat_q16: i64, year_frac_q16: u16, solar_frac_q16: u16) -> [f64; 3] {
    let phi = q16_deg_to_rad(lat_q16);
    let delta = declination_rad(year_frac_q16);
    let h = hour_angle_rad(solar_frac_q16);
    let (sin_phi, cos_phi) = (libm::sin(phi), libm::cos(phi));
    let (sin_d, cos_d) = (libm::sin(delta), libm::cos(delta));
    let cos_h = libm::cos(h);
    let east = -(cos_d * libm::sin(h));
    let north = cos_phi * sin_d - sin_phi * (cos_d * cos_h);
    let up = sin_phi * sin_d + cos_phi * (cos_d * cos_h);
    [east, up, -north]
}

/// Daylight share of the day in 0..=1: acos(clamp(−tan φ tan δ, −1, 1)) / π (1 = polar day, 0 = polar night).
///
/// `tan φ tan δ` is taken as `(sin φ sin δ) / (cos φ cos δ)` (`cos` of a
/// finite double is never exactly 0, so the quotient is finite or ±∞ before
/// the clamp, never NaN).
pub fn daylight_share(lat_q16: i64, year_frac_q16: u16) -> f64 {
    let phi = q16_deg_to_rad(lat_q16);
    let delta = declination_rad(year_frac_q16);
    let x = -((libm::sin(phi) * libm::sin(delta)) / (libm::cos(phi) * libm::cos(delta)));
    libm::acos(x.clamp(-1.0, 1.0)) / PI
}
