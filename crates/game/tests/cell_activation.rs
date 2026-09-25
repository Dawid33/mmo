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
    // PLAYER_SPAWN_X/Z = 128 voxels → cell (0,0) at 256-unit cells.
    server_apply(&mut s, activate(&[CellCoords::new(0, 0)]));
    server_apply(&mut s, GameEventKind::CreateClient(0));
    server_apply(&mut s, deactivate(&[CellCoords::new(0, 0)]));
    assert!(s.data().cells.active.contains(&CellCoords::new(0, 0)));
    assert!(s.data().player_entites.contains_key(&0));
    assert!(s.take_dormant().is_empty());
}
