//! Upgrade plan Task 1.1: what does rollback state cost on rapier 0.36 at
//! real region scale? Gates D1 (hybrid journal vs full snapshots) and D2
//! (whether the Full-mode digest needs a cached per-`Voxels` digest).
//!
//! ```sh
//! cargo run --release -p worldgen --example export_region_colliders
//! taskset -c 2 cargo run --release -p physics-next --example clone_cost
//! ```
//!
//! Builds the exported region's terrain (one fixed body per chunk, colliders
//! as the game attaches them) plus four kinematic player capsules walking on
//! the surface, then times over [`TICKS`] ticks, per tick: the step, a clone
//! of each structure, the hybrid journal's wholesale clone (islands, narrow
//! phase, broad phase), its first-touch capture (the moved bodies and their
//! colliders), a full 7-structure snapshot, and crc32-over-bincode of each
//! structure — for the `ColliderSet` also with every `Voxels` shape swapped
//! for a stand-in, i.e. what a digest that skips voxel data would cost —
//! and the big ones again through a reusable buffer and one crc32 pass.
//! Every [`FRESH_PIPELINE_EVERY`] ticks the step runs on a new
//! `PhysicsPipeline` (the plan's post-revert pipeline), timed separately.
use std::hint::black_box;
use std::time::{Duration, Instant};

use physics_next::{PhysicsWorld, digest, digest_buffered, load_region};
use rapier3d::prelude::*;

/// Default tick count; `CLONE_COST_TICKS` overrides it (a short run checks the rig).
const TICKS: usize = 600;
const CAPSULES: usize = 4;
const FRESH_PIPELINE_EVERY: usize = 50;
/// Ticks per second: the game ticks every `game::TICK_RATE` = 50 ms.
const TICK_HZ: f32 = 20.0;

#[derive(Default)]
struct Series(Vec<Duration>);

impl Series {
    fn time<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let t = Instant::now();
        let r = black_box(f());
        self.0.push(t.elapsed());
        r
    }
    fn row(&self, name: &str, extra: &str) {
        let mut v: Vec<f64> = self.0.iter().map(|d| d.as_secs_f64() * 1e6).collect();
        if v.is_empty() {
            return;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "| {name:<44} | {:>9.1} | {:>9.1} | {:>9.1} | {:>9.1} | {extra} |",
            pct(0.5),
            pct(0.95),
            v[v.len() - 1],
            mean
        );
    }
}

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/physics-next");
    let ticks: usize = std::env::var("CLONE_COST_TICKS").map_or(TICKS, |v| v.parse().expect("CLONE_COST_TICKS"));
    let region = std::env::args().nth(1).unwrap_or_else(|| "region_0_0.bin".into());
    let export = load_region(&dir.join(&region));

    let mut w = PhysicsWorld { params: IntegrationParameters { dt: 1.0 / TICK_HZ, ..Default::default() }, ..Default::default() };
    let t = Instant::now();
    w.add_region(&export);
    let build = t.elapsed();

    // The first step builds the broad phase over every terrain collider.
    let mut pipeline = PhysicsPipeline::new();
    let t = Instant::now();
    w.step(&mut pipeline);
    let genesis_step = t.elapsed();

    // Four walkers on the surface, at the region's quarter points.
    let [half_height, radius] = export.player_capsule;
    let lift = half_height + radius + radius / 8.0;
    let s = export.region_size;
    let mut walkers = Vec::new();
    for (fx, fz) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)].into_iter().take(CAPSULES) {
        let (x, z) = (s * fx, s * fz);
        let y = w.surface_y(x, z).expect("terrain under a walker") + lift;
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(Vector::new(x, y, z)));
        let collider = w.colliders.insert_with_parent(ColliderBuilder::capsule_y(half_height, radius), body, &mut w.bodies);
        walkers.push((body, collider));
    }
    w.step(&mut pipeline);

    // A digest that skips voxel data sees every Voxels shape as a few bytes.
    let mut stand_in = w.colliders.clone();
    let voxel_handles: Vec<_> = stand_in.iter().filter(|(_, c)| c.shape().as_voxels().is_some()).map(|(h, _)| h).collect();
    for &h in &voxel_handles {
        stand_in.get_mut(h).unwrap().set_shape(SharedShape::ball(1.0));
    }

    let names = ["bodies", "colliders", "islands", "narrow_phase", "broad_phase", "impulse_joints", "multibody_joints"];
    let mut clone: Vec<Series> = names.iter().map(|_| Series::default()).collect();
    let mut dig: Vec<Series> = names.iter().map(|_| Series::default()).collect();
    let mut sizes = [0usize; 7];
    let [mut step, mut fresh_step, mut small, mut full, mut full_drop, mut capture, mut stand_in_dig] =
        std::array::from_fn::<Series, 7, _>(|_| Series::default());
    let mut stand_in_size = 0;
    // Buffered variants: bodies, colliders, broad phase, stand-in colliders.
    let mut buffered: Vec<Series> = (0..4).map(|_| Series::default()).collect();
    let mut buf = Vec::new();
    let mut active_bodies = 0usize;
    let dx = export.walk_speed / TICK_HZ;

    for tick in 0..ticks {
        // Walk +x, following the surface.
        for &(body, _) in &walkers {
            let p = w.bodies[body].translation();
            let x = p.x + dx;
            let y = w.surface_y(x, p.z).map_or(p.y, |y| y + lift);
            w.bodies[body].set_next_kinematic_translation(Vector::new(x, y, p.z));
        }
        if tick % FRESH_PIPELINE_EVERY == 0 {
            pipeline = PhysicsPipeline::new();
            fresh_step.time(|| w.step(&mut pipeline));
        } else {
            step.time(|| w.step(&mut pipeline));
        }
        active_bodies = active_bodies.max(w.islands.active_bodies().count());

        clone[0].time(|| w.bodies.clone());
        clone[1].time(|| w.colliders.clone());
        clone[2].time(|| w.islands.clone());
        clone[3].time(|| w.narrow_phase.clone());
        clone[4].time(|| w.broad_phase.clone());
        clone[5].time(|| w.impulse_joints.clone());
        clone[6].time(|| w.multibody_joints.clone());
        small.time(|| (w.islands.clone(), w.narrow_phase.clone(), w.broad_phase.clone()));
        capture.time(|| {
            walkers.iter().map(|&(b, c)| (w.bodies[b].clone(), w.colliders[c].clone())).collect::<Vec<_>>()
        });
        let snapshot = full.time(|| {
            (
                w.bodies.clone(),
                w.colliders.clone(),
                w.islands.clone(),
                w.narrow_phase.clone(),
                w.broad_phase.clone(),
                w.impulse_joints.clone(),
                w.multibody_joints.clone(),
            )
        });
        full_drop.time(|| drop(snapshot));

        let d = [
            dig[0].time(|| digest(&w.bodies)),
            dig[1].time(|| digest(&w.colliders)),
            dig[2].time(|| digest(&w.islands)),
            dig[3].time(|| digest(&w.narrow_phase)),
            dig[4].time(|| digest(&w.broad_phase)),
            dig[5].time(|| digest(&w.impulse_joints)),
            dig[6].time(|| digest(&w.multibody_joints)),
        ];
        for (i, (_, len)) in d.iter().enumerate() {
            sizes[i] = *len;
        }
        stand_in_size = stand_in_dig.time(|| digest(&stand_in)).1;
        buffered[0].time(|| digest_buffered(&w.bodies, &mut buf));
        buffered[1].time(|| digest_buffered(&w.colliders, &mut buf));
        buffered[2].time(|| digest_buffered(&w.broad_phase, &mut buf));
        buffered[3].time(|| digest_buffered(&stand_in, &mut buf));
    }

    let voxels = export.chunks.iter().flat_map(|c| &c.colliders).filter(|c| matches!(c, physics_next::ExportedCollider::Voxels { .. })).count();
    let cuboids = export.chunks.iter().flat_map(|c| &c.colliders).count() - voxels;
    println!(
        "region {:?} ({} chunks/edge): {} terrain bodies, {} Voxels + {} cuboid colliders, {} walkers; {ticks} ticks, dt 1/{TICK_HZ}",
        export.region,
        export.region_chunks,
        export.chunks.len(),
        voxels,
        cuboids,
        CAPSULES
    );
    println!("build {build:?}; first step (broad phase over all terrain) {genesis_step:?}; max active bodies {active_bodies}");
    println!();
    println!("| {:<44} | {:>9} | {:>9} | {:>9} | {:>9} | bincode bytes |", "µs per tick", "p50", "p95", "max", "mean");
    println!("|{:-<46}|{:->11}|{:->11}|{:->11}|{:->11}|---------------|", "", "", "", "", "");
    step.row("step (warm pipeline)", "");
    fresh_step.row(&format!("step on a fresh PhysicsPipeline (every {FRESH_PIPELINE_EVERY})"), "");
    for (i, n) in names.iter().enumerate() {
        clone[i].row(&format!("clone {n}"), &sizes[i].to_string());
    }
    small.row("hybrid wholesale: islands+narrow+broad", &(sizes[2] + sizes[3] + sizes[4]).to_string());
    capture.row(&format!("hybrid capture: {CAPSULES} bodies + colliders"), "");
    full.row("full 7-structure clone", &sizes.iter().sum::<usize>().to_string());
    full_drop.row("  drop of that snapshot", "");
    for (i, n) in names.iter().enumerate() {
        dig[i].row(&format!("crc32(bincode) {n}"), &sizes[i].to_string());
    }
    stand_in_dig.row("crc32(bincode) colliders, Voxels as stand-ins", &stand_in_size.to_string());
    buffered[0].row("buffered crc32(bincode) bodies", &sizes[0].to_string());
    buffered[1].row("buffered crc32(bincode) colliders", &sizes[1].to_string());
    buffered[2].row("buffered crc32(bincode) broad_phase", &sizes[4].to_string());
    buffered[3].row("buffered crc32(bincode) colliders, Voxels stand-ins", &stand_in_size.to_string());
}
