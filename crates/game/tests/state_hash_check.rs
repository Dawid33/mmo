//! The server's every-N-ticks state hash (`game::state_hash_interval`,
//! `VOXEL_STATE_HASH_INTERVAL` in the server binary): a server region
//! hashes itself on the interval's ticks and ships the hash with the tick
//! event; the client compares its own confirmed state once reconcile
//! confirms that event — under its still-unconfirmed predictions, and on a
//! misprediction.
//!
//! Its own test binary: it sets the process-global interval, which would
//! turn every other runner test's `EventProcessed` ticks into hashed ones.

use crossbeam::channel::{unbounded, Receiver};
use game::{
    Block, Chunk, ChunkCoords, GameEventKind, ReconcileStats, Region, RegionCoords, RegionInput,
    RegionOutput, RegionRunner,
};

const CLIENT: game::ClientId = 7;

fn server() -> (RegionRunner, Receiver<(RegionCoords, RegionOutput)>) {
    let id = RegionCoords::new(0, 0);
    let chunks = (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect();
    let (out_send, out) = unbounded();
    let mut r = RegionRunner::new(id, Region::from_chunks(id, chunks), out_send);
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(CLIENT)));
    r.tick();
    r.handle_input(RegionInput::RequestSnapshot(CLIENT));
    (r, out)
}

fn key_input() -> GameEventKind {
    GameEventKind::PlayerInput(CLIENT, game::InputEvent::Key { key: game::Key::KeyW, pressed: true })
}

/// A client built from the server's snapshot, as on join.
fn client(out: &Receiver<(RegionCoords, RegionOutput)>) -> Region {
    let snapshot = out
        .try_iter()
        .find_map(|(_, o)| match o {
            RegionOutput::Snapshot(_, rollback) => Some(rollback),
            _ => None,
        })
        .expect("snapshot");
    let (send, _updates) = unbounded();
    Region::new(snapshot, Some(send), RegionCoords::new(0, 0), Some(CLIENT))
}

/// Delivers everything the server emitted, as the client's instance does:
/// a hashed event registers its hash first, then reconciles.
fn deliver(client: &mut Region, out: &Receiver<(RegionCoords, RegionOutput)>) -> ReconcileStats {
    let mut total = ReconcileStats::default();
    for (_, output) in out.try_iter() {
        let event = match output {
            RegionOutput::EventProcessed(ev) => ev,
            RegionOutput::EventProcessedHashed(ev, tick, hash) => {
                client.expect_state_hash(ev.id, tick, hash);
                ev
            }
            _ => continue,
        };
        let s = client.reconcile(event).unwrap();
        total.mispredictions += s.mispredictions;
        total.state_hash_checks += s.state_hash_checks;
        total.state_hash_mismatches += s.state_hash_mismatches;
    }
    total
}

/// The client leads by `lead` ticks, predicting (or not) the server's input.
fn run(lead: usize, client_predicts_input: bool, diverge: bool) -> ReconcileStats {
    let (mut server, out) = server();
    let mut client = client(&out);
    if diverge {
        // State the server never had, outside any prediction: reconcile
        // cannot correct it, only the hash check can see it.
        client.with_data(|d| assert!(d.set_block_safe([3, 1, 3], Block::new(game::edit::PLACE_BLOCK))));
    }
    let mut total = ReconcileStats::default();
    for round in 0..6 {
        for _ in 0..lead {
            client.handle_event(GameEventKind::Tick).unwrap();
        }
        let after_tick = client.current_tick() as u64;
        if round == 2 {
            if client_predicts_input {
                client.handle_event(key_input()).unwrap();
            }
            server.handle_input(RegionInput::EventAfterTick(key_input(), after_tick));
        }
        for _ in 0..lead {
            server.tick();
        }
        let s = deliver(&mut client, &out);
        total.mispredictions += s.mispredictions;
        total.state_hash_checks += s.state_hash_checks;
        total.state_hash_mismatches += s.state_hash_mismatches;
    }
    total
}

#[test]
fn hashed_ticks_check_the_confirmed_state() {
    game::set_state_hash_interval(1);

    // Every prediction matches: each check runs under the predictions still
    // on top (rolled back for the hash, replayed after).
    let agreed = run(2, true, false);
    assert_eq!(agreed.mispredictions, 0, "a predicted, stamped input never mispredicts");
    assert!(agreed.state_hash_checks >= 10, "every confirmed tick is checked: {agreed:?}");
    assert_eq!(agreed.state_hash_mismatches, 0, "{agreed:?}");

    // An unpredicted input: the ticks after it mispredict, and the checks
    // on those run at the point where the state is exactly the server's.
    let corrected = run(2, false, false);
    assert!(corrected.mispredictions > 0, "{corrected:?}");
    assert!(corrected.state_hash_checks >= 10, "{corrected:?}");
    assert_eq!(corrected.state_hash_mismatches, 0, "a corrected misprediction is no desync: {corrected:?}");

    // A real divergence is reported on every check.
    let diverged = run(2, true, true);
    assert!(diverged.state_hash_checks >= 10, "{diverged:?}");
    assert_eq!(diverged.state_hash_mismatches, diverged.state_hash_checks, "{diverged:?}");

    // Interval 0: no hashed events at all.
    game::set_state_hash_interval(0);
    let (mut server, out) = server();
    for _ in 0..5 {
        server.tick();
    }
    assert!(
        !out.try_iter().any(|(_, o)| matches!(o, RegionOutput::EventProcessedHashed(..))),
        "interval 0 must not hash"
    );
}
