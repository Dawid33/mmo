//! `worldctl`: world-store maintenance from the command line
//! (`src/bin/worldctl.rs` is `run` over `std::env::args`; `just worldctl …`,
//! `just forge-db <db> …`). Every command opens the store, which takes its
//! single-writer lock: a store another process holds (a running server) is
//! refused at once with the lock message, never waited for.
//!
//! - `forge --db <url> [--preset <name>] [--size <m>] [--cell <m>] [--seed <hex>]
//!   [--set <knob>=<value>]…`: forges a world in process
//!   (`worldgen::forge`, defaults = `ForgeParams::default()`, the flags as
//!   `just forge-proto` takes them) and stores it as tiles
//!   ([`crate::write_tiled`]). Refused when the store already holds a world.
//! - `retile --db <url>`: rebuilds the stored tiles from the archive row after
//!   the tile format changed (one transaction, [`crate::write_tiled`]). A
//!   store with no archive is refused: re-forge it.
//! - `export --db <url> <out.world>`: writes the stored forge file (the archive
//!   row) to `out.world`, for the file-based tools (`just world-map`,
//!   `cross-section`, `history`).

use std::path::Path;

use worldgen::forge::{self, ForgeParams};

use crate::{Dials, StoreHandle, StoreUrl};

/// What `worldctl` prints for a bad command line.
pub const USAGE: &str = "usage: worldctl forge --db <url> [--preset <name>] [--size <m>] [--cell <m>] \
[--seed <hex>] [--set <knob>=<value>]...\n       worldctl retile --db <url>\n       worldctl export --db <url> <out.world>";

/// Runs one `worldctl` command (`args` without the program name).
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("forge") => {
            let (db, params) = parse_forge(&args[1..])?;
            let crc = forge_into(&db, &params)?;
            println!("worldctl: forged a {} m world at {} m cells into {db:?} (content crc {crc:#010x})", params.size_m, params.cell_m);
            Ok(())
        }
        Some("retile") => {
            let db = parse_retile(&args[1..])?;
            let crc = retile(&db)?;
            println!("worldctl: re-tiled {db:?} from its archive (content crc {crc:#010x})");
            Ok(())
        }
        Some("export") => {
            let (db, out) = parse_export(&args[1..])?;
            let bytes = export(&db, Path::new(&out))?;
            println!("worldctl: exported {bytes} bytes to {out}");
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

fn parse_db(value: &str) -> Result<StoreUrl, String> {
    StoreUrl::parse(value).map_err(|e| format!("--db {value:?}: {e}"))
}

/// `forge`'s flags: the store and the forge parameters.
pub fn parse_forge(args: &[String]) -> Result<(StoreUrl, ForgeParams), String> {
    let mut db = None;
    let mut p = ForgeParams { quiet: false, ..ForgeParams::default() };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value\n{USAGE}"));
        match flag.as_str() {
            "--db" => db = Some(parse_db(value()?)?),
            "--size" => p.size_m = value()?.parse().map_err(|_| "--size <metres>".to_string())?,
            "--cell" => p.cell_m = value()?.parse().map_err(|_| "--cell <metres>".to_string())?,
            "--seed" => {
                let v = value()?;
                let hex = v.trim_start_matches("0x").replace('_', "");
                p.seed = u64::from_str_radix(&hex, 16).map_err(|_| format!("--seed {v}: not a hex number"))?;
            }
            // A generator preset (forge::presets, by name, any case): replaces
            // every knob, so give it before any --set.
            "--preset" => {
                let name = value()?;
                let preset = forge::presets::PRESETS
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| format!("--preset: no preset named {name} (see forge/presets.rs)"))?;
                p.knobs = forge::presets::apply(preset);
            }
            "--set" => {
                let v = value()?;
                let (key, number) = v.split_once('=').ok_or("--set <knob>=<value>")?;
                let number: f64 = number.parse().map_err(|_| format!("--set {key}: not a number"))?;
                if !p.knobs.set(key, number) {
                    return Err(format!("--set: no knob named {key} (see forge/knobs.rs)"));
                }
            }
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok((db.ok_or_else(|| format!("forge needs --db <url>\n{USAGE}"))?, p))
}

fn parse_retile(args: &[String]) -> Result<StoreUrl, String> {
    match args {
        [flag, value] if flag == "--db" => parse_db(value),
        _ => Err(USAGE.to_string()),
    }
}

fn parse_export(args: &[String]) -> Result<(StoreUrl, String), String> {
    let (mut db, mut out) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--db" => db = Some(parse_db(it.next().ok_or_else(|| format!("--db needs a value\n{USAGE}"))?)?),
            path if out.is_none() && !path.starts_with("--") => out = Some(path.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    match (db, out) {
        (Some(db), Some(out)) => Ok((db, out)),
        _ => Err(USAGE.to_string()),
    }
}

fn open(db: &StoreUrl) -> Result<StoreHandle, String> {
    StoreHandle::open(db, Dials::BUILT).map_err(|e| format!("world store {db:?}: {e}"))
}

/// Forges `params` and stores it in `db` as tiles; returns the world's
/// `content_crc`. Refused (before forging) when `db` already holds a world
/// or saved regions: a world is never written over another.
pub fn forge_into(db: &StoreUrl, params: &ForgeParams) -> Result<u32, String> {
    let store = open(db)?;
    let result = (|| {
        let info = store.world_info().map_err(|e| e.to_string())?;
        let regions = store.stored_regions().map_err(|e| e.to_string())?;
        if info.has_artifact || !regions.is_empty() {
            return Err(format!("{db:?} already holds a world; forge into a new database"));
        }
        let bytes = std::panic::catch_unwind(|| forge::forge(params).artifact(params).to_bytes())
            .map_err(|e| format!("the forge failed: {}", forge::play::panic_message(e.as_ref())))?;
        crate::write_tiled(&store, bytes).map_err(|e| e.to_string())
    })();
    store.shutdown();
    result
}

/// Writes `db`'s archived forge file to `out` (atomically: a temp file
/// renamed into place); returns its size.
pub fn export(db: &StoreUrl, out: &Path) -> Result<usize, String> {
    let store = open(db)?;
    let archive = store.forged_archive();
    store.shutdown();
    let bytes = archive
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{db:?} holds no forged world to export"))?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = worldgen::forged::save_temp_path(out);
    std::fs::write(&tmp, &bytes)
        .and_then(|()| std::fs::rename(&tmp, out))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", out.display())
        })?;
    Ok(bytes.len())
}

/// Rebuilds `db`'s tiles from its archived forge file, in one transaction
/// (`write_tiled`); returns the world's `content_crc`. A store with no
/// archive cannot be re-tiled.
pub fn retile(db: &StoreUrl) -> Result<u32, String> {
    let store = open(db)?;
    let result = (|| {
        let archive = store
            .forged_archive()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{db:?} keeps no archived forge file to re-tile from; re-forge the world"))?;
        crate::write_tiled(&store, archive).map_err(|e| e.to_string())
    })();
    store.shutdown();
    result
}
