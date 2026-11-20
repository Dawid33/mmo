//! `// mmo-fork: batched set_voxel, one content-hash pass` in the parry fork's
//! `voxels_edition.rs`: `Voxels::set_voxels` applies a batch of edits with the
//! effect of one `set_voxel` per item but a single content-hash recompute at
//! the end (`set_voxel` re-hashes every occupied chunk per call). The game
//! patches a chunk's colliders with it on a block edit instead of rebuilding
//! them, which is only sound if an edited shape is indistinguishable — in
//! `StateHash` and in contacts — from a fresh build of the same voxels.
use std::hash::Hasher;

use game::movement::{CHARACTER_SKIN, PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS};
use rapier3d::parry::query::{ShapeCastOptions, cast_shapes};
use rapier3d::parry::shape::{Capsule, Voxels};
use rapier3d::prelude::*;
use state_hash::StateHash;

const SIZE: f32 = 16.0;
/// Keys live in 0..EDGE per axis: spans two parry 8³ chunks per axis, so
/// edits cross chunk borders (neighbour bits in the adjacent chunk) and empty
/// whole chunks out (chunk freed, slot recycled).
const EDGE: i32 = 12;

fn vhash(v: &Voxels) -> u64 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finish()
}

/// Deterministic LCG (Knuth MMIX), no RNG dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn key(&mut self) -> IVector {
        IVector::new(
            (self.next() % EDGE as u64) as i32,
            (self.next() % EDGE as u64) as i32,
            (self.next() % EDGE as u64) as i32,
        )
    }
}

fn filled(v: &Voxels) -> Vec<IVector> {
    let mut keys: Vec<IVector> = v
        .voxels()
        .filter(|d| !d.state.is_empty())
        .map(|d| d.grid_coords)
        .collect();
    keys.sort_unstable_by_key(|k| (k.x, k.y, k.z));
    keys
}

/// Capsule casts from a grid of starts above and beside the shape, in a few
/// directions: every hit's TOI, normals and witnesses, bit for bit.
fn contacts(v: &Voxels) -> Vec<Option<[u32; 13]>> {
    let capsule = Capsule::new_y(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS);
    let options = ShapeCastOptions {
        target_distance: CHARACTER_SKIN,
        stop_at_penetration: false,
        max_time_of_impact: 4.0 * SIZE,
        compute_impact_geometry_on_penetration: true,
    };
    let span = EDGE as f32 * SIZE;
    let dirs = [
        Vector::NEG_Y,
        Vector::new(1.0, -0.5, 0.3).normalize(),
        Vector::new(-0.7, -1.0, -0.2).normalize(),
        Vector::X,
        Vector::NEG_Z,
    ];
    let mut out = Vec::new();
    for i in 0..9 {
        for j in 0..9 {
            let x = -SIZE + i as f32 * (span + 2.0 * SIZE) / 8.0 + 0.37;
            let z = -SIZE + j as f32 * (span + 2.0 * SIZE) / 8.0 + 0.11;
            for (k, dir) in dirs.iter().enumerate() {
                let y = if k < 3 {
                    span + 2.0 * SIZE
                } else {
                    (i + j) as f32 * span / 16.0 + 1.3
                };
                let start = if k == 3 {
                    Pose::translation(-2.0 * SIZE, y, z)
                } else if k == 4 {
                    Pose::translation(x, y, span + 2.0 * SIZE)
                } else {
                    Pose::translation(x, y, z)
                };
                let hit = cast_shapes(
                    &Pose::IDENTITY,
                    Vector::ZERO,
                    v,
                    &start,
                    *dir,
                    &capsule,
                    options,
                )
                .unwrap();
                out.push(hit.map(|h| {
                    let f = [
                        h.time_of_impact,
                        h.normal1.x,
                        h.normal1.y,
                        h.normal1.z,
                        h.normal2.x,
                        h.normal2.y,
                        h.normal2.z,
                        h.witness1.x,
                        h.witness1.y,
                        h.witness1.z,
                        h.witness2.x,
                        h.witness2.y,
                        h.witness2.z,
                    ];
                    f.map(f32::to_bits)
                }));
            }
        }
    }
    out
}

/// Gate: mmo-fork hunk parry voxels_edition.rs:119/130/146 batched set_voxels, one content-hash pass (51bf3d98)
#[test]
fn batch_equals_per_voxel_and_fresh() {
    for seed in [1u64, 2, 3, 0xC0FFEE] {
        let mut rng = Lcg(seed);
        let mut start: Vec<IVector> = (0..400).map(|_| rng.key()).collect();
        start.sort_unstable_by_key(|k| (k.x, k.y, k.z));
        start.dedup();
        let toggles: Vec<IVector> = (0..200).map(|_| rng.key()).collect();

        let mut per_voxel = Voxels::new(Vector::splat(SIZE), &start);
        let mut edits = Vec::new();
        for key in &toggles {
            let fill = per_voxel.voxel_state(*key).is_none_or(|s| s.is_empty());
            per_voxel.set_voxel(*key, fill);
            edits.push((*key, fill));
        }
        let mut batched = Voxels::new(Vector::splat(SIZE), &start);
        batched.set_voxels(edits);
        let fresh = Voxels::new(Vector::splat(SIZE), &filled(&per_voxel));

        assert_eq!(
            filled(&per_voxel),
            filled(&batched),
            "seed {seed}: same voxels"
        );
        assert_eq!(
            vhash(&per_voxel),
            vhash(&batched),
            "seed {seed}: batch == per voxel"
        );
        assert_eq!(
            vhash(&fresh),
            vhash(&batched),
            "seed {seed}: batch == fresh build"
        );
        let fresh_contacts = contacts(&fresh);
        assert!(
            fresh_contacts.iter().any(Option::is_some),
            "seed {seed}: the cast grid hits something"
        );
        assert_eq!(
            contacts(&per_voxel),
            fresh_contacts,
            "seed {seed}: per-voxel contacts"
        );
        assert_eq!(
            contacts(&batched),
            fresh_contacts,
            "seed {seed}: batched contacts"
        );
    }
}
