use crossbeam::channel::unbounded;
use game::{
    Block, BlockId, Chunk, ChunkCoords, Column, ColumnCoords, GameEventKind, Region, RegionCoords,
    RegionInput, RegionOutput, RegionRunner, RegionSeed, SerializedRegion, SubChunk,
};
use std::hash::{Hash, Hasher};

fn flat_chunks() -> Vec<(ChunkCoords, Chunk)> {
    (0..2)
        .flat_map(|x| (0..2).map(move |z| (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8))))
        .collect()
}

fn flat_columns() -> Vec<(ColumnCoords, Column)> {
    let below = Block::new(BlockId::AIR);
    let above = Block::new(BlockId::AIR);
    (0..2)
        .flat_map(move |x| {
            (0..2).map(move |z| {
                let sub = SubChunk::Dense(Box::new(Chunk::flat_floor(8)));
                (ColumnCoords::new(x, z), Column::new(0, below, above, vec![sub]))
            })
        })
        .collect()
}

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

fn runner(id: RegionCoords) -> (RegionRunner, crossbeam::channel::Receiver<(RegionCoords, RegionOutput)>) {
    let (out_send, out_recv) = unbounded();
    let region = Region::from_chunks(id, flat_chunks());
    (RegionRunner::new(id, region, out_send), out_recv)
}

#[test]
fn tick_emits_event_processed() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.tick();
    let (rc, output) = out.try_recv().expect("tick output");
    assert_eq!(rc, id);
    let RegionOutput::EventProcessed(ev) = output else { panic!("expected EventProcessed") };
    assert_eq!(ev.kind, GameEventKind::Tick);
    assert_eq!(ev.region_id, id);
}

#[test]
fn sync_clock_every_ten_ticks() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..10 {
        r.tick();
    }
    let clocks = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::SyncClock { .. }))
        .count();
    assert_eq!(clocks, 1, "exactly one SyncClock in the first 10 ticks");
}

#[test]
fn create_client_event_is_processed_and_snapshot_includes_player() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    assert!(r.handle_input(RegionInput::Event(GameEventKind::CreateClient(7))));
    assert!(r.handle_input(RegionInput::RequestSnapshot(7)));
    let outputs: Vec<_> = out.try_iter().map(|(_, o)| o).collect();
    assert!(matches!(&outputs[0], RegionOutput::EventProcessed(ev) if ev.kind == GameEventKind::CreateClient(7)));
    let RegionOutput::Snapshot(client, rollback) = &outputs[1] else { panic!("expected Snapshot") };
    assert_eq!(*client, 7);
    assert!(rollback.player_entites.contains_key(&7), "FIFO: snapshot after CreateClient includes the player");
}

#[test]
fn shutdown_stops_and_park_restore_is_hash_exact() {
    let id = RegionCoords::new(1, 0);
    let (mut r, out) = runner(id);
    // Mutate state so the roundtrip is non-trivial.
    r.handle_input(RegionInput::Event(GameEventKind::CreateClient(3)));
    for _ in 0..5 {
        r.tick();
    }
    let before = {
        // Snapshot for hashing: same clone path the wire uses.
        r.handle_input(RegionInput::RequestSnapshot(0));
        let RegionOutput::Snapshot(_, rb) = out.try_iter().map(|(_, o)| o).last().unwrap() else {
            panic!()
        };
        let mut h = crc32fast::Hasher::new();
        rb.data.hash(&mut h);
        h.finalize()
    };

    assert!(!r.handle_input(RegionInput::Shutdown), "Shutdown must stop the runner");
    let RegionOutput::Stopped(serialized, _far) = out.try_iter().map(|(_, o)| o).last().unwrap()
    else {
        panic!("expected Stopped");
    };

    // Restore: the exact cycle-in path the manager uses.
    let restored = RegionSeed::Parked(serialized, flat_columns()).into_region(id);
    assert_eq!(before, crc(&restored), "hash(before park) == hash(after restore), bit-exact");
}

#[test]
fn corrupt_parked_blob_falls_back_to_generation() {
    let id = RegionCoords::new(2, 2);
    let garbage = SerializedRegion(vec![0xde, 0xad, 0xbe, 0xef]);
    let region = RegionSeed::Parked(garbage, flat_columns()).into_region(id);
    assert_eq!(region.data().ecs.entities.len(), 4, "fell back to the 2x2 fallback chunks");
}

#[test]
fn request_far_snapshot_replies_with_terrain() {
    let id = RegionCoords::new(0, 0);
    let (mut r, out) = runner(id);
    r.handle_input(RegionInput::RequestFarSnapshot(7));
    let (rc, output) = out.try_recv().expect("far snapshot output");
    assert_eq!(rc, id);
    let RegionOutput::FarSnapshot(client, snap) = output else {
        panic!("expected FarSnapshot")
    };
    assert_eq!(client, 7);
    assert_eq!(snap.chunks.len(), 4);
}

#[test]
fn stable_terrain_never_pushes_far_change() {
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    for _ in 0..(game::FAR_SNAPSHOT_MIN_INTERVAL_TICKS * 2 + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::FarSnapshotChanged(_)))
        .count();
    assert_eq!(pushes, 0, "unchanged terrain must not push a far refresh");
}

#[test]
fn terrain_edit_pushes_one_throttled_far_change() {
    use game::{Chunk, ChunkCoords};
    let (mut r, out) = runner(RegionCoords::new(0, 0));
    // Mutate terrain: add a new chunk (a flat floor) at y=1 via the test hook.
    r.region_mut().with_data(|d| {
        d.create_mesh(ChunkCoords::new(0, 1, 0), Chunk::flat_floor(8));
    });
    for _ in 0..(game::FAR_SNAPSHOT_MIN_INTERVAL_TICKS + 1) {
        r.tick();
    }
    let pushes = out
        .try_iter()
        .filter(|(_, o)| matches!(o, RegionOutput::FarSnapshotChanged(_)))
        .count();
    assert_eq!(pushes, 1, "one change → exactly one throttled push");
}
