//! World autosave: every region's state on disk next to the world file, so a
//! restarted server picks up where the last one stopped.
//!
//! Layout: `<world>.regions/b<REGION_BLOCKS>-c<CHUNK_BLOCKS>/r.<x>.<z>.region`
//! (`worlds/foo/terrain.world` -> `worlds/foo/terrain.regions/b512-c16/…`), one
//! file per region that has ever run. The build dials name the directory
//! because region coordinates mean a different area under each, so switching
//! `region_blocks` just sees another (possibly empty) set of saves.
//!
//! A file is a header — [`REGION_MAGIC`], [`REGION_FILE_VERSION`], the two
//! dials, the forged world's `content_crc`, a crc32 of the payload — then the
//! lz4-compressed bincode `(Rollback, Vec<SavedTransfer>)` — the in-memory
//! parking format ([`game::SerializedRegion`]) followed by the crossings in
//! flight to the region at the save ([`game::RegionSave::into_serialized`]).
//! Writes are atomic (temp file + rename). A file that does not fit this
//! world/build or will not decode is moved aside to `<file>.bak` with a warning
//! and the region is generated afresh — it is never silently overwritten by the
//! next autosave.
//!
//! Who writes: [`spawn_saver`]'s thread, fed by the main loop from
//! `WorldManager::take_saves` (running regions' live clones every
//! [`crate::WORLD_SAVE_SECS`], parked blobs once after they park, everything
//! once more on an orderly exit). Who reads: the region's own thread, when a
//! region that has not run in this process starts (`region_threads`).

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam::channel::{unbounded, Receiver, Sender};
use game::{RegionCoords, RegionSave, Rollback, SavedRegion, SerializedRegion};

pub const REGION_MAGIC: [u8; 8] = *b"MMOREGN\0";
/// Bump when the header changes. (A change to the sim's state types changes
/// the bincode payload without this: such a file fails to decode and is set
/// aside, like any other unreadable one.)
/// 7: terrain slopes — pre-slope saves (half-block terrain, a `Walker` without
/// `walk_up`) are set aside as `.bak` and their regions regenerate.
/// 8: `EntityId` — `GameData` gained `region` / `next_entity_serial` / `id_index`
/// and `Ecs.id`, and `ghosts` / `arrivals` are keyed by it.
/// 9: the payload is `(Rollback, Vec<SavedTransfer>)` — the crossings in flight
/// to the region when it was saved, queued again on load.
/// 10: `GameData` gained `halo` (the terrain halo, `game::halo`).
/// 11: entity halo — `ghosts` became `halo_entities` (no TTL tick; kind and
/// collider kept), `GameData` gained `halo_sent`.
/// 12: `GameData` gained the intent queues (`intents`, `intent_replies`,
/// `intent_inbox` — `game::intents`).
/// 13: a queued intent reply (`SentReply`) gained `hops`.
/// 14: `GameData` gained `halo_ground` (the neighbours' ground recipes under
/// the halo); `Intent::BreakBlock` gained `survival`, `Intent::PlaceBlock` is new.
/// 15: item piles keyed by `WorldBlockPos`; `GameData` gained `halo_piles` and
/// `pile_takes`; `HaloUpdate` gained `piles`; `Intent::DepositItems` /
/// `TakeFromPile` and `IntentRefusal::PileFull` are new.
/// 16: chests keyed by `WorldBlockPos`; `GameData` gained `halo_containers`;
/// `Client::open_container` is a `WorldBlockPos`; `HaloUpdate` gained
/// `containers`; `Intent::PutInContainer` / `TakeFromContainer` and
/// `IntentRefusal::ContainerFull` are new.
/// 17: walks plan across seams (`path_plan.rs`): `Goal` is a world block
/// column, `Walker::path` world tiles, `Walker::exit` became `to_goal`.
/// 18: a wandering person's home (`people::Wander::home`) is a world block
/// column (`[i64; 2]`), so it means the same place across seams.
/// 19: `BodyState` (in saved in-flight transfers) carries the body's user
/// force/torque and soft-CCD distance (`EntityKind::Body`).
/// 20: `Carried` (in saved in-flight transfers) carries the entity's `drops`.
/// 21: `VoxelBody` gained `drops`; voxel bodies are `EntityKind::Body` with an
/// `EntityId`, and `Carried` gained `voxel_body`.
/// 22: `ColumnGround` gained `water_top` / `water_body` (recipe water) and
/// `GameData` gained `water_bodies`.
/// 23: chunks hold no water blocks — water is the recipe's alone (a v22
/// region's sea would be liquid blocks the sim no longer reads as water).
pub const REGION_FILE_VERSION: u32 = 23;
/// magic + version + region blocks + chunk blocks + world crc + payload crc.
const HEADER_LEN: usize = 8 + 4 * 5;

/// `terrain.world` -> `terrain.regions/b<REGION_BLOCKS>-c<CHUNK_BLOCKS>`.
pub fn regions_dir_for(world_file: &Path) -> PathBuf {
    world_file
        .with_extension("regions")
        .join(format!("b{}-c{}", game::REGION_BLOCKS, game::CHUNK_BLOCKS))
}

#[derive(Debug)]
pub enum RegionFileError {
    Io(std::io::Error),
    NotARegionFile,
    Version(u32),
    /// Saved under other build dials (region or chunk size).
    OtherBuild { region_blocks: u32, chunk_blocks: u32 },
    OtherWorld { found: u32, expected: u32 },
    Corrupt(String),
}

impl std::fmt::Display for RegionFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::NotARegionFile => write!(f, "not a region save"),
            Self::Version(v) => write!(f, "region save version {v}, expected {REGION_FILE_VERSION}"),
            Self::OtherBuild { region_blocks, chunk_blocks } => write!(
                f,
                "saved at region_blocks={region_blocks} chunk_blocks={chunk_blocks}, this build is {}/{}",
                game::REGION_BLOCKS,
                game::CHUNK_BLOCKS
            ),
            Self::OtherWorld { found, expected } => {
                write!(f, "saved for world {found:08x}, this world is {expected:08x}")
            }
            Self::Corrupt(why) => write!(f, "corrupt: {why}"),
        }
    }
}

/// The region saves of one world.
#[derive(Debug, Clone)]
pub struct RegionStore {
    dir: PathBuf,
    /// `ForgedWorld::content_crc` of the world the saves belong to.
    world_crc: u32,
}

impl RegionStore {
    pub fn new(dir: PathBuf, world_crc: u32) -> Self {
        Self { dir, world_crc }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn world_tick_path(&self) -> PathBuf {
        self.dir.join("world.tick")
    }

    /// Persist the world tick (`<dir>/world.tick`, atomic temp + rename).
    pub fn save_world_tick(&self, tick: game::Tick) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.world_tick_path();
        let tmp = path.with_extension("tick.tmp");
        std::fs::write(&tmp, tick.to_le_bytes())?;
        std::fs::rename(&tmp, &path)
    }

    /// The saved world tick; `None` when missing or unreadable.
    pub fn load_world_tick(&self) -> Option<game::Tick> {
        let bytes = std::fs::read(self.world_tick_path()).ok()?;
        Some(game::Tick::from_le_bytes(bytes.try_into().ok()?))
    }

    /// Where the world clock starts: the saved world tick, or — when
    /// `world.tick` is missing or unreadable but region files exist — the
    /// latest tick any of them was saved at (each is decoded: slow, but only
    /// on this recovery path), with a warning. Starting at 0 instead would
    /// leave every restored region (which never moves its clock back) waiting
    /// for the world to catch up with it. No region files: 0.
    pub fn start_tick(&self) -> game::Tick {
        if let Some(tick) = self.load_world_tick() {
            return tick;
        }
        let files = self.region_files();
        if files.is_empty() {
            return 0;
        }
        let mut latest: game::Tick = 0;
        for path in &files {
            let saved = std::fs::read(path).ok().and_then(|bytes| self.decode(&bytes).ok()).and_then(|b| b.to_saved().ok());
            let Some(saved) = saved else { continue };
            latest = latest.max(*saved.state.tick);
            for (_, stamp) in &saved.transfers {
                latest = latest.max(*stamp);
            }
        }
        log::warn!(
            "[server] {} is missing or unreadable but {} region file(s) exist: the world clock starts at tick {latest}, \
             the latest they were saved at",
            self.world_tick_path().display(),
            files.len()
        );
        latest
    }

    /// The region files in the store's directory (`r.<x>.<z>.region`).
    fn region_files(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("r.") && n.ends_with(".region"))
            })
            .collect();
        files.sort();
        files
    }

    pub fn path(&self, rc: RegionCoords) -> PathBuf {
        self.dir.join(format!("r.{}.{}.region", rc.x, rc.z))
    }

    pub fn encode(&self, region: &SerializedRegion) -> Vec<u8> {
        let payload = lz4_flex::compress_prepend_size(&region.0);
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.extend_from_slice(&REGION_MAGIC);
        out.extend_from_slice(&REGION_FILE_VERSION.to_le_bytes());
        out.extend_from_slice(&(game::REGION_BLOCKS as u32).to_le_bytes());
        out.extend_from_slice(&(game::CHUNK_BLOCKS as u32).to_le_bytes());
        out.extend_from_slice(&self.world_crc.to_le_bytes());
        out.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<SerializedRegion, RegionFileError> {
        if bytes.len() < HEADER_LEN || bytes[..8] != REGION_MAGIC {
            return Err(RegionFileError::NotARegionFile);
        }
        let word = |i: usize| u32::from_le_bytes(bytes[8 + 4 * i..12 + 4 * i].try_into().unwrap());
        let version = word(0);
        if version != REGION_FILE_VERSION {
            return Err(RegionFileError::Version(version));
        }
        let (region_blocks, chunk_blocks) = (word(1), word(2));
        if region_blocks as usize != game::REGION_BLOCKS || chunk_blocks as usize != game::CHUNK_BLOCKS {
            return Err(RegionFileError::OtherBuild { region_blocks, chunk_blocks });
        }
        if word(3) != self.world_crc {
            return Err(RegionFileError::OtherWorld { found: word(3), expected: self.world_crc });
        }
        let payload = &bytes[HEADER_LEN..];
        if crc32fast::hash(payload) != word(4) {
            return Err(RegionFileError::Corrupt("payload checksum mismatch".into()));
        }
        lz4_flex::decompress_size_prepended(payload)
            .map(SerializedRegion)
            .map_err(|e| RegionFileError::Corrupt(e.to_string()))
    }

    /// Write `region` atomically; returns the file size.
    pub fn save(&self, rc: RegionCoords, region: &SerializedRegion) -> std::io::Result<u64> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(rc);
        let bytes = self.encode(region);
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(bytes.len() as u64)
    }

    /// The saved state of `rc`, decoded, if there is a usable one. A file that
    /// will not load is set aside ([`Self::set_aside`]) so the region starts
    /// fresh and the next autosave cannot overwrite it.
    pub fn load(&self, rc: RegionCoords) -> Option<SavedRegion> {
        let path = self.path(rc);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::error!("[server] cannot read {}: {e}; generating the region", path.display());
                return None;
            }
        };
        let decoded = self.decode(&bytes).and_then(|blob| {
            blob.to_saved().map_err(|e| RegionFileError::Corrupt(format!("state does not decode: {e}")))
        });
        match decoded {
            Ok(saved) => Some(saved),
            Err(e) => {
                self.set_aside(rc, &e);
                None
            }
        }
    }

    fn set_aside(&self, rc: RegionCoords, why: &RegionFileError) {
        let path = self.path(rc);
        let bak = crate::world_file::set_aside_path(&path);
        match std::fs::rename(&path, &bak) {
            Ok(()) => log::warn!(
                "[server] {}: {why} — set aside as {}; generating the region",
                path.display(),
                bak.display()
            ),
            Err(e) => log::error!(
                "[server] {}: {why}; cannot move it to {}: {e}; generating the region",
                path.display(),
                bak.display()
            ),
        }
    }
}

/// The saver thread: serializes, compresses and writes every region state it
/// is sent, off the manager and region threads. One `world_saved` perf event
/// per burst (each time its queue runs dry). It exits once every sender is
/// dropped — join the handle to be sure the last save is on disk.
pub fn spawn_saver(store: RegionStore) -> (Sender<(RegionCoords, RegionSave)>, JoinHandle<()>) {
    let (send, recv) = unbounded();
    let handle = std::thread::Builder::new()
        .name("world-saver".into())
        .spawn(move || saver_loop(&store, &recv))
        .expect("spawn the world saver thread");
    (send, handle)
}

fn saver_loop(store: &RegionStore, recv: &Receiver<(RegionCoords, RegionSave)>) {
    let mut burst: Option<(Instant, u32, u64)> = None;
    while let Ok((rc, save)) = recv.recv() {
        let (_, regions, bytes) = burst.get_or_insert((Instant::now(), 0, 0));
        match store.save(rc, &save.into_serialized()) {
            Ok(size) => {
                *regions += 1;
                *bytes += size;
            }
            Err(e) => log::error!("[server] cannot save region {},{} to {}: {e}", rc.x, rc.z, store.dir().display()),
        }
        if recv.is_empty() {
            if let Some((started, regions, bytes)) = burst.take() {
                game::perf_event!(
                    "world_saved",
                    "regions={regions} bytes={bytes} ms={}",
                    game::perf::ms(started.elapsed())
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mmo-region-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn blob() -> SerializedRegion {
        RegionSave::Live(Box::new(Rollback::new(None)), Vec::new()).into_serialized()
    }

    #[test]
    fn regions_dir_sits_next_to_the_world() {
        let dir = regions_dir_for(Path::new("worlds/foo/terrain.world"));
        assert_eq!(
            dir,
            PathBuf::from(format!("worlds/foo/terrain.regions/b{}-c{}", game::REGION_BLOCKS, game::CHUNK_BLOCKS))
        );
    }

    #[test]
    fn an_older_file_version_is_refused() {
        let store = RegionStore::new(scratch("oldver"), 7);
        let mut bytes = store.encode(&blob());
        bytes[8..12].copy_from_slice(&(REGION_FILE_VERSION - 1).to_le_bytes());
        assert!(matches!(store.decode(&bytes), Err(RegionFileError::Version(v)) if v == REGION_FILE_VERSION - 1));
    }

    #[test]
    fn world_tick_round_trips() {
        let store = RegionStore::new(scratch("tick"), 7);
        assert_eq!(store.load_world_tick(), None);
        store.save_world_tick(123_456).unwrap();
        assert_eq!(store.load_world_tick(), Some(123_456));
    }

    /// A lost `world.tick` must not restart the clock at 0 under regions
    /// saved at a later tick (they would wait for the world to catch up).
    #[test]
    fn lost_world_tick_starts_at_the_latest_region_save() {
        let store = RegionStore::new(scratch("lost-tick"), 7);
        assert_eq!(store.start_tick(), 0, "a new world starts at 0");
        let at = |tick: game::Tick| {
            let mut state = Rollback::new(None);
            state.new_transaction();
            state.tick.update(|t| *t = tick);
            state.forget();
            RegionSave::Live(Box::new(state), Vec::new()).into_serialized()
        };
        store.save(RegionCoords::new(0, 0), &at(5_000)).unwrap();
        store.save(RegionCoords::new(1, 0), &at(7_000)).unwrap();
        assert_eq!(store.start_tick(), 7_000, "world.tick missing");
        std::fs::write(store.world_tick_path(), b"junk").unwrap();
        assert_eq!(store.start_tick(), 7_000, "world.tick corrupt");
        store.save_world_tick(9_000).unwrap();
        assert_eq!(store.start_tick(), 9_000, "the saved world tick wins");
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn save_then_load_round_trips() {
        let store = RegionStore::new(scratch("round"), 7);
        let rc = RegionCoords::new(-1, 3);
        let original = blob();
        store.save(rc, &original).unwrap();
        assert!(store.path(rc).ends_with("r.-1.3.region"));
        assert_eq!(store.decode(&std::fs::read(store.path(rc)).unwrap()).unwrap().0, original.0);
        assert!(store.load(rc).is_some());
        assert!(store.load(RegionCoords::new(0, 0)).is_none(), "no file, no state");
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn other_world_is_set_aside() {
        let dir = scratch("other");
        let rc = RegionCoords::new(0, 0);
        RegionStore::new(dir.clone(), 1).save(rc, &blob()).unwrap();
        let store = RegionStore::new(dir.clone(), 2);
        assert!(matches!(
            store.decode(&std::fs::read(store.path(rc)).unwrap()),
            Err(RegionFileError::OtherWorld { found: 1, expected: 2 })
        ));
        assert!(store.load(rc).is_none());
        assert!(!store.path(rc).exists());
        assert!(crate::world_file::set_aside_path(&store.path(rc)).exists(), "kept as .bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_payload_is_set_aside() {
        let store = RegionStore::new(scratch("corrupt"), 7);
        let rc = RegionCoords::new(2, 2);
        store.save(rc, &blob()).unwrap();
        let mut bytes = std::fs::read(store.path(rc)).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(store.path(rc), &bytes).unwrap();
        assert!(matches!(store.decode(&bytes), Err(RegionFileError::Corrupt(_))));
        assert!(store.load(rc).is_none());
        assert_eq!(std::fs::read(crate::world_file::set_aside_path(&store.path(rc))).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn undecodable_state_is_set_aside() {
        let store = RegionStore::new(scratch("state"), 7);
        let rc = RegionCoords::new(0, 1);
        store.save(rc, &SerializedRegion(vec![1, 2, 3])).unwrap();
        assert!(store.load(rc).is_none());
        assert!(crate::world_file::set_aside_path(&store.path(rc)).exists());
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn saver_writes_everything_before_join() {
        let store = RegionStore::new(scratch("saver"), 7);
        let (send, handle) = spawn_saver(store.clone());
        for x in 0..3 {
            send.send((RegionCoords::new(x, 0), RegionSave::Parked(blob(), Vec::new()))).unwrap();
        }
        send.send((RegionCoords::new(9, 9), RegionSave::Live(Box::new(Rollback::new(None)), Vec::new()))).unwrap();
        drop(send);
        handle.join().unwrap();
        for rc in [(0, 0), (1, 0), (2, 0), (9, 9)] {
            assert!(store.load(RegionCoords::new(rc.0, rc.1)).is_some(), "{rc:?} saved");
        }
        let _ = std::fs::remove_dir_all(store.dir());
    }
}
