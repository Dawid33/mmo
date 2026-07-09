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
