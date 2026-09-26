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
    Voxels { voxel_size: [f32; 3], keys: Vec<[i32; 3]> },
    Cuboid { half_extents: [f32; 3], translation: [f32; 3] },
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
#[derive(Default)]
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
        debug_assert!(self.soft_bodies.is_empty(), "the game never uses soft bodies");
    }

    /// The game's terrain: one fixed body per chunk (with or without
    /// colliders), colliders attached in export order.
    pub fn add_region(&mut self, export: &RegionExport) {
        for chunk in &export.chunks {
            let [x, y, z] = chunk.origin;
            let body = self.bodies.insert(RigidBodyBuilder::fixed().translation(Vector::new(x, y, z)));
            for c in &chunk.colliders {
                let collider = match c {
                    ExportedCollider::Voxels { voxel_size, keys } => {
                        let keys: Vec<IVector> = keys.iter().map(|&k| IVector::from_array(k)).collect();
                        ColliderBuilder::voxels(Vector::from_array(*voxel_size), &keys)
                    }
                    ExportedCollider::Cuboid { half_extents: [hx, hy, hz], translation } => {
                        ColliderBuilder::cuboid(*hx, *hy, *hz).translation(Vector::from_array(*translation))
                    }
                };
                self.colliders.insert_with_parent(collider, body, &mut self.bodies);
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
    let mut w = DigestWriter { hasher: &mut hasher, len: 0 };
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
