//! Narrow-phase probe: contact-graph shape + per-tick physics stage costs for
//! a real worldgen region, under the production hash mode (Delta).
//! Run: cargo run --release -p worldgen --example narrow_phase_probe
use game::{
    Chunk, ChunkCoords, EntityKey, GameEventKind, InputEvent, Key, Region, RegionCoords, Rollback,
};
use rapier3d::prelude::{PhysicsParts, PhysicsPipeline, StepJournal};
use slotmapd::KeyData;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use worldgen::material::Materials;

fn graph_stats(r: &Rollback, label: &str) {
    let phys = &r.physics;
    let g = phys.narrow_phase.contact_graph().raw_graph();
    let nodes = g.raw_nodes();
    let edges = g.raw_edges();

    let mut degree: HashMap<usize, usize> = HashMap::new();
    let mut fixed_fixed = 0usize;
    let mut same_parent = 0usize;
    let mut with_dynamic = 0usize;
    let mut voxels_voxels = 0usize;
    for e in edges {
        *degree.entry(e.source().index()).or_default() += 1;
        *degree.entry(e.target().index()).or_default() += 1;
        let h1 = nodes[e.source().index()].weight;
        let h2 = nodes[e.target().index()].weight;
        let (Some(c1), Some(c2)) = (phys.colliders.get(h1), phys.colliders.get(h2)) else {
            continue;
        };
        let p1 = c1.parent().map(|p| &phys.bodies[p]);
        let p2 = c2.parent().map(|p| &phys.bodies[p]);
        if c1.parent().is_some() && c1.parent() == c2.parent() {
            same_parent += 1;
        }
        match (p1.map(|b| b.is_fixed()), p2.map(|b| b.is_fixed())) {
            (Some(true), Some(true)) => fixed_fixed += 1,
            _ => with_dynamic += 1,
        }
        if c1.shape().as_voxels().is_some() && c2.shape().as_voxels().is_some() {
            voxels_voxels += 1;
        }
    }
    let max_deg = degree.values().copied().max().unwrap_or(0);
    let mean_deg = if degree.is_empty() {
        0.0
    } else {
        degree.values().sum::<usize>() as f64 / degree.len() as f64
    };
    println!(
        "[{label}] contact graph: {} nodes, {} edges \
         (fixed-fixed {fixed_fixed}, same-parent {same_parent}, non-fixed {with_dynamic}, voxels-voxels {voxels_voxels}); \
         degree mean {mean_deg:.1} max {max_deg}",
        nodes.len(),
        edges.len(),
    );
}

fn summarize(label: &str, times: &[Duration]) {
    if times.is_empty() {
        return;
    }
    let total: Duration = times.iter().sum();
    let mean = total / times.len() as u32;
    let max = times.iter().max().unwrap();
    let min = times.iter().min().unwrap();
    println!(
        "[{label}] ticks {}: mean {mean:?}  min {min:?}  max {max:?}",
        times.len()
    );
}

/// One production-equivalent journaled physics step (mirrors
/// PhysicsController::on_tick), timed per pipeline stage. Must run inside an
/// open transaction.
fn direct_step(r: &mut Rollback, pipeline: &mut PhysicsPipeline, label: Option<&str>) {
    let mut journal = StepJournal::default();
    let mut scope = r.physics.undo_scope_digest();
    {
        let p = scope.raw_fields();
        pipeline.step_journaled(
            *p.gravity,
            &*p.integration_parameters,
            p.islands,
            p.broad_phase,
            p.narrow_phase,
            p.bodies,
            p.colliders,
            p.implules_joint_set,
            p.multi_body_joint_set,
            p.soft_bodies,
            p.ccd_solver,
            &(),
            &(),
            Some(&mut journal),
        );
    }
    let t = Instant::now();
    let pre = if scope.wants_delta_digest() {
        journal.pre_digest()
    } else {
        0
    };
    let digest_dt = t.elapsed();
    scope.register(pre, move |phys, _| {
        let p = phys.raw_undo_parts();
        journal.revert_digest(PhysicsParts {
            islands: p.islands,
            broad_phase: p.broad_phase,
            narrow_phase: p.narrow_phase,
            bodies: p.bodies,
            colliders: p.colliders,
            impulse_joints: p.implules_joint_set,
            multibody_joints: p.multi_body_joint_set,
            soft_bodies: p.soft_bodies,
            ccd_solver: p.ccd_solver,
        })
    });
    if let Some(label) = label {
        let c = &pipeline.counters;
        println!(
            "[{label}] step {:.3}ms: user_changes {:.3}ms  broad {:.3}ms  narrow {:.3}ms  \
             islands {:.3}ms  solver {:.3}ms  update {:.3}ms  | pre_digest {digest_dt:?}",
            c.step_time.time_ms(),
            c.stages.user_changes.time_ms(),
            c.cd.broad_phase_time.time_ms(),
            c.cd.narrow_phase_time.time_ms(),
            c.stages.island_construction_time.time_ms(),
            c.stages.solver_time.time_ms(),
            c.stages.update_time.time_ms(),
        );
    }
}

fn main() {
    game::set_hash_mode(game::HashMode::Delta);
    let materials = Materials {
        stone: game::BlockId(2),
        dirt: game::BlockId(1),
        grass: game::BlockId(3),
        sand: game::BlockId(4),
        water: game::BlockId(5),
        bedrock: game::BlockId(6),
        rocks: [7, 8, 9, 10, 11, 12].map(game::BlockId),
        trees: worldgen::material::TreeBlocks::NONE,
        biome: worldgen::material::BiomeBlocks::NONE,
    };
    let rc = RegionCoords::new(0, 0);

    let t = Instant::now();
    let cols = worldgen::generate_region(rc, worldgen::WORLD_SEED, materials);
    println!("worldgen: {:?} ({} columns)", t.elapsed(), cols.len());

    let t = Instant::now();
    let mut region = Region::from_columns(rc, cols);
    println!("region build: {:?}", t.elapsed());

    region
        .handle_event(GameEventKind::CreateClient(0))
        .expect("create client");

    let mut pipeline = PhysicsPipeline::new();

    // First stepped tick: the broad phase sees every genesis collider as new,
    // so this is the pair-creation storm. Driven directly (same journaled step
    // the PhysicsController runs) so the stage counters are readable.
    region.with_data(|r| {
        r.new_transaction();
        let t = Instant::now();
        direct_step(r, &mut pipeline, Some("first tick"));
        println!("first tick total: {:?}", t.elapsed());
    });
    region.with_data(|r| graph_stats(r, "after first tick"));

    // A few settling ticks through the normal Region path.
    for _ in 0..5 {
        region.handle_event(GameEventKind::Tick).expect("tick");
    }

    // Quiescent steady state: nothing moves.
    let mut times = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        region.handle_event(GameEventKind::Tick).expect("tick");
        times.push(t.elapsed());
    }
    summarize("quiescent tick (full handle_event)", &times);
    region.with_data(|r| {
        r.new_transaction();
        direct_step(r, &mut pipeline, Some("quiescent direct step"));
    });

    // Moving player: hold W + slight mouse motion each tick.
    region
        .handle_event(GameEventKind::PlayerInput(
            0,
            InputEvent::Key {
                key: Key::KeyW,
                pressed: true,
            },
        ))
        .expect("input");
    let mut times = Vec::new();
    for _ in 0..200 {
        region
            .handle_event(GameEventKind::PlayerInput(
                0,
                InputEvent::MouseMotion { dx: 2.0, dy: 0.0 },
            ))
            .expect("input");
        let t = Instant::now();
        region.handle_event(GameEventKind::Tick).expect("tick");
        times.push(t.elapsed());
    }
    summarize("moving-player tick (full handle_event)", &times);
    region.with_data(|r| graph_stats(r, "after movement"));

    // Chunk insert (runtime edit path): create_mesh + the tick that pairs it.
    region.with_data(|r| {
        r.new_transaction();
        let t = Instant::now();
        let _e = r.create_mesh(ChunkCoords::new(4, 4, 4), Chunk::flat_floor(8));
        println!("create_mesh (attach colliders): {:?}", t.elapsed());
    });
    let t = Instant::now();
    region.handle_event(GameEventKind::Tick).expect("tick");
    println!("tick after chunk insert: {:?}", t.elapsed());

    // Chunk removal: pick a real terrain chunk body (fixed, with colliders,
    // max contact-graph degree) and remove its entity — the next step takes
    // the wholesale NarrowPhase snapshot + full digest.
    let victim: EntityKey = region
        .data()
        .physics
        .bodies
        .iter()
        .filter(|(_, b)| b.is_fixed() && !b.colliders().is_empty())
        .max_by_key(|(_, b)| b.colliders().len())
        .map(|(_, b)| EntityKey::from(KeyData::from_ffi(b.user_data as u64)))
        .expect("no fixed body found");
    region.with_data(|r| {
        r.new_transaction();
        let t = Instant::now();
        r.remove_entity_safe(victim, None);
        println!("remove_entity_safe: {:?}", t.elapsed());
    });
    let t = Instant::now();
    region.with_data(|r| {
        r.new_transaction();
        direct_step(r, &mut pipeline, Some("removal tick"));
    });
    println!("tick after chunk removal total: {:?}", t.elapsed());

    // Rollback across the removal tick: pays the wholesale narrow-phase
    // restore + the full revert digest.
    let t = Instant::now();
    region.with_data(|r| r.rollback());
    println!("rollback across removal tick: {:?}", t.elapsed());

    // Baseline: rollback across a quiescent tick.
    region.handle_event(GameEventKind::Tick).expect("tick");
    let t = Instant::now();
    region.with_data(|r| r.rollback());
    println!("rollback across quiescent tick: {:?}", t.elapsed());
}
