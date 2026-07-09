//! Per-entry hash-mode behavior. These tests mutate the process-global
//! HASH_MODE, so each takes MODE_LOCK and restores Full on drop.
use std::sync::{Mutex, MutexGuard};

use game::{HashMode, Rollback};

static MODE_LOCK: Mutex<()> = Mutex::new(());

struct ModeGuard(#[allow(dead_code)] MutexGuard<'static, ()>);
impl Drop for ModeGuard {
    fn drop(&mut self) {
        game::set_hash_mode(HashMode::Full);
    }
}

/// Locks out other mode-mutating tests, sets `mode`, restores Full on drop
/// (also on panic, so #[should_panic] tests don't poison later ones).
fn with_mode(mode: HashMode) -> ModeGuard {
    let g = MODE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    game::set_hash_mode(mode);
    ModeGuard(g)
}

fn new_rollback() -> (
    Rollback,
    crossbeam::channel::Receiver<game::GameDataUpdate>,
) {
    let (send, recv) = crossbeam::channel::unbounded();
    (Rollback::new(Some(send)), recv)
}

#[test]
fn mode_toggles_between_transactions_without_spurious_failures() {
    // The old set_hash_verification was a one-way OFF latch precisely because
    // re-enabling compared live hashes against sentinel 0s. Per-entry stamps
    // must make Off -> Full re-enabling safe.
    let _mode = with_mode(HashMode::Off);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.rollback(); // logged under Off: no check, no panic

    game::set_hash_mode(HashMode::Full);
    let before = game::state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1);
    r.rollback(); // logged under Full: checked, passes
    assert_eq!(before, game::state_hash(&r));
}

#[test]
fn mixed_mode_entries_within_one_transaction_verify_individually() {
    let _mode = with_mode(HashMode::Off);
    let (mut r, _recv) = new_rollback();
    let before = game::state_hash(&r);
    r.new_transaction();
    r.tick.update(|t| *t += 1); // Off entry
    game::set_hash_mode(HashMode::Full);
    r.next_game_event_id.update(|n| *n += 1); // Full entry, same transaction
    r.rollback();
    assert_eq!(before, game::state_hash(&r));
}

#[test]
fn undomap_ops_roll_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.clients.insert(1, game::Client::default());
    let h1 = game::state_hash(&r);

    r.new_transaction();
    r.clients.get_mut(&1).unwrap().fps_cam_mode = true; // get_mut delta
    r.clients.insert(2, game::Client::default());        // fresh insert (prev = None)
    r.clients.insert(1, game::Client::default());        // overwrite (prev = Some)
    r.clients.remove(&1);                                 // remove
    r.player_entites.insert(9, game::EntityKey::default());
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "delta-verified map undos must be bit-exact");
}

#[test]
fn entities_slotmap_rolls_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    let a = r.ecs.entities.insert(());
    let h1 = game::state_hash(&r);

    r.new_transaction();
    let _b = r.ecs.entities.insert(()); // fresh-slot or reuse path
    r.ecs.entities.remove(a);           // remove path
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "delta-verified slotmap undos must be bit-exact");
}

#[test]
fn entity_lifecycle_rolls_back_bit_exact_under_delta() {
    let _mode = with_mode(HashMode::Delta);
    let (mut r, _recv) = new_rollback();
    r.new_transaction();
    r.create_mesh(game::ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
    let h1 = game::state_hash(&r);

    r.new_transaction();
    r.create_player_safe(7); // exercises insert_safe/set_safe, slotmap emits,
                             // body-insert scope, capsule snapshot (Full), emits
    r.rollback();
    assert_eq!(h1, game::state_hash(&r), "player create must invert bit-exact under Delta");
}


#[test]
fn physics_ticks_roll_back_bit_exact_under_delta() {
    // End-to-end proof that the physics controller's `undo_scope_digest` +
    // `pre_digest`/`revert_digest` wiring inverts bit-exact under Delta. We drive
    // physics through the public `Region` tick path (which runs the physics
    // controller's `on_tick`) rather than naming the internal `PhysicsController`.
    // A live render-event sender is required because rolling back a tier-2 entry
    // unwraps it (same reason as camera_controller.rs).
    let _mode = with_mode(HashMode::Delta);
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut region = game::Region::new(
        Rollback::new(None),
        Some(send),
        game::RegionCoords::new(0, 0),
        None,
    );
    region
        .handle_event(game::GameEventKind::CreateClient(7))
        .unwrap();
    let h1 = game::state_hash(region.data());

    // Five journaled physics ticks (each its own transaction), then roll them all
    // back. Under Delta, each rollback runs revert_digest and asserts it equals
    // the pre-tick digest, so a mismatch would panic here.
    for _ in 0..5 {
        region.handle_event(game::GameEventKind::Tick).unwrap();
    }
    for _ in 0..5 {
        region.rollback_last_event();
    }
    assert_eq!(
        h1,
        game::state_hash(region.data()),
        "journal-digest-verified physics ticks must invert bit-exact under Delta"
    );
}

/// Perf evidence (not a CI gate — run with
/// `cargo test -p game --test hash_mode -- --ignored --nocapture perf_evidence`).
/// Times 100 physics ticks + full rollback under each mode. Expectation:
/// delta ~ off (within noise), full markedly slower as the world grows.
///
/// Adapted from the brief's `PhysicsController`-direct version: that type is
/// a private `game` module (not re-exported at the crate root — see the
/// comment on `physics_ticks_roll_back_bit_exact_under_delta` above), so this
/// drives physics through the same public `Region::handle_event(Tick)` path
/// that test uses, with world setup done via the forgotten-transaction
/// `with_data` hook so only the 100 ticks/rollbacks are timed.
#[test]
#[ignore = "perf evidence, prints numbers; run with --ignored --nocapture"]
fn perf_evidence_hash_modes() {
    fn run(mode: HashMode) -> (std::time::Duration, std::time::Duration) {
        let _m = with_mode(mode);
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut region = game::Region::new(
            Rollback::new(None),
            Some(send),
            game::RegionCoords::new(0, 0),
            None,
        );
        region.with_data(|r| {
            for x in 0..4 {
                for z in 0..4 {
                    r.create_mesh(game::ChunkCoords::new(x, 0, z), game::Chunk::flat_floor(8));
                }
            }
        });
        region
            .handle_event(game::GameEventKind::CreateClient(7))
            .unwrap();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            region.handle_event(game::GameEventKind::Tick).unwrap();
        }
        let ticks = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            region.rollback_last_event();
        }
        (ticks, t.elapsed())
    }
    for mode in [HashMode::Off, HashMode::Delta, HashMode::Full] {
        let (ticks, rollbacks) = run(mode);
        println!("{mode:?}: 100 ticks = {ticks:?}, 100 rollbacks = {rollbacks:?}");
    }
}
