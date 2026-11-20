//! `worldctl`: world-store maintenance from the command line
//! (`src/bin/worldctl.rs` is `run` over `std::env::args`; `just worldctl …`,
//! `just forge-db <db> …`). `forge` and `retile` open the store as its writer,
//! which on SQLite takes its single-writer lock: a store another process holds
//! (a running server) is refused at once with the lock message, never waited
//! for. `export` only reads: it opens read-only, beside a running server.
//!
//! - `forge --db <url> [--preset <name>] [--size <name>] [--cell <m>] [--seed <hex>]
//!   [--set <knob>=<value>]…`: forges a Planet in process
//!   (`worldgen::forge`, defaults = `ForgeParams::default()`: the dev size at
//!   32 m; the flags as `just forge-proto` takes them) and stores it as tiles
//!   ([`crate::write_tiled`]). Refused when the store already holds a world.
//! - `retile --db <url>`: rebuilds the stored tiles from the archive row after
//!   the tile format changed (one transaction, [`crate::write_tiled`]). A
//!   store with no archive is refused: re-forge it.
//! - `clock-reset <url>`: clears the world's clock anchor (R3: the shared tick numbering of
//!   host processes); the next host or coordinator start stores a fresh one.
//! - `super-region <url> <n>`: sets the world's super-region side (`world.super_region_regions`,
//!   regions per super-region side; hosts then run with `VOXEL_SUPER_REGION_REGIONS=<n>`).
//!   Refused once any host has claimed a super-region: the fences name super-regions of the
//!   old side. Safe against a host starting at the same time: on Postgres the change and a
//!   claim serialise on the world row (the change also locks `super_regions` against
//!   inserts), so either the claim commits first and the change is refused, or the change
//!   commits first and the claim — by a process that opened the store at the old side — is
//!   refused (`StoreError::OtherSuperRegionSide`); on SQLite the file lock admits one
//!   writer. `n = 1` puts a host seam one region from the spawn (R3's go / no-go world).
//!
//! - `hosts <url>` (Elastic 1): one line per `hosts` row, by id —
//!   `<id> <name> <live|dead> <clean|dirty> land=<super-regions owned> seen=<ms since its last
//!   beat, or vouched> drain=<ms until it leaves, or ->` (a name-less row shows `-`). The id is
//!   the bare number `retire-host` takes. Read-only (opened beside a running world).
//! - `desired <url>` (Elastic 4): one line per `desired_hosts` row, by slot —
//!   `<name> slot=<n> <wanted|draining> age=<ms since its add>` — the hosts the coordinator
//!   wants (only its decisions write them); none prints nothing, as `hosts`. Read-only; a SQLite
//!   store has none.
//! - `drain <url> <host id> [--in <secs>]` (Elastic 1): marks a dirty live host draining —
//!   it is to leave within `<secs>` (default [`game::DRAIN_WARNING_SECS`]) — and prints its
//!   `hosts` line. One statement over that one row ([`StoreHandle::drain_host`]); a mark only
//!   moves earlier (a second drain with more time leaves it, with less brings it forward; there
//!   is no un-drain). A draining host wins no new land and is not the gate host for logins; it
//!   learns the mark at its next heartbeat. Refused for an absent, dead or clean row, a row not
//!   beaten within the lease (its process is gone), and a notice above
//!   [`crate::DRAIN_MAX_SECS`]. Marking the only live host not draining yet adds a warning
//!   line: once it leaves there is no gate host (joins answer 503) and no claimer.
//! - `retire-host <url> <host id>` (R4, `claim` ownership, Postgres): retires a host that will
//!   never come back, so its dirty `hosts` row stops blocking every world cut (the id as
//!   `worldctl hosts` lists it: issued at registration, never reused). Refused while
//!   that host's row beat within [`game::HOST_LEASE_MS`] ("host N is running: stop it first"),
//!   and for an id with no dirty live row. With another host alive it only VOUCHES for the
//!   death ([`StoreHandle::vouch_host`]: `seen_ms = 0`, as a restart under its name would) — the
//!   coordinator declares it at its next read and a running host inherits through the usual
//!   succession (a vouched landless host is ended instead, Elastic 2a). With NO host alive it
//!   is refused (Elastic 2a): start one — the first registration finds nobody alive and its
//!   total-crash rule takes every dirty row's land. Under `static` ownership
//!   nothing declares a vouched row: there the remedy is `rollback` and a new host.
//! - `rollback <url> --yes` (R4, Postgres): the world restarts from its last committed cut —
//!   `world.generation + 1`, every open succession abandoned (its land and the predecessor's
//!   in-flight rows stay with the heir) — in one transaction under the `world` row's lock
//!   ([`StoreHandle::rollback_world_operator`]). Refused without `--yes`. Running hosts learn
//!   it at their next store write or peer hello (`StaleGeneration` / `HigherGeneration`: exit
//!   5, then restart from the cut); a running coordinator's next decision at the old
//!   generation is refused and recorded aborted, and it re-reads the open successions when a
//!   host attaches at the new generation.
//!
//! No command prints a Postgres URL's credentials: stores are shown by
//! [`StoreUrl`]'s `Display` (host and database only).
//! - `export --db <url> <out.world>`: writes the stored forge file (the archive
//!   row) to `out.world`, for the file-based tools (`just world-map`,
//!   `cross-section`, `history`).

use std::path::Path;

use game::HostId;
use worldgen::forge::{self, ForgeParams, play};

use crate::{Dials, OpenMode, StoreHandle, StoreUrl};

/// What `worldctl` prints for a bad command line.
pub const USAGE: &str = "usage: worldctl forge --db <url> \
[--preset <earth-like|great ranges|highlands|lowlands|dry|wet>] \
[--size <tiny|small|medium|large|dev> (default dev) | --width <m> --height <m>] [--cell <m>] \
[--seed <hex>] [--set <knob>=<value>]...\n       worldctl retile --db <url>\n       worldctl export --db <url> <out.world>\n       worldctl clock-reset <url>\n       worldctl super-region <url> <n>\n       worldctl hosts <url>\n       worldctl desired <url>\n       worldctl drain <url> <host id> [--in <secs>]\n       worldctl retire-host <url> <host id>\n       worldctl rollback <url> --yes\n       worldctl editor-grant <url> <account id>\n       worldctl editor-revoke <url> <account id>\n       worldctl editor-list <url>";

/// Runs one `worldctl` command (`args` without the program name); `Ok` is what it
/// prints. Neither that nor an error ever shows a Postgres URL's credentials.
pub fn run(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("forge") => {
            let (db, params) = parse_forge(&args[1..])?;
            let crc = forge_into(&db, &params)?;
            Ok(format!(
                "worldctl: forged a {} x {} m world at {} m cells into {db} (content crc {crc:#010x})",
                params.width_m, params.height_m, params.cell_m
            ))
        }
        Some("retile") => {
            let db = parse_retile(&args[1..])?;
            let crc = retile(&db)?;
            Ok(format!(
                "worldctl: re-tiled {db} from its archive (content crc {crc:#010x})"
            ))
        }
        Some("export") => {
            let (db, out) = parse_export(&args[1..])?;
            let bytes = export(&db, Path::new(&out))?;
            Ok(format!("worldctl: exported {bytes} bytes to {out}"))
        }
        Some("clock-reset") => {
            let db = parse_clock_reset(&args[1..])?;
            clock_reset(&db)?;
            Ok(format!(
                "worldctl: cleared the world clock anchor of {db}; the next host or coordinator start writes a new one"
            ))
        }
        Some("super-region") => {
            let (db, n) = parse_super_region(&args[1..])?;
            set_super_region(&db, n)?;
            Ok(format!(
                "worldctl: the world's super-regions are now {n} x {n} regions; run its hosts with VOXEL_SUPER_REGION_REGIONS={n}"
            ))
        }
        Some("hosts") => {
            let [url] = &args[1..] else {
                return Err(USAGE.to_string());
            };
            let db =
                StoreUrl::parse(url).map_err(|e| format!("hosts: the world store URL: {e}"))?;
            list_hosts(&db)
        }
        Some("desired") => {
            let [url] = &args[1..] else {
                return Err(USAGE.to_string());
            };
            let db =
                StoreUrl::parse(url).map_err(|e| format!("desired: the world store URL: {e}"))?;
            list_desired(&db)
        }
        Some("drain") => {
            let (db, id, in_ms) = parse_drain(&args[1..])?;
            drain_host(&db, id, in_ms)
        }
        Some("retire-host") => {
            let (db, id) = parse_retire_host(&args[1..])?;
            retire_host(&db, id)
        }
        Some("rollback") => {
            let db = parse_rollback(&args[1..])?;
            let (generation, abandoned) = rollback(&db)?;
            Ok(format!(
                "worldctl: rolled the world of {db} back to generation {generation} ({abandoned} open succession(s) \
                 abandoned): every host restarts from the last save"
            ))
        }
        Some("editor-grant") => {
            let (db, account_id) = parse_editor_account(&args[1..])?;
            let added = editor_grant(&db, &account_id)?;
            let action = if added { "granted" } else { "already granted" };
            Ok(format!(
                "worldctl: editor access {action} for account {account_id:?} in {db}; it applies at that account's next join"
            ))
        }
        Some("editor-revoke") => {
            let (db, account_id) = parse_editor_account(&args[1..])?;
            let removed = editor_revoke(&db, &account_id)?;
            let action = if removed { "revoked" } else { "already denied" };
            Ok(format!(
                "worldctl: editor access {action} for account {account_id:?} in {db}; an active session must be kicked and rejoin"
            ))
        }
        Some("editor-list") => {
            let [url] = &args[1..] else {
                return Err(USAGE.to_string());
            };
            let db = StoreUrl::parse(url)
                .map_err(|e| format!("editor-list: the world store URL: {e}"))?;
            editor_list(&db)
        }
        _ => Err(USAGE.to_string()),
    }
}

fn parse_editor_account(args: &[String]) -> Result<(StoreUrl, String), String> {
    let [url, account_id] = args else {
        return Err(USAGE.to_string());
    };
    let db = StoreUrl::parse(url).map_err(|e| format!("editor: the world store URL: {e}"))?;
    if account_id.is_empty() || account_id.len() > 256 {
        return Err("editor: account id must be 1..=256 bytes".into());
    }
    Ok((db, account_id.clone()))
}

fn editor_grant(db: &StoreUrl, account_id: &str) -> Result<bool, String> {
    let store = open(db, OpenMode::Writer)?;
    store
        .grant_editor(account_id)
        .map_err(|e| format!("world store {db}: {e}"))
}

fn editor_revoke(db: &StoreUrl, account_id: &str) -> Result<bool, String> {
    let store = open(db, OpenMode::Writer)?;
    store
        .revoke_editor(account_id)
        .map_err(|e| format!("world store {db}: {e}"))
}

fn editor_list(db: &StoreUrl) -> Result<String, String> {
    let store = open(db, OpenMode::ReadOnly)?;
    let accounts = store
        .editor_grants()
        .map_err(|e| format!("world store {db}: {e}"))?;
    if accounts.is_empty() {
        return Ok("worldctl: no editor grants".into());
    }
    Ok(accounts.join("\n"))
}

fn parse_db(value: &str) -> Result<StoreUrl, String> {
    StoreUrl::parse(value).map_err(|e| format!("--db {}: {e}", crate::redact_url(value)))
}

/// `forge`'s flags: the store and the forge parameters.
pub fn parse_forge(args: &[String]) -> Result<(StoreUrl, ForgeParams), String> {
    let mut db = None;
    let mut p = ForgeParams {
        quiet: false,
        ..ForgeParams::default()
    };
    let (mut width, mut height) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--db" => db = Some(parse_db(value()?)?),
            "--size" => {
                let size = play::size_flag(value()?)?;
                (width, height) = (Some(size.width_m), Some(size.height_m));
            }
            "--width" => {
                width = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--width <metres>".to_string())?,
                )
            }
            "--height" => {
                height = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--height <metres>".to_string())?,
                )
            }
            "--cell" => {
                p.cell_m = value()?
                    .parse()
                    .map_err(|_| "--cell <metres>".to_string())?
            }
            "--seed" => {
                let v = value()?;
                let hex = v.trim_start_matches("0x").replace('_', "");
                p.seed = u64::from_str_radix(&hex, 16)
                    .map_err(|_| format!("--seed {v}: not a hex number"))?;
            }
            // A generator preset (forge::presets, by name, any case): replaces
            // every knob, so give it before any --set.
            "--preset" => {
                let name = value()?;
                let preset = forge::presets::PRESETS
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        format!("--preset: no preset named {name} (see forge/presets.rs)")
                    })?;
                p.knobs = forge::presets::apply(preset);
            }
            // In the knob's range: load holds a stored knob to it.
            "--set" => p.knobs.set_flag(value()?)?,
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    (p.width_m, p.height_m) = play::sides_flags(width, height, (p.width_m, p.height_m))?;
    Ok((
        db.ok_or_else(|| format!("forge needs --db <url>\n{USAGE}"))?,
        p,
    ))
}

fn parse_retile(args: &[String]) -> Result<StoreUrl, String> {
    match args {
        [flag, value] if flag == "--db" => parse_db(value),
        _ => Err(USAGE.to_string()),
    }
}

/// `clock-reset`'s store: `<url>`, or `--db <url>` as the other commands take it.
fn parse_clock_reset(args: &[String]) -> Result<StoreUrl, String> {
    match args {
        [url] if !url.starts_with("--") => parse_db(url),
        _ => parse_retile(args),
    }
}

/// `super-region`'s store and side: `<url> <n>`, `n` a whole number >= 1 (what hosts accept
/// in `VOXEL_SUPER_REGION_REGIONS`). Errors never echo the URL.
fn parse_super_region(args: &[String]) -> Result<(StoreUrl, u32), String> {
    let [url, n] = args else {
        return Err(USAGE.to_string());
    };
    let n = n
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("super-region: n = {n:?} is not a whole number >= 1"))?;
    let db = StoreUrl::parse(url).map_err(|e| format!("super-region: the world store URL: {e}"))?;
    Ok((db, n))
}

/// `retire-host`'s store and host id: `<url> <host id>` (the bare number `worldctl hosts`
/// lists). Errors never echo the URL.
fn parse_retire_host(args: &[String]) -> Result<(StoreUrl, HostId), String> {
    let [url, id] = args else {
        return Err(USAGE.to_string());
    };
    let id = id.trim().parse::<u64>().map(HostId).map_err(|_| {
        format!("retire-host: {id:?} is not a host id (the number `worldctl hosts <url>` lists)")
    })?;
    let db = StoreUrl::parse(url).map_err(|e| format!("retire-host: the world store URL: {e}"))?;
    Ok((db, id))
}

/// `drain`'s arguments: `<url> <host id> [--in <secs>]` → the store, the id, the notice in ms
/// (default [`game::DRAIN_WARNING_SECS`]). Errors never echo the URL.
fn parse_drain(args: &[String]) -> Result<(StoreUrl, HostId, u64), String> {
    let (url, id, secs) = match args {
        [url, id] => (url, id, None),
        [url, id, flag, secs] if flag == "--in" => (url, id, Some(secs)),
        _ => return Err(USAGE.to_string()),
    };
    let id = id.trim().parse::<u64>().map(HostId).map_err(|_| {
        format!("drain: {id:?} is not a host id (the number `worldctl hosts <url>` lists)")
    })?;
    let secs = match secs {
        None => game::DRAIN_WARNING_SECS,
        Some(s) => s
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("drain: --in {s:?} is not a number of seconds"))?,
    };
    if secs > crate::DRAIN_MAX_SECS {
        return Err(format!(
            "drain: --in {secs} is longer than the longest notice, {} s (7 days, DRAIN_MAX_SECS)",
            crate::DRAIN_MAX_SECS
        ));
    }
    let in_ms = secs * 1000;
    let db = StoreUrl::parse(url).map_err(|e| format!("drain: the world store URL: {e}"))?;
    Ok((db, id, in_ms))
}

/// `worldctl drain`: marks host `id` of `db` draining within `in_ms` and prints its
/// [`host_line`] as it stands after the mark. Errors never echo the URL.
pub fn drain_host(db: &StoreUrl, id: HostId, in_ms: u64) -> Result<String, String> {
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::Writer)
        .map_err(|e| format!("drain: the world store: {e}"))?;
    let result = (|| {
        let fail = |e: crate::StoreError| format!("drain: {e}");
        store.drain_host(id, in_ms).map_err(fail)?;
        let rows = store.hosts().map_err(fail)?;
        let owners = store.owners().map_err(fail)?;
        let now = store.db_now_ms().map_err(fail)?;
        let row = rows
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("drain: host {}'s row is gone", id.0))?;
        let land = owners
            .iter()
            .filter(|(_, o, _)| u64::try_from(*o).is_ok_and(|o| o == id.0))
            .count();
        let line = host_line(row, land, now);
        // Another live host that is not draining: the next gate host and claimer. None: say so.
        let lease = store.host_lease_ms();
        let staying = rows.iter().any(|r| {
            r.id != id
                && !r.clean
                && r.state == crate::HOST_LIVE
                && r.seen_ms != 0
                && now.saturating_sub(r.seen_ms) <= lease
                && r.drain_in_ms.is_none()
        });
        if staying {
            return Ok(line);
        }
        Ok(format!(
            "{line}\nwarning: host {} was the only live host not draining: once it leaves the world has no gate host — \
             joins answer 503 until another host runs — and no host claims free land (start another host first)",
            id.0
        ))
    })();
    store.shutdown();
    result
}

/// What a `rollback` without `--yes` says.
const ROLLBACK_NEEDS_YES: &str = "rollback: refused without --yes — every host restarts from the last save (the world \
generation goes up by one, every open succession is abandoned; play since the last committed world cut is lost)";

/// `rollback`'s store: `<url> --yes`. Errors never echo the URL.
fn parse_rollback(args: &[String]) -> Result<StoreUrl, String> {
    let (url, yes) = match args {
        [url] => (url, false),
        [url, flag] if flag == "--yes" => (url, true),
        [flag, url] if flag == "--yes" => (url, true),
        _ => return Err(USAGE.to_string()),
    };
    let db = StoreUrl::parse(url).map_err(|e| format!("rollback: the world store URL: {e}"))?;
    if !yes {
        return Err(ROLLBACK_NEEDS_YES.to_string());
    }
    Ok(db)
}

fn parse_export(args: &[String]) -> Result<(StoreUrl, String), String> {
    let (mut db, mut out) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--db" => {
                db = Some(parse_db(
                    it.next()
                        .ok_or_else(|| format!("--db needs a value\n{USAGE}"))?,
                )?)
            }
            path if out.is_none() && !path.starts_with("--") => out = Some(path.to_string()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    match (db, out) {
        (Some(db), Some(out)) => Ok((db, out)),
        _ => Err(USAGE.to_string()),
    }
}

fn open(db: &StoreUrl, mode: OpenMode) -> Result<StoreHandle, String> {
    StoreHandle::open(db, Dials::BUILT, mode).map_err(|e| format!("world store {db}: {e}"))
}

/// Forges `params` and stores it in `db` as tiles; returns the world's
/// `content_crc`. Refused (before forging) when `db` already holds a world
/// or saved regions: a world is never written over another.
pub fn forge_into(db: &StoreUrl, params: &ForgeParams) -> Result<u32, String> {
    let store = open(db, OpenMode::Writer)?;
    let result = (|| {
        let info = store.world_info().map_err(|e| e.to_string())?;
        let regions = store.stored_regions().map_err(|e| e.to_string())?;
        if info.has_artifact || !regions.is_empty() {
            return Err(format!(
                "{db} already holds a world; forge into a new database"
            ));
        }
        let bytes = std::panic::catch_unwind(|| forge::forge(params).artifact(params).to_bytes())
            .map_err(|e| {
            format!(
                "the forge failed: {}",
                forge::play::panic_message(e.as_ref())
            )
        })?;
        crate::write_tiled(&store, bytes).map_err(|e| e.to_string())
    })();
    store.shutdown();
    result
}

/// Writes `db`'s archived forge file to `out` (atomically: a temp file
/// renamed into place); returns its size.
pub fn export(db: &StoreUrl, out: &Path) -> Result<usize, String> {
    let store = open(db, OpenMode::ReadOnly)?;
    let archive = store.forged_archive();
    store.shutdown();
    let bytes = archive
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{db} holds no forged world to export"))?;
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

/// Clears `db`'s world clock anchor (needed after restoring a world whose saved tick is
/// ahead of the anchor); the next host or coordinator start stores a new one.
pub fn clock_reset(db: &StoreUrl) -> Result<(), String> {
    let store = open(db, OpenMode::Writer)?;
    let result = store.clear_clock_anchor().map_err(|e| e.to_string());
    store.shutdown();
    result
}

/// Sets `db`'s super-region side to `n`, refused unless no host has claimed a super-region
/// yet. Errors never echo the URL.
pub fn set_super_region(db: &StoreUrl, n: u32) -> Result<(), String> {
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::Writer)
        .map_err(|e| format!("the world store: {e}"))?;
    let result = match store.set_super_region_regions(n) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "super-region: refused, a host has already claimed super-regions of this world (side {}); \
             the side is fixed once a host has started",
            store.super_region_regions()
        )),
        Err(e) => Err(format!("super-region: {e}")),
    };
    store.shutdown();
    result
}

/// One line of `worldctl hosts` for `row` (the module docs), `land` super-regions owned, the
/// database clock `now_ms`.
pub fn host_line(row: &crate::HostRow, land: usize, now_ms: u64) -> String {
    let name = if row.name.is_empty() {
        "-"
    } else {
        row.name.as_str()
    };
    let state = if row.state == crate::HOST_LIVE {
        "live"
    } else {
        "dead"
    };
    let clean = if row.clean { "clean" } else { "dirty" };
    let seen = if row.seen_ms == 0 {
        "vouched".to_string()
    } else {
        now_ms.saturating_sub(row.seen_ms).to_string()
    };
    let drain = row
        .drain_in_ms
        .map_or_else(|| "-".to_string(), |ms| ms.to_string());
    format!(
        "{} {name} {state} {clean} land={land} seen={seen} drain={drain}",
        row.id.0
    )
}

/// `worldctl hosts <url>`: every `hosts` row of `db`, one [`host_line`] each, by id. Opened
/// read-only (beside a running world). Errors never echo the URL.
pub fn list_hosts(db: &StoreUrl) -> Result<String, String> {
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::ReadOnly)
        .map_err(|e| format!("hosts: the world store: {e}"))?;
    let result = (|| {
        let fail = |e: crate::StoreError| format!("hosts: {e}");
        let rows = store.hosts().map_err(fail)?;
        let owners = store.owners().map_err(fail)?;
        let now = store.db_now_ms().map_err(fail)?;
        let lines: Vec<String> = rows
            .iter()
            .map(|r| {
                let land = owners
                    .iter()
                    .filter(|(_, o, _)| u64::try_from(*o).is_ok_and(|o| o == r.id.0))
                    .count();
                host_line(r, land, now)
            })
            .collect();
        Ok(lines.join("\n"))
    })();
    store.shutdown();
    result
}

/// One `worldctl desired` line: `<name> slot=<n> <wanted|draining> age=<ms>`.
pub fn desired_line(row: &game::coordinator::DesiredHost, now_ms: u64) -> String {
    let state = if row.draining { "draining" } else { "wanted" };
    format!(
        "{} slot={} {state} age={}",
        row.name,
        row.slot,
        now_ms.saturating_sub(row.created_ms)
    )
}

/// `worldctl desired <url>`: every `desired_hosts` row of `db`, one [`desired_line`] each, by
/// slot. Opened read-only (beside a running world). Errors never echo the URL.
pub fn list_desired(db: &StoreUrl) -> Result<String, String> {
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::ReadOnly)
        .map_err(|e| format!("desired: the world store: {e}"))?;
    let result = (|| {
        let fail = |e: crate::StoreError| format!("desired: {e}");
        let rows = store.desired_hosts().map_err(fail)?;
        let now = store.db_now_ms().map_err(fail)?;
        Ok(rows
            .iter()
            .map(|r| desired_line(r, now))
            .collect::<Vec<_>>()
            .join("\n"))
    })();
    store.shutdown();
    result
}

/// Retires host `ordinal` of `db` (see the module docs): what it did, or why not. Errors never
/// echo the URL.
pub fn retire_host(db: &StoreUrl, ordinal: HostId) -> Result<String, String> {
    if !matches!(db, StoreUrl::Postgres(_)) {
        return Err("retire-host: a world of several hosts is a Postgres world; this store has one host, nothing to retire".into());
    }
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::Writer)
        .map_err(|e| format!("retire-host: the world store: {e}"))?;
    let result = retire_on(&store, db, ordinal);
    store.shutdown();
    result
}

/// [`retire_host`] on an open store.
fn retire_on(store: &StoreHandle, db: &StoreUrl, ordinal: HostId) -> Result<String, String> {
    // Operator-facing text names a host by its number (as `worldctl retire-host <url> <n>` takes it).
    let ord = ordinal.0;
    let fail = |e: crate::StoreError| format!("retire-host: {e}");
    let rows = store.hosts().map_err(fail)?;
    let now = store.db_now_ms().map_err(fail)?;
    let fresh =
        |r: &crate::HostRow| r.seen_ms != 0 && now.saturating_sub(r.seen_ms) <= game::HOST_LEASE_MS;
    let running = |r: &crate::HostRow| !r.clean && r.state == crate::HOST_LIVE && fresh(r);
    if rows.iter().any(|r| r.id == ordinal && running(r)) {
        return Err(format!("retire-host: host {ord} is running: stop it first"));
    }
    let alive: Vec<HostId> = rows
        .iter()
        .filter(|r| r.id != ordinal && running(r))
        .map(|r| r.id)
        .collect();
    let alive_n: Vec<u64> = alive.iter().map(|h| h.0).collect();
    if !alive.is_empty() {
        return match store.vouch_host(ordinal).map_err(fail)? {
            crate::Vouched::Vouched => Ok(format!(
                "worldctl: host {ord}'s row in {db} is vouched dead (seen_ms = 0); hosts {alive_n:?} run: the coordinator \
                 declares host {ord} dead at its next read and one of them inherits its land, if it holds any (claim ownership)"
            )),
            crate::Vouched::Running => {
                Err(format!("retire-host: host {ord} is running: stop it first"))
            }
            crate::Vouched::Refused(why) => Err(format!("retire-host: {why}")),
        };
    }
    // Nobody alive (Elastic 2a): refused. The first host to register takes every dirty row's
    // land itself (a total crash, `register_host`); nothing here opens a succession.
    Err(format!("retire-host: {OFFLINE_RETIRE_REFUSED}"))
}

/// What `retire-host` says with no host running (Elastic 2a: the offline retirement is gone).
const OFFLINE_RETIRE_REFUSED: &str = "no host is running: start one — the first registration takes the land (a total crash); retire-host only vouches \
     for a dead row while hosts run";

/// Rolls `db`'s world back (`rollback --yes`): `(generation, successions abandoned)`.
pub fn rollback(db: &StoreUrl) -> Result<(u64, u64), String> {
    let store = StoreHandle::open(db, Dials::BUILT, OpenMode::Writer)
        .map_err(|e| format!("rollback: the world store: {e}"))?;
    let result = store
        .rollback_world_operator()
        .map_err(|e| format!("rollback: {e}"));
    store.shutdown();
    result
}

/// Rebuilds `db`'s tiles from its archived forge file, in one transaction
/// (`write_tiled`); returns the world's `content_crc`. A store with no
/// archive cannot be re-tiled.
pub fn retile(db: &StoreUrl) -> Result<u32, String> {
    let store = open(db, OpenMode::Writer)?;
    let result = (|| {
        let archive = store
            .forged_archive()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("{db} keeps no archived forge file to re-tile from; re-forge the world")
            })?;
        crate::write_tiled(&store, archive).map_err(|e| e.to_string())
    })();
    store.shutdown();
    result
}
