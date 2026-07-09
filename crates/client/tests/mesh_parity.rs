//! Oracle parity and performance gates for the binary mesher against the
//! vendored block-mesh greedy mesher on identical padded input.

use std::collections::BTreeSet;

use block_mesh::ndshape::ConstShape3u32;
use block_mesh::{greedy_quads, GreedyQuadsBuffer, RIGHT_HANDED_Y_UP_CONFIG};
use client::renderer::meshing::binary::{mesh_quads, PaddedVoxels};
use game::{BlockId, Voxel, CHUNK_VOXEL_COUNT};

type OracleShape = ConstShape3u32<34, 34, 34>;

/// Deterministic xorshift chunk content: density ~1/4, three block types.
fn random_chunk(seed: u64) -> Vec<Voxel> {
    let mut s = seed | 1;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut v = vec![Voxel::default(); CHUNK_VOXEL_COUNT];
    for cell in v.iter_mut() {
        if rng() % 4 == 0 {
            *cell = Voxel::new(BlockId((rng() % 3 + 1) as u16));
        }
    }
    v
}

/// Decompose our quads into (axis, positive, unit cell) faces.
fn ours(padded: &PaddedVoxels) -> BTreeSet<(usize, bool, [u32; 3])> {
    let mut set = BTreeSet::new();
    for q in mesh_quads(padded) {
        let axis = q.face.axis();
        let (ua, va) = client::renderer::meshing::binary::cross_axes(axis);
        for du in 0..q.w {
            for dv in 0..q.h {
                let mut cell = q.min;
                cell[ua] += du;
                cell[va] += dv;
                assert!(set.insert((axis, q.face.positive(), cell)), "overlapping quads");
            }
        }
    }
    set
}

/// Decompose block-mesh oracle quads into the same unit-face set.
fn oracle(padded: &PaddedVoxels) -> BTreeSet<(usize, bool, [u32; 3])> {
    let mut buffer = GreedyQuadsBuffer::new(padded.0.len());
    greedy_quads(
        &padded.0,
        &OracleShape {},
        [0; 3],
        [33; 3],
        &RIGHT_HANDED_Y_UP_CONFIG.faces,
        &mut buffer,
    );
    let mut set = BTreeSet::new();
    for (group, face) in buffer
        .quads
        .groups
        .iter()
        .zip(RIGHT_HANDED_Y_UP_CONFIG.faces.into_iter())
    {
        for quad in group.iter() {
            // Unit-face decomposition via the face's own u/v axes.
            let n = face.signed_normal();
            let (axis, positive) = if n.x != 0 {
                (0, n.x > 0)
            } else if n.y != 0 {
                (1, n.y > 0)
            } else {
                (2, n.z > 0)
            };
            // `permutation()` returns an `AxisPermutation`, not an indexable
            // array — `.axes()` gives `[N, U, V]` as `Axis`, so [1]/[2] are U/V.
            let perm_axes = face.permutation().axes();
            let axes = [perm_axes[1].index(), perm_axes[2].index()];
            for du in 0..quad.width {
                for dv in 0..quad.height {
                    let mut cell = [
                        quad.minimum[0] - 1,
                        quad.minimum[1] - 1,
                        quad.minimum[2] - 1,
                    ];
                    cell[axes[0]] += du;
                    cell[axes[1]] += dv;
                    set.insert((axis, positive, cell));
                }
            }
        }
    }
    set
}

#[test]
fn parity_with_block_mesh_on_random_chunks() {
    for seed in 1..=20u64 {
        let padded = PaddedVoxels::from_interior(&random_chunk(seed));
        assert_eq!(ours(&padded), oracle(&padded), "seed {seed}");
    }
}

#[test]
fn parity_with_a_neighbor_filled_shell() {
    // Extends parity to the shell-culling path: both meshers must agree when
    // the padded input's shell comes from a real neighbor (not just air).
    use std::collections::BTreeMap;

    use client::renderer::meshing::neighbors::gather_padded;

    let center = random_chunk(7);
    let full_neighbor = vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT];
    let mut neighbors: BTreeMap<[i32; 3], &[Voxel]> = BTreeMap::new();
    neighbors.insert([1, 0, 0], full_neighbor.as_slice());
    let padded = gather_padded(&center, &neighbors);
    assert_eq!(ours(&padded), oracle(&padded));
}

#[test]
fn parity_on_structured_chunks() {
    for chunk in [
        vec![Voxel::new(BlockId(1)); CHUNK_VOXEL_COUNT], // full
        {
            let c = game::Chunk::flat_floor(24);
            game::derive_voxels(&c.blocks, &c.chisel)
        },
        {
            let c = game::Chunk::flat_floor(8);
            game::derive_voxels(&c.blocks, &c.chisel)
        },
    ] {
        let padded = PaddedVoxels::from_interior(&chunk);
        assert_eq!(ours(&padded), oracle(&padded));
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn binary_mesher_beats_the_oracle_on_the_join_burst() {
    // The measured join burst is ~1088 dense subchunks per region. Gate:
    // the new mesher must be ≥2× faster than block-mesh on the same inputs
    // (relative gate — robust across debug/release and machines).
    let chunks: Vec<Vec<Voxel>> = (1..=64u64).map(random_chunk).collect();
    let padded: Vec<PaddedVoxels> =
        chunks.iter().map(|c| PaddedVoxels::from_interior(c)).collect();

    let t0 = std::time::Instant::now();
    let mut quads = 0usize;
    for _ in 0..17 {
        for p in &padded {
            quads += mesh_quads(p).len();
        }
    }
    let ours_t = t0.elapsed();

    let t1 = std::time::Instant::now();
    let mut oracle_quads = 0usize;
    for _ in 0..17 {
        for p in &padded {
            let mut buffer = GreedyQuadsBuffer::new(p.0.len());
            greedy_quads(
                &p.0,
                &OracleShape {},
                [0; 3],
                [33; 3],
                &RIGHT_HANDED_Y_UP_CONFIG.faces,
                &mut buffer,
            );
            oracle_quads += buffer.quads.num_quads();
        }
    }
    let oracle_t = t1.elapsed();

    assert!(quads > 0 && oracle_quads > 0);
    assert!(
        ours_t * 2 <= oracle_t,
        "binary mesher must be ≥2x block-mesh: ours {ours_t:?} vs oracle {oracle_t:?} \
         over {} chunk-meshes",
        64 * 17
    );
}
