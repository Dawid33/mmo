//! The studs world's genesis hook: fresh regions are seeded, parked ones never re-seeded.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{
    RegionCoords, RegionInput, RegionOutput, RegionSeed, RegionSpawner, Rollback,
    StudChunkCoords, REGION_CHUNKS,
};
use game::movement::BodyScale;
use server::region_threads::{StudGenesis, ThreadRegionSpawner};

const WAIT: Duration = Duration::from_secs(60);

fn spawner(genesis: StudGenesis) -> ThreadRegionSpawner {
    ThreadRegionSpawner::with_region_generator(Arc::new(|_| Vec::new())).with_genesis(genesis)
}

fn studs() -> StudGenesis {
    Arc::new(worldgen::stud_world::region_seed)
}

fn spawn(
    s: &mut ThreadRegionSpawner,
    id: RegionCoords,
    seed: RegionSeed,
) -> (Sender<RegionInput>, Receiver<(RegionCoords, RegionOutput)>) {
    let (out, recv) = unbounded();
    (s.spawn(id, seed, out), recv)
}

fn snapshot(send: &Sender<RegionInput>, recv: &Receiver<(RegionCoords, RegionOutput)>) -> Rollback {
    send.send(RegionInput::RequestSnapshot(0)).unwrap();
    loop {
        match recv.recv_timeout(WAIT).expect("region snapshot") {
            (_, RegionOutput::Snapshot(_, rollback)) => return rollback,
            _ => {}
        }
    }
}

/// Stud chunks of layer 0 in a region's own frame.
fn stud_count(r: &Rollback) -> usize {
    let mut n = 0;
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            n += usize::from(r.stud_chunk_at(StudChunkCoords { x, y: 0, z }).is_some());
        }
    }
    n
}

#[test]
fn fresh_stud_region_starts_with_its_chunks() {
    let mut s = spawner(studs());
    let (send, recv) = spawn(&mut s, RegionCoords { x: 0, z: 0 }, RegionSeed::Deferred(None));
    let r = snapshot(&send, &recv);
    assert_eq!(stud_count(&r), REGION_CHUNKS * REGION_CHUNKS);
    assert_eq!(r.body_scale(), BodyScale::STUD);
}

#[test]
fn far_region_is_empty_air() {
    let mut s = spawner(studs());
    let (send, recv) = spawn(&mut s, RegionCoords { x: 5, z: 5 }, RegionSeed::Deferred(None));
    let r = snapshot(&send, &recv);
    assert_eq!(stud_count(&r), 0);
    assert_eq!(r.body_scale(), BodyScale::STUD);
}

/// A parked region comes back as it was: genesis does not run again.
#[test]
fn parked_stud_region_is_not_reseeded() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let genesis: StudGenesis = Arc::new(move |rc| {
        counted.fetch_add(1, Ordering::SeqCst);
        worldgen::stud_world::region_seed(rc)
    });
    let mut s = spawner(genesis);
    let id = RegionCoords { x: 0, z: 0 };
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let before = snapshot(&send, &recv);
    send.send(RegionInput::Shutdown).unwrap();
    let parked = loop {
        if let (_, RegionOutput::Stopped(serialized, ..)) = recv.recv_timeout(WAIT).expect("region stops") {
            break serialized;
        }
    };
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(Some(parked)));
    let after = snapshot(&send, &recv);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "genesis re-ran on a parked region");
    assert_eq!(stud_count(&after), stud_count(&before));
    assert_eq!(after.body_scale(), BodyScale::STUD);
}

/// An edit made through the running region survives parking, and genesis
/// does not run again.
#[test]
fn parked_stud_region_keeps_its_edit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let genesis: StudGenesis = Arc::new(move |rc| {
        counted.fetch_add(1, Ordering::SeqCst);
        worldgen::stud_world::region_seed(rc)
    });
    let mut s = spawner(genesis);
    let id = RegionCoords { x: 0, z: 0 };
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    // A cell high above the generated ground: free of any seeded brick.
    let chunk = StudChunkCoords { x: 0, y: 4, z: 0 };
    let part = game::stud_catalog::STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
    let brick = game::StudBrick { min: [3, 0, 3], shape: game::StudShape::Part { part, rot: 0 }, material: 4, data: 0 };
    assert!(snapshot(&send, &recv).stud_chunk_at(chunk).is_none());
    send.send(RegionInput::Event(game::GameEventKind::Order(0, game::Order::PlaceStud { chunk, brick })))
        .unwrap();
    // Snapshots are served after the event (one input queue).
    let placed = snapshot(&send, &recv);
    assert_eq!(placed.stud_chunk_at(chunk).expect("brick placed").brick_at([3, 0, 3]), Some(&brick));
    send.send(RegionInput::Shutdown).unwrap();
    let parked = loop {
        if let (_, RegionOutput::Stopped(serialized, ..)) = recv.recv_timeout(WAIT).expect("region stops") {
            break serialized;
        }
    };
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(Some(parked)));
    let after = snapshot(&send, &recv);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "genesis re-ran on a parked region");
    assert_eq!(after.stud_chunk_at(chunk).expect("edit survived").brick_at([3, 0, 3]), Some(&brick));
}

/// The procedural world on stud terrain: a fresh region is seeded from generated recipes,
/// keeps them (implied ground), and a player has ground to spawn on.
#[test]
fn procedural_stud_region_starts_from_recipes() {
    let genesis: StudGenesis = Arc::new(|rc| worldgen::stud_gen::generate_stud_region(rc, 7));
    let mut s = spawner(genesis);
    let (send, recv) = spawn(&mut s, RegionCoords { x: 0, z: 0 }, RegionSeed::Deferred(None));
    let r = snapshot(&send, &recv);
    assert_eq!(r.body_scale(), BodyScale::STUD);
    let seed = worldgen::stud_gen::generate_stud_region(RegionCoords { x: 0, z: 0 }, 7);
    for (c, g) in &seed.ground {
        let top = g.surface(3, 3);
        let cell = [(c.x * game::STUD_CHUNK_STUDS + 3) as i32, top - 1, (c.z * game::STUD_CHUNK_STUDS + 3) as i32];
        assert!(r.stud_material_at(cell).is_some(), "top plate of {c:?}");
        assert!(r.stud_material_at([cell[0], top, cell[2]]).is_none(), "air over {c:?}");
        // Far under the materialised band the recipe still answers.
        assert_eq!(r.stud_material_at([cell[0], top - 400, cell[2]]), Some(game::MAT_STONE));
    }
    let (x, z) = r.spawn_xz();
    assert!(r.stud_surface_y(x, z).is_some(), "ground under the spawn");
}

// ---- stud terrain in the world store (stud world plan, P5) ----

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-stud-world-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_store(dir: &std::path::Path) -> world_store::StoreHandle {
    let url = world_store::StoreUrl::Sqlite(dir.join("w.sqlite"));
    let store = world_store::StoreHandle::open(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).unwrap();
    store.bind_terrain(world_store::TerrainKind::Studs).unwrap();
    store
}

/// Stud genesis from generated recipes (the procedural pipeline, seed 7), counting its calls.
fn counted_genesis() -> (StudGenesis, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let genesis: StudGenesis = Arc::new(move |rc| {
        counted.fetch_add(1, Ordering::SeqCst);
        worldgen::stud_gen::generate_stud_region(rc, 7)
    });
    (genesis, calls)
}

fn stored_spawner(store: &world_store::StoreHandle, id: RegionCoords, genesis: StudGenesis) -> ThreadRegionSpawner {
    spawner(genesis).with_store(server::claims::HostClaims::claim_around(store.clone(), id, 0).unwrap())
}

/// Stops the region and returns its last tick and final portable snapshot.
fn stop(send: &Sender<RegionInput>, recv: &Receiver<(RegionCoords, RegionOutput)>) -> (game::Tick, game::PortableRegion) {
    send.send(RegionInput::Shutdown).unwrap();
    loop {
        if let (_, RegionOutput::Stopped(_, _, tick, portable)) = recv.recv_timeout(WAIT).expect("region stops") {
            return (tick, *portable);
        }
    }
}

/// Commits `portable` as region `id`'s save, as a cut of the host would.
fn save(store: &world_store::StoreHandle, id: RegionCoords, cut: u64, tick: game::Tick, portable: game::PortableRegion) {
    let claims = server::claims::HostClaims::claim_around(store.clone(), id, 0).unwrap();
    let cut = game::WorldCut {
        id: game::CutId(cut),
        host: 0,
        tick,
        regions: vec![(id, portable)],
        in_flight: vec![],
        players: vec![],
        history: None,
    };
    claims.commit_cut(cut).recv().unwrap().expect("the cut commits");
}

fn stud_chunks(r: &Rollback) -> std::collections::BTreeMap<StudChunkCoords, game::StudChunk> {
    r.stud_index.iter().map(|(c, _)| (*c, r.stud_chunk_at(*c).unwrap().clone())).collect()
}

/// A stored world on stud terrain: a fresh region is seeded from recipes, edited and
/// saved; a new process (a new spawner on the same store) restores it from the store
/// with its edits and its implied ground, and never runs genesis for it again.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stored_stud_region_restores_its_edits() {
    let dir = scratch("restore");
    let store = open_store(&dir);
    let id = RegionCoords { x: 0, z: 0 };
    let (genesis, calls) = counted_genesis();
    let mut s = stored_spawner(&store, id, genesis.clone());
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));

    // A shaft from the surface of chunk column (0, 0) down past its surface layers,
    // and a brick in the air over it.
    let seed = worldgen::stud_gen::generate_stud_region(id, 7);
    let (col, g) = seed.ground.iter().find(|(c, _)| c.x == 0 && c.z == 0).expect("column (0, 0)");
    let top = g.surface(5, 5);
    let floor = (*g.surface_layers().start() - 1) * game::STUD_CHUNK_PLATES as i32 + 80;
    let air = StudChunkCoords { x: col.x, y: *g.surface_layers().end() + 3, z: col.z };
    let part = game::stud_catalog::STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
    let brick = game::StudBrick::part_at([3, 0, 3], part, 0, 4);
    // One carve spans at most a chunk's height: dig the shaft in steps.
    let step = game::STUD_CHUNK_PLATES as i32;
    let mut orders = Vec::new();
    let mut hi = top;
    while hi > floor {
        let lo = (hi - step).max(floor);
        orders.push(game::Order::CarveStud { lo: [4, lo, 4], hi: [6, hi, 6] });
        hi = lo;
    }
    orders.push(game::Order::PlaceStud { chunk: air, brick });
    for order in orders {
        send.send(RegionInput::Event(game::GameEventKind::Order(0, order))).unwrap();
    }
    let before = snapshot(&send, &recv);
    assert_eq!(before.stud_material_at([4, top - 1, 4]), None, "the shaft is dug");
    assert_eq!(before.stud_material_at([4, floor, 4]), None);
    assert!(before.stud_material_at([4, floor - 1, 4]).is_some(), "ground under the shaft");
    assert_eq!(before.stud_chunk_at(air).and_then(|c| c.brick_at([3, 0, 3])), Some(&brick));

    let (tick, portable) = stop(&send, &recv);
    s.reap(id);
    assert_eq!(portable.stud_ground.as_ref().map(Vec::len), Some(seed.ground.len()), "the first save carries the recipes");
    assert!(portable.stud_chunks.len() >= 3, "the dug chunks and the brick's: {:?}", portable.stud_chunks.keys());
    assert!(
        portable.stud_chunks.len() < before.stud_index.iter().count(),
        "untouched surface chunks are not stored"
    );
    save(&store, id, 1, tick, portable);
    drop(s);

    // "Restart": a new spawner on the same store.
    let mut s = stored_spawner(&store, id, genesis.clone());
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let after = snapshot(&send, &recv);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "genesis re-ran on a stored region");
    assert_eq!(after.body_scale(), BodyScale::STUD);
    assert_eq!(stud_chunks(&after), stud_chunks(&before), "the same chunks with the same bricks");
    for cell in [[4, top - 1, 4], [5, floor, 5], [4, floor - 1, 4], [7, top - 1, 7], [7, top, 7], [4, floor - 600, 4]] {
        assert_eq!(after.stud_material_at(cell), before.stud_material_at(cell), "cell {cell:?}");
    }
    assert_eq!(after.stud_chunk_at(air).and_then(|c| c.brick_at([3, 0, 3])), Some(&brick));
    for (c, g) in &seed.ground {
        let x = (c.x * game::STUD_CHUNK_STUDS + 20) as i32;
        let z = (c.z * game::STUD_CHUNK_STUDS + 20) as i32;
        assert!(after.stud_material_at([x, g.surface(20, 20) - 1, z]).is_some(), "top plate of {c:?}");
        assert!(after.stud_material_at([x, g.surface(20, 20), z]).is_none(), "air over {c:?}");
    }

    // Nothing changed since the restore: the next save writes no terrain.
    let (tick, portable) = stop(&send, &recv);
    s.reap(id);
    assert!(portable.stud_chunks.is_empty() && portable.stud_ground.is_none() && portable.ground.is_none());
    save(&store, id, 2, tick, portable);
    drop(s);
    let mut s = stored_spawner(&store, id, genesis);
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    assert_eq!(stud_chunks(&snapshot(&send, &recv)), stud_chunks(&before), "still there after a save with no rows");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let _ = stop(&send, &recv);
    s.reap(id);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stored stud region whose rows do not read back does not run: `LoadFailed`, and it
/// is never regenerated from recipes over its stored edits.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn damaged_stud_rows_stop_the_region() {
    let dir = scratch("damaged");
    let store = open_store(&dir);
    let id = RegionCoords { x: 0, z: 0 };
    let (genesis, calls) = counted_genesis();
    let mut s = stored_spawner(&store, id, genesis.clone());
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let part = game::stud_catalog::STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
    let chunk = StudChunkCoords { x: 0, y: 40, z: 0 };
    let order = game::Order::PlaceStud { chunk, brick: game::StudBrick::part_at([3, 0, 3], part, 0, 4) };
    send.send(RegionInput::Event(game::GameEventKind::Order(0, order))).unwrap();
    let (tick, portable) = stop(&send, &recv);
    s.reap(id);
    assert!(portable.stud_chunks.contains_key(&chunk));
    save(&store, id, 1, tick, portable);
    drop(s);
    store.shutdown();

    let url = world_store::StoreUrl::Sqlite(dir.join("w.sqlite"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (pool, _guard) =
            world_store::open_pool(&url, world_store::Dials::BUILT, world_store::OpenMode::Writer).await.unwrap();
        let world_store::Pool::Sqlite(p) = &pool else { unreachable!("a sqlite url") };
        sqlx::query("UPDATE stud_chunks SET data = x'01ff00ff' WHERE cy = 40").execute(p).await.unwrap();
        p.close().await;
    });

    let store = open_store(&dir);
    let mut s = stored_spawner(&store, id, genesis);
    let (_send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let why = loop {
        match recv.recv_timeout(WAIT).expect("the region answers") {
            (_, RegionOutput::LoadFailed(why)) => break why,
            (_, other) => panic!("a region with damaged stud rows ran: {other:?}"),
        }
    };
    assert!(why.contains("stud chunk"), "{why}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "genesis ran over stored data");
    drop(_send);
    s.reap(id);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Bricks carrying a tree id, per id, over a region's materialised chunks.
fn tree_bricks(r: &Rollback) -> std::collections::BTreeMap<u16, usize> {
    let mut out = std::collections::BTreeMap::new();
    for (c, _) in r.stud_index.iter() {
        for b in r.stud_chunk_at(*c).unwrap().bricks().filter(|b| b.data != 0) {
            *out.entry(b.data).or_default() += 1;
        }
    }
    out
}

fn stud_bodies(r: &Rollback) -> Vec<game::StudBody> {
    r.ecs.stud_body.iter().filter_map(|(_, v)| v.clone()).collect()
}

/// A generated region WITH TREES (stud world plan P6b) in a stored world: it is seeded
/// with every tree brick worldgen made; one tree is chopped (its top is a stud body in
/// flight, its cut's logs a pile at the stump); the save holds all of it, and a new
/// process restores exactly that — the standing trees, the stump, the falling body
/// with its bricks and drops (entity bundle format 2), the pile — without re-running
/// genesis.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn stored_stud_region_keeps_its_trees_and_a_falling_one() {
    let dir = scratch("trees");
    let store = open_store(&dir);
    // A region of the procedural world (seed 7) that has trees.
    let id = RegionCoords { x: 1, z: 0 };
    let seed = worldgen::stud_gen::generate_stud_region(id, 7);
    let mut want: std::collections::BTreeMap<u16, usize> = Default::default();
    for b in seed.features.iter().flat_map(|(_, bricks)| bricks) {
        *want.entry(b.data).or_default() += 1;
    }
    assert!(want.len() >= 2 && !want.contains_key(&0), "the region has trees: {}", want.len());
    let (genesis, calls) = counted_genesis();
    let mut s = stored_spawner(&store, id, genesis.clone());
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let fresh = snapshot(&send, &recv);
    assert_eq!(tree_bricks(&fresh), want, "every generated tree brick is in the seeded region");

    // Chop the first tree standing on the ground two levels over its foot's tile (a
    // brick of the foot may reach into the next level; so a stump is left).
    let n = game::STUD_CHUNK_STUDS as i32;
    let ground: std::collections::BTreeMap<(usize, usize), &game::StudColumnGround> =
        seed.ground.iter().map(|(c, g)| ((c.x, c.z), g)).collect();
    let (tree, foot) = seed
        .features
        .iter()
        .flat_map(|(c, bricks)| bricks.iter().map(move |b| (*c, b)))
        .filter(|(_, b)| b.material == game::MAT_WOOD)
        .map(|(c, b)| {
            let cell = [c.x as i32 * n + b.min[0] as i32, c.y * game::STUD_CHUNK_PLATES as i32 + b.min[1] as i32, c.z as i32 * n + b.min[2] as i32];
            (b.data, cell)
        })
        .find(|(_, cell)| ground[&((cell[0] / n) as usize, (cell[2] / n) as usize)].surface((cell[0] % n) as usize, (cell[2] % n) as usize) == cell[1])
        .expect("a tree stands on the ground");
    let (ts, tp) = (game::TILE_STUDS as i32, game::TILE_PLATES as i32);
    let tile = [foot[0].div_euclid(ts), foot[1].div_euclid(tp) + 2, foot[2].div_euclid(ts)];
    assert_eq!(fresh.stud_tree_at(tile), Some(tree), "the trunk is three tiles tall at least");
    let op = game::OpId { client: 0, seq: 1 };
    let area = game::TileBox::new(tile, tile);
    for order in [
        game::Order::Designate { op, kind: game::DesignationKind::Chop, priority: game::DEFAULT_PRIORITY, area },
        game::Order::DevCompleteAll,
    ] {
        send.send(RegionInput::Event(game::GameEventKind::Order(0, order))).unwrap();
    }
    let before = snapshot(&send, &recv);
    let standing = tree_bricks(&before);
    assert!(standing.get(&tree).is_some_and(|left| *left < want[&tree]), "a stump is left of the chopped tree");
    assert!(standing.iter().all(|(t, left)| *t == tree || *left == want[t]), "the other trees stand");
    let bodies = stud_bodies(&before);
    assert_eq!(bodies.len(), 1, "the chopped top is a stud body");
    assert!(bodies[0].model.bricks.iter().all(|b| b.data == tree));
    assert!(bodies[0].model.bricks.len() + standing[&tree] < want[&tree], "body + stump: the cut bricks are gone");

    let (tick, portable) = stop(&send, &recv);
    s.reap(id);
    assert!(portable.entities.iter().any(|b| b.carried.stud_body.is_some()), "the falling body is saved as a bundle");
    // Feature chunks differ from their recipe's pristine chunk, so each is stored whole
    // (the felled tree's own chunks may have gone back to pristine).
    assert!(2 * portable.stud_chunks.len() >= seed.features.len(), "{} rows", portable.stud_chunks.len());
    save(&store, id, 1, tick, portable);
    drop(s);

    let mut s = stored_spawner(&store, id, genesis);
    let (send, recv) = spawn(&mut s, id, RegionSeed::Deferred(None));
    let after = snapshot(&send, &recv);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "genesis re-ran on a stored region");
    assert_eq!(stud_chunks(&after), stud_chunks(&before), "the same chunks, trees and stump included");
    assert_eq!(tree_bricks(&after), standing);
    let restored = stud_bodies(&after);
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].model, bodies[0].model, "the falling body's bricks");
    assert_eq!((restored[0].policy, &restored[0].drops), (bodies[0].policy, &bodies[0].drops), "and what it will drop");
    assert_eq!(after.item_piles(), before.item_piles(), "the cut's logs at the stump");
    let _ = stop(&send, &recv);
    s.reap(id);
    store.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
