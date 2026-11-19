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

/// The finest of [`PREVIEW_CELL_CHOICES`] whose `width_m × height_m` grid has
/// at most [`PREVIEW_MAX_CELLS`] cells (the coarsest if none does).
pub fn preview_cell_for(width_m: f64, height_m: f64) -> f64 {
    PREVIEW_CELL_CHOICES
        .iter()
        .copied()
        .find(|&cell| (width_m / cell) * (height_m / cell) <= PREVIEW_MAX_CELLS)
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

/// The cost of a `width_m × height_m` forge at `cell_m` (`nx · nz` cells).
pub fn forge_estimate(width_m: f64, height_m: f64, cell_m: f64) -> ForgeEstimate {
    let cells = (width_m / cell_m).round() * (height_m / cell_m).round();
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

/// One of the generator's world sizes: a name and the world's two sides. Every
/// size is 2:1 (west–east twice north–south) and a multiple of 512 m (the
/// largest `REGION_BLOCKS`) and of 128 m (the coarsest forge cell), so it is a
/// whole number of regions and of cells with no snapping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldSize {
    pub name: &'static str,
    pub width_m: f64,
    pub height_m: f64,
}

/// The generator's world sizes, smallest first. The default is
/// [`DEFAULT_WORLD_SIZE`] (Medium); `ForgeParams::default()` (server
/// auto-forge, tests) stays 16 km / 32 m.
pub const WORLD_SIZES: [WorldSize; 4] = [
    WorldSize { name: "Tiny", width_m: 25_600.0, height_m: 12_800.0 },
    WorldSize { name: "Small", width_m: 50_176.0, height_m: 25_088.0 },
    WorldSize { name: "Medium", width_m: 100_352.0, height_m: 50_176.0 },
    WorldSize { name: "Large", width_m: 200_704.0, height_m: 100_352.0 },
];
pub const DEFAULT_WORLD_SIZE: usize = 2;

/// The developers' size (not in the menu): small enough to forge in seconds.
pub const DEV_WORLD_SIZE: WorldSize = WorldSize { name: "Dev", width_m: 16_384.0, height_m: 8_192.0 };

/// The size called `name` (any case): one of [`WORLD_SIZES`] or `"dev"`.
pub fn world_size_named(name: &str) -> Option<WorldSize> {
    WORLD_SIZES.iter().chain([&DEV_WORLD_SIZE]).find(|s| s.name.eq_ignore_ascii_case(name)).copied()
}

/// The forms a world size takes on a command line (`worldctl forge`, `forge_proto`).
pub const SIZE_FORMS: &str = "--size <tiny|small|medium|large|dev> or --width <m> --height <m>";

/// `--size`'s value: a size name ([`world_size_named`]). A bare number is
/// refused — a world has two sides; give them with `--width` / `--height`.
pub fn size_flag(value: &str) -> Result<WorldSize, String> {
    world_size_named(value).ok_or_else(|| format!("--size {value}: not a size name; give {SIZE_FORMS}"))
}

/// The two sides given by `--width` / `--height` (both or neither) over `default`.
pub fn sides_flags(width: Option<f64>, height: Option<f64>, default: (f64, f64)) -> Result<(f64, f64), String> {
    match (width, height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Ok((w, h)),
        (None, None) => Ok(default),
        _ => Err(format!("--width and --height are given together, both above 0; give {SIZE_FORMS}")),
    }
}

/// "16 × 8 km", "100 × 50 km": a size's sides, rounded to whole km.
pub fn km_text(width_m: f64, height_m: f64) -> String {
    format!("{:.0} × {:.0} km", width_m / 1_000.0, height_m / 1_000.0)
}

/// The cells Play may forge at (cell, label), the default first. Which are
/// offered depends on the size: [`offered_cells`].
pub const WORLD_CELLS: [(f64, &str); 3] = [(16.0, "16 m"), (32.0, "32 m (coarse, fast)"), (8.0, "8 m (slow)")];

/// The Play forge runs inside the game client: no size / cell whose
/// estimated peak memory is above this is offered or forged.
pub const MAX_PLAY_FORGE_RAM_BYTES: f64 = 12.0 * 1024.0 * 1024.0 * 1024.0;

/// Indices into [`WORLD_CELLS`] whose forge of `width_m × height_m` is
/// estimated within [`MAX_PLAY_FORGE_RAM_BYTES`].
pub fn offered_cells(width_m: f64, height_m: f64) -> Vec<usize> {
    (0..WORLD_CELLS.len())
        .filter(|&i| forge_estimate(width_m, height_m, WORLD_CELLS[i].0).peak_ram_bytes <= MAX_PLAY_FORGE_RAM_BYTES)
        .collect()
}

/// The most a Play forge may use: [`MAX_PLAY_FORGE_RAM_BYTES`], or half the
/// machine's memory when that is smaller (`physical` unknown: the cap).
pub fn play_ram_limit_bytes(physical: Option<f64>) -> f64 {
    physical.map_or(MAX_PLAY_FORGE_RAM_BYTES, |m| (m / 2.0).min(MAX_PLAY_FORGE_RAM_BYTES))
}

/// Play's backstop: `Err(status line)` when the forge's estimated peak
/// memory is over [`play_ram_limit_bytes`].
pub fn check_play_ram(width_m: f64, height_m: f64, cell_m: f64, physical: Option<f64>) -> Result<(), String> {
    let need = forge_estimate(width_m, height_m, cell_m).peak_ram_bytes;
    let limit = play_ram_limit_bytes(physical);
    if need <= limit {
        return Ok(());
    }
    let why = if limit < MAX_PLAY_FORGE_RAM_BYTES { "half of this machine's memory" } else { "the forge's memory cap" };
    Err(format!(
        "{} at {cell_m:.0} m needs ≈ {:.0} GB to forge (est.), over {why} ({:.0} GB): pick a coarser World cell or a smaller size.",
        km_text(width_m, height_m),
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

