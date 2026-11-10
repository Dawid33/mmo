//! Seeded random op sequences across transactions; rolling everything back
//! must restore the exact state hash. This is the core invariant of the
//! rollback system.
use game::StateHash;

use crate::common::{sc, slab_chunk};
use game::{EntityKey, Rollback};

fn state_hash(r: &Rollback) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    r.state_hash(&mut hasher);
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
                        r.create_stud_chunk_safe(sc(0, 0, 0), slab_chunk(3));
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

/// Seeded stud writes on flat ground (bricks placed, bricks removed, boxes
/// carved — also into implied ground, which materialises chunks — and boxes
/// filled), one transaction each; undoing them one by one must hit every
/// earlier hash exactly.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn per_brick_writes_undo_to_every_step() {
    use crate::common::{floor_seed, seed_studs, CHUNK_EDGE, FLOOR_PLATES};
    use game::stud_catalog::STUD_PARTS;
    game::set_hash_mode(game::HashMode::Full);
    let part = STUD_PARTS.iter().position(|p| p.number == "3001").expect("the 2 x 4 brick") as u16;
    for seed in 0u64..8 {
        let mut rng = oorandom::Rand64::new(seed as u128);
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut r = Rollback::new(Some(send));
        seed_studs(&mut r, floor_seed(0.0..2.0 * CHUNK_EDGE, 0.0..2.0 * CHUNK_EDGE, FLOOR_PLATES));
        let mut steps = vec![state_hash(&r)];
        let mut changed = 0;
        for _ in 0..48 {
            r.new_transaction();
            // A stud column of the 2 x 2 chunk columns, clear of their far edges.
            let (x, z) = (rng.rand_range(0..56) as i32, rng.rand_range(0..56) as i32);
            match rng.rand_range(0..4) {
                0 => {
                    // A brick somewhere above the ground of chunk (0, 0).
                    let min = [(x % 28) as u8, FLOOR_PLATES as u8 + rng.rand_range(0..9) as u8, (z % 28) as u8];
                    r.place_stud_safe(sc(0, 0, 0), game::StudBrick::part_at(min, part, rng.rand_range(0..4) as u8, 4));
                }
                1 => {
                    let cell = [(x % 32) as u8, FLOOR_PLATES as u8 - 1 + rng.rand_range(0..6) as u8, (z % 32) as u8];
                    r.remove_stud_safe(sc(0, 0, 0), cell);
                }
                2 => {
                    // Down from the surface, or a pocket in the implied ground below layer 0.
                    let top = if rng.rand_range(0..3) == 0 { -(rng.rand_range(1..40) as i32) } else { FLOOR_PLATES };
                    let depth = 1 + rng.rand_range(0..8) as i32;
                    r.carve_stud_safe([x, top - depth, z], [x + 1 + rng.rand_range(0..6) as i32, top, z + 1 + rng.rand_range(0..6) as i32]);
                }
                _ => {
                    let lo = [x, FLOOR_PLATES - 4 + rng.rand_range(0..8) as i32, z];
                    r.fill_stud_box_safe(lo, [lo[0] + 2, lo[1] + 1 + rng.rand_range(0..6) as i32, lo[2] + 2], game::MAT_STONE);
                }
            }
            let now = state_hash(&r);
            changed += usize::from(Some(&now) != steps.last());
            steps.push(now);
        }
        assert!(changed > 24, "seed {seed}: most writes change the terrain ({changed} of 48)");
        for expected in steps.iter().rev().skip(1) {
            r.rollback();
            assert_eq!(*expected, state_hash(&r), "seed {seed}: per-brick undo step mismatch");
        }
    }
}
