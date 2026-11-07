//! The server's player stores: a token resolves to the same `PlayerId` every
//! time (across reopening the world store), ids are never reused, and a
//! record comes back once a save cut has written it.
use game::{PlayerId, PlayerRecord, PlayerToken};
use server::player_store::{MemoryPlayerStore, PlayerStore, StorePlayers};
use world_store::{Dials, StoreHandle, StoreUrl};

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-player-store-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn token(n: u8) -> PlayerToken {
    PlayerToken::generate([n; 16])
}

fn record(x: i32) -> PlayerRecord {
    PlayerRecord {
        client: game::Client::default(),
        pose: game::Pose::from_translation(game::math::Vec3::new(1.5, 20.0, -3.0)),
        region: game::RegionCoords::new(x, -1),
        fortress: None,
        adventure: Some(game::history::SiteId(3)),
    }
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn resolve_same_token_same_id_across_reopen() {
    let dir = scratch("reopen");
    let url = StoreUrl::Sqlite(dir.join("w.sqlite"));
    let (a, b) = {
        let handle = StoreHandle::open(&url, Dials::BUILT).unwrap();
        let mut store = StorePlayers(handle.clone());
        let (a, ra) = store.resolve(&token(1)).unwrap();
        let (b, _) = store.resolve(&token(2)).unwrap();
        assert_ne!(a, b, "two tokens, two players");
        assert_eq!(store.resolve(&token(1)).unwrap().0, a, "same token, same player");
        assert!(ra.is_none(), "a new player has no record");
        // Records reach the store only in save cuts.
        let cut = game::WorldCut {
            id: game::CutId(1),
            tick: 5,
            regions: Vec::new(),
            in_flight: Vec::new(),
            players: vec![(a, record(4))],
            history: None,
        };
        handle.commit_cut(cut).recv().unwrap().expect("the cut commits");
        handle.shutdown();
        (a, b)
    };
    let handle = StoreHandle::open(&url, Dials::BUILT).unwrap();
    let mut store = StorePlayers(handle.clone());
    assert_eq!(store.resolve(&token(1)).unwrap(), (a, Some(record(4))), "the cut's record survives a reopen");
    assert_eq!(store.resolve(&token(2)).unwrap(), (b, None));
    let (c, _) = store.resolve(&token(3)).unwrap();
    assert!(c != a && c != b, "ids are never reused: {c:?}");
    handle.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_store_keeps_ids_only() {
    let mut memory = MemoryPlayerStore::default();
    let (m, none) = memory.resolve(&token(7)).unwrap();
    assert!(none.is_none());
    assert_eq!(memory.resolve(&token(7)).unwrap(), (m, None));
    assert_ne!(memory.resolve(&token(8)).unwrap().0, m);
    let _: PlayerId = m;
}
