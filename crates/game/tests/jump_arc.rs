//! A jump on flat terrain through the real character controller must follow
//! the ballistic arc `movement::plan` computes: launch on the tick Space is
//! read, rise for JUMP_SPEED/GRAVITY seconds, and land with the body back on
//! its skin. Collision must not shorten, flatten, or delay it.
mod common;

use common::{body, key, ticks, world_of};
use game::movement::{FALL_GRAVITY, GRAVITY, JUMP_SPEED, TICK_DT, UNITS_PER_METER};
use game::{Chunk, Key};

fn world() -> game::World {
    world_of(96.0..160.0, 96.0..160.0, |_, _| Chunk::flat_floor(16))
}

#[test]
fn jump_follows_the_ballistic_arc() {
    let mut world = world();
    let floor = body(&world).y;
    key(&mut world, Key::Space, true);
    let mut arc = Vec::new();
    for _ in 0..40 {
        ticks(&mut world, 1);
        let b = body(&world);
        arc.push((b.y - floor, b.grounded, b.vel_y));
        if b.grounded && arc.len() > 2 {
            break;
        }
    }
    key(&mut world, Key::Space, false);
    for (i, (h, g, vy)) in arc.iter().enumerate() {
        eprintln!("tick {i:2}: h = {:6.2} u ({:.3} m) grounded={g} vel_y={vy:.1}", h, h / UNITS_PER_METER);
    }
    // Ideal arc: rise v/g up to v²/2g, fall back under FALL_GRAVITY.
    let peak_m = JUMP_SPEED * JUMP_SPEED / (2.0 * GRAVITY);
    let airtime_s = JUMP_SPEED / GRAVITY + (2.0 * peak_m / FALL_GRAVITY).sqrt();
    let airtime_ticks = (airtime_s / TICK_DT).round() as usize;
    let peak = arc.iter().map(|a| a.0).fold(0.0, f32::max) / UNITS_PER_METER;
    eprintln!("airborne {} ticks (ideal {airtime_ticks}), peak {peak:.3} m (ideal {peak_m:.3})", arc.len());
    assert!(arc[0].0 > 0.0, "the body leaves the ground on the tick the jump is read");
    assert!((peak - peak_m).abs() < 0.05, "peak {peak} m vs ideal {peak_m} m");
    assert!(
        arc.len().abs_diff(airtime_ticks) <= 1,
        "airborne {} ticks vs ideal {airtime_ticks}",
        arc.len()
    );
}
