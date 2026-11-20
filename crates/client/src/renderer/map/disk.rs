//! The native disk cache of SERVED map tiles (spec 5.3; `MapSource::Http` only — a local world reads
//! its own store): `data_dir()/map/<world id>/<level>/<x>_<z>.tile`, each the tile's etag (u64 LE) then
//! its bytes, written to a temporary file and renamed into place (a reader never sees half a tile).
//! Above [`MAP_DISK_CACHE_BYTES`] the least recently used files go: the directory is scanned when the
//! cache opens and every [`MAP_DISK_SCAN_WRITES`] writes. A hit is shown at once and its etag sent as
//! `have` (`If-None-Match`), so the server answers 304 for a tile that did not change.
//!
//! Plain `std` on purpose (keys are `(level, x, z)`): nothing here knows Bevy or the tile format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The disk cache's cap (bytes of tile files), per player.
pub const MAP_DISK_CACHE_BYTES: u64 = 512 << 20;
/// Writes between two scans for eviction.
pub const MAP_DISK_SCAN_WRITES: u32 = 1000;

/// One world's tiles on disk.
pub struct DiskCache {
    dir: PathBuf,
    /// The whole cache (every world): what eviction scans.
    root: PathBuf,
    cap: u64,
    writes: u32,
}

impl DiskCache {
    /// The cache of world `world` under `root` (`data_dir()/map`), evicting down to `cap` first.
    pub fn open(root: &Path, world: &str, cap: u64) -> std::io::Result<DiskCache> {
        let dir = root.join(world);
        fs::create_dir_all(&dir)?;
        let cache = DiskCache {
            dir,
            root: root.to_path_buf(),
            cap,
            writes: 0,
        };
        cache.evict()?;
        Ok(cache)
    }

    fn path(&self, (level, x, z): (u8, i32, i32)) -> PathBuf {
        self.dir
            .join(level.to_string())
            .join(format!("{x}_{z}.tile"))
    }

    /// The cached etag and bytes of `key`; touches its time so eviction keeps it.
    pub fn get(&self, key: (u8, i32, i32)) -> Option<(u64, Vec<u8>)> {
        let path = self.path(key);
        let data = fs::read(&path).ok()?;
        if data.len() < 8 {
            return None;
        }
        let etag = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
        // Recently used = recently modified: a hit renews the file's time.
        let _ = fs::File::options()
            .append(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        Some((etag, data[8..].to_vec()))
    }

    /// Store `key`'s `etag` and `bytes` (temp file + rename).
    pub fn put(&mut self, key: (u8, i32, i32), etag: u64, bytes: &[u8]) -> std::io::Result<()> {
        let path = self.path(key);
        let parent = path.parent().expect("a tile file has a level directory");
        fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(
            ".{}.{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("tile"),
            std::process::id()
        ));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&etag.to_le_bytes())?;
            f.write_all(bytes)?;
        }
        fs::rename(&tmp, &path)?;
        self.writes += 1;
        if self.writes >= MAP_DISK_SCAN_WRITES {
            self.writes = 0;
            self.evict()?;
        }
        Ok(())
    }

    /// Delete the least recently used tile files (every world's) until the cache fits its cap.
    pub fn evict(&self) -> std::io::Result<()> {
        let mut files = Vec::new();
        collect(&self.root, &mut files)?;
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        if total <= self.cap {
            return Ok(());
        }
        files.sort_by_key(|(_, _, t)| *t);
        for (path, len, _) in files {
            if total <= self.cap {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }
}

/// Every `.tile` file under `dir`: (path, length, modified).
fn collect(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) -> std::io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let meta = entry.metadata()?;
        let path = entry.path();
        if meta.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "tile") {
            out.push((
                path,
                meta.len(),
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ));
        }
    }
    Ok(())
}
