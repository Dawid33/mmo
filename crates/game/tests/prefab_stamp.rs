//! The runtime stamp (`PrefabCommand::Stamp`): the rollback bar, one write per chunk,
//! overlaps skipped, who may stamp what, and equality with the worldgen seed path.

use crate::common::prefabs::{arch, build_file, build_of, empty_build, part, resolved};
use crate::common::{r0, uniform_seed, FLOOR_PLATES};
use crossbeam::channel::Receiver;
use game::{
    prefab_name_key, stamp_build_into_seed, BuildBoxFile, BuildFile, CellBox, GameDataUpdate, GameDataUpdateKind,
    GameEventKind, MaterialName, Order, PrefabCommand, PrefabKey, PrefabRef, Region, ResolvedPrefab, Rollback,
    StudBrick, StudChunk, StudChunkCoords, StudRegionSeed, REGION_CHUNKS, STUD_CHUNK_PLATES, STUD_CHUNK_STUDS,
};

const RED: u16 = 4;
const FLOOR: i64 = FLOOR_PLATES as i64;
/// Mid chunk (1, 0, 1), away from the spawn column at studs (16, 16).
const MID: [i64; 3] = [40, FLOOR, 40];

fn region_from(seed: StudRegionSeed) -> (Region, Receiver<GameDataUpdate>) {
    crate::common::install_real_block_props();
    let (send, recv) = crossbeam::channel::unbounded();
    let mut r = Region::new(Rollback::new(Some(send.clone())), Some(send), r0(), None);
    r.seed_stud_world(seed);
    r.handle_event(GameEventKind::CreateClient(0)).unwrap();
    r.forget_last_event();
    (r, recv)
}

fn region() -> (Region, Receiver<GameDataUpdate>) {
    region_from(uniform_seed())
}

fn hash(r: &Region) -> u32 {
    game::state_hash(r.data())
}

fn prefab(r: &mut Region, client: game::ClientId, cmd: PrefabCommand) {
    r.handle_event(GameEventKind::Prefab(client, cmd)).unwrap();
}

fn register(r: &mut Region, p: &ResolvedPrefab) -> PrefabRef {
    prefab(r, 0, PrefabCommand::Register(Box::new(p.clone())));
    r.forget_last_event();
    PrefabRef::Registered(p.key())
}

fn stamp(r: &mut Region, prefab_ref: PrefabRef, anchor: [i64; 3], turn: u8) {
    prefab(r, 0, PrefabCommand::Stamp { prefab: prefab_ref, anchor, turn });
}

/// The stud chunk writes among the updates sent since the last call.
fn chunk_writes(rx: &Receiver<GameDataUpdate>) -> usize {
    rx.try_iter()
        .filter(|u| matches!(u.update_kind, GameDataUpdateKind::AddStudChunk(..) | GameDataUpdateKind::SetStudChunk(..)))
        .count()
}

fn brick_at(r: &Region, cell: [i64; 3]) -> Option<StudBrick> {
    let (n, h) = (STUD_CHUNK_STUDS as i64, STUD_CHUNK_PLATES as i64);
    let c = StudChunkCoords { x: (cell[0] / n) as usize, y: cell[1].div_euclid(h) as i32, z: (cell[2] / n) as usize };
    let local = [(cell[0] % n) as u8, cell[1].rem_euclid(h) as u8, (cell[2] % n) as u8];
    r.data().stud_chunk_or_pristine(c).and_then(|chunk| chunk.brick_at(local).copied())
}

/// Every chunk of the region near the ground, as the sim sees it (materialised or implied).
fn terrain(r: &Region) -> Vec<(StudChunkCoords, Option<StudChunk>)> {
    let mut out = Vec::new();
    for x in 0..REGION_CHUNKS {
        for z in 0..REGION_CHUNKS {
            for y in -1..=1 {
                let c = StudChunkCoords { x, y, z };
                out.push((c, r.data().stud_chunk_or_pristine(c).map(|c| c.into_owned()).filter(|c| !c.is_empty())));
            }
        }
    }
    out
}

fn red_cells(r: &Region) -> usize {
    terrain(r).iter().filter_map(|(_, c)| c.as_ref()).flat_map(|c| c.bricks()).filter(|b| b.material == RED).map(|b| b.cells().count()).sum()
}

/// A prefab with a pit: a carve box below the anchor, bricks in it and around it, a
/// stone box that partly overlaps the ground left standing.
fn cellar() -> ResolvedPrefab {
    resolved(&build_file(
        "cellar",
        BuildFile {
            parts: vec![part("3001", [0, -6, 0], 0), part("3040", [4, -6, 1], 1), part("3007", [-2, 0, -3], 0)],
            boxes: vec![BuildBoxFile { min: [-2, -3, 3], size: [8, 3, 3], material: MaterialName::Natural("stone".into()), data: 0 }],
            carve: vec![CellBox { min: [-2, -6, -2], size: [8, 6, 6] }],
            markers: Vec::new(),
        },
    ))
}

#[test]
fn stamp_lays_the_prefab_and_undo_restores_the_hash() {
    let (mut r, _rx) = region();
    let key = register(&mut r, &resolved(&arch()));
    let before = hash(&r);
    let ground = terrain(&r);
    stamp(&mut r, key, MID, 0);
    assert_eq!(brick_at(&r, MID).map(|b| b.material), Some(RED));
    assert_eq!(red_cells(&r), 2 * 24 + 48);
    assert_ne!(before, hash(&r));
    r.rollback_last_event();
    assert_eq!(before, hash(&r));
    assert_eq!(ground, terrain(&r));
}

#[test]
fn a_stamp_with_a_pit_undoes_to_the_same_hash() {
    let (mut r, _rx) = region();
    let key = register(&mut r, &cellar());
    let before = hash(&r);
    stamp(&mut r, key, MID, 2);
    assert!(brick_at(&r, [MID[0], MID[1] - 1, MID[2]]).is_none(), "the pit is open");
    r.rollback_last_event();
    assert_eq!(before, hash(&r));
}

#[test]
fn one_update_per_chunk() {
    let (mut r, rx) = region();
    let key = register(&mut r, &resolved(&arch()));
    let _ = chunk_writes(&rx);
    // x 60..68: the lintel and one post cross from chunk 1 into chunk 2.
    stamp(&mut r, key, [60, FLOOR, 40], 0);
    assert_eq!(red_cells(&r), 2 * 24 + 48);
    assert_eq!(chunk_writes(&rx), 2);
}

#[test]
fn overlapping_bricks_are_skipped_not_fatal() {
    let (mut r, rx) = region();
    let key = register(&mut r, &resolved(&arch()));
    stamp(&mut r, key, MID, 0);
    let (once, _) = (terrain(&r), chunk_writes(&rx));
    stamp(&mut r, key, MID, 0);
    assert_eq!(once, terrain(&r));
    assert_eq!(chunk_writes(&rx), 0, "a stamp that lays nothing writes nothing");
}

#[test]
fn unregistered_key_changes_nothing() {
    let (mut r, rx) = region();
    let (ground, _) = (terrain(&r), chunk_writes(&rx));
    stamp(&mut r, PrefabRef::Registered(PrefabKey(1)), MID, 0);
    stamp(&mut r, PrefabRef::Builtin(0xDEAD_BEEF), MID, 0);
    assert_eq!(ground, terrain(&r));
    assert_eq!(chunk_writes(&rx), 0);
}

#[test]
fn another_players_registration_is_not_usable() {
    let (mut r, rx) = region();
    r.handle_event(GameEventKind::CreateClient(1)).unwrap();
    let key = register(&mut r, &resolved(&arch()));
    let (ground, _) = (terrain(&r), chunk_writes(&rx));
    prefab(&mut r, 1, PrefabCommand::Stamp { prefab: key, anchor: MID, turn: 0 });
    assert_eq!(ground, terrain(&r));
    assert_eq!(chunk_writes(&rx), 0);
}

#[test]
fn a_bad_turn_or_an_anchor_outside_the_region_changes_nothing() {
    let (mut r, rx) = region();
    let key = register(&mut r, &resolved(&arch()));
    let (ground, _) = (terrain(&r), chunk_writes(&rx));
    let n = (REGION_CHUNKS * STUD_CHUNK_STUDS) as i64;
    stamp(&mut r, key, MID, 4);
    for anchor in [[-1, FLOOR, 40], [n, FLOOR, 40], [40, FLOOR, -1], [40, FLOOR, n], [40, i64::MAX, 40], [40, i64::MIN, 40]] {
        stamp(&mut r, key, anchor, 0);
    }
    assert_eq!(ground, terrain(&r));
    assert_eq!(chunk_writes(&rx), 0);
}

#[test]
fn builtin_ref_stamps_dev_arch() {
    crate::common::install_test_prefabs();
    let (mut r, _rx) = region();
    stamp(&mut r, PrefabRef::Builtin(prefab_name_key("dev-arch")), MID, 0);
    assert_eq!(red_cells(&r), 2 * 24 + 48);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn brick_under_a_body_is_skipped() {
    let (mut r, _rx) = region();
    let key = register(&mut r, &resolved(&arch()));
    // A body dropped onto the tile under the arch's first post, left to land.
    let tile = [(MID[0] / 2) as i32, FLOOR_PLATES / game::TILE_PLATES as i32, (MID[2] / 2) as i32];
    r.handle_event(GameEventKind::Order(0, Order::DevDropBody { at: tile })).unwrap();
    for _ in 0..120 {
        r.handle_event(GameEventKind::Tick).unwrap();
    }
    stamp(&mut r, key, MID, 0);
    // The 1 m cube stands where the first post goes and reaches up into the lintel.
    assert!(brick_at(&r, MID).is_none(), "the post under the body is left out");
    assert_eq!(brick_at(&r, [MID[0] + 6, MID[1], MID[2]]).map(|b| b.material), Some(RED), "the other post is laid");
    assert_eq!(red_cells(&r), 24);
}

#[test]
fn the_ground_a_stamped_pit_exposes_gets_colliders() {
    // A pit dug from plate 6 down to the bottom of layer 0: its floor is the top of
    // layer -1, whose chunk was only implied by the recipe (no entity, no colliders).
    let (mut r, _rx) = region();
    let key = register(&mut r, &cellar());
    let below = StudChunkCoords { x: 1, y: -1, z: 1 };
    assert!(r.data().stud_key_at(below).is_none());
    let anchor = [MID[0], 6, MID[2]];
    stamp(&mut r, key, anchor, 0);
    assert!(r.data().stud_key_at(below).is_some());
    let floor = brick_at(&r, [anchor[0], -1, anchor[2] + 3]).expect("ground under the pit");
    assert!(matches!(floor.shape, game::StudShape::Part { .. }), "the pit's floor is real bricks");
    let wall = brick_at(&r, [anchor[0] - 3, 3, anchor[2]]).expect("ground beside the pit");
    assert!(matches!(wall.shape, game::StudShape::Part { .. }), "the pit's wall is real bricks");
}

/// The same prefab, anchor and turn through the runtime stamp and through the seed:
/// every chunk of the two regions must be equal.
fn assert_runtime_matches_seed(p: &ResolvedPrefab, anchor: [i64; 3], turn: u8) {
    let (mut a, _rx) = region();
    let key = register(&mut a, p);
    stamp(&mut a, key, anchor, turn);
    let mut seed = uniform_seed();
    stamp_build_into_seed(&mut seed, r0(), build_of(p), anchor, turn);
    let (b, _rx) = region_from(seed);
    let (a, b) = (terrain(&a), terrain(&b));
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x, y, "{} at {anchor:?} turn {turn}: chunk {:?} differs", p.name, x.0);
    }
}

#[test]
fn runtime_and_seed_paths_lay_identical_bricks() {
    assert_runtime_matches_seed(&cellar(), MID, 3);
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn runtime_and_seed_paths_agree_at_chunk_and_layer_edges() {
    let on_chunk_edge = [62, FLOOR, 40];
    let on_layer_edge = [40, STUD_CHUNK_PLATES as i64 + 2, 70];
    for (p, anchor, turn) in [
        (resolved(&arch()), MID, 0),
        (resolved(&arch()), on_chunk_edge, 1),
        (cellar(), on_chunk_edge, 2),
        (cellar(), on_layer_edge, 1),
        (cellar(), [40, 6, 40], 0),
    ] {
        assert_runtime_matches_seed(&p, anchor, turn);
    }
}

#[test]
fn a_big_box_is_laid_whole() {
    // 40 x 6 x 8 of stone standing on the ground across two chunks.
    let p = resolved(&build_file(
        "wall",
        BuildFile {
            boxes: vec![BuildBoxFile { min: [-20, 0, 0], size: [40, 6, 8], material: MaterialName::Natural("basalt".into()), data: 0 }],
            parts: vec![part("3005", [0, 6, 0], 0)],
            ..empty_build()
        },
    ));
    let (mut r, rx) = region();
    let key = register(&mut r, &p);
    let _ = chunk_writes(&rx);
    stamp(&mut r, key, [64, FLOOR, 40], 0);
    let basalt = game::stud_material_id("basalt").unwrap();
    let cells: usize = terrain(&r).iter().filter_map(|(_, c)| c.as_ref()).flat_map(|c| c.bricks()).filter(|b| b.material == basalt).map(|b| b.cells().count()).sum();
    assert_eq!(cells, 40 * 6 * 8);
    assert_eq!(chunk_writes(&rx), 2);
}

/// Plate-thin boxes: each tiles into 576 bricks, so `n` of them lay `576 * n`.
fn plates(n: usize) -> ResolvedPrefab {
    let boxes = (0..n as i32)
        .map(|y| BuildBoxFile { min: [-48, y, -48], size: [96, 1, 96], material: MaterialName::Natural("basalt".into()), data: 0 })
        .collect();
    resolved(&build_file("plates", BuildFile { boxes, ..empty_build() }))
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_stamp_over_the_brick_cap_is_refused_whole() {
    // Valid as a prefab (30 boxes, well under the volume cap), far too many bricks to lay.
    let p = plates(30);
    assert!(p.validate(game::Trust::Player).is_ok());
    let game::ResolvedKind::Build(build) = &p.kind;
    assert!(game::layout_build(build, [48, FLOOR, 48], 0).brick_count() > game::STAMP_MAX_BRICKS);
    let (mut r, rx) = region();
    let key = register(&mut r, &p);
    let (ground, _) = (terrain(&r), chunk_writes(&rx));
    stamp(&mut r, key, [48, FLOOR, 48], 0);
    assert_eq!(ground, terrain(&r));
    assert_eq!(chunk_writes(&rx), 0);
    // Under the cap it is laid.
    let small = plates(2);
    let key = register(&mut r, &small);
    stamp(&mut r, key, [48, FLOOR, 48], 0);
    assert!(chunk_writes(&rx) > 0);
}

#[test]
fn a_neighbours_share_is_capped() {
    // A slab reaching 40 studs into the west neighbour: its bricks there would each be
    // an intent. Only the first few are sent; the rest are left out.
    let p = resolved(&build_file(
        "slab",
        BuildFile {
            boxes: vec![BuildBoxFile { min: [-48, 0, -48], size: [96, 9, 96], material: MaterialName::Natural("basalt".into()), data: 0 }],
            ..empty_build()
        },
    ));
    let (mut r, _rx) = region();
    let key = register(&mut r, &p);
    let _ = r.take_intents();
    stamp(&mut r, key, [8, FLOOR, 64], 0);
    let sent = r.take_intents().len();
    // 40 x 96 studs of three courses: 720 bricks on the neighbour's side.
    assert_eq!(sent, game::STAMP_MAX_NEIGHBOUR_INTENTS);
}

#[test]
fn a_hostile_anchor_does_not_overflow() {
    // In a region away from the origin the anchor is brought into its frame: with
    // overflow checks on, a wrapped subtraction would panic the region thread.
    for rc in [game::RegionCoords::new(3, -2), game::RegionCoords::new(-3, 2)] {
        crate::common::install_real_block_props();
        let mut r = Region::new(Rollback::new(None), None, rc, None);
        r.seed_stud_world(uniform_seed());
        r.handle_event(GameEventKind::CreateClient(0)).unwrap();
        let key = register(&mut r, &resolved(&arch()));
        for anchor in [[i64::MIN, FLOOR, 0], [i64::MAX, FLOOR, 0], [0, FLOOR, i64::MIN], [0, FLOOR, i64::MAX], [i64::MIN, i64::MIN, i64::MIN]] {
            stamp(&mut r, key, anchor, 0);
        }
        assert_eq!(red_cells(&r), 0);
    }
}
