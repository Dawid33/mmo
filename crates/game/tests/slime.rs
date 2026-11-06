//! Slimes through the real region tick: a spawned slime falls to the ground,
//! then alternates Minecraft-style between sitting still and hopping forward;
//! it wanders across a region seam like any entity; and its ticks — the
//! spawn included — undo bit-exact and replay identically.
use crate::common;

use common::floor_chunks;
use game::slime::{slime_edge_units, slime_spawn_plan, SLIME_HOP_DELAY_MIN_TICKS};
use crossbeam::channel::Receiver;
use game::{
    state_hash, EntityKey, EntityKind, GameDataUpdate, GameDataUpdateKind,
    GameEventKind, Region, RegionCoords,
};
use game::rapier3d::math::Vector;

fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// Flat floor (top face at y = 16) over chunk columns `xs × zs`, with a
/// render-update channel as client regions (the ones that roll back) have.
fn floor_region(
    x_units: std::ops::Range<f32>,
    z_units: std::ops::Range<f32>,
) -> (Region, Receiver<GameDataUpdate>) {
    let chunks = floor_chunks(x_units, z_units, 16);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(game::Rollback::new(Some(send.clone())), Some(send), r0(), None);
    region.with_data(|r| r.seed_terrain_genesis(chunks));
    (region, recv)
}

fn spawn(region: &mut Region, x: f32, z: f32, size: u8, yaw: f32) -> EntityKey {
    let mut key = None;
    region.with_data(|d| {
        let pos = Vector::new(x, 40.0, z);
        key = Some(d.spawn_slime_safe(pos, size, yaw, 0));
    });
    key.unwrap()
}

fn tick(region: &mut Region) {
    region.handle_event(GameEventKind::Tick).unwrap();
    region.forget_last_event();
}

/// (position, grounded)
fn pose(region: &Region, e: EntityKey) -> ([f32; 3], bool) {
    let data = region.data();
    let t = data.physics.bodies.get(*data.ecs.rigidbody.get(e)).unwrap().translation();
    let m = data.ecs.motion.try_get(e).unwrap_or_default();
    ([t.x, t.y, t.z], m.grounded)
}

#[test]
fn slime_rests_then_hops_forward() {
    // Voxels 64..288 each way, the slime starts in the middle.
    let (mut region, updates) = floor_region(64.0..288.0, 64.0..288.0);
    let e = spawn(&mut region, 176.0, 176.0, 1, 0.0);
    assert_eq!(*region.data().ecs.kind.try_get(e), Some(EntityKind::Slime { size: 1 }));

    let mut trace = Vec::new();
    for _ in 0..160 {
        tick(&mut region);
        trace.push(pose(&region, e));
    }
    // It lands on the floor: body centre half a cube above the top face.
    let rest_y = 16.0 + slime_edge_units(1) / 2.0;
    let first_ground = trace.iter().position(|(_, g)| *g).expect("slime must land");
    assert!((trace[first_ground].0[1] - rest_y).abs() < 1.0, "rests on the floor: {:?}", trace[first_ground]);

    // Grounded runs are still (no drift while sitting) and at least the
    // minimum delay long; airborne runs rise and move horizontally.
    let mut hops = 0;
    let mut i = first_ground;
    while i < trace.len() {
        let grounded = trace[i].1;
        let mut j = i;
        while j < trace.len() && trace[j].1 == grounded {
            j += 1;
        }
        if grounded {
            let (a, b) = (trace[i].0, trace[j - 1].0);
            assert_eq!((a[0], a[2]), (b[0], b[2]), "a sitting slime must not slide");
            if j < trace.len() {
                assert!((j - i) as u64 >= SLIME_HOP_DELAY_MIN_TICKS, "rest too short: {}", j - i);
            }
        } else {
            hops += 1;
            let peak = trace[i..j].iter().map(|p| p.0[1]).fold(f32::MIN, f32::max);
            assert!(peak > rest_y + 12.0, "a hop clears most of a block, peak {peak}");
            let (a, b) = (trace[i - 1].0, trace[j.min(trace.len() - 1)].0);
            let dist = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
            assert!(dist > 8.0, "a hop moves the slime, moved {dist}");
        }
        i = j;
    }
    assert!(hops >= 3, "expected several hops in 8 s, got {hops}");

    // A camera-less mover's pose reaches the renderer every tick it is awake.
    let poses = updates
        .try_iter()
        .filter(|u| matches!(u.update_kind, GameDataUpdateKind::SetEntityPosition(k, _) if k == e))
        .count();
    assert!(poses >= 160, "expected a pose update per tick, got {poses}");
}

#[test]
fn slime_ticks_and_spawn_undo_bit_exact_and_replay() {
    // The first spawn attempt of region (0,0), with a floor put under it.
    let (t, plan) = (1..200u64)
        .map(|k| k * game::slime::SLIME_SPAWN_INTERVAL_TICKS)
        .find_map(|t| slime_spawn_plan(r0(), t).map(|p| (t, p)))
        .expect("some attempt within 200 intervals succeeds");
    // A floor of the 3x3 chunks around the one holding the planned spawn.
    let (cx, cz) = (common::chunk_index_of(plan.vx as f32), common::chunk_index_of(plan.vz as f32));
    let edge = common::CHUNK_EDGE;
    let (mut region, _updates) = floor_region(
        (cx as f32 - 1.0) * edge..(cx as f32 + 2.0) * edge,
        (cz as f32 - 1.0) * edge..(cz as f32 + 2.0) * edge,
    );
    // Fast-forward to the attempt. An earlier attempt whose column happens to
    // land on this floor spawns too; the count check below is a delta.
    for _ in 0..t {
        tick(&mut region);
    }
    let slimes = |r: &Region| r.data().ecs.slime.iter().filter(|(_, s)| s.is_some()).count();
    // Every tick from here — the spawn tick and the hops after it — must undo
    // bit-exact, and redo to the same state.
    let before_spawn = slimes(&region);
    for n in 0..60 {
        let before = state_hash(region.data());
        region.handle_event(GameEventKind::Tick).unwrap();
        let after = state_hash(region.data());
        region.rollback_last_event();
        assert_eq!(before, state_hash(region.data()), "tick {n} after the spawn attempt must undo bit-exact");
        region.handle_event(GameEventKind::Tick).unwrap();
        assert_eq!(after, state_hash(region.data()), "tick {n} must replay identically");
        region.forget_last_event();
        if n == 0 {
            assert_eq!(slimes(&region), before_spawn + 1, "the planned spawn happened");
        }
    }
}

// ---- across a seam ----

use crate::common::rig::{north, seam_floor_columns, Rig};
use game::{EntityId, InlineSchedule, SPAWN_REGION};

/// Rounds a slime heading straight at the seam from 1 m short has to cross
/// it and land in the neighbour (it hops ~1.3 m every ~1.5 s at most).
const CROSS_ROUNDS: usize = 300;
/// Seeds of the jittery schedule swept.
const CROSS_SEEDS: u64 = 12;

/// A slime 1 m short of the north seam (−z), its heading pinned north (yaw 0
/// faces −Z; it never re-rolls), must end up owned by north with its
/// `Slime` component — the pinned heading included — intact.
fn slime_crosses_north(schedule: Option<InlineSchedule>) {
    let mut rig = Rig::with_floor(schedule, seam_floor_columns);
    rig.join_spawn();
    let mut id: Option<EntityId> = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |region| {
        let tick = region.current_tick();
        let e = common::spawn_slime(region, [200.0, 24.0, game::movement::UNITS_PER_METER], 1, 0.0, tick);
        region.with_data(|d| {
            let mut s = (*d.data.ecs.slime.try_get(e)).expect("a slime");
            s.next_turn_tick = u64::MAX;
            d.data.ecs.slime.set_safe(e, Some(s));
        });
        id = *region.data().ecs.id.try_get(e);
    });
    let id = id.expect("the slime has an EntityId");
    for _ in 0..CROSS_ROUNDS {
        rig.round();
        let mut found = None;
        rig.manager.spawner_mut().with_region(north(), |region| {
            let data = region.data();
            if let Some(&key) = data.id_index.get(&id) {
                found = Some((*data.ecs.kind.try_get(key), *data.ecs.slime.try_get(key)));
            }
        });
        if let Some((kind, slime)) = found {
            assert_eq!(kind, Some(EntityKind::Slime { size: 1 }), "it is still a size-1 slime");
            let slime = slime.expect("the Slime component crossed with it");
            assert_eq!(slime.next_turn_tick, u64::MAX, "its own state crossed, not a fresh one");
            assert_eq!(slime.target_yaw, 0.0, "still heading north");
            return;
        }
    }
    panic!("the slime never crossed into {:?}", north());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn wanders_across_seam() {
    slime_crosses_north(None);
    for seed in 0..CROSS_SEEDS {
        if std::panic::catch_unwind(|| slime_crosses_north(Some(InlineSchedule::jittery(seed)))).is_err() {
            panic!("the slime crossing broke under InlineSchedule::jittery({seed})");
        }
    }
}
