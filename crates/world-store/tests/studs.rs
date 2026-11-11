//! Stud terrain rows (migration 0004; the only terrain rows since 0005): `stud_chunks`, `regions.stud_ground` and the
//! world's terrain kind. Written only in a fenced cut; a row that does not read back
//! is an error, never an empty region.

use std::collections::BTreeMap;

use game::{
    CutId, PortableRegion, RegionCoords, StoredRegion, StudBrick, StudChunk, StudChunkCoords,
    StudChunkRecord, WorldCut,
};
use world_store::{Dials, OpenMode, StoreError, StoreHandle, TerrainKind};

use crate::codec::{meta, stud_chunk, stud_ground};
use crate::handle::{TEST_SUPER_REGIONS, each_backend, fence, open, raw_exec};

const A: RegionCoords = RegionCoords { x: 0, z: 0 };

fn at(x: usize, y: i32, z: usize) -> StudChunkCoords {
    StudChunkCoords { x, y, z }
}

fn other_chunk() -> StudChunkRecord {
    StudChunkRecord::Edited(StudChunk::new([StudBrick::filler([1, 2, 3], [2, 3, 2], game::MAT_DIRT)]).unwrap())
}

/// A stud region's save: `first` carries the grounds (the voxel one empty).
fn region(tick: u64, first: bool, rows: &[(StudChunkCoords, Option<StudChunkRecord>)]) -> PortableRegion {
    let mut m = meta();
    m.tick = tick;
    PortableRegion {
        meta: m,
        stud_ground: first.then(stud_ground),
        stud_chunks: rows.iter().cloned().collect(),
        ..Default::default()
    }
}

fn cut(id: u64, region: PortableRegion) -> WorldCut {
    WorldCut { id: CutId(id), host: 0, tick: 100 * id, regions: vec![(A, region)], in_flight: vec![], players: vec![], history: None }
}

fn first_region() -> PortableRegion {
    // An edited chunk over the surface layer, one in the air (negative and positive
    // layers), and a pristine marker under column (1, 0).
    region(
        100,
        true,
        &[
            (at(0, 0, 0), Some(stud_chunk())),
            (at(0, 7, 3), Some(other_chunk())),
            (at(1, -3, 0), Some(StudChunkRecord::Pristine)),
        ],
    )
}

fn stud_rows(s: &StoredRegion) -> BTreeMap<StudChunkCoords, StudChunkRecord> {
    s.stud_chunks.iter().cloned().collect()
}

#[test]
fn stud_rows_round_trip() {
    each_backend("studs_roundtrip", true, |url| {
        let h = open(url);
        let f = fence(&h);
        let first = first_region();
        let done = h.commit_cut(cut(1, first.clone()), f.clone()).recv().unwrap().unwrap();
        assert_eq!(done.versions, vec![(A, 1)], "stud rows are terrain: the version moves");
        let got = h.load_region(A, &f).unwrap().expect("the region is stored");
        assert_eq!(got, StoredRegion::from(first.clone()));
        assert_eq!(got.stud_ground, stud_ground());
        assert_eq!(got.stud_chunks.len(), 3);

        // A later save: one row deleted, one replaced, one added; the recipes stay.
        let second = region(200, false, &[(at(0, 0, 0), None), (at(0, 7, 3), Some(stud_chunk())), (at(3, -1, 2), Some(other_chunk()))]);
        let done = h.commit_cut(cut(2, second), f.clone()).recv().unwrap().unwrap();
        assert_eq!(done.versions, vec![(A, 2)]);
        // A save with no stud rows leaves them and the version alone.
        let done = h.commit_cut(cut(3, region(300, false, &[])), f.clone()).recv().unwrap().unwrap();
        assert_eq!(done.versions, vec![(A, 2)]);
        h.shutdown();

        // Durable: the same after a reopen.
        let h = open(url);
        let f = fence(&h);
        let got = h.load_region(A, &f).unwrap().unwrap();
        assert_eq!(got.meta.tick, 300);
        assert_eq!(got.stud_ground, stud_ground(), "the recipes are written once and kept");
        assert_eq!(
            stud_rows(&got),
            BTreeMap::from([
                (at(0, 7, 3), stud_chunk()),
                (at(1, -3, 0), StudChunkRecord::Pristine),
                (at(3, -1, 2), other_chunk()),
            ])
        );
        // Deleting a row that is not there is not an error.
        h.commit_cut(cut(4, region(400, false, &[(at(2, 2, 2), None)])), f.clone()).recv().unwrap().unwrap();
        assert_eq!(h.load_region(A, &f).unwrap().unwrap().stud_chunks.len(), 3);
        h.shutdown();
    });
}

/// `load_terrain` (what a neighbour's halo seed reads) carries the stored region's stud
/// edge: its edge columns' recipes and stud rows, a `Pristine` marker derived from its
/// recipe (`game::stud_halo`, stud world plan P4).
#[test]
fn stored_terrain_carries_the_stud_edge() {
    each_backend("studs_terrain", true, |url| {
        let h = open(url);
        let f = fence(&h);
        h.commit_cut(cut(1, first_region()), f.clone()).recv().unwrap().unwrap();
        let stored = h.load_region(A, &f).unwrap().unwrap();
        let want = game::StudEdge::from_stored(
            &stored.stud_ground,
            stored.stud_chunks.iter().cloned().map(|(c, record)| match record {
                StudChunkRecord::Pristine => (c, None),
                StudChunkRecord::Edited(chunk) => (c, Some(chunk)),
            }),
        );
        let snap = h.load_terrain(A).unwrap().expect("the region is stored");
        assert_eq!(snap.stud, want);
        assert!(!snap.stud.ground.is_empty(), "the fixture's recipes lie in edge columns");
        assert!(snap.stud.chunks.iter().any(|(c, _)| *c == at(0, 0, 0)), "the edited edge chunk");
        h.shutdown();
    });
}

#[test]
fn stale_owner_writes_no_stud_rows() {
    each_backend("studs_fence", false, |url| {
        let h = open(url);
        let stale = h.claim_super_regions(0, TEST_SUPER_REGIONS.to_vec()).unwrap();
        // Nothing stored yet: a refused first save leaves no region and no rows.
        let owner = h.claim_super_regions(1, TEST_SUPER_REGIONS.to_vec()).unwrap();
        let err = h.commit_cut(cut(1, first_region()), stale.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        assert_eq!(h.load_region(A, &owner).unwrap(), None);

        // The owner saves; the stale host's edit, delete and new row are all refused.
        h.commit_cut(cut(2, first_region()), owner.clone()).recv().unwrap().unwrap();
        let edit = region(300, false, &[(at(0, 0, 0), None), (at(0, 7, 3), Some(stud_chunk())), (at(2, 2, 2), Some(other_chunk()))]);
        let err = h.commit_cut(cut(3, edit), stale.clone()).recv().unwrap().unwrap_err();
        assert!(matches!(err, StoreError::Fenced { sr: Some(_) }), "{err}");
        assert_eq!(h.load_region(A, &owner).unwrap().unwrap(), StoredRegion::from(first_region()), "nothing was written");
        // And it cannot read them either.
        assert!(matches!(h.load_region(A, &stale), Err(StoreError::Fenced { .. })));
        h.shutdown();
    });
}

/// Each damaged stud row or recipe makes the region's load an error.
#[test]
fn damaged_stud_rows_are_errors() {
    let (garbage_pg, garbage_sqlite) = ("'\\x01ff00ff'::bytea", "x'01ff00ff'");
    let damages: [(&str, &dyn Fn(&str) -> String); 5] = [
        ("a chunk that does not decode", &|g| format!("UPDATE stud_chunks SET data = {g} WHERE cx = 0 AND cy = 0")),
        ("recipes that do not decode", &|g| format!("UPDATE regions SET stud_ground = {g}")),
        ("a pristine marker without recipes", &|_| "UPDATE regions SET stud_ground = NULL".to_string()),
        ("a layer outside the world", &|_| "UPDATE stud_chunks SET cy = 1000000 WHERE cy = 7".to_string()),
        ("a chunk outside the region", &|_| "UPDATE stud_chunks SET cx = 100000 WHERE cy = 7".to_string()),
    ];
    for (what, sql) in damages {
        each_backend("studs_damaged", true, |url| {
            let h = open(url);
            let f = fence(&h);
            h.commit_cut(cut(1, first_region()), f.clone()).recv().unwrap().unwrap();
            assert!(h.load_region(A, &f).unwrap().is_some());
            h.shutdown();
            let garbage = if matches!(url, world_store::StoreUrl::Postgres(_)) { garbage_pg } else { garbage_sqlite };
            raw_exec(url, &sql(garbage));
            let h = open(url);
            let f = fence(&h);
            let err = h.load_region(A, &f).expect_err(what);
            assert!(matches!(err, StoreError::Corrupt(_)), "{what}: {err}");
            h.shutdown();
        });
    }
}

#[test]
fn terrain_kind_is_bound_once() {
    each_backend("studs_kind", true, |url| {
        let h = open(url);
        assert_eq!(h.terrain_kind().unwrap(), None, "a new world is tied to no terrain yet");
        h.bind_terrain(TerrainKind::Studs).unwrap();
        h.bind_terrain(TerrainKind::Studs).unwrap();
        assert_eq!(
            h.bind_terrain(TerrainKind::Voxels).unwrap_err(),
            StoreError::OtherTerrain { stored: TerrainKind::Studs, wanted: TerrainKind::Voxels }
        );
        assert_eq!(h.terrain_kind().unwrap(), Some(TerrainKind::Studs));
        h.shutdown();

        // Kept across a reopen; a read-only handle reads it and binds nothing.
        let h = open(url);
        assert!(matches!(h.bind_terrain(TerrainKind::Voxels), Err(StoreError::OtherTerrain { .. })));
        h.shutdown();
        let ro = StoreHandle::open(url, Dials::BUILT, OpenMode::ReadOnly).unwrap();
        assert_eq!(ro.terrain_kind().unwrap(), Some(TerrainKind::Studs));
        assert_eq!(ro.bind_terrain(TerrainKind::Studs).unwrap_err(), StoreError::ReadOnly);
        ro.shutdown();

        // A value that is no terrain kind is an error, not a default.
        raw_exec(url, "UPDATE world SET terrain = 9");
        let h = open(url);
        assert!(matches!(h.terrain_kind(), Err(StoreError::Corrupt(_))));
        assert!(matches!(h.bind_terrain(TerrainKind::Voxels), Err(StoreError::Corrupt(_))));
        h.shutdown();
    });
}
