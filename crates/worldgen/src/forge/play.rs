//! What the main-menu map generator needs from the forge, kept engine-free so
//! it is tested here: the preview's automatic cell size, the forge-time /
//! memory estimate shown before Play, and [`forge_to_file`], the cancellable
//! Play forge that never leaves a half-written world file behind.

#[cfg(not(target_arch = "wasm32"))]
use super::{forge_with, ForgeParams, ForgeProgress, ForgeStage, Forged};

/// Preview cell sizes [`preview_cell_for`] picks from, finest first.
pub const PREVIEW_CELL_CHOICES: [f64; 4] = [32.0, 64.0, 128.0, 256.0];

/// The preview stays at or under this many cells (512²): about a second to
/// forge, so it can follow the sliders.
pub const PREVIEW_MAX_CELLS: f64 = 262_144.0;

/// The finest of [`PREVIEW_CELL_CHOICES`] whose grid has at most
/// [`PREVIEW_MAX_CELLS`] cells (the coarsest if none does).
pub fn preview_cell_for(size_m: f64) -> f64 {
    PREVIEW_CELL_CHOICES
        .iter()
        .copied()
        .find(|&cell| (size_m / cell).powi(2) <= PREVIEW_MAX_CELLS)
        .unwrap_or(PREVIEW_CELL_CHOICES[PREVIEW_CELL_CHOICES.len() - 1])
}

/// Forge wall time per million cells at 1 M cells. Measured 2026-11-01 with
/// `just forge-bench` (release, rayon on all 20 logical cores of the dev box,
/// incl. writing the PNGs): 16 km / 16 m (1.0 M cells) 9.1 s, 32 km / 16 m
/// (4.0 M) 33.7 s. 64 km and 100 km at 16 m were not timed (minutes; the
/// 100 km / 16 m benchmark waits on the user), so those labels extrapolate.
pub const FORGE_SECS_PER_MCELL: f64 = 9.0;

/// Wall time grows as cells^this. The two measured points give 0.94; rounded
/// up because bigger grids fall out of cache and the pre-optimisation runs
/// grew faster than linearly.
pub const FORGE_COST_EXPONENT: f64 = 1.05;

/// Peak memory of a forge: a fixed base plus this per cell (the same runs:
/// 284 MB at 1.0 M cells, 813 MB at 4.0 M).
pub const FORGE_RAM_BYTES_PER_CELL: f64 = 180.0;
pub const FORGE_RAM_BASE_BYTES: f64 = 100.0e6;

/// What a forge at a size and cell is expected to cost (an estimate, scaled
/// from measured runs, see [`FORGE_SECS_PER_MCELL`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ForgeEstimate {
    pub secs: f64,
    pub peak_ram_bytes: f64,
}

pub fn forge_estimate(size_m: f64, cell_m: f64) -> ForgeEstimate {
    let cells = (size_m / cell_m).round().powi(2);
    ForgeEstimate {
        secs: FORGE_SECS_PER_MCELL * (cells / 1.0e6).powf(FORGE_COST_EXPONENT),
        peak_ram_bytes: FORGE_RAM_BASE_BYTES + FORGE_RAM_BYTES_PER_CELL * cells,
    }
}

impl ForgeEstimate {
    /// "≈ 10 s", "≈ 7 min", "≈ 1 h 20 min".
    pub fn time_text(&self) -> String {
        let secs = self.secs.max(1.0);
        if secs < 60.0 {
            format!("≈ {} s", ((secs / 5.0).ceil() * 5.0) as u64)
        } else if secs < 3_600.0 {
            format!("≈ {} min", (secs / 60.0).ceil() as u64)
        } else {
            let mins = (secs / 60.0).ceil() as u64;
            format!("≈ {} h {} min", mins / 60, mins % 60)
        }
    }

    /// "≈ 7 GB" / "≈ 300 MB".
    pub fn ram_text(&self) -> String {
        let gb = self.peak_ram_bytes / 1.0e9;
        if gb >= 1.0 { format!("≈ {} GB", gb.ceil() as u64) } else { format!("≈ {} MB", (gb * 10.0).ceil() as u64 * 100) }
    }

    /// The whole line, e.g. "≈ 7 min, ≈ 8 GB RAM (est.)".
    pub fn text(&self) -> String {
        format!("{}, {} RAM (est.)", self.time_text(), self.ram_text())
    }
}

/// The generator's world sizes (edge length, label). The default is
/// [`DEFAULT_WORLD_SIZE`], Dwarf Fortress's medium world (196 tiles of 512 m);
/// `ForgeParams::default()` (server auto-forge, tests) stays 16 km / 32 m.
pub const WORLD_SIZES: [(f64, &str); 4] =
    [(16_000.0, "16 km"), (32_000.0, "32 km"), (64_000.0, "64 km"), (100_352.0, "100 km (DF Medium)")];
pub const DEFAULT_WORLD_SIZE: usize = 3;

/// The cells Play may forge at (cell, label), the default first. Which are
/// offered depends on the size: [`offered_cells`].
pub const WORLD_CELLS: [(f64, &str); 3] = [(16.0, "16 m"), (32.0, "32 m (coarse, fast)"), (8.0, "8 m (slow)")];

/// The Play forge runs inside the game client: no size / cell whose
/// estimated peak memory is above this is offered or forged.
pub const MAX_PLAY_FORGE_RAM_BYTES: f64 = 12.0 * 1024.0 * 1024.0 * 1024.0;

/// Indices into [`WORLD_CELLS`] whose forge at `size_m` is estimated within
/// [`MAX_PLAY_FORGE_RAM_BYTES`] (100 km: 16 m and 32 m, not 8 m).
pub fn offered_cells(size_m: f64) -> Vec<usize> {
    (0..WORLD_CELLS.len())
        .filter(|&i| forge_estimate(size_m, WORLD_CELLS[i].0).peak_ram_bytes <= MAX_PLAY_FORGE_RAM_BYTES)
        .collect()
}

/// The most a Play forge may use: [`MAX_PLAY_FORGE_RAM_BYTES`], or half the
/// machine's memory when that is smaller (`physical` unknown: the cap).
pub fn play_ram_limit_bytes(physical: Option<f64>) -> f64 {
    physical.map_or(MAX_PLAY_FORGE_RAM_BYTES, |m| (m / 2.0).min(MAX_PLAY_FORGE_RAM_BYTES))
}

/// Play's backstop: `Err(status line)` when the forge's estimated peak
/// memory is over [`play_ram_limit_bytes`].
pub fn check_play_ram(size_m: f64, cell_m: f64, physical: Option<f64>) -> Result<(), String> {
    let need = forge_estimate(size_m, cell_m).peak_ram_bytes;
    let limit = play_ram_limit_bytes(physical);
    if need <= limit {
        return Ok(());
    }
    let why = if limit < MAX_PLAY_FORGE_RAM_BYTES { "half of this machine's memory" } else { "the forge's memory cap" };
    Err(format!(
        "{:.0} km at {cell_m:.0} m needs ≈ {:.0} GB to forge (est.), over {why} ({:.0} GB): pick a coarser World cell or a smaller size.",
        size_m / 1_000.0,
        (need / 1.0e9).ceil(),
        limit / 1.0e9
    ))
}

/// `MemTotal` of a /proc/meminfo text, in bytes.
pub fn parse_meminfo_total(meminfo: &str) -> Option<f64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024.0)
}

/// The machine's physical memory where it can be read without a dependency
/// (Linux `/proc/meminfo`); `None` elsewhere.
pub fn physical_memory_bytes() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(parse_meminfo_total)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// A panic payload as text (`panic!` with a literal or a format string).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Why [`forge_to_file`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeFileError {
    /// `cancel` was set; checked after every erosion iteration and between
    /// the later stages.
    Cancelled,
    /// The forge panicked (e.g. a world with no dry land for a spawn) or the
    /// file could not be written.
    Failed(String),
}

/// Forge `p` and write its world file to `path` atomically. The bytes go to
/// [`crate::forged::save_temp_path`]`(path)`, which is renamed to `path` only
/// when everything succeeded and `cancel` is still clear; on cancel, a panic
/// or an I/O error the temp file is removed, so neither file exists
/// afterwards. `path`'s directory is created if missing. `on_progress` sees
/// every `forge_with` report, then a [`ForgeStage::Saving`] one.
///
/// Returns the forged world (for a preview image) on success.
#[cfg(not(target_arch = "wasm32"))]
pub fn forge_to_file(
    p: &ForgeParams,
    path: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    on_progress: &mut dyn FnMut(ForgeProgress),
) -> Result<Forged, ForgeFileError> {
    use std::ops::ControlFlow;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::Ordering;

    let cancelled = || cancel.load(Ordering::SeqCst);
    let guarded = |what: &str, payload: Box<dyn std::any::Any + Send>| {
        ForgeFileError::Failed(format!("{what}: {}", panic_message(payload.as_ref())))
    };
    let tmp = crate::forged::save_temp_path(path);
    let mut iters = 0;
    let result = (|| {
        let forged = catch_unwind(AssertUnwindSafe(|| {
            forge_with(p, &mut |pr| {
                iters = pr.iters;
                on_progress(pr);
                if cancelled() { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })
        }))
        .map_err(|e| guarded("the forge failed", e))?
        .ok_or(ForgeFileError::Cancelled)?;
        if cancelled() {
            return Err(ForgeFileError::Cancelled);
        }
        on_progress(ForgeProgress { stage: ForgeStage::Saving, iter: 0, iters });
        let bytes = catch_unwind(AssertUnwindSafe(|| forged.artifact(p).to_bytes()))
            .map_err(|e| guarded("the world could not be stored", e))?;
        if cancelled() {
            return Err(ForgeFileError::Cancelled);
        }
        let io = |e: std::io::Error| ForgeFileError::Failed(format!("{}: {e}", path.display()));
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        std::fs::write(&tmp, bytes).map_err(io)?;
        if cancelled() {
            return Err(ForgeFileError::Cancelled);
        }
        std::fs::rename(&tmp, path).map_err(io)?;
        Ok(forged)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_cell_for_sizes() {
        assert_eq!(preview_cell_for(16_000.0), 32.0);
        assert_eq!(preview_cell_for(32_000.0), 64.0);
        assert_eq!(preview_cell_for(64_000.0), 128.0);
        assert_eq!(preview_cell_for(100_352.0), 256.0);
        assert_eq!(preview_cell_for(4_096.0), 32.0);
        assert_eq!(preview_cell_for(1_000_000.0), 256.0, "the coarsest when none fits");
    }

    #[test]
    fn estimate_grows_with_cells_and_reads_as_minutes() {
        let at = |size, cell| forge_estimate(size, cell);
        assert!((at(16_000.0, 16.0).secs - FORGE_SECS_PER_MCELL).abs() < 0.1);
        assert!(at(32_000.0, 16.0).secs > 4.0 * at(16_000.0, 16.0).secs);
        let df = at(100_352.0, 16.0);
        assert!(df.secs > 300.0 && df.secs < 900.0, "{df:?}");
        assert!(df.time_text().ends_with(" min"), "{}", df.time_text());
        assert!(df.peak_ram_bytes > 6.0e9 && df.peak_ram_bytes < 9.0e9, "{df:?}");
        assert_eq!(ForgeEstimate { secs: 9.1, peak_ram_bytes: 2.8e8 }.text(), "≈ 10 s, ≈ 300 MB RAM (est.)");
        assert_eq!(ForgeEstimate { secs: 425.0, peak_ram_bytes: 7.2e9 }.text(), "≈ 8 min, ≈ 8 GB RAM (est.)");
        assert_eq!(ForgeEstimate { secs: 4_900.0, peak_ram_bytes: 1.0e9 }.time_text(), "≈ 1 h 22 min");
    }

    fn tiny() -> ForgeParams {
        let mut p = ForgeParams::square(4_096.0, 128.0);
        p.knobs.iters = 20.0;
        p
    }

    /// A fresh scratch directory under target/ (removed first).
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/forge-play-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Cancel at erosion iteration `at` (or, `None`, when saving starts).
    fn cancelled_at(name: &str, at: Option<usize>) -> (std::path::PathBuf, Result<Forged, ForgeFileError>, Vec<ForgeProgress>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let path = scratch(name).join("terrain.world");
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let result = forge_to_file(&tiny(), &path, &cancel, &mut |pr| {
            seen.push(pr);
            let hit = match at {
                Some(i) => pr.stage == ForgeStage::Eroding && pr.iter == i,
                None => pr.stage == ForgeStage::Saving,
            };
            if hit {
                cancel.store(true, Ordering::SeqCst);
            }
        });
        (path, result, seen)
    }

    #[test]
    fn cancelled_play_forge_leaves_no_file() {
        let (path, result, seen) = cancelled_at("cancel-iter-2", Some(2));
        assert_eq!(result.err(), Some(ForgeFileError::Cancelled));
        let last = seen.last().unwrap();
        assert_eq!((last.stage, last.iter), (ForgeStage::Eroding, 2), "stopped within one iteration");
        assert!(!path.exists());
        assert!(!crate::forged::save_temp_path(&path).exists());
    }

    #[test]
    fn cancel_while_saving_leaves_no_file() {
        let (path, result, _) = cancelled_at("cancel-saving", None);
        assert_eq!(result.err(), Some(ForgeFileError::Cancelled));
        assert!(!path.exists());
        assert!(!crate::forged::save_temp_path(&path).exists());
    }

    #[test]
    fn finished_forge_renames_the_temp_file() {
        let path = scratch("finished").join("nested/terrain.world");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut stages = Vec::new();
        let forged = forge_to_file(&tiny(), &path, &cancel, &mut |pr| stages.push(pr.stage)).expect("forged");
        assert_eq!(stages.last(), Some(&ForgeStage::Saving));
        assert!(path.is_file());
        assert!(!crate::forged::save_temp_path(&path).exists(), "the temp file is renamed away");
        let loaded = crate::forged::ForgedWorld::load(&path).expect("the written world loads");
        let _ = (loaded, forged.world.nx);
    }

    #[test]
    fn unwritable_path_fails_and_leaves_no_temp() {
        // The "directory" is a file, so creating it fails.
        let dir = scratch("blocked");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file"), b"x").unwrap();
        let path = dir.join("file/terrain.world");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let result = forge_to_file(&tiny(), &path, &cancel, &mut |_| {});
        assert!(matches!(result, Err(ForgeFileError::Failed(_))));
        assert!(!path.exists() && !crate::forged::save_temp_path(&path).exists());
    }

    #[test]
    fn no_offered_cell_exceeds_the_ram_cap() {
        for &(size_m, label) in &WORLD_SIZES {
            let offered = offered_cells(size_m);
            assert!(offered.contains(&0), "{label}: the default 16 m cell is offered");
            for &i in &offered {
                let ram = forge_estimate(size_m, WORLD_CELLS[i].0).peak_ram_bytes;
                assert!(ram <= MAX_PLAY_FORGE_RAM_BYTES, "{label} at {}: {ram}", WORLD_CELLS[i].1);
            }
        }
        let cells = |size_m| offered_cells(size_m).iter().map(|&i| WORLD_CELLS[i].0).collect::<Vec<_>>();
        assert_eq!(cells(32_000.0), [16.0, 32.0, 8.0]);
        assert_eq!(cells(100_352.0), [16.0, 32.0], "100 km keeps 16 m, loses 8 m");
    }

    #[test]
    fn play_refused_above_ram_cap() {
        let gib = 1024.0 * 1024.0 * 1024.0;
        assert_eq!(check_play_ram(100_352.0, 16.0, Some(31.0 * gib)), Ok(()));
        assert_eq!(check_play_ram(100_352.0, 16.0, None), Ok(()), "unknown memory: the cap alone");
        let over_cap = check_play_ram(100_352.0, 8.0, Some(64.0 * gib)).unwrap_err();
        assert!(over_cap.contains("GB") && over_cap.contains("8 m"), "{over_cap}");
        // Half of physical memory is the other limit: 16 m at 100 km on an 8 GiB box.
        let small_box = check_play_ram(100_352.0, 16.0, Some(8.0 * gib)).unwrap_err();
        assert!(small_box.contains("memory"), "{small_box}");
        assert_eq!(check_play_ram(16_000.0, 16.0, Some(8.0 * gib)), Ok(()));
        assert!(play_ram_limit_bytes(Some(8.0 * gib)) == 4.0 * gib);
        assert!(play_ram_limit_bytes(None) == MAX_PLAY_FORGE_RAM_BYTES);
    }

    #[test]
    fn meminfo_total_parses() {
        assert_eq!(parse_meminfo_total("MemTotal:       32617140 kB\nMemFree: 1 kB\n"), Some(32_617_140.0 * 1024.0));
        assert_eq!(parse_meminfo_total("nothing"), None);
    }

    #[test]
    fn panic_messages_read_back() {
        let s = std::panic::catch_unwind(|| panic!("no dry land")).unwrap_err();
        assert_eq!(panic_message(s.as_ref()), "no dry land");
        let n = 3;
        let s = std::panic::catch_unwind(|| panic!("{n} cells")).unwrap_err();
        assert_eq!(panic_message(s.as_ref()), "3 cells");
    }
}
