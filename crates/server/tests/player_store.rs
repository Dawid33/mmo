//! The server's player stores: a token resolves to the same `PlayerId` every
//! time (across reopening the files), and a stored record comes back.
use std::path::PathBuf;

use game::{PlayerId, PlayerRecord, PlayerToken};
use server::player_store::{FilePlayerStore, MemoryPlayerStore, PlayerStore};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mmo-player-store-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
fn resolve_same_token_same_id_across_reopen() {
    let dir = scratch("reopen");
    let (a, b) = {
        let mut store = FilePlayerStore::open(dir.clone()).unwrap();
        let (a, ra) = store.resolve(&token(1)).unwrap();
        let (b, _) = store.resolve(&token(2)).unwrap();
        assert_ne!(a, b, "two tokens, two players");
        assert_eq!(store.resolve(&token(1)).unwrap().0, a, "same token, same player");
        assert!(ra.is_none(), "a new player has no record");
        (a, b)
    };
    let mut store = FilePlayerStore::open(dir.clone()).unwrap();
    assert_eq!(store.resolve(&token(1)).unwrap().0, a);
    assert_eq!(store.resolve(&token(2)).unwrap().0, b);
    let (c, _) = store.resolve(&token(3)).unwrap();
    assert!(c != a && c != b, "ids are never reused: {c:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn store_then_resolve_returns_record() {
    let dir = scratch("record");
    let id = {
        let mut store = FilePlayerStore::open(dir.clone()).unwrap();
        let (id, _) = store.resolve(&token(7)).unwrap();
        store.store(vec![(id, record(4))]).unwrap();
        assert_eq!(store.resolve(&token(7)).unwrap(), (id, Some(record(4))));
        id
    };
    let mut store = FilePlayerStore::open(dir.clone()).unwrap();
    assert_eq!(store.resolve(&token(7)).unwrap(), (id, Some(record(4))), "survives a reopen");
    store.store(vec![(id, record(5))]).unwrap();
    assert_eq!(store.resolve(&token(7)).unwrap().1, Some(record(5)), "a later store replaces it");
    let _ = std::fs::remove_dir_all(&dir);

    let mut memory = MemoryPlayerStore::default();
    let (m, none) = memory.resolve(&token(7)).unwrap();
    assert!(none.is_none());
    memory.store(vec![(m, record(6))]).unwrap();
    assert_eq!(memory.resolve(&token(7)).unwrap(), (m, Some(record(6))));
    assert_ne!(memory.resolve(&token(8)).unwrap().0, m);
}

#[test]
fn corrupt_record_is_set_aside() {
    let dir = scratch("corrupt");
    let mut store = FilePlayerStore::open(dir.clone()).unwrap();
    let (id, _) = store.resolve(&token(9)).unwrap();
    store.store(vec![(id, record(1))]).unwrap();
    let path = store.record_path(id);
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    assert_eq!(store.resolve(&token(9)).unwrap(), (id, None), "the player keeps its id, not the record");
    assert!(!path.exists(), "the corrupt file is moved away");
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    assert_eq!(std::fs::read(PathBuf::from(bak)).unwrap(), bytes, "kept as .bak");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn players_dir_sits_next_to_the_world() {
    assert_eq!(
        server::player_store::players_dir_for(std::path::Path::new("worlds/foo/terrain.world")),
        PathBuf::from("worlds/foo/terrain.players")
    );
    let _: PlayerId = PlayerId(0);
}

/// Task 6.4 (ruling R27): `Client::open_container` changed from
/// `Option<[i32; 3]>` to `Option<WorldBlockPos>` without a
/// `PLAYER_FILE_VERSION` bump. A stored record always holds `None` there
/// (`parked_client` closes the chest; before 6.4 `Client::arrive` reset it),
/// and bincode writes `None` as one tag byte for either type: a record
/// written by the old code is byte for byte the new one, so it still loads.
/// (A `Some` would not — it is never stored.)
#[test]
fn record_with_the_old_open_container_layout_still_loads() {
    let rec = record(2);
    assert_eq!(rec.client.open_container, None, "a stored record never holds an open chest");
    let new = bincode::serialize(&rec).unwrap();
    assert_eq!(fields(&rec, &rec.client.open_container), new, "the field-by-field mirror is the record's layout");
    let old = fields(&rec, &None::<[i32; 3]>);
    assert_eq!(old, new, "an old record (no chest open) has the same bytes");
    assert_eq!(bincode::deserialize::<PlayerRecord>(&old).unwrap(), rec, "and decodes");
    let mut open = rec.clone();
    open.client.open_container = Some(game::WorldBlockPos::new(1, 2, 3));
    assert_ne!(
        fields(&rec, &Some([1i32, 2, 3])),
        bincode::serialize(&open).unwrap(),
        "only `None` is layout-compatible — and only `None` is ever stored"
    );
    // Through the file store, as the live server reads it.
    let dir = scratch("old-layout");
    let mut store = FilePlayerStore::open(dir.clone()).unwrap();
    let (id, _) = store.resolve(&token(11)).unwrap();
    store.store(vec![(id, rec.clone())]).unwrap();
    let mut store = FilePlayerStore::open(dir.clone()).unwrap();
    assert_eq!(store.resolve(&token(11)).unwrap(), (id, Some(rec)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `rec` encoded field by field (bincode encodes a struct as the tuple of
/// its fields), with `open` in place of `Client::open_container`.
fn fields<T: serde::Serialize>(rec: &PlayerRecord, open: &T) -> Vec<u8> {
    let c = &rec.client;
    bincode::serialize(&(
        (&c.input, &c.fps_cam_mode, &c.mode, &c.inventory, &c.inventory_open, &c.selected, &c.cursor),
        (&c.mining, &c.break_cooldown_until, open),
        (&rec.pose, &rec.region, &rec.fortress, &rec.adventure),
    ))
    .unwrap()
}
