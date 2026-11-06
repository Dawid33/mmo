//! Who a connection is: the [`PlayerStore`] turns the [`PlayerToken`] a client
//! sends in its `Hello` into a stable [`PlayerId`], plus the [`PlayerRecord`]
//! kept for that player while it is offline.
//!
//! Resolution happens at the network edge (QUIC and WebTransport ingress), on
//! tokio's blocking pool, BEFORE the session is announced to the
//! `WorldManager` — which stays I/O-free and only sees the resolved id. That
//! edge is where an external account database slots in later (an async impl),
//! without touching the manager.
//!
//! Two stores:
//! - [`FilePlayerStore`] for forged worlds: `<world>.players/`
//!   (`worlds/foo/terrain.world` -> `worlds/foo/terrain.players/`) holding
//!   `index.ron` (token -> id, written atomically on every new player) and one
//!   `p.<id>.player` per player with a record: a header ([`PLAYER_MAGIC`],
//!   [`PLAYER_FILE_VERSION`], a crc32 of the payload), then the lz4-compressed
//!   bincode record, written as temp + rename like `region_store.rs`. A record
//!   that will not load is set aside as `.bak` (the player keeps its id and
//!   starts fresh); an index that will not load stops the server (every
//!   identity hangs off it — it is never replaced silently).
//! - [`MemoryPlayerStore`] for procedural worlds and tests: nothing on disk.
//!
//! Tokens are bearer secrets, stored as they are: there is no authentication
//! yet (it comes with the external account service).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use game::{ClientPacket, PlayerId, PlayerRecord, PlayerToken};

/// Resolves tokens to players and keeps their offline records.
pub trait PlayerStore: Send {
    /// The player of `token` and its record (if one was stored). An unknown
    /// token becomes a new player with a fresh id, never reused.
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)>;
    /// Keep these records (replacing any older one of the same player).
    fn store(&mut self, records: Vec<(PlayerId, PlayerRecord)>) -> io::Result<()>;
}

/// The store as the network edge shares it between connection tasks.
pub type SharedPlayerStore = Arc<Mutex<dyn PlayerStore>>;

/// Wrap a store for the network edge.
pub fn shared(store: impl PlayerStore + 'static) -> SharedPlayerStore {
    Arc::new(Mutex::new(store))
}

/// A batch of player records for the store (`WorldManager::take_player_saves`).
pub type PlayerSaves = Vec<(PlayerId, PlayerRecord)>;

/// The `player-saver` thread: writes each batch it receives through `store`,
/// off the manager thread (a file write, later a database round trip). Ends
/// when the sender is dropped, after the batches already sent; join it to know
/// they are written. A failed write is logged, never fatal.
pub fn spawn_saver(store: SharedPlayerStore) -> (crossbeam::channel::Sender<PlayerSaves>, std::thread::JoinHandle<()>) {
    let (send, recv) = crossbeam::channel::unbounded::<PlayerSaves>();
    let handle = std::thread::Builder::new()
        .name("player-saver".into())
        .spawn(move || {
            while let Ok(batch) = recv.recv() {
                let n = batch.len();
                let result = match store.lock() {
                    Ok(mut store) => store.store(batch),
                    Err(poisoned) => poisoned.into_inner().store(batch),
                };
                match result {
                    Ok(()) => log::debug!("[server] saved {n} player record(s)"),
                    Err(e) => log::error!("[server] cannot save {n} player record(s): {e}"),
                }
            }
        })
        .expect("spawn the player saver thread");
    (send, handle)
}

/// How long a new connection may take to send its `Hello` before it is closed.
pub const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Largest first packet the edge reads (a `Hello` is a few dozen bytes).
pub const HELLO_MAX_BYTES: usize = 4096;

/// The handshake: `first` must be a `Hello` with a valid token, which the
/// store resolves on tokio's blocking pool. `Err` is the refusal sent back.
pub async fn resolve_hello(
    store: &SharedPlayerStore,
    first: ClientPacket,
) -> Result<(PlayerId, Option<PlayerRecord>), String> {
    let ClientPacket::Hello { token } = first else {
        return Err("the first packet must be Hello".into());
    };
    if !token.is_valid() {
        return Err(format!("invalid player token {token:?}"));
    }
    let store = store.clone();
    let resolved = tokio::task::spawn_blocking(move || {
        let mut store = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        store.resolve(&token)
    })
    .await
    .map_err(|e| format!("player store task failed: {e}"))?;
    resolved.map_err(|e| {
        log::error!("[server] player store: {e}");
        format!("the player store failed: {e}")
    })
}

/// No files: ids from 0 per process. Procedural worlds and tests.
#[derive(Default)]
pub struct MemoryPlayerStore {
    ids: BTreeMap<PlayerToken, PlayerId>,
    records: BTreeMap<PlayerId, PlayerRecord>,
    next_id: u64,
}

impl PlayerStore for MemoryPlayerStore {
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        let id = *self.ids.entry(token.clone()).or_insert_with(|| {
            let id = PlayerId(self.next_id);
            self.next_id += 1;
            id
        });
        Ok((id, self.records.get(&id).cloned()))
    }

    fn store(&mut self, records: Vec<(PlayerId, PlayerRecord)>) -> io::Result<()> {
        self.records.extend(records);
        Ok(())
    }
}

/// `terrain.world` -> `terrain.players`.
pub fn players_dir_for(world_file: &Path) -> PathBuf {
    world_file.with_extension("players")
}

pub const PLAYER_MAGIC: [u8; 8] = *b"MMOPLYR\0";
/// Bump when the header changes. (A change to the sim's state types changes
/// the payload without this: such a record fails to decode and is set aside.)
/// (Not bumped for `Client::open_container` becoming a `WorldBlockPos`: a
/// stored record always holds `None` there — `parked_client` closes it — and
/// `None` encodes the same; `tests/player_store.rs` pins it.)
pub const PLAYER_FILE_VERSION: u32 = 5;
/// magic + version + payload crc.
const HEADER_LEN: usize = 8 + 4 * 2;
const INDEX_FILE: &str = "index.ron";

/// `index.ron`: every token ever seen and its player.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PlayerIndex {
    next_id: u64,
    players: BTreeMap<String, u64>,
}

/// Players of one forged world, on disk next to it.
pub struct FilePlayerStore {
    dir: PathBuf,
    index: PlayerIndex,
}

impl FilePlayerStore {
    /// Open (or start) the store in `dir`. Fails if the index exists but will
    /// not load: it is never replaced, the caller must decide.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        let path = dir.join(INDEX_FILE);
        let index = match std::fs::read_to_string(&path) {
            Ok(text) => ron::from_str(&text).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => PlayerIndex::default(),
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        Ok(Self { dir, index })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where `id`'s record lives.
    pub fn record_path(&self, id: PlayerId) -> PathBuf {
        self.dir.join(format!("p.{}.player", id.0))
    }

    fn save_index(&self) -> io::Result<()> {
        let text = ron::ser::to_string_pretty(&self.index, ron::ser::PrettyConfig::default())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        write_atomic(&self.dir.join(INDEX_FILE), text.as_bytes())
    }

    fn load_record(&self, id: PlayerId) -> Option<PlayerRecord> {
        let path = self.record_path(id);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::error!("[server] cannot read {}: {e}; the player starts fresh", path.display());
                return None;
            }
        };
        match decode_record(&bytes) {
            Ok(record) => Some(record),
            Err(why) => {
                let bak = crate::world_file::set_aside_path(&path);
                match std::fs::rename(&path, &bak) {
                    Ok(()) => log::warn!(
                        "[server] {}: {why} — set aside as {}; the player starts fresh",
                        path.display(),
                        bak.display()
                    ),
                    Err(e) => log::error!(
                        "[server] {}: {why}; cannot move it to {}: {e}; the player starts fresh",
                        path.display(),
                        bak.display()
                    ),
                }
                None
            }
        }
    }
}

impl PlayerStore for FilePlayerStore {
    fn resolve(&mut self, token: &PlayerToken) -> io::Result<(PlayerId, Option<PlayerRecord>)> {
        if let Some(&id) = self.index.players.get(&token.0) {
            let id = PlayerId(id);
            return Ok((id, self.load_record(id)));
        }
        let id = self.index.next_id;
        self.index.players.insert(token.0.clone(), id);
        self.index.next_id += 1;
        // The new id is only handed out once it is on disk.
        if let Err(e) = self.save_index() {
            self.index.players.remove(&token.0);
            self.index.next_id -= 1;
            return Err(e);
        }
        Ok((PlayerId(id), None))
    }

    fn store(&mut self, records: Vec<(PlayerId, PlayerRecord)>) -> io::Result<()> {
        let mut first_error = None;
        for (id, record) in records {
            let written = encode_record(&record).and_then(|bytes| write_atomic(&self.record_path(id), &bytes));
            if let Err(e) = written {
                log::error!("[server] cannot save player {}: {e}", id.0);
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn encode_record(record: &PlayerRecord) -> io::Result<Vec<u8>> {
    let raw = bincode::serialize(record).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let payload = lz4_flex::compress_prepend_size(&raw);
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&PLAYER_MAGIC);
    out.extend_from_slice(&PLAYER_FILE_VERSION.to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

fn decode_record(bytes: &[u8]) -> Result<PlayerRecord, String> {
    if bytes.len() < HEADER_LEN || bytes[..8] != PLAYER_MAGIC {
        return Err("not a player record".into());
    }
    let word = |i: usize| u32::from_le_bytes(bytes[8 + 4 * i..12 + 4 * i].try_into().unwrap());
    if word(0) != PLAYER_FILE_VERSION {
        return Err(format!("player record version {}, expected {PLAYER_FILE_VERSION}", word(0)));
    }
    let payload = &bytes[HEADER_LEN..];
    if crc32fast::hash(payload) != word(1) {
        return Err("payload checksum mismatch".into());
    }
    let raw = lz4_flex::decompress_size_prepended(payload).map_err(|e| e.to_string())?;
    bincode::deserialize(&raw).map_err(|e| format!("record does not decode: {e}"))
}

/// Temp file + rename, creating the directory.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
