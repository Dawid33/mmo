//! Seeded random op sequences across transactions; rolling everything back
//! must restore the exact state hash. This is the core invariant of the
//! rollback system.
use std::hash::Hash;

use game::{ChunkCoords, EntityKey, Rollback};

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.hash(&mut hasher);
    hasher.finalize()
}

#[test]
fn random_op_sequences_roll_back_to_every_boundary() {
    for seed in 0u64..8 {
        let mut rng = oorandom::Rand64::new(seed as u128);
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut r = Rollback::new(Some(send));
        let mut boundaries = vec![state_hash(&r)];

        for _tx in 0..5 {
            r.new_transaction();
            for _op in 0..(1 + rng.rand_range(0..4)) {
                match rng.rand_range(0..6) {
                    0 => r.tick.update(|t| *t += 1),
                    1 => r.next_game_event_id.update(|n| *n = n.wrapping_add(3)),
                    2 => {
                        let k = rng.rand_range(0..3) as usize;
                        r.player_entites.insert(k, EntityKey::default());
                    }
                    3 => {
                        let k = rng.rand_range(0..3) as usize;
                        r.player_entites.remove(&k);
                    }
                    4 => {
                        r.create_mesh(ChunkCoords::new(0, 0, 0), game::Chunk::flat_floor(8));
                    }
                    _ => {
                        r.ecs.create_entity_safe();
                    }
                }
            }
            boundaries.push(state_hash(&r));
        }

        for expected in boundaries.iter().rev().skip(1) {
            r.rollback();
            assert_eq!(*expected, state_hash(&r), "seed {seed}: boundary mismatch");
        }
    }
}
