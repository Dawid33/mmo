//! Intents (`game::intents`, spec §3.2 P6, Task 6.1): a write to something
//! the sender does not own is an `Intent`, applied by the owner at the
//! opening of tick T+1 for an intent sent at tick T — in the same region or
//! across a seam alike — in `(tick, source EntityId, seq)` order, and
//! answered with an `IntentReply` delivered to the sender one tick later.
//! `Order::DevBreakBlock` is the dev front end that sends one
//! (`Intent::BreakBlock` from the client's body).
use std::cell::RefCell;

use game::{
    BlockId, BlockPos, ClientId, EntityId, GameEventKind, InlineSchedule, Intent, IntentRefusal, IntentReply, Key,
    Order, PlayMode, Region, RegionCoords, WorldBlockPos, WorldView, SPAWN_REGION,
};

use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::{assert_seam_invariant, offset, spawn_tile, uniform_floor, Scenario};

/// A region over a one-block floor (block y 0) with `clients` players,
/// wired to a renderer channel so its events can be rolled back.
fn floor_region(clients: &[ClientId]) -> Region {
    crate::common::install_real_block_props();
    let rc = SPAWN_REGION;
    let built = Region::from_columns(rc, uniform_floor(rc));
    let (send, recv) = crossbeam::channel::unbounded();
    // The renderer side stays open for the test's life.
    std::mem::forget(recv);
    let mut region = Region::new(built.data().clone(), Some(send), rc, None);
    for &c in clients {
        region.handle_event(GameEventKind::CreateClient(c)).unwrap();
        region.commit_server_event();
    }
    region
}

fn body_id(region: &Region, client: ClientId) -> EntityId {
    let data = region.data();
    let key = *data.player_entites.get(&client).expect("a player body");
    data.entity_id(key).expect("the body has an id")
}

fn step(region: &mut Region, kind: GameEventKind) {
    region.handle_event(kind).unwrap();
    region.commit_server_event();
}

fn solid(region: &Region, p: WorldBlockPos) -> bool {
    region.data().halo_block(p).is_some_and(|b| b.id != BlockId::AIR)
}

/// A floor block of the spawn region, away from its edges.
fn floor_block() -> WorldBlockPos {
    WorldBlockPos::from_local(SPAWN_REGION, BlockPos::new(10, 0, 10))
}

#[test]
fn intent_applies_next_slot_same_region() {
    let mut region = floor_region(&[0]);
    let by = body_id(&region, 0);
    let target = floor_block();
    let sent_at = region.current_tick();
    step(&mut region, GameEventKind::Order(0, Order::DevBreakBlock(target)));
    assert!(solid(&region, target), "an intent never applies in the event that sends it");
    // The tick of the sending slot: not yet (T+1 is the next tick).
    step(&mut region, GameEventKind::Tick);
    assert!(solid(&region, target), "not applied by tick {sent_at}");
    // Tick T+1 opens with it — undoable bit-exact, like every sim write.
    let before = game::state_hash(region.data());
    region.handle_event(GameEventKind::Tick).unwrap();
    assert!(!solid(&region, target), "applied at the opening of tick {}", sent_at + 1);
    region.rollback_last_event();
    assert_eq!(before, game::state_hash(region.data()), "applying an intent undoes bit-exact");
    step(&mut region, GameEventKind::Tick);
    assert!(!solid(&region, target));
    assert_eq!(region.current_tick(), sent_at + 2);
    assert!(region.data().intent_inbox(by).is_empty(), "the reply is one tick behind the intent");
    step(&mut region, GameEventKind::Tick);
    assert_eq!(region.data().intent_inbox(by), vec![IntentReply::Granted(Intent::BreakBlock { by, survival: false })]);
    // An inbox holds one tick's replies.
    step(&mut region, GameEventKind::Tick);
    assert!(region.data().intent_inbox(by).is_empty());
}

#[test]
fn conflicting_takes_resolve_by_entity_id() {
    let mut region = floor_region(&[0, 1]);
    let (a, b) = (body_id(&region, 0), body_id(&region, 1));
    let (first, second) = if a < b { (a, b) } else { (b, a) };
    let target = floor_block();
    // The higher id sends first: arrival order must not decide.
    let (late_client, early_client) = if a < b { (1, 0) } else { (0, 1) };
    step(&mut region, GameEventKind::Order(late_client, Order::DevBreakBlock(target)));
    step(&mut region, GameEventKind::Order(early_client, Order::DevBreakBlock(target)));
    for _ in 0..3 {
        step(&mut region, GameEventKind::Tick);
    }
    assert!(!solid(&region, target));
    assert_eq!(region.data().intent_inbox(first), vec![IntentReply::Granted(Intent::BreakBlock { by: first, survival: false })]);
    assert_eq!(
        region.data().intent_inbox(second),
        vec![IntentReply::Refused(Intent::BreakBlock { by: second, survival: false }, IntentRefusal::NothingThere)]
    );
}

/// The read side: own ∪ halo blocks, entities sorted by id, a block ray.
#[test]
fn world_view_reads_blocks_entities_and_rays() {
    let region = floor_region(&[0, 1]);
    let view = region.data().view();
    assert_eq!(view.tick(), region.current_tick());
    assert!(view.block(floor_block()).is_some_and(|b| b.id != BlockId::AIR));
    let all = view.entities_in(game::WorldAabb::everything());
    let ids: Vec<EntityId> = all.iter().map(|e| e.id()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    assert!(ids.contains(&body_id(&region, 0)) && ids.contains(&body_id(&region, 1)));
    let me = view.entity(body_id(&region, 0)).expect("own entity");
    assert!(!me.is_mirror());
    // Straight down from 3 blocks above the floor block's top.
    let p = floor_block();
    let bv = game::BLOCK_VOXELS as f64;
    let ray = game::WorldRay {
        origin: [(p.x as f64 + 0.5) * bv, 4.0 * bv, (p.z as f64 + 0.5) * bv],
        dir: [0.0, -1.0, 0.0],
    };
    let hit = view.raycast(ray, 6.0).expect("the floor");
    assert_eq!(hit.block, p);
    assert_eq!(hit.normal, [0, 1, 0]);
}

/// The client's body's id in the region owning it.
fn player_id(rig: &mut Rig) -> (RegionCoords, EntityId) {
    let rc = rig.owners()[0];
    let mut id = None;
    rig.manager.spawner_mut().with_region(rc, |r| id = Some(body_id(r, CLIENT)));
    (rc, id.expect("running"))
}

fn inbox(rig: &mut Rig, rc: RegionCoords, id: EntityId) -> Vec<IntentReply> {
    let mut out = Vec::new();
    rig.manager.spawner_mut().with_region(rc, |r| out = r.data().intent_inbox(id));
    out
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn intent_to_unloaded_owner_is_refused() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let far = RegionCoords::new(SPAWN_REGION.x + 5, SPAWN_REGION.z + 5);
    assert!(!rig.manager.running_regions().contains(&far));
    let target = WorldBlockPos::from_local(far, BlockPos::new(3, 0, 3));
    let (home, by) = player_id(&mut rig);
    rig.order(Order::DevBreakBlock(target));
    let mut replies = Vec::new();
    for _ in 0..6 {
        rig.round();
        replies.extend(inbox(&mut rig, home, by));
    }
    assert_eq!(replies, vec![IntentReply::Refused(Intent::BreakBlock { by, survival: false }, IntentRefusal::OwnerNotRunning)]);
    assert!(!rig.manager.running_regions().contains(&far), "an intent never wakes its owner");
}

// --- Across a seam ------------------------------------------------------------

/// Blocks from the origin, along +x and +z, the walk starts at.
const WALK_START_BLOCKS: i64 = 6;
/// Rounds W+A is held (~13 blocks of diagonal walking: past the origin).
const WALK_ROUNDS: u32 = 60;
/// Rounds after the release, for the body to stop.
const STOP_ROUNDS: u32 = 20;
/// The broken block, relative to the origin: one level down (the floor),
/// past both seams (−x, −z) and well off the walk's diagonal.
const TARGET_OFFSET: [i64; 2] = [-3, -10];

/// The client's body sends `BreakBlock` at a floor block past the seam on
/// the first round, then walks across the seam (the harness wants the motion
/// to cross it). Records, per run, how many ticks after the send the block
/// broke at its owner and the reply reached the sender.
#[derive(Default)]
struct BreakAcross {
    /// Per run: (ticks to the break, ticks to the reply).
    seen: RefCell<Vec<(Option<u64>, Option<u64>)>>,
    /// The current run: the send tick, the target, the sender.
    run: RefCell<Option<(u64, WorldBlockPos, RegionCoords, EntityId)>>,
}

impl Scenario for BreakAcross {
    fn setup(&self, rig: &mut Rig, origin: WorldBlockPos) {
        let d = WALK_START_BLOCKS;
        let at = offset(origin, d, 0, d);
        rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(at)) });
        rig.order(Order::SetMode { mode: PlayMode::Fortress, at: None });
        rig.key(Key::KeyE, true);
        rig.key(Key::KeyE, false);
        for _ in 0..6 {
            rig.round();
        }
        let target = offset(origin, TARGET_OFFSET[0], -1, TARGET_OFFSET[1]);
        let (home, by) = player_id(rig);
        let mut sent_at = 0;
        rig.manager.spawner_mut().with_region(home, |r| sent_at = r.current_tick());
        *self.run.borrow_mut() = Some((sent_at, target, home, by));
        self.seen.borrow_mut().push((None, None));
    }

    fn drive(&self, rig: &mut Rig, round: u32) {
        let (sent_at, target, home, by) = self.run.borrow().expect("set up");
        if round == 0 {
            rig.order(Order::DevBreakBlock(target));
            [Key::KeyW, Key::KeyA].iter().for_each(|&k| rig.key(k, true));
        } else if round == WALK_ROUNDS {
            [Key::KeyW, Key::KeyA].iter().for_each(|&k| rig.key(k, false));
        }
        // What the previous round did, seen at its owners' ticks.
        let (owner, local) = target.to_local();
        let mut seen = self.seen.borrow_mut();
        let entry = seen.last_mut().expect("this run's entry");
        if entry.0.is_none() {
            rig.manager.spawner_mut().with_region(owner, |r| {
                if r.data().block_at([local.x, local.y, local.z]).is_none_or(|b| b.id == BlockId::AIR) {
                    entry.0 = Some(r.current_tick() - sent_at);
                }
            });
        }
        if entry.1.is_none() {
            rig.manager.spawner_mut().with_region(home, |r| {
                if !r.data().intent_inbox(by).is_empty() {
                    assert_eq!(r.data().intent_inbox(by), vec![IntentReply::Granted(Intent::BreakBlock { by, survival: false })]);
                    entry.1 = Some(r.current_tick() - sent_at);
                }
            });
        }
    }

    fn rounds(&self) -> u32 {
        WALK_ROUNDS + STOP_ROUNDS
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn intent_applies_next_slot_across_seam() {
    let s = BreakAcross::default();
    let end = assert_seam_invariant(&s, None);
    assert!(end.terrain.values().any(|d| d.is_some()), "the block broke: {:?}", end.terrain);
    // Lockstep: every placement broke it, and replied, at the same tick
    // after the send — the opening of T+1, the reply one tick later.
    let seen = s.seen.borrow().clone();
    assert_eq!(seen.len(), 4, "one entry per placement");
    for (i, s) in seen.iter().enumerate() {
        assert_eq!(*s, (Some(2), Some(3)), "placement #{i}: (ticks to the break, ticks to the reply)");
    }
    // Under jitter the outcome still matches.
    let s = BreakAcross::default();
    assert_seam_invariant(&s, Some(InlineSchedule::jittery));
}

// --- Fix round 1: replies follow their entity, autosave, parking -------------

/// The running region (if any) holding `id` and its inbox there, with that
/// region's tick.
fn find_inbox(rig: &mut Rig, id: EntityId) -> Option<(RegionCoords, u64, Vec<IntentReply>)> {
    let mut found = None;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |r| {
            let inbox = r.data().intent_inbox(id);
            if !inbox.is_empty() {
                found = Some((rc, r.current_tick(), inbox));
            }
        });
    }
    found
}

/// The sender of a same-region intent departs across the seam in the very
/// tick its intent applies (its reply already queued for it there): the
/// reply leaves with it and is delivered in the new region at the same tick
/// as without the crossing — the opening of T+2.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn reply_follows_sender_across_seam() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let west = RegionCoords::new(SPAWN_REGION.x - 1, SPAWN_REGION.z);
    let (home, by) = player_id(&mut rig);
    assert_eq!(home, SPAWN_REGION);
    let mut sent_at = 0;
    rig.manager.spawner_mut().with_region(home, |r| sent_at = r.current_tick());
    rig.order(Order::DevBreakBlock(floor_block()));
    // Tick T: the intent waits for T+1.
    rig.round();
    // Before tick T+1 the body is put past the west seam: tick T+1 applies
    // the intent (queuing the reply for it here), then its scan departs it.
    rig.manager.spawner_mut().with_region(home, |r| {
        let key = *r.data().player_entites.get(&CLIENT).expect("the body");
        let handle = (*r.data().ecs.rigidbody.try_get(key)).expect("a body");
        let t = r.data().physics.bodies.get(handle).expect("its body").translation();
        let past = -(game::FLIP_HYSTERESIS + 2.0);
        r.with_data(|d| d.set_body_pose_safe(key, game::Pose::translation(past, t.y, t.z)));
    });
    let mut got = Vec::new();
    for _ in 0..6 {
        rig.round();
        if let Some((rc, tick, inbox)) = find_inbox(&mut rig, by) {
            got.push((rc, tick - sent_at, inbox));
        }
    }
    assert_eq!(rig.owners(), [west], "the sender crossed");
    assert_eq!(
        got,
        vec![(west, 3, vec![IntentReply::Granted(Intent::BreakBlock { by, survival: false })])],
        "delivered once, in the new region, at the opening of T+2 (seen after it)"
    );
    assert_eq!(rig.manager.intent_replies_lost(), 0);
}

/// An autosave taken while a cross-region intent is held at its (lagging)
/// target: the target's saved state carries it — the source's does not —
/// and the restored target applies it exactly once.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn autosave_holds_intent_held_at_target_once() {
    let mut rig = Rig::with_floor(None, uniform_floor);
    rig.join_spawn();
    let west = RegionCoords::new(SPAWN_REGION.x - 1, SPAWN_REGION.z);
    let local = BlockPos::new(10, 0, 10);
    let target = WorldBlockPos::from_local(west, local);
    // West lags: the intent waits in its channel, then in its held queue.
    rig.manager.spawner_mut().hold(west, 3);
    rig.order(Order::DevBreakBlock(target));
    rig.round();
    rig.manager.request_save();
    rig.manager.spawner_mut().hold(west, 0);
    rig.settle();
    assert!(rig.manager.awaiting_save().is_empty(), "every region answered");
    let mut saves: std::collections::BTreeMap<RegionCoords, game::Rollback> = Default::default();
    for (rc, save) in rig.manager.take_saves() {
        let game::RegionSave::Live(state, _) = save else { panic!("{rc:?}: running regions save live") };
        saves.insert(rc, *state);
    }
    let held = |s: &game::Rollback| s.queued_intents().iter().filter(|i| i.target == game::Target::Block(target)).count();
    let carried: Vec<_> = saves.iter().filter(|(_, s)| held(s) > 0).map(|(rc, s)| (*rc, held(s))).collect();
    assert_eq!(carried, vec![(west, 1)], "saved once, with its target");
    // Restored, the target applies it once.
    let mut r = game::restore_saved(west, saves.remove(&west).unwrap());
    for _ in 0..3 {
        r.handle_event(GameEventKind::Tick).unwrap();
        r.commit_server_event();
    }
    assert!(r.data().block_at([local.x, local.y, local.z]).is_none_or(|b| b.id == BlockId::AIR), "applied");
    assert!(r.data().queued_intents().is_empty());
    let granted = r.take_intents().into_iter().filter(|m| matches!(m, game::IntentMsg::Reply(s) if matches!(s.reply, IntentReply::Granted(_)))).count();
    assert_eq!(granted, 1, "one grant, to the sender (elsewhere)");
}

/// A region that parks refuses the intents still queued in it: its parked
/// state holds no unanswered intent; the sender (parked with it) gets the
/// refusal queued for it.
#[test]
fn parking_refuses_queued_intents() {
    let region = floor_region(&[0]);
    let by = body_id(&region, 0);
    let (send, recv) = crossbeam::channel::unbounded();
    let mut runner = game::RegionRunner::new(SPAWN_REGION, region, send);
    runner.handle_input(game::RegionInput::Event(GameEventKind::Order(0, Order::DevBreakBlock(floor_block()))));
    assert_eq!(runner.region_mut().data().queued_intents().len(), 1);
    assert!(!runner.handle_input(game::RegionInput::Shutdown));
    let parked = recv
        .try_iter()
        .find_map(|(_, out)| match out {
            game::RegionOutput::Stopped(blob, ..) => Some(blob.to_rollback().unwrap()),
            _ => None,
        })
        .expect("stopped");
    assert!(parked.queued_intents().is_empty(), "no unanswered intent parks");
    let replies: Vec<IntentReply> = parked.queued_replies().iter().map(|r| r.reply).collect();
    assert_eq!(replies, vec![IntentReply::Refused(Intent::BreakBlock { by, survival: false }, IntentRefusal::OwnerNotRunning)]);
}
