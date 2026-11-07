//! The world-tick save cut (`game::save_cut`, world store Task 5): every
//! running region snapshots at the same world tick T, and what is in flight
//! at T is in the cut exactly once. Run on the inline spawner under jittered
//! schedules (`InlineSchedule::jittery`, seeds `0..SEEDS`).
use crate::common::cut_store::CutStore;
use crate::common::rig::{Rig, CLIENT};
use crate::common::seam_translate::{offset, spawn_tile, uniform_floor, Axis, Placement};
use game::{
    Block, BlockId, ClickButton, ClientPacket, Container, EntityId, GameEvent, GameEventKind, InFlightRow,
    InlineSchedule, InlineSpawner, InputEvent, IntentMsg, Inventory, ItemId, ItemStack, MouseButton, Order, PlayMode,
    PlayerId, RegionCoords, RegionSpawner, ServerEvent, SlotRef, UiAction, WorldBlockPos, WorldCut, CHEST_SLOTS,
    SAVE_CUT_LEAD_TICKS, SPAWN_REGION,
};
use std::collections::BTreeSet;

const SEEDS: u64 = 32;
/// Rounds a cut has to complete in (its lead, doubled once, plus jitter).
const CUT_ROUNDS: usize = 200;
/// Rounds a restored world runs before the count.
const RESTORED_ROUNDS: usize = 40;
const STONE: ItemId = ItemId(2);

fn north() -> RegionCoords {
    RegionCoords::new(SPAWN_REGION.x, SPAWN_REGION.z - 1)
}

fn world_tick(rig: &mut Rig) -> u64 {
    rig.manager.spawner_mut().world_tick()
}

fn region_tick(rig: &mut Rig, rc: RegionCoords) -> u64 {
    let mut t = 0;
    rig.manager.spawner_mut().with_region(rc, |r| t = r.current_tick());
    t
}

/// Rounds until the pending cut completes.
fn until_cut(rig: &mut Rig) -> WorldCut {
    for _ in 0..CUT_ROUNDS {
        if let Some(cut) = rig.manager.take_cut() {
            return cut;
        }
        assert!(rig.manager.pending_cut().is_some(), "the cut was aborted");
        rig.round();
    }
    panic!("the cut never completed; waiting for {:?}", rig.manager.awaiting_cut());
}

/// Runs `test` for every seed, naming the seed that broke it.
fn for_seeds(what: &str, test: impl Fn(u64) + std::panic::RefUnwindSafe) {
    let only: Option<u64> = std::env::var("SAVE_CUT_SEED").ok().and_then(|s| s.parse().ok());
    for seed in (0..SEEDS).filter(|s| only.is_none_or(|o| o == *s)) {
        if std::panic::catch_unwind(|| test(seed)).is_err() {
            panic!("{what} broke under InlineSchedule::jittery({seed})");
        }
    }
}

/// The 3x3 window around the spawn region running under
/// `InlineSchedule::jittery(seed)`, the client joined, and the world tick
/// past a cut's lead (so a test can place a cut a few ticks ahead).
fn jittery_rig(seed: u64) -> Rig {
    let mut rig = Rig::with_floor(Some(InlineSchedule::jittery(seed)), uniform_floor);
    rig.join_spawn();
    while world_tick(&mut rig) < SAVE_CUT_LEAD_TICKS + 4 {
        rig.round();
    }
    rig
}

/// A world restarted from `store`: its regions restored from the store, the
/// in-flight rows put back on their way, the 3x3 window around the spawn
/// region running. `player`: the store's record for the client's player
/// (restored), else a new player joins.
fn restored(store: &CutStore, seed: u64, player: bool) -> Rig {
    let mut spawner = InlineSpawner::with_portable(store.regions.clone());
    spawner.set_schedule(Some(InlineSchedule::jittery(seed)));
    let mut rig = Rig::with_spawner(spawner, uniform_floor);
    if player {
        let record = store.players.get(&PlayerId(CLIENT as u64)).cloned().expect("the player's record");
        rig.event(ServerEvent::ClientConnected { client: CLIENT, player: PlayerId(CLIENT as u64), record: Some(record) });
        for rc in SPAWN_REGION.window_3x3() {
            rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(rc), CLIENT));
        }
    } else {
        rig.join_spawn();
    }
    rig.manager.restore_in_flight(store.in_flight.clone());
    for _ in 0..RESTORED_ROUNDS {
        rig.round();
    }
    rig
}

/// How many times the running world holds `id`: owned by a region, in
/// flight, or waiting in an arrival mailbox.
fn live_count(rig: &mut Rig, id: EntityId) -> usize {
    let mut n = 0;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |r| n += r.data().id_index.contains_key(&id) as usize);
    }
    n + rig.manager.in_flight_ids().iter().filter(|(i, _)| *i == id).count()
        + rig.manager.pending_arrival_ids().iter().filter(|(i, _)| *i == id).count()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cut_snapshots_every_region_at_tick_t() {
    for_seeds("a cut of the 3x3 window", |seed| {
        let mut rig = jittery_rig(seed);
        for _ in 0..3 {
            rig.round();
        }
        let now = world_tick(&mut rig);
        let id = rig.manager.begin_cut(now).expect("no cut pending");
        assert_eq!(rig.manager.begin_cut(now), None, "one cut at a time");
        let t = now + SAVE_CUT_LEAD_TICKS;
        assert_eq!(rig.manager.pending_cut(), Some((id, t)));
        let cut = until_cut(&mut rig);
        assert_eq!((cut.id, cut.tick), (id, t));
        let regions: BTreeSet<RegionCoords> = cut.regions.iter().map(|(rc, _)| *rc).collect();
        assert_eq!(regions, SPAWN_REGION.window_3x3().into_iter().collect(), "every running region");
        for (rc, p) in &cut.regions {
            assert_eq!(p.meta.tick, t, "{rc:?}'s snapshot is at the cut tick");
            assert!(p.ground.is_some() && !p.chunks.is_empty(), "{rc:?}: a first snapshot is whole");
        }
        let answers: Vec<_> = rig.cut_snapshots.iter().filter(|a| a.1 == id).collect();
        assert_eq!(answers.len(), 9, "one answer per region: {answers:?}");
        for (rc, _, tick, meta) in answers {
            assert_eq!((*tick, *meta), (t, t), "{rc:?} answered at {tick} (meta {meta})");
        }
        assert_eq!(cut.players.len(), 1, "the client's player");
        assert_eq!(cut.players[0].0, PlayerId(CLIENT as u64));
    });
}

/// Spawns a slime two blocks inside the spawn region's north seam.
fn slime_by_north_seam(rig: &mut Rig) -> EntityId {
    let b = game::BLOCK_VOXELS as f32;
    let mut id = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let tick = r.current_tick();
        let e = crate::common::spawn_slime(r, [128.0, 2.0 * b, 2.0 * b], 1, 0.0, tick);
        id = *r.data().ecs.id.try_get(e);
    });
    id.expect("the slime has an EntityId")
}

/// Puts the slime `id` just past the north seam (in the spawn region's
/// frame), so the region's next tick departs it. Whether it was found there.
fn push_over_north_seam(rig: &mut Rig, id: EntityId) -> bool {
    let mut found = false;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let Some(&key) = r.data().id_index.get(&id) else { return };
        let handle = (*r.data().ecs.rigidbody.try_get(key)).expect("a body");
        let t = r.data().physics.bodies.get(handle).expect("its body").translation();
        r.with_data(|d| d.set_body_pose_safe(key, game::Pose::translation(t.x, t.y, -(game::FLIP_HYSTERESIS + 2.0))));
        found = true;
    });
    found
}

/// A slime crosses the north seam around the cut tick (`d` ticks off its
/// departure): the cut holds it exactly once — in a region snapshot or as an
/// in-flight row — and a world restored from the cut has it exactly once.
fn straddling_transfer(seed: u64, d: i64) {
    let mut rig = jittery_rig(seed);
    let id = slime_by_north_seam(&mut rig);
    rig.round();
    let w0 = world_tick(&mut rig);
    // The slime departs in the tick that brings the spawn region to ~w0 + 10.
    let t = (w0 as i64 + 10 + d) as u64;
    rig.manager.begin_cut(t - SAVE_CUT_LEAD_TICKS).expect("no cut pending");
    let mut pushed = None;
    let mut cut = None;
    for round in 0..CUT_ROUNDS {
        if pushed.is_none() && region_tick(&mut rig, SPAWN_REGION) + 1 >= w0 + 10 {
            let at = region_tick(&mut rig, SPAWN_REGION);
            assert!(push_over_north_seam(&mut rig, id), "the slime is still in the spawn region");
            pushed = Some((round, at + 1));
        }
        rig.round();
        if cut.is_none() {
            cut = rig.manager.take_cut();
        }
        if cut.is_none() && rig.manager.pending_cut().is_none() {
            // Missed (a region read its inputs late past the tick): a cut at
            // the doubled lead, as a server would retry.
            let now = world_tick(&mut rig);
            rig.manager.begin_cut(now).expect("no cut pending");
        }
        let settled = rig.manager.in_flight_count() == 0 && rig.manager.pending_arrival_total() == 0;
        if cut.is_some() && pushed.is_some_and(|(r, _)| round > r + 4) && settled {
            break;
        }
    }
    let cut = cut.expect("the cut completed");
    let (_, departs) = pushed.expect("pushed");
    let mut store = CutStore::default();
    store.apply(&cut);
    let at = store.places(id);
    assert_eq!(at.len(), 1, "cut at {} (departure ~{departs}): the slime is saved in {at:?}", cut.tick);
    let mut again = restored(&store, seed, false);
    assert_eq!(live_count(&mut again, id), 1, "restored from the cut at {} (departure ~{departs}, saved {at:?})", cut.tick);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn straddling_transfer_is_saved_exactly_once() {
    for_seeds("a slime crossing at the cut", |seed| {
        for d in -3..=3 {
            straddling_transfer(seed, d);
        }
    });
}

fn stone(count: u32) -> ItemStack {
    ItemStack { item: STONE, count }
}

fn count_stone(slots: impl IntoIterator<Item = Option<ItemStack>>) -> u32 {
    slots.into_iter().flatten().filter(|s| s.item == STONE).map(|s| s.count).sum()
}

/// Stone in a client: its inventory and cursor.
fn client_stone(c: &game::Client) -> u32 {
    count_stone(c.inventory.slots.iter().copied().chain([c.cursor]))
}

/// Stone an intent or reply in flight carries.
fn msg_stone(msg: &IntentMsg) -> u32 {
    match msg {
        IntentMsg::Intent(s) => match s.intent {
            game::Intent::PutInContainer { stack, .. } | game::Intent::DepositItems { stack, .. } => {
                count_stone([Some(stack)])
            }
            _ => 0,
        },
        IntentMsg::Reply(s) => s.reply.carried_items().map_or(0, |(_, stack)| count_stone([Some(stack)])),
    }
}

/// Stone the running world holds: clients, chests and piles of every
/// running region, and intents in flight.
fn live_stone(rig: &mut Rig) -> u32 {
    let mut n = 0;
    for rc in rig.manager.running_regions() {
        rig.manager.spawner_mut().with_region(rc, |r| {
            let data = r.data();
            n += data.clients.values().map(client_stone).sum::<u32>();
            n += data.containers().values().map(|c| count_stone(c.slots.iter().copied())).sum::<u32>();
            n += data.item_piles().values().map(|p| count_stone(p.stacks.iter().copied().map(Some))).sum::<u32>();
            n += data.queued_intents().iter().map(|s| msg_stone(&IntentMsg::Intent(*s))).sum::<u32>();
            n += data.queued_replies().iter().map(|s| msg_stone(&IntentMsg::Reply(*s))).sum::<u32>();
        });
    }
    n
}

/// Stone the store holds: the players, every stored region's chests, piles
/// and queued intents, and the in-flight rows.
fn stored_stone(store: &CutStore) -> u32 {
    let mut n = store.players.values().map(|p| client_stone(&p.client)).sum::<u32>();
    for r in store.regions.values() {
        n += r.meta.containers.values().map(|c| count_stone(c.slots.iter().copied())).sum::<u32>();
        n += r.meta.item_piles.values().map(|p| count_stone(p.stacks.iter().copied().map(Some))).sum::<u32>();
        n += r.meta.intents.values().map(|s| msg_stone(&IntentMsg::Intent(*s))).sum::<u32>();
        n += r.meta.intent_replies.values().map(|s| msg_stone(&IntentMsg::Reply(*s))).sum::<u32>();
    }
    for row in &store.in_flight {
        if let InFlightRow::Intent { msg, .. } = row {
            n += msg_stone(msg);
        }
    }
    n
}

fn ui(rig: &mut Rig, action: UiAction) {
    let ev = GameEvent::new(GameEventKind::UiAction(CLIENT, action), 0, SPAWN_REGION);
    rig.event(ServerEvent::ClientPacket(ClientPacket::GameEvent(ev), CLIENT));
}

fn click(rig: &mut Rig, slot: SlotRef) {
    ui(rig, UiAction::Click { slot, button: ClickButton::Left, shift: false });
}

/// An Adventure player on the spawn region's −x seam with ten stone on its
/// cursor and the (empty) chest one block across the seam open. Returns the
/// chest's cell.
fn player_at_chest_across_seam(rig: &mut Rig) -> WorldBlockPos {
    let origin = Placement::Seam(Axis::X).origin();
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: Some(spawn_tile(origin)) });
    rig.input(InputEvent::FpsCam(true));
    for _ in 0..6 {
        rig.round();
    }
    let rc = rig.owners()[0];
    rig.manager.spawner_mut().with_region(rc, |r| {
        r.with_data(|d| {
            let c = d.clients.get_mut(&CLIENT).unwrap();
            c.inventory = Inventory::new();
            c.inventory.slots[1] = Some(stone(10));
        })
    });
    let cell = offset(origin, -1, 0, -1);
    let (owner, l) = cell.to_local();
    let chest = game::chest_block().expect("the chest block");
    rig.manager.spawner_mut().with_region(owner, |r| {
        r.with_data(|d| {
            assert!(d.set_block_safe([l.x, l.y, l.z], Block::new(chest)), "chest at {cell:?}");
            d.write_container_safe(cell, Some(Container { slots: vec![None; CHEST_SLOTS] }));
        })
    });
    for _ in 0..3 {
        rig.round();
    }
    rig.aim_at([cell.x as f64 + 0.5, cell.y as f64 + 0.5, cell.z as f64 + 0.5]);
    rig.input(InputEvent::MouseButton { button: MouseButton::Right, pressed: true });
    rig.round();
    rig.input(InputEvent::MouseButton { button: MouseButton::Right, pressed: false });
    for _ in 0..2 {
        rig.round();
    }
    click(rig, SlotRef::Player(1));
    for _ in 0..2 {
        rig.round();
    }
    cell
}

/// The player puts ten stone into a chest across the seam around the cut
/// tick (`d` ticks off the put): the cut holds the stone exactly once — on
/// the player, in the chest, or in the put on its way — and so does a world
/// restored from it.
fn straddling_intent(seed: u64, d: i64) -> bool {
    let mut rig = jittery_rig(seed);
    let cell = player_at_chest_across_seam(&mut rig);
    assert_eq!(live_stone(&mut rig), 10, "ten stone on the cursor");
    let w0 = world_tick(&mut rig);
    let t = (w0 as i64 + 8 + d) as u64;
    rig.manager.begin_cut(t - SAVE_CUT_LEAD_TICKS).expect("no cut pending");
    let mut put = None;
    let mut cut = None;
    for round in 0..CUT_ROUNDS {
        if put.is_none() && region_tick(&mut rig, SPAWN_REGION) + 1 >= w0 + 8 {
            put = Some((round, region_tick(&mut rig, SPAWN_REGION)));
            click(&mut rig, SlotRef::Container(1));
        }
        rig.round();
        if cut.is_none() {
            cut = rig.manager.take_cut();
        }
        if cut.is_none() && rig.manager.pending_cut().is_none() {
            // Missed (a region read its inputs late past the tick): a cut at
            // the doubled lead, as a server would retry.
            let now = world_tick(&mut rig);
            rig.manager.begin_cut(now).expect("no cut pending");
        }
        if cut.is_some() && put.is_some_and(|(r, _)| round > r + 6) {
            break;
        }
    }
    let cut = cut.unwrap_or_else(|| panic!("no cut (t {t}, w0 {w0}, put {put:?}): pending {:?}, waiting for {:?}, missed {:?}", rig.manager.pending_cut(), rig.manager.awaiting_cut(), rig.cut_missed));
    let (_, put_at) = put.expect("put");
    let mut in_chest = None;
    let (owner, _) = cell.to_local();
    rig.manager.spawner_mut().with_region(owner, |r| in_chest = r.data().containers().get(&cell).map(|c| c.slots[1]));
    // Under jitter the chest view may not have opened (the right click is
    // timed on the client side): then nothing moves, still checked.
    let went_in = in_chest == Some(Some(stone(10)));
    assert_eq!(live_stone(&mut rig), 10, "the running world keeps its stone");
    let mut store = CutStore::default();
    store.apply(&cut);
    assert_eq!(stored_stone(&store), 10, "cut at {} (put at ~{put_at}): the stone is saved once", cut.tick);
    let mut again = restored(&store, seed, true);
    assert_eq!(live_stone(&mut again), 10, "restored from the cut at {} (put at ~{put_at})", cut.tick);
    went_in
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn straddling_intent_is_saved_exactly_once() {
    let puts = std::sync::atomic::AtomicUsize::new(0);
    let runs = std::sync::atomic::AtomicUsize::new(0);
    for_seeds("a chest put across the seam at the cut", |seed| {
        for d in -3..=3 {
            runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if straddling_intent(seed, d) {
                puts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });
    let (puts, runs) = (puts.into_inner(), runs.into_inner());
    assert!(puts * 10 >= runs * 9, "the put went in in only {puts} of {runs} runs");
}

/// Every chunk a cut carries for `rc`.
fn chunks_of(cut: &WorldCut, rc: RegionCoords) -> BTreeSet<game::ChunkCoords> {
    cut.regions.iter().filter(|(r, _)| *r == rc).flat_map(|(_, p)| p.chunks.keys().copied()).collect()
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn park_during_pending_cut() {
    for_seeds("a park during a pending cut", |seed| {
        let mut rig = jittery_rig(seed);
        rig.round();
        let now = world_tick(&mut rig);
        let id = rig.manager.begin_cut(now).expect("no cut pending");
        // North parks long before the cut tick.
        rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north()), CLIENT));
        rig.now_ms += game::UNLOAD_GRACE_MS;
        rig.manager.maintain(rig.now_ms);
        let cut = until_cut(&mut rig);
        assert_eq!(cut.id, id);
        assert!(rig.manager.parked_regions().contains(&north()), "north parked");
        let (_, p) = cut.regions.iter().find(|(rc, _)| *rc == north()).expect("north is in the cut");
        assert!(p.meta.tick < cut.tick, "its parked state (tick {}), before the cut tick {}", p.meta.tick, cut.tick);
        let answer = rig.cut_snapshots.iter().find(|a| a.0 == north() && a.1 == id).expect("north answered at its Shutdown");
        assert!(answer.3 < cut.tick, "with its state where it parked, before the cut tick: {answer:?}");
        rig.manager.cut_committed(cut.id, Vec::new());
        // Woken from its parked blob, unchanged, and cut again.
        rig.event(ServerEvent::ClientPacket(ClientPacket::RequestRegionConnection(north()), CLIENT));
        rig.round();
        let now = world_tick(&mut rig);
        rig.manager.begin_cut(now).expect("no cut pending");
        let again = until_cut(&mut rig);
        let mut all = BTreeSet::new();
        rig.manager.spawner_mut().with_region(north(), |r| all = r.data().chunk_terms().into_keys().collect());
        let union: BTreeSet<_> = chunks_of(&cut, north()).union(&chunks_of(&again, north())).copied().collect();
        assert!(!all.is_empty());
        assert_eq!(union, all, "the two cuts hold every chunk of north");
    });
}

/// Places a block on the floor at local `(x, z)` of the spawn region.
fn edit_spawn(rig: &mut Rig, x: i32, z: i32) {
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| assert!(d.set_block_safe([x, 1, z], Block::new(BlockId(1))), "placed at {x},{z}"));
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn failed_cut_keeps_uncommitted() {
    for_seeds("a failed cut", |seed| {
        let mut rig = jittery_rig(seed);
        let now = world_tick(&mut rig);
        rig.manager.begin_cut(now).expect("no cut pending");
        let first = until_cut(&mut rig);
        assert!(!chunks_of(&first, SPAWN_REGION).is_empty());
        rig.manager.cut_failed(first.id);
        // One other chunk changes (the far corner of the region).
        let far = game::REGION_BLOCKS as i32 - 2;
        edit_spawn(&mut rig, far, far);
        let now = world_tick(&mut rig);
        rig.manager.begin_cut(now).expect("no cut pending");
        let second = until_cut(&mut rig);
        let (a, b) = (chunks_of(&first, SPAWN_REGION), chunks_of(&second, SPAWN_REGION));
        assert!(a.is_subset(&b), "the failed cut's chunks are carried again");
        for (rc, _) in &first.regions {
            assert!(second.regions.iter().any(|(r, _)| r == rc), "{rc:?} carried again");
        }
        // Committed, only what changes since goes in the next cut.
        rig.manager.cut_committed(second.id, second.regions.iter().map(|(rc, _)| (*rc, 1)).collect());
        assert_eq!(rig.manager.stored_regions().len(), second.regions.len());
        edit_spawn(&mut rig, 1, 1);
        let now = world_tick(&mut rig);
        rig.manager.begin_cut(now).expect("no cut pending");
        let third = until_cut(&mut rig);
        assert_eq!(chunks_of(&third, SPAWN_REGION).len(), 1, "one chunk changed since the commit");
        assert!(chunks_of(&third, north()).is_empty(), "nothing changed in north");
    });
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn late_region_misses_and_retries() {
    for_seeds("a missed cut", |seed| {
        let mut rig = jittery_rig(seed);
        // North ticks on but reads its inputs only well past the cut tick.
        rig.manager.spawner_mut().hold_inputs(north(), SAVE_CUT_LEAD_TICKS as u32 + 10);
        let now = world_tick(&mut rig);
        let missed = rig.manager.begin_cut(now).expect("no cut pending");
        for _ in 0..CUT_ROUNDS {
            if rig.manager.pending_cut().is_none() {
                break;
            }
            rig.round();
        }
        assert_eq!(rig.manager.pending_cut(), None, "the cut was aborted");
        assert!(rig.manager.take_cut().is_none(), "nothing to take");
        let now = world_tick(&mut rig);
        let id = rig.manager.begin_cut(now).expect("no cut pending");
        assert_ne!(id, missed);
        assert_eq!(rig.manager.pending_cut(), Some((id, now + 2 * SAVE_CUT_LEAD_TICKS)), "the lead doubled");
        let cut = until_cut(&mut rig);
        assert_eq!(cut.tick, now + 2 * SAVE_CUT_LEAD_TICKS);
        // A completed cut resets the lead.
        let now = world_tick(&mut rig);
        rig.manager.begin_cut(now).expect("no cut pending");
        assert_eq!(rig.manager.pending_cut().map(|c| c.1), Some(now + SAVE_CUT_LEAD_TICKS));
    });
}

/// A region that parks during a pending cut, before reaching its tick, with
/// a crossing into it stamped past the cut tick held in its queue: it answers
/// with its state before that crossing (`Shutdown` applies what is held for
/// later slots only after the cut snapshot), so the slime — still in its
/// source's snapshot — is saved once.
fn target_parks_with_late_crossing(seed: u64) {
    let mut rig = jittery_rig(seed);
    let id = slime_by_north_seam(&mut rig);
    rig.round();
    // North sits out the rest: its inputs (the cut, the crossing, the
    // Shutdown) wait in its channel, its clock stays behind.
    rig.manager.spawner_mut().hold(north(), u32::MAX);
    let now = world_tick(&mut rig);
    let cut_id = rig.manager.begin_cut(now).expect("no cut pending");
    let t = now + SAVE_CUT_LEAD_TICKS;
    // The source answers at T; only then does the slime cross (stamped > T).
    for _ in 0..CUT_ROUNDS {
        if rig.manager.awaiting_cut() == [north()] {
            break;
        }
        rig.round();
    }
    assert_eq!(rig.manager.awaiting_cut(), [north()], "everyone but north answered");
    assert!(region_tick(&mut rig, SPAWN_REGION) >= t);
    assert!(push_over_north_seam(&mut rig, id), "the slime is still in the spawn region");
    for _ in 0..3 {
        rig.round();
    }
    assert!(rig.manager.in_flight_ids().iter().any(|(i, rc)| *i == id && *rc == north()), "in flight to north");
    // North's last subscriber goes; it is told to stop, then released: it
    // reads the cut, the crossing and the Shutdown before any tick.
    rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    assert!(region_tick(&mut rig, north()) < t, "north is behind the cut tick");
    rig.manager.spawner_mut().hold(north(), 0);
    rig.settle();
    let cut = until_cut(&mut rig);
    assert_eq!(cut.id, cut_id);
    assert!(rig.cut_snapshots.iter().any(|a| a.0 == north() && a.1 == cut_id), "north answered with a cut snapshot");
    let mut store = CutStore::default();
    store.apply(&cut);
    let at = store.places(id);
    assert_eq!(at, [(SPAWN_REGION, "state")], "saved once, in its source's snapshot");
    let mut again = restored(&store, seed, false);
    assert_eq!(live_count(&mut again, id), 1, "restored once");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn target_parks_during_cut_with_a_crossing_past_t() {
    for_seeds("a target parking with a crossing past the cut", target_parks_with_late_crossing);
}

/// A stopping region whose thread dies before `Stopped` would hold a cut
/// forever: past [`game::CUT_DEADLINE_LEADS`] leads the cut is aborted, and
/// the next one leaves the silent region out and completes.
#[test]
fn cut_past_its_deadline_is_aborted() {
    let mut rig = jittery_rig(0);
    rig.event(ServerEvent::ClientPacket(ClientPacket::ReleaseRegionConnection(north()), CLIENT));
    rig.now_ms += game::UNLOAD_GRACE_MS;
    rig.manager.maintain(rig.now_ms);
    // Shutdown is in its channel; the thread dies before reading it.
    rig.kill(north());
    let now = world_tick(&mut rig);
    rig.manager.begin_cut(now).expect("no cut pending");
    let deadline = now + SAVE_CUT_LEAD_TICKS * (1 + game::CUT_DEADLINE_LEADS);
    while world_tick(&mut rig) <= deadline {
        rig.round();
        assert!(rig.manager.take_cut().is_none(), "the cut waits for north");
    }
    assert_eq!(rig.manager.awaiting_cut(), [north()]);
    rig.manager.maintain(rig.now_ms);
    assert_eq!(rig.manager.pending_cut(), None, "aborted past its deadline");
    let now = world_tick(&mut rig);
    rig.manager.begin_cut(now).expect("a new cut");
    assert!(!rig.manager.awaiting_cut().contains(&north()), "the silent region is left out");
    let cut = until_cut(&mut rig);
    assert_eq!(cut.regions.len(), 8, "every other region");
}

/// A 3x3 world map around the spawn region, every tile with a dry landing.
fn map() -> game::WorldMap {
    let tile = game::TileSummary {
        ground_min_m: 0.0,
        ground_mean_m: 0.0,
        ground_max_m: 0.0,
        relief_m: 0.0,
        slope: [0.0, 0.0],
        sea_fraction: 0.0,
        lake_fraction: 0.0,
        lake_level_m: None,
        rivers: 0,
        river_max_discharge_m2: 0.0,
        river_max_width_m: 0.0,
        surface_rock: None,
        landing: Some([8, 8]),
        biome: game::Biome::Grassland,
        region: 0,
        temperature_c: 10.0,
        precip_mm: 700.0,
        savagery: 50,
        alignment: 0,
        volcano: false,
    };
    let tiles = vec![tile; 9];
    game::WorldMap {
        origin: RegionCoords::new(SPAWN_REGION.x - 1, SPAWN_REGION.z - 1),
        width: 3,
        height: 3,
        tiles,
        rock_names: vec![],
        region_types: vec![],
    }
}

fn press(rig: &mut Rig, key: game::Key) {
    rig.input(InputEvent::Key { key, pressed: true });
    rig.round();
    rig.input(InputEvent::Key { key, pressed: false });
}

/// A player moved to another region (dev Play: parked out of the spawn
/// region, created in north) around the cut tick, throwing a stone there
/// right after it lands: its body is in one snapshot at most, and the stone
/// — on the player, or in north's pile — is saved once.
fn move_straddling(seed: u64, d: i64) {
    let mut rig = Rig::with_floor(Some(InlineSchedule::jittery(seed)), uniform_floor);
    rig.manager.set_world_map(map());
    rig.join_spawn();
    while world_tick(&mut rig) < SAVE_CUT_LEAD_TICKS + 4 {
        rig.round();
    }
    rig.order(Order::SetMode { mode: PlayMode::Adventure, at: None });
    rig.input(InputEvent::FpsCam(true));
    rig.round();
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        r.with_data(|d| {
            let c = d.clients.get_mut(&CLIENT).unwrap();
            c.inventory = Inventory::new();
            c.inventory.slots[0] = Some(stone(10));
            c.selected = 0;
        })
    });
    rig.round();
    let mut body = None;
    rig.manager.spawner_mut().with_region(SPAWN_REGION, |r| {
        let data = r.data();
        body = *data.ecs.id.try_get(*data.player_entites.get(&CLIENT).unwrap());
    });
    let body = body.expect("the player's body");
    let w0 = world_tick(&mut rig);
    let t = (w0 as i64 + 8 + d) as u64;
    rig.manager.begin_cut(t - SAVE_CUT_LEAD_TICKS).expect("no cut pending");
    let mut played = None;
    let mut cut = None;
    for round in 0..CUT_ROUNDS {
        if played.is_none() && region_tick(&mut rig, SPAWN_REGION) + 1 >= w0 + 8 {
            played = Some(round);
            rig.event(ServerEvent::ClientPacket(ClientPacket::PlayRegion(north()), CLIENT));
        }
        if played.is_some() {
            // A move parks the body: an allowed exit (it comes back as a new one).
            rig.tracked.remove(&body);
        }
        if played.is_some_and(|r| round == r + 2) {
            rig.input(InputEvent::FpsCam(true));
        }
        if played.is_some_and(|r| round == r + 3) {
            rig.tracked.remove(&body);
            press(&mut rig, game::Key::KeyQ);
        }
        rig.round();
        if cut.is_none() {
            cut = rig.manager.take_cut();
        }
        if cut.is_none() && rig.manager.pending_cut().is_none() {
            let now = world_tick(&mut rig);
            rig.manager.begin_cut(now).expect("no cut pending");
        }
        if cut.is_some() && played.is_some_and(|r| round > r + 8) {
            break;
        }
    }
    let cut = cut.expect("the cut completed");
    let bodies: usize = cut.regions.iter().map(|(_, p)| p.players.iter().filter(|(c, _)| *c == CLIENT).count()).sum();
    assert!(bodies <= 1, "cut at {}: the body is in {bodies} snapshots", cut.tick);
    // Its creation may be held past the cut tick: until then it is in no region.
    for _ in 0..2 * SAVE_CUT_LEAD_TICKS {
        if !rig.owners().is_empty() {
            break;
        }
        rig.round();
    }
    assert_eq!(live_stone(&mut rig), 10, "the running world keeps its stone");
    let mut store = CutStore::default();
    store.apply(&cut);
    assert_eq!(stored_stone(&store), 10, "cut at {} (Play at ~{}): the stone is saved once", cut.tick, w0 + 7);
    let mut again = restored(&store, seed, true);
    assert_eq!(live_stone(&mut again), 10, "restored from the cut at {}", cut.tick);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn move_straddling_cut_keeps_items_once() {
    for_seeds("a move straddling the cut", |seed| {
        for d in -3..=3 {
            move_straddling(seed, d);
        }
    });
}
