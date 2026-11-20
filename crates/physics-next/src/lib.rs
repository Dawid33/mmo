//! Harness for the rapier 0.36 / parry 0.31 upgrade
//! (`docs/superpowers/plans/2026-09-22-rapier-035-upgrade.md`). Phase 1:
//! spikes against crates.io rapier; Phase 2 swaps in the vendored `*-next`
//! forks and the fork patches' pinning tests land here. Nothing in
//! `game`/`client`/`server` depends on this crate.

use rapier3d::prelude::*;
use serde::Deserialize;

/// Mirror of `crates/worldgen/examples/export_region_colliders.rs`'s output
/// (bincode is structural: keep the field order and types in sync).
#[derive(Deserialize)]
pub struct RegionExport {
    pub region: [i32; 2],
    pub region_chunks: u32,
    pub region_size: f32,
    pub units_per_meter: f32,
    /// `(half_height, radius)` of the player capsule.
    pub player_capsule: [f32; 2],
    /// World units per second.
    pub walk_speed: f32,
    pub chunks: Vec<ExportedChunk>,
}

#[derive(Deserialize)]
pub struct ExportedChunk {
    pub origin: [f32; 3],
    pub colliders: Vec<ExportedCollider>,
}

#[derive(Deserialize)]
pub enum ExportedCollider {
    Voxels {
        voxel_size: [f32; 3],
        keys: Vec<[i32; 3]>,
    },
    Cuboid {
        half_extents: [f32; 3],
        translation: [f32; 3],
    },
}

pub fn load_region(path: &std::path::Path) -> RegionExport {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nexport it first: cargo run --release -p worldgen --example export_region_colliders",
            path.display()
        )
    });
    bincode::deserialize(&bytes).expect("region export format")
}

/// Every structure `PhysicsPipeline::step` takes by `&mut`.
#[derive(Default, Clone)]
pub struct PhysicsWorld {
    pub gravity: Vector,
    pub params: IntegrationParameters,
    pub islands: IslandManager,
    pub broad_phase: BroadPhaseBvh,
    pub narrow_phase: NarrowPhase,
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub impulse_joints: ImpulseJointSet,
    pub multibody_joints: MultibodyJointSet,
    pub soft_bodies: SoftBodySet,
    pub ccd: CCDSolver,
}

impl PhysicsWorld {
    pub fn step(&mut self, pipeline: &mut PhysicsPipeline) {
        pipeline.step(
            self.gravity,
            &self.params,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            &mut self.ccd,
            &(),
            &(),
        );
        debug_assert!(
            self.soft_bodies.is_empty(),
            "the game never uses soft bodies"
        );
    }

    /// One step recorded into a fresh `StepJournal` (upgrade plan Task 4.2).
    pub fn step_journaled(&mut self, pipeline: &mut PhysicsPipeline) -> StepJournal {
        let mut journal = StepJournal::new();
        pipeline.step_journaled(
            self.gravity,
            &self.params,
            &mut self.islands,
            &mut self.broad_phase,
            &mut self.narrow_phase,
            &mut self.bodies,
            &mut self.colliders,
            &mut self.impulse_joints,
            &mut self.multibody_joints,
            &mut self.soft_bodies,
            &mut self.ccd,
            &(),
            &(),
            Some(&mut journal),
        );
        journal
    }

    /// The seven structures a journal restores, plus the CCD solver whose
    /// cache it discards.
    pub fn parts(&mut self) -> PhysicsParts<'_> {
        PhysicsParts {
            islands: &mut self.islands,
            broad_phase: &mut self.broad_phase,
            narrow_phase: &mut self.narrow_phase,
            bodies: &mut self.bodies,
            colliders: &mut self.colliders,
            impulse_joints: &mut self.impulse_joints,
            multibody_joints: &mut self.multibody_joints,
            soft_bodies: &mut self.soft_bodies,
            ccd_solver: &mut self.ccd,
        }
    }

    pub fn revert(&mut self, journal: StepJournal) {
        journal.revert(self.parts());
    }

    /// bincode of each structure a journal restores, in `PhysicsParts` order.
    /// Byte equality is the revert bar in the tests (std `Hash` is gone from
    /// the glam-era forks; `StateHash` is Task 4.4).
    pub fn state_bytes(&self) -> [Vec<u8>; 8] {
        let b = |t: &dyn erased::Ser| t.bytes();
        [
            b(&self.islands),
            b(&self.broad_phase),
            b(&self.narrow_phase),
            b(&self.bodies),
            b(&self.colliders),
            b(&self.impulse_joints),
            b(&self.multibody_joints),
            b(&self.soft_bodies),
        ]
    }

    /// crc32 `StateHash` of each structure, in `state_bytes` order (Task 4.4).
    pub fn state_hashes(&self) -> [u64; 8] {
        fn h<T: state_hash::StateHash>(t: &T) -> u64 {
            use std::hash::Hasher;
            let mut hasher = crc32fast::Hasher::new();
            t.state_hash(&mut hasher);
            hasher.finish()
        }
        [
            h(&self.islands),
            h(&self.broad_phase),
            h(&self.narrow_phase),
            h(&self.bodies),
            h(&self.colliders),
            h(&self.impulse_joints),
            h(&self.multibody_joints),
            h(&self.soft_bodies),
        ]
    }
}

/// The parts on which bytes and `StateHash` disagree about whether two states
/// (`(state_bytes, state_hashes)`) are equal: a missed field (bytes differ,
/// hash equal) or a hashed non-state (bytes equal, hash differs).
pub fn hash_disagreements(
    a: &([Vec<u8>; 8], [u64; 8]),
    b: &([Vec<u8>; 8], [u64; 8]),
) -> Vec<&'static str> {
    (0..8)
        .filter(|&i| (a.0[i] == b.0[i]) != (a.1[i] == b.1[i]))
        .map(|i| STATE_PARTS[i])
        .collect()
}

/// Names of [`PhysicsWorld::state_bytes`]'s entries, for failure messages.
pub const STATE_PARTS: [&str; 8] = [
    "islands",
    "broad_phase",
    "narrow_phase",
    "bodies",
    "colliders",
    "impulse_joints",
    "multibody_joints",
    "soft_bodies",
];

/// The parts whose bytes differ, e.g. `["bodies", "broad_phase"]`.
pub fn differing_parts(a: &[Vec<u8>; 8], b: &[Vec<u8>; 8]) -> Vec<&'static str> {
    (0..8)
        .filter(|&i| a[i] != b[i])
        .map(|i| STATE_PARTS[i])
        .collect()
}

mod erased {
    pub trait Ser {
        fn bytes(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn bytes(&self) -> Vec<u8> {
            bincode::serialize(self).expect("serialize")
        }
    }
}

impl PhysicsWorld {
    /// The game's terrain: one fixed body per chunk (with or without
    /// colliders), colliders attached in export order.
    pub fn add_region(&mut self, export: &RegionExport) {
        for chunk in &export.chunks {
            let [x, y, z] = chunk.origin;
            let body = self
                .bodies
                .insert(RigidBodyBuilder::fixed().translation(Vector::new(x, y, z)));
            for c in &chunk.colliders {
                let collider = match c {
                    ExportedCollider::Voxels { voxel_size, keys } => {
                        let keys: Vec<IVector> =
                            keys.iter().map(|&k| IVector::from_array(k)).collect();
                        ColliderBuilder::voxels(Vector::from_array(*voxel_size), &keys)
                    }
                    ExportedCollider::Cuboid {
                        half_extents: [hx, hy, hz],
                        translation,
                    } => ColliderBuilder::cuboid(*hx, *hy, *hz)
                        .translation(Vector::from_array(*translation)),
                };
                self.colliders
                    .insert_with_parent(collider, body, &mut self.bodies);
            }
        }
    }

    /// Top surface below `(x, z)`, by a ray cast from far above.
    pub fn surface_y(&self, x: f32, z: f32) -> Option<f32> {
        let qp = self.broad_phase.as_query_pipeline(
            self.narrow_phase.query_dispatcher(),
            &self.bodies,
            &self.colliders,
            QueryFilter::exclude_kinematic(),
        );
        const TOP: f32 = 1.0e5;
        let ray = Ray::new(Vector::new(x, TOP, z), -Vector::Y);
        qp.cast_ray(&ray, 2.0 * TOP, true).map(|(_, toi)| TOP - toi)
    }
}

/// `io::Write` that feeds a crc32 hasher and counts bytes, without
/// allocating: the shape of the planned `StateDigest` (upgrade plan D2).
pub struct DigestWriter<'a> {
    pub hasher: &'a mut crc32fast::Hasher,
    pub len: usize,
}

impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.len += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// crc32 of `value`'s bincode bytes, and their length.
pub fn digest<T: serde::Serialize>(value: &T) -> (u32, usize) {
    let mut hasher = crc32fast::Hasher::new();
    let mut w = DigestWriter {
        hasher: &mut hasher,
        len: 0,
    };
    bincode::serialize_into(&mut w, value).expect("digest serialize");
    let len = w.len;
    (hasher.finalize(), len)
}

/// [`digest`] through a reusable buffer: bincode writes into `buf`, then one
/// crc32 pass. Separates bincode's per-field cost from `crc32fast::update`'s
/// per-call overhead (the unbuffered writer calls it once per primitive).
pub fn digest_buffered<T: serde::Serialize>(value: &T, buf: &mut Vec<u8>) -> (u32, usize) {
    buf.clear();
    bincode::serialize_into(&mut *buf, value).expect("digest serialize");
    (crc32fast::hash(buf), buf.len())
}
