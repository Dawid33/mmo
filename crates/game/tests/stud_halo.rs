//! The stud halo (`stud_halo.rs`, stud world plan P4): seeds derive the neighbour's
//! pristine edge locally, edited chunks travel, edits past the edge are intents, and
//! every new mutation meets the rollback bar. Regions are driven by hand here (events
//! in, updates and intents out); the scheduling side is `handoff_schedule::stud_*`.
use std::collections::BTreeMap;

use game::portable::SaveTerms;
use game::stud_catalog::STUD_PARTS;
use game::*;

const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };
/// Surface plate of the flat test ground (layer 0).
const TOP: i32 = 30;
const N: i32 = REGION_CHUNKS as i32;
const CW: i32 = STUD_CHUNK_STUDS as i32;

fn west() -> RegionCoords {
    RegionCoords::new(0, 0)
}

fn east() -> RegionCoords {
    RegionCoords::new(1, 0)
}

fn flat() -> StudColumnGround {
    StudColumnGround::flat(TOP, SOIL, MAT_STONE, -2000)
}

fn flat_seed() -> StudRegionSeed {
    let ground = (0..REGION_CHUNKS)
        .flat_map(|x| (0..REGION_CHUNKS).map(move |z| (ColumnCoords::new(x, z), flat())))
        .collect();
    StudRegionSeed { ground, features: Vec::new(), ..Default::default() }
}

/// A fresh stud region of flat ground.
fn region(rc: RegionCoords) -> Region {
    let mut region = Region::new(Rollback::new(None), Some(sink()), rc, None);
    region.seed_stud_world(flat_seed());
    region
}

/// A render sink nobody reads (a rollback needs one).
fn sink() -> crossbeam::channel::Sender<GameDataUpdate> {
    crossbeam::channel::unbounded().0
}

fn hash(r: &Region) -> u32 {
    state_hash(r.data())
}

fn brick(min: [u8; 3]) -> StudBrick {
    let part = STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
    StudBrick::part_at(min, part, 0, 4)
}

fn at(x: i32, y: i32, z: i32) -> StudChunkCoords {
    StudChunkCoords { x: x as usize, y, z: z as usize }
}

/// Applies `kind` for good.
fn apply(r: &mut Region, kind: GameEventKind) {
    r.handle_event(kind).unwrap();
    r.forget_last_event();
}

/// Checks the rollback bar of `kind` on a copy of the region (so what the event sends
/// is not sent twice), then applies it for good.
fn apply_checked(r: &mut Region, kind: GameEventKind, what: &str) {
    let mut probe = Region::new(r.data().clone(), Some(sink()), *r.data().region, None);
    let before = hash(&probe);
    probe.handle_event(kind.clone()).unwrap();
    let after = hash(&probe);
    probe.rollback_last_event();
    assert_eq!(hash(&probe), before, "{what}: undone exactly");
    assert_eq!(hash(r), before, "the copy is the region");
    r.handle_event(kind).unwrap();
    r.forget_last_event();
    assert_eq!(hash(r), after, "{what}: the same state on the copy and the region");
}

/// `from`'s seed for `to`, applied to `to`.
fn seed(from: &Region, to: &mut Region, to_rc: RegionCoords) {
    let seed = from.data().halo_seed_for(to_rc).expect("neighbours");
    apply(to, GameEventKind::HaloUpdate(seed));
}

/// Moves `from`'s pending halo updates and intents to `to` (its only neighbour here).
fn deliver(from: &mut Region, to: &mut Region, to_rc: RegionCoords) -> (usize, Vec<IntentMsg>) {
    let mut updates = 0;
    for (update, target) in from.take_halo_updates() {
        if target == to_rc {
            apply_checked(to, GameEventKind::HaloUpdate(update), "halo delta");
            updates += 1;
        }
    }
    let intents = from.take_intents();
    for msg in &intents {
        if msg.region_hint() == Some(to_rc) {
            apply_checked(to, GameEventKind::Intent(*msg), "intent receipt");
        }
    }
    (updates, intents)
}

fn tick(r: &mut Region) {
    apply_checked(r, GameEventKind::Tick, "tick");
}

/// Every stud chunk `owner` has in the columns `viewer` mirrors is in `viewer`'s halo
/// with the same bricks, and the halo holds nothing else of `owner`.
fn assert_halo_agrees(viewer: &Region, viewer_rc: RegionCoords, owner: &Region, owner_rc: RegionCoords) {
    let offset = (viewer_rc.x - owner_rc.x, viewer_rc.z - owner_rc.z);
    let mut want: BTreeMap<WorldChunkPos, StudChunk> = BTreeMap::new();
    for (c, _) in owner.data().stud_index.iter() {
        if game::halo::halo_targets(c.x as i32, c.z as i32).contains(&offset) {
            let chunk = owner.data().stud_chunk_at(*c).unwrap().clone();
            want.insert(WorldChunkPos::from_local(owner_rc, [c.x as i32, c.y, c.z as i32]), chunk);
        }
    }
    let have: BTreeMap<WorldChunkPos, StudChunk> = viewer
        .data()
        .stud_halo()
        .iter()
        .filter(|(k, _)| k.to_local().0 == owner_rc)
        .filter_map(|(k, h)| Some((*k, h.chunk.clone()?)))
        .collect();
    assert_eq!(have.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>(), "{viewer_rc:?} mirrors {owner_rc:?}'s chunks");
    assert!(have == want, "{viewer_rc:?}'s halo of {owner_rc:?} holds its bricks");
}

/// Enabled colliders on halo bodies, as shape hashes per chunk.
fn halo_collider_count(r: &Rollback, key: WorldChunkPos) -> usize {
    let body = r.stud_halo().get(&key).expect("a halo chunk").body;
    let colliders = r.physics.bodies.get(body).expect("its body").colliders();
    colliders.iter().filter(|&&ch| r.physics.colliders.get(ch).is_some_and(|c| c.is_enabled())).count()
}

#[test]
fn seed_is_recipes_only_and_the_receiver_derives_the_pristine_edge() {
    let east_region = region(east());
    let mut west_region = region(west());
    let update = east_region.data().halo_seed_for(west()).expect("neighbours");
    assert!(update.replace);
    assert!(update.studs.is_empty(), "an unedited edge sends no chunk");
    assert_eq!(update.stud_ground.len(), REGION_CHUNKS, "one recipe per facing chunk column");
    assert!(update.stud_ground.iter().all(|(k, _)| k.local_in(west())[0] == N && k.y == 0));

    let before = hash(&west_region);
    let bodies = west_region.data().physics.bodies.len();
    west_region.handle_event(GameEventKind::HaloUpdate(update.clone())).unwrap();
    assert_ne!(hash(&west_region), before);
    for z in 0..N {
        let chunk = west_region.data().stud_halo_chunk_at([N, 0, z]).expect("the neighbour's surface chunk");
        assert_eq!(Some(chunk), flat().pristine_chunk(0).as_ref());
        assert_eq!(halo_collider_count(west_region.data(), WorldChunkPos::from_local(west(), [N, 0, z])), stud_collider_shapes(chunk).len());
    }
    assert!(west_region.data().stud_halo_chunk_at([N, -1, 0]).is_none(), "below the surface layers nothing is materialised");
    assert_eq!(west_region.data().physics.bodies.len(), bodies + REGION_CHUNKS);
    // Never owner-side state: not indexed, not an entity.
    assert!(west_region.data().stud_index.keys().all(|c| c.x < REGION_CHUNKS && c.z < REGION_CHUNKS));
    assert_eq!(west_region.data().stud_index.len(), REGION_CHUNKS * REGION_CHUNKS);
    assert_halo_agrees(&west_region, west(), &east_region, east());

    west_region.rollback_last_event();
    assert_eq!(hash(&west_region), before, "the seed is undone exactly");
    assert_eq!(west_region.data().physics.bodies.len(), bodies);

    // Seeding twice changes nothing the second time.
    apply(&mut west_region, GameEventKind::HaloUpdate(update.clone()));
    let (seeded, bodies) = (west_region.data().stud_halo().clone(), west_region.data().physics.bodies.len());
    apply(&mut west_region, GameEventKind::HaloUpdate(update));
    assert!(*west_region.data().stud_halo() == seeded);
    assert_eq!(west_region.data().physics.bodies.len(), bodies);
}

#[test]
fn world_view_reads_studs_across_the_seam() {
    let east_region = region(east());
    let mut west_region = region(west());
    let edge = REGION_STUDS;
    {
        let view = west_region.data().view();
        assert_eq!(view.stud_material_at([edge - 1, (TOP - 1) as i64, 5]), Some(MAT_GRASS), "own ground");
        assert_eq!(view.stud_material_at([edge, (TOP - 1) as i64, 5]), None, "no halo yet");
    }
    seed(&east_region, &mut west_region, west());
    let view = west_region.data().view();
    assert_eq!(view.stud_material_at([edge, (TOP - 1) as i64, 5]), Some(MAT_GRASS), "the halo chunk");
    assert_eq!(view.stud_material_at([edge + 3, TOP as i64, 5]), None, "air over it");
    assert_eq!(view.stud_material_at([edge + 3, -500, 5]), Some(MAT_STONE), "the neighbour's implied ground, from its recipe");
    assert_eq!(view.stud_material_at([edge + CW as i64, 10, 5]), None, "past the band");
    assert_eq!(view.stud_material_at([-1, 10, 5]), None, "no neighbour seeded there");

    // A ray from this side, down into the neighbour's ground.
    let top_units = plate_units(TOP) as f64;
    let ray = WorldRay { origin: [REGION_SIZE as f64 - 14.0, top_units + 40.0, 100.0], dir: [0.70710677, -0.70710677, 0.0] };
    let hit = view.stud_raycast(ray, 100.0).expect("the neighbour's ground");
    assert_eq!(hit.chunk, WorldChunkPos::from_local(east(), [0, 0, 0]));
    assert_eq!((hit.cell[1], hit.normal), ((TOP - 1) as u8, [0, 1, 0]));
    assert_eq!(hit.cell[0], 3, "26 units past the edge is stud 3");
    let (chunk, b) = hit.placement(brick([0; 3]).shape_part(), 0, 4).expect("a placement in the neighbour");
    assert_eq!(chunk, WorldChunkPos::from_local(east(), [0, 0, 0]));
    assert_eq!(b.min[1], TOP as u8, "on top of the struck brick");
    // The same ray stops at the edge without the view (own chunks only).
    let own = raycast_studs(
        |c| west_region.data().stud_chunk_at(c),
        game::rapier3d::math::Vector::new(REGION_SIZE - 16.0, top_units as f32 + 40.0, 100.0),
        game::rapier3d::math::Vector::new(1.0, -1.0, 0.0),
        100.0,
    );
    assert!(own.is_none());
}

trait PartOf {
    fn shape_part(&self) -> u16;
}

impl PartOf for StudBrick {
    fn shape_part(&self) -> u16 {
        match self.shape {
            StudShape::Part { part, .. } => part,
            StudShape::Filler { .. } => panic!("a part"),
        }
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn an_edit_of_the_edge_travels_as_a_whole_chunk_delta() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    // A trench in the east's first chunk column, and one in its interior.
    apply(&mut east_region, GameEventKind::Order(0, Order::CarveStud { lo: [2, TOP - 6, 40], hi: [6, TOP, 44] }));
    apply(&mut east_region, GameEventKind::Order(0, Order::CarveStud { lo: [CW + 2, TOP - 6, 40], hi: [CW + 6, TOP, 44] }));
    let updates = east_region.take_halo_updates();
    assert_eq!(updates.len(), 1, "only the edge chunk has a mirror");
    let (update, target) = &updates[0];
    assert_eq!(*target, west());
    assert!(!update.replace && update.stud_ground.is_empty());
    assert_eq!(update.studs.len(), 1);
    assert_eq!(update.studs[0].chunk, WorldChunkPos::from_local(east(), [0, 0, 1]));
    apply_checked(&mut west_region, GameEventKind::HaloUpdate(update.clone()), "the delta");
    assert_halo_agrees(&west_region, west(), &east_region, east());
    let view = west_region.data().view();
    assert_eq!(view.stud_material_at([REGION_STUDS + 3, (TOP - 1) as i64, 41]), None, "the trench shows through the halo");
    assert_eq!(view.stud_material_at([REGION_STUDS + 3, (TOP - 7) as i64, 41]), Some(MAT_STONE));

    // A later seed of the edited neighbour lists just that chunk.
    let reseed = east_region.data().halo_seed_for(west()).unwrap();
    assert_eq!(reseed.studs.len(), 1);
    let mut fresh = region(west());
    apply_checked(&mut fresh, GameEventKind::HaloUpdate(reseed), "the seed of an edited neighbour");
    assert_halo_agrees(&fresh, west(), &east_region, east());
    assert!(fresh.data().stud_halo() == west_region.data().stud_halo(), "seed and seed + delta end equal");
}

#[test]
fn a_reseed_drops_what_the_neighbour_no_longer_has() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    // A brick in the air over the east's edge: a chunk in layer 4.
    east_region.with_data(|r| assert!(r.place_stud_safe(at(0, 4, 0), brick([3, 0, 3]))));
    seed(&east_region, &mut west_region, west());
    assert!(west_region.data().stud_halo_chunk_at([N, 4, 0]).is_some());
    // A regenerated east (as after a thread death) has no such chunk.
    let pristine = region(east());
    let update = pristine.data().halo_seed_for(west()).unwrap();
    apply_checked(&mut west_region, GameEventKind::HaloUpdate(update), "the reseed");
    assert!(west_region.data().stud_halo_chunk_at([N, 4, 0]).is_none());
    assert_halo_agrees(&west_region, west(), &pristine, east());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_carve_past_the_edge_is_an_intent_to_the_owner() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    seed(&west_region, &mut east_region, east());
    let edge = N * CW;
    // 4 studs on each side of the seam, 6 plates deep, 4 wide.
    let (lo, hi) = ([edge - 4, TOP - 6, 40], [edge + 4, TOP, 44]);
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::CarveStud { lo, hi }), "the order");
    assert_eq!(west_region.data().stud_material_at([edge - 1, TOP - 1, 41]), None, "own part carved at once");
    assert_eq!(west_region.data().view().stud_material_at([REGION_STUDS, (TOP - 1) as i64, 41]), Some(MAT_GRASS), "not the halo");

    let (_, intents) = deliver(&mut west_region, &mut east_region, east());
    let [IntentMsg::Intent(sent)] = intents[..] else { panic!("one intent, got {intents:?}") };
    assert_eq!(sent.target, Target::Stud(WorldChunkPos::from_local(east(), [0, 0, 1])));
    let Intent::CarveStud { lo: wl, hi: wh, .. } = sent.intent else { panic!("a carve") };
    assert_eq!((wl, wh), ([REGION_STUDS, (TOP - 6) as i64, 40], [REGION_STUDS + 4, TOP as i64, 44]));
    assert_eq!(east_region.data().stud_material_at([0, TOP - 1, 41]), Some(MAT_GRASS), "not before its due tick");
    // Due at the sender's next tick: the owner applies it at the opening of that tick.
    tick(&mut east_region);
    tick(&mut east_region);
    tick(&mut west_region);
    tick(&mut west_region);
    for x in 0..4 {
        for z in 40..44 {
            for y in TOP - 6..TOP {
                assert_eq!(east_region.data().stud_material_at([x, y, z]), None, "east cell {x},{y},{z}");
                assert_eq!(west_region.data().stud_material_at([edge - 1 - x, y, z]), None, "west cell");
            }
        }
    }
    assert_eq!(east_region.data().stud_material_at([4, TOP - 1, 41]), Some(MAT_GRASS), "nothing past the box");
    assert_eq!(east_region.data().stud_material_at([0, TOP - 7, 41]), Some(MAT_STONE));
    assert!(east_region.data().queued_intents().is_empty());
    // The hole comes back as a halo delta; the west's own edit went the other way.
    let (updates, replies) = deliver(&mut east_region, &mut west_region, west());
    assert_eq!(updates, 1);
    assert!(
        matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Granted(Intent::CarveStud { .. }), .. })]),
        "granted: {replies:?}"
    );
    deliver(&mut west_region, &mut east_region, east());
    assert_halo_agrees(&west_region, west(), &east_region, east());
    assert_halo_agrees(&east_region, east(), &west_region, west());
    // The reply to a bodiless sender comes back to its region and is dropped there.
    tick(&mut west_region);
    tick(&mut west_region);
    assert!(west_region.data().queued_replies().is_empty());
}

/// The re-brick rule (`game::stud_rebrick`) by brute force over one chunk of `layer`:
/// every filler cell with an open face neighbour — inside by occupancy, outside the
/// chunk by `outside_open` — has its slot turned into bricks.
fn rebrick_from_outside(chunk: &mut StudChunk, layer: i32, outside_open: impl Fn([i32; 3]) -> bool) {
    let (n, h) = (CW, STUD_CHUNK_PLATES as i32);
    let occ = chunk.occupancy();
    let mut slots = std::collections::BTreeSet::new();
    for b in chunk.bricks().filter(|b| matches!(b.shape, StudShape::Filler { .. })).copied().collect::<Vec<_>>() {
        for cell in b.cells() {
            let c = cell.map(i32::from);
            let open = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]].iter().any(|d| {
                let q = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                if (0..n).contains(&q[0]) && (0..h).contains(&q[1]) && (0..n).contains(&q[2]) { !occ.get(q) } else { outside_open(q) }
            });
            if open {
                slots.insert(rebrick_slot(layer, cell));
            }
        }
    }
    for (lo, hi) in slots {
        chunk.rebrick_box(lo, hi);
    }
}

/// A hole dug against the seam exposes the neighbour's ground: the neighbour turns
/// the slots behind the open cells into real bricks when the hole reaches its halo —
/// the bricks one region would have laid — and the bricks come back as a halo delta.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_hole_against_the_seam_re_bricks_the_neighbours_ground() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    seed(&west_region, &mut east_region, east());
    let edge = N * CW;
    // Deep in the filler: two studs against the seam, plates 6..12, z 40..42.
    let (lo, hi) = ([edge - 2, 6, 40], [edge, 12, 42]);
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::CarveStud { lo, hi }), "the dig");
    let is_part = |r: &Region, cell: [i32; 3]| {
        let c = at(cell[0] / CW, 0, cell[2] / CW);
        let local = [(cell[0] % CW) as u8, cell[1] as u8, (cell[2] % CW) as u8];
        r.data().stud_chunk_or_pristine(c).and_then(|ch| ch.brick_at(local).map(|b| matches!(b.shape, StudShape::Part { .. })))
    };
    assert_eq!(is_part(&west_region, [edge - 3, 8, 40]), Some(true), "the digger's own wall");
    assert_eq!(is_part(&east_region, [0, 8, 40]), Some(false), "the neighbour's ground is filler until it hears");
    let (updates, _) = deliver(&mut west_region, &mut east_region, east());
    assert_eq!(updates, 1);
    for y in 6..12 {
        for z in 40..42 {
            assert_eq!(is_part(&east_region, [0, y, z]), Some(true), "east wall cell {y},{z}");
        }
    }
    // Exactly the bricks of one region: the pristine chunk under the rule, the hole
    // past its west face open.
    let mut want = flat().pristine_chunk(0).unwrap();
    rebrick_from_outside(&mut want, 0, |q| q[0] == -1 && (6..12).contains(&q[1]) && (8..10).contains(&q[2]));
    assert!(want != flat().pristine_chunk(0).unwrap());
    assert!(east_region.data().stud_chunk_at(at(0, 0, 1)) == Some(&want), "the neighbour's bricks are one region's");
    // The re-bricked chunk travels back; both halos agree with their owners.
    let (updates, _) = deliver(&mut east_region, &mut west_region, west());
    assert_eq!(updates, 1);
    deliver(&mut west_region, &mut east_region, east());
    assert_halo_agrees(&west_region, west(), &east_region, east());
    assert_halo_agrees(&east_region, east(), &west_region, west());
}

#[test]
fn a_carve_wholly_in_the_neighbour_and_one_that_removes_nothing() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    let edge = N * CW;
    // Air over the neighbour: the intent is sent (the sender does not judge) and refused.
    apply(&mut west_region, GameEventKind::Order(0, Order::CarveStud { lo: [edge + 1, TOP + 3, 4], hi: [edge + 3, TOP + 6, 6] }));
    let (_, intents) = deliver(&mut west_region, &mut east_region, east());
    assert_eq!(intents.len(), 1);
    let before = hash(&east_region);
    tick(&mut east_region);
    tick(&mut east_region);
    let replies = east_region.take_intents();
    assert!(
        matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Refused(_, IntentRefusal::NothingThere), .. })]),
        "refused: {replies:?}"
    );
    assert!(east_region.take_halo_updates().is_empty(), "nothing was written");
    assert_ne!(hash(&east_region), before, "only the clock moved");
    // An oversized or empty box sends nothing at all.
    apply(&mut west_region, GameEventKind::Order(0, Order::CarveStud { lo: [edge - 40, 0, 0], hi: [edge + 40, 6, 6] }));
    apply(&mut west_region, GameEventKind::Order(0, Order::CarveStud { lo: [edge + 4, 0, 0], hi: [edge + 4, 6, 6] }));
    assert!(west_region.take_intents().is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_dig_at_the_seam_materialises_the_ground_behind_it() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    seed(&west_region, &mut east_region, east());
    // Deep in the east's layer -1, against the seam: the west's chunk across the face
    // is implied ground.
    assert!(west_region.data().stud_chunk_at(at(N - 1, -1, 0)).is_none());
    apply(&mut east_region, GameEventKind::Order(0, Order::CarveStud { lo: [0, -40, 4], hi: [2, -34, 6] }));
    assert!(east_region.data().stud_chunk_at(at(0, -1, 0)).is_some());
    let (updates, _) = deliver(&mut east_region, &mut west_region, west());
    assert_eq!(updates, 1);
    let opened = west_region.data().stud_chunk_at(at(N - 1, -1, 0)).expect("materialised behind the neighbour's hole");
    // Its wall behind the hole is real bricks, as one region would have laid them.
    let mut want = flat().pristine_chunk(-1).unwrap();
    let h = STUD_CHUNK_PLATES as i32;
    rebrick_from_outside(&mut want, -1, |q| q[0] == CW && (h - 40..h - 34).contains(&q[1]) && (4..6).contains(&q[2]));
    assert!(opened == &want);
    // And that chunk goes back as the west's own delta, so both halos are whole.
    deliver(&mut west_region, &mut east_region, east());
    assert_halo_agrees(&west_region, west(), &east_region, east());
    assert_halo_agrees(&east_region, east(), &west_region, west());

    // The same hole met by a seed instead (the west starts after the dig).
    let mut late = region(west());
    let update = east_region.data().halo_seed_for(west()).unwrap();
    apply_checked(&mut late, GameEventKind::HaloUpdate(update), "the seed of a dug neighbour");
    assert!(late.data().stud_chunk_at(at(N - 1, -1, 0)).is_some());
}

#[test]
fn a_placement_in_the_neighbour_is_an_intent_too() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    let target = WorldChunkPos::from_local(east(), [0, 0, 2]);
    let b = brick([0, TOP as u8, 5]);
    let before = hash(&west_region);
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::PlaceStudAt { chunk: target, brick: b }), "the order");
    assert_ne!(hash(&west_region), before, "the event counter moved");
    let (_, intents) = deliver(&mut west_region, &mut east_region, east());
    assert!(matches!(intents[..], [IntentMsg::Intent(SentIntent { target: Target::Stud(c), intent: Intent::PlaceStud { .. }, .. })] if c == target));
    tick(&mut east_region);
    tick(&mut east_region);
    assert_eq!(east_region.data().stud_chunk_at(at(0, 0, 2)).unwrap().brick_at([1, TOP as u8, 5]).map(|b| b.material), Some(4));
    let (updates, replies) = deliver(&mut east_region, &mut west_region, west());
    assert_eq!(updates, 1);
    assert!(matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Granted(_), .. })]));
    assert_halo_agrees(&west_region, west(), &east_region, east());
    assert_eq!(west_region.data().view().stud_material_at([REGION_STUDS + 1, TOP as i64, 2 * CW as i64 + 5]), Some(4));

    // The same brick again overlaps: refused by the owner.
    apply(&mut west_region, GameEventKind::Order(0, Order::PlaceStudAt { chunk: target, brick: b }));
    deliver(&mut west_region, &mut east_region, east());
    tick(&mut east_region);
    tick(&mut east_region);
    let replies = east_region.take_intents();
    assert!(
        matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Refused(_, IntentRefusal::Occupied), .. })]),
        "refused: {replies:?}"
    );

    // An own chunk is placed at once, with no intent.
    let own = WorldChunkPos::from_local(west(), [N - 1, 0, 2]);
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::PlaceStudAt { chunk: own, brick: b }), "an own placement");
    assert!(west_region.data().stud_chunk_at(at(N - 1, 0, 2)).unwrap().brick_at([1, TOP as u8, 5]).is_some());
    assert!(west_region.take_intents().is_empty());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn the_halo_is_never_stored_and_a_restore_starts_without_one() {
    let mut east_region = region(east());
    // An edit of the east's edge surface chunk, so the seed carries a chunk too.
    apply(&mut east_region, GameEventKind::Order(0, Order::CarveStud { lo: [2, TOP - 6, 40], hi: [6, TOP, 44] }));
    let mut plain = region(west());
    let mut with_halo = region(west());
    for r in [&mut plain, &mut with_halo] {
        apply(r, GameEventKind::Order(0, Order::CarveStud { lo: [4, TOP - 3, 4], hi: [6, TOP, 6] }));
    }
    seed(&east_region, &mut with_halo, west());
    assert!(!with_halo.data().stud_halo().is_empty() && !with_halo.data().stud_halo_grounds().is_empty());

    let (saved, _) = with_halo.data().portable_snapshot(&SaveTerms::default());
    let (reference, _) = plain.data().portable_snapshot(&SaveTerms::default());
    assert!(saved.stud_chunks == reference.stud_chunks, "the same stud rows with or without a halo");
    assert!(saved.stud_ground == reference.stud_ground, "and the same recipes");
    assert_eq!(saved.stud_chunks.len(), 1, "just the edited own chunk");
    assert!(saved.stud_chunks.keys().all(|c| c.x < REGION_CHUNKS && c.z < REGION_CHUNKS));
    assert_eq!(saved.stud_ground.as_ref().map(Vec::len), Some(REGION_CHUNKS * REGION_CHUNKS));

    let (restored, _) = restore_portable(west(), saved.into());
    assert!(restored.data().stud_halo().is_empty() && restored.data().stud_halo_grounds().is_empty());
    assert_eq!(restored.data().stud_index.len(), REGION_CHUNKS * REGION_CHUNKS);
    assert_eq!(restored.data().body_scale(), game::movement::BodyScale::STUD);
    // Reseeded at its start, it mirrors the neighbour again.
    let mut restored = restored;
    seed(&east_region, &mut restored, west());
    assert_halo_agrees(&restored, west(), &east_region, east());
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn every_seed_source_gives_the_same_halo() {
    // A neighbour with a surface edit, a deep pocket against its own chunk face (a
    // pristine chunk materialised below the surface) and a brick in the air.
    let mut east_region = region(east());
    east_region.with_data(|r| {
        assert!(!r.carve_stud_safe([2, TOP - 6, 40], [6, TOP, 44]).is_empty());
        assert!(!r.carve_stud_safe([10, -40, 30], [12, -34, 34]).is_empty());
        assert!(r.place_stud_safe(at(0, 4, 0), brick([3, 0, 3])));
    });
    let live = east_region.data().stud_edge();
    assert_eq!(live, east_region.terrain_snapshot().stud, "the terrain snapshot carries the edge");
    assert_eq!(live.ground.len(), 4 * REGION_CHUNKS - 4, "the edge columns only");

    // From the store: the stored rows of the same region.
    let (saved, _) = east_region.data().portable_snapshot(&SaveTerms::default());
    let stored: StoredRegion = saved.into();
    let from_store = StudEdge::from_stored(
        &stored.stud_ground,
        stored.stud_chunks.iter().cloned().map(|(c, record)| match record {
            StudChunkRecord::Pristine => (c, None),
            StudChunkRecord::Edited(chunk) => (c, Some(chunk)),
        }),
    );
    assert_eq!(from_store, live, "stored rows give the running region's edge");

    // From worldgen: a never-run neighbour is its seed.
    let fresh = region(east());
    assert_eq!(StudEdge::from_seed(&flat_seed()), fresh.data().stud_edge());

    // And the halos they seed are equal.
    let mut a = region(west());
    let mut b = region(west());
    seed(&east_region, &mut a, west());
    let snap = crate::common::stud_terrain_snapshot(from_store);
    let update = game::halo::halo_seed_from_snapshot(east(), west(), &snap).unwrap();
    apply(&mut b, GameEventKind::HaloUpdate(update));
    assert!(a.data().stud_halo() == b.data().stud_halo());
    assert_eq!(hash(&a), hash(&b));
    assert_halo_agrees(&a, west(), &east_region, east());

    // With trees (feature bricks: not a function of the recipes). A never-run
    // neighbour's seed is generated for the facing columns only (`RegionRunner::
    // seed_halo_from_worldgen`); with the windows' feature bricks in it, it gives the
    // halo the running neighbour gives — both sides of the seam hold the same trees.
    let tree = |min: [u8; 3], material: u16| StudBrick { material, data: 7, ..brick(min) };
    let mut treed = flat_seed();
    treed.features.push((at(0, 0, 1), vec![tree([4, TOP as u8, 4], game::MAT_WOOD), tree([4, TOP as u8 + 3, 4], game::MAT_LEAVES)]));
    treed.features.push((at(0, 0, 3), vec![tree([0, TOP as u8, 30], game::MAT_WOOD)]));
    treed.features.push((at(1, 0, 1), vec![tree([4, TOP as u8, 4], game::MAT_WOOD)]));
    let mut east_trees = Region::new(Rollback::new(None), Some(sink()), east(), None);
    east_trees.seed_stud_world(treed.clone());
    let (xs, zs) = game::halo::facing_columns(east(), west());
    let inside = |x: usize, z: usize| xs.contains(&x) && zs.contains(&z);
    let windowed = game::StudRegionSeed {
        ground: treed.ground.iter().filter(|(c, _)| inside(c.x, c.z)).cloned().collect(),
        features: treed.features.iter().filter(|(c, _)| inside(c.x, c.z)).cloned().collect(),
        ..Default::default()
    };
    let (mut c, mut d) = (region(west()), region(west()));
    seed(&east_trees, &mut c, west());
    let snap = crate::common::stud_terrain_snapshot(StudEdge::from_seed(&windowed));
    apply(&mut d, GameEventKind::HaloUpdate(game::halo::halo_seed_from_snapshot(east(), west(), &snap).unwrap()));
    assert!(c.data().stud_halo() == d.data().stud_halo());
    assert_eq!(hash(&c), hash(&d));
    assert_halo_agrees(&d, west(), &east_trees, east());
    let trees = |r: &Region| -> usize {
        r.data().stud_halo().values().filter_map(|h| h.chunk.as_ref()).flat_map(|c| c.bricks()).filter(|b| b.data == 7).count()
    };
    assert_eq!(trees(&d), 3, "the edge columns' tree bricks are in the halo, the interior one is not");
}

#[test]
fn features_in_the_edge_travel_with_a_worldgen_seed() {
    let mut seed = flat_seed();
    // A feature brick on the ground of the east's edge column (0, 1) and one inside.
    seed.features.push((at(0, 0, 1), vec![brick([4, TOP as u8, 4])]));
    seed.features.push((at(1, 0, 1), vec![brick([4, TOP as u8, 4])]));
    let mut east_region = Region::new(Rollback::new(None), Some(sink()), east(), None);
    east_region.seed_stud_world(seed.clone());
    let edge = StudEdge::from_seed(&seed);
    assert_eq!(edge, east_region.data().stud_edge());
    let listed: Vec<StudChunkCoords> = edge.chunks.iter().map(|(c, _)| *c).collect();
    assert_eq!(listed, vec![at(0, 0, 1)], "the edge chunk holding a feature, not the interior one");
}

/// The stud halo reaches a render sink (`SetStudHalo`, `SetStudHaloGround`): folding
/// the stream gives exactly the halo, and an undo takes it back out (P7a: the client
/// culls region-edge faces against it and reads tiles past the edge).
#[test]
fn the_render_stream_mirrors_the_stud_halo_do_and_undo() {
    let mut east_region = region(east());
    let (send, recv) = crossbeam::channel::unbounded();
    let mut west_region = Region::new(Rollback::new(None), Some(send), west(), None);
    west_region.seed_stud_world(flat_seed());

    let mut chunks: BTreeMap<WorldChunkPos, StudChunk> = BTreeMap::new();
    let mut grounds: BTreeMap<WorldChunkPos, StudColumnGround> = BTreeMap::new();
    let fold = |chunks: &mut BTreeMap<WorldChunkPos, StudChunk>, grounds: &mut BTreeMap<WorldChunkPos, StudColumnGround>| {
        for u in recv.try_iter() {
            match u.update_kind {
                GameDataUpdateKind::SetStudHalo(changes) => {
                    for (k, c) in changes {
                        match c {
                            Some(c) => chunks.insert(k, c),
                            None => chunks.remove(&k),
                        };
                    }
                }
                GameDataUpdateKind::SetStudHaloGround(changes) => {
                    for (k, g) in changes {
                        match g {
                            Some(g) => grounds.insert(k, g),
                            None => grounds.remove(&k),
                        };
                    }
                }
                _ => {}
            }
        }
    };
    let halo_of = |r: &Region| -> BTreeMap<WorldChunkPos, StudChunk> {
        r.data().stud_halo().iter().filter_map(|(k, h)| Some((*k, h.chunk.clone()?))).collect()
    };
    fold(&mut chunks, &mut grounds);
    assert!(chunks.is_empty() && grounds.is_empty(), "genesis mirrors nothing");

    // The seed: recipes, and the pristine edge chunks the receiver derives.
    seed(&east_region, &mut west_region, west());
    fold(&mut chunks, &mut grounds);
    assert_eq!(chunks.len(), REGION_CHUNKS, "one surface chunk per facing column");
    assert!(chunks == halo_of(&west_region));
    assert!(&grounds == west_region.data().stud_halo_grounds());

    // An edit of the owner's edge arrives as a delta — and is undone exactly.
    let c = at(0, 0, 3);
    apply(&mut east_region, GameEventKind::Order(1, Order::PlaceStud { chunk: c, brick: brick([0, TOP as u8, 4]) }));
    let (update, target) = east_region.take_halo_updates().into_iter().next().expect("a delta for the west");
    assert_eq!(target, west());
    let before = chunks.clone();
    west_region.handle_event(GameEventKind::HaloUpdate(update)).unwrap();
    fold(&mut chunks, &mut grounds);
    assert!(chunks != before && chunks == halo_of(&west_region), "the edited chunk reached the sink");
    let key = WorldChunkPos::from_local(east(), [0, 0, 3]);
    assert_eq!(chunks.get(&key), east_region.data().stud_chunk_at(c));
    west_region.rollback_last_event();
    fold(&mut chunks, &mut grounds);
    assert!(chunks == before && chunks == halo_of(&west_region), "the undo sent the prior chunk");
}

#[test]
fn a_removal_in_the_neighbour_is_an_intent_too() {
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    let target = WorldChunkPos::from_local(east(), [0, 0, 2]);
    let b = brick([0, TOP as u8, 5]);
    // The east builds a brick on its edge; the west sees it through its halo.
    apply(&mut east_region, GameEventKind::Order(0, Order::PlaceStudAt { chunk: target, brick: b }));
    deliver(&mut east_region, &mut west_region, west());
    let seen = [REGION_STUDS + 1, TOP as i64, 2 * CW as i64 + 5];
    assert_eq!(west_region.data().view().stud_material_at(seen), Some(4));

    // The world-addressed removal, ordered in the west: an intent to the owner.
    let cell = [1, TOP as u8, 5];
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::RemoveStudAt { chunk: target, cell }), "the order");
    let (_, intents) = deliver(&mut west_region, &mut east_region, east());
    assert!(
        matches!(intents[..], [IntentMsg::Intent(SentIntent { target: Target::Stud(c), intent: Intent::RemoveStud { .. }, .. })] if c == target),
        "{intents:?}"
    );
    assert!(east_region.data().stud_chunk_at(at(0, 0, 2)).unwrap().brick_at(cell).is_some(), "not before its due tick");
    tick(&mut east_region);
    tick(&mut east_region);
    assert!(east_region.data().stud_chunk_at(at(0, 0, 2)).unwrap().brick_at(cell).is_none(), "the owner removed it");
    let (updates, replies) = deliver(&mut east_region, &mut west_region, west());
    assert_eq!(updates, 1, "the gap comes back as a halo delta");
    assert!(matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Granted(Intent::RemoveStud { .. }), .. })]));
    assert_halo_agrees(&west_region, west(), &east_region, east());
    assert_eq!(west_region.data().view().stud_material_at(seen), None);

    // Nothing there any more: refused by the owner, nothing written.
    apply(&mut west_region, GameEventKind::Order(0, Order::RemoveStudAt { chunk: target, cell }));
    deliver(&mut west_region, &mut east_region, east());
    tick(&mut east_region);
    tick(&mut east_region);
    let replies = east_region.take_intents();
    assert!(
        matches!(replies[..], [IntentMsg::Reply(SentReply { reply: IntentReply::Refused(_, IntentRefusal::NothingThere), .. })]),
        "refused: {replies:?}"
    );
    assert!(east_region.take_halo_updates().is_empty());

    // An own brick goes at once, with no intent.
    let own = WorldChunkPos::from_local(west(), [N - 1, 0, 2]);
    apply(&mut west_region, GameEventKind::Order(0, Order::PlaceStudAt { chunk: own, brick: b }));
    apply_checked(&mut west_region, GameEventKind::Order(0, Order::RemoveStudAt { chunk: own, cell }), "an own removal");
    assert!(west_region.data().stud_chunk_at(at(N - 1, 0, 2)).unwrap().brick_at(cell).is_none());
    assert!(west_region.take_intents().is_empty());
}

/// Survival on stud terrain across a seam (P10a): an Adventure player at the west's
/// east edge mines a brick of the EAST region seen through the stud halo, then places
/// a brick against it — the `BreakStud` / `PlaceBrick` intents addressed to the
/// owner's stud chunk, applied by the owner, whose piles take the drop.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn survival_mining_and_placing_reach_across_the_seam() {
    const CLIENT: ClientId = 3;
    crate::common::install_real_block_props();
    let tiles = REGION_BLOCKS as i32;
    let stand = TOP / TILE_PLATES as i32;
    let mut east_region = region(east());
    let mut west_region = region(west());
    seed(&east_region, &mut west_region, west());
    seed(&west_region, &mut east_region, east());
    apply(&mut west_region, GameEventKind::CreateClient(CLIENT));
    let at_tile = [tiles - 2, stand, 20];
    apply(&mut west_region, GameEventKind::Order(CLIENT, Order::SetMode { mode: PlayMode::Adventure, at: Some(at_tile) }));
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::FpsCam(true)));
    west_region.with_data(|d| d.clients.get_mut(&CLIENT).unwrap().inventory = Inventory::new());
    // The two regions tick together (one world clock: an intent is due by it).
    for _ in 0..10 {
        tick(&mut west_region);
        tick(&mut east_region);
    }
    // A quarter turn to face +x (east), 45 degrees down: 0.0026 rad per pixel.
    let quarter = std::f32::consts::FRAC_PI_2 / 0.0026;
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::MouseMotion { dx: quarter, dy: 302.0 }));
    for _ in 0..3 {
        tick(&mut west_region);
        tick(&mut east_region);
    }
    let hit = west_region.data().eye_stud_hit(CLIENT).expect("the ground ahead");
    let studs = tiles * TILE_STUDS as i32;
    assert!(hit.lo[0] >= studs, "the brick under the crosshair is the east's: {hit:?}");
    let p = hit.lo;
    let east_cell = [p[0] - studs, p[1], p[2]];
    assert!(east_region.data().stud_material_at(east_cell).is_some());
    let need = west_region.data().stud_mine_ticks(p).expect("a halo brick is mined like an own one");
    assert_eq!(Some(need), east_region.data().stud_mine_ticks(east_cell), "the same break time on both sides");

    // --- Mine it: the break is sent on the tick before the last one held.
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::MouseButton { button: MouseButton::Left, pressed: true }));
    let mut sent = Vec::new();
    for _ in 0..need {
        tick(&mut west_region);
        tick(&mut east_region);
        let (_, intents) = deliver(&mut west_region, &mut east_region, east());
        sent.extend(intents);
        deliver(&mut east_region, &mut west_region, west());
    }
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::MouseButton { button: MouseButton::Left, pressed: false }));
    let (lo, hi) = (world_stud_cell(west(), hit.lo.map(i64::from)), world_stud_cell(west(), hit.hi.map(i64::from)));
    assert!(
        matches!(sent[..], [IntentMsg::Intent(SentIntent { target: Target::Stud(c), intent: Intent::BreakStud { lo: l, hi: h, survival: true, .. }, .. })]
            if c == stud_chunk_of_cell(lo) && (l, h) == (lo, hi)),
        "one survival break, addressed to the east's brick: {sent:?}"
    );
    for _ in 0..3 {
        tick(&mut east_region);
        tick(&mut west_region);
        deliver(&mut east_region, &mut west_region, west());
        deliver(&mut west_region, &mut east_region, east());
    }
    assert_eq!(east_region.data().stud_material_at(east_cell), None, "the owner broke the brick");
    assert_eq!(west_region.data().stud_material_seen(p.map(i64::from)), None, "and the hole came back through the halo");
    let dirt = ItemId::block(block_props().id_named("dirt").expect("real block props"));
    let piled = |r: &Region| r.data().item_piles().values().flat_map(|p| &p.stacks).filter(|s| s.item == dirt).map(|s| s.count).sum::<u32>();
    assert_eq!(piled(&west_region), 0, "a drop lies in the OWNER's piles, never on the sender's side");

    // --- Place stone into the hole: taken from the hotbar when sent, placed by the owner.
    let stone = block_props().id_named("stone").unwrap();
    west_region.with_data(|d| d.clients.get_mut(&CLIENT).unwrap().inventory.slots[0] = Some(ItemStack { item: ItemId::block(stone), count: 2 }));
    let hit = west_region.data().eye_stud_hit(CLIENT).expect("the hole's floor or wall");
    let (chunk, brick) = brick_placement(&hit.ray, MAT_STONE).expect("a brick against the hit face");
    assert!(chunk[0] >= REGION_CHUNKS as i64, "the chunk it goes into is the east's too: {chunk:?}");
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::MouseButton { button: MouseButton::Right, pressed: true }));
    tick(&mut west_region);
    tick(&mut east_region);
    apply(&mut west_region, GameEventKind::PlayerInput(CLIENT, InputEvent::MouseButton { button: MouseButton::Right, pressed: false }));
    assert_eq!(west_region.data().clients[&CLIENT].inventory.slots[0].map(|s| s.count), Some(1), "one stone taken");
    let (_, intents) = deliver(&mut west_region, &mut east_region, east());
    assert!(
        matches!(intents[..], [IntentMsg::Intent(SentIntent { intent: Intent::PlaceBrick { brick: b, .. }, .. })] if b == brick),
        "{intents:?}"
    );
    for _ in 0..3 {
        tick(&mut east_region);
        tick(&mut west_region);
        deliver(&mut east_region, &mut west_region, west());
        deliver(&mut west_region, &mut east_region, east());
    }
    let east_chunk = StudChunkCoords { x: (chunk[0] - REGION_CHUNKS as i64) as usize, y: chunk[1] as i32, z: chunk[2] as usize };
    assert_eq!(east_region.data().stud_chunk_at(east_chunk).and_then(|c| c.brick_at(brick.min)), Some(&brick), "the owner placed it");
    assert_eq!(west_region.data().clients[&CLIENT].inventory.slots[0].map(|s| s.count), Some(1), "granted: nothing refunded");
    assert_halo_agrees(&west_region, west(), &east_region, east());
}

// --- Cliffs (the P2 open item, closed 2026-11-11) -----------------------------------

/// A plateau two stud layers over [`TOP`].
const HIGH: i32 = TOP + 2 * STUD_CHUNK_PLATES as i32;

fn seed_of(top: impl Fn(usize, usize) -> i32) -> StudRegionSeed {
    let ground = (0..REGION_CHUNKS)
        .flat_map(|x| (0..REGION_CHUNKS).map(move |z| (x, z)))
        .map(|(x, z)| (ColumnCoords::new(x, z), StudColumnGround::flat(top(x, z), SOIL, MAT_STONE, -2000)))
        .collect();
    let mut seed = StudRegionSeed { ground, features: Vec::new(), ..Default::default() };
    // Hand-made ground: the aprons a generator samples are the neighbours' edges.
    link_stud_aprons(&mut seed.ground);
    seed
}

/// `seed` with the apron of its edge columns on `side` set to a neighbour region whose
/// ground stands at `top` there.
fn facing(mut seed: StudRegionSeed, side: StudSide, top: i32) -> StudRegionSeed {
    let last = REGION_CHUNKS - 1;
    for (c, g) in seed.ground.iter_mut() {
        let on_edge = match side {
            StudSide::West => c.x == 0,
            StudSide::East => c.x == last,
            StudSide::North => c.z == 0,
            StudSide::South => c.z == last,
        };
        if on_edge {
            g.set_apron_side(side, &[top; STUD_CHUNK_STUDS]);
        }
    }
    seed
}

/// Every cell of the face `x` (region studs) of plates `plates` is a real brick.
fn assert_brick_face(d: &Rollback, x: i32, plates: std::ops::Range<i32>) {
    for z in (0..N * CW).step_by(5) {
        for plate in plates.clone() {
            let c = at(x / CW, plate.div_euclid(STUD_CHUNK_PLATES as i32), z / CW);
            let local = [(x % CW) as u8, plate.rem_euclid(STUD_CHUNK_PLATES as i32) as u8, (z % CW) as u8];
            let brick = d.stud_chunk_at(c).and_then(|ch| ch.brick_at(local)).unwrap_or_else(|| panic!("no brick at {x},{plate},{z}"));
            assert!(matches!(brick.shape, StudShape::Part { .. }), "the cliff face at {x},{plate},{z} is {brick:?}");
        }
    }
}

fn region_with(rc: RegionCoords, seed: StudRegionSeed) -> Region {
    let mut region = Region::new(Rollback::new(None), Some(sink()), rc, None);
    region.seed_stud_world(seed);
    region
}

/// A cliff between two chunk columns INSIDE a region is closed at genesis: the high
/// column's ground has bricks and colliders all the way down the face the low column
/// opens onto (implied ground has neither: the face was a hole a body walked into),
/// and nothing more — columns that do not stand over a neighbour stay implied.
#[test]
fn a_cliff_inside_a_region_is_closed_at_genesis() {
    // Chunk column x = 1 is a plateau; the rest of the region is the plain.
    let r = region_with(west(), seed_of(|x, _| if x == 1 { HIGH } else { TOP }));
    let d = r.data();
    for z in 0..N {
        for y in 0..=2 {
            let chunk = d.stud_chunk_at(at(1, y, z)).unwrap_or_else(|| panic!("the plateau's layer {y} at z {z} has bricks"));
            assert_eq!(Some(chunk), d.stud_ground_recipes()[&ColumnCoords::new(1, z as usize)].pristine_chunk(y).as_ref());
            let key = d.stud_key_at(at(1, y, z)).unwrap();
            let body = d.ecs.rigidbody.try_get(key).and_then(|h| d.physics.bodies.get(h)).expect("its body");
            assert!(!body.colliders().is_empty(), "…and a collider");
        }
        // The wall, seen from the plain: solid cells at every plate from the plain's top to the plateau's.
        for plate in [TOP, TOP + 40, HIGH - 1] {
            assert!(d.stud_material_at([CW, plate, z * CW + 3]).is_some(), "the face at plate {plate}");
        }
        assert!(d.stud_chunk_at(at(1, -1, z)).is_none(), "nothing under the plain's level");
    }
    // Both faces of the plateau are real bricks from the plain's top up, and what is
    // behind them is still filler.
    assert_brick_face(d, CW, TOP..HIGH);
    assert_brick_face(d, 2 * CW - 1, TOP..HIGH);
    let core = d.stud_chunk_at(at(1, 1, 0)).unwrap().brick_at([16, 40, 16]).unwrap();
    assert!(matches!(core.shape, StudShape::Filler { .. }));
    // A dig beside the cliff leaves its bricks as they are.
    let wall: Vec<StudBrick> = d.stud_chunk_at(at(1, 0, 0)).unwrap().bricks().filter(|b| matches!(b.shape, StudShape::Part { .. })).copied().collect();
    let mut dug = region_with(west(), seed_of(|x, _| if x == 1 { HIGH } else { TOP }));
    dug.with_data(|r| assert!(!r.carve_stud_safe([CW - 2, TOP - 6, 8], [CW, TOP, 10]).is_empty()));
    let after = dug.data().stud_chunk_at(at(1, 0, 0)).unwrap();
    for b in &wall {
        assert_eq!(after.brick_at(b.min), Some(b), "a cliff brick moved");
    }
    // The cost of a sheer two-layer cliff on both faces of a chunk column, per layer.
    let g = &d.stud_ground_recipes()[&ColumnCoords::new(1, 0)];
    let closed = g.clone().with_apron(vec![STUD_APRON_CLOSED; STUD_APRON_LEN].into()).unwrap();
    for y in 0..=2 {
        let count = |c: &StudChunk| (c.len(), c.bricks().filter(|b| matches!(b.shape, StudShape::Filler { .. })).count());
        let t = std::time::Instant::now();
        let open = g.pristine_chunk(y).unwrap();
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        println!("plateau chunk layer {y}: (bricks, fillers) closed {:?} -> cliff {:?}, {ms:.2} ms", count(&closed.pristine_chunk(y).unwrap()), count(&open));
        assert!(open.occupancy() == closed.pristine_chunk(y).unwrap().occupancy(), "the cliff's bricks move no cell");
    }
    // Cliff chunks are pristine: nothing of them is stored (derived on load), and
    // they travel in a join snapshot as their recipe's diff — nothing.
    let (saved, _) = d.portable_snapshot(&SaveTerms::default());
    assert!(saved.stud_chunks.is_empty(), "no stud row for a cliff: {:?}", saved.stud_chunks.keys().collect::<Vec<_>>());
    let (restored, _) = restore_portable(west(), saved.into());
    assert_eq!(restored.data().stud_index.len(), d.stud_index.len());
    for (c, _) in d.stud_index.iter() {
        assert!(restored.data().stud_chunk_at(*c) == d.stud_chunk_at(*c), "restored {c:?}");
    }
    let wire = encode_packet(&ServerPacket::Region(west(), d.clone())).unwrap();
    let flat_wire = encode_packet(&ServerPacket::Region(west(), region(west()).data().clone())).unwrap();
    let ServerPacket::Region(_, back) = decode_packet(&wire).unwrap() else { panic!("a region packet") };
    assert_eq!(state_hash(&back), state_hash(d));
    println!("cliff region join snapshot: {} bytes (flat region: {})", wire.len(), flat_wire.len());
    assert!(wire.len() < flat_wire.len() * 3, "cliff chunks ride as pristine: {} vs {}", wire.len(), flat_wire.len());
    // The natural face is targeted as its brick.
    let hit = d.raycast_studs_seen(
        game::rapier3d::math::Vector::new((CW - 4) as f32 * STUD_UNITS as f32, plate_units(TOP + 20), 20.5 * STUD_UNITS as f32),
        game::rapier3d::math::Vector::new(1.0, 0.0, 0.0),
        100.0,
    )
    .expect("the ray meets the cliff");
    let brick = d.stud_chunk_seen(hit.chunk).and_then(|c| c.brick_at(hit.cell)).copied().unwrap();
    assert!(matches!(brick.shape, StudShape::Part { .. }), "the cliff is targeted as {brick:?}");
    let size = brick.size();
    assert_eq!(mining::stud_target_box(&brick, hit.cell), (brick.min, [0, 1, 2].map(|i| brick.min[i] + size[i])));
    // The plain's columns hold their one surface layer.
    assert_eq!(d.stud_index.len(), REGION_CHUNKS * (REGION_CHUNKS - 1) + REGION_CHUNKS * 3);
    assert_eq!(region(west()).data().stud_index.len(), REGION_CHUNKS * REGION_CHUNKS, "flat ground: surface layers only");
}

/// A cliff AT a seam is closed on both sides whatever the neighbour's seed comes
/// from: the high region's recipes know the low ground past the seam (their apron),
/// so it starts with its wall — real bricks, as one region lays them — with no
/// neighbour heard from; the low region's halo derives the wall's chunks from the
/// recipes alone, and a never-run high neighbour's seed gives it the same halo.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn a_cliff_at_a_seam_is_closed_on_both_sides() {
    let plateau = facing(seed_of(|_, _| HIGH), StudSide::East, TOP);
    let plain = || facing(flat_seed(), StudSide::West, HIGH);
    let mut high = region_with(west(), plateau.clone());
    let low_region = region_with(east(), plain());
    for z in 0..N {
        for y in 0..=2 {
            assert!(high.data().stud_chunk_at(at(N - 1, y, z)).is_some(), "own wall layer {y} at z {z}");
        }
        assert!(high.data().stud_chunk_at(at(N - 2, 0, z)).is_none(), "nothing behind the edge column");
    }
    assert_brick_face(high.data(), N * CW - 1, TOP..HIGH);
    // One region holding both grounds lays the same bricks in the wall: the plateau's
    // edge column next to a plain column.
    let mut one = vec![
        (ColumnCoords::new(0, 0), StudColumnGround::flat(HIGH, SOIL, MAT_STONE, -2000)),
        (ColumnCoords::new(1, 0), flat()),
    ];
    link_stud_aprons(&mut one);
    for y in 0..=2 {
        let mut reference = one[0].1.clone();
        // (its other sides as the edge column's: plateau all round)
        for side in [StudSide::West, StudSide::North, StudSide::South] {
            reference.set_apron_side(side, &[HIGH; STUD_CHUNK_STUDS]);
        }
        assert!(high.data().stud_chunk_at(at(N - 1, y, 1)) == reference.pristine_chunk(y).as_ref(), "layer {y}: one region's bricks");
    }
    // Hearing from the neighbour changes no ground: the wall was there already.
    let chunks = high.data().stud_index.len();
    let update = low_region.data().halo_seed_for(west()).expect("neighbours");
    assert!(update.studs.is_empty(), "the low region's edge is all derived");
    apply_checked(&mut high, GameEventKind::HaloUpdate(update), "the low neighbour's seed");
    assert_eq!(high.data().stud_index.len(), chunks);

    // The low side: its halo of the running high region, and of a never-run one.
    let (mut a, mut b) = (region_with(east(), plain()), region_with(east(), plain()));
    seed(&high, &mut a, east());
    let (xs, zs) = game::halo::facing_columns(west(), east());
    let window = StudRegionSeed {
        ground: plateau.ground.iter().filter(|(c, _)| xs.contains(&c.x) && zs.contains(&c.z)).cloned().collect(),
        features: Vec::new(),
        ..Default::default()
    };
    let snap = crate::common::stud_terrain_snapshot(StudEdge::from_seed(&window));
    apply_checked(&mut b, GameEventKind::HaloUpdate(game::halo::halo_seed_from_snapshot(west(), east(), &snap).unwrap()), "a never-run seed");
    for r in [&a, &b] {
        for z in 0..N {
            for y in 0..=2 {
                assert!(r.data().stud_halo_chunk_at([-1, y, z]).is_some(), "the wall in the halo: layer {y} at z {z}");
            }
        }
    }
    assert!(a.data().stud_halo() == b.data().stud_halo(), "the same halo from the running region and from its seed");
    assert_eq!(hash(&a), hash(&b));
    assert_halo_agrees(&a, east(), &high, west());
}
