//! `entities` keyed per region (migration `0009_entities_per_region`): rows saved under
//! the old key (one per entity, world-wide) load unchanged after the migration, and an
//! entity twice in one part of a cut is still refused before anything is written (the
//! database no longer refuses an entity in two regions).

use game::{CutId, EntityBundle, EntityId, PortableRegion, RegionCoords, StoredRegion, WorldCut};
use world_store::{
    Dials, FORMAT_ENTITY_BUNDLE, FORMAT_REGION_GROUND, FORMAT_REGION_META, FormatKind, OpenMode, StoreError, StoreHandle,
    encode, newest_migration,
};

use crate::codec::{bundle, meta, region_ground};
use crate::handle::{each_backend, fence, open};
use crate::stud_refusal::{blob, migrate_to, old_store};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

/// The last schema with one `entities` row per entity world-wide (`0008_cut_decisions`).
const WORLD_KEYED_SCHEMA: i64 = 8;

fn entity(serial: u64) -> EntityBundle {
    let mut b = bundle();
    b.id = EntityId { birth_region: RegionCoords::new(3, -2), serial };
    b
}

/// A region's row as this build writes it. A store from before `0011_voxel_terrain_returns`
/// that holds a region is refused at open (`stud_refusal`), so the region rows of
/// [`world_keyed_entities_migrate_unchanged`] go in once the store is migrated; `0009`
/// touches only `entities`.
fn region_row(url: &world_store::StoreUrl, rc: RegionCoords) -> String {
    let b = |bytes: Vec<u8>| blob(url, &bytes);
    format!(
        "INSERT INTO regions (rx, rz, saved_tick, terrain_version, meta, ground) VALUES ({}, {}, 700, 2, {}, {})",
        rc.x,
        rc.z,
        b(encode(FORMAT_REGION_META, &meta())),
        b(encode(FORMAT_REGION_GROUND, &region_ground()))
    )
}

/// A region's entities' rows as a schema-0008 build wrote them.
fn old_rows(url: &world_store::StoreUrl, rc: RegionCoords, entities: &[EntityBundle]) -> Vec<String> {
    let b = |bytes: Vec<u8>| blob(url, &bytes);
    let mut rows = vec![];
    for e in entities {
        rows.push(format!(
            "INSERT INTO entities (birth_rx, birth_rz, serial, rx, rz, saved_tick, kind, bundle) VALUES ({}, {}, {}, {}, {}, 700, 1, {})",
            e.id.birth_region.x,
            e.id.birth_region.z,
            e.id.serial as i64,
            rc.x,
            rc.z,
            b(encode(FORMAT_ENTITY_BUNDLE, e))
        ));
    }
    rows
}

fn stored(entities: Vec<EntityBundle>) -> StoredRegion {
    StoredRegion { meta: meta(), chunks: vec![], ground: region_ground(), entities }
}

/// A store with entity rows under the old key is migrated by its first writer (a
/// read-only handle refuses it, asking for a writer) and loads every entity unchanged;
/// after that an entity moving between two regions of a cut is saved in its new one.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn world_keyed_entities_migrate_unchanged() {
    each_backend("entity_key", true, |url| {
        let (in_a, in_b) = (vec![entity(5), entity(7)], vec![entity(9)]);
        let mut rows = old_rows(url, A, &in_a);
        rows.extend(old_rows(url, B, &in_b));
        old_store(url, WORLD_KEYED_SCHEMA, "1", &rows);

        let err = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).err().unwrap();
        assert_eq!(err, StoreError::OtherFormat { found: WORLD_KEYED_SCHEMA as u32, kind: FormatKind::Schema });

        // The first writer migrates; then the regions' rows (see `region_row`).
        open(url).shutdown();
        migrate_to(url, newest_migration(), &[region_row(url, A), region_row(url, B)]);
        let h = open(url);
        let f = fence(&h);
        assert_eq!(h.load_region(A, &f).unwrap(), Some(stored(in_a.clone())), "region A's entities unchanged");
        assert_eq!(h.load_region(B, &f).unwrap(), Some(stored(in_b.clone())), "region B's entities unchanged");
        // Entity 7 moves from A to B in one cut: deleted from A's rows, inserted into B's.
        let region = |entities: Vec<EntityBundle>| PortableRegion { meta: meta(), entities, ..Default::default() };
        let cut = WorldCut {
            id: CutId(1),
            host: 0,
            tick: 800,
            regions: vec![(A, region(vec![entity(5)])), (B, region(vec![entity(7), entity(9)]))],
            in_flight: vec![],
            players: vec![],
            history: None,
            adopts: vec![],
        };
        h.commit_cut(cut, f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_region(A, &f).unwrap().unwrap().entities, vec![entity(5)]);
        assert_eq!(h.load_region(B, &f).unwrap().unwrap().entities, vec![entity(7), entity(9)]);
        h.shutdown();
        // Now at this build's schema: a read-only handle opens it.
        StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap().shutdown();
    });
}

/// An entity twice in one region of a part, or in two of its regions, is refused as a
/// conservation bug (the per-region key would take the second), and nothing is written.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn entity_twice_in_one_part_is_refused() {
    each_backend("entity_twice", false, |url| {
        let h = open(url);
        let f = fence(&h);
        let region = |entities: Vec<EntityBundle>| PortableRegion {
            meta: meta(),
            ground: Some(region_ground()),
            entities,
            ..Default::default()
        };
        let twice = [
            vec![(A, region(vec![entity(5), entity(5)]))],
            vec![(A, region(vec![entity(5)])), (B, region(vec![entity(5)]))],
        ];
        for (n, regions) in twice.into_iter().enumerate() {
            let cut = WorldCut {
                id: CutId(n as u64 + 1),
                host: 0,
                tick: 100,
                regions,
                in_flight: vec![],
                players: vec![],
                history: None,
                adopts: vec![],
            };
            let err = h.commit_cut(cut, f.clone()).recv().unwrap().unwrap_err();
            assert!(
                matches!(&err, StoreError::Corrupt(m) if m.contains("two regions") && m.contains("conservation bug")),
                "case {n}: {err}"
            );
            assert!(h.stored_regions().unwrap().is_empty(), "case {n}: nothing written");
        }
        h.shutdown();
    });
}

/// Fix round 1, path (a): an entity stored in region A by a committed cut, then saved in
/// region B by a later cut that leaves A out (A stopped without answering, or was given up
/// at exit) — A's stale row would keep a second copy. The old world-wide key refused it;
/// the stale-row guard refuses it now, nothing written. A healthy crossing (A and B both
/// in the cut) is saved.
#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn entity_saved_while_a_region_outside_the_cut_still_holds_it_is_refused() {
    each_backend("entity_stale", false, |url| {
        let h = open(url);
        let f = fence(&h);
        let region = |entities: Vec<EntityBundle>| PortableRegion {
            meta: meta(),
            ground: Some(region_ground()),
            entities,
            ..Default::default()
        };
        let cut = |id: u64, regions: Vec<(RegionCoords, PortableRegion)>| WorldCut {
            id: CutId(id),
            host: 0,
            tick: 100 * id,
            regions,
            in_flight: vec![],
            players: vec![],
            history: None,
            adopts: vec![],
        };
        h.commit_cut(cut(1, vec![(A, region(vec![entity(5)])), (B, region(vec![]))]), f.clone()).recv().unwrap().unwrap();
        let err = h.commit_cut(cut(2, vec![(B, region(vec![entity(5)]))]), f.clone()).recv().unwrap().unwrap_err();
        assert!(
            matches!(&err, StoreError::Corrupt(m) if m.contains("still stored in RegionCoords { x: 0, z: 0 }")
                && m.contains("conservation bug")),
            "{err}"
        );
        assert_eq!(h.load_region(A, &f).unwrap().unwrap().entities, vec![entity(5)], "unchanged");
        assert!(h.load_region(B, &f).unwrap().unwrap().entities.is_empty(), "unchanged");
        assert_eq!(h.world_info().unwrap().world_tick, 100, "the cut was not committed");
        // The healthy crossing: A rewritten without it in the same cut.
        h.commit_cut(cut(3, vec![(A, region(vec![])), (B, region(vec![entity(5)]))]), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_region(B, &f).unwrap().unwrap().entities, vec![entity(5)]);
        // And a cut leaving A out that does not write the entity again is fine.
        h.commit_cut(cut(4, vec![(B, region(vec![entity(5), entity(7)]))]), f.clone()).recv().unwrap().unwrap();
        h.shutdown();
    });
}
