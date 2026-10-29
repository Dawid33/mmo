//! Seeded random op sequences across transactions; rolling everything back
//! must restore the exact state hash. This is the core invariant of the
//! rollback system.
use game::StateHash;

use game::{ChunkCoords, EntityKey, Rollback};

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

/// Seeded per-block writes on one chunk (blocks, chisel entries, writes that
/// densify a uniform grid and writes that collapse it back), one transaction
/// each; undoing them one by one must hit every earlier hash exactly. The
/// inverse restores only the old block + chisel entry through
/// `ChunkBlocks::set`, which is exact because `set` normalises canonically
/// in both directions.
#[test]
fn per_block_writes_undo_to_every_step() {
    use game::{Block, BlockId, BlockIndex, ChiselData, CHUNK_BLOCK_COUNT};
    game::set_hash_mode(game::HashMode::Full);
    for seed in 0u64..8 {
        let mut rng = oorandom::Rand64::new(seed as u128);
        let (send, _recv) = crossbeam::channel::unbounded();
        let mut r = Rollback::new(Some(send));
        r.new_transaction();
        let key = r.create_mesh(ChunkCoords::new(0, 0, 0), game::Chunk::uniform(Block::new(BlockId(1))));
        let mut steps = vec![state_hash(&r)];
        for step in 0..48 {
            r.new_transaction();
            // Every 12th step floods the chunk with one block, so the grid
            // collapses to Uniform and the next write densifies it again.
            if step % 12 == 11 {
                let fill = Block::new(BlockId(rng.rand_range(0..3) as _));
                for i in 0..CHUNK_BLOCK_COUNT {
                    r.ecs.chunk.set_chunk_block_safe(key, BlockIndex::from_linear(i), fill, None);
                }
            } else {
                let index = BlockIndex::from_linear(rng.rand_range(0..CHUNK_BLOCK_COUNT as u64) as usize);
                let id = BlockId(rng.rand_range(0..3) as _);
                let chisel = (rng.rand_range(0..3) == 0)
                    .then(|| ChiselData::cuboid(id, [0, 0, 0], [16, rng.rand_range(1..16) as usize, 16]));
                r.ecs.chunk.set_chunk_block_safe(key, index, Block::new(id), chisel);
            }
            steps.push(state_hash(&r));
        }
        for expected in steps.iter().rev().skip(1) {
            r.rollback();
            assert_eq!(*expected, state_hash(&r), "seed {seed}: per-block undo step mismatch");
        }
    }
}
