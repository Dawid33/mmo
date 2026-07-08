//! Headless proof of the collider-debug-line flow: flag on ⇒ per-region
//! DebugLines events with real content; flag off ⇒ silence (zero cost).
use client::harness::SimHarness;
use game::ClientUpdateEvent;

fn debug_line_events(evs: &[ClientUpdateEvent]) -> Vec<(game::RegionCoords, usize)> {
    evs.iter()
        .filter_map(|e| match e {
            ClientUpdateEvent::DebugLines(id, lines) => Some((*id, lines.len())),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn debug_lines_emit_when_enabled_and_stop_when_disabled() {
    let mut h = SimHarness::new();
    h.connect();
    h.drain_bridge_events(); // discard join-time noise

    // Off by default: ticks emit no DebugLines.
    h.step();
    assert!(
        debug_line_events(&h.drain_bridge_events()).is_empty(),
        "flag off must cost nothing"
    );

    // On: every loaded region gets a buffer each tick, and the home region's
    // is non-empty (terrain colliders sit under the spawned player).
    h.debug.set_collider_wireframes(true);
    let mut saw_nonempty = false;
    for _ in 0..5 {
        h.step();
        let evs = h.drain_bridge_events();
        let dbg = debug_line_events(&evs);
        if dbg.iter().any(|(_, n)| *n > 0) {
            saw_nonempty = true;
            break;
        }
    }
    assert!(saw_nonempty, "expected non-empty collider wireframes near the player");

    // Off again: silence resumes.
    h.debug.set_collider_wireframes(false);
    h.drain_bridge_events();
    h.step();
    assert!(debug_line_events(&h.drain_bridge_events()).is_empty());
}
