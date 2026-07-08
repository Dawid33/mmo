//! Keep-up budget benchmark — the pre-merge tripwire for the death spiral.
//!
//! The spiral condition is a wall-clock inequality: the cost to reconcile one
//! server event at steady prediction-log depth must fit in the tick period.
//! Reconciling a mispredict replays the whole unconfirmed log at ~1 region
//! tick of simulation per replayed Tick, and the client also simulates every
//! predicted region each tick. This test measures the primitive — one region
//! tick on real worldgen terrain with a player — and asserts the inequality
//! with a named safety factor, so a game change that slows the sim enough to
//! threaten the spiral fails HERE with a number instead of shipping as
//! mysterious live rubber-banding.
//!
//! Run in release via `just keepup-budget`. Debug timings are legitimately
//! over budget, so the assertion is release-only (the numbers still print).
use game::{GameEventKind, Region, RegionCoords, TICK_RATE};
use std::time::Instant;

const WARMUP_TICKS: usize = 20;
const MEASURE_TICKS: usize = 200;
/// Regions the client currently predicts every tick (the 3x3 window).
const PREDICTED_REGIONS: usize = 9;
/// Steady-state unconfirmed log depth the design must tolerate (client lead
/// plus in-flight inputs); one worst-case mispredict replays this many ticks.
const STEADY_LEAD_TICKS: usize = 10;
/// Required headroom multiple over the bare inequality — a RATCHET, not a
/// target. 1.0 = barely keeps up with zero margin. Measured baseline
/// 2026-07-08 (release, this machine): region tick 4.35ms → headroom 0.61x,
/// i.e. the client does NOT yet have worst-case keep-up headroom — that is
/// the known optimization debt. The gate is set just under today's baseline
/// so it catches REGRESSIONS from here; raise it every time optimization
/// work lands, until it sits at >= 1.0 (ideally 2.0).
const KEEPUP_SAFETY_FACTOR: f64 = 0.55;

#[test]
#[ignore = "wall-clock perf gate; run in release via `just keepup-budget`"]
fn region_tick_cost_fits_keepup_budget() {
    // Real worldgen terrain + a player, same construction as the server.
    let registry = client::blocks::load_registry();
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
    let rc = RegionCoords::new(0, 0);
    let columns = worldgen::generate_region(rc, worldgen::WORLD_SEED, materials);
    let mut region = Region::from_columns(rc, columns);
    region.handle_event(GameEventKind::CreateClient(0)).unwrap();
    region.forget_last_event();

    for _ in 0..WARMUP_TICKS {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let start = Instant::now();
    for _ in 0..MEASURE_TICKS {
        region.handle_event(GameEventKind::Tick).unwrap();
        region.forget_last_event();
    }
    let tick_ms = start.elapsed().as_secs_f64() * 1000.0 / MEASURE_TICKS as f64;

    // Worst steady-state frame: simulate every predicted region once, plus
    // one mispredict replaying the whole lead-deep log.
    let frame_ms = (PREDICTED_REGIONS + STEADY_LEAD_TICKS) as f64 * tick_ms;
    let headroom = TICK_RATE as f64 / frame_ms;
    eprintln!(
        "[budget] region tick: {tick_ms:.3}ms | worst frame ({PREDICTED_REGIONS} regions + \
         {STEADY_LEAD_TICKS}-deep replay): {frame_ms:.1}ms | tick period: {TICK_RATE}ms | \
         headroom: {headroom:.2}x (required: {KEEPUP_SAFETY_FACTOR:.2}x)"
    );
    if cfg!(debug_assertions) {
        eprintln!("[budget] debug build — numbers informational only, not asserting");
        return;
    }
    assert!(
        headroom >= KEEPUP_SAFETY_FACTOR,
        "keep-up budget exceeded: worst frame {frame_ms:.1}ms vs {TICK_RATE}ms tick period \
         (headroom {headroom:.2}x < {KEEPUP_SAFETY_FACTOR:.2}x). A game change made region \
         ticks too slow — this is the death-spiral precondition; profile the region tick."
    );
}
