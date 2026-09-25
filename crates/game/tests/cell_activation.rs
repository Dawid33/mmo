//! Cell loading inside one region — spec
//! docs/superpowers/specs/2026-09-25-island-regions-cell-loading-design.md.
use game::{state_hash, Region, RegionCoords};

pub fn r0() -> RegionCoords {
    RegionCoords::new(0, 0)
}

/// 8-chunk cells: a 4×4 grid in the default 32-chunk region.
pub const CC: usize = 8;

#[test]
fn legacy_regions_are_not_in_cell_mode() {
    let r = Region::from_chunks(r0(), Vec::new());
    assert!(!r.data().cells.enabled());
    assert_eq!(r.data().cells.cell_chunks, 0);
}

#[test]
fn a_cell_mode_region_starts_empty() {
    let r = Region::new_cells(r0(), CC, None, None);
    assert!(r.data().cells.enabled());
    assert!(r.data().cells.active.is_empty());
    assert_eq!(r.data().ecs.chunk.iter().filter(|(_, c)| c.is_some()).count(), 0);
}

#[test]
fn cell_state_is_part_of_the_state_hash() {
    let a = Region::new_cells(r0(), CC, None, None);
    let b = Region::from_chunks(r0(), Vec::new());
    assert_ne!(state_hash(a.data()), state_hash(b.data()));
}

use game::{
    CellChanges, CellCoords, CellPayload, Chunk, ChunkCoords, DormantCell, GameError,
    GameEventKind,
};

/// Two flat-floor chunks at the corner of a cell.
pub fn cell_content(cell: CellCoords) -> DormantCell {
    let (x, z) = (cell.x as usize * CC, cell.z as usize * CC);
    DormantCell {
        chunks: vec![
            (ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)),
            (ChunkCoords::new(x + 1, 0, z), Chunk::flat_floor(4)),
        ],
        entities: vec![],
    }
}

pub fn activate(cells: &[CellCoords]) -> GameEventKind {
    GameEventKind::CellChanges(CellChanges {
        deactivate: vec![],
        activate: cells
            .iter()
            .map(|&cell| CellPayload { cell, version: 1, materialized_hash: 0, content: cell_content(cell) })
            .collect(),
    })
}

pub fn deactivate(cells: &[CellCoords]) -> GameEventKind {
    GameEventKind::CellChanges(CellChanges { deactivate: cells.to_vec(), activate: vec![] })
}

/// Server-side apply: handle + forget, returning the broadcast event.
pub fn server_apply(r: &mut Region, kind: GameEventKind) -> game::GameEvent {
    let ev = r.handle_event(kind).unwrap();
    r.forget_last_event();
    ev
}

#[test]
fn server_fills_the_materialized_hash() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let ev = server_apply(&mut s, activate(&[CellCoords::new(1, 2)]));
    let GameEventKind::CellChanges(c) = ev.kind else { panic!() };
    assert_ne!(c.activate[0].materialized_hash, 0);
    assert_eq!(c.activate[0].materialized_hash, s.data().cell_hash(CellCoords::new(1, 2)));
    assert!(s.data().cells.active.contains(&CellCoords::new(1, 2)));
}

#[test]
fn a_client_replica_matches_the_server_bit_exact() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    for step in [
        activate(&[CellCoords::new(0, 0), CellCoords::new(3, 3)]),
        deactivate(&[CellCoords::new(0, 0)]),
        activate(&[CellCoords::new(0, 0)]),
    ] {
        let ev = server_apply(&mut s, step);
        c.handle_event(ev.kind).unwrap();
        c.forget_last_event();
        assert_eq!(state_hash(s.data()), state_hash(c.data()));
    }
}

#[test]
fn a_tampered_hash_is_rejected_by_a_client() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let ev = server_apply(&mut s, activate(&[CellCoords::new(2, 2)]));
    let GameEventKind::CellChanges(mut changes) = ev.kind else { panic!() };
    changes.activate[0].materialized_hash ^= 1;
    match c.handle_event(GameEventKind::CellChanges(changes)) {
        Err(GameError::CellHashMismatch(cell)) => assert_eq!(cell, CellCoords::new(2, 2)),
        other => panic!("expected CellHashMismatch, got {other:?}"),
    }
}

#[test]
fn dematerialize_returns_the_content_and_empties_the_cell() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let cell = CellCoords::new(1, 1);
    server_apply(&mut s, activate(&[cell]));
    server_apply(&mut s, deactivate(&[cell]));
    assert!(!s.data().cells.active.contains(&cell));
    assert!(s.data().cell_chunk_keys(cell).is_empty());
    let dormant = s.take_dormant();
    assert_eq!(dormant.len(), 1);
    assert_eq!(dormant[0].0, cell);
    assert_eq!(dormant[0].1, cell_content(cell));
    assert!(s.take_dormant().is_empty(), "drained");
}

#[test]
fn reactivation_round_trips_content() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    let cell = CellCoords::new(0, 3);
    server_apply(&mut s, activate(&[cell]));
    let before = s.data().terrain_snapshot().terrain_hash();
    server_apply(&mut s, deactivate(&[cell]));
    let (_, content) = s.take_dormant().remove(0);
    server_apply(
        &mut s,
        GameEventKind::CellChanges(CellChanges {
            deactivate: vec![],
            activate: vec![CellPayload { cell, version: 2, materialized_hash: 0, content }],
        }),
    );
    assert_eq!(before, s.data().terrain_snapshot().terrain_hash());
}

#[test]
fn a_cell_holding_a_player_is_not_deactivated() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    // Cell mode spawns in the region's centre cell (player_spawn_xz).
    let (sx, sz) = game::player_spawn_xz(true);
    let spawn = game::cell_of_pos(sx, sz, CC).unwrap();
    server_apply(&mut s, activate(&[spawn]));
    server_apply(&mut s, GameEventKind::CreateClient(0));
    server_apply(&mut s, deactivate(&[spawn]));
    assert!(s.data().cells.active.contains(&spawn));
    assert!(s.data().player_entites.contains_key(&0));
    assert!(s.take_dormant().is_empty());
}

use game::parry::math::{Real, Vector};

fn slime_in_cell_2_2(s: &mut Region) {
    // Centre of dormant cell (2,2), well above ground.
    let at = 2.5 * game::cell_size_units(CC);
    s.with_data(|rb| {
        rb.spawn_slime_safe(
            Vector::new(Real::from(at), Real::from(40.0), Real::from(at)),
            1,
            Real::from(0.0),
            0,
        );
    });
}

#[test]
fn a_mob_in_a_dormant_cell_is_frozen_by_the_tick() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    slime_in_cell_2_2(&mut s);
    server_apply(&mut s, GameEventKind::Tick);
    let frozen = s.take_frozen();
    assert_eq!(frozen.len(), 1);
    assert_eq!(frozen[0].0, CellCoords::new(2, 2));
    assert_eq!(s.data().ecs.slime.iter().filter(|(_, x)| x.is_some()).count(), 0);
}

#[test]
fn freezing_a_stray_rolls_back_bit_exact() {
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    slime_in_cell_2_2(&mut s);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let before = state_hash(c.data());
    c.handle_event(GameEventKind::Tick).unwrap();
    assert_eq!(c.take_frozen().len(), 1, "the client predicts the freeze too");
    c.rollback_last_event();
    assert_eq!(before, state_hash(c.data()));
}

#[test]
fn edits_in_dormant_cells_are_refused() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    let block = game::Block::new(game::BlockId(1));
    let mut placed_dormant = true;
    let mut placed_active = false;
    s.with_data(|rb| {
        // Cell (3,3) starts at chunk 24 = block 48.
        placed_dormant = rb.set_block_safe([50, 3, 50], block);
        placed_active = rb.set_block_safe([3, 20, 3], block);
    });
    assert!(!placed_dormant, "no chunk may be created in a dormant cell");
    assert!(placed_active);
    assert!(s.data().cell_chunk_keys(CellCoords::new(3, 3)).is_empty());
}

#[test]
fn slime_spawns_are_gated_to_active_cells() {
    let mut s = Region::new_cells(r0(), CC, None, None);
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    // Voxel (100,100) is cell (0,0); voxel (700,700) is cell (2,2).
    assert!(s.data().slime_spawn_allowed(100, 100));
    assert!(!s.data().slime_spawn_allowed(700, 700));
    let legacy = Region::from_chunks(r0(), Vec::new());
    assert!(legacy.data().slime_spawn_allowed(700, 700), "legacy regions are ungated");
}

use crossbeam::channel::{unbounded, Receiver};
use game::{CellConfig, CellStore, RegionInput, RegionOutput, RegionRunner};

/// A flat 4×4-cell region: one floor chunk per column at y = 0.
pub fn flat_store() -> CellStore {
    let mut chunks = Vec::new();
    for x in 0..game::REGION_CHUNKS {
        for z in 0..game::REGION_CHUNKS {
            chunks.push((ChunkCoords::new(x, 0, z), Chunk::flat_floor(8)));
        }
    }
    CellStore::from_chunks(chunks, CC)
}

pub fn cell_runner() -> (RegionRunner, Receiver<(RegionCoords, RegionOutput)>) {
    let (out, rx) = unbounded();
    let runner = RegionRunner::new_with_cells(
        r0(),
        Region::new_cells(r0(), CC, None, None),
        out,
        flat_store(),
        CellConfig::new(CC),
    );
    (runner, rx)
}

pub fn processed(rx: &Receiver<(RegionCoords, RegionOutput)>) -> Vec<game::GameEvent> {
    rx.try_iter()
        .filter_map(|(_, o)| match o {
            RegionOutput::EventProcessed(e) => Some(e),
            _ => None,
        })
        .collect()
}

#[test]
fn create_client_activates_the_spawn_cells_first() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    let evs = processed(&rx);
    assert!(matches!(evs[0].kind, GameEventKind::CellChanges(_)));
    assert!(matches!(evs[1].kind, GameEventKind::CreateClient(0)));
    // Spawn is the centre cell: its full 3×3 is required.
    assert_eq!(runner.region_mut().data().cells.active.len(), 9);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn ticks_grow_the_active_set_one_wanted_cell_at_a_time() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    let _ = processed(&rx);
    let mut sizes = Vec::new();
    for _ in 0..8 {
        runner.tick();
        sizes.push(runner.region_mut().data().cells.active.len());
    }
    // Radius 2 from the centre cell (2,2) covers the whole 4×4 grid: 9
    // required, then +1 wanted cell per tick.
    assert_eq!(sizes, vec![10, 11, 12, 13, 14, 15, 16, 16]);
    let evs = processed(&rx);
    let cell_events = evs.iter().filter(|e| matches!(e.kind, GameEventKind::CellChanges(_))).count();
    assert_eq!(cell_events, 7, "no empty CellChanges once the set is complete");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn deactivated_cells_go_back_to_the_store_with_a_new_version_only_if_changed() {
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    for _ in 0..8 {
        runner.tick();
    }
    // Remove the player: everything releases after the grace period.
    runner.region_mut().with_data(|rb| {
        let key = *rb.data.player_entites.get(&0).unwrap();
        rb.remove_entity_safe(key, Some(0));
        rb.data.player_entites.remove(&0);
    });
    for _ in 0..game::CELL_RELEASE_GRACE_TICKS + 2 {
        runner.tick();
    }
    assert!(runner.region_mut().data().cells.active.is_empty());
    // Untouched terrain: versions stay 0.
    assert_eq!(runner.cell_store().unwrap().version(CellCoords::new(0, 0)), Some(0));
    let _ = processed(&rx);
}

#[test]
fn a_cell_missing_from_the_store_is_skipped_not_fatal() {
    let (out, _rx) = unbounded();
    let mut runner = RegionRunner::new_with_cells(
        r0(),
        Region::new_cells(r0(), CC, None, None),
        out,
        // A 1-cell grid: only (0,0) exists; the runner asks for (0,1), (1,1)…
        CellStore::from_chunks(Vec::new(), game::REGION_CHUNKS),
        CellConfig::new(CC),
    );
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    runner.tick();
    assert!(runner.region_mut().data().cells.active.contains(&CellCoords::new(0, 0)));
    assert!(!runner.region_mut().data().cells.active.contains(&CellCoords::new(1, 1)));
}

use game::InputEvent;

/// Client joined from the runner's current state, predicting ahead of the
/// server and reconciling every server event.
struct Harness {
    runner: RegionRunner,
    rx: Receiver<(RegionCoords, RegionOutput)>,
    client: Region,
    _recv: Receiver<game::GameDataUpdate>,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(CellConfig::new(CC))
    }

    fn with_config(config: CellConfig) -> Self {
        game::set_hash_mode(game::HashMode::Full);
        let (out, rx) = unbounded();
        let mut runner = RegionRunner::new_with_cells(
            r0(),
            Region::new_cells(r0(), CC, None, None),
            out,
            flat_store(),
            config,
        );
        runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
        let _ = processed(&rx);
        let (send, recv) = unbounded();
        let snapshot = runner.region_mut().data().clone();
        let client = Region::new(snapshot, Some(send), r0(), Some(0));
        Self { runner, rx, client, _recv: recv }
    }

    /// Client predicts up to `lead` events, then the server runs one tick and
    /// the client reconciles everything it emitted.
    fn step(&mut self, lead: usize, input: Option<InputEvent>) {
        if let Some(i) = input {
            self.client.handle_event(GameEventKind::PlayerInput(0, i.clone())).unwrap();
            self.runner.handle_input(RegionInput::Event(GameEventKind::PlayerInput(0, i)));
        }
        while self.client.pending_event_ids().len() < lead {
            self.client.handle_event(GameEventKind::Tick).unwrap();
        }
        self.runner.tick();
        for ev in processed(&self.rx) {
            self.client.reconcile(ev).unwrap();
        }
    }

    /// Server catches up with every prediction, then the hashes must match.
    fn settle_and_compare(&mut self) {
        for _ in 0..8 {
            if self.client.pending_event_ids().is_empty() {
                break;
            }
            self.runner.tick();
            for ev in processed(&self.rx) {
                self.client.reconcile(ev).unwrap();
            }
        }
        assert!(self.client.pending_event_ids().is_empty(), "client still ahead");
        assert_eq!(
            state_hash(self.runner.region_mut().data()),
            state_hash(self.client.data()),
            "client replica diverged"
        );
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_predicting_client_converges_while_cells_load() {
    let mut h = Harness::new();
    for _ in 0..8 {
        h.step(2, None);
    }
    h.settle_and_compare();
    assert_eq!(h.client.data().cells.active.len(), 16);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_client_with_no_pending_predictions_still_applies_cell_changes() {
    let mut h = Harness::new();
    // No client prediction at all: server events park in the input buffer,
    // then apply once the client predicts again.
    h.runner.tick();
    let evs = processed(&h.rx);
    assert!(evs.iter().any(|e| matches!(e.kind, GameEventKind::CellChanges(_))));
    for ev in evs {
        h.client.reconcile(ev).unwrap();
    }
    for _ in 0..3 {
        h.step(2, None);
    }
    h.settle_and_compare();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn walking_across_cells_converges_for_every_seed() {
    use game::Key;
    for seed in 0..8u64 {
        let mut h = Harness::new();
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let player_cell = |h: &Harness| {
            let (x, z) = h.client.data().player_positions()[0];
            game::cell_of_pos(x, z, CC)
        };
        let start = player_cell(&h);
        // Enter fps mode, then hold W with random prediction leads.
        h.step(1, Some(InputEvent::FpsCam(true)));
        h.step(1, Some(InputEvent::Key { key: Key::KeyW, pressed: true }));
        for _ in 0..100 {
            let lead = 1 + (next() % 3) as usize;
            h.step(lead, None);
        }
        h.settle_and_compare();
        assert_eq!(h.client.data().player_positions().len(), 1, "seed {seed}: player stayed in the region");
        let end = player_cell(&h);
        assert_ne!(start, end, "seed {seed}: the walk must cross a cell edge");
        let active = &h.client.data().cells.active;
        for c in game::required_cells(h.client.data().player_positions()[0], CC) {
            assert!(active.contains(&c), "seed {seed}: required cell {c:?} active after the walk");
        }
    }
}

#[test]
fn a_legacy_region_ignores_cell_changes() {
    let mut r = Region::from_chunks(r0(), Vec::new());
    let ev = r.handle_event(activate(&[CellCoords::new(0, 0)]));
    assert!(ev.is_ok(), "no panic, no error: {ev:?}");
    assert!(r.data().cells.active.is_empty());
    assert_eq!(r.data().ecs.chunk.iter().filter(|(_, c)| c.is_some()).count(), 0);
}

/// 1-chunk (2-block) cells. The player stands in cell (4,5) looking −Z
/// (yaw 0) across cell (4,4) at a two-chunk-tall wall in cell (4,3); a
/// right click places against the wall's +Z face, i.e. at block z = 8, which
/// is inside cell (4,4). `middle_active` controls whether (4,4) is active.
fn place_across_a_cell(middle_active: bool) -> Region {
    use game::{InputEvent, IsometryReal, Key, MouseButton};
    let cell = |x, z| CellCoords::new(x, z);
    let solid = || Chunk::flat_floor(32);
    let payload = |c: CellCoords, chunks: Vec<(ChunkCoords, Chunk)>| CellPayload {
        cell: c,
        version: 1,
        materialized_hash: 0,
        content: DormantCell { chunks, entities: vec![] },
    };
    let mut activate = vec![
        payload(cell(4, 3), vec![(ChunkCoords::new(4, 0, 3), solid()), (ChunkCoords::new(4, 1, 3), solid())]),
        payload(cell(4, 5), vec![(ChunkCoords::new(4, 0, 5), solid())]),
    ];
    if middle_active {
        activate.push(payload(cell(4, 4), vec![]));
    }
    activate.sort_by_key(|p| p.cell);
    let mut s = Region::new_cells(r0(), 1, None, None);
    server_apply(&mut s, GameEventKind::CellChanges(CellChanges { deactivate: vec![], activate }));
    server_apply(&mut s, GameEventKind::CreateClient(0));
    let input = |ev| GameEventKind::PlayerInput(0, ev);
    server_apply(&mut s, input(InputEvent::Key { key: Key::KeyE, pressed: true }));
    server_apply(&mut s, input(InputEvent::Key { key: Key::KeyE, pressed: false }));
    // Block (8.5, _, 10.5): voxel (136, _, 168), dropped onto the floor.
    s.with_data(|rb| {
        let e = *rb.data.player_entites.get(&0).unwrap();
        rb.set_body_pose_safe(e, IsometryReal::translation(136.0.into(), 60.0.into(), 168.0.into()));
    });
    for _ in 0..12 {
        server_apply(&mut s, GameEventKind::Tick);
    }
    for ev in [
        input(InputEvent::MouseButton { button: MouseButton::Right, pressed: true }),
        GameEventKind::Tick,
        input(InputEvent::MouseButton { button: MouseButton::Right, pressed: false }),
        GameEventKind::Tick,
    ] {
        server_apply(&mut s, ev);
    }
    s
}

#[test]
fn placing_into_an_active_neighbour_cell_works() {
    // Control: proves the look ray reaches the wall across the middle cell.
    let s = place_across_a_cell(true);
    assert_eq!(s.data().cell_chunk_keys(CellCoords::new(4, 4)).len(), 1);
}

#[test]
fn a_player_cannot_place_into_a_dormant_cell() {
    let s = place_across_a_cell(false);
    assert!(
        s.data().cell_chunk_keys(CellCoords::new(4, 4)).is_empty(),
        "player_edit_safe created a chunk inside dormant cell (4,4)"
    );
}

/// Radius 0, no hysteresis, 1-tick grace: cells release as soon as the
/// player leaves their 3×3, so raw removal runs under client prediction.
fn eager_release() -> CellConfig {
    CellConfig { sim_radius: 0, hysteresis: 0, release_grace_ticks: 1, ..CellConfig::new(CC) }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn cells_deactivate_under_prediction_and_replicas_stay_bit_exact() {
    use game::Key;
    let mut h = Harness::with_config(eager_release());
    let mut released = 0usize;
    h.step(1, Some(InputEvent::FpsCam(true)));
    h.step(1, Some(InputEvent::Key { key: Key::KeyW, pressed: true }));
    for i in 0..100 {
        let before = h.client.data().cells.active.clone();
        h.step(1 + i % 3, None);
        released += before.difference(&h.client.data().cells.active).count();
    }
    h.settle_and_compare();
    assert_eq!(h.client.data().player_positions().len(), 1, "player stayed in the region");
    assert!(released > 0, "the walk must release at least one cell");
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_stray_freeze_under_prediction_converges() {
    // Radius 0: only the spawn cell's 3×3 (cells 1..=3) is active, so (0,0)
    // stays dormant. A slime there, placed on both replicas identically.
    let mut h = Harness::with_config(eager_release());
    let at = 0.5 * game::cell_size_units(CC);
    let spawn = |rb: &mut game::Rollback| {
        rb.spawn_slime_safe(Vector::new(Real::from(at), Real::from(40.0), Real::from(at)), 1, Real::from(0.0), 0);
    };
    h.runner.region_mut().with_data(spawn);
    h.client.with_data(spawn);
    assert_eq!(state_hash(h.runner.region_mut().data()), state_hash(h.client.data()));
    for _ in 0..6 {
        h.step(3, None);
    }
    h.settle_and_compare();
    assert_eq!(h.client.data().ecs.slime.iter().filter(|(_, x)| x.is_some()).count(), 0);
    assert_eq!(h.runner.cell_store().unwrap().version(CellCoords::new(0, 0)), Some(1));
}

fn render_kinds(recv: &Receiver<game::GameDataUpdate>) -> Vec<&'static str> {
    recv.try_iter()
        .map(|u| match u.update_kind {
            game::GameDataUpdateKind::CreateEntity(_) => "create",
            game::GameDataUpdateKind::SetEntityPosition(..) => "pos",
            game::GameDataUpdateKind::SetVoxelComponent(..) => "voxels",
            game::GameDataUpdateKind::RemoveEntity(_) => "remove",
            game::GameDataUpdateKind::AddTerrainChunk(..) => "terrain",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_client_tells_the_renderer_about_activated_and_released_terrain() {
    let (send, recv) = unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let _ = recv.try_iter().count();
    let ev = server_apply(&mut s, activate(&[CellCoords::new(1, 1)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    // cell_content has two chunks: one raw-chunk update each — voxels are
    // built off-thread by the renderer, never on the sim thread.
    assert_eq!(render_kinds(&recv), vec!["terrain", "terrain"]);
    let ev = server_apply(&mut s, deactivate(&[CellCoords::new(1, 1)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    assert_eq!(render_kinds(&recv), vec!["remove", "remove"]);
}

#[test]
fn a_reused_chunk_key_is_removed_before_it_is_recreated() {
    let (send, recv) = unbounded();
    let mut s = Region::new_cells(r0(), CC, None, None);
    let mut c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    let ev = server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    let _ = recv.try_iter().count();
    // One event: release (0,0), load (2,2) — the freed keys are reused.
    let ev = server_apply(
        &mut s,
        GameEventKind::CellChanges(CellChanges {
            deactivate: vec![CellCoords::new(0, 0)],
            activate: vec![CellPayload {
                cell: CellCoords::new(2, 2),
                version: 1,
                materialized_hash: 0,
                content: cell_content(CellCoords::new(2, 2)),
            }],
        }),
    );
    c.handle_event(ev.kind).unwrap();
    c.forget_last_event();
    let kinds = render_kinds(&recv);
    assert_eq!(&kinds[..2], &["remove", "remove"], "removals come first: {kinds:?}");
    assert_eq!(state_hash(s.data()), state_hash(c.data()));
}

#[test]
fn a_server_region_has_no_render_sink() {
    let s = Region::new_cells(r0(), CC, None, None);
    assert!(!s.data().data.has_render_sink(), "server regions derive and send nothing");
    let (send, _recv) = unbounded();
    let c = Region::new(s.data().clone(), Some(send), r0(), Some(0));
    assert!(c.data().data.has_render_sink());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn terrain_requests_include_dormant_cells() {
    // LOD builds read a region's terrain through RequestTerrain/TerrainChanged:
    // a cell region must report its dormant cells too, or released cells
    // turn into holes in the LOD rings.
    let (mut runner, rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    for _ in 0..8 {
        runner.tick();
    }
    runner.region_mut().with_data(|rb| {
        let key = *rb.data.player_entites.get(&0).unwrap();
        rb.remove_entity_safe(key, Some(0));
        rb.data.player_entites.remove(&0);
    });
    for _ in 0..game::CELL_RELEASE_GRACE_TICKS + 2 {
        runner.tick();
    }
    assert!(runner.region_mut().data().cells.active.is_empty());
    let _ = rx.try_iter().count();
    runner.handle_input(RegionInput::RequestTerrain(7));
    let terrain = rx
        .try_iter()
        .find_map(|(_, o)| match o {
            RegionOutput::Terrain(7, t) => Some(t),
            _ => None,
        })
        .expect("terrain reply");
    assert_eq!(
        terrain.chunks.len(),
        game::REGION_CHUNKS * game::REGION_CHUNKS,
        "every generated chunk, active or dormant"
    );
}

#[test]
fn a_runner_times_its_baseline_terrain_hash() {
    let (out, _rx) = unbounded();
    let runner = RegionRunner::new(r0(), Region::from_chunks(r0(), vec![(ChunkCoords::new(0, 0, 0), Chunk::flat_floor(8))]), out);
    assert!(runner.baseline_hash_time() > std::time::Duration::ZERO);
}

#[test]
fn a_cell_mode_player_spawns_in_the_region_centre_cell() {
    let (mut runner, _rx) = cell_runner();
    runner.handle_input(RegionInput::Event(GameEventKind::CreateClient(0)));
    let d = runner.region_mut().data();
    let (x, z) = d.player_positions()[0];
    let n = game::cells_per_side(CC);
    assert_eq!(game::cell_of_pos(x, z, CC), Some(CellCoords::new(n / 2, n / 2)));
    // Not clipped by the region edge: the full 3×3 is active around it.
    assert_eq!(d.cells.active.len(), 9);
    assert_eq!(game::player_spawn_xz(false), (game::PLAYER_SPAWN_X, game::PLAYER_SPAWN_Z));
}
