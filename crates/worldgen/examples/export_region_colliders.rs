//! Exports one generated region's terrain colliders, exactly as the legacy
//! (fully materialized, non-cell) genesis path builds them
//! (`Region::from_columns` → `seed_terrain_genesis`), for the rapier 0.36
//! clone/hash spike in `crates/physics-next/examples/clone_cost.rs`.
//!
//! ```sh
//! cargo run --release -p worldgen --example export_region_colliders [-- <x> <z>]
//! ```
//!
//! `--hash-cost` (first argument, before the coords) instead times the old
//! forks' std `Hash` into crc32 — what `game::state_hash` does — of the same
//! region's physics structures (upgrade plan Task 4.4 Step 4, the baseline for
//! `StateHash` in `clone_cost`); `HASH_COST_ITERS` sets the sample count.
//!
//! Writes `target/physics-next/region_<x>_<z>.bin` (bincode [`RegionExport`];
//! `physics-next` mirrors the types, so keep the two in sync). One entry per
//! terrain body — including bodies with no collider, since the game inserts a
//! fixed body for every chunk — in body-set order, each with its colliders in
//! attach order: a full-chunk cuboid collapse, or `Voxels` grids at block
//! (16), half-block (8) and voxel (1) scale.
//!
//! Lives in `worldgen` because `game` can't depend on it (worldgen depends
//! on game). Build it with the default region size and cells off: that is
//! the production collider set the spike measures.
use game::RegionCoords;
use serde::Serialize;
use worldgen::material::Materials;

#[derive(Serialize)]
pub struct RegionExport {
    pub region: [i32; 2],
    pub region_chunks: u32,
    /// `game::REGION_SIZE` (world units).
    pub region_size: f32,
    /// `game::UNITS_PER_METER`.
    pub units_per_meter: f32,
    /// The player body: `(PLAYER_CAPSULE_HALF_HEIGHT, PLAYER_CAPSULE_RADIUS)`.
    pub player_capsule: [f32; 2],
    /// `WALK_SPEED` in world units per second.
    pub walk_speed: f32,
    pub chunks: Vec<ExportedChunk>,
}

#[derive(Serialize)]
pub struct ExportedChunk {
    /// The chunk body's translation (world units, region-local).
    pub origin: [f32; 3],
    pub colliders: Vec<ExportedCollider>,
}

#[derive(Serialize)]
pub enum ExportedCollider {
    /// Solid cells of one `Voxels` grid, in the shape's iteration order.
    Voxels { voxel_size: [f32; 3], keys: Vec<[i32; 3]> },
    /// The full-chunk collapse; `translation` is body-local.
    Cuboid { half_extents: [f32; 3], translation: [f32; 3] },
}

fn main() {
    let hash_cost = std::env::args().nth(1).is_some_and(|a| a == "--hash-cost");
    let mut args = std::env::args().skip(1 + hash_cost as usize).map(|a| a.parse::<i32>().expect("region coords are integers"));
    let (x, z) = (args.next().unwrap_or(0), args.next().unwrap_or(0));
    if game::CELL_CHUNKS != 0 {
        panic!("export_region_colliders measures the legacy region path: build with cells off");
    }
    let mats = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
    };
    let rc = RegionCoords::new(x, z);
    let region = game::Region::from_columns(rc, worldgen::generate_region(rc, worldgen::WORLD_SEED, mats));
    let physics = &region.data().data.physics;
    if hash_cost {
        return time_hashes(physics);
    }

    let (mut voxel_grids, mut cells, mut cuboids) = ([0usize; 3], [0usize; 3], 0usize);
    let mut chunks = Vec::new();
    for (_, body) in physics.bodies.iter() {
        assert!(body.is_fixed(), "a fresh region holds only terrain bodies");
        let t = body.translation();
        let mut colliders = Vec::new();
        for &h in body.colliders() {
            let c = physics.colliders.get(h).unwrap();
            let local = c.position_wrt_parent().copied().unwrap_or_default();
            if let Some(v) = c.shape().as_voxels() {
                let s = v.voxel_size();
                let keys: Vec<[i32; 3]> = v
                    .voxels()
                    .filter(|d| !d.state.is_empty())
                    .map(|d| [d.grid_coords.x, d.grid_coords.y, d.grid_coords.z])
                    .collect();
                let scale = match s.x.0 as u32 {
                    16 => 0,
                    8 => 1,
                    1 => 2,
                    other => panic!("unexpected voxel size {other}"),
                };
                voxel_grids[scale] += 1;
                cells[scale] += keys.len();
                colliders.push(ExportedCollider::Voxels { voxel_size: [s.x.0, s.y.0, s.z.0], keys });
            } else if let Some(cub) = c.shape().as_cuboid() {
                let he = cub.half_extents;
                let tr = local.translation.vector;
                cuboids += 1;
                colliders.push(ExportedCollider::Cuboid {
                    half_extents: [he.x.0, he.y.0, he.z.0],
                    translation: [tr.x.0, tr.y.0, tr.z.0],
                });
            } else {
                panic!("unexpected terrain collider shape {:?}", c.shape().shape_type());
            }
        }
        chunks.push(ExportedChunk { origin: [t.x.0, t.y.0, t.z.0], colliders });
    }

    let export = RegionExport {
        region: [x, z],
        region_chunks: game::REGION_CHUNKS as u32,
        region_size: game::REGION_SIZE,
        units_per_meter: game::movement::UNITS_PER_METER,
        player_capsule: [game::movement::PLAYER_CAPSULE_HALF_HEIGHT, game::movement::PLAYER_CAPSULE_RADIUS],
        walk_speed: game::movement::WALK_SPEED * game::movement::UNITS_PER_METER,
        chunks,
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/physics-next");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("region_{x}_{z}.bin"));
    let bytes = bincode::serialize(&export).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    println!("region ({x}, {z}), REGION_CHUNKS {}", game::REGION_CHUNKS);
    println!("bodies (one per chunk): {}", export.chunks.len());
    println!("cuboid collapses:       {cuboids}");
    for (i, size) in [16, 8, 1].iter().enumerate() {
        println!("voxels grids @ {size:>2}:     {} ({} solid cells)", voxel_grids[i], cells[i]);
    }
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}

/// Task 4.4 Step 4 baseline: p50/p95/max/mean µs of std `Hash` into crc32 per
/// structure, as `game::state_hash` hashes them, plus each one's bincode size.
fn time_hashes(physics: &game::PhysicsState) {
    use std::hash::{Hash, Hasher};
    use std::time::Instant;
    let iters: usize = std::env::var("HASH_COST_ITERS").map_or(600, |v| v.parse().expect("HASH_COST_ITERS"));
    fn row<T: Hash + serde::Serialize>(name: &str, value: &T, iters: usize) {
        let mut v = Vec::with_capacity(iters);
        let mut acc = 0u64;
        for _ in 0..iters {
            let t = Instant::now();
            let mut h = crc32fast::Hasher::new();
            value.hash(&mut h);
            acc = acc.wrapping_add(std::hint::black_box(h.finish()));
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
        std::hint::black_box(acc);
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let bytes = bincode::serialize(value).unwrap().len();
        println!("| std Hash {name:<35} | {:>9.1} | {:>9.1} | {:>9.1} | {:>9.1} | {bytes} |", pct(0.5), pct(0.95), v[v.len() - 1], mean);
    }
    // Genesis leaves the broad phase empty (the first tick builds it): step a
    // copy once, as clone_cost does before timing, so the broad phase is the
    // whole terrain's.
    let (mut islands, mut broad_phase, mut narrow_phase) =
        ((*physics.islands).clone(), (*physics.broad_phase).clone(), (*physics.narrow_phase).clone());
    let (mut bodies, mut colliders) = ((*physics.bodies).clone(), (*physics.colliders).clone());
    let (mut ij, mut mj, mut ccd) =
        ((*physics.implules_joint_set).clone(), (*physics.multi_body_joint_set).clone(), (*physics.ccd_solver).clone());
    rapier3d::pipeline::PhysicsPipeline::new().step(
        &physics.gravity,
        &physics.integration_parameters,
        &mut islands,
        &mut broad_phase,
        &mut narrow_phase,
        &mut bodies,
        &mut colliders,
        &mut ij,
        &mut mj,
        &mut ccd,
        &(),
        &(),
    );
    println!("old forks (nalgebra era), region (0, 0) legacy genesis + one step: {} bodies, {} colliders; {iters} samples", bodies.len(), colliders.len());
    println!("| {:<44} | {:>9} | {:>9} | {:>9} | {:>9} | bincode bytes |", "µs per hash", "p50", "p95", "max", "mean");
    row("bodies", &bodies, iters);
    row("colliders", &colliders, iters);
    row("islands", &islands, iters);
    row("narrow_phase", &narrow_phase, iters);
    row("broad_phase", &broad_phase, iters);
}
