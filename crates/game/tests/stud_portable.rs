//! Stud terrain in the portable format (`portable.rs`, stud world plan P5): only what
//! the ground recipe cannot give back is stored, and a restore brings back the same
//! materialised chunks, bricks, colliders and implied ground.
use std::collections::BTreeMap;

use game::portable::{PortableRegion, SaveTerms};
use game::stud_catalog::STUD_PARTS;
use game::*;
use state_hash::StateHash;

const SOIL: StudProfile = StudProfile { cover: MAT_GRASS, cover_plates: 1, soil: MAT_DIRT, soil_plates: 5 };

fn at(x: usize, y: i32, z: usize) -> StudChunkCoords {
    StudChunkCoords { x, y, z }
}

fn brick(min: [u8; 3]) -> StudBrick {
    let part = STUD_PARTS.iter().position(|p| p.number == "3001").unwrap() as u16;
    StudBrick::part_at(min, part, 0, 4)
}

/// Three chunk columns of flat ground at plate 30 (layer 0): two side by side in x, one
/// behind in z.
fn fresh() -> Region {
    let mut region = Region::new(Rollback::new(None), None, RegionCoords::new(0, 0), None);
    let flat = || StudColumnGround::flat(30, SOIL, MAT_STONE, -2000);
    region.seed_stud_world(StudRegionSeed {
        ground: [(0, 0), (1, 0), (0, 1)].into_iter().map(|(x, z)| (ColumnCoords::new(x, z), flat())).collect(),
        features: Vec::new(),
    });
    region
}

/// The fixture: a shaft through the layer boundary at plate 0, a trench through the
/// chunk face between columns 0 and 1, a pocket deep against that face (it opens the
/// pristine neighbour) and a brick in the air.
fn edited() -> Region {
    let mut region = fresh();
    region.with_data(|r| {
        assert!(!r.carve_stud_safe([4, -6, 4], [6, 30, 6]).is_empty(), "the shaft");
        assert!(!r.carve_stud_safe([30, 24, 10], [34, 30, 12]).is_empty(), "the trench");
        assert!(!r.carve_stud_safe([30, -40, 4], [32, -34, 6]).is_empty(), "the pocket");
        assert!(r.place_stud_safe(at(0, 4, 0), brick([3, 0, 3])), "the brick in the air");
    });
    region
}

fn snapshot(region: &Region, saved: &SaveTerms) -> (PortableRegion, SaveTerms) {
    region.data().portable_snapshot(saved)
}

fn materialised(r: &Rollback) -> BTreeMap<StudChunkCoords, StudChunk> {
    r.stud_index.iter().map(|(c, _)| (*c, r.stud_chunk_at(*c).expect("an indexed chunk").clone())).collect()
}

fn shape_hash<T: StateHash + ?Sized>(v: &T) -> u32 {
    let mut h = crc32fast::Hasher::new();
    v.state_hash(&mut h);
    h.finalize()
}

/// Every materialised chunk's enabled colliders are the ones a fresh build of its bricks gives.
fn assert_colliders(r: &Rollback) {
    for (c, key) in r.stud_index.iter() {
        let chunk = r.stud_chunk_at(*c).unwrap();
        let body = r.ecs.rigidbody.try_get(*key).expect("a stud body");
        let have: Vec<u32> = r
            .physics
            .bodies
            .get(body)
            .unwrap()
            .colliders()
            .iter()
            .map(|&ch| r.physics.colliders.get(ch).unwrap())
            .filter(|col| col.is_enabled())
            .map(|col| shape_hash(col.shared_shape()))
            .collect();
        let want: Vec<u32> = stud_collider_shapes(chunk).iter().map(|s| shape_hash(s)).collect();
        assert!(!want.is_empty(), "{c:?} has bricks");
        assert_eq!(have, want, "colliders of {c:?}");
    }
}

/// Probe cells over all three columns and the air beside them, from far under the
/// materialised band to above the brick in the air.
fn probes() -> impl Iterator<Item = [i32; 3]> {
    let ys = (-130..=40).chain([-2001, -1999, -500, 360, 361, 362, 363, 364]);
    ys.flat_map(|y| (0..66).step_by(1).flat_map(move |x| [3, 4, 5, 10, 11, 12, 31, 32, 40].map(|z| [x, y, z])))
}

#[test]
fn stud_region_round_trips() {
    let region = edited();
    let (original, terms) = snapshot(&region, &SaveTerms::default());

    // What is stored: the edited chunks whole, the opened neighbour as a marker, and
    // nothing for the untouched surface chunk.
    let kinds: BTreeMap<StudChunkCoords, bool> = original
        .stud_chunks
        .iter()
        .map(|(c, r)| (*c, matches!(r, Some(StudChunkRecord::Edited(_)))))
        .collect();
    assert_eq!(
        kinds,
        BTreeMap::from([
            (at(0, -1, 0), true),
            (at(0, 0, 0), true),
            (at(0, 4, 0), true),
            (at(1, 0, 0), true),
            (at(1, -1, 0), false),
        ]),
        "true = bricks, false = a pristine marker"
    );
    assert_eq!(original.stud_chunks[&at(1, -1, 0)], Some(StudChunkRecord::Pristine));
    assert_eq!(original.stud_ground.as_ref().map(Vec::len), Some(3), "a first snapshot carries the recipes");
    assert_eq!(terms.studs.len(), 6, "every materialised chunk has a term");
    assert_eq!(terms.studs[&at(0, 0, 1)].kept, StudKept::Derived);

    let (restored_region, restored_terms) = restore_portable(RegionCoords::new(0, 0), StoredRegion::from(original.clone()));
    let (before, after) = (region.data(), restored_region.data());
    assert_eq!(restored_terms, terms, "the restored chunks have the saved terms");
    assert_eq!(after.body_scale(), movement::BodyScale::STUD);
    assert_eq!(materialised(after), materialised(before), "the same chunks are materialised, with the same bricks");
    assert_colliders(before);
    assert_colliders(after);
    for cell in probes() {
        assert_eq!(after.stud_material_at(cell), before.stud_material_at(cell), "cell {cell:?}");
    }
    // The probes saw what they are about.
    assert_eq!(after.stud_material_at([4, -3, 4]), None, "the shaft under the layer boundary");
    assert_eq!(after.stud_material_at([32, 27, 10]), None, "the trench past the chunk face");
    assert_eq!(after.stud_material_at([3, 360, 3]), Some(4), "the brick in the air");
    assert_eq!(after.stud_material_at([40, -500, 4]), Some(MAT_STONE), "implied ground");
    assert_eq!(after.stud_surface_y(20.0, 300.0), before.stud_surface_y(20.0, 300.0));

    // A whole snapshot of the restored region is the stored one.
    let (again, _) = snapshot(&restored_region, &SaveTerms::default());
    assert_eq!(again.stud_chunks, original.stud_chunks);
    assert_eq!(again.stud_ground, original.stud_ground);
    // And against its terms nothing is written.
    let (idle, idle_terms) = snapshot(&restored_region, &restored_terms);
    assert!(idle.stud_chunks.is_empty() && idle.stud_ground.is_none());
    assert_eq!(idle_terms, restored_terms);
}

#[test]
fn second_snapshot_writes_nothing_and_an_edit_writes_its_chunk() {
    let mut region = edited();
    let (_, terms) = snapshot(&region, &SaveTerms::default());
    let (idle, idle_terms) = snapshot(&region, &terms);
    assert!(idle.stud_chunks.is_empty() && idle.stud_ground.is_none());
    assert_eq!(idle_terms, terms);

    region.with_data(|r| assert!(r.place_stud_safe(at(0, 4, 0), brick([10, 0, 10]))));
    let (second, _) = snapshot(&region, &terms);
    assert_eq!(second.stud_chunks.keys().copied().collect::<Vec<_>>(), vec![at(0, 4, 0)]);
    assert!(second.stud_ground.is_none());
}

/// A chunk edited back to what its recipe gives leaves the stored set: its row is deleted
/// in a surface layer, and becomes a marker anywhere else.
#[test]
fn unedited_chunk_leaves_the_stored_set() {
    let mut region = fresh();
    let (first, terms) = snapshot(&region, &SaveTerms::default());
    assert!(first.stud_chunks.is_empty(), "pristine surface chunks are not stored");
    let surface = at(0, 0, 1);
    region.with_data(|r| assert!(r.place_stud_safe(surface, brick([3, 30, 3]))));
    let (placed, terms) = snapshot(&region, &terms);
    assert!(matches!(placed.stud_chunks.get(&surface), Some(Some(StudChunkRecord::Edited(_)))));
    region.with_data(|r| assert!(r.remove_stud_safe(surface, [3, 30, 3])));
    let (removed, terms) = snapshot(&region, &terms);
    assert_eq!(removed.stud_chunks, BTreeMap::from([(surface, None)]), "the row is deleted");
    assert_eq!(terms.studs[&surface].kept, StudKept::Derived);

    // Folded into one uncommitted save, the delete wins over the older write.
    let mut merged = placed.clone();
    merged.merge(removed);
    assert_eq!(merged.stud_chunks, BTreeMap::from([(surface, None)]));
    assert!(StoredRegion::from(merged).stud_chunks.is_empty());

    // Under the surface layers: materialised by a carve next door, then edited and restored.
    let deep = at(1, -1, 0);
    region.with_data(|r| assert!(!r.carve_stud_safe([30, -40, 4], [32, -34, 6]).is_empty()));
    let (opened, terms) = snapshot(&region, &terms);
    assert_eq!(opened.stud_chunks.get(&deep), Some(&Some(StudChunkRecord::Pristine)));
    let pristine = region.data().stud_chunk_at(deep).unwrap().clone();
    region.with_data(|r| {
        let key = r.stud_key_at(deep).unwrap();
        r.set_stud_chunk_safe(key, StudChunk::default());
    });
    let (emptied, terms) = snapshot(&region, &terms);
    assert_eq!(emptied.stud_chunks.get(&deep), Some(&Some(StudChunkRecord::Edited(StudChunk::default()))));
    region.with_data(|r| {
        let key = r.stud_key_at(deep).unwrap();
        r.set_stud_chunk_safe(key, pristine);
    });
    let (refilled, _) = snapshot(&region, &terms);
    assert_eq!(refilled.stud_chunks.get(&deep), Some(&Some(StudChunkRecord::Pristine)));
}

/// A chunk whose creation is rolled back leaves the stored set too.
#[test]
fn rolled_back_chunk_row_is_deleted() {
    // Updates are not read here.
    let (send, _recv) = crossbeam::channel::unbounded();
    let mut r = Rollback::new(Some(send));
    r.seed_stud_ground_genesis(vec![(ColumnCoords::new(0, 0), StudColumnGround::flat(30, SOIL, MAT_STONE, -2000))], Default::default());
    let (_, terms) = r.portable_snapshot(&SaveTerms::default());
    r.new_transaction();
    assert!(r.place_stud_safe(at(0, 4, 0), brick([3, 0, 3])));
    let (placed, terms) = r.portable_snapshot(&terms);
    assert!(matches!(placed.stud_chunks.get(&at(0, 4, 0)), Some(Some(_))));
    r.rollback();
    let (undone, _) = r.portable_snapshot(&terms);
    assert_eq!(undone.stud_chunks, BTreeMap::from([(at(0, 4, 0), None)]));
}
