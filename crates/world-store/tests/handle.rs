//! The store thread and `StoreHandle`: cut commits in one transaction, loads,
//! player ids. Every body runs on SQLite (in memory, or a temp file where it
//! reopens the store) and, with `MMO_TEST_DATABASE_URL` set, on a fresh
//! Postgres database.

use std::collections::BTreeMap;

use game::{
    Chunk, ChunkCoords, CutId, EntityBundle, EntityId, InFlightRow, IntentMsg, PlayerToken, PortableRegion,
    RegionCoords, StoredRegion, WorldCut,
};
use world_store::{Dials, Pool, StoreError, StoreHandle, StoreUrl, open_pool};

use crate::codec::{bundle, chunk, intent, meta, player, region_ground, reply};
use crate::common;

const A: RegionCoords = RegionCoords { x: 0, z: 0 };
const B: RegionCoords = RegionCoords { x: 1, z: -1 };

/// Runs `body` once per backend. `reopen`: SQLite on a temp file instead of in
/// memory (the body opens the store more than once).
fn each_backend(tag: &str, reopen: bool, body: impl Fn(&StoreUrl)) {
    if reopen {
        let dir = std::env::temp_dir().join(format!(
            "mmo_ws_handle_{tag}_{}_{}",
            std::process::id(),
            std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        body(&StoreUrl::Sqlite(dir.join("world.sqlite")));
        let _ = std::fs::remove_dir_all(dir);
    } else {
        body(&StoreUrl::Memory);
    }
    let Some(admin) = common::pg_admin_url() else { return common::skip_msg() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(common::PgTestDb::create(&admin));
    body(&StoreUrl::Postgres(db.url.clone()));
    rt.block_on(db.drop_db());
}

fn open(url: &StoreUrl) -> StoreHandle {
    StoreHandle::open(url, Dials::BUILT).unwrap()
}

/// Runs raw SQL against a closed store (the handle must have shut down).
fn raw_exec(url: &StoreUrl, sql: &str) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (pool, _guard) = open_pool(url, Dials::BUILT).await.unwrap();
        match &pool {
            Pool::Sqlite(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
            Pool::Pg(p) => {
                sqlx::query(sql).execute(p).await.unwrap();
                p.close().await;
            }
        }
    });
}

fn token(n: u8) -> PlayerToken {
    PlayerToken::generate([n; 16])
}

fn entity(birth: RegionCoords, serial: u64) -> EntityBundle {
    let mut b = bundle();
    b.id = EntityId { birth_region: birth, serial };
    b
}

fn region(tick: u64, chunks: &[(ChunkCoords, Chunk)], ground: bool, entities: Vec<EntityBundle>) -> PortableRegion {
    let mut m = meta();
    m.tick = tick;
    PortableRegion {
        meta: m,
        chunks: chunks.iter().cloned().collect(),
        ground: ground.then(region_ground),
        entities,
        players: Vec::new(),
    }
}

fn other_chunk() -> Chunk {
    let mut c = chunk();
    c.set_block(9, 9, 9, game::Block::new(game::BlockId(5)));
    c
}

fn first_cut(h: &StoreHandle) -> WorldCut {
    let (pid, rec) = h.resolve_player(&token(1)).unwrap();
    assert_eq!(rec, None);
    WorldCut {
        id: CutId(1),
        tick: 1000,
        regions: vec![
            (
                A,
                region(
                    1000,
                    &[(ChunkCoords::new(0, 0, 0), chunk()), (ChunkCoords::new(1, -2, 3), other_chunk())],
                    true,
                    vec![entity(A, 2), entity(B, 1)],
                ),
            ),
            (B, region(1000, &[(ChunkCoords::new(2, 5, 0), chunk())], true, vec![entity(A, 7)])),
        ],
        in_flight: vec![
            InFlightRow::Transfer { target: B, bundle: entity(A, 9), stamp: 998 },
            InFlightRow::Intent { target: A, msg: intent() },
            InFlightRow::Intent { target: B, msg: IntentMsg::Reply(reply()) },
        ],
        players: vec![(pid, player())],
        history: Some(vec![1, 2, 3, 4]),
    }
}

/// `p` as the store gives it back: entities sorted by id, players dropped.
fn stored(p: &PortableRegion) -> StoredRegion {
    let mut s = StoredRegion::from(p.clone());
    s.entities.sort_by_key(|b| b.id);
    s
}

#[test]
fn cut_roundtrip() {
    each_backend("roundtrip", false, |url| {
        let h = open(url);
        let cut = first_cut(&h);
        let rx = h.commit_cut(cut.clone());
        // A load while the commit runs neither deadlocks nor sees half a cut.
        let mid = h.load_region(A).unwrap();
        assert!(mid.is_none() || mid == Some(stored(&cut.regions[0].1)));
        let done = rx.recv().unwrap().unwrap();
        assert_eq!(done.id, CutId(1));
        assert_eq!(done.versions, vec![(A, 1), (B, 1)]);

        for (rc, p) in &cut.regions {
            assert_eq!(h.load_region(*rc).unwrap().as_ref(), Some(&stored(p)), "region {rc:?}");
            let terrain = h.load_terrain(*rc).unwrap().unwrap();
            assert_eq!(terrain.chunks, stored(p).chunks);
            assert_eq!(terrain.ground, region_ground());
        }
        assert_eq!(h.stored_regions().unwrap(), BTreeMap::from([(A, 1), (B, 1)]));
        assert_eq!(h.load_in_flight().unwrap(), cut.in_flight);
        assert_eq!(h.load_history().unwrap(), Some(vec![1, 2, 3, 4]));
        assert_eq!(h.world_info().unwrap().world_tick, 1000);
        let (pid, rec) = h.resolve_player(&token(1)).unwrap();
        assert_eq!(pid, cut.players[0].0);
        assert_eq!(rec, Some(player()));
        h.shutdown();
    });
}

#[test]
fn second_cut_writes_only_given_chunks() {
    each_backend("second", false, |url| {
        let h = open(url);
        let first = first_cut(&h);
        h.commit_cut(first.clone()).recv().unwrap().unwrap();

        let changed = ChunkCoords::new(1, -2, 3);
        let mut new_chunk = other_chunk();
        new_chunk.set_block(0, 1, 0, game::Block::new(game::BlockId(7)));
        let second = WorldCut {
            id: CutId(2),
            tick: 1100,
            regions: vec![
                (A, region(1100, &[(changed, new_chunk.clone())], false, vec![entity(A, 2)])),
                (B, region(1100, &[], false, vec![entity(A, 7), entity(B, 1)])),
            ],
            in_flight: vec![],
            players: vec![],
            history: None,
        };
        let done = h.commit_cut(second).recv().unwrap().unwrap();
        assert_eq!(done.versions, vec![(A, 2), (B, 1)]);
        assert_eq!(h.stored_regions().unwrap(), BTreeMap::from([(A, 2), (B, 1)]));

        let a = h.load_region(A).unwrap().unwrap();
        assert_eq!(a.meta.tick, 1100);
        assert_eq!(a.chunks, vec![(ChunkCoords::new(0, 0, 0), chunk()), (changed, new_chunk)]);
        assert_eq!(a.ground, region_ground(), "ground kept when the cut has none");
        assert_eq!(a.entities, vec![entity(A, 2)]);
        let b = h.load_region(B).unwrap().unwrap();
        assert_eq!(b.chunks, vec![(ChunkCoords::new(2, 5, 0), chunk())]);
        assert_eq!(b.entities, vec![entity(A, 7), entity(B, 1)], "an entity moved A -> B in one cut");
        assert_eq!(h.load_in_flight().unwrap(), vec![]);
        assert_eq!(h.load_history().unwrap(), Some(vec![1, 2, 3, 4]), "history kept when the cut has none");
        assert_eq!(h.resolve_player(&token(1)).unwrap().1, Some(player()), "player kept");
        assert_eq!(h.world_info().unwrap().world_tick, 1100);
        h.shutdown();
    });
}

#[test]
fn commit_cut_is_atomic() {
    each_backend("atomic", false, |url| {
        let h = open(url);
        let first = first_cut(&h);
        h.commit_cut(first.clone()).recv().unwrap().unwrap();

        let bad = WorldCut {
            id: CutId(2),
            tick: 1200,
            regions: vec![
                (A, region(1200, &[(ChunkCoords::new(0, 0, 0), other_chunk())], false, vec![entity(A, 2)])),
                (B, region(1200, &[], false, vec![entity(A, 2)])),
            ],
            in_flight: vec![],
            players: vec![(first.players[0].0, {
                let mut p = player();
                p.fortress = None;
                p
            })],
            history: Some(vec![9]),
        };
        let err = h.commit_cut(bad).recv().unwrap().unwrap_err();
        assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("two regions")), "{err}");

        for (rc, p) in &first.regions {
            assert_eq!(h.load_region(*rc).unwrap().as_ref(), Some(&stored(p)), "region {rc:?} unchanged");
        }
        assert_eq!(h.stored_regions().unwrap(), BTreeMap::from([(A, 1), (B, 1)]));
        assert_eq!(h.load_in_flight().unwrap(), first.in_flight);
        assert_eq!(h.load_history().unwrap(), Some(vec![1, 2, 3, 4]));
        assert_eq!(h.resolve_player(&token(1)).unwrap().1, Some(player()));
        assert_eq!(h.world_info().unwrap().world_tick, 1000);
        h.shutdown();
    });
}

#[test]
fn resolve_player_is_stable() {
    each_backend("players", true, |url| {
        let h = open(url);
        let (p1, r1) = h.resolve_player(&token(1)).unwrap();
        let (p2, _) = h.resolve_player(&token(2)).unwrap();
        assert_eq!(r1, None);
        assert_eq!(p2.0, p1.0 + 1);
        assert_eq!(h.resolve_player(&token(1)).unwrap(), (p1, None));
        h.shutdown();

        let h = open(url);
        assert_eq!(h.resolve_player(&token(1)).unwrap(), (p1, None), "same id across a reopen");
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let (p3, r3) = rt.block_on(h.resolve_player_async(&token(3))).unwrap();
        assert_eq!((p3.0, r3), (p2.0 + 1, None), "a new token gets the next id");
        assert_eq!(rt.block_on(h.resolve_player_async(&token(2))).unwrap(), (p2, None));
        h.shutdown();
    });
}

#[test]
fn u64_max_serial_roundtrips() {
    each_backend("u64max", false, |url| {
        let h = open(url);
        let (pid, _) = h.resolve_player(&token(1)).unwrap();
        let far = RegionCoords::new(i32::MIN, i32::MAX);
        let max = entity(far, u64::MAX);
        let mut p = region(u64::MAX, &[(ChunkCoords::new(0, i32::MIN, 0), chunk())], true, vec![max.clone()]);
        p.meta.next_entity_serial = u64::MAX;
        let cut = WorldCut {
            id: CutId(u64::MAX),
            tick: u64::MAX,
            regions: vec![(far, p.clone())],
            in_flight: vec![InFlightRow::Transfer { target: far, bundle: entity(A, u64::MAX - 1), stamp: u64::MAX }],
            players: vec![(pid, player())],
            history: None,
        };
        let done = h.commit_cut(cut.clone()).recv().unwrap().unwrap();
        assert_eq!(done.id, CutId(u64::MAX));
        assert_eq!(h.load_region(far).unwrap(), Some(stored(&p)));
        assert_eq!(h.load_in_flight().unwrap(), cut.in_flight);
        assert_eq!(h.world_info().unwrap().world_tick, u64::MAX);
        h.shutdown();
    });
}

#[test]
fn missing_region_is_none_not_error() {
    each_backend("missing", false, |url| {
        let h = open(url);
        assert_eq!(h.load_region(A).unwrap(), None);
        assert!(h.load_terrain(A).unwrap().is_none());
        assert!(h.stored_regions().unwrap().is_empty());
        assert!(h.load_in_flight().unwrap().is_empty());
        assert_eq!(h.load_history().unwrap(), None);
        assert_eq!(h.artifact().unwrap(), None);
        let info = h.world_info().unwrap();
        assert_eq!((info.content_crc, info.world_tick, info.has_artifact), (None, 0, false));

        h.put_artifact(vec![5, 6, 7], 0xDEAD_BEEF).unwrap();
        assert_eq!(h.artifact().unwrap(), Some(vec![5, 6, 7]));
        let info = h.world_info().unwrap();
        assert_eq!((info.content_crc, info.has_artifact), (Some(0xDEAD_BEEF), true));
        h.shutdown();
    });
}

#[test]
fn corrupt_chunk_is_error() {
    each_backend("corrupt", true, |url| {
        let h = open(url);
        let cut = first_cut(&h);
        h.commit_cut(cut).recv().unwrap().unwrap();
        h.shutdown();

        let garbage = match url {
            StoreUrl::Postgres(_) => "'\\x01ff00ff'::bytea",
            _ => "x'01ff00ff'",
        };
        raw_exec(url, &format!("UPDATE chunks SET data = {garbage} WHERE rx = 0 AND rz = 0 AND cx = 0"));

        let h = open(url);
        let err = h.load_region(A).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt(_)), "{err}");
        assert!(matches!(h.load_terrain(A), Err(StoreError::Corrupt(_))));
        assert!(h.load_region(B).unwrap().is_some(), "other regions still load");
        h.shutdown();
    });
}

#[test]
fn shutdown_drains_queued_commits() {
    each_backend("drain", true, |url| {
        let h = open(url);
        let cut = first_cut(&h);
        let rx = h.commit_cut(cut);
        h.shutdown();
        assert!(rx.recv().unwrap().is_ok(), "a commit queued before shutdown is committed");
        let h = open(url);
        assert_eq!(h.world_info().unwrap().world_tick, 1000);
        h.shutdown();
    });
}

/// A region's first row must carry its ground: `ground: None` means
/// "unchanged since the stored one", so on a new region it would be stored
/// empty. Refused as `Corrupt`, nothing committed; on a known region it is fine.
#[test]
fn first_region_row_without_ground_is_refused() {
    each_backend("noground", false, |url| {
        let h = open(url);
        let cut = |id, rc, ground| WorldCut {
            id: CutId(id),
            tick: 1000 + id,
            regions: vec![(rc, region(1000 + id, &[(ChunkCoords::new(0, 0, 0), chunk())], ground, vec![]))],
            in_flight: vec![],
            players: vec![],
            history: None,
        };
        let err = h.commit_cut(cut(1, A, false)).recv().unwrap().unwrap_err();
        assert!(matches!(&err, StoreError::Corrupt(m) if m.contains("no ground")), "{err}");
        assert_eq!(h.load_region(A).unwrap(), None, "nothing committed");
        h.commit_cut(cut(2, A, true)).recv().unwrap().unwrap();
        h.commit_cut(cut(3, A, false)).recv().unwrap().unwrap();
        assert_eq!(h.load_region(A).unwrap().unwrap().ground, region_ground(), "the stored ground is kept");
        h.shutdown();
    });
}
